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
    pub scattered_depth: Option<u32>,
    pub face_count: Option<NumericFilter>,
    /// `tagcount:N` — number of leaf tags (intentional tags the user applied,
    /// excluding auto-expanded ancestors). See `tag_util::leaf_tag_count`.
    pub tag_count: Option<NumericFilter>,
    pub duration: Option<NumericFilter>,
    pub codec: Option<String>,
    pub stale_days: Option<NumericFilter>,
    pub meta_filters: Vec<(String, String)>,
    pub orphan: bool,
    pub orphan_false: bool,
    pub missing: bool,
    pub volumes: Vec<String>,
    pub volumes_exclude: Vec<String>,
    pub volume_none: bool,
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
    #[cfg(feature = "ai")]
    pub similar: Option<String>,
    #[cfg(feature = "ai")]
    pub similar_limit: Option<usize>,
    #[cfg(feature = "ai")]
    pub min_sim: Option<f32>,
    #[cfg(feature = "ai")]
    pub text_query: Option<String>,
    #[cfg(feature = "ai")]
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
        self.stale_days = NumericFilter::or(&self.stale_days, &other.stale_days);
        if self.date_prefix.is_none() { self.date_prefix = other.date_prefix.clone(); }
        if self.date_from.is_none() { self.date_from = other.date_from.clone(); }
        if self.date_until.is_none() { self.date_until = other.date_until.clone(); }
        if self.stacked.is_none() { self.stacked = other.stacked; }
        if self.geo_bbox.is_none() { self.geo_bbox = other.geo_bbox; }
        if self.has_gps.is_none() { self.has_gps = other.has_gps; }
        if self.has_faces.is_none() { self.has_faces = other.has_faces; }
        if self.has_embed.is_none() { self.has_embed = other.has_embed; }
        #[cfg(feature = "ai")]
        {
            if self.similar.is_none() { self.similar = other.similar.clone(); }
            if self.similar_limit.is_none() { self.similar_limit = other.similar_limit; }
            if self.min_sim.is_none() { self.min_sim = other.min_sim; }
            if self.text_query.is_none() { self.text_query = other.text_query.clone(); }
            if self.text_query_limit.is_none() { self.text_query_limit = other.text_query_limit; }
        }

        // Bool fields: OR
        self.orphan = self.orphan || other.orphan;
        self.orphan_false = self.orphan_false || other.orphan_false;
        self.missing = self.missing || other.missing;
        self.volume_none = self.volume_none || other.volume_none;
        self.color_label_none = self.color_label_none || other.color_label_none;
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
            scattered_depth: self.scattered_depth,
            face_count: self.face_count.clone(),
            tag_count: self.tag_count.clone(),
            duration: self.duration.clone(),
            codec: self.codec.clone(),
            stale_days: self.stale_days.clone(),
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
pub(super) fn tokenize_query(query: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut chars = query.chars().peekable();

    while chars.peek().is_some() {
        // Skip whitespace
        while chars.peek().map_or(false, |c| c.is_whitespace()) {
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

    for token in tokenize_query(query) {
        // Detect negation prefix
        let (negated, token_body) = if token.starts_with('-') && token.len() > 1 && token.as_bytes()[1] != b'-' {
            (true, &token[1..])
        } else {
            (false, token.as_str())
        };

        if let Some(value) = token_body.strip_prefix("id:") {
            parsed.asset_ids.push(value.to_string());
        } else if let Some(value) = token_body.strip_prefix("type:") {
            if negated {
                parsed.asset_types_exclude.push(value.to_string());
            } else {
                parsed.asset_types.push(value.to_string());
            }
        } else if let Some(value) = token_body.strip_prefix("tag:") {
            if negated {
                parsed.tags_exclude.push(value.to_string());
            } else {
                parsed.tags.push(value.to_string());
            }
        } else if let Some(value) = token_body.strip_prefix("format:") {
            if negated {
                parsed.formats_exclude.push(value.to_string());
            } else {
                parsed.formats.push(value.to_string());
            }
        } else if let Some(value) = token_body.strip_prefix("rating:") {
            parsed.rating = parse_numeric_filter(value);
        } else if let Some(value) = token_body.strip_prefix("camera:") {
            if negated {
                parsed.cameras_exclude.push(value.to_string());
            } else {
                parsed.cameras.push(value.to_string());
            }
        } else if let Some(value) = token_body.strip_prefix("lens:") {
            if negated {
                parsed.lenses_exclude.push(value.to_string());
            } else {
                parsed.lenses.push(value.to_string());
            }
        } else if let Some(value) = token_body.strip_prefix("description:") {
            if negated {
                parsed.descriptions_exclude.push(value.to_string());
            } else {
                parsed.descriptions.push(value.to_string());
            }
        } else if let Some(value) = token_body.strip_prefix("desc:") {
            // Short alias for description:
            if negated {
                parsed.descriptions_exclude.push(value.to_string());
            } else {
                parsed.descriptions.push(value.to_string());
            }
        } else if let Some(value) = token_body.strip_prefix("iso:") {
            parsed.iso = parse_numeric_filter(value);
        } else if let Some(value) = token_body.strip_prefix("focal:") {
            parsed.focal = parse_numeric_filter(value);
        } else if let Some(value) = token_body.strip_prefix("f:") {
            parsed.aperture = parse_numeric_filter(value);
        } else if let Some(value) = token_body.strip_prefix("width:") {
            parsed.width = parse_numeric_filter(value);
        } else if let Some(value) = token_body.strip_prefix("height:") {
            parsed.height = parse_numeric_filter(value);
        } else if let Some(value) = token_body.strip_prefix("meta:") {
            if let Some((key, val)) = value.split_once('=') {
                parsed.meta_filters.push((key.to_string(), val.to_string()));
            }
        } else if token_body == "orphan:true" {
            parsed.orphan = true;
        } else if token_body == "orphan:false" {
            parsed.orphan_false = true;
        } else if token_body == "missing:true" {
            parsed.missing = true;
        } else if let Some(value) = token_body.strip_prefix("stale:") {
            parsed.stale_days = parse_numeric_filter(value);
        } else if let Some(value) = token_body.strip_prefix("volume:") {
            if value == "none" {
                parsed.volume_none = true;
            } else if negated {
                parsed.volumes_exclude.push(value.to_string());
            } else {
                parsed.volumes.push(value.to_string());
            }
        } else if let Some(value) = token_body.strip_prefix("label:") {
            if value == "none" {
                parsed.color_label_none = true;
            } else if negated {
                parsed.color_labels_exclude.push(value.to_string());
            } else {
                parsed.color_labels.push(value.to_string());
            }
        } else if let Some(value) = token_body.strip_prefix("collection:") {
            if negated {
                parsed.collections_exclude.push(value.to_string());
            } else {
                parsed.collections.push(value.to_string());
            }
        } else if let Some(value) = token_body.strip_prefix("path:") {
            if negated {
                parsed.path_prefixes_exclude.push(value.to_string());
            } else {
                parsed.path_prefixes.push(value.to_string());
            }
        } else if let Some(value) = token_body.strip_prefix("copies:") {
            parsed.copies = parse_numeric_filter(value);
        } else if let Some(value) = token_body.strip_prefix("variants:") {
            parsed.variant_count = parse_numeric_filter(value);
        } else if let Some(value) = token_body.strip_prefix("scattered:") {
            // Support scattered:N+/D syntax where /D is the path depth
            if let Some((num_part, depth_part)) = value.rsplit_once('/') {
                parsed.scattered = parse_numeric_filter(num_part);
                parsed.scattered_depth = depth_part.parse::<u32>().ok();
            } else {
                parsed.scattered = parse_numeric_filter(value);
            }
        } else if let Some(value) = token_body.strip_prefix("date:") {
            parsed.date_prefix = Some(value.to_string());
        } else if let Some(value) = token_body.strip_prefix("dateFrom:") {
            parsed.date_from = Some(value.to_string());
        } else if let Some(value) = token_body.strip_prefix("dateUntil:") {
            parsed.date_until = Some(value.to_string());
        } else if token_body == "stacked:true" {
            parsed.stacked = Some(true);
        } else if token_body == "stacked:false" {
            parsed.stacked = Some(false);
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
        } else if let Some(value) = token_body.strip_prefix("duration:") {
            parsed.duration = parse_numeric_filter(value);
        } else if let Some(value) = token_body.strip_prefix("codec:") {
            parsed.codec = Some(value.to_string());
        } else if let Some(value) = token_body.strip_prefix("faces:") {
            if value == "any" {
                parsed.has_faces = Some(true);
            } else if value == "none" {
                parsed.has_faces = Some(false);
            } else {
                parsed.face_count = parse_numeric_filter(value);
            }
        } else if let Some(value) = token_body.strip_prefix("tagcount:") {
            // Number of intentional (leaf) tags on the asset — the tags
            // the user actually applied, excluding auto-expanded ancestors.
            // `tagcount:0` finds untagged assets; `tagcount:5+` finds
            // heavily-tagged ones. Useful for tag restructuring.
            parsed.tag_count = parse_numeric_filter(value);
        } else if let Some(value) = token_body.strip_prefix("embed:") {
            if value == "any" || value == "true" {
                parsed.has_embed = Some(true);
            } else if value == "none" || value == "false" {
                parsed.has_embed = Some(false);
            }
        } else if let Some(value) = token_body.strip_prefix("person:") {
            if negated {
                parsed.persons_exclude.push(value.to_string());
            } else {
                parsed.persons.push(value.to_string());
            }
        } else if let Some(_value) = token_body.strip_prefix("similar:") {
            #[cfg(feature = "ai")]
            {
                // similar:<asset-id> or similar:<asset-id>:<limit>
                if let Some((id, limit_str)) = _value.rsplit_once(':') {
                    if let Ok(limit) = limit_str.parse::<usize>() {
                        parsed.similar = Some(id.to_string());
                        parsed.similar_limit = Some(limit);
                    } else {
                        // Not a valid limit, treat entire value as asset ID
                        parsed.similar = Some(_value.to_string());
                    }
                } else {
                    parsed.similar = Some(_value.to_string());
                }
            }
        } else if let Some(_value) = token_body.strip_prefix("min_sim:") {
            #[cfg(feature = "ai")]
            {
                if let Ok(v) = _value.parse::<f32>() {
                    parsed.min_sim = Some(v.clamp(0.0, 100.0));
                }
            }
        } else if let Some(_value) = token_body.strip_prefix("text:") {
            #[cfg(feature = "ai")]
            {
                if !_value.is_empty() {
                    // text:"query":limit or text:query:limit or text:"query" or text:query
                    // Check if the value ends with :<number> after the query part
                    if let Some((query_part, limit_str)) = _value.rsplit_once(':') {
                        if let Ok(limit) = limit_str.parse::<usize>() {
                            if !query_part.is_empty() {
                                parsed.text_query = Some(query_part.to_string());
                                parsed.text_query_limit = Some(limit);
                            }
                        } else {
                            parsed.text_query = Some(_value.to_string());
                        }
                    } else {
                        parsed.text_query = Some(_value.to_string());
                    }
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

/// Parse an integer range value: "3200" (exact), "3200+" (min), "100-800" (range).
/// Unified numeric filter supporting exact, minimum, range, and OR values.
///
// ═══ NUMERIC FILTER ═══

/// All numeric search filters (rating, iso, focal, f, width, height, copies,
/// variants, scattered, face_count) use this type for consistent syntax:
/// `x` (exact), `x+` (minimum), `x-y` (range), `x,y` (OR), `x,y+` (combined).
#[derive(Debug, Clone, PartialEq)]
pub enum NumericFilter {
    /// Exactly this value
    Exact(f64),
    /// This value or more
    Min(f64),
    /// Between min and max (inclusive)
    Range(f64, f64),
    /// Any of these exact values
    Values(Vec<f64>),
    /// Any of these exact values OR >= min
    ValuesOrMin { values: Vec<f64>, min: f64 },
}

impl NumericFilter {
    /// Merge another filter (from default_filter) if self is None.
    pub fn or(a: &Option<Self>, b: &Option<Self>) -> Option<Self> {
        a.clone().or_else(|| b.clone())
    }
}

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
        if p.starts_with(&mount) {
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
            let relative = p
                .strip_prefix(&mount)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
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
    result.to_string_lossy().replace('\\', "/")
}

