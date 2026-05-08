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
    /// Parses the query string into structured filters and emits a URL param
    /// for each filter type that the browse page recognises as a dedicated
    /// dropdown / input. This populates the widgets so the user can refine
    /// the saved search by tweaking a dropdown rather than editing raw query
    /// text. Filters without a dedicated widget (cameras, descriptions, geo,
    /// tagcount, has_faces, …) round-trip through `q=` instead.
    ///
    /// Bug history: pre-fix this function emitted only `q`, `type`, `tag`,
    /// `format`, `label`, `rating`, `sort` — `path`, `volume`, `collection`,
    /// and `person` were silently dropped, so a saved search like
    /// `path:Pictures/2026/2026-05/` produced a URL with no `path=` param
    /// and clicking the chip "lost" the filter. Free-text-only `q=` (which
    /// stripped structured tokens that DID have widget mappings) compounded
    /// the problem since niche filters had no fallback channel.
    pub fn to_url_params(&self) -> String {
        let parsed = parse_search_query(&self.query);
        let mut params = Vec::new();

        // q= carries the free-text portion ONLY — structured tokens that
        // have widget mappings are emitted as dedicated URL params below
        // (so the dropdown stays editable without conflicting with q=).
        // The few niche tokens we don't yet round-trip (cameras, lenses,
        // descriptions, iso/focal/aperture/width/height, codec, geo bbox,
        // has_faces, tagcount, scattered, …) are still dropped for now —
        // the most common ones now travel through dedicated params.
        if let Some(ref text) = parsed.text {
            params.push(format!("q={}", urlencoded(text)));
        }

        // Structured filters with dedicated widgets on the browse page.
        // Multi-value URL params (`tag`, `person`) accept comma-separated
        // chip lists per `build_parsed_search` — emit comma-joined so a
        // saved search filtering on "tag:a,b" round-trips correctly.
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

        // Rating: reconstruct the filter string
        if let Some(ref f) = parsed.rating {
            match f {
                crate::query::NumericFilter::Min(v) => params.push(format!("rating={}%2B", *v as u8)),
                crate::query::NumericFilter::Exact(v) => params.push(format!("rating={}", *v as u8)),
                crate::query::NumericFilter::Range(lo, hi) => params.push(format!("rating={}-{}", *lo as u8, *hi as u8)),
                _ => {}
            }
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
