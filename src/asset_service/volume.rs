//! `volume` section of `AssetService` — methods extracted from the original
//! 8.9-kLOC asset_service.rs into a multi-file `impl AssetService` block.
//!
//! Free functions and result types live in the parent `asset_service` module.

use super::*;

impl AssetService {
    // ═══ VOLUME OPERATIONS ═══

    /// Remove a volume and all its associated data (locations, recipes, orphaned assets/previews).
    /// Report-only by default; `--apply` executes removal.
    ///
    /// Dual-store order in apply mode (architecture audit F2):
    /// 1. the sidecars (source of truth) drop every location and recipe on
    ///    the volume — idempotent, so a re-run after a failure is safe;
    /// 2. every catalog row change (locations, recipes, the rows of assets
    ///    that became orphaned, the volume row) runs in one transaction;
    /// 3. only after that commit are derived files removed and the volume
    ///    taken out of `volumes.yaml`.
    ///
    /// The registry entry goes last on purpose: if anything before it fails,
    /// the volume stays registered and `volume remove --apply` can simply be
    /// re-run to finish the job.
    pub fn remove_volume(
        &self,
        label: &str,
        apply: bool,
        on_file: impl Fn(&Path, CleanupStatus, Duration),
    ) -> Result<VolumeRemoveResult> {
        let registry = DeviceRegistry::new(&self.catalog_root);
        let catalog = Catalog::open(&self.catalog_root)?;
        let metadata_store = MetadataStore::new(&self.catalog_root);

        let volume = registry.resolve_volume(label)?;
        let vol_id_str = volume.id.to_string();

        let mut result = VolumeRemoveResult {
            volume_label: volume.label.clone(),
            volume_id: vol_id_str.clone(),
            locations: 0,
            locations_removed: 0,
            recipes: 0,
            recipes_removed: 0,
            orphaned_assets: 0,
            removed_assets: 0,
            orphaned_previews: 0,
            removed_previews: 0,
            apply,
            errors: Vec::new(),
        };

        // Gather all locations and recipes on this volume
        let locations = catalog.list_locations_for_volume_under_prefix(&vol_id_str, "")?;
        let recipes = catalog.list_recipes_for_volume_under_prefix(&vol_id_str, "")?;
        result.locations = locations.len();
        result.recipes = recipes.len();

        if apply {
            // 1. Sidecars first. Only entries whose sidecar update succeeded
            //    have their catalog row removed, so both stores keep agreeing.
            let mut location_rows: Vec<(&(String, String), Duration)> = Vec::new();
            for loc in &locations {
                let (content_hash, relative_path) = loc;
                let file_start = Instant::now();
                match self.remove_sidecar_file_location(
                    &metadata_store,
                    &catalog,
                    content_hash,
                    volume.id,
                    relative_path,
                ) {
                    Ok(()) => location_rows.push((loc, file_start.elapsed())),
                    Err(e) => result.errors.push(format!(
                        "Failed to update sidecar for {relative_path}: {e}"
                    )),
                }
            }
            let mut recipe_rows: Vec<(&(String, String, String, String), Duration)> = Vec::new();
            for rec in &recipes {
                let (_recipe_id, _content_hash, variant_hash, relative_path) = rec;
                let file_start = Instant::now();
                match self.remove_sidecar_recipe(
                    &metadata_store,
                    &catalog,
                    variant_hash,
                    volume.id,
                    relative_path,
                ) {
                    Ok(()) => recipe_rows.push((rec, file_start.elapsed())),
                    Err(e) => result.errors.push(format!(
                        "Failed to update sidecar for recipe {relative_path}: {e}"
                    )),
                }
            }

            // 2. Catalog rows in one transaction.
            let purged = catalog.with_tx(|c| {
                for ((content_hash, relative_path), _) in &location_rows {
                    c.delete_file_location(content_hash, &vol_id_str, relative_path)?;
                }
                for ((recipe_id, _, _, _), _) in &recipe_rows {
                    c.delete_recipe(recipe_id)?;
                }
                let orphaned_ids = c.list_orphaned_asset_ids()?;
                let mut purged = Vec::with_capacity(orphaned_ids.len());
                for asset_id in &orphaned_ids {
                    purged.push(self.purge_orphaned_asset_rows(c, asset_id)?);
                }
                c.delete_volume(&vol_id_str)?;
                Ok(purged)
            });
            let purged = match purged {
                Ok(p) => p,
                Err(e) => {
                    result.errors.push(format!(
                        "Catalog update failed and was rolled back; the volume stays \
                         registered, re-run `volume remove --apply` to retry: {e:#}"
                    ));
                    return Ok(result);
                }
            };

            // 3. Post-commit: progress callbacks, orphan side effects.
            for ((_, relative_path), elapsed) in &location_rows {
                result.locations_removed += 1;
                on_file(&PathBuf::from(relative_path), CleanupStatus::Stale, *elapsed);
            }
            for ((_, _, _, relative_path), elapsed) in &recipe_rows {
                result.recipes_removed += 1;
                on_file(&PathBuf::from(relative_path), CleanupStatus::Stale, *elapsed);
            }
            result.orphaned_assets = purged.len();
            let preview_gen = crate::preview::PreviewGenerator::new(
                &self.catalog_root,
                self.verbosity,
                &self.preview_config,
            );
            for asset in &purged {
                let file_start = Instant::now();
                self.purge_orphaned_asset_files(
                    &metadata_store,
                    &preview_gen,
                    asset,
                    &mut result.errors,
                );
                result.removed_assets += 1;
                on_file(
                    &PathBuf::from(&asset.asset_id),
                    CleanupStatus::OrphanedAsset,
                    file_start.elapsed(),
                );
            }
        } else {
            // Report mode: predict which assets would become orphaned.
            let stale_locations: Vec<(String, String, String)> = locations
                .iter()
                .map(|(hash, path)| (hash.clone(), vol_id_str.clone(), path.clone()))
                .collect();
            result.orphaned_assets = catalog
                .list_would_be_orphaned_asset_ids(&stale_locations)?
                .len();
        }

        // Orphaned previews and smart previews
        let variant_hashes = catalog.list_all_variant_hashes()?;
        scan_orphaned_sharded_files(
            &self.catalog_root.join("previews"),
            |stem| {
                let content_hash = format!("sha256:{stem}");
                variant_hashes.contains(&content_hash)
            },
            apply,
            &mut result.orphaned_previews,
            &mut result.removed_previews,
            &mut result.errors,
            &on_file,
        );
        scan_orphaned_sharded_files(
            &self.catalog_root.join("smart-previews"),
            |stem| {
                let content_hash = format!("sha256:{stem}");
                variant_hashes.contains(&content_hash)
            },
            apply,
            &mut result.orphaned_previews,
            &mut result.removed_previews,
            &mut result.errors,
            &on_file,
        );

        // Finally, take the volume out of volumes.yaml (see the order note above).
        if apply {
            if let Err(e) = registry.remove(label) {
                result.errors.push(format!("Failed to remove volume from registry: {e}"));
            }
        }

        Ok(result)
    }

    /// Combine a source volume into a target volume, rewriting paths.
    ///
    /// The source must be a subdirectory of the target (same physical disk,
    /// deeper mount point). All file_locations and recipes are moved from source
    /// to target with a computed path prefix. In apply mode, sidecars are
    /// updated first (source of truth), then SQLite bulk update, then the
    /// source volume is removed.
    pub fn combine_volume(
        &self,
        source_label: &str,
        target_label: &str,
        apply: bool,
        on_asset: impl Fn(&str, Duration),
    ) -> Result<VolumeCombineResult> {
        let registry = DeviceRegistry::new(&self.catalog_root);
        let catalog = Catalog::open(&self.catalog_root)?;
        let metadata_store = MetadataStore::new(&self.catalog_root);

        let source = registry.resolve_volume(source_label)?;
        let target = registry.resolve_volume(target_label)?;

        let source_id = source.id.to_string();
        let target_id = target.id.to_string();

        if source.id == target.id {
            bail!(
                "Source and target are the same volume ('{}').",
                source.label
            );
        }

        // Compute path prefix: source mount must be under target mount
        let prefix = source
            .mount_point
            .strip_prefix(&target.mount_point)
            .map_err(|_| {
                anyhow::anyhow!(
                    "Source volume '{}' ({}) is not a subdirectory of target volume '{}' ({}). \
                     Cannot compute path prefix.",
                    source.label,
                    source.mount_point.display(),
                    target.label,
                    target.mount_point.display(),
                )
            })?;

        let prefix_str = if prefix.as_os_str().is_empty() {
            String::new()
        } else {
            let mut p = crate::path_util::forward_slashes(prefix);
            if !p.ends_with('/') {
                p.push('/');
            }
            p
        };

        // Count locations and recipes
        let locations = catalog.list_locations_for_volume_under_prefix(&source_id, "")?;
        let recipes = catalog.list_recipes_for_volume_under_prefix(&source_id, "")?;
        let asset_ids = catalog.list_asset_ids_on_volume(&source_id)?;

        let mut result = VolumeCombineResult {
            source_label: source.label.clone(),
            source_id: source_id.clone(),
            target_label: target.label.clone(),
            target_id: target_id.clone(),
            path_prefix: prefix_str.clone(),
            locations: locations.len(),
            locations_moved: 0,
            recipes: recipes.len(),
            recipes_moved: 0,
            assets_affected: asset_ids.len(),
            apply,
            errors: Vec::new(),
        };

        if !apply {
            return Ok(result);
        }

        // --- Apply mode ---

        // 1. Update sidecars (source of truth)
        for asset_id_str in &asset_ids {
            let asset_start = Instant::now();
            let uuid = match asset_id_str.parse::<Uuid>() {
                Ok(u) => u,
                Err(e) => {
                    result
                        .errors
                        .push(format!("Invalid asset UUID {asset_id_str}: {e}"));
                    continue;
                }
            };
            match metadata_store.load(uuid) {
                Ok(mut asset) => {
                    let mut changed = false;

                    // Rewrite variant locations
                    for variant in &mut asset.variants {
                        for loc in &mut variant.locations {
                            if loc.volume_id == source.id {
                                loc.volume_id = target.id;
                                let old_path = loc.relative_path_str();
                                loc.relative_path =
                                    PathBuf::from(format!("{prefix_str}{old_path}"));
                                changed = true;
                            }
                        }
                    }

                    // Rewrite recipe locations
                    for recipe in &mut asset.recipes {
                        if recipe.location.volume_id == source.id {
                            recipe.location.volume_id = target.id;
                            let old_path = recipe.location.relative_path_str();
                            recipe.location.relative_path =
                                PathBuf::from(format!("{prefix_str}{old_path}"));
                            changed = true;
                        }
                    }

                    if changed {
                        if let Err(e) = metadata_store.save(&asset) {
                            result.errors.push(format!(
                                "Failed to save sidecar for asset {asset_id_str}: {e}"
                            ));
                        }
                    }
                    on_asset(asset_id_str, asset_start.elapsed());
                }
                Err(e) => {
                    result
                        .errors
                        .push(format!("Failed to load sidecar for asset {asset_id_str}: {e}"));
                }
            }
        }

        // 2. Catalog rows in one transaction: make sure the target volume row
        //    exists (it may not if nothing was imported onto it), move the
        //    locations and recipes, drop the source volume row.
        let moved = catalog.with_tx(|c| {
            c.ensure_volume(&target)?;
            let locations = c.bulk_move_file_locations(&source_id, &target_id, &prefix_str)?;
            let recipes = c.bulk_move_recipes(&source_id, &target_id, &prefix_str)?;
            c.delete_volume(&source_id)?;
            Ok((locations, recipes))
        });
        match moved {
            Ok((locations, recipes)) => {
                result.locations_moved = locations;
                result.recipes_moved = recipes;
            }
            Err(e) => {
                // Rolled back; the source stays registered so the command can
                // be re-run (the sidecar rewrites above are idempotent).
                result.errors.push(format!(
                    "Catalog update failed and was rolled back; re-run to retry: {e:#}"
                ));
                return Ok(result);
            }
        }

        // 3. Remove the source from volumes.yaml last (same order rule as
        //    `remove_volume`: a registered volume can always be retried).
        if let Err(e) = registry.remove(source_label) {
            result
                .errors
                .push(format!("Failed to remove volume from registry: {e}"));
        }

        Ok(result)
    }

    /// Split a subdirectory from a volume into a new volume.
    ///
    /// The inverse of `combine_volume`: creates a new volume at the source's
    /// mount_point/path, then moves matching file_locations and recipes from
    /// the source to the new volume, stripping the path prefix. The source
    /// volume is NOT removed (it likely has other assets).
    pub fn split_volume(
        &self,
        source_label: &str,
        new_label: &str,
        path: &str,
        purpose: Option<&str>,
        apply: bool,
        on_asset: impl Fn(&str, Duration),
    ) -> Result<VolumeSplitResult> {
        let registry = DeviceRegistry::new(&self.catalog_root);
        let catalog = Catalog::open(&self.catalog_root)?;
        let metadata_store = MetadataStore::new(&self.catalog_root);

        let source = registry.resolve_volume(source_label)?;
        let source_id = source.id.to_string();

        // Normalize prefix: ensure it ends with '/'
        let prefix = if path.ends_with('/') {
            path.to_string()
        } else {
            format!("{path}/")
        };

        // Count matching locations and recipes
        let locations = catalog.list_locations_for_volume_under_prefix(&source_id, &prefix)?;
        let recipes = catalog.list_recipes_for_volume_under_prefix(&source_id, &prefix)?;
        let asset_ids = catalog.list_asset_ids_on_volume_with_prefix(&source_id, &prefix)?;

        // Compute new mount point
        let new_mount = source.mount_point.join(path);

        let mut result = VolumeSplitResult {
            source_label: source.label.clone(),
            source_id: source_id.clone(),
            new_label: new_label.to_string(),
            new_id: String::new(), // filled after registration
            path_prefix: prefix.clone(),
            locations: locations.len(),
            locations_moved: 0,
            recipes: recipes.len(),
            recipes_moved: 0,
            assets_affected: asset_ids.len(),
            apply,
            errors: Vec::new(),
        };

        if !apply {
            return Ok(result);
        }

        // --- Apply mode ---

        // 1. Register the new volume
        use crate::models::volume::{VolumeType, VolumePurpose};
        let vol_purpose = purpose.and_then(VolumePurpose::parse);
        let new_volume = registry.register(new_label, &new_mount, VolumeType::Local, vol_purpose)?;
        let new_id = new_volume.id.to_string();
        result.new_id = new_id.clone();

        // 2. Update sidecars (source of truth)
        for asset_id_str in &asset_ids {
            let asset_start = std::time::Instant::now();
            let uuid = match asset_id_str.parse::<uuid::Uuid>() {
                Ok(u) => u,
                Err(e) => {
                    result.errors.push(format!("Invalid asset UUID {asset_id_str}: {e}"));
                    continue;
                }
            };
            match metadata_store.load(uuid) {
                Ok(mut asset) => {
                    let mut changed = false;

                    for variant in &mut asset.variants {
                        for loc in &mut variant.locations {
                            if loc.volume_id == source.id {
                                let rel = loc.relative_path_str();
                                if let Some(stripped) = rel.strip_prefix(&prefix) {
                                    loc.volume_id = new_volume.id;
                                    loc.relative_path = std::path::PathBuf::from(stripped);
                                    changed = true;
                                }
                            }
                        }
                    }

                    for recipe in &mut asset.recipes {
                        if recipe.location.volume_id == source.id {
                            let rel = recipe.location.relative_path_str();
                            if let Some(stripped) = rel.strip_prefix(&prefix) {
                                recipe.location.volume_id = new_volume.id;
                                recipe.location.relative_path = std::path::PathBuf::from(stripped);
                                changed = true;
                            }
                        }
                    }

                    if changed {
                        if let Err(e) = metadata_store.save(&asset) {
                            result.errors.push(format!("Failed to save sidecar for asset {asset_id_str}: {e}"));
                        }
                    }
                    on_asset(asset_id_str, asset_start.elapsed());
                }
                Err(e) => {
                    result.errors.push(format!("Failed to load sidecar for asset {asset_id_str}: {e}"));
                }
            }
        }

        // 3. Catalog rows in one transaction: register the new volume row,
        //    move matching locations and recipes with the prefix stripped.
        let moved = catalog.with_tx(|c| {
            c.ensure_volume(&new_volume)?;
            let locations = c.bulk_split_file_locations(&source_id, &new_id, &prefix)?;
            let recipes = c.bulk_split_recipes(&source_id, &new_id, &prefix)?;
            Ok((locations, recipes))
        });
        match moved {
            Ok((locations, recipes)) => {
                result.locations_moved = locations;
                result.recipes_moved = recipes;
            }
            Err(e) => result.errors.push(format!(
                "Catalog update failed and was rolled back; run `rebuild-catalog` \
                 or re-run the split to retry: {e:#}"
            )),
        }

        Ok(result)
    }

}
