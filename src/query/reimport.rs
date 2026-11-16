//! Whole-asset resyncs of `QueryEngine`: `reimport_metadata` /
//! `reimport_exif_only` (re-extract from the files on disk) and
//! `restore_asset` (undo). Both rebuild every derived row set through
//! [`Catalog::resync_asset_from_sidecar`].

use std::collections::HashMap;

use anyhow::Result;

use crate::device_registry::DeviceRegistry;

use super::QueryEngine;

impl QueryEngine {
    // ═══ METADATA REIMPORT ═══

    /// Clear asset-level metadata and re-extract from variant source files (XMP recipes + embedded XMP).
    /// Returns the updated tags list.
    pub fn reimport_metadata(&self, asset_id_prefix: &str) -> Result<Vec<String>> {
        self.reimport_metadata_inner(asset_id_prefix, false)
    }

    /// Reimport only EXIF/source_metadata from media files, leaving tags,
    /// description, rating, and label untouched.
    pub fn reimport_exif_only(&self, asset_id_prefix: &str) -> Result<Vec<String>> {
        self.reimport_metadata_inner(asset_id_prefix, true)
    }

    pub(super) fn reimport_metadata_inner(&self, asset_id_prefix: &str, exif_only: bool) -> Result<Vec<String>> {
        let catalog = self.catalog()?;
        let store = &self.meta_store;
        let registry = DeviceRegistry::new(&self.catalog_root);

        let full_id = catalog.resolve_asset_id_required(asset_id_prefix)?;

        let uuid: uuid::Uuid = full_id.parse()?;
        let mut asset = store.load(uuid)?;

        if !exif_only {
            // Clear asset-level metadata that comes from XMP sources
            asset.tags.clear();
            asset.tag_sources.clear();
            asset.description = None;
            asset.rating = None;
            asset.color_label = None;
        }

        // Build volume lookup (id string -> Volume)
        let volumes = registry.list().unwrap_or_default();
        let vol_map: HashMap<String, &crate::models::volume::Volume> =
            volumes.iter().map(|v| (v.id.to_string(), v)).collect();

        // Re-extract from XMP recipe files (skip in exif_only mode)
        let recipes = if exif_only { vec![] } else { catalog.list_recipes_for_asset(&full_id)? };
        for (_recipe_id, _content_hash, variant_hash, relative_path, volume_id) in &recipes {
            let vol = match vol_map.get(volume_id) {
                Some(v) if v.is_online => *v,
                _ => continue,
            };
            let ext = std::path::Path::new(relative_path)
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("");
            if !ext.eq_ignore_ascii_case("xmp") {
                continue;
            }
            let full_path = vol.mount_point.join(relative_path);
            if full_path.exists() {
                let xmp = crate::xmp_reader::extract(&full_path);
                xmp.apply_to(&mut asset, variant_hash);
            }
        }

        // Re-extract from embedded XMP in JPEG/TIFF media files (skip in exif_only mode).
        // Sidecar-wins gate: when the asset has an XMP recipe, that
        // sidecar is authoritative — embedded XMP in JPEG/TIFF variants
        // is frozen at import time (writeback never patches it) and
        // reading it would re-inject stale flat keywords. The same gate
        // sits in `asset_service::refresh` for `refresh --media` /
        // `sync-metadata --media`; see `Catalog::asset_has_recipe` for
        // the policy docstring.
        let locations = catalog.list_file_locations_for_asset(&full_id)?;
        let sidecar_authoritative = if !exif_only {
            catalog.asset_has_recipe(&full_id)?
        } else {
            false
        };
        if !exif_only && !sidecar_authoritative {
        for (content_hash, relative_path, volume_id) in &locations {
            let vol = match vol_map.get(volume_id) {
                Some(v) if v.is_online => *v,
                _ => continue,
            };
            let ext = std::path::Path::new(relative_path)
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("");
            if !matches!(ext.to_lowercase().as_str(), "jpg" | "jpeg" | "tif" | "tiff") {
                continue;
            }
            let full_path = vol.mount_point.join(relative_path);
            if full_path.exists() {
                let embedded_xmp = crate::embedded_xmp::extract_embedded_xmp(&full_path);
                if !embedded_xmp.keywords.is_empty()
                    || embedded_xmp.description.is_some()
                    || !embedded_xmp.source_metadata.is_empty()
                {
                    embedded_xmp.apply_to(&mut asset, content_hash);
                }
            }
        }
        } // end if !exif_only && !sidecar_authoritative

        // Re-extract EXIF from media files to refresh source_metadata and date
        let mut earliest_date: Option<chrono::DateTime<chrono::Utc>> = None;
        for (content_hash, relative_path, volume_id) in &locations {
            let vol = match vol_map.get(volume_id) {
                Some(v) if v.is_online => *v,
                _ => continue,
            };
            let full_path = vol.mount_point.join(relative_path);
            if !full_path.exists() {
                continue;
            }
            let exif = crate::exif_reader::extract(&full_path);
            if let Some(variant) = asset.variants.iter_mut().find(|v| v.content_hash == *content_hash) {
                // Update source_metadata from EXIF (overwrite with fresh data)
                for (key, val) in &exif.source_metadata {
                    variant.source_metadata.insert(key.to_string(), val.to_string());
                }
            }
            // Track earliest date for created_at
            if let Some(date_str) = exif.source_metadata.get("date_taken") {
                if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(date_str) {
                    let utc = dt.with_timezone(&chrono::Utc);
                    if earliest_date.is_none() || utc < earliest_date.unwrap() {
                        earliest_date = Some(utc);
                    }
                }
            }
        }
        // Update created_at to earliest date from EXIF
        if let Some(dt) = earliest_date {
            asset.created_at = dt;
        }

        // Deduplicate recipes by location (volume_id + relative_path).
        // Can happen when auto-group merges two assets that both had the same XMP recipe.
        {
            let mut seen = std::collections::HashSet::new();
            asset.recipes.retain(|r| {
                let key = format!("{}:{}", r.location.volume_id, r.location.relative_path_str());
                seen.insert(key)
            });
        }

        // Deduplicate variant locations
        for variant in &mut asset.variants {
            let mut seen = std::collections::HashSet::new();
            variant.locations.retain(|l| {
                let key = format!("{}:{}", l.volume_id, l.relative_path_str());
                seen.insert(key)
            });
        }

        // Sidecar first (source of truth), then every derived SQLite row
        // in one transaction — stale variants / locations / recipes go,
        // the sidecar's come back, denormalized columns are recomputed.
        store.save(&asset)?;
        catalog.resync_asset_from_sidecar(&asset)?;

        Ok(asset.tags.clone())
    }

    /// Restore an asset to a prior full state (undo). Writes the YAML
    /// sidecar, then resyncs every derived catalog row from it (asset,
    /// variants, file locations, recipes, denormalized columns — via
    /// [`Catalog::resync_asset_from_sidecar`], one transaction), and flags
    /// every `.xmp` recipe `pending_writeback` so a later `maki writeback`
    /// reconciles the files on disk. Does NOT touch `.xmp` files live —
    /// undo restores catalog+sidecar state only (the documented default;
    /// most catalogs run with auto-writeback off).
    ///
    /// Keeps SQLite recipe pending flags equal to the YAML it writes, so
    /// `maki doctor` stays clean after an undo.
    pub fn restore_asset(&self, asset: &crate::models::Asset) -> Result<()> {
        let catalog = self.catalog()?;
        let store = &self.meta_store;

        // Flag every .xmp recipe pending in the in-memory copy first, so
        // the sidecar we save and the catalog rows we insert agree.
        let mut asset = asset.clone();
        for recipe in &mut asset.recipes {
            let is_xmp = recipe
                .location
                .relative_path
                .extension()
                .and_then(|e| e.to_str())
                .map(|e| e.eq_ignore_ascii_case("xmp"))
                .unwrap_or(false);
            if is_xmp {
                recipe.pending_writeback = true;
            }
        }

        store.save(&asset)?;
        catalog.resync_asset_from_sidecar(&asset)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::Catalog;
    use crate::metadata_store::MetadataStore;
    use crate::models::{Asset, AssetType, Variant, VariantRole, Volume};
    use crate::query::test_support::*;

    /// Undo must bring variants and file locations back too, not only the
    /// asset row and recipes (the pre-F7 `restore_asset` left the rows of a
    /// variant added after the recorded state in place).
    #[test]
    fn restore_asset_resyncs_variants_and_locations() {
        use crate::models::volume::FileLocation;

        let dir = tempfile::tempdir().unwrap();
        let catalog = Catalog::open(dir.path()).unwrap();
        catalog.initialize().unwrap();
        let store = MetadataStore::new(dir.path());
        let volume = Volume::new(
            "Vol".to_string(),
            std::path::PathBuf::from("/tmp/vol"),
            crate::models::VolumeType::Local,
        );
        catalog.ensure_volume(&volume).unwrap();
        let loc = |path: &str| FileLocation {
            volume_id: volume.id,
            relative_path: std::path::PathBuf::from(path),
            verified_at: None,
        };
        let variant = |hash: &str, asset_id: uuid::Uuid, name: &str| Variant {
            content_hash: hash.to_string(),
            asset_id,
            role: VariantRole::Original,
            format: "jpg".to_string(),
            file_size: 10,
            original_filename: name.to_string(),
            source_metadata: Default::default(),
            locations: vec![loc(name)],
        };

        let mut asset = Asset::new(AssetType::Image, "sha256:undo1");
        asset.variants.push(variant("sha256:undo1", asset.id, "one.jpg"));
        asset.rating = Some(2);
        store.save(&asset).unwrap();
        catalog.resync_asset_from_sidecar(&asset).unwrap();
        let before = asset.clone();

        // Later state: a second variant with its own location, and a rating.
        let mut v2 = variant("sha256:undo2", asset.id, "two.jpg");
        v2.role = VariantRole::Alternate;
        asset.variants.push(v2);
        asset.rating = Some(5);
        store.save(&asset).unwrap();
        catalog.resync_asset_from_sidecar(&asset).unwrap();
        let id = asset.id.to_string();
        assert_eq!(count(&catalog, "SELECT COUNT(*) FROM variants WHERE asset_id = ?1", &id), 2);

        QueryEngine::new(dir.path()).restore_asset(&before).unwrap();

        assert_eq!(count(&catalog, "SELECT COUNT(*) FROM variants WHERE asset_id = ?1", &id), 1);
        assert_eq!(
            count(
                &catalog,
                "SELECT COUNT(*) FROM file_locations WHERE content_hash IN (SELECT content_hash FROM variants WHERE asset_id = ?1)",
                &id
            ),
            1
        );
        let (rating, variant_count): (Option<i64>, i64) = catalog
            .conn()
            .query_row(
                "SELECT rating, variant_count FROM assets WHERE id = ?1",
                rusqlite::params![id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(rating, Some(2));
        assert_eq!(variant_count, 1);
        assert_eq!(store.load(before.id).unwrap().variants.len(), 1);
    }

}
