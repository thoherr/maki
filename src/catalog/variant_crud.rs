//! `variant_crud` section of `Catalog` — methods extracted from the original
//! 9.2-kLOC catalog.rs into a multi-file `impl Catalog` block.
//!
//! Types, helper functions, and the connection ctor live in the parent
//! `catalog` module.

use super::*;

impl Catalog {
    // ═══ VARIANT & LOCATION CRUD ═══

    /// Insert a variant into the catalog.
    pub fn insert_variant(&self, variant: &Variant) -> Result<()> {
        let meta = &variant.source_metadata;
        let meta_json = serde_json::to_string(meta)?;

        let camera_model = meta.get("camera_model").cloned();
        let lens_model = meta.get("lens_model").cloned();
        let focal_length_mm: Option<f64> = meta
            .get("focal_length")
            .and_then(|v| v.trim_end_matches(" mm").parse().ok());
        let f_number: Option<f64> = meta.get("f_number").and_then(|v| v.parse().ok());
        let iso: Option<i64> = meta.get("iso").and_then(|v| v.parse().ok());
        let image_width: Option<i64> = meta.get("image_width").and_then(|v| v.parse().ok());
        let image_height: Option<i64> = meta.get("image_height").and_then(|v| v.parse().ok());

        self.conn.execute(
            "INSERT OR REPLACE INTO variants (content_hash, asset_id, role, format, file_size, original_filename, source_metadata, \
             camera_model, lens_model, focal_length_mm, f_number, iso, image_width, image_height) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
            rusqlite::params![
                variant.content_hash,
                variant.asset_id.to_string(),
                variant.role.as_str(),
                variant.format,
                variant.file_size,
                variant.original_filename,
                meta_json,
                camera_model,
                lens_model,
                focal_length_mm,
                f_number,
                iso,
                image_width,
                image_height,
            ],
        )?;
        Ok(())
    }

    /// Insert a file location for a variant.
    pub fn insert_file_location(&self, content_hash: &str, loc: &FileLocation) -> Result<()> {
        // Check if this exact location already exists (no unique constraint on table)
        let exists: bool = self.conn.query_row(
            "SELECT COUNT(*) > 0 FROM file_locations WHERE content_hash = ?1 AND volume_id = ?2 AND relative_path = ?3",
            rusqlite::params![content_hash, loc.volume_id.to_string(), loc.relative_path_str()],
            |r| r.get(0),
        )?;
        if exists {
            return Ok(());
        }
        self.conn.execute(
            "INSERT INTO file_locations (content_hash, volume_id, relative_path, verified_at) \
             VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![
                content_hash,
                loc.volume_id.to_string(),
                loc.relative_path_str(),
                loc.verified_at.map(|t| t.to_rfc3339()),
            ],
        )?;
        Ok(())
    }

    /// Migrate a variant's content-hash identity after its file bytes
    /// changed on disk. Embedded XMP write-back (`maki writeback
    /// --embed`) rewrites the JPEG itself, and `content_hash` IS the
    /// variant's identity — so every referencing row must move to the
    /// new hash in one transaction:
    ///
    /// - `variants.content_hash` (the PK)
    /// - `file_locations.content_hash`
    /// - `recipes.variant_hash`
    /// - `assets.best_variant_hash` where it equals the old hash
    /// - `assets.preview_variant` where it equals the old hash
    ///
    /// SQLite-side only. The caller must also: update the YAML sidecar
    /// (variant `content_hash`, recipe `variant_hash`, asset
    /// `preview_variant`) via the normal save path, rename preview /
    /// smart-preview files on disk, and run
    /// [`Catalog::update_denormalized_variant_columns`] — see
    /// `QueryEngine::writeback_embedded` for the full cascade.
    pub fn migrate_variant_hash(&self, old_hash: &str, new_hash: &str) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        // `file_locations.content_hash` and `recipes.variant_hash`
        // reference `variants(content_hash)`; updating parent and
        // children one statement at a time necessarily passes through
        // states where the constraint is violated. Defer enforcement
        // to COMMIT — the end state is consistent.
        tx.execute_batch("PRAGMA defer_foreign_keys = ON")?;
        tx.execute(
            "UPDATE variants SET content_hash = ?1 WHERE content_hash = ?2",
            rusqlite::params![new_hash, old_hash],
        )?;
        tx.execute(
            "UPDATE file_locations SET content_hash = ?1 WHERE content_hash = ?2",
            rusqlite::params![new_hash, old_hash],
        )?;
        tx.execute(
            "UPDATE recipes SET variant_hash = ?1 WHERE variant_hash = ?2",
            rusqlite::params![new_hash, old_hash],
        )?;
        tx.execute(
            "UPDATE assets SET best_variant_hash = ?1 WHERE best_variant_hash = ?2",
            rusqlite::params![new_hash, old_hash],
        )?;
        tx.execute(
            "UPDATE assets SET preview_variant = ?1 WHERE preview_variant = ?2",
            rusqlite::params![new_hash, old_hash],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// List all file locations with their associated asset IDs.
    /// Returns `(asset_id, volume_id, relative_path)` tuples.
    pub fn list_all_locations_with_assets(&self) -> Result<Vec<(String, String, String)>> {
        let mut stmt = self.conn.prepare(
            "SELECT v.asset_id, fl.volume_id, fl.relative_path \
             FROM file_locations fl \
             JOIN variants v ON fl.content_hash = v.content_hash",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?;
        let mut results = Vec::new();
        for row in rows {
            results.push(row?);
        }
        Ok(results)
    }

}

#[cfg(test)]
mod tests {
    use crate::catalog::test_support::*;
    use crate::catalog::*;

    /// `migrate_variant_hash` must move every referencing row to the
    /// new hash in one go: variants PK, file_locations, recipes'
    /// variant_hash, and the assets row's best_variant_hash +
    /// preview_variant where they pointed at the old hash.
    #[test]
    fn migrate_variant_hash_updates_all_references() {
        let catalog = Catalog::open_in_memory().unwrap();
        catalog.initialize().unwrap();

        let old_hash = "sha256:oldhash";
        let new_hash = "sha256:newhash";

        let mut asset = crate::models::Asset::new(crate::models::AssetType::Image, old_hash);
        asset.preview_variant = Some(old_hash.to_string());
        let volume_id = uuid::Uuid::new_v4();
        catalog
            .conn()
            .execute(
                "INSERT INTO volumes (id, label, mount_point, volume_type) \
                 VALUES (?1, 'test-vol', '/mnt/test', 'external')",
                rusqlite::params![volume_id.to_string()],
            )
            .unwrap();
        let variant = crate::models::Variant {
            content_hash: old_hash.to_string(),
            asset_id: asset.id,
            role: crate::models::VariantRole::Original,
            format: "jpg".to_string(),
            file_size: 100,
            original_filename: "img.jpg".to_string(),
            source_metadata: Default::default(),
            locations: vec![crate::models::volume::FileLocation {
                volume_id,
                relative_path: std::path::PathBuf::from("photos/img.jpg"),
                verified_at: None,
            }],
        };
        asset.variants.push(variant.clone());
        catalog.insert_asset(&asset).unwrap();
        catalog.insert_variant(&variant).unwrap();
        catalog
            .insert_file_location(old_hash, &variant.locations[0])
            .unwrap();
        let recipe = crate::models::Recipe {
            id: uuid::Uuid::new_v4(),
            variant_hash: old_hash.to_string(),
            software: "test".to_string(),
            recipe_type: crate::models::RecipeType::Sidecar,
            content_hash: "sha256:recipehash".to_string(),
            location: crate::models::volume::FileLocation {
                volume_id,
                relative_path: std::path::PathBuf::from("photos/img.xmp"),
                verified_at: None,
            },
            pending_writeback: false,
        };
        catalog.insert_recipe(&recipe).unwrap();

        catalog.migrate_variant_hash(old_hash, new_hash).unwrap();

        let q = |sql: &str, param: &str| -> i64 {
            catalog
                .conn()
                .query_row(sql, rusqlite::params![param], |r| r.get(0))
                .unwrap()
        };
        assert_eq!(q("SELECT COUNT(*) FROM variants WHERE content_hash = ?1", new_hash), 1);
        assert_eq!(q("SELECT COUNT(*) FROM variants WHERE content_hash = ?1", old_hash), 0);
        assert_eq!(q("SELECT COUNT(*) FROM file_locations WHERE content_hash = ?1", new_hash), 1);
        assert_eq!(q("SELECT COUNT(*) FROM file_locations WHERE content_hash = ?1", old_hash), 0);
        assert_eq!(q("SELECT COUNT(*) FROM recipes WHERE variant_hash = ?1", new_hash), 1);
        assert_eq!(q("SELECT COUNT(*) FROM recipes WHERE variant_hash = ?1", old_hash), 0);
        let (best, preview): (Option<String>, Option<String>) = catalog
            .conn()
            .query_row(
                "SELECT best_variant_hash, preview_variant FROM assets WHERE id = ?1",
                rusqlite::params![asset.id.to_string()],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(best.as_deref(), Some(new_hash));
        assert_eq!(preview.as_deref(), Some(new_hash));
    }

    #[test]
    fn insert_variant_populates_metadata_columns() {
        let catalog = setup_metadata_catalog();

        let row: (Option<String>, Option<String>, Option<f64>, Option<f64>, Option<i64>, Option<i64>, Option<i64>) =
            catalog.conn.query_row(
                "SELECT camera_model, lens_model, focal_length_mm, f_number, iso, image_width, image_height \
                 FROM variants WHERE content_hash = 'sha256:meta1'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?)),
            ).unwrap();

        assert_eq!(row.0.as_deref(), Some("X-T5"));
        assert_eq!(row.1.as_deref(), Some("XF56mmF1.2 R"));
        assert!((row.2.unwrap() - 56.0).abs() < 0.01);
        assert!((row.3.unwrap() - 1.2).abs() < 0.01);
        assert_eq!(row.4, Some(400));
        assert_eq!(row.5, Some(6240));
        assert_eq!(row.6, Some(4160));
    }
}
