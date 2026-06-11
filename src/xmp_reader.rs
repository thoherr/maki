//! XMP read/write — parses Adobe XMP packets (sidecar files and embedded
//! in JPEG/TIFF) for tags, rating, description, color label.
//!
//! Also handles writeback: when `[writeback] enabled = true`, edits
//! in MAKI flow back into the original XMP file on disk so other tools
//! (Lightroom, Capture One) see them.
//!
//! # Writer architecture: locate + splice
//!
//! The writers (`update_rating`, `update_label`, `update_description`,
//! `update_tags`, `update_hierarchical_subjects`) share a single
//! XML-aware pass over the document ([`locate`]) that uses quick-xml —
//! the same parser the reader uses — to produce byte-offset *spans* for
//! everything a writer may touch: the `rdf:Description` start tags (with
//! per-attribute value spans), and the property blocks inside them
//! (`dc:subject`, `dc:description`, `lr:hierarchicalSubject`,
//! element-form `xmp:Rating` / `xmp:Label`), identified by **namespace
//! URI + local name** with in-scope `xmlns` resolution.
//!
//! Each writer then computes the new semantic state; if nothing changed
//! it returns the input bytes unchanged (byte-stability — no SHA drift
//! on no-op writebacks), otherwise it splices the affected span with a
//! canonically rendered replacement and leaves every other byte of the
//! document untouched (comments, CDATA, unrelated blocks, formatting).
//!
//! This replaced an earlier regex edit-in-place design that caused the
//! v4.5.14–v4.5.17 bug train: regexes keyed on literal prefixes missed
//! namespace-URI-equivalent bindings (`lightroom:` vs `lr:`), could
//! match inside comments or text, and regex replacement strings expand
//! `$n` sequences appearing in user text. Splicing is plain string
//! concatenation on spans produced by a real XML parser, so reader and
//! writer now share quick-xml as the single XML understanding.

use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::path::Path;

use anyhow::Result;
use quick_xml::events::Event;
use quick_xml::Reader;

/// Extracted metadata from an XMP sidecar file.
pub struct XmpData {
    /// Keywords from `dc:subject`.
    pub keywords: Vec<String>,
    /// Hierarchical keywords from `lr:hierarchicalSubject` (pipe-separated in XMP, stored with `/`).
    pub hierarchical_keywords: Vec<String>,
    /// Description from `dc:description`.
    pub description: Option<String>,
    /// Additional metadata: rating, label, creator, copyright.
    pub source_metadata: HashMap<String, String>,
}

impl XmpData {
    pub(crate) fn empty() -> Self {
        Self {
            keywords: Vec::new(),
            hierarchical_keywords: Vec::new(),
            description: None,
            source_metadata: HashMap::new(),
        }
    }
}

/// Which RDF container we're currently inside.
#[derive(Debug, Clone, PartialEq)]
enum Context {
    None,
    SubjectBag,
    HierarchicalBag,
    DescriptionAlt,
    CreatorContainer,
    RightsAlt,
}

/// Return the local name of an XML tag (strip namespace prefix).
fn local_name(tag: &[u8]) -> Vec<u8> {
    match tag.iter().position(|&b| b == b':') {
        Some(pos) => tag[pos + 1..].to_vec(),
        None => tag.to_vec(),
    }
}

/// Extract XMP metadata from a file. Infallible — returns empty data on any error.
pub fn extract(path: &Path) -> XmpData {
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(_) => return XmpData::empty(),
    };
    parse_xmp(&content)
}

// ───────────────────────── XML-aware locator ─────────────────────────
//
// One quick-xml pass over the document produces byte-offset spans for
// everything the writers below may need to touch. The writers then
// splice replacements into those spans; no regex is involved.

const NS_RDF: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#";
const NS_DC: &str = "http://purl.org/dc/elements/1.1/";
const NS_XMP: &str = "http://ns.adobe.com/xap/1.0/";
const NS_LR: &str = "http://ns.adobe.com/lightroom/1.0/";

/// An attribute on an `rdf:Description` start tag, with exact byte spans.
#[derive(Debug)]
struct AttrSpan {
    /// Prefix part of the qname (`""` when unprefixed).
    prefix: String,
    /// Local part of the qname.
    local: String,
    /// Namespace URI the prefix resolves to, when a binding is in scope.
    ns: Option<String>,
    /// The whole attribute including all leading whitespace, through the
    /// closing quote — the span to delete when removing the attribute.
    full_span: Range<usize>,
    /// The raw (undecoded) value bytes between the quotes — the span to
    /// splice when replacing the value.
    value_span: Range<usize>,
}

impl AttrSpan {
    fn is_xmlns(&self) -> bool {
        self.prefix == "xmlns" || (self.prefix.is_empty() && self.local == "xmlns")
    }

    /// Namespace-URI match with a literal-prefix fallback for documents
    /// that use the conventional prefix without declaring it.
    fn matches(&self, uri: &str, local: &str, fallback_prefix: &str) -> bool {
        !self.is_xmlns()
            && self.local == local
            && (self.ns.as_deref() == Some(uri) || self.prefix == fallback_prefix)
    }
}

/// A located `rdf:Description` element.
#[derive(Debug)]
struct DescriptionSpan {
    /// Byte position just after `<rdf:Description` — the attribute /
    /// xmlns injection point (matches the historical injection spot).
    name_end: usize,
    /// `<` through `>` (or `/>`) of the start tag.
    start_span: Range<usize>,
    /// Start of the `[ \t]*` indentation run before the start tag.
    indent_start: usize,
    self_closing: bool,
    attrs: Vec<AttrSpan>,
    /// Position of the `<` of the matching `</rdf:Description>`.
    close_pos: Option<usize>,
    /// Start of the indentation run before the close tag — the block
    /// injection point.
    close_indent_start: usize,
}

/// A located property block (direct child of an `rdf:Description`).
#[derive(Debug)]
struct PropBlock {
    prefix: String,
    local: String,
    ns: Option<String>,
    /// Full block: start of the leading `[ \t]*` indentation through the
    /// `>` of the end tag (or `/>` for an empty element).
    span: Range<usize>,
    /// The `[ \t]*` indentation before the opening tag.
    indent: String,
    /// Raw (undecoded) text spans of each `rdf:li` item, document order.
    items: Vec<Range<usize>>,
    /// Raw text span of the element's own direct content (element-form
    /// `xmp:Rating` / `xmp:Label`).
    text_span: Range<usize>,
    self_closing: bool,
}

impl PropBlock {
    fn matches(&self, uri: &str, local: &str, fallback_prefixes: &[&str]) -> bool {
        self.local == local
            && (self.ns.as_deref() == Some(uri)
                || fallback_prefixes.contains(&self.prefix.as_str()))
    }

    fn qname(&self) -> String {
        if self.prefix.is_empty() {
            self.local.clone()
        } else {
            format!("{}:{}", self.prefix, self.local)
        }
    }
}

#[derive(Debug, Default)]
struct XmpLayout {
    descriptions: Vec<DescriptionSpan>,
    props: Vec<PropBlock>,
}

impl XmpLayout {
    /// First attribute (document order, across all rdf:Description tags)
    /// bound to `uri` with the given local name.
    fn find_attr(&self, uri: &str, local: &str, fallback_prefix: &str) -> Option<&AttrSpan> {
        self.descriptions
            .iter()
            .flat_map(|d| d.attrs.iter())
            .find(|a| a.matches(uri, local, fallback_prefix))
    }

    /// First property block (document order) bound to `uri` with the
    /// given local name.
    fn find_prop(&self, uri: &str, local: &str, fallback_prefixes: &[&str]) -> Option<&PropBlock> {
        self.props
            .iter()
            .find(|p| p.matches(uri, local, fallback_prefixes))
    }

    /// Every `hierarchicalSubject` block bound to the Lightroom
    /// namespace URI — any prefix (`lr:`, `lightroom:`, exotic ones).
    fn lightroom_blocks(&self) -> Vec<&PropBlock> {
        self.props
            .iter()
            .filter(|p| p.matches(NS_LR, "hierarchicalSubject", &["lr", "lightroom"]))
            .collect()
    }

    /// First rdf:Description with a real end tag — the block injection
    /// target (mirrors the historical "first `</rdf:Description>` in
    /// document order" behavior).
    fn injection_target(&self) -> Option<&DescriptionSpan> {
        self.descriptions.iter().find(|d| d.close_pos.is_some())
    }

    /// First self-closing rdf:Description — conversion target when no
    /// description has an end tag.
    fn self_closing_target(&self) -> Option<&DescriptionSpan> {
        self.descriptions.iter().find(|d| d.self_closing)
    }
}

/// State for a property block whose end tag has not been seen yet.
struct OpenProp {
    depth: usize,
    prefix: String,
    local: String,
    ns: Option<String>,
    indent_start: usize,
    tag_start: usize,
    text_start: usize,
    items: Vec<Range<usize>>,
}

/// Split a qname into (prefix, local) strings.
fn split_qname(qname: &[u8]) -> (String, String) {
    let s = String::from_utf8_lossy(qname);
    match s.find(':') {
        Some(pos) => (s[..pos].to_string(), s[pos + 1..].to_string()),
        None => (String::new(), s.into_owned()),
    }
}

/// Resolve a prefix against the in-scope xmlns declarations.
fn resolve_prefix(ns_stack: &[Vec<(String, String)>], prefix: &str) -> Option<String> {
    ns_stack.iter().rev().find_map(|scope| {
        scope
            .iter()
            .rev()
            .find(|(p, _)| p == prefix)
            .map(|(_, uri)| uri.clone())
    })
}

/// Collect the xmlns declarations on a start tag.
fn xmlns_decls(e: &quick_xml::events::BytesStart<'_>) -> Vec<(String, String)> {
    let mut decls = Vec::new();
    for attr in e.attributes().flatten() {
        let key = String::from_utf8_lossy(attr.key.as_ref()).into_owned();
        let prefix = if key == "xmlns" {
            Some(String::new())
        } else {
            key.strip_prefix("xmlns:").map(str::to_string)
        };
        if let Some(p) = prefix {
            let uri = attr
                .unescape_value()
                .map(|v| v.into_owned())
                .unwrap_or_else(|_| String::from_utf8_lossy(&attr.value).into_owned());
            decls.push((p, uri));
        }
    }
    decls
}

/// Start of the `[ \t]*` indentation run immediately before `pos`.
fn line_indent_start(content: &str, pos: usize) -> usize {
    let bytes = content.as_bytes();
    let mut i = pos;
    while i > 0 && (bytes[i - 1] == b' ' || bytes[i - 1] == b'\t') {
        i -= 1;
    }
    i
}

/// Manually scan the raw bytes of a start tag for attributes with exact
/// byte spans (quick-xml's attribute iterator does not expose offsets).
fn scan_attributes(
    content: &str,
    tag_span: Range<usize>,
    ns_stack: &[Vec<(String, String)>],
) -> Vec<AttrSpan> {
    let raw = content[tag_span.clone()].as_bytes();
    let base = tag_span.start;
    let len = raw.len();
    let mut attrs = Vec::new();
    // Skip '<' + qname.
    let mut i = 1;
    while i < len && !raw[i].is_ascii_whitespace() && raw[i] != b'>' && raw[i] != b'/' {
        i += 1;
    }
    loop {
        let ws_start = i;
        while i < len && raw[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= len || raw[i] == b'>' || raw[i] == b'/' {
            break;
        }
        let name_start = i;
        while i < len
            && raw[i] != b'='
            && !raw[i].is_ascii_whitespace()
            && raw[i] != b'>'
            && raw[i] != b'/'
        {
            i += 1;
        }
        let name = &content[base + name_start..base + i];
        while i < len && raw[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= len || raw[i] != b'=' {
            break; // malformed / bare attribute — stop scanning
        }
        i += 1;
        while i < len && raw[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= len || (raw[i] != b'"' && raw[i] != b'\'') {
            break;
        }
        let quote = raw[i];
        i += 1;
        let v_start = i;
        while i < len && raw[i] != quote {
            i += 1;
        }
        if i >= len {
            break; // unterminated value
        }
        let v_end = i;
        i += 1;
        let (prefix, local) = match name.find(':') {
            Some(pos) => (name[..pos].to_string(), name[pos + 1..].to_string()),
            None => (String::new(), name.to_string()),
        };
        // Attributes never inherit the default namespace.
        let ns = if prefix.is_empty() {
            None
        } else {
            resolve_prefix(ns_stack, &prefix)
        };
        attrs.push(AttrSpan {
            prefix,
            local,
            ns,
            full_span: base + ws_start..base + v_end + 1,
            value_span: base + v_start..base + v_end,
        });
    }
    attrs
}

/// Is this element one of the property blocks the writers care about?
fn is_interesting_prop(local: &str, ns: Option<&str>, prefix: &str) -> bool {
    match local {
        "subject" | "description" => ns == Some(NS_DC) || prefix == "dc",
        "hierarchicalSubject" => ns == Some(NS_LR) || prefix == "lr" || prefix == "lightroom",
        "Rating" | "Label" => ns == Some(NS_XMP) || prefix == "xmp",
        _ => false,
    }
}

/// Handle a Start or Empty event during [`locate`].
#[allow(clippy::too_many_arguments)]
fn record_open(
    content: &str,
    start: usize,
    end: usize,
    e: &quick_xml::events::BytesStart<'_>,
    elem_depth: usize,
    self_closing: bool,
    ns_stack: &[Vec<(String, String)>],
    layout: &mut XmpLayout,
    open_desc: &mut Option<(usize, usize)>,
    open_prop: &mut Option<OpenProp>,
    open_li: &mut Option<(usize, usize)>,
) {
    let qname_len = e.name().as_ref().len();
    let (prefix, local) = split_qname(e.name().as_ref());
    let ns = resolve_prefix(ns_stack, &prefix);

    if open_desc.is_none()
        && local == "Description"
        && (ns.as_deref() == Some(NS_RDF) || prefix == "rdf")
    {
        let attrs = scan_attributes(content, start..end, ns_stack);
        layout.descriptions.push(DescriptionSpan {
            name_end: start + 1 + qname_len,
            start_span: start..end,
            indent_start: line_indent_start(content, start),
            self_closing,
            attrs,
            close_pos: None,
            close_indent_start: 0,
        });
        if !self_closing {
            *open_desc = Some((elem_depth, layout.descriptions.len() - 1));
        }
        return;
    }

    let Some((desc_depth, _)) = *open_desc else {
        return;
    };

    if open_prop.is_none()
        && elem_depth == desc_depth + 1
        && is_interesting_prop(&local, ns.as_deref(), &prefix)
    {
        let indent_start = line_indent_start(content, start);
        if self_closing {
            layout.props.push(PropBlock {
                prefix,
                local,
                ns,
                span: indent_start..end,
                indent: content[indent_start..start].to_string(),
                items: Vec::new(),
                text_span: end..end,
                self_closing: true,
            });
        } else {
            *open_prop = Some(OpenProp {
                depth: elem_depth,
                prefix,
                local,
                ns,
                indent_start,
                tag_start: start,
                text_start: end,
                items: Vec::new(),
            });
        }
        return;
    }

    if open_prop.is_some() && open_li.is_none() && local == "li" {
        if self_closing {
            if let Some(op) = open_prop.as_mut() {
                op.items.push(end..end);
            }
        } else {
            *open_li = Some((elem_depth, end));
        }
    }
}

/// Parse `content` once with quick-xml and locate every span the
/// writers may need: rdf:Description start tags (with attribute spans)
/// and the interesting property blocks inside them. Infallible —
/// returns whatever was located before the first parse error.
fn locate(content: &str) -> XmpLayout {
    let mut layout = XmpLayout::default();
    let mut reader = Reader::from_str(content);

    let mut ns_stack: Vec<Vec<(String, String)>> = Vec::new();
    let mut depth = 0usize;
    // (element depth, index into layout.descriptions)
    let mut open_desc: Option<(usize, usize)> = None;
    let mut open_prop: Option<OpenProp> = None;
    // (element depth, text start) of the current rdf:li
    let mut open_li: Option<(usize, usize)> = None;

    loop {
        let start = usize::try_from(reader.buffer_position()).unwrap_or(usize::MAX);
        let event = match reader.read_event() {
            Ok(ev) => ev,
            Err(_) => break,
        };
        let end = usize::try_from(reader.buffer_position()).unwrap_or(usize::MAX);
        match event {
            Event::Start(ref e) => {
                ns_stack.push(xmlns_decls(e));
                depth += 1;
                record_open(
                    content, start, end, e, depth, false, &ns_stack, &mut layout,
                    &mut open_desc, &mut open_prop, &mut open_li,
                );
            }
            Event::Empty(ref e) => {
                ns_stack.push(xmlns_decls(e));
                record_open(
                    content, start, end, e, depth + 1, true, &ns_stack, &mut layout,
                    &mut open_desc, &mut open_prop, &mut open_li,
                );
                ns_stack.pop();
            }
            Event::End(ref e) => {
                let elem_depth = depth;
                let (_, local) = split_qname(e.name().as_ref());
                if open_li.is_some_and(|(d, _)| d == elem_depth) && local == "li" {
                    let (_, text_start) = open_li.take().unwrap();
                    if let Some(op) = open_prop.as_mut() {
                        op.items.push(text_start..start);
                    }
                } else if open_prop
                    .as_ref()
                    .is_some_and(|op| op.depth == elem_depth && op.local == local)
                {
                    let op = open_prop.take().unwrap();
                    layout.props.push(PropBlock {
                        prefix: op.prefix,
                        local: op.local,
                        ns: op.ns,
                        span: op.indent_start..end,
                        indent: content[op.indent_start..op.tag_start].to_string(),
                        items: op.items,
                        text_span: op.text_start..start,
                        self_closing: false,
                    });
                } else if open_desc.is_some_and(|(d, _)| d == elem_depth) && local == "Description"
                {
                    let (_, idx) = open_desc.take().unwrap();
                    layout.descriptions[idx].close_pos = Some(start);
                    layout.descriptions[idx].close_indent_start =
                        line_indent_start(content, start);
                }
                depth = depth.saturating_sub(1);
                ns_stack.pop();
            }
            Event::Eof => break,
            _ => {}
        }
    }

    layout
}

// ───────────────────────── splice helpers ─────────────────────────

/// Apply non-overlapping `(range, replacement)` edits, sorted by start.
fn splice(content: &str, edits: Vec<(Range<usize>, String)>) -> String {
    let mut out = String::with_capacity(content.len() + 256);
    let mut last = 0;
    for (range, replacement) in edits {
        debug_assert!(range.start >= last, "splice edits must be sorted and non-overlapping");
        out.push_str(&content[last..range.start]);
        out.push_str(&replacement);
        last = range.end;
    }
    out.push_str(&content[last..]);
    out
}

/// Extend a block span to swallow the newline immediately before it, so
/// removing the block doesn't leave a blank line behind.
fn with_preceding_newline(content: &str, span: &Range<usize>) -> Range<usize> {
    if content[..span.start].ends_with('\n') {
        span.start - 1..span.end
    } else {
        span.clone()
    }
}

/// Inject a rendered property block into the document: before the first
/// `</rdf:Description>` when one exists, otherwise by converting the
/// first self-closing `<rdf:Description …/>`. `render` receives the
/// default block indentation (description indent + one space).
/// `xmlns_attr` (e.g. ` xmlns:dc="…"`) is inserted right after the first
/// `<rdf:Description` when the caller determined it is missing.
/// `extra_edits` are applied alongside (block strips for the
/// hierarchical collapse); when no rdf:Description exists, only the
/// extra edits are applied.
fn inject_block(
    content: &str,
    layout: &XmpLayout,
    xmlns_attr: Option<&str>,
    render: impl Fn(&str) -> String,
    extra_edits: Vec<(Range<usize>, String)>,
) -> String {
    if let Some(target) = layout.injection_target() {
        let close_pos = target.close_pos.unwrap();
        let desc_indent = &content[target.close_indent_start..close_pos];
        let mut block = render(&format!("{desc_indent} "));
        block.push('\n');
        let mut edits = extra_edits;
        if let Some(ns) = xmlns_attr {
            if let Some(first) = layout.descriptions.first() {
                edits.push((first.name_end..first.name_end, ns.to_string()));
            }
        }
        edits.push((target.close_indent_start..target.close_indent_start, block));
        edits.sort_by_key(|(r, _)| r.start);
        return splice(content, edits);
    }

    if let Some(target) = layout.self_closing_target() {
        let desc_indent = &content[target.indent_start..target.start_span.start];
        let attrs_raw = &content[target.name_end..target.start_span.end - 2];
        let ns_part = xmlns_attr.unwrap_or("");
        let block = render(&format!("{desc_indent} "));
        let replacement = format!(
            "{desc_indent}<rdf:Description{ns_part}{attrs_raw}>\n{block}\n{desc_indent}</rdf:Description>"
        );
        let mut edits = extra_edits;
        edits.push((target.indent_start..target.start_span.end, replacement));
        edits.sort_by_key(|(r, _)| r.start);
        return splice(content, edits);
    }

    // No rdf:Description to inject into.
    splice(content, extra_edits)
}

/// Update the `xmp:Rating` value in an XMP file on disk.
///
/// Uses string-based find/replace to preserve all other XMP content byte-for-byte.
/// Returns `Ok(true)` if the file was modified, `Ok(false)` if no change was needed.
/// Rating of `None` or `Some(0)` writes `"0"` (XMP convention for "no rating").
pub fn update_rating(path: &Path, rating: Option<u8>) -> Result<bool> {
    let content = std::fs::read_to_string(path)?;
    let rating_str = match rating {
        Some(r) if r > 0 => r.to_string(),
        _ => "0".to_string(),
    };

    let modified = update_rating_in_string(&content, &rating_str);

    if modified == content {
        return Ok(false);
    }

    std::fs::write(path, &modified)?;
    Ok(true)
}

/// Apply a rating update to an XMP string, returning the modified string.
fn update_rating_in_string(content: &str, rating_str: &str) -> String {
    let layout = locate(content);

    // Attribute form: xmp:Rating="…"
    if let Some(attr) = layout.find_attr(NS_XMP, "Rating", "xmp") {
        if &content[attr.value_span.clone()] == rating_str {
            return content.to_string();
        }
        return splice(content, vec![(attr.value_span.clone(), rating_str.to_string())]);
    }

    // Element form: <xmp:Rating>…</xmp:Rating>
    if let Some(block) = layout
        .find_prop(NS_XMP, "Rating", &["xmp"])
        .filter(|b| !b.self_closing)
    {
        if &content[block.text_span.clone()] == rating_str {
            return content.to_string();
        }
        return splice(content, vec![(block.text_span.clone(), rating_str.to_string())]);
    }

    // Neither form found — inject attribute if rating > 0
    if rating_str == "0" {
        return content.to_string();
    }

    // Inject xmp:Rating attribute into the first rdf:Description element;
    // no rdf:Description → return unchanged.
    if let Some(desc) = layout.descriptions.first() {
        return splice(
            content,
            vec![(desc.name_end..desc.name_end, format!(r#" xmp:Rating="{rating_str}""#))],
        );
    }
    content.to_string()
}

/// Update the `dc:subject` keywords in an XMP file on disk.
///
/// Applies delta operations: adds `tags_to_add` and removes `tags_to_remove`
/// from the existing `dc:subject` / `rdf:Bag` keyword list.
/// Preserves tags in the XMP that are not mentioned in either list.
/// Returns `Ok(true)` if the file was modified, `Ok(false)` if no change was needed.
pub fn update_tags(path: &Path, tags_to_add: &[String], tags_to_remove: &[String]) -> Result<bool> {
    if tags_to_add.is_empty() && tags_to_remove.is_empty() {
        return Ok(false);
    }
    let content = std::fs::read_to_string(path)?;
    let modified = update_tags_in_string(&content, tags_to_add, tags_to_remove);
    if modified == content {
        return Ok(false);
    }
    std::fs::write(path, &modified)?;
    Ok(true)
}

/// Render a canonical `<qname><rdf:Bag>…` keyword block at the given
/// indent (no trailing newline).
fn render_bag_block(qname: &str, indent: &str, tags: &[String]) -> String {
    let bag_indent = format!("{indent} ");
    let li_indent = format!("{indent}  ");
    let mut block = format!("{indent}<{qname}>\n{bag_indent}<rdf:Bag>\n");
    for tag in tags {
        block.push_str(&format!("{li_indent}<rdf:li>{}</rdf:li>\n", xml_escape(tag)));
    }
    block.push_str(&format!("{bag_indent}</rdf:Bag>\n{indent}</{qname}>"));
    block
}

/// Apply tag add/remove operations to an XMP string, returning the modified string.
fn update_tags_in_string(content: &str, tags_to_add: &[String], tags_to_remove: &[String]) -> String {
    let remove_set: HashSet<&str> = tags_to_remove.iter().map(|s| s.as_str()).collect();
    let layout = locate(content);

    if let Some(block) = layout.find_prop(NS_DC, "subject", &["dc"]) {
        // Parse existing tags. `xml_unescape` is essential here — without
        // it `&amp;`-style entities are kept as literal text, never match
        // the catalog (which carries decoded `&`), and accumulate an extra
        // `&amp;` layer on every writeback (the `&` in `&amp;` gets
        // re-escaped to `&amp;amp;`, then `&amp;amp;amp;`, etc.).
        let original: Vec<String> = block
            .items
            .iter()
            .map(|span| xml_unescape(&content[span.clone()]))
            .collect();

        let mut tags = original.clone();

        // Apply removals
        tags.retain(|t| !remove_set.contains(t.as_str()));

        // Apply additions (deduplicated)
        for tag in tags_to_add {
            if !tags.iter().any(|t| t == tag) {
                tags.push(tag.clone());
            }
        }

        // No semantic change — return the input bytes unchanged.
        if tags == original {
            return content.to_string();
        }

        if tags.is_empty() {
            // Remove the entire dc:subject block including the preceding newline
            return splice(
                content,
                vec![(with_preceding_newline(content, &block.span), String::new())],
            );
        }

        // Rebuild the block with the same indentation and element prefix.
        return splice(
            content,
            vec![(block.span.clone(), render_bag_block(&block.qname(), &block.indent, &tags))],
        );
    }

    // No existing dc:subject — only proceed if we have tags to add
    if tags_to_add.is_empty() {
        return content.to_string();
    }

    // Ensure xmlns:dc is declared, then inject the block.
    let xmlns_attr = if content.contains("xmlns:dc") {
        None
    } else {
        Some(format!(r#" xmlns:dc="{NS_DC}""#))
    };
    inject_block(
        content,
        &layout,
        xmlns_attr.as_deref(),
        |indent| render_bag_block("dc:subject", indent, tags_to_add),
        Vec::new(),
    )
}

/// Update the `lr:hierarchicalSubject` keywords in an XMP file on disk.
///
/// Only processes hierarchical tags (containing `/`). Flat tags are ignored.
/// Converts `/` to `|` for XMP storage format.
/// Returns `Ok(true)` if the file was modified, `Ok(false)` if no change was needed.
pub fn update_hierarchical_subjects(
    path: &Path,
    tags_to_add: &[String],
    tags_to_remove: &[String],
) -> Result<bool> {
    // The add list is filtered to pipe-containing tags only — flat
    // tags don't belong in `lr:hierarchicalSubject`, and silently
    // dropping them here prevents callers from accidentally polluting
    // the block (e.g. with `dc:subject` flat components).
    let hier_add: Vec<String> = tags_to_add
        .iter()
        .filter(|t| t.contains('|'))
        .cloned()
        .collect();
    // The remove list, however, is taken as-is. If the caller asks
    // to drop a leaf-only entry from `lr:hierarchicalSubject` (which
    // is what `--mirror-tags` / `--force` does for any pre-existing
    // non-pipe garbage), we honor it. Filtering by pipe here would
    // silently keep stale leaf entries — `Bavaria`, `Konzert`,
    // multi-escape leftovers, anything flat that legitimately
    // shouldn't be in the hierarchical block.
    let hier_remove: Vec<String> = tags_to_remove.to_vec();

    if hier_add.is_empty() && hier_remove.is_empty() {
        return Ok(false);
    }

    let content = std::fs::read_to_string(path)?;
    let modified = update_hierarchical_in_string(&content, &hier_add, &hier_remove);
    if modified == content {
        return Ok(false);
    }
    std::fs::write(path, &modified)?;
    Ok(true)
}

/// Render a canonical `lr:hierarchicalSubject` block at the given indent.
fn render_hierarchical_block(indent: &str, tags: &[String]) -> String {
    render_bag_block("lr:hierarchicalSubject", indent, tags)
}

/// Apply hierarchical subject add/remove operations to an XMP string.
/// Tags use pipe-separated format (e.g., `animals|birds|eagles`).
///
/// `hierarchicalSubject` is keyed by namespace URI, not prefix. Some tools
/// (older CaptureOne, third-party exporters) bind a prefix other than `lr:`
/// to the Lightroom namespace — e.g. `lightroom:hierarchicalSubject`. When
/// MAKI writes to only the `lr:` block but leaves a parallel `lightroom:`
/// block intact, the latter becomes a stale parallel source of truth and
/// flat-name leaves leak back into the catalog on re-import.
///
/// This function:
/// 1. Finds every `hierarchicalSubject` block whose prefix is bound to the
///    Lightroom namespace URI (the locator resolves in-scope `xmlns`
///    declarations; bare `lr:`/`lightroom:` prefixes are honored even
///    without a declaration).
/// 2. If exactly one block exists and it is the canonical `lr:` form, edits
///    it in place — and returns the input bytes unchanged when the update
///    is a semantic no-op.
/// 3. Otherwise (zero blocks, multiple blocks, or a single non-canonical
///    block) strips every match, accumulates tags, and writes one canonical
///    `lr:` block.
fn update_hierarchical_in_string(
    content: &str,
    hier_to_add: &[String],
    hier_to_remove: &[String],
) -> String {
    let remove_set: HashSet<&str> = hier_to_remove.iter().map(|s| s.as_str()).collect();

    let layout = locate(content);
    let blocks = layout.lightroom_blocks();

    // Union of entries across all blocks, decoded with `xml_unescape`
    // (`&amp;` → `&`, etc.) so existing entries are compared against the
    // catalog's decoded form, not the still-escaped on-disk form — see
    // `xml_unescape` for the runaway-escape bug this prevents.
    // `original_seq` keeps the raw entry sequence (duplicates included)
    // for the semantic-change check.
    let mut accumulated_tags: Vec<String> = Vec::new();
    let mut original_seq: Vec<String> = Vec::new();
    for block in &blocks {
        for span in &block.items {
            let t = xml_unescape(&content[span.clone()]);
            if !accumulated_tags.contains(&t) {
                accumulated_tags.push(t.clone());
            }
            original_seq.push(t);
        }
    }

    let mut tags = accumulated_tags;
    tags.retain(|t| !remove_set.contains(t.as_str()));
    for tag in hier_to_add {
        if !tags.iter().any(|t| t == tag) {
            tags.push(tag.clone());
        }
    }

    // Fast path: exactly one canonical `lr:` block — edit in place.
    if blocks.len() == 1 && blocks[0].prefix == "lr" {
        let block = blocks[0];

        // No semantic change — return the input bytes unchanged.
        if tags == original_seq {
            return content.to_string();
        }

        if tags.is_empty() {
            return splice(
                content,
                vec![(with_preceding_newline(content, &block.span), String::new())],
            );
        }

        return splice(
            content,
            vec![(block.span.clone(), render_hierarchical_block(&block.indent, &tags))],
        );
    }

    // Slow path: zero blocks → fall through to inject; otherwise (multiple
    // blocks, or single non-canonical prefix) → strip every match and
    // re-inject a single canonical block.

    if blocks.is_empty() && hier_to_add.is_empty() {
        return content.to_string();
    }

    let preserved_indent = blocks.first().map(|b| b.indent.clone());
    let strips: Vec<(Range<usize>, String)> = blocks
        .iter()
        .map(|b| (with_preceding_newline(content, &b.span), String::new()))
        .collect();

    if tags.is_empty() {
        return splice(content, strips);
    }

    // Ensure xmlns:lr is declared, then inject one canonical block.
    let xmlns_attr = if content.contains("xmlns:lr=") {
        None
    } else {
        Some(format!(r#" xmlns:lr="{NS_LR}""#))
    };
    inject_block(
        content,
        &layout,
        xmlns_attr.as_deref(),
        |default_indent| {
            let indent = preserved_indent
                .clone()
                .unwrap_or_else(|| default_indent.to_string());
            render_hierarchical_block(&indent, &tags)
        },
        strips,
    )
}

/// Update the `dc:description` in an XMP file on disk.
///
/// Uses string-based find/replace to preserve all other XMP content byte-for-byte.
/// Returns `Ok(true)` if the file was modified, `Ok(false)` if no change was needed.
/// `description` of `None` or `Some("")` removes the `dc:description` block.
pub fn update_description(path: &Path, description: Option<&str>) -> Result<bool> {
    let content = std::fs::read_to_string(path)?;
    let modified = update_description_in_string(&content, description);
    if modified == content {
        return Ok(false);
    }
    std::fs::write(path, &modified)?;
    Ok(true)
}

/// Render a canonical `dc:description` block at the given indent
/// (no trailing newline).
fn render_description_block(indent: &str, text: &str) -> String {
    format!(
        "{indent}<dc:description>\n{indent} <rdf:Alt>\n{indent}  <rdf:li xml:lang=\"x-default\">{}</rdf:li>\n{indent} </rdf:Alt>\n{indent}</dc:description>",
        xml_escape(text)
    )
}

/// Apply a description update to an XMP string, returning the modified string.
fn update_description_in_string(content: &str, description: Option<&str>) -> String {
    let desc_text = description.unwrap_or("");
    let layout = locate(content);

    if let Some(block) = layout.find_prop(NS_DC, "description", &["dc"]) {
        if desc_text.is_empty() {
            // Remove the entire dc:description block including the
            // preceding newline.
            return splice(
                content,
                vec![(with_preceding_newline(content, &block.span), String::new())],
            );
        }

        if let Some(li_span) = block.items.first() {
            // Replace only the rdf:li text — the block formatting and the
            // original `<rdf:li …>` open tag stay byte-for-byte.
            let escaped = xml_escape(desc_text);
            if content[li_span.clone()] == escaped {
                return content.to_string();
            }
            return splice(content, vec![(li_span.clone(), escaped)]);
        }

        // Degenerate block without an rdf:li — re-render it canonically.
        return splice(
            content,
            vec![(block.span.clone(), render_description_block(&block.indent, desc_text))],
        );
    }

    // No existing dc:description — only proceed if we have text to add
    if desc_text.is_empty() {
        return content.to_string();
    }

    // Ensure xmlns:dc is declared, then inject the block.
    let xmlns_attr = if content.contains("xmlns:dc") {
        None
    } else {
        Some(format!(r#" xmlns:dc="{NS_DC}""#))
    };
    inject_block(
        content,
        &layout,
        xmlns_attr.as_deref(),
        |indent| render_description_block(indent, desc_text),
        Vec::new(),
    )
}

/// Update the `xmp:Label` value in an XMP file on disk.
///
/// Uses string-based find/replace to preserve all other XMP content byte-for-byte.
/// Returns `Ok(true)` if the file was modified, `Ok(false)` if no change was needed.
/// `None` removes the label attribute/element entirely (unlike rating which uses "0").
pub fn update_label(path: &Path, label: Option<&str>) -> Result<bool> {
    let content = std::fs::read_to_string(path)?;
    let modified = update_label_in_string(&content, label);
    if modified == content {
        return Ok(false);
    }
    std::fs::write(path, &modified)?;
    Ok(true)
}

/// Apply a label update to an XMP string, returning the modified string.
fn update_label_in_string(content: &str, label: Option<&str>) -> String {
    // Escape ONCE here — the raw label may contain XML-special
    // characters (`R&D`, `19" rack`); injecting it verbatim into the
    // attribute/element corrupted the document (property-test finding).
    let label_escaped = label.map(xml_escape).unwrap_or_default();
    let layout = locate(content);

    // Attribute form: xmp:Label="…"
    if let Some(attr) = layout.find_attr(NS_XMP, "Label", "xmp") {
        if label_escaped.is_empty() {
            // Remove the attribute, including its leading whitespace.
            return splice(content, vec![(attr.full_span.clone(), String::new())]);
        }
        if content[attr.value_span.clone()] == label_escaped {
            return content.to_string();
        }
        return splice(content, vec![(attr.value_span.clone(), label_escaped)]);
    }

    // Element form: <xmp:Label>…</xmp:Label>
    if let Some(block) = layout
        .find_prop(NS_XMP, "Label", &["xmp"])
        .filter(|b| !b.self_closing)
    {
        if label_escaped.is_empty() {
            // Remove the element line: leading indent + element + one
            // trailing newline.
            let mut span = block.span.clone();
            if content[span.end..].starts_with('\n') {
                span.end += 1;
            }
            return splice(content, vec![(span, String::new())]);
        }
        if content[block.text_span.clone()] == label_escaped {
            return content.to_string();
        }
        return splice(content, vec![(block.text_span.clone(), label_escaped)]);
    }

    // Neither form found — inject attribute if label is non-empty
    if label_escaped.is_empty() {
        return content.to_string();
    }

    // Inject xmp:Label attribute into the first rdf:Description element;
    // no rdf:Description → return unchanged.
    if let Some(desc) = layout.descriptions.first() {
        return splice(
            content,
            vec![(desc.name_end..desc.name_end, format!(r#" xmp:Label="{label_escaped}""#))],
        );
    }
    content.to_string()
}

/// Escape special XML characters in a string.
/// Escape text for embedding in XML. Covers both element text and
/// ATTRIBUTE values — `"` must be encoded because several fields
/// (`xmp:Label`, `xmp:Rating`) are written as double-quoted attributes;
/// an unescaped quote there truncates the attribute and corrupts the
/// whole document (found by the property tests: a label like `19" rack`
/// made `extract` lose every field in the file).
fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// Decode XML entity references that `xml_escape` would have produced,
/// plus the two common attribute-style entities `&quot;` / `&apos;` for
/// robustness against XMP written by other tools.
///
/// **Order matters**: `&amp;` must be decoded **last** so we don't
/// turn an encoded `&lt;` (which appears in the file as `&amp;lt;`
/// when nested-escaped) into a real `<` prematurely.
///
/// Required by the regex-based readers in `update_tags_in_string` and
/// `update_hierarchical_in_string`, which capture raw `<rdf:li>...
/// </rdf:li>` text — if the captured text isn't decoded before the
/// dedup / remove-set comparison, an entry like
/// `<rdf:li>Bobby &amp; the BigTones</rdf:li>` is treated as the
/// literal string `Bobby &amp; the BigTones`, never matches the
/// catalog's `Bobby & the BigTones`, and gets re-escaped on every
/// writeback round — producing the runaway `&amp;amp;amp;...`
/// nesting that accumulates one extra `amp;` layer per `maki
/// writeback --all` pass.
fn xml_unescape(s: &str) -> String {
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

/// Create a new XMP sidecar file from scratch with the given metadata.
///
/// Generates a well-formed XMP document suitable for CaptureOne, Lightroom,
/// and other tools that read `.xmp` sidecar files.
pub fn create_xmp(
    keywords: &[String],
    rating: Option<u8>,
    label: Option<&str>,
    description: Option<&str>,
) -> String {
    let mut parts = Vec::new();
    parts.push(r#"<?xml version="1.0" encoding="UTF-8"?>
<x:xmpmeta xmlns:x="adobe:ns:meta/">
 <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
  <rdf:Description rdf:about=""
    xmlns:dc="http://purl.org/dc/elements/1.1/"
    xmlns:xmp="http://ns.adobe.com/xap/1.0/"
    xmlns:lr="http://ns.adobe.com/lightroom/1.0/""#.to_string());
    if let Some(r) = rating {
        parts.push(format!("\n    xmp:Rating=\"{r}\""));
    }
    if let Some(l) = label {
        parts.push(format!("\n    xmp:Label=\"{}\"", xml_escape(l)));
    }
    parts.push(">".to_string());
    if !keywords.is_empty() {
        // dc:subject: flat individual component names (CaptureOne convention)
        let dc_components: Vec<String> = keywords.iter()
            .flat_map(|t| t.split('|').map(|s| s.to_string()))
            .collect::<std::collections::HashSet<_>>()
            .into_iter()
            .collect();
        parts.push("   <dc:subject>\n    <rdf:Bag>".to_string());
        for kw in &dc_components {
            parts.push(format!("     <rdf:li>{}</rdf:li>", xml_escape(kw)));
        }
        parts.push("    </rdf:Bag>\n   </dc:subject>".to_string());
        // lr:hierarchicalSubject: all ancestor paths (CaptureOne convention)
        let hier_tags: Vec<String> = crate::tag_util::expand_all_ancestors(keywords);
        if !hier_tags.is_empty() {
            parts.push("   <lr:hierarchicalSubject>\n    <rdf:Bag>".to_string());
            for kw in &hier_tags {
                parts.push(format!("     <rdf:li>{}</rdf:li>", xml_escape(kw)));
            }
            parts.push("    </rdf:Bag>\n   </lr:hierarchicalSubject>".to_string());
        }
    }
    if let Some(desc) = description {
        if !desc.is_empty() {
            parts.push(format!(
                "   <dc:description>\n    <rdf:Alt>\n     <rdf:li xml:lang=\"x-default\">{}</rdf:li>\n    </rdf:Alt>\n   </dc:description>",
                xml_escape(desc)
            ));
        }
    }
    parts.push("  </rdf:Description>\n </rdf:RDF>\n</x:xmpmeta>".to_string());
    parts.join("\n")
}

/// Parse XMP metadata from an XML string.
pub(crate) fn parse_xmp(xml: &str) -> XmpData {
    let mut data = XmpData::empty();
    let mut reader = Reader::from_str(xml);

    let mut context = Context::None;
    let mut in_li = false;
    let mut capture_rating = false;
    let mut capture_label = false;
    let mut text_buf = String::new();

    loop {
        match reader.read_event() {
            Ok(Event::Start(ref e)) => {
                let name = local_name(e.name().as_ref());
                handle_open_tag(
                    &name, e, &mut context, &mut in_li,
                    &mut capture_rating, &mut capture_label,
                    &mut text_buf, &mut data,
                );
            }
            Ok(Event::Empty(ref e)) => {
                let name = local_name(e.name().as_ref());
                handle_open_tag(
                    &name, e, &mut context, &mut in_li,
                    &mut capture_rating, &mut capture_label,
                    &mut text_buf, &mut data,
                );
            }
            Ok(Event::Text(ref e)) => {
                if let Ok(t) = e.unescape() {
                    if in_li || capture_rating || capture_label {
                        text_buf.push_str(&t);
                    }
                }
            }
            Ok(Event::End(ref e)) => {
                let name = local_name(e.name().as_ref());
                match name.as_slice() {
                    b"li" => {
                        if in_li {
                            let text = text_buf.trim().to_string();
                            if !text.is_empty() {
                                match context {
                                    Context::SubjectBag => {
                                        data.keywords.push(text);
                                    }
                                    Context::HierarchicalBag => {
                                        // Keep pipe-separated form as-is — `|` is the
                                        // internal hierarchy separator.
                                        data.hierarchical_keywords.push(text);
                                    }
                                    Context::DescriptionAlt => {
                                        if data.description.is_none() {
                                            data.description = Some(text);
                                        }
                                    }
                                    Context::CreatorContainer => {
                                        data.source_metadata
                                            .entry("creator".to_string())
                                            .or_insert(text);
                                    }
                                    Context::RightsAlt => {
                                        data.source_metadata
                                            .entry("copyright".to_string())
                                            .or_insert(text);
                                    }
                                    Context::None => {}
                                }
                            }
                            in_li = false;
                            text_buf.clear();
                        }
                    }
                    b"Rating" => {
                        if capture_rating {
                            let val = text_buf.trim().to_string();
                            if !val.is_empty() && val != "0" {
                                data.source_metadata.insert("rating".to_string(), val);
                            }
                            capture_rating = false;
                            text_buf.clear();
                        }
                    }
                    b"Label" => {
                        if capture_label {
                            let val = text_buf.trim().to_string();
                            if !val.is_empty() {
                                data.source_metadata.insert("label".to_string(), val);
                            }
                            capture_label = false;
                            text_buf.clear();
                        }
                    }
                    b"subject" | b"hierarchicalSubject" | b"description" | b"creator" | b"rights" => {
                        context = Context::None;
                    }
                    _ => {}
                }
            }
            Ok(Event::Eof) => break,
            Err(_) => break,
            _ => {}
        }
    }

    data
}

/// Handle a Start or Empty element event.
fn handle_open_tag(
    name: &[u8],
    e: &quick_xml::events::BytesStart<'_>,
    context: &mut Context,
    in_li: &mut bool,
    capture_rating: &mut bool,
    capture_label: &mut bool,
    text_buf: &mut String,
    data: &mut XmpData,
) {
    match name {
        b"Description" => {
            for attr in e.attributes().flatten() {
                let key = local_name(attr.key.as_ref());
                // Decode entity references — attribute values arrive raw
                // from quick-xml, and labels can legitimately contain
                // escaped quotes/ampersands (`19&quot; rack`, `R&amp;D`).
                // Reading them undecoded breaks the write→read round
                // trip (property-test finding).
                let val = attr
                    .unescape_value()
                    .map(|v| v.to_string())
                    .unwrap_or_else(|_| String::from_utf8_lossy(&attr.value).to_string());
                match key.as_slice() {
                    b"Rating" => {
                        if !val.is_empty() && val != "0" {
                            data.source_metadata.insert("rating".to_string(), val);
                        }
                    }
                    b"Label" => {
                        if !val.is_empty() {
                            data.source_metadata.insert("label".to_string(), val);
                        }
                    }
                    _ => {}
                }
            }
        }
        b"subject" => *context = Context::SubjectBag,
        b"hierarchicalSubject" => *context = Context::HierarchicalBag,
        b"description" => *context = Context::DescriptionAlt,
        b"creator" => *context = Context::CreatorContainer,
        b"rights" => *context = Context::RightsAlt,
        b"Rating" => {
            if !data.source_metadata.contains_key("rating") {
                *capture_rating = true;
                text_buf.clear();
            }
        }
        b"Label" => {
            if !data.source_metadata.contains_key("label") {
                *capture_label = true;
                text_buf.clear();
            }
        }
        b"li" => {
            if *context != Context::None {
                *in_li = true;
                text_buf.clear();
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    // ── locator tests ────────────────────────────────────────

    #[test]
    fn locator_description_and_prop_spans_round_trip() {
        let xmp = r#"<?xml version="1.0" encoding="UTF-8"?>
<x:xmpmeta xmlns:x="adobe:ns:meta/">
 <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
  <rdf:Description rdf:about=""
    xmlns:dc="http://purl.org/dc/elements/1.1/"
    xmlns:xmp="http://ns.adobe.com/xap/1.0/"
    xmlns:lr="http://ns.adobe.com/lightroom/1.0/"
    xmp:Rating="4"
    xmp:Label="Blue">
   <dc:subject>
    <rdf:Bag>
     <rdf:li>landscape</rdf:li>
     <rdf:li>black &amp; white</rdf:li>
    </rdf:Bag>
   </dc:subject>
   <lr:hierarchicalSubject>
    <rdf:Bag>
     <rdf:li>nature|sky</rdf:li>
    </rdf:Bag>
   </lr:hierarchicalSubject>
   <dc:description>
    <rdf:Alt>
     <rdf:li xml:lang="x-default">A sunset</rdf:li>
    </rdf:Alt>
   </dc:description>
  </rdf:Description>
 </rdf:RDF>
</x:xmpmeta>"#;

        let layout = locate(xmp);
        assert_eq!(layout.descriptions.len(), 1);
        let d = &layout.descriptions[0];
        assert!(xmp[d.start_span.clone()].starts_with("<rdf:Description"));
        assert!(xmp[d.start_span.clone()].ends_with('>'));
        assert!(!d.self_closing);
        assert_eq!(&xmp[d.name_end - 16..d.name_end], "<rdf:Description");
        let close = d.close_pos.unwrap();
        assert!(xmp[close..].starts_with("</rdf:Description>"));
        assert_eq!(&xmp[d.close_indent_start..close], "  ");

        // Attribute spans.
        let rating = layout.find_attr(NS_XMP, "Rating", "xmp").unwrap();
        assert_eq!(&xmp[rating.value_span.clone()], "4");
        assert!(xmp[rating.full_span.clone()].ends_with(r#"xmp:Rating="4""#));
        assert!(xmp[rating.full_span.clone()].starts_with('\n'));
        assert_eq!(rating.ns.as_deref(), Some(NS_XMP));

        // Subject block spans + decoded-comparable raw items.
        let s = layout.find_prop(NS_DC, "subject", &["dc"]).unwrap();
        assert_eq!(
            &xmp[s.span.clone()],
            "   <dc:subject>\n    <rdf:Bag>\n     <rdf:li>landscape</rdf:li>\n     <rdf:li>black &amp; white</rdf:li>\n    </rdf:Bag>\n   </dc:subject>"
        );
        assert_eq!(s.indent, "   ");
        assert_eq!(s.items.len(), 2);
        assert_eq!(&xmp[s.items[0].clone()], "landscape");
        assert_eq!(&xmp[s.items[1].clone()], "black &amp; white");

        // Lightroom + description blocks found by namespace URI.
        assert_eq!(layout.lightroom_blocks().len(), 1);
        let desc_block = layout.find_prop(NS_DC, "description", &["dc"]).unwrap();
        assert_eq!(desc_block.items.len(), 1);
        assert_eq!(&xmp[desc_block.items[0].clone()], "A sunset");
    }

    #[test]
    fn locator_self_closing_description() {
        let xmp = r#"<?xml version="1.0" encoding="UTF-8"?>
<x:xmpmeta xmlns:x="adobe:ns:meta/">
 <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
  <rdf:Description rdf:about=""
    xmlns:xmp="http://ns.adobe.com/xap/1.0/"
    xmp:Rating="5"/>
 </rdf:RDF>
</x:xmpmeta>"#;

        let layout = locate(xmp);
        assert_eq!(layout.descriptions.len(), 1);
        let d = &layout.descriptions[0];
        assert!(d.self_closing);
        assert!(d.close_pos.is_none());
        assert!(xmp[d.start_span.clone()].ends_with("/>"));
        let rating = layout.find_attr(NS_XMP, "Rating", "xmp").unwrap();
        assert_eq!(&xmp[rating.value_span.clone()], "5");
    }

    #[test]
    fn locator_finds_alien_lightroom_binding() {
        // Namespace-URI matching: any prefix bound to the Lightroom URI
        // is recognized; the conventional `lr`/`lightroom` prefixes are
        // recognized even without a declaration in scope.
        let xmp = r#"<?xml version="1.0" encoding="UTF-8"?>
<x:xmpmeta xmlns:x="adobe:ns:meta/">
 <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
  <rdf:Description rdf:about=""
    xmlns:lrc="http://ns.adobe.com/lightroom/1.0/">
   <lrc:hierarchicalSubject>
    <rdf:Bag>
     <rdf:li>nature|sky</rdf:li>
    </rdf:Bag>
   </lrc:hierarchicalSubject>
   <lightroom:hierarchicalSubject>
    <rdf:Bag>
     <rdf:li>Bavaria</rdf:li>
    </rdf:Bag>
   </lightroom:hierarchicalSubject>
  </rdf:Description>
 </rdf:RDF>
</x:xmpmeta>"#;

        let layout = locate(xmp);
        let blocks = layout.lightroom_blocks();
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0].prefix, "lrc");
        assert_eq!(blocks[1].prefix, "lightroom");
    }

    #[test]
    fn locator_resolves_namespace_over_prefix() {
        // A dc:subject written under an unconventional prefix bound to
        // the Dublin Core URI is still located (the old regex writers
        // keyed on the literal `dc:` prefix and missed this).
        let xmp = r#"<?xml version="1.0" encoding="UTF-8"?>
<x:xmpmeta xmlns:x="adobe:ns:meta/">
 <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
  <rdf:Description rdf:about=""
    xmlns:dcx="http://purl.org/dc/elements/1.1/">
   <dcx:subject>
    <rdf:Bag>
     <rdf:li>portrait</rdf:li>
    </rdf:Bag>
   </dcx:subject>
  </rdf:Description>
 </rdf:RDF>
</x:xmpmeta>"#;

        let layout = locate(xmp);
        let block = layout.find_prop(NS_DC, "subject", &["dc"]).unwrap();
        assert_eq!(block.prefix, "dcx");
        assert_eq!(block.qname(), "dcx:subject");
        assert_eq!(&xmp[block.items[0].clone()], "portrait");
    }

    #[test]
    fn comment_between_blocks_survives_update() {
        // Comments (and any other content outside the spliced spans)
        // must pass through untouched — the old regex writers could in
        // principle match inside them.
        let xmp = r#"<?xml version="1.0" encoding="UTF-8"?>
<x:xmpmeta xmlns:x="adobe:ns:meta/">
 <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
  <rdf:Description rdf:about=""
    xmlns:dc="http://purl.org/dc/elements/1.1/"
    xmlns:xmp="http://ns.adobe.com/xap/1.0/"
    xmp:Rating="2">
   <dc:subject>
    <rdf:Bag>
     <rdf:li>landscape</rdf:li>
    </rdf:Bag>
   </dc:subject>
   <!-- keep me: written by AcmeTool 1.2 -->
   <dc:description>
    <rdf:Alt>
     <rdf:li xml:lang="x-default">A sunset</rdf:li>
    </rdf:Alt>
   </dc:description>
  </rdf:Description>
 </rdf:RDF>
</x:xmpmeta>"#;

        // Tag update: only the dc:subject block changes; the comment and
        // everything after it stay byte-for-byte.
        let result = update_tags_in_string(xmp, &["ocean".to_string()], &[]);
        assert!(result.contains("<!-- keep me: written by AcmeTool 1.2 -->"));
        assert!(result.contains("<rdf:li>ocean</rdf:li>"));
        let tail = "   <!-- keep me: written by AcmeTool 1.2 -->\n   <dc:description>";
        assert!(result.contains(tail), "comment context must be untouched:\n{result}");

        // Rating update: everything except the attribute value is untouched.
        let result = update_rating_in_string(xmp, "5");
        assert_eq!(result, xmp.replace(r#"xmp:Rating="2""#, r#"xmp:Rating="5""#));
    }

    #[test]
    fn empty_file_returns_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("empty.xmp");
        std::fs::write(&path, "").unwrap();

        let data = extract(&path);
        assert!(data.keywords.is_empty());
        assert!(data.description.is_none());
        assert!(data.source_metadata.is_empty());
    }

    #[test]
    fn nonexistent_file_returns_empty() {
        let data = extract(&PathBuf::from("/nonexistent/file.xmp"));
        assert!(data.keywords.is_empty());
        assert!(data.description.is_none());
        assert!(data.source_metadata.is_empty());
    }

    #[test]
    fn full_xmp_extracts_all_fields() {
        let xmp = r#"<?xml version="1.0" encoding="UTF-8"?>
<x:xmpmeta xmlns:x="adobe:ns:meta/">
 <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
  <rdf:Description rdf:about=""
    xmlns:dc="http://purl.org/dc/elements/1.1/"
    xmlns:xmp="http://ns.adobe.com/xap/1.0/"
    xmp:Rating="4"
    xmp:Label="Blue">
   <dc:subject>
    <rdf:Bag>
     <rdf:li>landscape</rdf:li>
     <rdf:li>sunset</rdf:li>
     <rdf:li>ocean</rdf:li>
    </rdf:Bag>
   </dc:subject>
   <dc:description>
    <rdf:Alt>
     <rdf:li xml:lang="x-default">A beautiful sunset over the ocean</rdf:li>
    </rdf:Alt>
   </dc:description>
   <dc:creator>
    <rdf:Seq>
     <rdf:li>John Doe</rdf:li>
    </rdf:Seq>
   </dc:creator>
   <dc:rights>
    <rdf:Alt>
     <rdf:li xml:lang="x-default">Copyright 2024 John Doe</rdf:li>
    </rdf:Alt>
   </dc:rights>
  </rdf:Description>
 </rdf:RDF>
</x:xmpmeta>"#;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("full.xmp");
        std::fs::write(&path, xmp).unwrap();

        let data = extract(&path);
        assert_eq!(data.keywords, vec!["landscape", "sunset", "ocean"]);
        assert_eq!(
            data.description.as_deref(),
            Some("A beautiful sunset over the ocean")
        );
        assert_eq!(data.source_metadata.get("rating").unwrap(), "4");
        assert_eq!(data.source_metadata.get("label").unwrap(), "Blue");
        assert_eq!(data.source_metadata.get("creator").unwrap(), "John Doe");
        assert_eq!(
            data.source_metadata.get("copyright").unwrap(),
            "Copyright 2024 John Doe"
        );
    }

    #[test]
    fn partial_xmp_returns_available_fields() {
        let xmp = r#"<?xml version="1.0" encoding="UTF-8"?>
<x:xmpmeta xmlns:x="adobe:ns:meta/">
 <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
  <rdf:Description rdf:about=""
    xmlns:dc="http://purl.org/dc/elements/1.1/"
    xmlns:xmp="http://ns.adobe.com/xap/1.0/"
    xmp:Rating="3">
   <dc:subject>
    <rdf:Bag>
     <rdf:li>portrait</rdf:li>
    </rdf:Bag>
   </dc:subject>
  </rdf:Description>
 </rdf:RDF>
</x:xmpmeta>"#;

        let data = parse_xmp(xmp);
        assert_eq!(data.keywords, vec!["portrait"]);
        assert!(data.description.is_none());
        assert_eq!(data.source_metadata.get("rating").unwrap(), "3");
        assert!(!data.source_metadata.contains_key("label"));
        assert!(!data.source_metadata.contains_key("creator"));
        assert!(!data.source_metadata.contains_key("copyright"));
    }

    #[test]
    fn attributes_on_rdf_description() {
        let xmp = r#"<?xml version="1.0" encoding="UTF-8"?>
<x:xmpmeta xmlns:x="adobe:ns:meta/">
 <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
  <rdf:Description rdf:about=""
    xmlns:xmp="http://ns.adobe.com/xap/1.0/"
    xmp:Rating="5"
    xmp:Label="Red"/>
 </rdf:RDF>
</x:xmpmeta>"#;

        let data = parse_xmp(xmp);
        assert_eq!(data.source_metadata.get("rating").unwrap(), "5");
        assert_eq!(data.source_metadata.get("label").unwrap(), "Red");
    }

    #[test]
    fn element_form_rating_and_label() {
        let xmp = r#"<?xml version="1.0" encoding="UTF-8"?>
<x:xmpmeta xmlns:x="adobe:ns:meta/">
 <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
  <rdf:Description rdf:about=""
    xmlns:xmp="http://ns.adobe.com/xap/1.0/">
   <xmp:Rating>2</xmp:Rating>
   <xmp:Label>Green</xmp:Label>
  </rdf:Description>
 </rdf:RDF>
</x:xmpmeta>"#;

        let data = parse_xmp(xmp);
        assert_eq!(data.source_metadata.get("rating").unwrap(), "2");
        assert_eq!(data.source_metadata.get("label").unwrap(), "Green");
    }

    // ── hierarchical subject tests ──────────────────────────

    #[test]
    fn parse_hierarchical_subject() {
        let xmp = r#"<?xml version="1.0" encoding="UTF-8"?>
<x:xmpmeta xmlns:x="adobe:ns:meta/">
 <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
  <rdf:Description rdf:about=""
    xmlns:dc="http://purl.org/dc/elements/1.1/"
    xmlns:lr="http://ns.adobe.com/lightroom/1.0/">
   <dc:subject>
    <rdf:Bag>
     <rdf:li>animals</rdf:li>
     <rdf:li>birds</rdf:li>
     <rdf:li>eagles</rdf:li>
     <rdf:li>sunset</rdf:li>
    </rdf:Bag>
   </dc:subject>
   <lr:hierarchicalSubject>
    <rdf:Bag>
     <rdf:li>animals|birds|eagles</rdf:li>
     <rdf:li>nature|sky|sunset</rdf:li>
    </rdf:Bag>
   </lr:hierarchicalSubject>
  </rdf:Description>
 </rdf:RDF>
</x:xmpmeta>"#;

        let data = parse_xmp(xmp);
        assert_eq!(data.keywords, vec!["animals", "birds", "eagles", "sunset"]);
        assert_eq!(
            data.hierarchical_keywords,
            vec!["animals|birds|eagles", "nature|sky|sunset"]
        );
    }

    #[test]
    fn parse_hierarchical_subject_single_level() {
        let xmp = r#"<?xml version="1.0" encoding="UTF-8"?>
<x:xmpmeta xmlns:x="adobe:ns:meta/">
 <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
  <rdf:Description rdf:about=""
    xmlns:lr="http://ns.adobe.com/lightroom/1.0/">
   <lr:hierarchicalSubject>
    <rdf:Bag>
     <rdf:li>landscape</rdf:li>
    </rdf:Bag>
   </lr:hierarchicalSubject>
  </rdf:Description>
 </rdf:RDF>
</x:xmpmeta>"#;

        let data = parse_xmp(xmp);
        assert!(data.keywords.is_empty());
        assert_eq!(data.hierarchical_keywords, vec!["landscape"]);
    }

    #[test]
    fn parse_no_hierarchical_subject() {
        let xmp = r#"<?xml version="1.0" encoding="UTF-8"?>
<x:xmpmeta xmlns:x="adobe:ns:meta/">
 <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
  <rdf:Description rdf:about=""
    xmlns:dc="http://purl.org/dc/elements/1.1/">
   <dc:subject>
    <rdf:Bag>
     <rdf:li>landscape</rdf:li>
    </rdf:Bag>
   </dc:subject>
  </rdf:Description>
 </rdf:RDF>
</x:xmpmeta>"#;

        let data = parse_xmp(xmp);
        assert_eq!(data.keywords, vec!["landscape"]);
        assert!(data.hierarchical_keywords.is_empty());
    }

    // ── update_rating tests ──────────────────────────────────

    #[test]
    fn update_rating_attribute_form() {
        let xmp = r#"<?xml version="1.0" encoding="UTF-8"?>
<x:xmpmeta xmlns:x="adobe:ns:meta/">
 <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
  <rdf:Description rdf:about=""
    xmlns:xmp="http://ns.adobe.com/xap/1.0/"
    xmp:Rating="3"
    xmp:Label="Blue">
  </rdf:Description>
 </rdf:RDF>
</x:xmpmeta>"#;

        let result = update_rating_in_string(xmp, "5");
        assert!(result.contains(r#"xmp:Rating="5""#));
        assert!(result.contains(r#"xmp:Label="Blue""#));
        assert!(!result.contains(r#"xmp:Rating="3""#));
    }

    #[test]
    fn update_rating_element_form() {
        let xmp = r#"<?xml version="1.0" encoding="UTF-8"?>
<x:xmpmeta xmlns:x="adobe:ns:meta/">
 <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
  <rdf:Description rdf:about=""
    xmlns:xmp="http://ns.adobe.com/xap/1.0/">
   <xmp:Rating>2</xmp:Rating>
   <xmp:Label>Green</xmp:Label>
  </rdf:Description>
 </rdf:RDF>
</x:xmpmeta>"#;

        let result = update_rating_in_string(xmp, "4");
        assert!(result.contains("<xmp:Rating>4</xmp:Rating>"));
        assert!(result.contains("<xmp:Label>Green</xmp:Label>"));
        assert!(!result.contains("<xmp:Rating>2</xmp:Rating>"));
    }

    #[test]
    fn update_rating_inject_when_missing() {
        let xmp = r#"<?xml version="1.0" encoding="UTF-8"?>
<x:xmpmeta xmlns:x="adobe:ns:meta/">
 <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
  <rdf:Description rdf:about=""
    xmlns:xmp="http://ns.adobe.com/xap/1.0/"
    xmp:Label="Red">
  </rdf:Description>
 </rdf:RDF>
</x:xmpmeta>"#;

        let result = update_rating_in_string(xmp, "3");
        assert!(result.contains(r#"xmp:Rating="3""#));
        assert!(result.contains(r#"xmp:Label="Red""#));
    }

    #[test]
    fn update_rating_clear_sets_zero() {
        let xmp = r#"<?xml version="1.0" encoding="UTF-8"?>
<x:xmpmeta xmlns:x="adobe:ns:meta/">
 <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
  <rdf:Description rdf:about=""
    xmlns:xmp="http://ns.adobe.com/xap/1.0/"
    xmp:Rating="4">
  </rdf:Description>
 </rdf:RDF>
</x:xmpmeta>"#;

        let result = update_rating_in_string(xmp, "0");
        assert!(result.contains(r#"xmp:Rating="0""#));
        assert!(!result.contains(r#"xmp:Rating="4""#));
    }

    #[test]
    fn update_rating_no_inject_when_clearing() {
        let xmp = r#"<?xml version="1.0" encoding="UTF-8"?>
<x:xmpmeta xmlns:x="adobe:ns:meta/">
 <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
  <rdf:Description rdf:about=""
    xmlns:xmp="http://ns.adobe.com/xap/1.0/">
  </rdf:Description>
 </rdf:RDF>
</x:xmpmeta>"#;

        let result = update_rating_in_string(xmp, "0");
        // Should not inject xmp:Rating="0" when there's no existing rating
        assert!(!result.contains("xmp:Rating"));
        assert_eq!(result, xmp);
    }

    #[test]
    fn update_rating_preserves_other_content() {
        let xmp = r#"<?xml version="1.0" encoding="UTF-8"?>
<x:xmpmeta xmlns:x="adobe:ns:meta/">
 <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
  <rdf:Description rdf:about=""
    xmlns:dc="http://purl.org/dc/elements/1.1/"
    xmlns:xmp="http://ns.adobe.com/xap/1.0/"
    xmp:Rating="2"
    xmp:Label="Blue">
   <dc:subject>
    <rdf:Bag>
     <rdf:li>landscape</rdf:li>
     <rdf:li>sunset</rdf:li>
    </rdf:Bag>
   </dc:subject>
   <dc:description>
    <rdf:Alt>
     <rdf:li xml:lang="x-default">A beautiful sunset</rdf:li>
    </rdf:Alt>
   </dc:description>
  </rdf:Description>
 </rdf:RDF>
</x:xmpmeta>"#;

        let result = update_rating_in_string(xmp, "5");
        assert!(result.contains(r#"xmp:Rating="5""#));
        assert!(result.contains(r#"xmp:Label="Blue""#));
        assert!(result.contains("<rdf:li>landscape</rdf:li>"));
        assert!(result.contains("<rdf:li>sunset</rdf:li>"));
        assert!(result.contains("A beautiful sunset"));
    }

    #[test]
    fn update_rating_file_on_disk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.xmp");
        let xmp = r#"<?xml version="1.0" encoding="UTF-8"?>
<x:xmpmeta xmlns:x="adobe:ns:meta/">
 <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
  <rdf:Description rdf:about=""
    xmlns:xmp="http://ns.adobe.com/xap/1.0/"
    xmp:Rating="1">
  </rdf:Description>
 </rdf:RDF>
</x:xmpmeta>"#;
        std::fs::write(&path, xmp).unwrap();

        let modified = update_rating(&path, Some(4)).unwrap();
        assert!(modified);

        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains(r#"xmp:Rating="4""#));
    }

    #[test]
    fn update_rating_nonexistent_file() {
        let result = update_rating(Path::new("/nonexistent/file.xmp"), Some(3));
        assert!(result.is_err());
    }

    #[test]
    fn update_rating_no_change_returns_false() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.xmp");
        let xmp = r#"<?xml version="1.0" encoding="UTF-8"?>
<x:xmpmeta xmlns:x="adobe:ns:meta/">
 <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
  <rdf:Description rdf:about=""
    xmlns:xmp="http://ns.adobe.com/xap/1.0/"
    xmp:Rating="3">
  </rdf:Description>
 </rdf:RDF>
</x:xmpmeta>"#;
        std::fs::write(&path, xmp).unwrap();

        let modified = update_rating(&path, Some(3)).unwrap();
        assert!(!modified);
    }

    // ── update_tags tests ────────────────────────────────────

    #[test]
    fn update_tags_add_to_existing() {
        let xmp = r#"<?xml version="1.0" encoding="UTF-8"?>
<x:xmpmeta xmlns:x="adobe:ns:meta/">
 <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
  <rdf:Description rdf:about=""
    xmlns:dc="http://purl.org/dc/elements/1.1/"
    xmlns:xmp="http://ns.adobe.com/xap/1.0/">
   <dc:subject>
    <rdf:Bag>
     <rdf:li>landscape</rdf:li>
     <rdf:li>sunset</rdf:li>
    </rdf:Bag>
   </dc:subject>
  </rdf:Description>
 </rdf:RDF>
</x:xmpmeta>"#;

        let result = update_tags_in_string(
            xmp,
            &["ocean".to_string()],
            &[],
        );
        assert!(result.contains("<rdf:li>landscape</rdf:li>"));
        assert!(result.contains("<rdf:li>sunset</rdf:li>"));
        assert!(result.contains("<rdf:li>ocean</rdf:li>"));
    }

    #[test]
    fn update_tags_remove_from_existing() {
        let xmp = r#"<?xml version="1.0" encoding="UTF-8"?>
<x:xmpmeta xmlns:x="adobe:ns:meta/">
 <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
  <rdf:Description rdf:about=""
    xmlns:dc="http://purl.org/dc/elements/1.1/">
   <dc:subject>
    <rdf:Bag>
     <rdf:li>landscape</rdf:li>
     <rdf:li>sunset</rdf:li>
     <rdf:li>ocean</rdf:li>
    </rdf:Bag>
   </dc:subject>
  </rdf:Description>
 </rdf:RDF>
</x:xmpmeta>"#;

        let result = update_tags_in_string(
            xmp,
            &[],
            &["sunset".to_string()],
        );
        assert!(result.contains("<rdf:li>landscape</rdf:li>"));
        assert!(!result.contains("<rdf:li>sunset</rdf:li>"));
        assert!(result.contains("<rdf:li>ocean</rdf:li>"));
    }

    #[test]
    fn update_tags_add_and_remove() {
        let xmp = r#"<?xml version="1.0" encoding="UTF-8"?>
<x:xmpmeta xmlns:x="adobe:ns:meta/">
 <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
  <rdf:Description rdf:about=""
    xmlns:dc="http://purl.org/dc/elements/1.1/">
   <dc:subject>
    <rdf:Bag>
     <rdf:li>landscape</rdf:li>
     <rdf:li>sunset</rdf:li>
    </rdf:Bag>
   </dc:subject>
  </rdf:Description>
 </rdf:RDF>
</x:xmpmeta>"#;

        let result = update_tags_in_string(
            xmp,
            &["mountains".to_string()],
            &["sunset".to_string()],
        );
        assert!(result.contains("<rdf:li>landscape</rdf:li>"));
        assert!(!result.contains("<rdf:li>sunset</rdf:li>"));
        assert!(result.contains("<rdf:li>mountains</rdf:li>"));
    }

    #[test]
    fn update_tags_remove_all_removes_block() {
        let xmp = r#"<?xml version="1.0" encoding="UTF-8"?>
<x:xmpmeta xmlns:x="adobe:ns:meta/">
 <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
  <rdf:Description rdf:about=""
    xmlns:dc="http://purl.org/dc/elements/1.1/"
    xmp:Rating="3">
   <dc:subject>
    <rdf:Bag>
     <rdf:li>landscape</rdf:li>
    </rdf:Bag>
   </dc:subject>
  </rdf:Description>
 </rdf:RDF>
</x:xmpmeta>"#;

        let result = update_tags_in_string(
            xmp,
            &[],
            &["landscape".to_string()],
        );
        assert!(!result.contains("dc:subject"));
        assert!(!result.contains("rdf:Bag"));
        assert!(!result.contains("landscape"));
        // Other content preserved
        assert!(result.contains("xmp:Rating"));
    }

    #[test]
    fn update_tags_inject_when_no_subject() {
        let xmp = r#"<?xml version="1.0" encoding="UTF-8"?>
<x:xmpmeta xmlns:x="adobe:ns:meta/">
 <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
  <rdf:Description rdf:about=""
    xmlns:dc="http://purl.org/dc/elements/1.1/"
    xmlns:xmp="http://ns.adobe.com/xap/1.0/"
    xmp:Rating="3">
  </rdf:Description>
 </rdf:RDF>
</x:xmpmeta>"#;

        let result = update_tags_in_string(
            xmp,
            &["landscape".to_string(), "sunset".to_string()],
            &[],
        );
        assert!(result.contains("<dc:subject>"));
        assert!(result.contains("<rdf:li>landscape</rdf:li>"));
        assert!(result.contains("<rdf:li>sunset</rdf:li>"));
        assert!(result.contains("xmp:Rating"));
    }

    #[test]
    fn update_tags_inject_adds_xmlns_dc() {
        let xmp = r#"<?xml version="1.0" encoding="UTF-8"?>
<x:xmpmeta xmlns:x="adobe:ns:meta/">
 <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
  <rdf:Description rdf:about=""
    xmlns:xmp="http://ns.adobe.com/xap/1.0/"
    xmp:Rating="3">
  </rdf:Description>
 </rdf:RDF>
</x:xmpmeta>"#;

        let result = update_tags_in_string(
            xmp,
            &["portrait".to_string()],
            &[],
        );
        assert!(result.contains("xmlns:dc"));
        assert!(result.contains("<rdf:li>portrait</rdf:li>"));
    }

    #[test]
    fn update_tags_inject_self_closing_description() {
        let xmp = r#"<?xml version="1.0" encoding="UTF-8"?>
<x:xmpmeta xmlns:x="adobe:ns:meta/">
 <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
  <rdf:Description rdf:about=""
    xmlns:xmp="http://ns.adobe.com/xap/1.0/"
    xmp:Rating="3"/>
 </rdf:RDF>
</x:xmpmeta>"#;

        let result = update_tags_in_string(
            xmp,
            &["nature".to_string()],
            &[],
        );
        assert!(result.contains("xmlns:dc"));
        assert!(result.contains("<rdf:li>nature</rdf:li>"));
        assert!(result.contains("</rdf:Description>"));
        assert!(!result.contains("/>"));
    }

    #[test]
    fn update_tags_no_change_add_existing() {
        let xmp = r#"<?xml version="1.0" encoding="UTF-8"?>
<x:xmpmeta xmlns:x="adobe:ns:meta/">
 <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
  <rdf:Description rdf:about=""
    xmlns:dc="http://purl.org/dc/elements/1.1/">
   <dc:subject>
    <rdf:Bag>
     <rdf:li>landscape</rdf:li>
    </rdf:Bag>
   </dc:subject>
  </rdf:Description>
 </rdf:RDF>
</x:xmpmeta>"#;

        let result = update_tags_in_string(
            xmp,
            &["landscape".to_string()],
            &[],
        );
        // Should still contain the tag, and the content should round-trip
        assert!(result.contains("<rdf:li>landscape</rdf:li>"));
    }

    #[test]
    fn update_tags_remove_nonexistent_is_noop() {
        let xmp = r#"<?xml version="1.0" encoding="UTF-8"?>
<x:xmpmeta xmlns:x="adobe:ns:meta/">
 <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
  <rdf:Description rdf:about=""
    xmlns:dc="http://purl.org/dc/elements/1.1/">
   <dc:subject>
    <rdf:Bag>
     <rdf:li>landscape</rdf:li>
    </rdf:Bag>
   </dc:subject>
  </rdf:Description>
 </rdf:RDF>
</x:xmpmeta>"#;

        let result = update_tags_in_string(
            xmp,
            &[],
            &["nonexistent".to_string()],
        );
        assert!(result.contains("<rdf:li>landscape</rdf:li>"));
    }

    #[test]
    fn update_tags_preserves_other_content() {
        let xmp = r#"<?xml version="1.0" encoding="UTF-8"?>
<x:xmpmeta xmlns:x="adobe:ns:meta/">
 <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
  <rdf:Description rdf:about=""
    xmlns:dc="http://purl.org/dc/elements/1.1/"
    xmlns:xmp="http://ns.adobe.com/xap/1.0/"
    xmp:Rating="4"
    xmp:Label="Blue">
   <dc:subject>
    <rdf:Bag>
     <rdf:li>landscape</rdf:li>
    </rdf:Bag>
   </dc:subject>
   <dc:description>
    <rdf:Alt>
     <rdf:li xml:lang="x-default">A beautiful sunset</rdf:li>
    </rdf:Alt>
   </dc:description>
  </rdf:Description>
 </rdf:RDF>
</x:xmpmeta>"#;

        let result = update_tags_in_string(
            xmp,
            &["ocean".to_string()],
            &[],
        );
        assert!(result.contains(r#"xmp:Rating="4""#));
        assert!(result.contains(r#"xmp:Label="Blue""#));
        assert!(result.contains("A beautiful sunset"));
        assert!(result.contains("<rdf:li>landscape</rdf:li>"));
        assert!(result.contains("<rdf:li>ocean</rdf:li>"));
    }

    #[test]
    fn update_tags_file_on_disk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.xmp");
        let xmp = r#"<?xml version="1.0" encoding="UTF-8"?>
<x:xmpmeta xmlns:x="adobe:ns:meta/">
 <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
  <rdf:Description rdf:about=""
    xmlns:dc="http://purl.org/dc/elements/1.1/">
   <dc:subject>
    <rdf:Bag>
     <rdf:li>landscape</rdf:li>
    </rdf:Bag>
   </dc:subject>
  </rdf:Description>
 </rdf:RDF>
</x:xmpmeta>"#;
        std::fs::write(&path, xmp).unwrap();

        let modified = update_tags(&path, &["ocean".to_string()], &["landscape".to_string()]).unwrap();
        assert!(modified);

        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("<rdf:li>ocean</rdf:li>"));
        assert!(!content.contains("<rdf:li>landscape</rdf:li>"));
    }

    #[test]
    fn update_tags_xml_escapes_special_chars() {
        let xmp = r#"<?xml version="1.0" encoding="UTF-8"?>
<x:xmpmeta xmlns:x="adobe:ns:meta/">
 <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
  <rdf:Description rdf:about=""
    xmlns:dc="http://purl.org/dc/elements/1.1/">
   <dc:subject>
    <rdf:Bag>
     <rdf:li>existing</rdf:li>
    </rdf:Bag>
   </dc:subject>
  </rdf:Description>
 </rdf:RDF>
</x:xmpmeta>"#;

        let result = update_tags_in_string(
            xmp,
            &["black & white".to_string()],
            &[],
        );
        assert!(result.contains("<rdf:li>black &amp; white</rdf:li>"));
    }

    // ── update_description tests ──────────────────────────────

    #[test]
    fn update_description_existing_block() {
        let xmp = r#"<?xml version="1.0" encoding="UTF-8"?>
<x:xmpmeta xmlns:x="adobe:ns:meta/">
 <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
  <rdf:Description rdf:about=""
    xmlns:dc="http://purl.org/dc/elements/1.1/"
    xmlns:xmp="http://ns.adobe.com/xap/1.0/"
    xmp:Rating="3">
   <dc:description>
    <rdf:Alt>
     <rdf:li xml:lang="x-default">Old description</rdf:li>
    </rdf:Alt>
   </dc:description>
  </rdf:Description>
 </rdf:RDF>
</x:xmpmeta>"#;

        let result = update_description_in_string(xmp, Some("New description"));
        assert!(result.contains("New description"));
        assert!(!result.contains("Old description"));
        assert!(result.contains(r#"xmp:Rating="3""#));
    }

    #[test]
    fn update_description_clear_removes_block() {
        let xmp = r#"<?xml version="1.0" encoding="UTF-8"?>
<x:xmpmeta xmlns:x="adobe:ns:meta/">
 <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
  <rdf:Description rdf:about=""
    xmlns:dc="http://purl.org/dc/elements/1.1/"
    xmp:Rating="4">
   <dc:description>
    <rdf:Alt>
     <rdf:li xml:lang="x-default">Remove me</rdf:li>
    </rdf:Alt>
   </dc:description>
  </rdf:Description>
 </rdf:RDF>
</x:xmpmeta>"#;

        let result = update_description_in_string(xmp, None);
        assert!(!result.contains("dc:description"));
        assert!(!result.contains("Remove me"));
        assert!(result.contains("xmp:Rating"));
    }

    #[test]
    fn update_description_clear_with_empty_string() {
        let xmp = r#"<?xml version="1.0" encoding="UTF-8"?>
<x:xmpmeta xmlns:x="adobe:ns:meta/">
 <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
  <rdf:Description rdf:about=""
    xmlns:dc="http://purl.org/dc/elements/1.1/">
   <dc:description>
    <rdf:Alt>
     <rdf:li xml:lang="x-default">Remove me</rdf:li>
    </rdf:Alt>
   </dc:description>
  </rdf:Description>
 </rdf:RDF>
</x:xmpmeta>"#;

        let result = update_description_in_string(xmp, Some(""));
        assert!(!result.contains("dc:description"));
    }

    #[test]
    fn update_description_inject_when_missing() {
        let xmp = r#"<?xml version="1.0" encoding="UTF-8"?>
<x:xmpmeta xmlns:x="adobe:ns:meta/">
 <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
  <rdf:Description rdf:about=""
    xmlns:dc="http://purl.org/dc/elements/1.1/"
    xmlns:xmp="http://ns.adobe.com/xap/1.0/"
    xmp:Rating="3">
  </rdf:Description>
 </rdf:RDF>
</x:xmpmeta>"#;

        let result = update_description_in_string(xmp, Some("Injected description"));
        assert!(result.contains("dc:description"));
        assert!(result.contains("Injected description"));
        assert!(result.contains("rdf:Alt"));
        assert!(result.contains(r#"xml:lang="x-default""#));
        assert!(result.contains("xmp:Rating"));
    }

    #[test]
    fn update_description_inject_adds_xmlns_dc() {
        let xmp = r#"<?xml version="1.0" encoding="UTF-8"?>
<x:xmpmeta xmlns:x="adobe:ns:meta/">
 <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
  <rdf:Description rdf:about=""
    xmlns:xmp="http://ns.adobe.com/xap/1.0/"
    xmp:Rating="3">
  </rdf:Description>
 </rdf:RDF>
</x:xmpmeta>"#;

        let result = update_description_in_string(xmp, Some("New desc"));
        assert!(result.contains("xmlns:dc"));
        assert!(result.contains("New desc"));
    }

    #[test]
    fn update_description_inject_self_closing() {
        let xmp = r#"<?xml version="1.0" encoding="UTF-8"?>
<x:xmpmeta xmlns:x="adobe:ns:meta/">
 <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
  <rdf:Description rdf:about=""
    xmlns:xmp="http://ns.adobe.com/xap/1.0/"
    xmp:Rating="3"/>
 </rdf:RDF>
</x:xmpmeta>"#;

        let result = update_description_in_string(xmp, Some("Self-closing test"));
        assert!(result.contains("xmlns:dc"));
        assert!(result.contains("Self-closing test"));
        assert!(result.contains("</rdf:Description>"));
        assert!(!result.contains("/>"));
    }

    #[test]
    fn update_description_preserves_other_content() {
        let xmp = r#"<?xml version="1.0" encoding="UTF-8"?>
<x:xmpmeta xmlns:x="adobe:ns:meta/">
 <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
  <rdf:Description rdf:about=""
    xmlns:dc="http://purl.org/dc/elements/1.1/"
    xmlns:xmp="http://ns.adobe.com/xap/1.0/"
    xmp:Rating="4"
    xmp:Label="Blue">
   <dc:subject>
    <rdf:Bag>
     <rdf:li>landscape</rdf:li>
     <rdf:li>sunset</rdf:li>
    </rdf:Bag>
   </dc:subject>
   <dc:description>
    <rdf:Alt>
     <rdf:li xml:lang="x-default">A beautiful sunset</rdf:li>
    </rdf:Alt>
   </dc:description>
  </rdf:Description>
 </rdf:RDF>
</x:xmpmeta>"#;

        let result = update_description_in_string(xmp, Some("Updated sunset"));
        assert!(result.contains(r#"xmp:Rating="4""#));
        assert!(result.contains(r#"xmp:Label="Blue""#));
        assert!(result.contains("<rdf:li>landscape</rdf:li>"));
        assert!(result.contains("<rdf:li>sunset</rdf:li>"));
        assert!(result.contains("Updated sunset"));
        assert!(!result.contains("A beautiful sunset"));
    }

    #[test]
    fn update_description_xml_escapes() {
        let xmp = r#"<?xml version="1.0" encoding="UTF-8"?>
<x:xmpmeta xmlns:x="adobe:ns:meta/">
 <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
  <rdf:Description rdf:about=""
    xmlns:dc="http://purl.org/dc/elements/1.1/">
   <dc:description>
    <rdf:Alt>
     <rdf:li xml:lang="x-default">old</rdf:li>
    </rdf:Alt>
   </dc:description>
  </rdf:Description>
 </rdf:RDF>
</x:xmpmeta>"#;

        let result = update_description_in_string(xmp, Some("black & white <nice>"));
        assert!(result.contains("black &amp; white &lt;nice&gt;"));
    }

    #[test]
    fn update_description_file_on_disk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.xmp");
        let xmp = r#"<?xml version="1.0" encoding="UTF-8"?>
<x:xmpmeta xmlns:x="adobe:ns:meta/">
 <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
  <rdf:Description rdf:about=""
    xmlns:dc="http://purl.org/dc/elements/1.1/">
   <dc:description>
    <rdf:Alt>
     <rdf:li xml:lang="x-default">Original</rdf:li>
    </rdf:Alt>
   </dc:description>
  </rdf:Description>
 </rdf:RDF>
</x:xmpmeta>"#;
        std::fs::write(&path, xmp).unwrap();

        let modified = update_description(&path, Some("Updated")).unwrap();
        assert!(modified);

        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("Updated"));
        assert!(!content.contains("Original"));
    }

    #[test]
    fn update_description_no_change_returns_false() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.xmp");
        let xmp = r#"<?xml version="1.0" encoding="UTF-8"?>
<x:xmpmeta xmlns:x="adobe:ns:meta/">
 <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
  <rdf:Description rdf:about=""
    xmlns:dc="http://purl.org/dc/elements/1.1/">
   <dc:description>
    <rdf:Alt>
     <rdf:li xml:lang="x-default">Same text</rdf:li>
    </rdf:Alt>
   </dc:description>
  </rdf:Description>
 </rdf:RDF>
</x:xmpmeta>"#;
        std::fs::write(&path, xmp).unwrap();

        let modified = update_description(&path, Some("Same text")).unwrap();
        assert!(!modified);
    }

    #[test]
    fn update_description_none_no_existing_is_noop() {
        let xmp = r#"<?xml version="1.0" encoding="UTF-8"?>
<x:xmpmeta xmlns:x="adobe:ns:meta/">
 <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
  <rdf:Description rdf:about=""
    xmlns:xmp="http://ns.adobe.com/xap/1.0/"
    xmp:Rating="3">
  </rdf:Description>
 </rdf:RDF>
</x:xmpmeta>"#;

        let result = update_description_in_string(xmp, None);
        assert_eq!(result, xmp);
    }

    // ── update_label tests ──────────────────────────────────

    #[test]
    fn update_label_attribute_form() {
        let xmp = r#"<?xml version="1.0" encoding="UTF-8"?>
<x:xmpmeta xmlns:x="adobe:ns:meta/">
 <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
  <rdf:Description rdf:about=""
    xmlns:xmp="http://ns.adobe.com/xap/1.0/"
    xmp:Rating="3"
    xmp:Label="Blue">
  </rdf:Description>
 </rdf:RDF>
</x:xmpmeta>"#;

        let result = update_label_in_string(xmp, Some("Red"));
        assert!(result.contains(r#"xmp:Label="Red""#));
        assert!(!result.contains(r#"xmp:Label="Blue""#));
        assert!(result.contains(r#"xmp:Rating="3""#));
    }

    #[test]
    fn update_label_element_form() {
        let xmp = r#"<?xml version="1.0" encoding="UTF-8"?>
<x:xmpmeta xmlns:x="adobe:ns:meta/">
 <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
  <rdf:Description rdf:about=""
    xmlns:xmp="http://ns.adobe.com/xap/1.0/">
   <xmp:Rating>2</xmp:Rating>
   <xmp:Label>Green</xmp:Label>
  </rdf:Description>
 </rdf:RDF>
</x:xmpmeta>"#;

        let result = update_label_in_string(xmp, Some("Yellow"));
        assert!(result.contains("<xmp:Label>Yellow</xmp:Label>"));
        assert!(!result.contains("<xmp:Label>Green</xmp:Label>"));
        assert!(result.contains("<xmp:Rating>2</xmp:Rating>"));
    }

    #[test]
    fn update_label_clear_removes_attribute() {
        let xmp = r#"<?xml version="1.0" encoding="UTF-8"?>
<x:xmpmeta xmlns:x="adobe:ns:meta/">
 <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
  <rdf:Description rdf:about=""
    xmlns:xmp="http://ns.adobe.com/xap/1.0/"
    xmp:Rating="4"
    xmp:Label="Blue">
  </rdf:Description>
 </rdf:RDF>
</x:xmpmeta>"#;

        let result = update_label_in_string(xmp, None);
        assert!(!result.contains("xmp:Label"));
        assert!(result.contains(r#"xmp:Rating="4""#));
    }

    #[test]
    fn update_label_clear_removes_element() {
        let xmp = r#"<?xml version="1.0" encoding="UTF-8"?>
<x:xmpmeta xmlns:x="adobe:ns:meta/">
 <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
  <rdf:Description rdf:about=""
    xmlns:xmp="http://ns.adobe.com/xap/1.0/">
   <xmp:Rating>2</xmp:Rating>
   <xmp:Label>Green</xmp:Label>
  </rdf:Description>
 </rdf:RDF>
</x:xmpmeta>"#;

        let result = update_label_in_string(xmp, None);
        assert!(!result.contains("xmp:Label"));
        assert!(result.contains("<xmp:Rating>2</xmp:Rating>"));
    }

    #[test]
    fn update_label_inject_when_missing() {
        let xmp = r#"<?xml version="1.0" encoding="UTF-8"?>
<x:xmpmeta xmlns:x="adobe:ns:meta/">
 <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
  <rdf:Description rdf:about=""
    xmlns:xmp="http://ns.adobe.com/xap/1.0/"
    xmp:Rating="3">
  </rdf:Description>
 </rdf:RDF>
</x:xmpmeta>"#;

        let result = update_label_in_string(xmp, Some("Red"));
        assert!(result.contains(r#"xmp:Label="Red""#));
        assert!(result.contains(r#"xmp:Rating="3""#));
    }

    #[test]
    fn update_label_no_inject_when_clearing() {
        let xmp = r#"<?xml version="1.0" encoding="UTF-8"?>
<x:xmpmeta xmlns:x="adobe:ns:meta/">
 <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
  <rdf:Description rdf:about=""
    xmlns:xmp="http://ns.adobe.com/xap/1.0/"
    xmp:Rating="3">
  </rdf:Description>
 </rdf:RDF>
</x:xmpmeta>"#;

        let result = update_label_in_string(xmp, None);
        assert!(!result.contains("xmp:Label"));
        assert_eq!(result, xmp);
    }

    #[test]
    fn update_label_file_on_disk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.xmp");
        let xmp = r#"<?xml version="1.0" encoding="UTF-8"?>
<x:xmpmeta xmlns:x="adobe:ns:meta/">
 <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
  <rdf:Description rdf:about=""
    xmlns:xmp="http://ns.adobe.com/xap/1.0/"
    xmp:Label="Blue">
  </rdf:Description>
 </rdf:RDF>
</x:xmpmeta>"#;
        std::fs::write(&path, xmp).unwrap();

        let modified = update_label(&path, Some("Green")).unwrap();
        assert!(modified);

        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains(r#"xmp:Label="Green""#));
    }

    #[test]
    fn update_label_no_change_returns_false() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.xmp");
        let xmp = r#"<?xml version="1.0" encoding="UTF-8"?>
<x:xmpmeta xmlns:x="adobe:ns:meta/">
 <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
  <rdf:Description rdf:about=""
    xmlns:xmp="http://ns.adobe.com/xap/1.0/"
    xmp:Label="Red">
  </rdf:Description>
 </rdf:RDF>
</x:xmpmeta>"#;
        std::fs::write(&path, xmp).unwrap();

        let modified = update_label(&path, Some("Red")).unwrap();
        assert!(!modified);
    }

    #[test]
    fn update_label_preserves_other_content() {
        let xmp = r#"<?xml version="1.0" encoding="UTF-8"?>
<x:xmpmeta xmlns:x="adobe:ns:meta/">
 <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
  <rdf:Description rdf:about=""
    xmlns:dc="http://purl.org/dc/elements/1.1/"
    xmlns:xmp="http://ns.adobe.com/xap/1.0/"
    xmp:Rating="4"
    xmp:Label="Blue">
   <dc:subject>
    <rdf:Bag>
     <rdf:li>landscape</rdf:li>
    </rdf:Bag>
   </dc:subject>
  </rdf:Description>
 </rdf:RDF>
</x:xmpmeta>"#;

        let result = update_label_in_string(xmp, Some("Purple"));
        assert!(result.contains(r#"xmp:Label="Purple""#));
        assert!(result.contains(r#"xmp:Rating="4""#));
        assert!(result.contains("<rdf:li>landscape</rdf:li>"));
    }

    // ── update_hierarchical_subjects tests ──────────────────

    #[test]
    fn update_hierarchical_add_to_existing() {
        let xmp = r#"<?xml version="1.0" encoding="UTF-8"?>
<x:xmpmeta xmlns:x="adobe:ns:meta/">
 <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
  <rdf:Description rdf:about=""
    xmlns:lr="http://ns.adobe.com/lightroom/1.0/">
   <lr:hierarchicalSubject>
    <rdf:Bag>
     <rdf:li>animals|birds</rdf:li>
    </rdf:Bag>
   </lr:hierarchicalSubject>
  </rdf:Description>
 </rdf:RDF>
</x:xmpmeta>"#;

        let result = update_hierarchical_in_string(
            xmp,
            &["nature|sky|sunset".to_string()],
            &[],
        );
        assert!(result.contains("<rdf:li>animals|birds</rdf:li>"));
        assert!(result.contains("<rdf:li>nature|sky|sunset</rdf:li>"));
    }

    #[test]
    fn update_hierarchical_subjects_removes_leaf_only_entries() {
        // Real-world bug from user file Z91_4714.xmp: lr:hierarchicalSubject
        // had accumulated leaf-only entries (`Bavaria`, `Konzert`, a
        // multi-escape leftover) over years of writing back from various
        // tools. mirror-tags and --force compute remove-sets that
        // include these leaf entries; `update_hierarchical_subjects`
        // must honor them. The previous pipe-only filter on hier_remove
        // silently kept them all.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.xmp");
        let xmp = r#"<?xml version="1.0" encoding="UTF-8"?>
<x:xmpmeta xmlns:x="adobe:ns:meta/">
 <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
  <rdf:Description rdf:about=""
    xmlns:lr="http://ns.adobe.com/lightroom/1.0/">
   <lr:hierarchicalSubject>
    <rdf:Bag>
     <rdf:li>Bavaria</rdf:li>
     <rdf:li>Konzert</rdf:li>
     <rdf:li>Bobby &amp;amp; the BigTones</rdf:li>
     <rdf:li>location|Germany|Bayern</rdf:li>
     <rdf:li>person|ensemble|band|Bobby &amp; the BigTones</rdf:li>
    </rdf:Bag>
   </lr:hierarchicalSubject>
  </rdf:Description>
 </rdf:RDF>
</x:xmpmeta>"#;
        std::fs::write(&path, xmp).unwrap();

        // Caller asks to remove the leaf-only entries (matches what
        // --force / mirror-tags would compute for a file like this).
        let result = update_hierarchical_subjects(
            &path,
            &[],
            &[
                "Bavaria".to_string(),
                "Konzert".to_string(),
                // parse_xmp would decode the file's `&amp;amp;` to
                // this literal form; the function's li_re reader does
                // the same.
                "Bobby &amp; the BigTones".to_string(),
            ],
        )
        .unwrap();
        assert!(result, "function must report modified=true");

        let after = std::fs::read_to_string(&path).unwrap();
        assert!(!after.contains("<rdf:li>Bavaria</rdf:li>"));
        assert!(!after.contains("<rdf:li>Konzert</rdf:li>"));
        assert!(!after.contains("<rdf:li>Bobby &amp;amp; the BigTones</rdf:li>"));
        // The two correctly-pipe-pathed entries survive.
        assert!(after.contains("<rdf:li>location|Germany|Bayern</rdf:li>"));
        assert!(after.contains("<rdf:li>person|ensemble|band|Bobby &amp; the BigTones</rdf:li>"));
    }

    #[test]
    fn update_hierarchical_remove_from_existing() {
        let xmp = r#"<?xml version="1.0" encoding="UTF-8"?>
<x:xmpmeta xmlns:x="adobe:ns:meta/">
 <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
  <rdf:Description rdf:about=""
    xmlns:lr="http://ns.adobe.com/lightroom/1.0/">
   <lr:hierarchicalSubject>
    <rdf:Bag>
     <rdf:li>animals|birds|eagles</rdf:li>
     <rdf:li>nature|sunset</rdf:li>
    </rdf:Bag>
   </lr:hierarchicalSubject>
  </rdf:Description>
 </rdf:RDF>
</x:xmpmeta>"#;

        let result = update_hierarchical_in_string(
            xmp,
            &[],
            &["animals|birds|eagles".to_string()],
        );
        assert!(!result.contains("animals|birds|eagles"));
        assert!(result.contains("<rdf:li>nature|sunset</rdf:li>"));
    }

    #[test]
    fn update_hierarchical_remove_all_removes_block() {
        let xmp = r#"<?xml version="1.0" encoding="UTF-8"?>
<x:xmpmeta xmlns:x="adobe:ns:meta/">
 <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
  <rdf:Description rdf:about=""
    xmlns:lr="http://ns.adobe.com/lightroom/1.0/"
    xmp:Rating="3">
   <lr:hierarchicalSubject>
    <rdf:Bag>
     <rdf:li>animals|birds</rdf:li>
    </rdf:Bag>
   </lr:hierarchicalSubject>
  </rdf:Description>
 </rdf:RDF>
</x:xmpmeta>"#;

        let result = update_hierarchical_in_string(
            xmp,
            &[],
            &["animals|birds".to_string()],
        );
        assert!(!result.contains("lr:hierarchicalSubject"));
        assert!(!result.contains("animals|birds"));
        assert!(result.contains("xmp:Rating"));
    }

    #[test]
    fn update_hierarchical_inject_when_missing() {
        let xmp = r#"<?xml version="1.0" encoding="UTF-8"?>
<x:xmpmeta xmlns:x="adobe:ns:meta/">
 <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
  <rdf:Description rdf:about=""
    xmlns:dc="http://purl.org/dc/elements/1.1/"
    xmp:Rating="3">
  </rdf:Description>
 </rdf:RDF>
</x:xmpmeta>"#;

        let result = update_hierarchical_in_string(
            xmp,
            &["animals|birds|eagles".to_string()],
            &[],
        );
        assert!(result.contains("lr:hierarchicalSubject"));
        assert!(result.contains("xmlns:lr"));
        assert!(result.contains("<rdf:li>animals|birds|eagles</rdf:li>"));
        assert!(result.contains("xmp:Rating"));
    }

    #[test]
    fn update_hierarchical_subjects_filters_flat_tags() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.xmp");
        let xmp = r#"<?xml version="1.0" encoding="UTF-8"?>
<x:xmpmeta xmlns:x="adobe:ns:meta/">
 <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
  <rdf:Description rdf:about=""
    xmlns:dc="http://purl.org/dc/elements/1.1/">
  </rdf:Description>
 </rdf:RDF>
</x:xmpmeta>"#;
        std::fs::write(&path, xmp).unwrap();

        // Flat tags should be ignored
        let modified = update_hierarchical_subjects(
            &path,
            &["landscape".to_string()],
            &[],
        )
        .unwrap();
        assert!(!modified, "flat tags should be ignored by update_hierarchical_subjects");

        // Hierarchical tags (containing `|`) should be written
        let modified = update_hierarchical_subjects(
            &path,
            &["animals|birds|eagles".to_string()],
            &[],
        )
        .unwrap();
        assert!(modified);

        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("animals|birds|eagles"));
    }

    #[test]
    fn update_hierarchical_round_trip() {
        let xmp = r#"<?xml version="1.0" encoding="UTF-8"?>
<x:xmpmeta xmlns:x="adobe:ns:meta/">
 <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
  <rdf:Description rdf:about=""
    xmlns:lr="http://ns.adobe.com/lightroom/1.0/">
   <lr:hierarchicalSubject>
    <rdf:Bag>
     <rdf:li>animals|birds|eagles</rdf:li>
    </rdf:Bag>
   </lr:hierarchicalSubject>
  </rdf:Description>
 </rdf:RDF>
</x:xmpmeta>"#;

        // Parse it
        let data = parse_xmp(xmp);
        assert_eq!(data.hierarchical_keywords, vec!["animals|birds|eagles"]);

        // Add a new hierarchical tag
        let result = update_hierarchical_in_string(
            xmp,
            &["nature|sky|sunset".to_string()],
            &[],
        );
        assert!(result.contains("<rdf:li>animals|birds|eagles</rdf:li>"));
        assert!(result.contains("<rdf:li>nature|sky|sunset</rdf:li>"));

        // Parse the result — should have both
        let data2 = parse_xmp(&result);
        assert_eq!(
            data2.hierarchical_keywords,
            vec!["animals|birds|eagles", "nature|sky|sunset"]
        );
    }

    #[test]
    fn update_hierarchical_collapses_dual_namespace_blocks() {
        // Reproduces the real-world XMP that triggered this bug: a
        // `lightroom:hierarchicalSubject` block (legacy, flat leaves) sits
        // beside an `lr:hierarchicalSubject` block (MAKI's canonical
        // pipe-paths). Writeback must collapse them into a single `lr:`
        // block so the next refresh doesn't re-import flat leaves.
        let xmp = r#"<?xml version="1.0" encoding="UTF-8"?>
<x:xmpmeta xmlns:x="adobe:ns:meta/">
 <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
  <rdf:Description rdf:about=""
    xmlns:lr="http://ns.adobe.com/lightroom/1.0/"
    xmlns:lightroom="http://ns.adobe.com/lightroom/1.0/">
   <lr:hierarchicalSubject>
    <rdf:Bag>
     <rdf:li>location|Germany|Bayern</rdf:li>
    </rdf:Bag>
   </lr:hierarchicalSubject>
   <lightroom:hierarchicalSubject>
    <rdf:Bag>
     <rdf:li>Bavaria</rdf:li>
     <rdf:li>Germany</rdf:li>
    </rdf:Bag>
   </lightroom:hierarchicalSubject>
  </rdf:Description>
 </rdf:RDF>
</x:xmpmeta>"#;

        let result = update_hierarchical_in_string(xmp, &[], &[]);

        // The legacy `lightroom:` block is gone.
        assert!(
            !result.contains("<lightroom:hierarchicalSubject"),
            "lightroom: block should be removed:\n{result}"
        );
        // A single canonical `lr:` block remains.
        let lr_count = result.matches("<lr:hierarchicalSubject>").count();
        assert_eq!(lr_count, 1, "expected exactly one lr: block:\n{result}");
        // Flat leaves from the legacy block survive (they had no canonical
        // home — better to keep them visible than to silently drop user
        // data).
        assert!(result.contains("<rdf:li>Bavaria</rdf:li>"));
        // The original pipe-path is preserved.
        assert!(result.contains("<rdf:li>location|Germany|Bayern</rdf:li>"));
    }

    #[test]
    fn update_hierarchical_collapses_alien_prefix() {
        // A tool binds an exotic prefix to the Lightroom namespace.
        // The block must still be detected and canonicalised to `lr:`.
        let xmp = r#"<?xml version="1.0" encoding="UTF-8"?>
<x:xmpmeta xmlns:x="adobe:ns:meta/">
 <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
  <rdf:Description rdf:about=""
    xmlns:lrc="http://ns.adobe.com/lightroom/1.0/">
   <lrc:hierarchicalSubject>
    <rdf:Bag>
     <rdf:li>nature|landscape</rdf:li>
    </rdf:Bag>
   </lrc:hierarchicalSubject>
  </rdf:Description>
 </rdf:RDF>
</x:xmpmeta>"#;

        let result = update_hierarchical_in_string(
            xmp,
            &["nature|sky|sunset".to_string()],
            &[],
        );

        assert!(
            !result.contains("<lrc:hierarchicalSubject"),
            "exotic prefix block should be removed:\n{result}"
        );
        assert!(result.contains("<lr:hierarchicalSubject>"));
        assert!(result.contains("<rdf:li>nature|landscape</rdf:li>"));
        assert!(result.contains("<rdf:li>nature|sky|sunset</rdf:li>"));
        // xmlns:lr should have been added since only `xmlns:lrc=...` was
        // declared previously.
        assert!(result.contains(r#"xmlns:lr="http://ns.adobe.com/lightroom/1.0/""#));
    }

    #[test]
    fn update_hierarchical_canonical_lr_only_is_byte_stable() {
        // A file with only a canonical `lr:` block and no edits should
        // be returned unchanged — no spurious re-rendering that could
        // cause SHA drift on no-op writebacks.
        let xmp = r#"<?xml version="1.0" encoding="UTF-8"?>
<x:xmpmeta xmlns:x="adobe:ns:meta/">
 <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
  <rdf:Description rdf:about=""
    xmlns:lr="http://ns.adobe.com/lightroom/1.0/">
   <lr:hierarchicalSubject>
    <rdf:Bag>
     <rdf:li>animals|birds|eagles</rdf:li>
    </rdf:Bag>
   </lr:hierarchicalSubject>
  </rdf:Description>
 </rdf:RDF>
</x:xmpmeta>"#;

        let result = update_hierarchical_in_string(xmp, &[], &[]);
        assert_eq!(result, xmp);
    }

    #[test]
    fn xml_unescape_decodes_standard_entities() {
        assert_eq!(xml_unescape("Bobby &amp; the BigTones"), "Bobby & the BigTones");
        assert_eq!(xml_unescape("&lt;tag&gt;"), "<tag>");
        assert_eq!(xml_unescape("a &quot;b&quot; c"), "a \"b\" c");
        assert_eq!(xml_unescape("can&apos;t"), "can't");
        // Nested case: `&amp;` decoded LAST, so `&amp;lt;` decodes to
        // `&lt;`, not `<`.
        assert_eq!(xml_unescape("&amp;lt;"), "&lt;");
        // Idempotent on already-decoded strings.
        assert_eq!(xml_unescape("plain text"), "plain text");
    }

    #[test]
    fn xml_escape_unescape_round_trip() {
        for s in &[
            "Bobby & the BigTones",
            "rock <metal> roll",
            "name: \"value\"",
            "can't won't",
            "a & b < c > d",
            "no specials",
        ] {
            let escaped = xml_escape(s);
            assert_eq!(xml_unescape(&escaped), *s, "round-trip failed for {s:?}");
        }
    }

    #[test]
    fn update_hierarchical_does_not_runaway_escape_ampersand() {
        // Regression for the bug surfaced by `maki writeback --all`:
        // when an `<rdf:li>` already contained `&amp;`, the writer was
        // re-escaping the captured raw text (`&amp;` → `&amp;amp;`),
        // adding one extra `amp;` layer per writeback pass. Symptom:
        // re-running writeback on the same catalog state kept
        // "writing" the same recipes forever, with files growing
        // entries like `Bobby &amp;amp;amp;amp;amp; the BigTones`.
        let xmp = r#"<?xml version="1.0" encoding="UTF-8"?>
<x:xmpmeta xmlns:x="adobe:ns:meta/">
 <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
  <rdf:Description rdf:about=""
    xmlns:lr="http://ns.adobe.com/lightroom/1.0/">
   <lr:hierarchicalSubject>
    <rdf:Bag>
     <rdf:li>person|ensemble|band|Bobby &amp; the BigTones</rdf:li>
    </rdf:Bag>
   </lr:hierarchicalSubject>
  </rdf:Description>
 </rdf:RDF>
</x:xmpmeta>"#;

        // First writeback: catalog already has `Bobby & the BigTones` —
        // the tag is in the file. No additions, no removals. Output
        // must be byte-stable (or at minimum: must NOT introduce
        // `&amp;amp;`).
        let catalog_tag = "person|ensemble|band|Bobby & the BigTones".to_string();
        let after_first = update_hierarchical_in_string(xmp, &[catalog_tag.clone()], &[]);
        assert!(
            !after_first.contains("&amp;amp;"),
            "Round 1 must not introduce nested &amp;amp; escapes. Got:\n{after_first}"
        );
        // The original entry's encoding is preserved (one `&amp;`).
        assert!(after_first.contains("Bobby &amp; the BigTones"));
        // No duplicate entry got added (the tag was already there).
        assert_eq!(
            after_first.matches("Bobby &amp;").count(),
            1,
            "Should have exactly one `Bobby &amp;…` entry. Got:\n{after_first}"
        );

        // Second writeback on the result: must be a no-op for this
        // single-tag block. The pre-v4.5.17-fix bug surfaced as the
        // entry being re-captured as literal `Bobby &amp; the BigTones`
        // (not decoded), then xml_escape'd to `Bobby &amp;amp; the
        // BigTones`, so the file changed every round.
        let after_second = update_hierarchical_in_string(&after_first, &[catalog_tag], &[]);
        assert_eq!(after_second, after_first, "Round 2 must be a no-op");
    }

    #[test]
    fn parse_xmp_decodes_8_layer_escape() {
        // The exact form the user reported in Z91_4714.xmp:
        // an 8-layer file entry. If quick-xml's `unescape()`
        // recursively expanded entities (decoding the &amp; inside
        // the result of decoding &amp;amp;), we'd end up with the
        // literal `Bobby & the BigTones` — which catalog HAS — and
        // mirror-tags would silently keep the broken entry as "valid".
        // Single-pass decoding is essential; this test locks it.
        let xmp = r#"<?xml version="1.0" encoding="UTF-8"?>
<x:xmpmeta xmlns:x="adobe:ns:meta/">
 <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
  <rdf:Description rdf:about=""
    xmlns:dc="http://purl.org/dc/elements/1.1/">
   <dc:subject>
    <rdf:Bag>
     <rdf:li>Bobby &amp;amp;amp;amp;amp;amp;amp;amp; the BigTones</rdf:li>
    </rdf:Bag>
   </dc:subject>
  </rdf:Description>
 </rdf:RDF>
</x:xmpmeta>"#;

        let data = parse_xmp(xmp);
        assert_eq!(
            data.keywords,
            vec!["Bobby &amp;amp;amp;amp;amp;amp;amp; the BigTones".to_string()],
            "parse_xmp must SINGLE-pass decode (8-layer file → 7-layer \
             literal). Multi-pass would silently turn this into the \
             catalog's `Bobby & the BigTones` and defeat mirror-tags."
        );
    }

    #[test]
    fn mirror_tags_drops_broken_entry_end_to_end() {
        // Reproduces user's reported state on Z91_4714.xmp:
        //   dc:subject has one correct 1-escape entry AND one broken
        //   8-escape leftover from pre-fix writebacks. With mirror-tags
        //   ON, the broken entry MUST be removed in one writeback pass.
        let xmp = r#"<?xml version="1.0" encoding="UTF-8"?>
<x:xmpmeta xmlns:x="adobe:ns:meta/">
 <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
  <rdf:Description rdf:about=""
    xmlns:dc="http://purl.org/dc/elements/1.1/">
   <dc:subject>
    <rdf:Bag>
     <rdf:li>person</rdf:li>
     <rdf:li>ensemble</rdf:li>
     <rdf:li>band</rdf:li>
     <rdf:li>Bobby &amp; the BigTones</rdf:li>
     <rdf:li>Bobby &amp;amp;amp;amp;amp;amp;amp;amp; the BigTones</rdf:li>
    </rdf:Bag>
   </dc:subject>
  </rdf:Description>
 </rdf:RDF>
</x:xmpmeta>"#;

        // Simulate what writeback_process computes:
        //   dc_tags  = flat components of catalog hierarchy (literal `&`)
        //   dc_remove = xmp.keywords NOT in dc_tags
        let catalog_dc_tags = vec![
            "person".to_string(),
            "ensemble".to_string(),
            "band".to_string(),
            "Bobby & the BigTones".to_string(),
        ];
        let xmp_data = parse_xmp(xmp);
        let dc_set: std::collections::HashSet<&str> =
            catalog_dc_tags.iter().map(|s| s.as_str()).collect();
        let dc_remove: Vec<String> = xmp_data
            .keywords
            .iter()
            .filter(|t| !dc_set.contains(t.as_str()))
            .cloned()
            .collect();

        // The mirror-tags computation must identify the broken entry as
        // a removal candidate — parse_xmp decodes once (8-escape file
        // form → 7-escape literal in memory).
        assert_eq!(
            dc_remove,
            vec!["Bobby &amp;amp;amp;amp;amp;amp;amp; the BigTones".to_string()],
            "mirror-tags must compute the broken entry as a removal. \
             parse_xmp.keywords was: {:?}",
            xmp_data.keywords
        );

        // And update_tags_in_string with that remove list must drop it.
        let after = update_tags_in_string(xmp, &catalog_dc_tags, &dc_remove);
        assert!(
            !after.contains("amp;amp;"),
            "Broken entry must be removed.\n---- BEFORE ----\n{xmp}\n---- AFTER ----\n{after}"
        );
        assert!(
            after.contains("Bobby &amp; the BigTones"),
            "Correct entry must survive. Got:\n{after}"
        );
        assert_ne!(after, xmp, "File must have changed (Ok(true) path)");
    }

    #[test]
    fn extract_decodes_multi_layer_entries_one_step() {
        // mirror-tags computation uses `extract` (parse_xmp) to read
        // existing keywords and diff against catalog. Verify that the
        // parse-side decoding matches what `xml_unescape` produces, so
        // the remove-set built from extract() values actually matches
        // the entries `update_hierarchical_in_string` sees after its
        // own li_re + xml_unescape pass.
        let xmp = r#"<?xml version="1.0" encoding="UTF-8"?>
<x:xmpmeta xmlns:x="adobe:ns:meta/">
 <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
  <rdf:Description rdf:about=""
    xmlns:lr="http://ns.adobe.com/lightroom/1.0/">
   <lr:hierarchicalSubject>
    <rdf:Bag>
     <rdf:li>person|ensemble|band|Bobby &amp; the BigTones</rdf:li>
     <rdf:li>person|ensemble|band|Bobby &amp;amp; the BigTones</rdf:li>
     <rdf:li>person|ensemble|band|Bobby &amp;amp;amp;amp; the BigTones</rdf:li>
    </rdf:Bag>
   </lr:hierarchicalSubject>
  </rdf:Description>
 </rdf:RDF>
</x:xmpmeta>"#;

        let data = parse_xmp(xmp);
        // Parse-side decoding strips one entity layer per call. So:
        //   file `&amp;`        → string `&`
        //   file `&amp;amp;`    → string `&amp;`
        //   file `&amp;amp;amp;amp;` → string `&amp;amp;amp;`
        assert_eq!(
            data.hierarchical_keywords,
            vec![
                "person|ensemble|band|Bobby & the BigTones".to_string(),
                "person|ensemble|band|Bobby &amp; the BigTones".to_string(),
                "person|ensemble|band|Bobby &amp;amp;amp; the BigTones".to_string(),
            ],
            "parse_xmp must decode exactly one entity layer per pass"
        );

        // And xml_unescape on the same raw li texts must produce the
        // identical strings, otherwise the mirror-tags remove-set
        // won't match what update_hierarchical_in_string sees.
        assert_eq!(
            xml_unescape("Bobby &amp; the BigTones"),
            "Bobby & the BigTones"
        );
        assert_eq!(
            xml_unescape("Bobby &amp;amp; the BigTones"),
            "Bobby &amp; the BigTones"
        );
        assert_eq!(
            xml_unescape("Bobby &amp;amp;amp;amp; the BigTones"),
            "Bobby &amp;amp;amp; the BigTones"
        );
    }

    #[test]
    fn update_hierarchical_with_multi_layer_escaped_entries() {
        // Reproduces the user's catalog state: a file that already
        // accumulated multiple `&amp;…` layers from pre-fix writebacks.
        // Verify byte-stability across two rounds — any change is a
        // bug because the catalog tag (decoded to literal `&`) is
        // already present after round 1's unescape.
        let xmp = r#"<?xml version="1.0" encoding="UTF-8"?>
<x:xmpmeta xmlns:x="adobe:ns:meta/">
 <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
  <rdf:Description rdf:about=""
    xmlns:lr="http://ns.adobe.com/lightroom/1.0/">
   <lr:hierarchicalSubject>
    <rdf:Bag>
     <rdf:li>person|ensemble|band|Bobby &amp; the BigTones</rdf:li>
     <rdf:li>person|ensemble|band|Bobby &amp;amp;amp; the BigTones</rdf:li>
     <rdf:li>person|ensemble|band|Bobby &amp;amp;amp;amp;amp; the BigTones</rdf:li>
     <rdf:li>person|ensemble|band|Bobby &amp;amp;amp;amp;amp;amp;amp; the BigTones</rdf:li>
    </rdf:Bag>
   </lr:hierarchicalSubject>
  </rdf:Description>
 </rdf:RDF>
</x:xmpmeta>"#;

        let catalog_tag = "person|ensemble|band|Bobby & the BigTones".to_string();
        let after_first = update_hierarchical_in_string(xmp, &[catalog_tag.clone()], &[]);

        // Strongest assertion: round 1 is byte-stable. Every existing
        // `&amp;amp;…` entry must decode (xml_unescape) and re-encode
        // (xml_escape) back to itself, with no additional `amp;` layer
        // and no new entries appended.
        assert_eq!(
            after_first, xmp,
            "Round 1 must not modify the file when the catalog tag is \
             already present (after unescape).\n\
             ---- BEFORE ----\n{xmp}\n---- AFTER ----\n{after_first}"
        );

        // Round 2 same assertion against round-1 output.
        let after_second = update_hierarchical_in_string(&after_first, &[catalog_tag], &[]);
        assert_eq!(after_second, after_first, "Round 2 must be byte-stable");
    }

    #[test]
    fn update_tags_does_not_runaway_escape_ampersand() {
        // Same bug, dc:subject side. The flat-tag writer
        // (`update_tags_in_string`) used the same regex-captures-raw-text
        // pattern, so it suffered the identical escape escalation.
        let xmp = r#"<?xml version="1.0" encoding="UTF-8"?>
<x:xmpmeta xmlns:x="adobe:ns:meta/">
 <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
  <rdf:Description rdf:about=""
    xmlns:dc="http://purl.org/dc/elements/1.1/">
   <dc:subject>
    <rdf:Bag>
     <rdf:li>Bobby &amp; the BigTones</rdf:li>
    </rdf:Bag>
   </dc:subject>
  </rdf:Description>
 </rdf:RDF>
</x:xmpmeta>"#;

        let catalog_tag = "Bobby & the BigTones".to_string();
        let after_first = update_tags_in_string(xmp, &[catalog_tag.clone()], &[]);
        assert!(
            !after_first.contains("&amp;amp;"),
            "Round 1 must not introduce nested &amp;amp; escapes. Got:\n{after_first}"
        );
        let after_second = update_tags_in_string(&after_first, &[catalog_tag], &[]);
        assert_eq!(after_second, after_first, "Round 2 must be a no-op");
    }
}
