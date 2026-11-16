//! `recipe_query` section of `Catalog` — methods extracted from the original
//! 9.2-kLOC catalog.rs into a multi-file `impl Catalog` block.
//!
//! Types, helper functions, and the connection ctor live in the parent
//! `catalog` module.

use super::*;

impl Catalog {
    // ═══ LOCATION & RECIPE QUERIES ═══

    /// Check if a recipe with the given content hash exists.
    pub fn has_recipe_by_content_hash(&self, content_hash: &str) -> Result<bool> {
        let count: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM recipes WHERE content_hash = ?1",
            rusqlite::params![content_hash],
            |row| row.get(0),
        )?;
        Ok(count > 0)
    }

    /// Check whether an asset has any recipe (`.xmp` sidecar) across any of
    /// its variants. **This is the canonical sidecar-wins gate.**
    ///
    /// Every code path that reads embedded XMP from a JPEG/TIFF variant
    /// must call this first and skip the read when it returns `true`.
    /// The sidecar is MAKI's authoritative metadata file (writeback keeps
    /// it current); embedded XMP in JPEG/TIFF containers is frozen at
    /// import time — writeback never patches it — and reading it would
    /// re-inject stale flat keywords on every refresh.
    ///
    /// Known call sites (keep this list in sync when adding new ones):
    /// - `asset_service::refresh::refresh` (the `refresh --media` loop)
    /// - `asset_service::refresh::sync_metadata` (Phase 3 media loop)
    /// - `query::reimport_metadata_inner` (the web UI's "Reload
    ///   metadata" button + `maki refresh --reimport`)
    pub fn asset_has_recipe(&self, asset_id: &str) -> Result<bool> {
        let count: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM recipes r \
             JOIN variants v ON r.variant_hash = v.content_hash \
             WHERE v.asset_id = ?1",
            rusqlite::params![asset_id],
            |row| row.get(0),
        )?;
        Ok(count > 0)
    }

    /// Find a recipe by its location (variant_hash, volume_id, relative_path).
    /// Returns `(recipe_id, content_hash)` if found.
    pub fn find_recipe_by_location(
        &self,
        variant_hash: &str,
        volume_id: &str,
        relative_path: &str,
    ) -> Result<Option<(String, String)>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, content_hash FROM recipes \
             WHERE variant_hash = ?1 AND volume_id = ?2 AND relative_path = ?3",
        )?;
        let mut rows = stmt.query(rusqlite::params![variant_hash, volume_id, relative_path])?;
        match rows.next()? {
            Some(row) => Ok(Some((row.get(0)?, row.get(1)?))),
            None => Ok(None),
        }
    }

    /// Update a recipe's content hash (used when a recipe file changes on re-import).
    pub fn update_recipe_content_hash(&self, recipe_id: &str, new_content_hash: &str) -> Result<()> {
        let changed = self.conn.execute(
            "UPDATE recipes SET content_hash = ?1 WHERE id = ?2",
            rusqlite::params![new_content_hash, recipe_id],
        )?;
        if changed == 0 {
            anyhow::bail!("no recipe found with id '{recipe_id}'");
        }
        Ok(())
    }

    /// Mark a recipe as needing XMP write-back (e.g. volume was offline during edit).
    pub fn mark_pending_writeback(&self, recipe_id: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE recipes SET pending_writeback = 1 WHERE id = ?1",
            rusqlite::params![recipe_id],
        )?;
        Ok(())
    }

    /// Clear the pending write-back flag (after successful XMP write).
    pub fn clear_pending_writeback(&self, recipe_id: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE recipes SET pending_writeback = 0 WHERE id = ?1",
            rusqlite::params![recipe_id],
        )?;
        Ok(())
    }

    /// List recipes with pending write-back, optionally filtered by volume.
    /// Returns `(recipe_id, asset_id, volume_id, relative_path)`.
    pub fn list_pending_writeback_recipes(
        &self,
        volume_id: Option<&str>,
    ) -> Result<Vec<(String, String, String, String)>> {
        let (sql, params): (String, Vec<Box<dyn rusqlite::types::ToSql>>) = if let Some(vid) = volume_id {
            (
                "SELECT r.id, v.asset_id, r.volume_id, r.relative_path \
                 FROM recipes r \
                 JOIN variants v ON r.variant_hash = v.content_hash \
                 WHERE r.pending_writeback = 1 AND r.volume_id = ?1"
                    .to_string(),
                vec![Box::new(vid.to_string())],
            )
        } else {
            (
                "SELECT r.id, v.asset_id, r.volume_id, r.relative_path \
                 FROM recipes r \
                 JOIN variants v ON r.variant_hash = v.content_hash \
                 WHERE r.pending_writeback = 1"
                    .to_string(),
                vec![],
            )
        };
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(params.iter()), |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
            ))
        })?;
        let mut result = Vec::new();
        for row in rows {
            result.push(row?);
        }
        Ok(result)
    }

    /// Find a recipe by volume and path (ignoring variant_hash).
    /// Returns `(recipe_id, content_hash, variant_hash)` if found.
    pub fn find_recipe_by_volume_and_path(
        &self,
        volume_id: &str,
        relative_path: &str,
    ) -> Result<Option<(String, String, String)>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, content_hash, variant_hash FROM recipes \
             WHERE volume_id = ?1 AND relative_path = ?2",
        )?;
        let mut rows = stmt.query(rusqlite::params![volume_id, relative_path])?;
        match rows.next()? {
            Some(row) => Ok(Some((row.get(0)?, row.get(1)?, row.get(2)?))),
            None => Ok(None),
        }
    }

    /// Update `verified_at` timestamp on a recipe by its location.
    pub fn update_recipe_verified_at(
        &self,
        variant_hash: &str,
        volume_id: &str,
        relative_path: &str,
    ) -> Result<()> {
        let now = chrono::Utc::now().to_rfc3339();
        self.conn.execute(
            "UPDATE recipes SET verified_at = ?1 \
             WHERE variant_hash = ?2 AND volume_id = ?3 AND relative_path = ?4",
            rusqlite::params![now, variant_hash, volume_id, relative_path],
        )?;
        Ok(())
    }

    /// Find a variant by its exact volume + relative_path.
    /// Returns `(content_hash, format)`.
    pub fn find_variant_by_volume_and_path(
        &self,
        volume_id: &str,
        relative_path: &str,
    ) -> Result<Option<(String, String)>> {
        let mut stmt = self.conn.prepare(
            "SELECT v.content_hash, v.format FROM file_locations fl \
             JOIN variants v ON fl.content_hash = v.content_hash \
             WHERE fl.volume_id = ?1 AND fl.relative_path = ?2 \
             LIMIT 1",
        )?;
        let mut rows = stmt.query(rusqlite::params![volume_id, relative_path])?;
        match rows.next()? {
            Some(row) => Ok(Some((row.get(0)?, row.get(1)?))),
            None => Ok(None),
        }
    }

    /// Find a variant whose file location on the given volume shares the same
    /// directory prefix and filename stem. Returns `(content_hash, asset_id)`.
    /// Optionally excludes a specific asset ID from results (used by fix-recipes
    /// to avoid self-matching the standalone recipe asset).
    pub fn find_variant_hash_by_stem_and_directory(
        &self,
        stem: &str,
        directory_prefix: &str,
        volume_id: &str,
        exclude_asset_id: Option<&str>,
    ) -> Result<Option<(String, String)>> {
        // Match file_locations where: same volume, path starts with directory_prefix,
        // and the filename (without extension) matches the stem.
        let path_pattern = if directory_prefix.is_empty() {
            format!("{stem}.%")
        } else {
            format!("{directory_prefix}/{stem}.%")
        };
        let (sql, params): (String, Vec<Box<dyn rusqlite::types::ToSql>>) = if let Some(exclude) = exclude_asset_id {
            (
                "SELECT fl.content_hash, v.asset_id FROM file_locations fl \
                 JOIN variants v ON fl.content_hash = v.content_hash \
                 WHERE fl.volume_id = ?1 AND fl.relative_path LIKE ?2 AND v.asset_id != ?3 \
                 LIMIT 1".to_string(),
                vec![
                    Box::new(volume_id.to_string()),
                    Box::new(path_pattern),
                    Box::new(exclude.to_string()),
                ],
            )
        } else {
            (
                "SELECT fl.content_hash, v.asset_id FROM file_locations fl \
                 JOIN variants v ON fl.content_hash = v.content_hash \
                 WHERE fl.volume_id = ?1 AND fl.relative_path LIKE ?2 \
                 LIMIT 1".to_string(),
                vec![
                    Box::new(volume_id.to_string()),
                    Box::new(path_pattern),
                ],
            )
        };
        let mut stmt = self.conn.prepare(&sql)?;
        let param_refs: Vec<&dyn rusqlite::types::ToSql> = params.iter().map(|p| p.as_ref()).collect();
        let mut rows = stmt.query(param_refs.as_slice())?;
        match rows.next()? {
            Some(row) => Ok(Some((row.get(0)?, row.get(1)?))),
            None => Ok(None),
        }
    }

    /// List assets that have exactly one variant whose format is a recipe extension
    /// (xmp, cos, cot, cop, pp3, dop, on1) and asset_type = 'other'.
    /// Returns `(asset_id, content_hash, format)` for each match.
    /// Optionally scoped by volume or asset ID.
    pub fn list_recipe_only_assets(
        &self,
        volume_id: Option<&str>,
        asset_id: Option<&str>,
    ) -> Result<Vec<(String, String, String)>> {
        let recipe_extensions: &[&str] = &["xmp", "cos", "cot", "cop", "pp3", "dop", "on1"];

        let mut sql = String::from(
            "SELECT a.id, v.content_hash, v.format \
             FROM assets a \
             JOIN variants v ON v.asset_id = a.id \
             WHERE a.asset_type = 'other'",
        );
        let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
        let mut param_idx = 1;

        if let Some(aid) = asset_id {
            sql.push_str(&format!(" AND a.id = ?{param_idx}"));
            params.push(Box::new(aid.to_string()));
            param_idx += 1;
        }

        if let Some(vid) = volume_id {
            sql.push_str(&format!(
                " AND v.content_hash IN (SELECT content_hash FROM file_locations WHERE volume_id = ?{param_idx})"
            ));
            params.push(Box::new(vid.to_string()));
        }

        sql.push_str(" GROUP BY a.id HAVING COUNT(v.content_hash) = 1");

        let mut stmt = self.conn.prepare(&sql)?;
        let param_refs: Vec<&dyn rusqlite::types::ToSql> = params.iter().map(|p| p.as_ref()).collect();
        let rows = stmt.query_map(param_refs.as_slice(), |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?;

        let mut results = Vec::new();
        for row in rows {
            let (aid, hash, fmt) = row?;
            if recipe_extensions.contains(&fmt.to_lowercase().as_str()) {
                results.push((aid, hash, fmt));
            }
        }
        Ok(results)
    }

    /// Find all asset IDs that have file locations on a given volume under any of
    /// the given path prefixes. Used by `import --auto-group` to scope auto-grouping
    /// to the "neighborhood" of imported files.
    pub fn find_asset_ids_by_volume_and_path_prefixes(
        &self,
        volume_id: &str,
        prefixes: &[String],
    ) -> Result<Vec<String>> {
        if prefixes.is_empty() {
            return Ok(Vec::new());
        }

        // Build dynamic OR clause: one `fl.relative_path LIKE ?N` per prefix
        let mut conditions = Vec::new();
        let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
        params.push(Box::new(volume_id.to_string()));

        for (i, prefix) in prefixes.iter().enumerate() {
            conditions.push(format!("fl.relative_path LIKE ?{}", i + 2));
            if prefix.is_empty() {
                params.push(Box::new("%".to_string()));
            } else {
                params.push(Box::new(format!("{prefix}/%")));
            }
        }

        let sql = format!(
            "SELECT DISTINCT v.asset_id FROM variants v \
             JOIN file_locations fl ON v.content_hash = fl.content_hash \
             WHERE fl.volume_id = ?1 AND ({})",
            conditions.join(" OR ")
        );

        let param_refs: Vec<&dyn rusqlite::types::ToSql> = params.iter().map(|p| p.as_ref()).collect();
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(param_refs.as_slice(), |row| row.get::<_, String>(0))?;

        let mut ids = Vec::new();
        for row in rows {
            ids.push(row?);
        }
        Ok(ids)
    }


    // ── Sync queries (location & recipe paths) ─────────────────────

    /// List all file locations on a volume whose path starts with the given prefix.
    /// Returns `(content_hash, relative_path)` pairs.
    pub fn list_locations_for_volume_under_prefix(
        &self,
        volume_id: &str,
        prefix: &str,
    ) -> Result<Vec<(String, String)>> {
        let pattern = if prefix.is_empty() {
            "%".to_string()
        } else {
            format!("{prefix}%")
        };
        let mut stmt = self.conn.prepare(
            "SELECT content_hash, relative_path FROM file_locations \
             WHERE volume_id = ?1 AND relative_path LIKE ?2",
        )?;
        let rows = stmt.query_map(rusqlite::params![volume_id, pattern], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        let mut result = Vec::new();
        for row in rows {
            result.push(row?);
        }
        Ok(result)
    }

    /// List all recipes on a volume whose path starts with the given prefix.
    /// Returns `(recipe_id, content_hash, variant_hash, relative_path)` tuples.
    pub fn list_recipes_for_volume_under_prefix(
        &self,
        volume_id: &str,
        prefix: &str,
    ) -> Result<Vec<(String, String, String, String)>> {
        let pattern = if prefix.is_empty() {
            "%".to_string()
        } else {
            format!("{prefix}%")
        };
        let mut stmt = self.conn.prepare(
            "SELECT id, content_hash, variant_hash, relative_path FROM recipes \
             WHERE volume_id = ?1 AND relative_path LIKE ?2",
        )?;
        let rows = stmt.query_map(rusqlite::params![volume_id, pattern], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
            ))
        })?;
        let mut result = Vec::new();
        for row in rows {
            result.push(row?);
        }
        Ok(result)
    }

    /// Like `list_recipes_for_volume_under_prefix` but also returns `pending_writeback`.
    /// Returns `(id, content_hash, variant_hash, relative_path, pending_writeback)`.
    pub fn list_recipes_with_pending_for_volume(
        &self,
        volume_id: &str,
    ) -> Result<Vec<(String, String, String, String, bool)>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, content_hash, variant_hash, relative_path, pending_writeback \
             FROM recipes WHERE volume_id = ?1",
        )?;
        let rows = stmt.query_map(rusqlite::params![volume_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, bool>(4)?,
            ))
        })?;
        let mut result = Vec::new();
        for row in rows {
            result.push(row?);
        }
        Ok(result)
    }

    /// List recipes for a specific variant on a specific volume.
    /// Returns `(recipe_id, content_hash, relative_path)` tuples.
    pub fn list_recipes_for_variant_on_volume(
        &self,
        variant_hash: &str,
        volume_id: &str,
    ) -> Result<Vec<(String, String, String)>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, content_hash, relative_path FROM recipes \
             WHERE variant_hash = ?1 AND volume_id = ?2",
        )?;
        let rows = stmt.query_map(rusqlite::params![variant_hash, volume_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?;
        let mut result = Vec::new();
        for row in rows {
            result.push(row?);
        }
        Ok(result)
    }

    /// List all recipes for a given asset (across all volumes).
    /// Returns `(recipe_id, content_hash, variant_hash, relative_path, volume_id)` tuples.
    pub fn list_recipes_for_asset(
        &self,
        asset_id: &str,
    ) -> Result<Vec<(String, String, String, String, String)>> {
        let mut stmt = self.conn.prepare(
            "SELECT r.id, r.content_hash, r.variant_hash, r.relative_path, r.volume_id \
             FROM recipes r \
             JOIN variants v ON r.variant_hash = v.content_hash \
             WHERE v.asset_id = ?1",
        )?;
        let rows = stmt.query_map(rusqlite::params![asset_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
            ))
        })?;
        let mut result = Vec::new();
        for row in rows {
            result.push(row?);
        }
        Ok(result)
    }

    /// List all file locations for an asset's variants.
    /// Returns (content_hash, relative_path, volume_id).
    pub fn list_file_locations_for_asset(
        &self,
        asset_id: &str,
    ) -> Result<Vec<(String, String, String)>> {
        let mut stmt = self.conn.prepare(
            "SELECT fl.content_hash, fl.relative_path, fl.volume_id \
             FROM file_locations fl \
             JOIN variants v ON fl.content_hash = v.content_hash \
             WHERE v.asset_id = ?1",
        )?;
        let rows = stmt.query_map(rusqlite::params![asset_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?;
        let mut result = Vec::new();
        for row in rows {
            result.push(row?);
        }
        Ok(result)
    }

    /// Find all asset IDs that share the same directory (session) as the given asset.
    /// Goes up one directory level from each of the asset's file locations to find
    /// "session roots", then returns all asset IDs with files under those roots.
    pub fn find_same_session_asset_ids(&self, asset_id: &str) -> Result<std::collections::HashSet<String>> {
        let locations = self.list_file_locations_for_asset(asset_id)?;
        let mut session_ids = std::collections::HashSet::new();

        for (_hash, rel_path, volume_id) in &locations {
            // Get the parent directory of this file
            let parent = std::path::Path::new(rel_path)
                .parent()
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_default();
            if parent.is_empty() {
                continue;
            }
            // Go up one more level to get the "session root" (e.g., Capture/2026-02-22/)
            let session_root = std::path::Path::new(&parent)
                .parent()
                .map(|p| {
                    let s = p.to_string_lossy().to_string();
                    if s.is_empty() { parent.clone() } else { s }
                })
                .unwrap_or_else(|| parent.clone());
            let prefix = format!("{}%", session_root);

            let mut stmt = self.conn.prepare(
                "SELECT DISTINCT v.asset_id FROM variants v \
                 JOIN file_locations fl ON fl.content_hash = v.content_hash \
                 WHERE fl.volume_id = ?1 AND fl.relative_path LIKE ?2",
            )?;
            let rows = stmt.query_map(rusqlite::params![volume_id, prefix], |row| {
                row.get::<_, String>(0)
            })?;
            for id in rows.flatten() {
                session_ids.insert(id);
            }
        }
        Ok(session_ids)
    }

    /// Update the relative_path for a file location (variant moved on disk).
    pub fn update_file_location_path(
        &self,
        content_hash: &str,
        volume_id: &str,
        old_path: &str,
        new_path: &str,
    ) -> Result<()> {
        let new_path_norm = crate::path_util::forward_slashes(Path::new(new_path));
        let old_path_norm = crate::path_util::forward_slashes(Path::new(old_path));
        let changed = self.conn.execute(
            "UPDATE file_locations SET relative_path = ?1 \
             WHERE content_hash = ?2 AND volume_id = ?3 AND relative_path = ?4",
            rusqlite::params![new_path_norm, content_hash, volume_id, old_path_norm],
        )?;
        if changed == 0 {
            anyhow::bail!(
                "No file location found for hash '{content_hash}' at '{old_path}'"
            );
        }
        Ok(())
    }

    /// Update the relative_path for a recipe (recipe file moved on disk).
    pub fn update_recipe_relative_path(
        &self,
        recipe_id: &str,
        new_path: &str,
    ) -> Result<()> {
        let new_path_norm = crate::path_util::forward_slashes(Path::new(new_path));
        let changed = self.conn.execute(
            "UPDATE recipes SET relative_path = ?1 WHERE id = ?2",
            rusqlite::params![new_path_norm, recipe_id],
        )?;
        if changed == 0 {
            anyhow::bail!("no recipe found with id '{recipe_id}'");
        }
        Ok(())
    }

}

#[cfg(test)]
mod tests {
    use crate::catalog::test_support::*;
    use crate::catalog::*;

    #[test]
    fn list_file_locations_for_asset_works() {
        let catalog = Catalog::open_in_memory().unwrap();
        catalog.initialize().unwrap();

        let asset = crate::models::Asset::new(crate::models::AssetType::Image, "sha256:loctest");
        let asset_id = asset.id.to_string();
        catalog.insert_asset(&asset).unwrap();

        let variant = test_variant(asset.id, "sha256:loctest", "test.jpg");
        catalog.insert_variant(&variant).unwrap();

        let volume = crate::models::Volume::new(
            "test-vol".to_string(),
            std::path::PathBuf::from("/mnt/test"),
            crate::models::VolumeType::Local,
        );
        catalog.ensure_volume(&volume).unwrap();

        let loc = crate::models::FileLocation {
            volume_id: volume.id,
            relative_path: std::path::PathBuf::from("photos/test.jpg"),
            verified_at: None,
        };
        catalog
            .insert_file_location("sha256:loctest", &loc)
            .unwrap();

        let locs = catalog.list_file_locations_for_asset(&asset_id).unwrap();
        assert_eq!(locs.len(), 1);
        assert_eq!(locs[0].0, "sha256:loctest");
        assert_eq!(locs[0].1, "photos/test.jpg");
        assert_eq!(locs[0].2, volume.id.to_string());

        // Non-existent asset returns empty
        let empty = catalog
            .list_file_locations_for_asset("nonexistent")
            .unwrap();
        assert!(empty.is_empty());
    }

    /// Helper to set up a catalog with a volume, asset, variant, and recipe for recipe tests.
    fn setup_recipe_catalog() -> (Catalog, crate::models::Volume, crate::models::Asset, String) {
        let catalog = Catalog::open_in_memory().unwrap();
        catalog.initialize().unwrap();

        let volume = crate::models::Volume::new(
            "vol".to_string(),
            std::path::PathBuf::from("/mnt/vol"),
            crate::models::VolumeType::Local,
        );
        catalog.ensure_volume(&volume).unwrap();

        let asset = crate::models::Asset::new(crate::models::AssetType::Image, "sha256:rectest");
        catalog.insert_asset(&asset).unwrap();

        let variant = test_variant(asset.id, "sha256:rectest", "photo.nef");
        catalog.insert_variant(&variant).unwrap();

        let loc = crate::models::FileLocation {
            volume_id: volume.id,
            relative_path: std::path::PathBuf::from("photos/photo.nef"),
            verified_at: None,
        };
        catalog.insert_file_location("sha256:rectest", &loc).unwrap();

        let recipe_id = uuid::Uuid::new_v4();
        let recipe = crate::models::Recipe {
            id: recipe_id,
            variant_hash: "sha256:rectest".to_string(),
            software: "Adobe/CaptureOne".to_string(),
            recipe_type: crate::models::RecipeType::Sidecar,
            content_hash: "sha256:recipe_old".to_string(),
            location: crate::models::FileLocation {
                volume_id: volume.id,
                relative_path: std::path::PathBuf::from("photos/photo.xmp"),
                verified_at: None,
            },
            pending_writeback: false,
        };
        catalog.insert_recipe(&recipe).unwrap();

        (catalog, volume, asset, recipe_id.to_string())
    }

    #[test]
    fn insert_recipe_preserves_pending_writeback_flag() {
        // Regression for v4.5.15 bug: `insert_recipe` previously omitted
        // `pending_writeback` from its column list, so every catalog
        // write (rebuild-catalog, reimport, sync) silently reset the
        // flag back to the schema default (0), even when the in-memory
        // recipe (loaded from YAML) carried `pending_writeback: true`.
        // YAML and SQLite would then diverge silently — the symptom
        // being `maki writeback --asset <id>` reporting "0 written"
        // while the YAML sidecar still carried `pending_writeback: true`.
        let (catalog, volume, _asset, recipe_id) = setup_recipe_catalog();

        // Recipe starts un-pending (setup creates it with the default).
        catalog.mark_pending_writeback(&recipe_id).unwrap();
        assert_eq!(
            catalog.list_pending_writeback_recipes(None).unwrap().len(),
            1,
            "recipe should be pending after mark"
        );

        // Reload the YAML sidecar's view of the recipe (with pending=true)
        // and re-insert it — simulates rebuild-catalog / reimport.
        let recipe = crate::models::Recipe {
            id: recipe_id.parse().unwrap(),
            variant_hash: "sha256:rectest".to_string(),
            software: "Adobe/CaptureOne".to_string(),
            recipe_type: crate::models::RecipeType::Sidecar,
            content_hash: "sha256:recipe_old".to_string(),
            location: crate::models::FileLocation {
                volume_id: volume.id,
                relative_path: std::path::PathBuf::from("photos/photo.xmp"),
                verified_at: None,
            },
            pending_writeback: true,
        };
        catalog.insert_recipe(&recipe).unwrap();

        // After re-insert, SQLite must still report pending.
        assert_eq!(
            catalog.list_pending_writeback_recipes(None).unwrap().len(),
            1,
            "pending_writeback must survive insert_recipe — \
             v4.5.14-and-earlier reset it to the schema default (0)"
        );
    }

    #[test]
    fn asset_has_recipe_true_when_recipe_attached() {
        let (catalog, _vol, asset, _recipe_id) = setup_recipe_catalog();
        assert!(catalog.asset_has_recipe(&asset.id.to_string()).unwrap());
    }

    #[test]
    fn asset_has_recipe_false_when_only_variants() {
        let catalog = Catalog::open_in_memory().unwrap();
        catalog.initialize().unwrap();

        let asset = crate::models::Asset::new(crate::models::AssetType::Image, "sha256:noxmp");
        catalog.insert_asset(&asset).unwrap();
        let variant = test_variant(asset.id, "sha256:noxmp", "noxmp.jpg");
        catalog.insert_variant(&variant).unwrap();

        assert!(!catalog.asset_has_recipe(&asset.id.to_string()).unwrap());
    }

    #[test]
    fn find_recipe_by_location_returns_match() {
        let (catalog, volume, _, recipe_id) = setup_recipe_catalog();
        let result = catalog
            .find_recipe_by_location(
                "sha256:rectest",
                &volume.id.to_string(),
                "photos/photo.xmp",
            )
            .unwrap();
        assert!(result.is_some());
        let (id, hash) = result.unwrap();
        assert_eq!(id, recipe_id);
        assert_eq!(hash, "sha256:recipe_old");
    }

    #[test]
    fn pending_writeback_mark_and_clear() {
        let (catalog, _volume, _asset, recipe_id) = setup_recipe_catalog();

        // Initially no pending writebacks
        let pending = catalog.list_pending_writeback_recipes(None).unwrap();
        assert!(pending.is_empty());

        // Mark as pending
        catalog.mark_pending_writeback(&recipe_id).unwrap();
        let pending = catalog.list_pending_writeback_recipes(None).unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].0, recipe_id);

        // Clear
        catalog.clear_pending_writeback(&recipe_id).unwrap();
        let pending = catalog.list_pending_writeback_recipes(None).unwrap();
        assert!(pending.is_empty());
    }

    #[test]
    fn pending_writeback_volume_filter() {
        let (catalog, volume, _asset, recipe_id) = setup_recipe_catalog();

        catalog.mark_pending_writeback(&recipe_id).unwrap();

        // Filter by correct volume
        let pending = catalog
            .list_pending_writeback_recipes(Some(&volume.id.to_string()))
            .unwrap();
        assert_eq!(pending.len(), 1);

        // Filter by wrong volume
        let pending = catalog
            .list_pending_writeback_recipes(Some(&uuid::Uuid::nil().to_string()))
            .unwrap();
        assert!(pending.is_empty());
    }

    #[test]
    fn find_recipe_by_location_returns_none() {
        let (catalog, volume, _, _) = setup_recipe_catalog();
        let result = catalog
            .find_recipe_by_location(
                "sha256:rectest",
                &volume.id.to_string(),
                "photos/other.xmp",
            )
            .unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn update_recipe_content_hash_works() {
        let (catalog, volume, _, recipe_id) = setup_recipe_catalog();
        catalog
            .update_recipe_content_hash(&recipe_id, "sha256:recipe_new")
            .unwrap();

        let result = catalog
            .find_recipe_by_location(
                "sha256:rectest",
                &volume.id.to_string(),
                "photos/photo.xmp",
            )
            .unwrap()
            .unwrap();
        assert_eq!(result.1, "sha256:recipe_new");
    }

    #[test]
    fn find_recipe_by_volume_and_path_works() {
        let (catalog, volume, _, recipe_id) = setup_recipe_catalog();
        let result = catalog
            .find_recipe_by_volume_and_path(
                &volume.id.to_string(),
                "photos/photo.xmp",
            )
            .unwrap();
        assert!(result.is_some());
        let (id, hash, variant_hash) = result.unwrap();
        assert_eq!(id, recipe_id);
        assert_eq!(hash, "sha256:recipe_old");
        assert_eq!(variant_hash, "sha256:rectest");

        // Non-existent path returns None
        let result = catalog
            .find_recipe_by_volume_and_path(
                &volume.id.to_string(),
                "photos/nonexistent.xmp",
            )
            .unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn update_recipe_verified_at_works() {
        let (catalog, volume, _, _) = setup_recipe_catalog();
        catalog
            .update_recipe_verified_at(
                "sha256:rectest",
                &volume.id.to_string(),
                "photos/photo.xmp",
            )
            .unwrap();

        let verified: Option<String> = catalog
            .conn
            .query_row(
                "SELECT verified_at FROM recipes WHERE variant_hash = ?1 AND volume_id = ?2 AND relative_path = ?3",
                rusqlite::params!["sha256:rectest", volume.id.to_string(), "photos/photo.xmp"],
                |row| row.get(0),
            )
            .unwrap();
        assert!(verified.is_some());
    }

    #[test]
    fn find_variant_hash_by_stem_and_directory_works() {
        let (catalog, volume, _, _) = setup_recipe_catalog();

        // "photo" stem in "photos" directory should match "photos/photo.nef"
        let result = catalog
            .find_variant_hash_by_stem_and_directory(
                "photo",
                "photos",
                &volume.id.to_string(),
                None,
            )
            .unwrap();
        assert!(result.is_some());
        let (hash, _asset_id) = result.unwrap();
        assert_eq!(hash, "sha256:rectest");

        // Wrong stem returns None
        let result = catalog
            .find_variant_hash_by_stem_and_directory(
                "other",
                "photos",
                &volume.id.to_string(),
                None,
            )
            .unwrap();
        assert!(result.is_none());

        // Wrong directory returns None
        let result = catalog
            .find_variant_hash_by_stem_and_directory(
                "photo",
                "other_dir",
                &volume.id.to_string(),
                None,
            )
            .unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn list_recipe_only_assets_finds_standalone_xmp() {
        let catalog = Catalog::open_in_memory().unwrap();
        catalog.initialize().unwrap();

        // Create an "other" asset with a single xmp variant
        let mut asset = crate::models::Asset::new(crate::models::AssetType::Other, "sha256:xmponly");
        asset.variants.push(test_variant(asset.id, "sha256:xmponly", "DSC_001.xmp"));
        catalog.insert_asset(&asset).unwrap();
        catalog.insert_variant(&asset.variants[0]).unwrap();

        let results = catalog.list_recipe_only_assets(None, None).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].0, asset.id.to_string());
        assert_eq!(results[0].1, "sha256:xmponly");
        assert_eq!(results[0].2, "xmp");
    }

    #[test]
    fn list_recipe_only_assets_ignores_multi_variant() {
        let catalog = Catalog::open_in_memory().unwrap();
        catalog.initialize().unwrap();

        // Asset with 2 variants should not be returned
        let mut asset = crate::models::Asset::new(crate::models::AssetType::Other, "sha256:multi1");
        asset.variants.push(test_variant(asset.id, "sha256:multi1", "DSC_001.xmp"));
        asset.variants.push(crate::models::Variant {
            content_hash: "sha256:multi2".to_string(),
            asset_id: asset.id,
            role: crate::models::VariantRole::Export,
            format: "jpg".to_string(),
            file_size: 5000,
            original_filename: "DSC_001.jpg".to_string(),
            source_metadata: Default::default(),
            locations: vec![],
        });
        catalog.insert_asset(&asset).unwrap();
        catalog.insert_variant(&asset.variants[0]).unwrap();
        catalog.insert_variant(&asset.variants[1]).unwrap();

        let results = catalog.list_recipe_only_assets(None, None).unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn list_recipe_only_assets_ignores_non_recipe() {
        let catalog = Catalog::open_in_memory().unwrap();
        catalog.initialize().unwrap();

        // Single-variant asset with jpg format should not be returned
        let mut asset = crate::models::Asset::new(crate::models::AssetType::Image, "sha256:jpgonly");
        asset.variants.push(crate::models::Variant { file_size: 5000, ..test_variant(asset.id, "sha256:jpgonly", "photo.jpg") });
        catalog.insert_asset(&asset).unwrap();
        catalog.insert_variant(&asset.variants[0]).unwrap();

        let results = catalog.list_recipe_only_assets(None, None).unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn find_asset_ids_by_volume_and_path_prefixes_basic() {
        let catalog = Catalog::open_in_memory().unwrap();
        catalog.initialize().unwrap();

        let volume = crate::models::Volume::new(
            "vol1".to_string(),
            std::path::PathBuf::from("/mnt/vol1"),
            crate::models::VolumeType::Local,
        );
        catalog.ensure_volume(&volume).unwrap();

        let mut asset = crate::models::Asset::new(crate::models::AssetType::Image, "sha256:aaa");
        asset.variants.push(test_variant(asset.id, "sha256:aaa", "DSC_001.ARW"));
        catalog.insert_asset(&asset).unwrap();
        catalog.insert_variant(&asset.variants[0]).unwrap();
        catalog
            .insert_file_location(
                "sha256:aaa",
                &crate::models::FileLocation {
                    volume_id: volume.id,
                    relative_path: std::path::PathBuf::from("session/Capture/DSC_001.ARW"),
                    verified_at: None,
                },
            )
            .unwrap();

        // Prefix "session" should match
        let ids = catalog
            .find_asset_ids_by_volume_and_path_prefixes(
                &volume.id.to_string(),
                &["session".to_string()],
            )
            .unwrap();
        assert_eq!(ids.len(), 1);
        assert_eq!(ids[0], asset.id.to_string());

        // Prefix "other" should not match
        let ids = catalog
            .find_asset_ids_by_volume_and_path_prefixes(
                &volume.id.to_string(),
                &["other".to_string()],
            )
            .unwrap();
        assert!(ids.is_empty());
    }

    #[test]
    fn find_asset_ids_by_volume_and_path_prefixes_wrong_volume() {
        let catalog = Catalog::open_in_memory().unwrap();
        catalog.initialize().unwrap();

        let volume = crate::models::Volume::new(
            "vol1".to_string(),
            std::path::PathBuf::from("/mnt/vol1"),
            crate::models::VolumeType::Local,
        );
        catalog.ensure_volume(&volume).unwrap();

        let mut asset = crate::models::Asset::new(crate::models::AssetType::Image, "sha256:bbb");
        asset.variants.push(crate::models::Variant { file_size: 500, ..test_variant(asset.id, "sha256:bbb", "photo.jpg") });
        catalog.insert_asset(&asset).unwrap();
        catalog.insert_variant(&asset.variants[0]).unwrap();
        catalog
            .insert_file_location(
                "sha256:bbb",
                &crate::models::FileLocation {
                    volume_id: volume.id,
                    relative_path: std::path::PathBuf::from("photos/photo.jpg"),
                    verified_at: None,
                },
            )
            .unwrap();

        // Query with a fake volume ID — should return empty
        let ids = catalog
            .find_asset_ids_by_volume_and_path_prefixes(
                "00000000-0000-0000-0000-000000000000",
                &["photos".to_string()],
            )
            .unwrap();
        assert!(ids.is_empty());
    }

    #[test]
    fn find_asset_ids_by_volume_and_path_prefixes_empty_prefix() {
        let catalog = Catalog::open_in_memory().unwrap();
        catalog.initialize().unwrap();

        let volume = crate::models::Volume::new(
            "vol1".to_string(),
            std::path::PathBuf::from("/mnt/vol1"),
            crate::models::VolumeType::Local,
        );
        catalog.ensure_volume(&volume).unwrap();

        let mut asset = crate::models::Asset::new(crate::models::AssetType::Image, "sha256:ccc");
        asset.variants.push(crate::models::Variant { file_size: 500, ..test_variant(asset.id, "sha256:ccc", "photo.jpg") });
        catalog.insert_asset(&asset).unwrap();
        catalog.insert_variant(&asset.variants[0]).unwrap();
        catalog
            .insert_file_location(
                "sha256:ccc",
                &crate::models::FileLocation {
                    volume_id: volume.id,
                    relative_path: std::path::PathBuf::from("deep/nested/photo.jpg"),
                    verified_at: None,
                },
            )
            .unwrap();

        // Empty prefix matches everything on the volume
        let ids = catalog
            .find_asset_ids_by_volume_and_path_prefixes(
                &volume.id.to_string(),
                &[String::new()],
            )
            .unwrap();
        assert_eq!(ids.len(), 1);
        assert_eq!(ids[0], asset.id.to_string());
    }
}
