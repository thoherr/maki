//! Saved-search store — persists named search queries to a YAML file in
//! the catalog root so users can re-run them with `maki saved-search run NAME`.

use std::path::Path;

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::query::parse_search_query;

/// A saved search (smart album) — a named query that can be re-executed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SavedSearch {
    pub name: String,
    /// Search query in the same format as `maki search` (e.g. "type:image tag:landscape rating:4+")
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub query: String,
    /// Sort order (e.g. "date_desc", "name_asc"). Omitted = default (date_desc).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sort: Option<String>,
    /// Whether this search appears as a chip on the browse page.
    #[serde(default, skip_serializing_if = "is_false")]
    pub favorite: bool,
}

fn is_false(v: &bool) -> bool {
    !v
}

impl SavedSearch {
    /// Convert the stored query into browse-page URL parameters.
    ///
    /// Strategy: tokenize the raw query, separate tokens into two streams,
    /// emit each appropriately:
    ///
    ///   1. **URL-param-backed tokens** — positive tokens of types that
    ///      have a dedicated dropdown / input on the browse page (`type`,
    ///      `tag`, `format`, `label`, `volume`, `collection`, `person`,
    ///      `path`, `rating`). Emitted as their dedicated URL param so the
    ///      widget reflects the filter and the user can refine via the UI.
    ///   2. **Remainder tokens** — everything else: free text, exclude
    ///      variants (`-tag:rejected`), niche structured filters
    ///      (`camera:`, `iso:`, `tagcount:`, `geo:`, `has_faces:`, …),
    ///      multi-value tokens that overflow the single-value URL params
    ///      (`format:jpg format:tiff` keeps the second in q=). Joined back
    ///      with spaces and stuffed into `q=`. Tokens whose value contains
    ///      whitespace are re-quoted so the round-trip is loss-free.
    ///
    /// This way EVERY filter — including ones we haven't enumerated —
    /// survives the chip-click round-trip. Adding a new filter type
    /// requires no change here unless we also add a dedicated widget for it.
    ///
    /// Bug history (pre-rewrite): only `q=text-only`, `type`, `tag`,
    /// `format`, `label`, `rating`, `sort` were emitted. `path`, `volume`,
    /// `collection`, `person` (issue #user-reported) plus every niche
    /// filter was silently dropped. Fixed iteratively in two passes —
    /// first added the four dedicated params, now token-level remainder
    /// for the long tail.
    pub fn to_url_params(&self) -> String {
        let parsed = parse_search_query(&self.query);
        let mut params = Vec::new();

        // ── 1. Dedicated URL params for widget-backed filters ──────────
        // Drives the browse-page dropdowns; tokens covered here are
        // skipped from the q= remainder below to avoid double-application.
        if let Some(t) = parsed.asset_types.first() {
            params.push(format!("type={}", urlencoded(t)));
        }
        if !parsed.tags.is_empty() {
            params.push(format!("tag={}", urlencoded(&parsed.tags.join(","))));
        }
        if let Some(f) = parsed.formats.first() {
            params.push(format!("format={}", urlencoded(f)));
        }
        if let Some(l) = parsed.color_labels.first() {
            params.push(format!("label={}", urlencoded(l)));
        } else if parsed.color_label_none {
            // `label:none` round-trips via the special "none" sentinel
            // value — `build_parsed_search` flips parsed.color_label_none
            // when it sees label=none.
            params.push("label=none".to_string());
        }
        if let Some(v) = parsed.volumes.first() {
            params.push(format!("volume={}", urlencoded(v)));
        }
        if let Some(c) = parsed.collections.first() {
            params.push(format!("collection={}", urlencoded(c)));
        }
        if !parsed.persons.is_empty() {
            params.push(format!("person={}", urlencoded(&parsed.persons.join(","))));
        }
        if let Some(p) = parsed.path_prefixes.first() {
            params.push(format!("path={}", urlencoded(p)));
        }
        if let Some(ref f) = parsed.rating {
            match f {
                crate::query::NumericFilter::Min(v) => params.push(format!("rating={}%2B", *v as u8)),
                crate::query::NumericFilter::Exact(v) => params.push(format!("rating={}", *v as u8)),
                crate::query::NumericFilter::Range(lo, hi) => params.push(format!("rating={}-{}", *lo as u8, *hi as u8)),
                _ => {}
            }
        }

        // ── 2. Remainder query into q= ─────────────────────────────────
        // Walk every original token; drop the ones we just covered above.
        // Everything else (free text, excludes, niche filters, additional
        // values for single-value URL params) goes into q= so the browse
        // search engine still applies them.
        let remainder = remainder_query(&self.query);
        if !remainder.is_empty() {
            params.push(format!("q={}", urlencoded(&remainder)));
        }

        // Sort
        let sort = self.sort.as_deref().unwrap_or("date_desc");
        params.push(format!("sort={}", urlencoded(sort)));

        params.join("&")
    }
}

/// File structure for searches.toml.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SavedSearchFile {
    #[serde(default, rename = "search")]
    pub searches: Vec<SavedSearch>,
}

const FILENAME: &str = "searches.toml";

/// Load saved searches from the catalog root. Returns empty list if file doesn't exist.
pub fn load(catalog_root: &Path) -> Result<SavedSearchFile> {
    let path = catalog_root.join(FILENAME);
    if path.exists() {
        let contents = std::fs::read_to_string(&path)?;
        let file: SavedSearchFile = toml::from_str(&contents)?;
        Ok(file)
    } else {
        Ok(SavedSearchFile::default())
    }
}

/// Save saved searches to the catalog root. Creates the file if it doesn't exist.
pub fn save(catalog_root: &Path, file: &SavedSearchFile) -> Result<()> {
    let path = catalog_root.join(FILENAME);
    let contents = toml::to_string_pretty(file)?;
    std::fs::write(path, contents)?;
    Ok(())
}

/// Find a saved search by name (case-sensitive).
pub fn find_by_name<'a>(file: &'a SavedSearchFile, name: &str) -> Option<&'a SavedSearch> {
    file.searches.iter().find(|s| s.name == name)
}

/// Filter prefixes that are emitted as dedicated URL params and therefore
/// don't need to ride along in `q=`. Excluded variants (`-tag:foo`,
/// `-volume:Photos`, …) intentionally aren't here — there's no URL param
/// for negation, so those tokens stay in the remainder.
const WIDGET_PREFIXES: &[&str] = &[
    "type:", "tag:", "format:", "label:",
    "volume:", "collection:", "person:", "path:", "rating:",
];

/// Build the remainder query: the original raw query minus the tokens we
/// already emit as dedicated URL params. Tokens whose value has whitespace
/// are re-quoted so the result tokenizes back to the same shape.
///
/// Multi-value handling: `tag` and `person` are emitted as comma-joined
/// URL params (consumed via `value.split(',')` server-side), so EVERY
/// `tag:` / `person:` token can be dropped from the remainder. Single-value
/// URL params (`type`, `format`, `volume`, `collection`, `path`, `rating`,
/// `label`) only consume the FIRST occurrence — additional occurrences
/// stay in the remainder so the catalog ANDs them in.
fn remainder_query(raw: &str) -> String {
    use crate::query::tokenize_query;

    // Track how many of each single-value-URL-param prefix we've consumed.
    // The first occurrence is dropped (handled by the URL param); additional
    // ones survive in q=.
    let multi_value: &[&str] = &["tag:", "person:"];
    let mut consumed_single: std::collections::HashMap<&'static str, bool> = std::collections::HashMap::new();
    for &p in WIDGET_PREFIXES {
        if !multi_value.contains(&p) {
            consumed_single.insert(p, false);
        }
    }

    let mut kept: Vec<String> = Vec::new();
    for tok in tokenize_query(raw) {
        // Determine if this is a positive (un-negated) widget-backed token.
        // Negations stay in the remainder regardless.
        let is_positive_widget_backed = !tok.starts_with('-')
            && WIDGET_PREFIXES.iter().any(|p| tok.starts_with(p));

        if is_positive_widget_backed {
            // Find which prefix matched.
            let prefix = WIDGET_PREFIXES.iter()
                .find(|p| tok.starts_with(*p))
                .copied()
                .unwrap();
            if multi_value.contains(&prefix) {
                // tag / person — every occurrence handled by the URL param.
                continue;
            }
            // Single-value URL params absorb only the first occurrence.
            let consumed = consumed_single.get_mut(prefix).unwrap();
            if !*consumed {
                *consumed = true;
                continue;
            }
            // Second occurrence — fall through to keep it.
        }
        kept.push(requote_token(&tok));
    }
    kept.join(" ")
}

/// Re-add quotes around a token's value if it contains whitespace, so the
/// tokenizer reconstructs the same single-token boundary on the next pass.
/// Without quoting, `tag:Fools Theater` would re-tokenize as
/// `["tag:Fools", "Theater"]` — losing the structured form and turning
/// "Theater" into a free-text term.
fn requote_token(tok: &str) -> String {
    if !tok.chars().any(char::is_whitespace) {
        return tok.to_string();
    }
    if let Some(colon) = tok.find(':') {
        let (prefix, rest) = tok.split_at(colon);
        // rest starts with ':' — skip it before the value.
        let value = &rest[1..];
        format!("{prefix}:\"{value}\"")
    } else {
        format!("\"{tok}\"")
    }
}

/// Minimal percent-encoding for URL parameter values.
fn urlencoded(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            ' ' => out.push_str("%20"),
            '&' => out.push_str("%26"),
            '=' => out.push_str("%3D"),
            '+' => out.push_str("%2B"),
            '#' => out.push_str("%23"),
            '%' => out.push_str("%25"),
            _ => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_toml() {
        let file = SavedSearchFile {
            searches: vec![
                SavedSearch {
                    name: "Landscapes".to_string(),
                    query: "type:image tag:landscape rating:4+".to_string(),
                    sort: Some("name_asc".to_string()),
                    favorite: false,
                },
                SavedSearch {
                    name: "Unrated".to_string(),
                    query: "rating:0".to_string(),
                    sort: None,
                    favorite: false,
                },
            ],
        };

        let toml_str = toml::to_string_pretty(&file).unwrap();
        let parsed: SavedSearchFile = toml::from_str(&toml_str).unwrap();
        assert_eq!(file, parsed);
    }

    #[test]
    fn parse_empty_file() {
        let file: SavedSearchFile = toml::from_str("").unwrap();
        assert!(file.searches.is_empty());
    }

    #[test]
    fn to_url_params_basic() {
        let ss = SavedSearch {
            name: "Test".to_string(),
            query: "type:image tag:landscape rating:4+".to_string(),
            sort: Some("name_asc".to_string()),
            favorite: false,
        };
        let params = ss.to_url_params();
        assert!(params.contains("type=image"));
        assert!(params.contains("tag=landscape"));
        assert!(params.contains("rating=4%2B"));
        assert!(params.contains("sort=name_asc"));
    }

    #[test]
    fn to_url_params_with_text() {
        let ss = SavedSearch {
            name: "Test".to_string(),
            query: "sunset beach type:image".to_string(),
            sort: None,
            favorite: false,
        };
        let params = ss.to_url_params();
        assert!(params.contains("q=sunset%20beach"));
        assert!(params.contains("type=image"));
        assert!(params.contains("sort=date_desc"));
    }

    /// Regression: `path:` was silently dropped from the URL, so a saved
    /// search like `path:Pictures/Masters/2026/2026-05/` clicked from a
    /// browse-page chip reloaded the page with NO path filter applied.
    #[test]
    fn to_url_params_preserves_path() {
        let ss = SavedSearch {
            name: "May 2026".to_string(),
            query: "path:Pictures/Masters/2026/2026-05/".to_string(),
            sort: None,
            favorite: false,
        };
        let params = ss.to_url_params();
        // Path round-trips with its trailing slash intact. The minimal
        // `urlencoded` helper only escapes characters that conflict with
        // query syntax (`&`, `=`, `+`, ` `); forward slashes are URL-safe
        // in query values and pass through unchanged.
        assert!(
            params.contains("path=Pictures/Masters/2026/2026-05/"),
            "expected path= in URL params, got: {params}"
        );
    }

    #[test]
    fn to_url_params_preserves_volume_collection_person() {
        let ss = SavedSearch {
            name: "Test".to_string(),
            query: "volume:Photos collection:Wedding person:Alice".to_string(),
            sort: None,
            favorite: false,
        };
        let params = ss.to_url_params();
        assert!(params.contains("volume=Photos"), "missing volume: {params}");
        assert!(params.contains("collection=Wedding"), "missing collection: {params}");
        assert!(params.contains("person=Alice"), "missing person: {params}");
    }

    /// Niche filters without dedicated widgets (camera, iso, has_faces,
    /// tagcount, geo, etc.) round-trip through the q= remainder.
    #[test]
    fn to_url_params_preserves_niche_filters() {
        let ss = SavedSearch {
            name: "Test".to_string(),
            query: "camera:Sony iso:3200 tagcount:0 has_faces:true".to_string(),
            sort: None,
            favorite: false,
        };
        let params = ss.to_url_params();
        // None of these have dedicated URL params, so they should survive
        // in q=. URL-encoded form: spaces → %20, colons stay as-is.
        assert!(params.contains("q=camera:Sony%20iso:3200%20tagcount:0%20has_faces:true"),
            "niche filters dropped: {params}");
    }

    /// Negated tokens (`-tag:foo`, `-camera:Sony`) have no URL-param
    /// counterpart, so they stay in q= even when the positive variant has
    /// one (e.g. `-tag:` is preserved while `tag:` is consumed by tag=).
    #[test]
    fn to_url_params_preserves_negations() {
        let ss = SavedSearch {
            name: "Test".to_string(),
            query: "tag:wedding -tag:rejected -camera:Phone -volume:Backups".to_string(),
            sort: None,
            favorite: false,
        };
        let params = ss.to_url_params();
        assert!(params.contains("tag=wedding"), "positive tag should be in URL param: {params}");
        // Negations stay in q=. Use percent-encoded form (`%20` = space, %2D = `-`...
        // actually `-` is URL-safe and passes through).
        assert!(params.contains("-tag:rejected"), "negative tag dropped: {params}");
        assert!(params.contains("-camera:Phone"), "negative camera dropped: {params}");
        assert!(params.contains("-volume:Backups"), "negative volume dropped: {params}");
    }

    /// Quoted values with whitespace round-trip with quotes re-applied so
    /// the tokenizer reproduces the same token boundaries on the next pass.
    #[test]
    fn to_url_params_requotes_whitespace_values() {
        let ss = SavedSearch {
            name: "Test".to_string(),
            query: r#"camera:"Canon EOS R5" -lens:"24-70 f/4""#.to_string(),
            sort: None,
            favorite: false,
        };
        let params = ss.to_url_params();
        // Camera and lens both have no URL param; both stay in q= but are
        // re-quoted so the value's spaces don't break tokenization.
        // urlencoded turns spaces into %20 and quotes pass through.
        assert!(
            params.contains(r#"camera:"Canon%20EOS%20R5""#),
            "quoted camera value lost: {params}"
        );
        assert!(
            params.contains(r#"-lens:"24-70%20f/4""#),
            "quoted negated lens value lost: {params}"
        );
    }

    /// Multi-value single-URL-param overflow: `format:` only has one URL
    /// slot, so a saved search with two formats keeps the second in q=.
    #[test]
    fn to_url_params_overflow_to_q_for_single_value_params() {
        let ss = SavedSearch {
            name: "Test".to_string(),
            query: "format:jpg format:tiff".to_string(),
            sort: None,
            favorite: false,
        };
        let params = ss.to_url_params();
        assert!(params.contains("format=jpg"), "first format missing: {params}");
        assert!(params.contains("q=format:tiff"), "second format dropped: {params}");
    }

    /// `label:none` round-trips via the special `label=none` sentinel that
    /// `build_parsed_search` recognises and converts to color_label_none.
    #[test]
    fn to_url_params_preserves_label_none() {
        let ss = SavedSearch {
            name: "Test".to_string(),
            query: "label:none rating:4+".to_string(),
            sort: None,
            favorite: false,
        };
        let params = ss.to_url_params();
        assert!(params.contains("label=none"), "label:none lost: {params}");
        assert!(params.contains("rating=4%2B"), "rating: {params}");
    }

    #[test]
    fn to_url_params_multi_value_tag_and_person() {
        // The browse page's `tag` and `person` URL params accept
        // comma-separated lists; multi-value filters in the saved query
        // should round-trip as joined strings rather than first-value-only.
        let ss = SavedSearch {
            name: "Test".to_string(),
            query: "tag:wedding tag:landscape person:Alice person:Bob".to_string(),
            sort: None,
            favorite: false,
        };
        let params = ss.to_url_params();
        // Commas pass through `urlencoded` unchanged (URL-safe in query
        // values). The browse page splits the value on `,` server-side.
        assert!(params.contains("tag=wedding,landscape"), "tag list: {params}");
        assert!(params.contains("person=Alice,Bob"), "person list: {params}");
    }

    #[test]
    fn load_missing_file() {
        let dir = tempfile::tempdir().unwrap();
        let file = load(dir.path()).unwrap();
        assert!(file.searches.is_empty());
    }

    #[test]
    fn save_and_load() {
        let dir = tempfile::tempdir().unwrap();
        let file = SavedSearchFile {
            searches: vec![SavedSearch {
                name: "Test".to_string(),
                query: "type:image".to_string(),
                sort: None,
                favorite: false,
            }],
        };
        save(dir.path(), &file).unwrap();
        let loaded = load(dir.path()).unwrap();
        assert_eq!(loaded.searches.len(), 1);
        assert_eq!(loaded.searches[0].name, "Test");
    }

    #[test]
    fn find_by_name_found() {
        let file = SavedSearchFile {
            searches: vec![
                SavedSearch {
                    name: "A".to_string(),
                    query: "".to_string(),
                    sort: None,
                    favorite: false,
                },
                SavedSearch {
                    name: "B".to_string(),
                    query: "type:video".to_string(),
                    sort: None,
                    favorite: false,
                },
            ],
        };
        assert_eq!(find_by_name(&file, "B").unwrap().query, "type:video");
        assert!(find_by_name(&file, "C").is_none());
    }

    #[test]
    fn favorite_default_false() {
        let toml_str = r#"
[[search]]
name = "Legacy"
query = "type:image"
"#;
        let file: SavedSearchFile = toml::from_str(toml_str).unwrap();
        assert_eq!(file.searches.len(), 1);
        assert!(!file.searches[0].favorite);
    }

    #[test]
    fn favorite_roundtrip() {
        let file = SavedSearchFile {
            searches: vec![
                SavedSearch {
                    name: "Fav".to_string(),
                    query: "type:image".to_string(),
                    sort: None,
                    favorite: true,
                },
                SavedSearch {
                    name: "NotFav".to_string(),
                    query: "type:video".to_string(),
                    sort: None,
                    favorite: false,
                },
            ],
        };
        let toml_str = toml::to_string_pretty(&file).unwrap();
        // favorite = true is serialized, favorite = false is skipped
        assert!(toml_str.contains("favorite = true"));
        assert!(!toml_str.contains("favorite = false"));
        let parsed: SavedSearchFile = toml::from_str(&toml_str).unwrap();
        assert_eq!(file, parsed);
    }
}
