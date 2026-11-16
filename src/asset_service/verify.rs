//! `verify` section of `AssetService` — methods extracted from the original
//! 8.9-kLOC asset_service.rs into a multi-file `impl AssetService` block.
//!
//! Free functions and result types live in the parent `asset_service` module.

use super::*;

impl AssetService {
    // ═══ VERIFY ═══

    /// Verify file integrity by re-hashing files and comparing against stored content hashes.
    ///
    /// Two modes:
    /// - **Path mode** (`paths` non-empty): verify specific files/directories on disk.
    /// - **Catalog mode** (`paths` empty): verify all known file locations, optionally
    ///   filtered by `volume_filter` or `asset_filter`.
    pub fn verify(
        &self,
        paths: &[PathBuf],
        volume_filter: Option<&str>,
        asset_filter: Option<&str>,
        filter: &FileTypeFilter,
        max_age_days: Option<u64>,
        on_file: impl Fn(&Path, VerifyStatus, Duration),
    ) -> Result<VerifyResult> {
        let registry = DeviceRegistry::new(&self.catalog_root);
        let mut run = VerifyRun {
            service: self,
            content_store: ContentStore::new(&self.catalog_root),
            metadata_store: MetadataStore::new(&self.catalog_root),
            catalog: Catalog::open(&self.catalog_root)?,
            volumes: registry.list()?,
            max_age_days,
            result: VerifyResult {
                verified: 0,
                failed: 0,
                modified: 0,
                skipped: 0,
                skipped_recent: 0,
                errors: Vec::new(),
            },
            on_file,
        };

        if !paths.is_empty() {
            // Path mode
            let files = resolve_files(paths, &[]);
            for file_path in &files {
                run.verify_file(file_path, filter)?;
            }
        } else {
            // Catalog mode
            let volume_filter_resolved = match volume_filter {
                Some(label) => Some(registry.resolve_volume(label)?),
                None => None,
            };
            let assets = run.catalog_mode_assets(asset_filter, volume_filter_resolved.as_ref())?;
            for asset in &assets {
                run.verify_asset(asset, volume_filter_resolved.as_ref())?;
            }
        }

        Ok(run.result)
    }

    /// Update the `verified_at` timestamp in the sidecar YAML for a specific file location.
    fn update_sidecar_verified_at(
        &self,
        metadata_store: &MetadataStore,
        catalog: &Catalog,
        content_hash: &str,
        volume_id: Uuid,
        relative_path: &Path,
    ) -> Result<()> {
        let asset_id = match catalog.find_asset_id_by_variant(content_hash)? {
            Some(id) => id,
            None => return Ok(()),
        };
        let uuid: Uuid = asset_id.parse()?;
        let mut asset = metadata_store.load(uuid)?;
        let now = chrono::Utc::now();

        let mut changed = false;
        for variant in &mut asset.variants {
            if variant.content_hash == content_hash {
                for loc in &mut variant.locations {
                    if loc.volume_id == volume_id && loc.relative_path == relative_path {
                        loc.verified_at = Some(now);
                        changed = true;
                    }
                }
            }
        }

        if changed {
            metadata_store.save(&asset)?;
        }

        Ok(())
    }

    /// Update the sidecar YAML with a recipe's verified_at timestamp.
    fn update_sidecar_recipe_verified_at(
        &self,
        metadata_store: &MetadataStore,
        catalog: &Catalog,
        variant_hash: &str,
        volume_id: Uuid,
        relative_path: &Path,
    ) -> Result<()> {
        let asset_id = match catalog.find_asset_id_by_variant(variant_hash)? {
            Some(id) => id,
            None => return Ok(()),
        };
        let uuid: Uuid = asset_id.parse()?;
        let mut asset = metadata_store.load(uuid)?;
        let now = chrono::Utc::now();

        let mut changed = false;
        for recipe in &mut asset.recipes {
            if recipe.location.volume_id == volume_id
                && recipe.location.relative_path == relative_path
            {
                recipe.location.verified_at = Some(now);
                changed = true;
            }
        }

        if changed {
            metadata_store.save(&asset)?;
        }

        Ok(())
    }
}

/// One `verify` in progress.
struct VerifyRun<'a, F> {
    service: &'a AssetService,
    content_store: ContentStore,
    metadata_store: MetadataStore,
    catalog: Catalog,
    volumes: Vec<Volume>,
    max_age_days: Option<u64>,
    result: VerifyResult,
    on_file: F,
}

impl<F: Fn(&Path, VerifyStatus, Duration)> VerifyRun<'_, F> {
    /// Path mode, one file: hash it and match the hash against the
    /// catalog's variants and recipes; a recipe at this location with a
    /// different hash was modified and is re-read.
    fn verify_file(&mut self, file_path: &Path, filter: &FileTypeFilter) -> Result<()> {
        let file_start = std::time::Instant::now();

        // Skip files whose extension isn't in an enabled type group
        let ext = file_path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("");
        if !ext.is_empty() && !filter.is_importable(ext) {
            return Ok(());
        }

        // Find which volume this file is on
        let Some(volume) = self.volumes.iter().find(|v| file_path.starts_with(&v.mount_point)) else {
            self.result.skipped += 1;
            self.result.errors.push(format!(
                "no volume found for {}",
                file_path.display()
            ));
            (self.on_file)(file_path, VerifyStatus::Skipped, file_start.elapsed());
            return Ok(());
        };

        let relative_path = file_path
            .strip_prefix(&volume.mount_point)
            .unwrap_or(file_path);
        let volume_id_str = volume.id.to_string();
        let relative_str = crate::path_util::forward_slashes(relative_path);

        // Skip recently verified files
        if let Some(days) = self.max_age_days {
            let verified_at = self
                .catalog
                .get_location_verified_at(&volume_id_str, &relative_str)?;
            if is_recently_verified(verified_at.as_deref(), days) {
                self.result.skipped_recent += 1;
                (self.on_file)(file_path, VerifyStatus::SkippedRecent, file_start.elapsed());
                return Ok(());
            }
        }

        // Hash the file
        let hash = match self.content_store.hash_file(file_path) {
            Ok(h) => h,
            Err(e) => {
                self.result.skipped += 1;
                self.result.errors.push(format!(
                    "{}: {}",
                    file_path.display(),
                    e
                ));
                (self.on_file)(file_path, VerifyStatus::Missing, file_start.elapsed());
                return Ok(());
            }
        };

        // Look up variant by hash
        if self.catalog.find_asset_id_by_variant(&hash)?.is_some() {
            // File matches a known variant — verified
            self.result.verified += 1;
            self.catalog
                .update_verified_at(&hash, &volume_id_str, &relative_str)?;
            // Also update sidecar verified_at
            self.service.update_sidecar_verified_at(
                &self.metadata_store,
                &self.catalog,
                &hash,
                volume.id,
                relative_path,
            )?;
            (self.on_file)(file_path, VerifyStatus::Ok, file_start.elapsed());
        } else if self.catalog.has_recipe_by_content_hash(&hash)? {
            // Not a variant — a known recipe file by hash
            self.result.verified += 1;
            self.catalog
                .update_recipe_verified_at(&hash, &volume_id_str, &relative_str)?;
            (self.on_file)(file_path, VerifyStatus::Ok, file_start.elapsed());
        } else if let Some((recipe_id, _old_hash, variant_hash)) = self
            .catalog
            .find_recipe_by_volume_and_path(&volume_id_str, &relative_str)?
        {
            // Recipe at this location has a different hash — modified.
            // Sidecar (via the variant's owning asset) first, then
            // the catalog rows in one transaction.
            self.service.apply_modified_recipe(
                &self.catalog,
                &self.metadata_store,
                &recipe_id,
                &hash,
                &variant_hash,
                volume,
                file_path,
                &relative_str,
            )?;
            self.result.modified += 1;
            (self.on_file)(file_path, VerifyStatus::Modified, file_start.elapsed());
        } else {
            self.result.skipped += 1;
            self.result.errors.push(format!(
                "Untracked: {}",
                file_path.display()
            ));
            (self.on_file)(file_path, VerifyStatus::Untracked, file_start.elapsed());
        }
        Ok(())
    }

    /// Catalog mode: the assets to verify — one asset, the assets with
    /// locations older than `max_age_days` (counting the rest as skipped),
    /// or every asset.
    fn catalog_mode_assets(
        &mut self,
        asset_filter: Option<&str>,
        volume_filter: Option<&Volume>,
    ) -> Result<Vec<Asset>> {
        if let Some(asset_id) = asset_filter {
            let full_id = self.catalog.resolve_asset_id_required(asset_id)?;
            let uuid: Uuid = full_id.parse()?;
            return Ok(vec![self.metadata_store.load(uuid)?]);
        }
        if let Some(days) = self.max_age_days {
            // Fast path: query SQLite for asset IDs with stale locations,
            // then load only those sidecars. Avoids loading all 260k+ YAMLs.
            let catalog = &self.catalog;
            let vol_filter = volume_filter.map(|v| v.id.to_string());
            let stale_ids = catalog.find_assets_with_stale_locations(days, vol_filter.as_deref())?;
            // Count skipped locations: total (file_locations + recipes) minus stale
            let total_fl = catalog.count_file_locations(vol_filter.as_deref())?;
            let stale_fl = catalog.count_stale_locations(days, vol_filter.as_deref())?;
            let total_recipes = catalog.count_recipes(vol_filter.as_deref())?;
            // File locations that are recent + all recipes for recently-verified assets
            let stale_asset_count = stale_ids.len();
            let total_assets = catalog.count_assets()? as usize;
            let recent_recipes = if total_assets > 0 && stale_asset_count < total_assets {
                // Proportional estimate of recipe locations for recently-verified assets
                total_recipes.saturating_sub(
                    (total_recipes as f64 * stale_asset_count as f64 / total_assets as f64) as usize
                )
            } else {
                0
            };
            self.result.skipped_recent = total_fl.saturating_sub(stale_fl) + recent_recipes;
            if self.service.verbosity.verbose {
                eprintln!("  Verify: {} asset(s) with stale locations, {} location(s) skipped as recent",
                    stale_ids.len(), self.result.skipped_recent);
            }
            return Ok(stale_ids
                .iter()
                .filter_map(|id| id.parse::<Uuid>().ok())
                .filter_map(|uuid| self.metadata_store.load(uuid).ok())
                .collect());
        }
        let summaries = self.metadata_store.list()?;
        summaries
            .iter()
            .map(|s| self.metadata_store.load(s.id))
            .collect::<Result<Vec<_>>>()
    }

    /// Catalog mode, one asset: every variant location and every recipe.
    fn verify_asset(&mut self, asset: &Asset, volume_filter: Option<&Volume>) -> Result<()> {
        // Verify variant file locations
        for variant in &asset.variants {
            for loc in &variant.locations {
                self.verify_location(volume_filter, &variant.content_hash, loc, None)?;
            }
        }

        // Verify recipe file locations
        for recipe in &asset.recipes {
            self.verify_location(
                volume_filter,
                &recipe.content_hash,
                &recipe.location,
                Some(&recipe.variant_hash),
            )?;
        }
        Ok(())
    }

    /// Verify a single file location (used by catalog mode).
    fn verify_location(
        &mut self,
        volume_filter: Option<&Volume>,
        content_hash: &str,
        loc: &FileLocation,
        recipe_variant_hash: Option<&str>,
    ) -> Result<()> {
        let file_start = std::time::Instant::now();

        // Apply volume filter
        if let Some(filter_vol) = volume_filter {
            if loc.volume_id != filter_vol.id {
                return Ok(());
            }
        }

        // Skip recently verified files
        if let Some(days) = self.max_age_days {
            if let Some(ref verified_at) = loc.verified_at {
                let age = chrono::Utc::now() - *verified_at;
                if age.num_days() < days as i64 {
                    self.result.skipped_recent += 1;
                    (self.on_file)(&loc.relative_path, VerifyStatus::SkippedRecent, file_start.elapsed());
                    return Ok(());
                }
            }
        }

        // Find the volume
        let volume = match self.volumes.iter().find(|v| v.id == loc.volume_id) {
            Some(v) => v,
            None => {
                self.result.skipped += 1;
                self.result.errors.push(format!(
                    "Volume {} not found for {}",
                    loc.volume_id,
                    loc.relative_path.display()
                ));
                (self.on_file)(&loc.relative_path, VerifyStatus::Skipped, file_start.elapsed());
                return Ok(());
            }
        };

        // Skip offline volumes
        if !volume.is_online {
            self.result.skipped += 1;
            (self.on_file)(&loc.relative_path, VerifyStatus::Skipped, file_start.elapsed());
            return Ok(());
        }

        let full_path = volume.mount_point.join(&loc.relative_path);

        if !full_path.exists() {
            self.result.skipped += 1;
            self.result.errors.push(format!(
                "Missing: {} ({}:{})",
                full_path.display(),
                volume.label,
                loc.relative_path.display()
            ));
            (self.on_file)(&full_path, VerifyStatus::Missing, file_start.elapsed());
            return Ok(());
        }

        match self.content_store.verify(content_hash, &full_path) {
            Ok(true) => {
                self.result.verified += 1;
                if let Some(variant_hash) = recipe_variant_hash {
                    self.catalog.update_recipe_verified_at(
                        variant_hash,
                        &volume.id.to_string(),
                        &loc.relative_path_str(),
                    )?;
                    self.service.update_sidecar_recipe_verified_at(
                        &self.metadata_store,
                        &self.catalog,
                        variant_hash,
                        loc.volume_id,
                        &loc.relative_path,
                    )?;
                } else {
                    self.catalog.update_verified_at(
                        content_hash,
                        &volume.id.to_string(),
                        &loc.relative_path_str(),
                    )?;
                    self.service.update_sidecar_verified_at(
                        &self.metadata_store,
                        &self.catalog,
                        content_hash,
                        volume.id,
                        &loc.relative_path,
                    )?;
                }
                (self.on_file)(&full_path, VerifyStatus::Ok, file_start.elapsed());
            }
            Ok(false) => {
                if let Some(variant_hash) = recipe_variant_hash {
                    // Recipe files are expected to change — report as modified, not failed
                    self.reread_modified_recipe(volume, loc, variant_hash, &full_path)?;
                    self.result.modified += 1;
                    (self.on_file)(&full_path, VerifyStatus::Modified, file_start.elapsed());
                } else {
                    self.result.failed += 1;
                    self.result.errors.push(format!(
                        "FAILED: {} ({}:{})",
                        full_path.display(),
                        volume.label,
                        loc.relative_path.display()
                    ));
                    (self.on_file)(&full_path, VerifyStatus::Mismatch, file_start.elapsed());
                }
            }
            Err(e) => {
                self.result.skipped += 1;
                self.result.errors.push(format!(
                    "Error reading {}: {}",
                    full_path.display(),
                    e
                ));
                (self.on_file)(&full_path, VerifyStatus::Missing, file_start.elapsed());
            }
        }

        Ok(())
    }

    /// A recipe whose file changed: re-hash it, update the recipe in the
    /// sidecar (re-reading XMP metadata when applicable) and then the
    /// catalog rows in one transaction.
    fn reread_modified_recipe(
        &self,
        volume: &Volume,
        loc: &FileLocation,
        variant_hash: &str,
        full_path: &Path,
    ) -> Result<()> {
        let new_hash = self.content_store.hash_file(full_path)?;

        // Sidecar (source of truth) first, then the catalog
        // rows in one transaction.
        let mut xmp_asset: Option<Asset> = None;
        if let Some(asset_id) = self.catalog.find_asset_id_by_variant(variant_hash)? {
            let uuid: Uuid = asset_id.parse()?;
            let mut asset = self.metadata_store.load(uuid)?;
            if let Some(recipe) = asset.recipes.iter_mut().find(|r| {
                r.location.volume_id == loc.volume_id
                    && r.location.relative_path == loc.relative_path
            }) {
                recipe.content_hash = new_hash.clone();

                // Re-extract XMP data if applicable
                let ext = loc.relative_path.extension()
                    .and_then(|e| e.to_str())
                    .unwrap_or("");
                let is_xmp = ext.eq_ignore_ascii_case("xmp");
                if is_xmp {
                    let xmp = crate::xmp_reader::extract(full_path);
                    xmp.reapply_to(&mut asset, variant_hash);
                }

                self.metadata_store.save(&asset)?;
                if is_xmp {
                    xmp_asset = Some(asset);
                }
            }
        }

        let recipe_row = self.catalog.find_recipe_by_volume_and_path(
            &volume.id.to_string(),
            &loc.relative_path_str(),
        )?;
        self.catalog.with_tx(|c| {
            if let Some((recipe_id, _, _)) = &recipe_row {
                c.update_recipe_content_hash(recipe_id, &new_hash)?;
            }
            if let Some(asset) = &xmp_asset {
                c.insert_asset(asset)?;
                if let Some(v) = asset.variants.iter().find(|v| v.content_hash == variant_hash) {
                    c.insert_variant(v)?;
                }
            }
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use crate::asset_service::test_support::*;
    use crate::asset_service::*;

    #[test]
    fn verify_passes_intact_files_and_reports_corruption() {
        let env = ServiceEnv::new();
        env.add("a.jpg", "intact");
        env.add("b.jpg", "will change");

        let all = env.service.verify(&[], None, None, &FileTypeFilter::default(), None, |_, _, _| {}).unwrap();
        assert_eq!((all.verified, all.failed), (2, 0), "{all:?}");

        env.write("b.jpg", "bit rot");
        let after = env.service.verify(&[], None, None, &FileTypeFilter::default(), None, |_, _, _| {}).unwrap();
        assert_eq!((after.verified, after.failed), (1, 1), "{after:?}");
    }

    #[test]
    fn verify_max_age_skips_recently_verified_files() {
        let env = ServiceEnv::new();
        env.add("a.jpg", "intact");
        env.service.verify(&[], None, None, &FileTypeFilter::default(), None, |_, _, _| {}).unwrap();

        let again = env.service.verify(&[], None, None, &FileTypeFilter::default(), Some(30), |_, _, _| {}).unwrap();
        assert_eq!((again.verified, again.skipped_recent), (0, 1), "{again:?}");
    }
}
