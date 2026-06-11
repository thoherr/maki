//! `Asset` — the core unit of the catalog. An asset has a stable ID,
//! metadata (tags, rating, label, description), and one or more variants
//! (different renditions of the same logical content).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::recipe::Recipe;
use super::variant::Variant;

/// Fixed namespace UUID for deriving content-addressable asset IDs via UUID v5.
/// Generated once; must never change (doing so would break all existing asset IDs).
const DAM_NAMESPACE: Uuid = Uuid::from_bytes([
    0x8a, 0x3b, 0x7e, 0x01, 0x4f, 0xd2, 0x4a, 0x6b, 0x9c, 0x1d, 0xe7, 0x5a, 0x0b, 0xf3, 0x28,
    0x4c,
]);

/// The type of digital asset.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AssetType {
    Image,
    Video,
    Audio,
    Document,
    Other,
}

/// Where a tag value came from — human curation or one of the machine
/// pipelines. Tags absent from [`Asset::tag_sources`] default to
/// [`TagSource::User`] (pre-provenance sidecars carry no map; treating
/// their tags as human curation is the conservative choice — no
/// backfill is possible or attempted).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TagSource {
    /// Added by a human (CLI/web edit, configured import auto_tags).
    User,
    /// Extracted from XMP keywords (sidecar or embedded) at import/reimport.
    XmpImport,
    /// Applied by the SigLIP zero-shot auto-tagger.
    AutoTag,
    /// Applied from a VLM describe run.
    Vlm,
}

impl std::fmt::Display for TagSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            TagSource::User => "user",
            TagSource::XmpImport => "xmp-import",
            TagSource::AutoTag => "auto-tag",
            TagSource::Vlm => "vlm",
        };
        f.write_str(s)
    }
}

impl std::str::FromStr for TagSource {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "user" => Ok(TagSource::User),
            "xmp-import" => Ok(TagSource::XmpImport),
            "auto-tag" => Ok(TagSource::AutoTag),
            "vlm" => Ok(TagSource::Vlm),
            other => Err(format!(
                "unknown tag source '{other}'. Valid sources: user, xmp-import, auto-tag, vlm"
            )),
        }
    }
}

/// The central entity. Represents a logical asset (e.g. "photo of sunset at beach").
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Asset {
    pub id: Uuid,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub created_at: DateTime<Utc>,
    pub asset_type: AssetType,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    /// Tag provenance: tag value → source. A tag absent from the map has
    /// source [`TagSource::User`]. Invariant: keys ⊆ `tags`
    /// ([`MetadataStore::save`](crate::metadata_store::MetadataStore::save)
    /// self-heals stale entries). BTreeMap keeps YAML output
    /// deterministic; sidecars without the field stay byte-identical on
    /// rewrite thanks to `skip_serializing_if`.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub tag_sources: std::collections::BTreeMap<String, TagSource>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rating: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color_label: Option<String>,
    /// Face-detection scan status.
    ///
    /// `None` = never scanned. `Some("done")` = scan completed, regardless of
    /// whether any faces were found. Without this field in the sidecar, a
    /// `rebuild-catalog` would lose the "scanned, no face" knowledge and every
    /// landscape / document / product shot would be re-scanned — potentially
    /// hours of wasted work on a big catalog. Also prevents deleted-face
    /// ghosts from coming back: once a user dismisses a detection, the asset
    /// stays marked as scanned and won't be re-detected on subsequent runs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub face_scan_status: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preview_rotation: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preview_variant: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub variants: Vec<Variant>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub recipes: Vec<Recipe>,
}

impl Asset {
    /// Create a new asset with a deterministic ID derived from the content hash.
    /// Same content hash always produces the same asset ID.
    /// Compute the deterministic asset ID for a given content hash.
    pub fn id_for_hash(content_hash: &str) -> Uuid {
        Uuid::new_v5(&DAM_NAMESPACE, content_hash.as_bytes())
    }

    pub fn new(asset_type: AssetType, content_hash: &str) -> Self {
        Self {
            id: Uuid::new_v5(&DAM_NAMESPACE, content_hash.as_bytes()),
            name: None,
            created_at: Utc::now(),
            asset_type,
            tags: Vec::new(),
            tag_sources: std::collections::BTreeMap::new(),
            description: None,
            rating: None,
            color_label: None,
            face_scan_status: None,
            preview_rotation: None,
            preview_variant: None,
            variants: Vec::new(),
            recipes: Vec::new(),
        }
    }

    // ── Tag mutation API ─────────────────────────────────────────────
    //
    // The sanctioned way to mutate `tags`: these methods keep the
    // `tag_sources` provenance map consistent with the tag list. New
    // write paths should route through them instead of touching
    // `asset.tags` directly.

    /// Append tags not already present (exact match) and record `source`
    /// for the genuinely-new ones. An existing tag keeps its existing
    /// source — re-importing XMP must not demote a user tag. For
    /// `TagSource::User` no map entry is written (absent = user), which
    /// keeps sidecars lean. Returns the number of tags added.
    pub fn add_tags_with_source(&mut self, tags: &[String], source: TagSource) -> usize {
        let mut added = 0;
        for tag in tags {
            if self.tags.contains(tag) {
                continue;
            }
            self.tags.push(tag.clone());
            if source != TagSource::User {
                self.tag_sources.insert(tag.clone(), source);
            }
            added += 1;
        }
        added
    }

    /// Remove exact matches from both the tag list and the provenance
    /// map. Returns the number of list entries removed.
    pub fn remove_tags(&mut self, tags: &[String]) -> usize {
        let before = self.tags.len();
        self.tags.retain(|t| !tags.iter().any(|r| r == t));
        for tag in tags {
            self.tag_sources.remove(tag);
        }
        before - self.tags.len()
    }

    /// Rename a tag value in the provenance map, carrying the source
    /// entry over to the new value (a renamed machine tag stays a
    /// machine tag). Does NOT touch the tag list — callers handle the
    /// list rename themselves (rename flows have their own
    /// case-sensitivity and ancestor-expansion rules).
    pub fn rename_tag_value(&mut self, old: &str, new: &str) {
        if let Some(source) = self.tag_sources.remove(old) {
            self.tag_sources.insert(new.to_string(), source);
        }
    }

    /// Look up the source of a tag; absent from the map = `User`.
    pub fn tag_source(&self, tag: &str) -> TagSource {
        self.tag_sources.get(tag).copied().unwrap_or(TagSource::User)
    }

    /// Drop provenance entries whose tag is no longer in the tag list.
    pub fn prune_tag_sources(&mut self) {
        let tags = &self.tags;
        self.tag_sources.retain(|k, _| tags.contains(k));
    }

    /// True if any provenance entry references a tag not in the list
    /// (cheap pre-check so `MetadataStore::save` only clones when
    /// healing is actually needed).
    pub fn has_stale_tag_sources(&self) -> bool {
        self.tag_sources.keys().any(|k| !self.tags.contains(k))
    }

    /// Validate and canonicalize a color label string.
    ///
    /// Accepts case-insensitive color names from the CaptureOne superset:
    /// Red, Orange, Yellow, Green, Blue, Pink, Purple.
    /// Returns the canonical title-case name, or an error for unknown colors.
    ///
    /// # Examples
    ///
    /// ```
    /// use maki::models::Asset;
    ///
    /// assert_eq!(Asset::validate_color_label("red").unwrap(), Some("Red".to_string()));
    /// assert_eq!(Asset::validate_color_label("BLUE").unwrap(), Some("Blue".to_string()));
    /// assert_eq!(Asset::validate_color_label("").unwrap(), None);
    /// assert!(Asset::validate_color_label("magenta").is_err());
    /// ```
    pub fn validate_color_label(s: &str) -> Result<Option<String>, String> {
        let s = s.trim();
        if s.is_empty() {
            return Ok(None);
        }
        match s.to_lowercase().as_str() {
            "red" => Ok(Some("Red".to_string())),
            "orange" => Ok(Some("Orange".to_string())),
            "yellow" => Ok(Some("Yellow".to_string())),
            "green" => Ok(Some("Green".to_string())),
            "blue" => Ok(Some("Blue".to_string())),
            "pink" => Ok(Some("Pink".to_string())),
            "purple" => Ok(Some("Purple".to_string())),
            _ => Err(format!(
                "unknown color label '{s}'. Valid colors: Red, Orange, Yellow, Green, Blue, Pink, Purple"
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn asset() -> Asset {
        Asset::new(AssetType::Image, "sha256:tagsource-test")
    }

    #[test]
    fn add_tags_with_source_records_machine_sources_only() {
        let mut a = asset();
        let added = a.add_tags_with_source(
            &["sunset".to_string(), "beach".to_string()],
            TagSource::AutoTag,
        );
        assert_eq!(added, 2);
        assert_eq!(a.tag_source("sunset"), TagSource::AutoTag);

        // User-source adds stay out of the map (absent = user).
        let added = a.add_tags_with_source(&["manual".to_string()], TagSource::User);
        assert_eq!(added, 1);
        assert!(!a.tag_sources.contains_key("manual"));
        assert_eq!(a.tag_source("manual"), TagSource::User);
    }

    #[test]
    fn add_tags_with_source_does_not_overwrite_existing_source() {
        let mut a = asset();
        a.add_tags_with_source(&["concert".to_string()], TagSource::User);
        // Re-importing the same tag from XMP must not demote it.
        let added = a.add_tags_with_source(&["concert".to_string()], TagSource::XmpImport);
        assert_eq!(added, 0);
        assert_eq!(a.tag_source("concert"), TagSource::User);

        a.add_tags_with_source(&["machine".to_string()], TagSource::Vlm);
        a.add_tags_with_source(&["machine".to_string()], TagSource::AutoTag);
        assert_eq!(a.tag_source("machine"), TagSource::Vlm);
    }

    #[test]
    fn remove_tags_drops_list_and_map_entries() {
        let mut a = asset();
        a.add_tags_with_source(&["x".to_string(), "y".to_string()], TagSource::Vlm);
        let removed = a.remove_tags(&["x".to_string(), "missing".to_string()]);
        assert_eq!(removed, 1);
        assert_eq!(a.tags, vec!["y".to_string()]);
        assert!(!a.tag_sources.contains_key("x"));
        assert!(a.tag_sources.contains_key("y"));
    }

    #[test]
    fn rename_tag_value_carries_source() {
        let mut a = asset();
        a.add_tags_with_source(&["old".to_string()], TagSource::AutoTag);
        a.rename_tag_value("old", "new");
        assert_eq!(a.tag_source("new"), TagSource::AutoTag);
        assert!(!a.tag_sources.contains_key("old"));
        // Renaming a user tag is a no-op on the map.
        a.rename_tag_value("absent", "still-absent");
        assert!(!a.tag_sources.contains_key("still-absent"));
    }

    #[test]
    fn prune_tag_sources_drops_stale_entries() {
        let mut a = asset();
        a.add_tags_with_source(&["keep".to_string(), "gone".to_string()], TagSource::Vlm);
        // Simulate a bypassing mutation site.
        a.tags.retain(|t| t != "gone");
        assert!(a.has_stale_tag_sources());
        a.prune_tag_sources();
        assert!(!a.has_stale_tag_sources());
        assert!(a.tag_sources.contains_key("keep"));
        assert!(!a.tag_sources.contains_key("gone"));
    }

    #[test]
    fn tag_source_display_from_str_round_trip() {
        for s in [TagSource::User, TagSource::XmpImport, TagSource::AutoTag, TagSource::Vlm] {
            let parsed: TagSource = s.to_string().parse().unwrap();
            assert_eq!(parsed, s);
        }
        assert!("nonsense".parse::<TagSource>().is_err());
    }
}
