//! Search-query parsing layer: filter tokenizer, `ParsedSearch` struct,
//! `NumericFilter` enum, date input parser, path normaliser.
//!
//! Free of any DB or write-path dependencies — the parser turns a user-typed
//! query string into a structured `ParsedSearch` that the search builder in
//! `catalog::build_search_where` consumes. Splitting it out lets the rest of
//! `query.rs` (which is dominated by write-path methods on `QueryEngine`)
//! be navigated without scrolling past 1000 lines of filter parsing.

use anyhow::Result;
use chrono::{DateTime, NaiveDate, TimeZone, Utc};

use crate::catalog::SearchOptions;
use crate::models::volume::Volume;

// ═══ DATE PARSING ═══

/// Parse a flexible date input string into a `DateTime<Utc>`.
///
/// Supported formats:
/// - `YYYY` → Jan 1 of that year, midnight UTC
/// - `YYYY-MM` → 1st of that month, midnight UTC
/// - `YYYY-MM-DD` → midnight UTC on that date
/// - Full ISO 8601 / RFC 3339 (e.g. `2024-06-15T12:30:00Z`) — parsed as-is
///
/// # Examples
///
/// ```
/// use maki::query::parse_date_input;
///
/// let dt = parse_date_input("2026").unwrap();
/// assert_eq!(dt.to_rfc3339(), "2026-01-01T00:00:00+00:00");
///
/// let dt = parse_date_input("2026-03").unwrap();
/// assert_eq!(dt.to_rfc3339(), "2026-03-01T00:00:00+00:00");
///
/// let dt = parse_date_input("2026-03-15").unwrap();
/// assert_eq!(dt.to_rfc3339(), "2026-03-15T00:00:00+00:00");
///
/// assert!(parse_date_input("not-a-date").is_err());
/// ```
pub fn parse_date_input(s: &str) -> Result<DateTime<Utc>> {
    let s = s.trim();

    // Try RFC 3339 / ISO 8601 first
    if let Ok(dt) = DateTime::parse_from_rfc3339(s) {
        return Ok(dt.with_timezone(&Utc));
    }

    // YYYY-MM-DD
    if let Ok(nd) = NaiveDate::parse_from_str(s, "%Y-%m-%d") {
        return Ok(Utc.from_utc_datetime(&nd.and_hms_opt(0, 0, 0).unwrap()));
    }

    // YYYY-MM
    if let Some((y, m)) = s.split_once('-') {
        if let (Ok(year), Ok(month)) = (y.parse::<i32>(), m.parse::<u32>()) {
            if let Some(nd) = NaiveDate::from_ymd_opt(year, month, 1) {
                return Ok(Utc.from_utc_datetime(&nd.and_hms_opt(0, 0, 0).unwrap()));
            }
        }
    }

    // YYYY
    if let Ok(year) = s.parse::<i32>() {
        if let Some(nd) = NaiveDate::from_ymd_opt(year, 1, 1) {
            return Ok(Utc.from_utc_datetime(&nd.and_hms_opt(0, 0, 0).unwrap()));
        }
    }

    anyhow::bail!("invalid date format: '{s}'. Use YYYY, YYYY-MM, YYYY-MM-DD, or ISO 8601.")
}

/// Parsed search query with all supported filter prefixes.
///
/// Multi-value fields (Vecs) support:
/// - **Repeated filters** = AND: `tag:landscape tag:sunset` (must have both)
/// - **Comma within a value** = OR: `tag:alice,bob` (either tag matches)
/// - **`-` prefix** = negation: `-tag:rejected` excludes matching assets
#[derive(Debug, Default)]
// ═══ PARSED SEARCH ═══

pub struct ParsedSearch {
    pub text: Option<String>,
    pub text_exclude: Vec<String>,
    pub asset_types: Vec<String>,
    pub asset_types_exclude: Vec<String>,
    pub tags: Vec<String>,
    pub tags_exclude: Vec<String>,
    pub formats: Vec<String>,
    pub formats_exclude: Vec<String>,
    pub color_labels: Vec<String>,
    pub color_labels_exclude: Vec<String>,
    pub color_label_none: bool,
    /// `-label:none` — assets that have a colour label.
    pub color_label_any: bool,
    pub cameras: Vec<String>,
    pub cameras_exclude: Vec<String>,
    pub lenses: Vec<String>,
    pub lenses_exclude: Vec<String>,
    pub descriptions: Vec<String>,
    pub descriptions_exclude: Vec<String>,
    pub collections: Vec<String>,
    pub collections_exclude: Vec<String>,
    pub path_prefixes: Vec<String>,
    pub path_prefixes_exclude: Vec<String>,
    pub rating: Option<NumericFilter>,
    pub iso: Option<NumericFilter>,
    pub focal: Option<NumericFilter>,
    pub aperture: Option<NumericFilter>,
    pub width: Option<NumericFilter>,
    pub height: Option<NumericFilter>,
    pub copies: Option<NumericFilter>,
    pub variant_count: Option<NumericFilter>,
    pub scattered: Option<NumericFilter>,
    pub face_count: Option<NumericFilter>,
    /// `tagcount:N` — number of leaf tags (intentional tags the user applied,
    /// excluding auto-expanded ancestors). See `tag_util::leaf_tag_count`.
    pub tag_count: Option<NumericFilter>,
    pub duration: Option<NumericFilter>,
    pub codec: Option<String>,
    /// `key:Am` — musical key (exact, case-insensitive; set by `maki audio analyze`).
    pub audio_key: Option<String>,
    /// `bpm:120` / `bpm:100-140` — tempo (set by `maki audio analyze`).
    pub audio_bpm: Option<NumericFilter>,
    pub stale_days: Option<NumericFilter>,
    /// `stale:never` — locations that have never been verified.
    pub stale_never: bool,
    pub meta_filters: Vec<(String, String)>,
    pub orphan: bool,
    pub orphan_false: bool,
    pub missing: bool,
    pub volumes: Vec<String>,
    pub volumes_exclude: Vec<String>,
    pub volume_none: bool,
    /// `-volume:none` — assets with at least one location on an online volume.
    pub volume_any_online: bool,
    pub date_prefix: Option<String>,
    pub date_from: Option<String>,
    pub date_until: Option<String>,
    pub stacked: Option<bool>,
    pub geo_bbox: Option<(f64, f64, f64, f64)>,  // (south, west, north, east)
    pub has_gps: Option<bool>,
    pub has_faces: Option<bool>,
    pub persons: Vec<String>,
    pub persons_exclude: Vec<String>,
    pub asset_ids: Vec<String>,
    pub has_embed: Option<bool>,
    /// `similar:<id>[:<limit>]` — needs the `ai` feature at execution.
    pub similar: Option<String>,
    pub similar_limit: Option<usize>,
    /// `min_sim:<percent>` for `similar:` / `text:` / query-image searches.
    pub min_sim: Option<f32>,
    /// `text:<query>[:<limit>]` — needs the `ai` feature at execution.
    pub text_query: Option<String>,
    pub text_query_limit: Option<usize>,
}

impl ParsedSearch {
    /// Merge another `ParsedSearch` into this one (AND semantics).
    ///
    /// Vec fields are extended (both must match). Option fields prefer `self`'s
    /// value; the other's value is used only when `self` has `None`.
    /// Bool fields are OR'd (either being true activates the filter).
    pub fn merge_from(&mut self, other: &ParsedSearch) {
        // Vec fields: extend
        self.text_exclude.extend(other.text_exclude.iter().cloned());
        self.asset_types.extend(other.asset_types.iter().cloned());
        self.asset_types_exclude.extend(other.asset_types_exclude.iter().cloned());
        self.tags.extend(other.tags.iter().cloned());
        self.tags_exclude.extend(other.tags_exclude.iter().cloned());
        self.formats.extend(other.formats.iter().cloned());
        self.formats_exclude.extend(other.formats_exclude.iter().cloned());
        self.color_labels.extend(other.color_labels.iter().cloned());
        self.color_labels_exclude.extend(other.color_labels_exclude.iter().cloned());
        self.cameras.extend(other.cameras.iter().cloned());
        self.cameras_exclude.extend(other.cameras_exclude.iter().cloned());
        self.lenses.extend(other.lenses.iter().cloned());
        self.lenses_exclude.extend(other.lenses_exclude.iter().cloned());
        self.descriptions.extend(other.descriptions.iter().cloned());
        self.descriptions_exclude.extend(other.descriptions_exclude.iter().cloned());
        self.collections.extend(other.collections.iter().cloned());
        self.collections_exclude.extend(other.collections_exclude.iter().cloned());
        self.path_prefixes.extend(other.path_prefixes.iter().cloned());
        self.path_prefixes_exclude.extend(other.path_prefixes_exclude.iter().cloned());
        self.volumes.extend(other.volumes.iter().cloned());
        self.volumes_exclude.extend(other.volumes_exclude.iter().cloned());
        self.meta_filters.extend(other.meta_filters.iter().cloned());
        self.persons.extend(other.persons.iter().cloned());
        self.persons_exclude.extend(other.persons_exclude.iter().cloned());
        self.asset_ids.extend(other.asset_ids.iter().cloned());

        // Option fields: prefer self, fall back to other
        if self.text.is_none() { self.text = other.text.clone(); }
        self.rating = NumericFilter::or(&self.rating, &other.rating);
        self.iso = NumericFilter::or(&self.iso, &other.iso);
        self.focal = NumericFilter::or(&self.focal, &other.focal);
        self.aperture = NumericFilter::or(&self.aperture, &other.aperture);
        self.width = NumericFilter::or(&self.width, &other.width);
        self.height = NumericFilter::or(&self.height, &other.height);
        self.copies = NumericFilter::or(&self.copies, &other.copies);
        self.variant_count = NumericFilter::or(&self.variant_count, &other.variant_count);
        self.scattered = NumericFilter::or(&self.scattered, &other.scattered);
        self.face_count = NumericFilter::or(&self.face_count, &other.face_count);
        self.tag_count = NumericFilter::or(&self.tag_count, &other.tag_count);
        self.duration = NumericFilter::or(&self.duration, &other.duration);
        self.audio_bpm = NumericFilter::or(&self.audio_bpm, &other.audio_bpm);
        self.stale_days = NumericFilter::or(&self.stale_days, &other.stale_days);
        if self.codec.is_none() { self.codec = other.codec.clone(); }
        if self.audio_key.is_none() { self.audio_key = other.audio_key.clone(); }
        if self.date_prefix.is_none() { self.date_prefix = other.date_prefix.clone(); }
        if self.date_from.is_none() { self.date_from = other.date_from.clone(); }
        if self.date_until.is_none() { self.date_until = other.date_until.clone(); }
        if self.stacked.is_none() { self.stacked = other.stacked; }
        if self.geo_bbox.is_none() { self.geo_bbox = other.geo_bbox; }
        if self.has_gps.is_none() { self.has_gps = other.has_gps; }
        if self.has_faces.is_none() { self.has_faces = other.has_faces; }
        if self.has_embed.is_none() { self.has_embed = other.has_embed; }
        if self.similar.is_none() { self.similar = other.similar.clone(); }
        if self.similar_limit.is_none() { self.similar_limit = other.similar_limit; }
        if self.min_sim.is_none() { self.min_sim = other.min_sim; }
        if self.text_query.is_none() { self.text_query = other.text_query.clone(); }
        if self.text_query_limit.is_none() { self.text_query_limit = other.text_query_limit; }

        // Bool fields: OR
        self.orphan = self.orphan || other.orphan;
        self.orphan_false = self.orphan_false || other.orphan_false;
        self.missing = self.missing || other.missing;
        self.volume_none = self.volume_none || other.volume_none;
        self.volume_any_online = self.volume_any_online || other.volume_any_online;
        self.color_label_none = self.color_label_none || other.color_label_none;
        self.color_label_any = self.color_label_any || other.color_label_any;
        self.stale_never = self.stale_never || other.stale_never;
    }

    /// Convert to `SearchOptions` for passing to catalog search methods.
    pub fn to_search_options(&self) -> SearchOptions<'_> {
        SearchOptions {
            asset_ids: &self.asset_ids,
            text: self.text.as_deref(),
            text_exclude: &self.text_exclude,
            asset_types: &self.asset_types,
            asset_types_exclude: &self.asset_types_exclude,
            tags: &self.tags,
            tags_exclude: &self.tags_exclude,
            formats: &self.formats,
            formats_exclude: &self.formats_exclude,
            color_labels: &self.color_labels,
            color_labels_exclude: &self.color_labels_exclude,
            color_label_none: self.color_label_none,
            color_label_any: self.color_label_any,
            cameras: &self.cameras,
            cameras_exclude: &self.cameras_exclude,
            lenses: &self.lenses,
            lenses_exclude: &self.lenses_exclude,
            descriptions: &self.descriptions,
            descriptions_exclude: &self.descriptions_exclude,
            collections: &self.collections,
            collections_exclude: &self.collections_exclude,
            path_prefixes: &self.path_prefixes,
            path_prefixes_exclude: &self.path_prefixes_exclude,
            rating: self.rating.clone(),
            iso: self.iso.clone(),
            focal: self.focal.clone(),
            aperture: self.aperture.clone(),
            width: self.width.clone(),
            height: self.height.clone(),
            copies: self.copies.clone(),
            variant_count: self.variant_count.clone(),
            scattered: self.scattered.clone(),
            face_count: self.face_count.clone(),
            tag_count: self.tag_count.clone(),
            duration: self.duration.clone(),
            codec: self.codec.clone(),
            audio_key: self.audio_key.clone(),
            audio_bpm: self.audio_bpm.clone(),
            stale_days: self.stale_days.clone(),
            stale_never: self.stale_never,
            meta_filters: self
                .meta_filters
                .iter()
                .map(|(k, v)| (k.as_str(), v.as_str()))
                .collect(),
            orphan: self.orphan,
            orphan_false: self.orphan_false,
            date_prefix: self.date_prefix.as_deref(),
            date_from: self.date_from.as_deref(),
            date_until: self.date_until.as_deref(),
            stacked_filter: self.stacked,
            geo_bbox: self.geo_bbox,
            has_gps: self.has_gps,
            has_faces: self.has_faces,
            has_embed: self.has_embed,
            ..Default::default()
        }
    }
}

// ═══ QUERY TOKENIZER ═══

/// Tokenize a search query respecting double-quoted values.
///
/// Splits on whitespace, but `prefix:"multi word value"` stays as a single token
/// with quotes stripped from the value. Unquoted tokens work as before.
///
/// Examples:
///   `tag:"Fools Theater" rating:4+` → `["tag:Fools Theater", "rating:4+"]`
///   `tag:landscape type:image`      → `["tag:landscape", "type:image"]`
///   `hello world`                   → `["hello", "world"]`
pub fn tokenize_query(query: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut chars = query.chars().peekable();

    while chars.peek().is_some() {
        // Skip whitespace
        while chars.peek().is_some_and(|c| c.is_whitespace()) {
            chars.next();
        }
        if chars.peek().is_none() {
            break;
        }

        let mut token = String::new();
        let mut in_quotes = false;

        while let Some(&c) = chars.peek() {
            if in_quotes {
                chars.next();
                if c == '"' {
                    in_quotes = false;
                } else {
                    token.push(c);
                }
            } else if c == '"' {
                chars.next();
                in_quotes = true;
            } else if c.is_whitespace() {
                break;
            } else {
                chars.next();
                token.push(c);
            }
        }

        if !token.is_empty() {
            tokens.push(token);
        }
    }

    tokens
}

// ═══ SEARCH PARSER ═══

/// Filters whose value is appended verbatim to a `Vec<String>` field, with
/// the negation flag selecting between include and exclude lists.
///
/// Adding a new filter of this shape is one line: a `(prefix, |p, v, neg| { ... })`
/// entry in this table. The closure runs in the parsing hot path, so keep it
/// short — anything beyond the simple include/exclude split belongs in the
/// custom-parsing block in `parse_search_query`.
const SIMPLE_FILTERS: &[(&str, fn(&mut ParsedSearch, &str, bool))] = &[
    ("id:",          |p, v, _n| p.asset_ids.push(v.to_string())),
    ("type:",        |p, v, n| if n { p.asset_types_exclude.push(v.to_string()) } else { p.asset_types.push(v.to_string()) }),
    ("tag:",         |p, v, n| if n { p.tags_exclude.push(v.to_string())        } else { p.tags.push(v.to_string()) }),
    ("format:",      |p, v, n| if n { p.formats_exclude.push(v.to_string())     } else { p.formats.push(v.to_string()) }),
    ("camera:",      |p, v, n| if n { p.cameras_exclude.push(v.to_string())     } else { p.cameras.push(v.to_string()) }),
    ("lens:",        |p, v, n| if n { p.lenses_exclude.push(v.to_string())      } else { p.lenses.push(v.to_string()) }),
    ("description:", |p, v, n| if n { p.descriptions_exclude.push(v.to_string()) } else { p.descriptions.push(v.to_string()) }),
    ("desc:",        |p, v, n| if n { p.descriptions_exclude.push(v.to_string()) } else { p.descriptions.push(v.to_string()) }),
    ("collection:",  |p, v, n| if n { p.collections_exclude.push(v.to_string())  } else { p.collections.push(v.to_string()) }),
    ("path:",        |p, v, n| if n { p.path_prefixes_exclude.push(v.to_string()) } else { p.path_prefixes.push(v.to_string()) }),
    ("person:",      |p, v, n| if n { p.persons_exclude.push(v.to_string())     } else { p.persons.push(v.to_string()) }),
];

/// Filters whose value is parsed via `parse_numeric_filter` and assigned to a
/// single `Option<NumericFilter>` field. Negation is ignored — numeric
/// comparisons don't have an exclude shape.
const NUMERIC_FILTERS: &[(&str, fn(&mut ParsedSearch, Option<NumericFilter>))] = &[
    ("rating:",   |p, n| p.rating = n),
    ("iso:",      |p, n| p.iso = n),
    ("focal:",    |p, n| p.focal = n),
    ("f:",        |p, n| p.aperture = n),
    ("width:",    |p, n| p.width = n),
    ("height:",   |p, n| p.height = n),
    ("stale:",    |p, n| p.stale_days = n),
    ("copies:",   |p, n| p.copies = n),
    ("variants:", |p, n| p.variant_count = n),
    ("duration:", |p, n| p.duration = n),
    // Intentional (leaf) tags only — `tagcount:0` finds untagged assets.
    ("tagcount:", |p, n| p.tag_count = n),
];

/// Boolean tokens that don't carry a value — exact full-token matches.
const BOOLEAN_TOKENS: &[(&str, fn(&mut ParsedSearch))] = &[
    ("orphan:true",   |p| p.orphan = true),
    ("orphan:false",  |p| p.orphan_false = true),
    ("missing:true",  |p| p.missing = true),
    ("stacked:true",  |p| p.stacked = Some(true)),
    ("stacked:false", |p| p.stacked = Some(false)),
];

/// Filters whose value sets a single `Option<String>` field — date range
/// bounds, etc. Negation is ignored (no exclude shape).
const STRING_FILTERS: &[(&str, fn(&mut ParsedSearch, &str))] = &[
    ("date:",      |p, v| p.date_prefix = Some(v.to_string())),
    ("dateFrom:",  |p, v| p.date_from = Some(v.to_string())),
    ("dateUntil:", |p, v| p.date_until = Some(v.to_string())),
];

/// Parse a search query string into structured filters.
///
/// Supports prefix filters: `type:image`, `tag:landscape`, `format:jpg`, `rating:3+`,
/// `camera:fuji`, `lens:56mm`, `iso:3200`, `iso:100-800`, `focal:50`, `focal:35-70`,
/// `f:2.8`, `f:1.4-2.8`, `width:4000+`, `height:2000+`, `meta:key=value`.
/// Values with spaces can be quoted: `tag:"Fools Theater"`, `camera:"Canon EOS R5"`.
/// Remaining tokens are joined as free-text search.
///
/// # Examples
///
/// ```
/// use maki::query::{parse_search_query, NumericFilter};
///
/// let p = parse_search_query("tag:sunset type:image rating:3+");
/// assert_eq!(p.tags, vec!["sunset"]);
/// assert_eq!(p.asset_types, vec!["image"]);
/// assert_eq!(p.rating, Some(NumericFilter::Min(3.0)));
///
/// // Negation with - prefix
/// let p = parse_search_query("-tag:rejected");
/// assert_eq!(p.tags_exclude, vec!["rejected"]);
///
/// // Quoted values with spaces
/// let p = parse_search_query("tag:\"Fools Theater\" camera:\"Canon EOS R5\"");
/// assert_eq!(p.tags, vec!["Fools Theater"]);
/// assert_eq!(p.cameras, vec!["Canon EOS R5"]);
///
/// // Rating range
/// let p = parse_search_query("rating:3-5");
/// assert_eq!(p.rating, Some(NumericFilter::Range(3.0, 5.0)));
///
/// // Free text (unrecognized tokens)
/// let p = parse_search_query("sunset beach");
/// assert_eq!(p.text, Some("sunset beach".to_string()));
/// ```
pub fn parse_search_query(query: &str) -> ParsedSearch {
    let mut parsed = ParsedSearch::default();
    let mut text_parts = Vec::new();

    'token: for token in tokenize_query(query) {
        // Detect negation prefix
        let (negated, token_body) = if token.starts_with('-') && token.len() > 1 && token.as_bytes()[1] != b'-' {
            (true, &token[1..])
        } else {
            (false, token.as_str())
        };

        // `stale:never` is the one non-numeric form of an otherwise numeric
        // filter; catch it before the numeric table swallows the token.
        if token_body == "stale:never" {
            parsed.stale_never = true;
            continue 'token;
        }

        // Table-driven dispatch for filter shapes that fit one of the four
        // patterns. The custom-parsing block below handles filters with their
        // own per-token logic (volume:none, scattered:N, geo:any/lat,lng,r,
        // faces:any/N, embed:any/true, similar:id:limit, text:"q":limit, meta:k=v).

        for (prefix, setter) in SIMPLE_FILTERS {
            if let Some(value) = token_body.strip_prefix(prefix) {
                setter(&mut parsed, value, negated);
                continue 'token;
            }
        }

        for (prefix, setter) in NUMERIC_FILTERS {
            if let Some(value) = token_body.strip_prefix(prefix) {
                setter(&mut parsed, parse_numeric_filter(value));
                continue 'token;
            }
        }

        for (prefix, setter) in STRING_FILTERS {
            if let Some(value) = token_body.strip_prefix(prefix) {
                setter(&mut parsed, value);
                continue 'token;
            }
        }

        for (full, setter) in BOOLEAN_TOKENS {
            if token_body == *full {
                setter(&mut parsed);
                continue 'token;
            }
        }

        // --- Custom-parsing filters (per-token logic) ---

        if let Some(value) = token_body.strip_prefix("meta:") {
            if let Some((key, val)) = value.split_once('=') {
                parsed.meta_filters.push((key.to_string(), val.to_string()));
            }
        } else if let Some(value) = token_body.strip_prefix("volume:") {
            if value == "none" {
                if negated { parsed.volume_any_online = true } else { parsed.volume_none = true }
            } else if negated {
                parsed.volumes_exclude.push(value.to_string());
            } else {
                parsed.volumes.push(value.to_string());
            }
        } else if let Some(value) = token_body.strip_prefix("label:") {
            if value == "none" {
                if negated { parsed.color_label_any = true } else { parsed.color_label_none = true }
            } else if negated {
                parsed.color_labels_exclude.push(value.to_string());
            } else {
                parsed.color_labels.push(value.to_string());
            }
        } else if let Some(value) = token_body.strip_prefix("scattered:") {
            // scattered:N / scattered:N+ — count of distinct session roots
            // (the former `/depth` suffix was parsed but never applied; it
            // is gone).
            parsed.scattered = parse_numeric_filter(value);
        } else if let Some(value) = token_body.strip_prefix("geo:") {
            if value == "any" {
                parsed.has_gps = Some(true);
            } else if value == "none" {
                parsed.has_gps = Some(false);
            } else {
                // Try lat,lng,radius_km or south,west,north,east
                let parts: Vec<f64> = value.split(',').filter_map(|s| s.parse().ok()).collect();
                if parts.len() == 3 {
                    // geo:lat,lng,radius_km → bounding box
                    let lat = parts[0];
                    let lng = parts[1];
                    let r = parts[2];
                    let dlat = r / 111.0;
                    let dlng = r / (111.0 * lat.to_radians().cos());
                    parsed.geo_bbox = Some((lat - dlat, lng - dlng, lat + dlat, lng + dlng));
                } else if parts.len() == 4 {
                    // geo:south,west,north,east
                    parsed.geo_bbox = Some((parts[0], parts[1], parts[2], parts[3]));
                }
            }
        } else if let Some(value) = token_body.strip_prefix("codec:") {
            parsed.codec = Some(value.to_string());
        } else if let Some(value) = token_body.strip_prefix("key:") {
            parsed.audio_key = Some(value.to_string());
        } else if let Some(value) = token_body.strip_prefix("bpm:") {
            parsed.audio_bpm = parse_numeric_filter(value);
        } else if let Some(value) = token_body.strip_prefix("faces:") {
            if value == "any" {
                parsed.has_faces = Some(true);
            } else if value == "none" {
                parsed.has_faces = Some(false);
            } else {
                parsed.face_count = parse_numeric_filter(value);
            }
        } else if let Some(value) = token_body.strip_prefix("embed:") {
            if value == "any" || value == "true" {
                parsed.has_embed = Some(true);
            } else if value == "none" || value == "false" {
                parsed.has_embed = Some(false);
            }
        } else if let Some(value) = token_body.strip_prefix("similar:") {
            // similar:<asset-id> or similar:<asset-id>:<limit>
            if let Some((id, limit_str)) = value.rsplit_once(':') {
                if let Ok(limit) = limit_str.parse::<usize>() {
                    parsed.similar = Some(id.to_string());
                    parsed.similar_limit = Some(limit);
                } else {
                    // Not a valid limit, treat entire value as asset ID
                    parsed.similar = Some(value.to_string());
                }
            } else {
                parsed.similar = Some(value.to_string());
            }
        } else if let Some(value) = token_body.strip_prefix("min_sim:") {
            if let Ok(v) = value.parse::<f32>() {
                parsed.min_sim = Some(v.clamp(0.0, 100.0));
            }
        } else if let Some(value) = token_body.strip_prefix("text:") {
            if !value.is_empty() {
                // text:"query":limit or text:query:limit or text:"query" or text:query
                // Check if the value ends with :<number> after the query part
                if let Some((query_part, limit_str)) = value.rsplit_once(':') {
                    if let Ok(limit) = limit_str.parse::<usize>() {
                        if !query_part.is_empty() {
                            parsed.text_query = Some(query_part.to_string());
                            parsed.text_query_limit = Some(limit);
                        }
                    } else {
                        parsed.text_query = Some(value.to_string());
                    }
                } else {
                    parsed.text_query = Some(value.to_string());
                }
            }
        } else if negated {
            // Negated free text: -word
            text_parts.push(token_body.to_string());
            // Actually this should go to text_exclude
            text_parts.pop();
            parsed.text_exclude.push(token_body.to_string());
        } else {
            text_parts.push(token);
        }
    }

    if !text_parts.is_empty() {
        parsed.text = Some(text_parts.join(" "));
    }

    parsed
}

// ═══ NUMERIC FILTER ═══

pub use crate::models::NumericFilter;

/// Parse a numeric filter value string into a NumericFilter.
///
/// # Examples
///
/// ```
/// use maki::query::parse_numeric_filter;
///
/// assert_eq!(parse_numeric_filter("3"), Some(maki::query::NumericFilter::Exact(3.0)));
/// assert_eq!(parse_numeric_filter("3+"), Some(maki::query::NumericFilter::Min(3.0)));
/// assert_eq!(parse_numeric_filter("3-5"), Some(maki::query::NumericFilter::Range(3.0, 5.0)));
/// assert_eq!(parse_numeric_filter("2,4"), Some(maki::query::NumericFilter::Values(vec![2.0, 4.0])));
/// ```
pub fn parse_numeric_filter(value: &str) -> Option<NumericFilter> {
    if value.contains(',') {
        let mut values = Vec::new();
        let mut min = None;
        for part in value.split(',') {
            let part = part.trim();
            if let Some(num_str) = part.strip_suffix('+') {
                if let Ok(n) = num_str.parse::<f64>() {
                    min = Some(n);
                }
            } else if part.contains('-') {
                if let Some((lo, hi)) = part.split_once('-') {
                    if let (Ok(a), Ok(b)) = (lo.parse::<f64>(), hi.parse::<f64>()) {
                        // Range inside comma list: return as range
                        return Some(NumericFilter::Range(a, b));
                    }
                }
            } else if let Ok(n) = part.parse::<f64>() {
                values.push(n);
            }
        }
        if let Some(m) = min {
            if values.is_empty() {
                Some(NumericFilter::Min(m))
            } else {
                Some(NumericFilter::ValuesOrMin { values, min: m })
            }
        } else if values.len() == 1 {
            Some(NumericFilter::Exact(values[0]))
        } else if !values.is_empty() {
            Some(NumericFilter::Values(values))
        } else {
            None
        }
    } else if let Some(num_str) = value.strip_suffix('+') {
        num_str.parse::<f64>().ok().map(NumericFilter::Min)
    } else if value.contains('-') {
        let (lo, hi) = value.split_once('-')?;
        let a = lo.parse::<f64>().ok()?;
        let b = hi.parse::<f64>().ok()?;
        Some(NumericFilter::Range(a, b))
    } else {
        value.parse::<f64>().ok().map(NumericFilter::Exact)
    }
}
// ═══ PATH NORMALIZATION ═══

/// Resolve and normalize a `path:` filter value for search.
///
/// When `cwd` is provided (CLI context):
/// - `~` or `~/...` is expanded to the user's home directory
/// - `./...` or `../...` is resolved relative to `cwd`
///
/// After resolution, if the path is absolute and matches a volume mount point
/// (longest prefix match), returns (volume-relative path, Some(volume_id)).
/// Otherwise returns (path, None) unchanged.
pub fn normalize_path_for_search(
    path: &str,
    volumes: &[Volume],
    cwd: Option<&std::path::Path>,
) -> (String, Option<String>) {
    // Step 1: Expand ~ and resolve ./ ../ when cwd is available
    let resolved = if let Some(cwd) = cwd {
        let home = std::env::var("HOME")
            .or_else(|_| std::env::var("USERPROFILE"));
        if path == "~" {
            home.map(|h| h.to_string())
                .unwrap_or_else(|_| path.to_string())
        } else if let Some(rest) = path.strip_prefix("~/") {
            home.map(|h| std::path::PathBuf::from(h).join(rest).to_string_lossy().to_string())
                .unwrap_or_else(|_| path.to_string())
        } else if path.starts_with("./") || path.starts_with("../") {
            let joined = cwd.join(path);
            // Clean the path components (handle ./ and ../) without requiring
            // the path to exist on disk (unlike canonicalize)
            clean_path(&joined)
        } else {
            path.to_string()
        }
    } else {
        path.to_string()
    };

    // Step 2: If absolute, try to match a volume mount point
    // On Windows, canonicalized paths have \\?\ prefix — strip it for matching
    #[cfg(windows)]
    let resolved = resolved.strip_prefix(r"\\?\").unwrap_or(&resolved).to_string();
    let p = std::path::Path::new(&resolved);
    if !p.is_absolute() {
        return (resolved, None);
    }

    let mut best: Option<&Volume> = None;
    let mut best_len = 0;

    for v in volumes {
        // On Windows, volume mount points may also have \\?\ prefix
        #[cfg(windows)]
        let mount = std::path::PathBuf::from(
            v.mount_point.to_string_lossy().strip_prefix(r"\\?\").unwrap_or(&v.mount_point.to_string_lossy())
        );
        #[cfg(unix)]
        let mount = &v.mount_point;
        if p.starts_with(mount) {
            let len = mount.as_os_str().len();
            if len > best_len {
                best = Some(v);
                best_len = len;
            }
        }
    }

    match best {
        Some(vol) => {
            // Use the same \\?\-stripped mount for strip_prefix
            #[cfg(windows)]
            let mount = std::path::PathBuf::from(
                vol.mount_point.to_string_lossy().strip_prefix(r"\\?\").unwrap_or(&vol.mount_point.to_string_lossy())
            );
            #[cfg(unix)]
            let mount = &vol.mount_point;
            let relative = crate::path_util::forward_slashes(p.strip_prefix(mount).unwrap());
            (relative, Some(vol.id.to_string()))
        }
        None => (resolved, None),
    }
}

/// Logically clean a path by resolving `.` and `..` components without
/// touching the filesystem (unlike `canonicalize` which requires the path to exist).
fn clean_path(path: &std::path::Path) -> String {
    let mut parts: Vec<&std::ffi::OsStr> = Vec::new();
    for component in path.components() {
        match component {
            std::path::Component::CurDir => {} // skip .
            std::path::Component::ParentDir => {
                parts.pop(); // go up
            }
            other => parts.push(other.as_os_str()),
        }
    }
    let result: std::path::PathBuf = parts.iter().collect();
    // Normalize to forward slashes for cross-platform consistency
    crate::path_util::forward_slashes(&result)
}


#[cfg(test)]
mod tests {
    use super::*;
    use crate::query::test_support::*;

    // ── parse_search_query tests ──────────────────────────────────

    #[test]
    fn parse_camera_filter() {
        let p = parse_search_query("camera:fuji");
        assert_eq!(p.cameras, vec!["fuji"]);
        assert!(p.text.is_none());
    }

    #[test]
    fn parse_lens_filter() {
        let p = parse_search_query("lens:56mm");
        assert_eq!(p.lenses, vec!["56mm"]);
    }

    #[test]
    fn parse_description_filter() {
        let p = parse_search_query("description:sunset");
        assert_eq!(p.descriptions, vec!["sunset"]);
        assert!(p.text.is_none());
    }

    #[test]
    fn parse_description_short_alias() {
        let p = parse_search_query("desc:sunset");
        assert_eq!(p.descriptions, vec!["sunset"]);
    }

    #[test]
    fn parse_description_negated() {
        let p = parse_search_query("-description:rejected");
        assert_eq!(p.descriptions_exclude, vec!["rejected"]);
        assert!(p.descriptions.is_empty());
    }

    #[test]
    fn parse_description_quoted() {
        let p = parse_search_query("description:\"colorful flowers\"");
        assert_eq!(p.descriptions, vec!["colorful flowers"]);
    }

    #[test]
    fn parse_iso_exact() {
        let p = parse_search_query("iso:3200");
        assert_eq!(p.iso, Some(NumericFilter::Exact(3200.0)));
    }

    #[test]
    fn parse_tagcount_exact() {
        let p = parse_search_query("tagcount:3");
        assert_eq!(p.tag_count, Some(NumericFilter::Exact(3.0)));
    }

    #[test]
    fn parse_tagcount_min() {
        let p = parse_search_query("tagcount:5+");
        assert_eq!(p.tag_count, Some(NumericFilter::Min(5.0)));
    }

    #[test]
    fn parse_tagcount_range() {
        let p = parse_search_query("tagcount:2-5");
        assert_eq!(p.tag_count, Some(NumericFilter::Range(2.0, 5.0)));
    }

    #[test]
    fn parse_tagcount_zero_finds_untagged() {
        // tagcount:0 = no leaf tags = no intentional tags = untagged asset.
        // Users restructuring a catalogue rely on this to find the gaps.
        let p = parse_search_query("tagcount:0");
        assert_eq!(p.tag_count, Some(NumericFilter::Exact(0.0)));
    }

    #[test]
    fn parse_iso_min() {
        let p = parse_search_query("iso:3200+");
        assert_eq!(p.iso, Some(NumericFilter::Min(3200.0)));
    }

    #[test]
    fn parse_iso_range() {
        let p = parse_search_query("iso:100-800");
        assert_eq!(p.iso, Some(NumericFilter::Range(100.0, 800.0)));
    }

    #[test]
    fn parse_focal_exact() {
        let p = parse_search_query("focal:50");
        assert_eq!(p.focal, Some(NumericFilter::Exact(50.0)));
    }

    #[test]
    fn parse_focal_range() {
        let p = parse_search_query("focal:35-70");
        assert_eq!(p.focal, Some(NumericFilter::Range(35.0, 70.0)));
    }

    #[test]
    fn parse_f_exact() {
        let p = parse_search_query("f:2.8");
        assert_eq!(p.aperture, Some(NumericFilter::Exact(2.8)));
    }

    #[test]
    fn parse_f_min() {
        let p = parse_search_query("f:2.8+");
        assert_eq!(p.aperture, Some(NumericFilter::Min(2.8)));
    }

    #[test]
    fn parse_f_range() {
        let p = parse_search_query("f:1.4-2.8");
        assert_eq!(p.aperture, Some(NumericFilter::Range(1.4, 2.8)));
    }

    #[test]
    fn parse_width_min() {
        let p = parse_search_query("width:4000+");
        assert_eq!(p.width, Some(NumericFilter::Min(4000.0)));
    }

    #[test]
    fn parse_height_min() {
        let p = parse_search_query("height:2000+");
        assert_eq!(p.height, Some(NumericFilter::Min(2000.0)));
    }

    #[test]
    fn parse_meta_filter() {
        let p = parse_search_query("meta:label=Red");
        assert_eq!(p.meta_filters.len(), 1);
        assert_eq!(p.meta_filters[0].0, "label");
        assert_eq!(p.meta_filters[0].1, "Red");
    }

    #[test]
    fn parse_mixed_filters_with_text() {
        let p = parse_search_query("camera:fuji sunset iso:400 landscape");
        assert_eq!(p.cameras, vec!["fuji"]);
        assert_eq!(p.iso, Some(NumericFilter::Exact(400.0)));
        assert_eq!(p.text.as_deref(), Some("sunset landscape"));
    }

    #[test]
    fn parse_existing_filters_still_work() {
        let p = parse_search_query("type:image tag:nature format:jpg rating:3+");
        assert_eq!(p.asset_types, vec!["image"]);
        assert_eq!(p.tags, vec!["nature"]);
        assert_eq!(p.formats, vec!["jpg"]);
        assert_eq!(p.rating, Some(NumericFilter::Min(3.0)));
    }

    #[test]
    fn parse_quoted_tag_with_spaces() {
        let p = parse_search_query(r#"tag:"Fools Theater" rating:4+"#);
        assert_eq!(p.tags, vec!["Fools Theater"]);
        assert_eq!(p.rating, Some(NumericFilter::Min(4.0)));
        assert!(p.text.is_none());
    }

    #[test]
    fn parse_quoted_camera_and_lens() {
        let p = parse_search_query(r#"camera:"Canon EOS R5" lens:"RF 50mm f/1.2""#);
        assert_eq!(p.cameras, vec!["Canon EOS R5"]);
        assert_eq!(p.lenses, vec!["RF 50mm f/1.2"]);
    }

    #[test]
    fn parse_quoted_label() {
        let p = parse_search_query(r#"label:"light blue" type:image"#);
        assert_eq!(p.color_labels, vec!["light blue"]);
        assert_eq!(p.asset_types, vec!["image"]);
    }

    #[test]
    fn parse_quoted_collection() {
        let p = parse_search_query(r#"collection:"My Favorites""#);
        assert_eq!(p.collections, vec!["My Favorites"]);
    }

    #[test]
    fn parse_mixed_quoted_and_unquoted() {
        let p = parse_search_query(r#"sunset tag:"Fools Theater" rating:5"#);
        assert_eq!(p.tags, vec!["Fools Theater"]);
        assert_eq!(p.rating, Some(NumericFilter::Exact(5.0)));
        assert_eq!(p.text.as_deref(), Some("sunset"));
    }

    #[test]
    fn tokenize_basic() {
        assert_eq!(tokenize_query("hello world"), vec!["hello", "world"]);
        assert_eq!(tokenize_query(r#"tag:"two words""#), vec!["tag:two words"]);
        assert_eq!(
            tokenize_query(r#"tag:"a b" rating:3+"#),
            vec!["tag:a b", "rating:3+"]
        );
        // Unmatched quote: consumes rest of input
        assert_eq!(tokenize_query(r#"tag:"open"#), vec!["tag:open"]);
        // Empty input
        assert!(tokenize_query("").is_empty());
        assert!(tokenize_query("   ").is_empty());
    }

    #[test]
    fn parse_orphan_filter() {
        let p = parse_search_query("orphan:true");
        assert!(p.orphan);
        assert!(!p.orphan_false);
        assert!(p.text.is_none());
    }

    #[test]
    fn parse_orphan_false_filter() {
        let p = parse_search_query("orphan:false");
        assert!(!p.orphan);
        assert!(p.orphan_false);
    }

    #[test]
    fn parse_missing_filter() {
        let p = parse_search_query("missing:true");
        assert!(p.missing);
        assert!(p.text.is_none());
    }

    #[test]
    fn parse_stale_filter() {
        let p = parse_search_query("stale:30");
        assert_eq!(p.stale_days, Some(NumericFilter::Exact(30.0)));
        assert!(p.text.is_none());
    }

    #[test]
    fn parse_stale_filter_zero() {
        let p = parse_search_query("stale:0");
        assert_eq!(p.stale_days, Some(NumericFilter::Exact(0.0)));
    }

    #[test]
    fn parse_volume_none_filter() {
        let p = parse_search_query("volume:none");
        assert!(p.volume_none);
        assert!(p.volumes.is_empty());
        assert!(p.text.is_none());
    }

    #[test]
    fn parse_volume_label_filter() {
        let p = parse_search_query("volume:Photos");
        assert!(!p.volume_none);
        assert_eq!(p.volumes, vec!["Photos"]);
        assert!(p.volumes_exclude.is_empty());
    }

    #[test]
    fn parse_volume_label_negated() {
        let p = parse_search_query("-volume:Archive");
        assert_eq!(p.volumes_exclude, vec!["Archive"]);
        assert!(p.volumes.is_empty());
    }

    #[test]
    fn parse_volume_label_comma_or() {
        let p = parse_search_query("volume:Photos,Archive");
        assert_eq!(p.volumes, vec!["Photos,Archive"]);
    }

    #[test]
    fn parse_volume_label_with_other_filters() {
        let p = parse_search_query("volume:Working type:image rating:3+");
        assert_eq!(p.volumes, vec!["Working"]);
        assert_eq!(p.asset_types, vec!["image"]);
        assert_eq!(p.rating, Some(NumericFilter::Min(3.0)));
    }

    #[test]
    fn parse_volume_quoted_label() {
        let p = parse_search_query("volume:\"External SSD\" type:image");
        assert_eq!(p.volumes, vec!["External SSD"]);
        assert_eq!(p.asset_types, vec!["image"]);
    }

    #[test]
    fn parse_location_health_combined() {
        let p = parse_search_query("orphan:true stale:7 tag:landscape");
        assert!(p.orphan);
        assert_eq!(p.stale_days, Some(NumericFilter::Exact(7.0)));
        assert_eq!(p.tags, vec!["landscape"]);
        assert!(!p.missing);
        assert!(!p.volume_none);
    }

    #[test]
    fn parse_label_filter() {
        let p = parse_search_query("label:Red");
        assert_eq!(p.color_labels, vec!["Red"]);
        assert!(!p.color_label_none);
        assert!(p.text.is_none());
    }

    #[test]
    fn parse_label_none_filter() {
        let p = parse_search_query("label:none");
        assert!(p.color_label_none);
        assert!(p.color_labels.is_empty());
        assert!(p.text.is_none());
    }

    #[test]
    fn parse_label_none_negated_means_labeled() {
        let p = parse_search_query("-label:none");
        assert!(p.color_label_any);
        assert!(!p.color_label_none);
        assert!(p.color_labels_exclude.is_empty());
    }

    #[test]
    fn parse_volume_none_negated_means_online() {
        let p = parse_search_query("-volume:none");
        assert!(p.volume_any_online);
        assert!(!p.volume_none);
        assert!(p.volumes_exclude.is_empty());
    }

    #[test]
    fn parse_stale_never() {
        let p = parse_search_query("stale:never");
        assert!(p.stale_never);
        assert!(p.stale_days.is_none());
        assert!(p.text.is_none());
        let p = parse_search_query("stale:30");
        assert!(!p.stale_never);
        assert_eq!(p.stale_days, Some(NumericFilter::Exact(30.0)));
    }

    #[test]
    fn parse_label_with_other_filters() {
        let p = parse_search_query("label:Blue tag:landscape sunset");
        assert_eq!(p.color_labels, vec!["Blue"]);
        assert_eq!(p.tags, vec!["landscape"]);
        assert_eq!(p.text.as_deref(), Some("sunset"));
    }

    #[test]
    fn parse_path_filter() {
        let p = parse_search_query("path:Capture/2026-02-22");
        assert_eq!(p.path_prefixes, vec!["Capture/2026-02-22"]);
        assert!(p.text.is_none());
    }

    #[test]
    fn parse_path_filter_quoted() {
        let p = parse_search_query(r#"path:"Photos/My Trip""#);
        assert_eq!(p.path_prefixes, vec!["Photos/My Trip"]);
    }

    #[test]
    fn parse_path_with_other_filters() {
        let p = parse_search_query("path:Capture/2026 rating:3+ tag:landscape");
        assert_eq!(p.path_prefixes, vec!["Capture/2026"]);
        assert_eq!(p.rating, Some(NumericFilter::Min(3.0)));
        assert_eq!(p.tags, vec!["landscape"]);
        assert!(p.text.is_none());
    }

    // ── copies filter parse tests ─────────────────────────────────

    #[test]
    fn parse_copies_exact() {
        let p = parse_search_query("copies:2");
        assert_eq!(p.copies, Some(NumericFilter::Exact(2.0)));
        assert!(p.text.is_none());
    }

    #[test]
    fn parse_copies_min() {
        let p = parse_search_query("copies:2+");
        assert_eq!(p.copies, Some(NumericFilter::Min(2.0)));
        assert!(p.text.is_none());
    }

    #[test]
    fn parse_copies_with_other_filters() {
        let p = parse_search_query("copies:3+ rating:4+ tag:landscape");
        assert_eq!(p.copies, Some(NumericFilter::Min(3.0)));
        assert_eq!(p.rating, Some(NumericFilter::Min(4.0)));
        assert_eq!(p.tags, vec!["landscape"]);
    }

    // ── variants filter parse tests ─────────────────────────────────

    #[test]
    fn parse_variants_exact() {
        let p = parse_search_query("variants:3");
        assert_eq!(p.variant_count, Some(NumericFilter::Exact(3.0)));
    }

    #[test]
    fn parse_variants_min() {
        let p = parse_search_query("variants:3+");
        assert_eq!(p.variant_count, Some(NumericFilter::Min(3.0)));
    }

    #[test]
    fn parse_variants_with_other_filters() {
        let p = parse_search_query("variants:5+ tag:landscape");
        assert_eq!(p.variant_count, Some(NumericFilter::Min(5.0)));
        assert_eq!(p.tags, vec!["landscape"]);
    }

    // ── scattered filter parse tests ─────────────────────────────────

    #[test]
    fn parse_scattered() {
        let p = parse_search_query("scattered:2");
        assert_eq!(p.scattered, Some(NumericFilter::Exact(2.0)));
    }

    #[test]
    fn parse_scattered_with_variants() {
        let p = parse_search_query("scattered:2 variants:3+");
        assert_eq!(p.scattered, Some(NumericFilter::Exact(2.0)));
        assert_eq!(p.variant_count, Some(NumericFilter::Min(3.0)));
    }

    #[test]
    fn parse_scattered_with_plus_suffix() {
        let p = parse_search_query("scattered:2+");
        assert_eq!(p.scattered, Some(NumericFilter::Min(2.0)));
    }

    #[test]
    fn parse_duration_exact() {
        let p = parse_search_query("duration:60");
        assert_eq!(p.duration, Some(NumericFilter::Exact(60.0)));
    }

    #[test]
    fn parse_duration_min() {
        let p = parse_search_query("duration:30+");
        assert_eq!(p.duration, Some(NumericFilter::Min(30.0)));
    }

    #[test]
    fn parse_duration_range() {
        let p = parse_search_query("duration:10-120");
        assert_eq!(p.duration, Some(NumericFilter::Range(10.0, 120.0)));
    }

    #[test]
    fn parse_duration_with_type() {
        let p = parse_search_query("duration:60+ type:video");
        assert_eq!(p.duration, Some(NumericFilter::Min(60.0)));
        assert_eq!(p.asset_types, vec!["video"]);
    }

    // ── codec filter parse tests ──────────────────────────────────────

    #[test]
    fn parse_codec() {
        let p = parse_search_query("codec:h264");
        assert_eq!(p.codec, Some("h264".to_string()));
    }

    #[test]
    fn parse_codec_with_duration() {
        let p = parse_search_query("codec:hevc duration:60+");
        assert_eq!(p.codec, Some("hevc".to_string()));
        assert_eq!(p.duration, Some(NumericFilter::Min(60.0)));
    }

    #[test]
    fn parse_rating_comma_separated() {
        // rating:4,5 → exact values 4 and 5
        let p = parse_search_query("rating:4,5");
        assert_eq!(p.rating, Some(NumericFilter::Values(vec![4.0, 5.0])));
    }

    #[test]
    fn parse_rating_comma_with_min() {
        // rating:2,4+ → exact 2 OR minimum 4
        let p = parse_search_query("rating:2,4+");
        assert_eq!(p.rating, Some(NumericFilter::ValuesOrMin { values: vec![2.0], min: 4.0 }));
    }

    #[test]
    fn parse_rating_comma_multiple() {
        let p = parse_search_query("rating:1,3,5");
        assert_eq!(p.rating, Some(NumericFilter::Values(vec![1.0, 3.0, 5.0])));
    }

    #[test]
    fn parse_rating_range() {
        let p = parse_search_query("rating:3-5");
        assert_eq!(p.rating, Some(NumericFilter::Range(3.0, 5.0)));
    }

    #[test]
    fn parse_rating_range_low() {
        let p = parse_search_query("rating:1-2");
        assert_eq!(p.rating, Some(NumericFilter::Range(1.0, 2.0)));
    }

    // ── date filter parse tests ─────────────────────────────────────

    #[test]
    fn parse_date_prefix_day() {
        let p = parse_search_query("date:2026-02-25");
        assert_eq!(p.date_prefix.as_deref(), Some("2026-02-25"));
        assert!(p.text.is_none());
    }

    #[test]
    fn parse_date_prefix_month() {
        let p = parse_search_query("date:2026-02");
        assert_eq!(p.date_prefix.as_deref(), Some("2026-02"));
    }

    #[test]
    fn parse_date_prefix_year() {
        let p = parse_search_query("date:2026");
        assert_eq!(p.date_prefix.as_deref(), Some("2026"));
    }

    #[test]
    fn parse_date_from() {
        let p = parse_search_query("dateFrom:2026-01-15");
        assert_eq!(p.date_from.as_deref(), Some("2026-01-15"));
        assert!(p.text.is_none());
    }

    #[test]
    fn parse_date_until() {
        let p = parse_search_query("dateUntil:2026-02-28");
        assert_eq!(p.date_until.as_deref(), Some("2026-02-28"));
    }

    #[test]
    fn parse_date_range_combined() {
        let p = parse_search_query("dateFrom:2026-01-01 dateUntil:2026-12-31 tag:landscape");
        assert_eq!(p.date_from.as_deref(), Some("2026-01-01"));
        assert_eq!(p.date_until.as_deref(), Some("2026-12-31"));
        assert_eq!(p.tags, vec!["landscape"]);
    }

    #[test]
    fn parse_geo_any() {
        let p = parse_search_query("geo:any");
        assert_eq!(p.has_gps, Some(true));
        assert!(p.geo_bbox.is_none());
    }

    #[test]
    fn parse_geo_none() {
        let p = parse_search_query("geo:none");
        assert_eq!(p.has_gps, Some(false));
    }

    #[test]
    fn parse_geo_lat_lng_radius() {
        let p = parse_search_query("geo:52.5,13.4,10");
        assert!(p.geo_bbox.is_some());
        let (s, w, n, e) = p.geo_bbox.unwrap();
        assert!((s - (52.5 - 10.0/111.0)).abs() < 0.01);
        assert!(n > s);
        assert!(e > w);
        assert!(w < 13.4);
        assert!(e > 13.4);
    }

    #[test]
    fn parse_geo_bbox() {
        let p = parse_search_query("geo:50,10,55,15");
        assert!(p.geo_bbox.is_some());
        let (s, w, n, e) = p.geo_bbox.unwrap();
        assert!((s - 50.0).abs() < 0.001);
        assert!((w - 10.0).abs() < 0.001);
        assert!((n - 55.0).abs() < 0.001);
        assert!((e - 15.0).abs() < 0.001);
    }

    #[test]
    fn parse_embed_any() {
        let p = parse_search_query("embed:any");
        assert_eq!(p.has_embed, Some(true));
    }

    #[test]
    fn parse_embed_true() {
        let p = parse_search_query("embed:true");
        assert_eq!(p.has_embed, Some(true));
    }

    #[test]
    fn parse_embed_none() {
        let p = parse_search_query("embed:none");
        assert_eq!(p.has_embed, Some(false));
    }

    #[test]
    fn parse_embed_false() {
        let p = parse_search_query("embed:false");
        assert_eq!(p.has_embed, Some(false));
    }

    // ── negation and OR parse tests ────────────────────────────────

    #[test]
    fn parse_negated_tag() {
        let p = parse_search_query("-tag:rejected");
        assert!(p.tags.is_empty());
        assert_eq!(p.tags_exclude, vec!["rejected"]);
        assert!(p.text.is_none());
    }

    #[test]
    fn parse_negated_format() {
        let p = parse_search_query("-format:xmp");
        assert!(p.formats.is_empty());
        assert_eq!(p.formats_exclude, vec!["xmp"]);
    }

    #[test]
    fn parse_negated_type() {
        let p = parse_search_query("-type:other");
        assert!(p.asset_types.is_empty());
        assert_eq!(p.asset_types_exclude, vec!["other"]);
    }

    #[test]
    fn parse_negated_label() {
        let p = parse_search_query("-label:Red");
        assert!(p.color_labels.is_empty());
        assert_eq!(p.color_labels_exclude, vec!["Red"]);
    }

    #[test]
    fn parse_negated_camera() {
        let p = parse_search_query("-camera:phone");
        assert!(p.cameras.is_empty());
        assert_eq!(p.cameras_exclude, vec!["phone"]);
    }

    #[test]
    fn parse_negated_lens() {
        let p = parse_search_query("-lens:kit");
        assert!(p.lenses.is_empty());
        assert_eq!(p.lenses_exclude, vec!["kit"]);
    }

    #[test]
    fn parse_negated_collection() {
        let p = parse_search_query("-collection:Rejects");
        assert!(p.collections.is_empty());
        assert_eq!(p.collections_exclude, vec!["Rejects"]);
    }

    #[test]
    fn parse_negated_path() {
        let p = parse_search_query("-path:Trash");
        assert!(p.path_prefixes.is_empty());
        assert_eq!(p.path_prefixes_exclude, vec!["Trash"]);
    }

    #[test]
    fn parse_negated_text() {
        let p = parse_search_query("sunset -boring");
        assert_eq!(p.text.as_deref(), Some("sunset"));
        assert_eq!(p.text_exclude, vec!["boring"]);
    }

    #[test]
    fn parse_negated_quoted_tag() {
        let p = parse_search_query(r#"-tag:"Fools Theater""#);
        assert_eq!(p.tags_exclude, vec!["Fools Theater"]);
        assert!(p.tags.is_empty());
    }

    #[test]
    fn parse_comma_or_format() {
        let p = parse_search_query("format:nef,cr3");
        assert_eq!(p.formats, vec!["nef,cr3"]);
    }

    #[test]
    fn parse_comma_or_tag() {
        let p = parse_search_query("tag:alice,bob");
        assert_eq!(p.tags, vec!["alice,bob"]);
    }

    #[test]
    fn parse_comma_or_type() {
        let p = parse_search_query("type:image,video");
        assert_eq!(p.asset_types, vec!["image,video"]);
    }

    #[test]
    fn parse_repeated_tags_are_and() {
        let p = parse_search_query("tag:landscape tag:sunset");
        assert_eq!(p.tags, vec!["landscape", "sunset"]);
    }

    #[test]
    fn parse_combined_negation_or_and() {
        let p = parse_search_query("tag:alice,bob tag:portrait -tag:rejected -type:other");
        assert_eq!(p.tags, vec!["alice,bob", "portrait"]);
        assert_eq!(p.tags_exclude, vec!["rejected"]);
        assert_eq!(p.asset_types_exclude, vec!["other"]);
    }

    #[test]
    fn parse_negation_does_not_affect_rating() {
        // Rating doesn't support negation — the `-` is ignored
        let p = parse_search_query("-rating:3+");
        assert_eq!(p.rating, Some(NumericFilter::Min(3.0)));
        assert!(p.text.is_none());
    }

    #[test]
    fn parse_negation_with_all_filters() {
        let p = parse_search_query("tag:keep -tag:reject format:nef,cr3 -format:xmp label:Red -label:Blue camera:nikon -camera:phone");
        assert_eq!(p.tags, vec!["keep"]);
        assert_eq!(p.tags_exclude, vec!["reject"]);
        assert_eq!(p.formats, vec!["nef,cr3"]);
        assert_eq!(p.formats_exclude, vec!["xmp"]);
        assert_eq!(p.color_labels, vec!["Red"]);
        assert_eq!(p.color_labels_exclude, vec!["Blue"]);
        assert_eq!(p.cameras, vec!["nikon"]);
        assert_eq!(p.cameras_exclude, vec!["phone"]);
    }

    #[test]
    fn parse_multiple_text_excludes() {
        let p = parse_search_query("sunset -boring -blurry");
        assert_eq!(p.text.as_deref(), Some("sunset"));
        assert_eq!(p.text_exclude, vec!["boring", "blurry"]);
    }

    #[test]
    fn parse_similar_basic() {
        let p = parse_search_query("similar:abc12345");
        assert_eq!(p.similar.as_deref(), Some("abc12345"));
        assert!(p.similar_limit.is_none());
    }

    #[test]
    fn parse_similar_with_limit() {
        let p = parse_search_query("similar:abc12345:50");
        assert_eq!(p.similar.as_deref(), Some("abc12345"));
        assert_eq!(p.similar_limit, Some(50));
    }

    #[test]
    fn parse_similar_with_other_filters() {
        let p = parse_search_query("similar:abc12345 rating:3+ tag:landscape");
        assert_eq!(p.similar.as_deref(), Some("abc12345"));
        assert_eq!(p.rating, Some(NumericFilter::Min(3.0)));
        assert_eq!(p.tags, vec!["landscape"]);
    }

    #[test]
    fn parse_similar_uuid_like() {
        let p = parse_search_query("similar:550e8400-e29b-41d4-a716-446655440000");
        assert_eq!(p.similar.as_deref(), Some("550e8400-e29b-41d4-a716-446655440000"));
        assert!(p.similar_limit.is_none());
    }

    #[test]
    fn parse_similar_uuid_with_limit() {
        let p = parse_search_query("similar:550e8400-e29b-41d4-a716-446655440000:10");
        assert_eq!(p.similar.as_deref(), Some("550e8400-e29b-41d4-a716-446655440000"));
        assert_eq!(p.similar_limit, Some(10));
    }

    #[test]
    fn parse_min_sim() {
        let p = parse_search_query("similar:abc123 min_sim:90");
        assert_eq!(p.similar.as_deref(), Some("abc123"));
        assert_eq!(p.min_sim, Some(90.0));
    }

    #[test]
    fn parse_min_sim_without_similar() {
        let p = parse_search_query("min_sim:85");
        assert_eq!(p.min_sim, Some(85.0));
        assert!(p.similar.is_none());
    }

    #[test]
    fn parse_text_query_basic() {
        let p = parse_search_query("text:sunset");
        assert_eq!(p.text_query.as_deref(), Some("sunset"));
    }

    #[test]
    fn parse_text_query_quoted() {
        let p = parse_search_query("text:\"sunset on the beach\"");
        assert_eq!(p.text_query.as_deref(), Some("sunset on the beach"));
    }

    #[test]
    fn parse_text_query_with_other_filters() {
        let p = parse_search_query("text:\"colorful flowers\" rating:3+ type:image");
        assert_eq!(p.text_query.as_deref(), Some("colorful flowers"));
        assert_eq!(p.rating, Some(NumericFilter::Min(3.0)));
        assert_eq!(p.asset_types, vec!["image".to_string()]);
    }

    #[test]
    fn parse_text_query_empty_ignored() {
        let p = parse_search_query("text:");
        assert!(p.text_query.is_none());
    }

    #[test]
    fn parse_double_dash_not_negated() {
        // A token starting with `--` should not be treated as negation
        let p = parse_search_query("--help");
        assert_eq!(p.text.as_deref(), Some("--help"));
        assert!(p.text_exclude.is_empty());
    }


    // ── normalize_path_for_search tests ────────────────────────────


    #[test]
    #[cfg(unix)]
    fn normalize_absolute_path_matching_volume() {
        let vol = make_volume("Photos", "/Volumes/Photos");
        let (rel, vid) = normalize_path_for_search(
            "/Volumes/Photos/Capture/2026", std::slice::from_ref(&vol), None,
        );
        assert_eq!(rel, "Capture/2026");
        assert_eq!(vid, Some(vol.id.to_string()));
    }

    #[test]
    fn normalize_absolute_path_no_match() {
        let vol = make_volume("Photos", "/Volumes/Photos");
        let (rel, vid) = normalize_path_for_search("/mnt/other/data", &[vol], None);
        assert_eq!(rel, "/mnt/other/data");
        assert!(vid.is_none());
    }

    #[test]
    fn normalize_relative_path_unchanged() {
        let vol = make_volume("Photos", "/Volumes/Photos");
        let (rel, vid) = normalize_path_for_search("Capture/2026", &[vol], None);
        assert_eq!(rel, "Capture/2026");
        assert!(vid.is_none());
    }

    #[test]
    #[cfg(unix)]
    fn normalize_picks_longest_mount_point() {
        let vol_parent = make_volume("Root", "/Volumes");
        let vol_child = make_volume("Photos", "/Volumes/Photos");
        let volumes = vec![vol_parent, vol_child.clone()];
        let (rel, vid) = normalize_path_for_search(
            "/Volumes/Photos/Capture/2026", &volumes, None,
        );
        assert_eq!(rel, "Capture/2026");
        assert_eq!(vid, Some(vol_child.id.to_string()));
    }

    #[test]
    #[cfg(unix)]
    fn normalize_tilde_expands_to_home() {
        let home = std::env::var("HOME").unwrap();
        let vol = make_volume("Home", &home);
        let cwd = std::path::Path::new("/tmp");

        let (rel, vid) = normalize_path_for_search(
            "~/Photos/2026", std::slice::from_ref(&vol), Some(cwd),
        );
        assert_eq!(rel, "Photos/2026");
        assert_eq!(vid, Some(vol.id.to_string()));
    }

    #[test]
    #[cfg(unix)]
    fn normalize_tilde_alone() {
        let home = std::env::var("HOME").unwrap();
        let vol = make_volume("Home", &home);
        let cwd = std::path::Path::new("/tmp");

        let (rel, vid) = normalize_path_for_search("~", std::slice::from_ref(&vol), Some(cwd));
        assert_eq!(rel, "");
        assert_eq!(vid, Some(vol.id.to_string()));
    }

    #[test]
    fn normalize_tilde_without_cwd_unchanged() {
        let vol = make_volume("Photos", "/Volumes/Photos");
        let (rel, vid) = normalize_path_for_search("~/Photos", &[vol], None);
        assert_eq!(rel, "~/Photos");
        assert!(vid.is_none());
    }

    #[test]
    #[cfg(unix)]
    fn normalize_dot_slash_resolves_relative_to_cwd() {
        let vol = make_volume("Photos", "/Volumes/Photos");
        let cwd = std::path::Path::new("/Volumes/Photos/Capture");

        let (rel, vid) = normalize_path_for_search(
            "./2026-02-22", std::slice::from_ref(&vol), Some(cwd),
        );
        assert_eq!(rel, "Capture/2026-02-22");
        assert_eq!(vid, Some(vol.id.to_string()));
    }

    #[test]
    #[cfg(unix)]
    fn normalize_dotdot_resolves_relative_to_cwd() {
        let vol = make_volume("Photos", "/Volumes/Photos");
        let cwd = std::path::Path::new("/Volumes/Photos/Capture/2026");

        let (rel, vid) = normalize_path_for_search(
            "../2025", std::slice::from_ref(&vol), Some(cwd),
        );
        assert_eq!(rel, "Capture/2025");
        assert_eq!(vid, Some(vol.id.to_string()));
    }

    #[test]
    fn normalize_plain_relative_unchanged_even_with_cwd() {
        let vol = make_volume("Photos", "/Volumes/Photos");
        let cwd = std::path::Path::new("/Volumes/Photos/Capture");

        let (rel, vid) = normalize_path_for_search(
            "Capture/2026", &[vol], Some(cwd),
        );
        // Plain relative paths stay as volume-relative prefix matches
        assert_eq!(rel, "Capture/2026");
        assert!(vid.is_none());
    }

    // ── Windows path normalization tests ────────────────────────────

    #[test]
    #[cfg(windows)]
    fn normalize_windows_absolute_path_matching_volume() {
        let vol = make_volume("Photos", r"D:\Photos");
        let (rel, vid) = normalize_path_for_search(
            r"D:\Photos\Capture\2026", &[vol.clone()], None,
        );
        assert_eq!(rel, "Capture/2026");
        assert_eq!(vid, Some(vol.id.to_string()));
    }

    #[test]
    #[cfg(windows)]
    fn normalize_windows_picks_longest_mount_point() {
        let vol_parent = make_volume("Drive", r"D:\");
        let vol_child = make_volume("Photos", r"D:\Photos");
        let (rel, vid) = normalize_path_for_search(
            r"D:\Photos\Capture\2026", &[vol_parent, vol_child.clone()], None,
        );
        assert_eq!(rel, "Capture/2026");
        assert_eq!(vid, Some(vol_child.id.to_string()));
    }

    #[test]
    #[cfg(windows)]
    fn normalize_windows_tilde_expands_to_userprofile() {
        let home = std::env::var("USERPROFILE").unwrap();
        let vol = make_volume("Home", &home);
        let cwd = std::path::Path::new(r"C:\Temp");

        let (rel, vid) = normalize_path_for_search(
            "~/Photos/2026", &[vol.clone()], Some(cwd),
        );
        assert_eq!(rel, "Photos/2026");
        assert_eq!(vid, Some(vol.id.to_string()));
    }

    #[test]
    #[cfg(windows)]
    fn normalize_windows_dot_slash_resolves_relative_to_cwd() {
        let vol = make_volume("Photos", r"D:\Photos");
        let cwd = std::path::Path::new(r"D:\Photos\Capture");

        let (rel, vid) = normalize_path_for_search(
            "./2026-02-22", &[vol.clone()], Some(cwd),
        );
        assert_eq!(rel, "Capture/2026-02-22");
        assert_eq!(vid, Some(vol.id.to_string()));
    }

    #[test]
    #[cfg(windows)]
    fn normalize_windows_dotdot_resolves_relative_to_cwd() {
        let vol = make_volume("Photos", r"D:\Photos");
        let cwd = std::path::Path::new(r"D:\Photos\Capture\2026");

        let (rel, vid) = normalize_path_for_search(
            "../2025", &[vol.clone()], Some(cwd),
        );
        assert_eq!(rel, "Capture/2025");
        assert_eq!(vid, Some(vol.id.to_string()));
    }

    #[test]
    fn merge_from_combines_vec_fields() {
        let mut base = parse_search_query("tag:sunset");
        let default = parse_search_query("-tag:rest rating:1+");
        base.merge_from(&default);
        assert_eq!(base.tags, vec!["sunset".to_string()]);
        assert_eq!(base.tags_exclude, vec!["rest".to_string()]);
        assert_eq!(base.rating, Some(NumericFilter::Min(1.0)));
    }

    #[test]
    fn merge_from_prefers_self_options() {
        let mut base = parse_search_query("rating:3+");
        let default = parse_search_query("rating:1+");
        base.merge_from(&default);
        // Self's rating takes priority
        assert_eq!(base.rating, Some(NumericFilter::Min(3.0)));
    }

    #[test]
    fn merge_from_empty_base() {
        let mut base = ParsedSearch::default();
        let default = parse_search_query("-tag:rest type:image");
        base.merge_from(&default);
        assert_eq!(base.tags_exclude, vec!["rest".to_string()]);
        assert_eq!(base.asset_types, vec!["image".to_string()]);
    }

    /// Regression: `duration:`, `codec:`, `key:` and `bpm:` were missing
    /// from `merge_from`, so a `[browse] default_filter` using them was
    /// silently ignored.
    #[test]
    fn merge_from_carries_media_filters() {
        let mut base = ParsedSearch::default();
        let default = parse_search_query("duration:60+ codec:h264 key:Am bpm:100-140");
        assert_eq!(default.duration, Some(NumericFilter::Min(60.0)));
        base.merge_from(&default);
        assert_eq!(base.duration, Some(NumericFilter::Min(60.0)));
        assert_eq!(base.codec.as_deref(), Some("h264"));
        assert_eq!(base.audio_key.as_deref(), Some("Am"));
        assert_eq!(base.audio_bpm, Some(NumericFilter::Range(100.0, 140.0)));

        // Self wins over the default for every one of them.
        let mut base = parse_search_query("duration:10 codec:hevc key:C bpm:90");
        base.merge_from(&default);
        assert_eq!(base.duration, Some(NumericFilter::Exact(10.0)));
        assert_eq!(base.codec.as_deref(), Some("hevc"));
        assert_eq!(base.audio_key.as_deref(), Some("C"));
        assert_eq!(base.audio_bpm, Some(NumericFilter::Exact(90.0)));
    }

    /// The parser is build-independent: the `ai`-only filters land in
    /// `ParsedSearch` whether or not the feature is compiled in (the
    /// search bails at execution instead).
    #[test]
    fn ai_filters_parse_identically_in_every_build() {
        let p = parse_search_query("similar:abc12345:20 min_sim:75 text:sunset:10 rating:4");
        assert_eq!(p.similar.as_deref(), Some("abc12345"));
        assert_eq!(p.similar_limit, Some(20));
        assert_eq!(p.min_sim, Some(75.0));
        assert_eq!(p.text_query.as_deref(), Some("sunset"));
        assert_eq!(p.text_query_limit, Some(10));
        assert_eq!(p.rating, Some(NumericFilter::Exact(4.0)));
    }
}
