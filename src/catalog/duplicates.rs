//! `duplicates` section of `Catalog` — methods extracted from the original
//! 9.2-kLOC catalog.rs into a multi-file `impl Catalog` block.
//!
//! Types, helper functions, and the connection ctor live in the parent
//! `catalog` module.

use super::*;

impl Catalog {
    // ═══ DUPLICATES ═══

    /// Find variants that have more than one file location (duplicates).
    pub fn find_duplicates(&self) -> Result<Vec<DuplicateEntry>> {
        self.load_duplicate_entries(
            "SELECT v.content_hash, v.original_filename, v.format, v.file_size, a.name, a.id \
             FROM variants v \
             JOIN assets a ON v.asset_id = a.id \
             WHERE v.content_hash IN ( \
                 SELECT content_hash FROM file_locations \
                 GROUP BY content_hash HAVING COUNT(*) > 1 \
             ) \
             ORDER BY v.file_size DESC",
        )
    }

    /// Find variants with 2+ locations on the **same** volume.
    pub fn find_duplicates_same_volume(&self) -> Result<Vec<DuplicateEntry>> {
        self.load_duplicate_entries(
            "SELECT v.content_hash, v.original_filename, v.format, v.file_size, a.name, a.id \
             FROM variants v \
             JOIN assets a ON v.asset_id = a.id \
             WHERE v.content_hash IN ( \
                 SELECT content_hash FROM file_locations \
                 GROUP BY content_hash, volume_id HAVING COUNT(*) > 1 \
             ) \
             ORDER BY v.file_size DESC",
        )
    }

    /// Find variants with locations on 2+ **different** volumes.
    pub fn find_duplicates_cross_volume(&self) -> Result<Vec<DuplicateEntry>> {
        self.load_duplicate_entries(
            "SELECT v.content_hash, v.original_filename, v.format, v.file_size, a.name, a.id \
             FROM variants v \
             JOIN assets a ON v.asset_id = a.id \
             WHERE v.content_hash IN ( \
                 SELECT content_hash FROM file_locations \
                 GROUP BY content_hash HAVING COUNT(DISTINCT volume_id) > 1 \
             ) \
             ORDER BY v.file_size DESC",
        )
    }

    /// Find duplicates with optional filters for volume, format, and path prefix.
    pub fn find_duplicates_filtered(
        &self,
        mode: &str,
        volume: Option<&str>,
        format: Option<&str>,
        path_prefix: Option<&str>,
    ) -> Result<Vec<DuplicateEntry>> {
        // Build the inner GROUP BY subquery based on mode
        let inner = match mode {
            "same" => {
                "SELECT content_hash FROM file_locations \
                 GROUP BY content_hash, volume_id HAVING COUNT(*) > 1"
            }
            "cross" => {
                "SELECT content_hash FROM file_locations \
                 GROUP BY content_hash HAVING COUNT(DISTINCT volume_id) > 1"
            }
            _ => {
                "SELECT content_hash FROM file_locations \
                 GROUP BY content_hash HAVING COUNT(*) > 1"
            }
        };

        let mut sql = format!(
            "SELECT v.content_hash, v.original_filename, v.format, v.file_size, a.name, a.id \
             FROM variants v \
             JOIN assets a ON v.asset_id = a.id \
             WHERE v.content_hash IN ({inner})"
        );

        let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();

        if let Some(vol) = volume {
            sql.push_str(
                " AND v.content_hash IN (SELECT content_hash FROM file_locations WHERE volume_id = ?)",
            );
            params.push(Box::new(vol.to_string()));
        }

        if let Some(prefix) = path_prefix {
            let like = path_pattern_to_like(prefix);
            sql.push_str(
                " AND v.content_hash IN (SELECT content_hash FROM file_locations WHERE relative_path LIKE ? ESCAPE '\\')",
            );
            params.push(Box::new(like));
        }

        if let Some(fmt) = format {
            sql.push_str(" AND LOWER(v.format) = ?");
            params.push(Box::new(fmt.to_lowercase()));
        }

        sql.push_str(" ORDER BY v.file_size DESC");

        self.load_duplicate_entries_filtered(&sql, &params)
    }

    /// Like `load_duplicate_entries` but accepts dynamic params.
    pub(super) fn load_duplicate_entries_filtered(
        &self,
        variant_query: &str,
        params: &[Box<dyn rusqlite::types::ToSql>],
    ) -> Result<Vec<DuplicateEntry>> {
        let mut stmt = self.conn.prepare(variant_query)?;

        let param_refs: Vec<&dyn rusqlite::types::ToSql> =
            params.iter().map(|p| p.as_ref()).collect();

        let entries: Vec<DuplicateEntry> = stmt
            .query_map(param_refs.as_slice(), |row| {
                Ok(DuplicateEntry {
                    content_hash: row.get(0)?,
                    original_filename: row.get(1)?,
                    format: row.get(2)?,
                    file_size: row.get(3)?,
                    asset_name: row.get(4)?,
                    asset_id: row.get(5)?,
                    locations: Vec::new(),
                    volume_count: 0,
                    same_volume_groups: Vec::new(),
                    preview_url: String::new(),
                })
            })?
            .collect::<std::result::Result<_, _>>()?;

        let mut lstmt = self.conn.prepare(
            "SELECT fl.relative_path, vol.label, vol.id, vol.purpose, fl.verified_at \
             FROM file_locations fl \
             JOIN volumes vol ON fl.volume_id = vol.id \
             WHERE fl.content_hash = ?1",
        )?;

        let entries: Vec<DuplicateEntry> = entries
            .into_iter()
            .map(|mut e| {
                e.locations = Self::load_locations_for_hash_checked(&mut lstmt, &e.content_hash)?;
                Self::compute_duplicate_stats(&mut e);
                Ok(e)
            })
            .collect::<Result<_>>()?;

        Ok(entries)
    }

    /// Delete a specific file location row. Returns true if a row was deleted.
    pub fn delete_file_location(
        &self,
        content_hash: &str,
        volume_id: &str,
        relative_path: &str,
    ) -> Result<bool> {
        let changed = self.conn.execute(
            "DELETE FROM file_locations WHERE content_hash = ?1 AND volume_id = ?2 AND relative_path = ?3",
            rusqlite::params![content_hash, volume_id, relative_path],
        )?;
        Ok(changed > 0)
    }

    /// Delete a recipe record by ID. Returns true if a row was deleted.
    pub fn delete_recipe(&self, recipe_id: &str) -> Result<bool> {
        let changed = self.conn.execute(
            "DELETE FROM recipes WHERE id = ?1",
            rusqlite::params![recipe_id],
        )?;
        Ok(changed > 0)
    }

    /// Update the volume and path for a recipe.
    pub fn update_recipe_location(
        &self,
        recipe_id: &str,
        volume_id: &str,
        relative_path: &str,
    ) -> Result<()> {
        let changed = self.conn.execute(
            "UPDATE recipes SET volume_id = ?1, relative_path = ?2 WHERE id = ?3",
            rusqlite::params![volume_id, relative_path, recipe_id],
        )?;
        if changed == 0 {
            anyhow::bail!("no recipe found with id '{recipe_id}'");
        }
        Ok(())
    }

    /// Update the `verified_at` timestamp for a file location.
    pub fn update_verified_at(
        &self,
        content_hash: &str,
        volume_id: &str,
        relative_path: &str,
    ) -> Result<()> {
        let now = chrono::Utc::now().to_rfc3339();
        self.conn.execute(
            "UPDATE file_locations SET verified_at = ?1 \
             WHERE content_hash = ?2 AND volume_id = ?3 AND relative_path = ?4",
            rusqlite::params![now, content_hash, volume_id, relative_path],
        )?;
        Ok(())
    }

    /// Get the verified_at timestamp for a file location or recipe at this volume+path.
    pub fn get_location_verified_at(
        &self,
        volume_id: &str,
        relative_path: &str,
    ) -> Result<Option<String>> {
        // Check file_locations first
        let mut stmt = self.conn.prepare(
            "SELECT verified_at FROM file_locations WHERE volume_id = ?1 AND relative_path = ?2 LIMIT 1",
        )?;
        let mut rows = stmt.query(rusqlite::params![volume_id, relative_path])?;
        if let Some(row) = rows.next()? {
            let verified_at: Option<String> = row.get(0)?;
            if verified_at.is_some() {
                return Ok(verified_at);
            }
        }
        // Fall back to recipes table
        let mut stmt = self.conn.prepare(
            "SELECT verified_at FROM recipes WHERE volume_id = ?1 AND relative_path = ?2 LIMIT 1",
        )?;
        let mut rows = stmt.query(rusqlite::params![volume_id, relative_path])?;
        if let Some(row) = rows.next()? {
            return Ok(row.get(0)?);
        }
        Ok(None)
    }

}

#[cfg(test)]
mod tests {
    use crate::catalog::test_support::*;
    use crate::catalog::*;

    #[test]
    fn find_duplicates_returns_entries_with_multiple_locations() {
        let catalog = Catalog::open_in_memory().unwrap();
        catalog.initialize().unwrap();

        let vol1 = crate::models::Volume::new(
            "vol-a".to_string(),
            std::path::PathBuf::from("/mnt/a"),
            crate::models::VolumeType::Local,
        );
        let vol2 = crate::models::Volume::new(
            "vol-b".to_string(),
            std::path::PathBuf::from("/mnt/b"),
            crate::models::VolumeType::Local,
        );
        catalog.ensure_volume(&vol1).unwrap();
        catalog.ensure_volume(&vol2).unwrap();

        // Asset with a variant that has two locations (duplicate)
        let asset = crate::models::Asset::new(crate::models::AssetType::Image, "sha256:dup1");
        catalog.insert_asset(&asset).unwrap();

        let variant = crate::models::Variant { file_size: 5000, ..test_variant(asset.id, "sha256:dup1", "photo.jpg") };
        catalog.insert_variant(&variant).unwrap();

        let loc1 = crate::models::FileLocation {
            volume_id: vol1.id,
            relative_path: std::path::PathBuf::from("photos/photo.jpg"),
            verified_at: None,
        };
        let loc2 = crate::models::FileLocation {
            volume_id: vol2.id,
            relative_path: std::path::PathBuf::from("backup/photo.jpg"),
            verified_at: None,
        };
        catalog.insert_file_location(&variant.content_hash, &loc1).unwrap();
        catalog.insert_file_location(&variant.content_hash, &loc2).unwrap();

        // Asset with only one location (not a duplicate)
        let asset2 = crate::models::Asset::new(crate::models::AssetType::Image, "sha256:single1");
        catalog.insert_asset(&asset2).unwrap();

        let variant2 = test_variant(asset2.id, "sha256:single1", "unique.png");
        catalog.insert_variant(&variant2).unwrap();

        let loc3 = crate::models::FileLocation {
            volume_id: vol1.id,
            relative_path: std::path::PathBuf::from("photos/unique.png"),
            verified_at: None,
        };
        catalog.insert_file_location(&variant2.content_hash, &loc3).unwrap();

        let dupes = catalog.find_duplicates().unwrap();
        assert_eq!(dupes.len(), 1);
        assert_eq!(dupes[0].content_hash, "sha256:dup1");
        assert_eq!(dupes[0].original_filename, "photo.jpg");
        assert_eq!(dupes[0].locations.len(), 2);
    }

    #[test]
    fn find_duplicates_empty_when_no_duplicates() {
        let catalog = Catalog::open_in_memory().unwrap();
        catalog.initialize().unwrap();
        let dupes = catalog.find_duplicates().unwrap();
        assert!(dupes.is_empty());
    }

    #[test]
    fn find_duplicates_same_volume() {
        let catalog = Catalog::open_in_memory().unwrap();
        catalog.initialize().unwrap();

        let vol1 = crate::models::Volume::new(
            "vol-a".to_string(),
            std::path::PathBuf::from("/mnt/a"),
            crate::models::VolumeType::Local,
        );
        let vol2 = crate::models::Volume::new(
            "vol-b".to_string(),
            std::path::PathBuf::from("/mnt/b"),
            crate::models::VolumeType::Local,
        );
        catalog.ensure_volume(&vol1).unwrap();
        catalog.ensure_volume(&vol2).unwrap();

        // Variant with 2 locations on SAME volume (same-volume dup)
        let asset1 = crate::models::Asset::new(crate::models::AssetType::Image, "sha256:sv1");
        catalog.insert_asset(&asset1).unwrap();
        let v1 = crate::models::Variant { file_size: 5000, ..test_variant(asset1.id, "sha256:sv1", "same_vol.jpg") };
        catalog.insert_variant(&v1).unwrap();
        catalog.insert_file_location(&v1.content_hash, &crate::models::FileLocation {
            volume_id: vol1.id,
            relative_path: std::path::PathBuf::from("photos/same_vol.jpg"),
            verified_at: None,
        }).unwrap();
        catalog.insert_file_location(&v1.content_hash, &crate::models::FileLocation {
            volume_id: vol1.id,
            relative_path: std::path::PathBuf::from("backup/same_vol.jpg"),
            verified_at: None,
        }).unwrap();

        // Variant with locations on DIFFERENT volumes (cross-volume)
        let asset2 = crate::models::Asset::new(crate::models::AssetType::Image, "sha256:cv1");
        catalog.insert_asset(&asset2).unwrap();
        let v2 = crate::models::Variant { file_size: 3000, ..test_variant(asset2.id, "sha256:cv1", "cross_vol.jpg") };
        catalog.insert_variant(&v2).unwrap();
        catalog.insert_file_location(&v2.content_hash, &crate::models::FileLocation {
            volume_id: vol1.id,
            relative_path: std::path::PathBuf::from("photos/cross_vol.jpg"),
            verified_at: None,
        }).unwrap();
        catalog.insert_file_location(&v2.content_hash, &crate::models::FileLocation {
            volume_id: vol2.id,
            relative_path: std::path::PathBuf::from("backup/cross_vol.jpg"),
            verified_at: None,
        }).unwrap();

        // same-volume should only return sv1
        let same = catalog.find_duplicates_same_volume().unwrap();
        assert_eq!(same.len(), 1);
        assert_eq!(same[0].content_hash, "sha256:sv1");
        assert!(!same[0].same_volume_groups.is_empty());

        // cross-volume should only return cv1
        let cross = catalog.find_duplicates_cross_volume().unwrap();
        assert_eq!(cross.len(), 1);
        assert_eq!(cross[0].content_hash, "sha256:cv1");
        assert_eq!(cross[0].volume_count, 2);
    }

    #[test]
    fn find_duplicates_location_details_enriched() {
        let catalog = Catalog::open_in_memory().unwrap();
        catalog.initialize().unwrap();

        let mut vol = crate::models::Volume::new(
            "work".to_string(),
            std::path::PathBuf::from("/mnt/work"),
            crate::models::VolumeType::Local,
        );
        vol.purpose = Some(crate::models::volume::VolumePurpose::Working);
        catalog.ensure_volume(&vol).unwrap();

        let mut vol2 = crate::models::Volume::new(
            "backup".to_string(),
            std::path::PathBuf::from("/mnt/backup"),
            crate::models::VolumeType::External,
        );
        vol2.purpose = Some(crate::models::volume::VolumePurpose::Backup);
        catalog.ensure_volume(&vol2).unwrap();

        let asset = crate::models::Asset::new(crate::models::AssetType::Image, "sha256:enr1");
        catalog.insert_asset(&asset).unwrap();
        let variant = crate::models::Variant { file_size: 5000, ..test_variant(asset.id, "sha256:enr1", "photo.jpg") };
        catalog.insert_variant(&variant).unwrap();
        catalog.insert_file_location(&variant.content_hash, &crate::models::FileLocation {
            volume_id: vol.id,
            relative_path: std::path::PathBuf::from("photos/photo.jpg"),
            verified_at: None,
        }).unwrap();
        catalog.insert_file_location(&variant.content_hash, &crate::models::FileLocation {
            volume_id: vol2.id,
            relative_path: std::path::PathBuf::from("backup/photo.jpg"),
            verified_at: None,
        }).unwrap();

        let dupes = catalog.find_duplicates().unwrap();
        assert_eq!(dupes.len(), 1);
        assert_eq!(dupes[0].volume_count, 2);
        assert!(dupes[0].same_volume_groups.is_empty());

        // Check enriched location details
        let work_loc = dupes[0].locations.iter().find(|l| l.volume_label == "work").unwrap();
        assert_eq!(work_loc.volume_id, vol.id.to_string());
        assert_eq!(work_loc.volume_purpose.as_deref(), Some("working"));

        let backup_loc = dupes[0].locations.iter().find(|l| l.volume_label == "backup").unwrap();
        assert_eq!(backup_loc.volume_id, vol2.id.to_string());
        assert_eq!(backup_loc.volume_purpose.as_deref(), Some("backup"));
    }

    #[test]
    fn delete_file_location_removes_row() {
        let catalog = Catalog::open_in_memory().unwrap();
        catalog.initialize().unwrap();

        let volume = crate::models::Volume::new(
            "vol".to_string(),
            std::path::PathBuf::from("/mnt/vol"),
            crate::models::VolumeType::Local,
        );
        catalog.ensure_volume(&volume).unwrap();

        let asset = crate::models::Asset::new(crate::models::AssetType::Image, "sha256:delloc1");
        catalog.insert_asset(&asset).unwrap();

        let variant = crate::models::Variant { file_size: 100, ..test_variant(asset.id, "sha256:delloc1", "photo.jpg") };
        catalog.insert_variant(&variant).unwrap();

        let loc = crate::models::FileLocation {
            volume_id: volume.id,
            relative_path: std::path::PathBuf::from("photos/photo.jpg"),
            verified_at: None,
        };
        catalog
            .insert_file_location(&variant.content_hash, &loc)
            .unwrap();

        let deleted = catalog
            .delete_file_location("sha256:delloc1", &volume.id.to_string(), "photos/photo.jpg")
            .unwrap();
        assert!(deleted);

        // Verify location is gone
        let details = catalog
            .load_asset_details(&asset.id.to_string())
            .unwrap()
            .unwrap();
        assert!(details.variants[0].locations.is_empty());
    }

    #[test]
    fn delete_file_location_returns_false_for_missing() {
        let catalog = Catalog::open_in_memory().unwrap();
        catalog.initialize().unwrap();

        let deleted = catalog
            .delete_file_location("sha256:nope", "some-vol", "some/path.jpg")
            .unwrap();
        assert!(!deleted);
    }

    #[test]
    fn update_recipe_location_changes_values() {
        let catalog = Catalog::open_in_memory().unwrap();
        catalog.initialize().unwrap();

        let vol1 = crate::models::Volume::new(
            "vol1".to_string(),
            std::path::PathBuf::from("/mnt/vol1"),
            crate::models::VolumeType::Local,
        );
        let vol2 = crate::models::Volume::new(
            "vol2".to_string(),
            std::path::PathBuf::from("/mnt/vol2"),
            crate::models::VolumeType::Local,
        );
        catalog.ensure_volume(&vol1).unwrap();
        catalog.ensure_volume(&vol2).unwrap();

        let asset = crate::models::Asset::new(crate::models::AssetType::Image, "sha256:rec1");
        catalog.insert_asset(&asset).unwrap();

        let variant = crate::models::Variant { file_size: 500, ..test_variant(asset.id, "sha256:rec1", "photo.nef") };
        catalog.insert_variant(&variant).unwrap();

        let recipe_id = uuid::Uuid::new_v4();
        let recipe = crate::models::Recipe {
            id: recipe_id,
            variant_hash: "sha256:rec1".to_string(),
            software: "Adobe".to_string(),
            recipe_type: crate::models::RecipeType::Sidecar,
            content_hash: "sha256:recipe_hash".to_string(),
            location: crate::models::FileLocation {
                volume_id: vol1.id,
                relative_path: std::path::PathBuf::from("photos/photo.xmp"),
                verified_at: None,
            },
            pending_writeback: false,
        };
        catalog.insert_recipe(&recipe).unwrap();

        catalog
            .update_recipe_location(
                &recipe_id.to_string(),
                &vol2.id.to_string(),
                "backup/photo.xmp",
            )
            .unwrap();

        // Verify by querying the recipe
        let row: (String, String) = catalog
            .conn
            .query_row(
                "SELECT volume_id, relative_path FROM recipes WHERE id = ?1",
                rusqlite::params![recipe_id.to_string()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(row.0, vol2.id.to_string());
        assert_eq!(row.1, "backup/photo.xmp");
    }

    #[test]
    fn update_verified_at_sets_timestamp() {
        let catalog = Catalog::open_in_memory().unwrap();
        catalog.initialize().unwrap();

        let volume = crate::models::Volume::new(
            "vol".to_string(),
            std::path::PathBuf::from("/mnt/vol"),
            crate::models::VolumeType::Local,
        );
        catalog.ensure_volume(&volume).unwrap();

        let asset = crate::models::Asset::new(crate::models::AssetType::Image, "sha256:ver1");
        catalog.insert_asset(&asset).unwrap();

        let variant = crate::models::Variant { file_size: 100, ..test_variant(asset.id, "sha256:ver1", "photo.jpg") };
        catalog.insert_variant(&variant).unwrap();

        let loc = crate::models::FileLocation {
            volume_id: volume.id,
            relative_path: std::path::PathBuf::from("photos/photo.jpg"),
            verified_at: None,
        };
        catalog.insert_file_location(&variant.content_hash, &loc).unwrap();

        // Initially null
        let before: Option<String> = catalog
            .conn
            .query_row(
                "SELECT verified_at FROM file_locations WHERE content_hash = ?1",
                rusqlite::params!["sha256:ver1"],
                |row| row.get(0),
            )
            .unwrap();
        assert!(before.is_none());

        // Update
        catalog
            .update_verified_at("sha256:ver1", &volume.id.to_string(), "photos/photo.jpg")
            .unwrap();

        let after: Option<String> = catalog
            .conn
            .query_row(
                "SELECT verified_at FROM file_locations WHERE content_hash = ?1",
                rusqlite::params!["sha256:ver1"],
                |row| row.get(0),
            )
            .unwrap();
        assert!(after.is_some());
    }

    #[test]
    fn update_verified_at_noop_for_missing() {
        let catalog = Catalog::open_in_memory().unwrap();
        catalog.initialize().unwrap();
        // Should not error, just update 0 rows
        catalog
            .update_verified_at("sha256:nope", "some-vol", "some/path.jpg")
            .unwrap();
    }

    #[test]
    fn update_recipe_location_errors_on_missing() {
        let catalog = Catalog::open_in_memory().unwrap();
        catalog.initialize().unwrap();
        let err = catalog
            .update_recipe_location("nonexistent-id", "vol", "path")
            .unwrap_err();
        assert!(err.to_string().contains("no recipe found"));
    }

    #[test]
    fn get_location_verified_at_returns_timestamp() {
        let catalog = Catalog::open_in_memory().unwrap();
        catalog.initialize().unwrap();

        let volume = crate::models::Volume::new(
            "vol".to_string(),
            std::path::PathBuf::from("/mnt/vol"),
            crate::models::VolumeType::Local,
        );
        catalog.ensure_volume(&volume).unwrap();

        let asset = crate::models::Asset::new(crate::models::AssetType::Image, "sha256:gv1");
        catalog.insert_asset(&asset).unwrap();

        let variant = crate::models::Variant { file_size: 100, ..test_variant(asset.id, "sha256:gv1", "photo.jpg") };
        catalog.insert_variant(&variant).unwrap();

        let loc = crate::models::FileLocation {
            volume_id: volume.id,
            relative_path: std::path::PathBuf::from("photos/photo.jpg"),
            verified_at: None,
        };
        catalog.insert_file_location(&variant.content_hash, &loc).unwrap();

        // Before verify: should be None
        let before = catalog
            .get_location_verified_at(&volume.id.to_string(), "photos/photo.jpg")
            .unwrap();
        assert!(before.is_none());

        // Set verified_at
        catalog
            .update_verified_at("sha256:gv1", &volume.id.to_string(), "photos/photo.jpg")
            .unwrap();

        // After verify: should have a timestamp
        let after = catalog
            .get_location_verified_at(&volume.id.to_string(), "photos/photo.jpg")
            .unwrap();
        assert!(after.is_some());
    }

    #[test]
    fn get_location_verified_at_returns_none_for_unknown() {
        let catalog = Catalog::open_in_memory().unwrap();
        catalog.initialize().unwrap();

        let result = catalog
            .get_location_verified_at("nonexistent-vol", "no/such/path.jpg")
            .unwrap();
        assert!(result.is_none());
    }
}
