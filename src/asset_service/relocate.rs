//! `relocate` section of `AssetService` — methods extracted from the original
//! 8.9-kLOC asset_service.rs into a multi-file `impl AssetService` block.
//!
//! Free functions and result types live in the parent `asset_service` module.

use super::*;
use crate::error::MakiError;

impl AssetService {
    // ═══ RELOCATE & UPDATE LOCATION ═══

    /// Relocate all files of an asset to a target volume.
    ///
    /// Copies variant files and recipe files, verifies integrity, updates metadata.
    /// With `remove_source`, deletes source files after successful copy.
    /// With `dry_run`, only reports what would happen.
    ///
    /// Dual-store order (architecture audit F2): files are copied first, then
    /// the new locations are applied to the in-memory `Asset`, the sidecar is
    /// saved (source of truth), and finally every catalog row change runs in
    /// one transaction. A failure in the catalog step leaves the sidecar ahead
    /// of the catalog, which `maki doctor` / `rebuild-catalog` reconcile; a
    /// failure never leaves half of an asset's rows updated.
    ///
    /// Phases: [`plan_relocation`] (pure), then on a [`RelocateRun`] the
    /// copy, the metadata/catalog apply (with optional sidecar creation),
    /// and the source removal.
    pub fn relocate(
        &self,
        asset_id: &str,
        target_volume_label: &str,
        remove_source: bool,
        create_sidecars: bool,
        dry_run: bool,
    ) -> Result<RelocateResult> {
        let catalog = Catalog::open(&self.catalog_root)?;
        let metadata_store = MetadataStore::new(&self.catalog_root);
        let content_store = ContentStore::new(&self.catalog_root);
        let registry = DeviceRegistry::new(&self.catalog_root);

        // Resolve asset
        let full_id = catalog.resolve_asset_id_required(asset_id)?;
        let asset_uuid: Uuid = full_id.parse()?;
        let asset = metadata_store.load(asset_uuid)?;

        // Resolve target volume
        let target_volume = registry.resolve_volume(target_volume_label)?;
        if !target_volume.mount_point.exists() {
            return Err(offline_mount_point(&target_volume).into());
        }

        // Get all volumes for resolving source paths
        let volumes = registry.list()?;
        let plan = plan_relocation(&asset, &target_volume, &volumes)?;

        // Early return if nothing to do
        if plan.is_empty() {
            return Ok(RelocateResult {
                copied: 0,
                skipped: 0,
                removed: 0,
                actions: vec!["All files already on target volume".to_string()],
            });
        }

        let mut run = RelocateRun {
            catalog,
            metadata_store,
            content_store,
            target_volume,
            asset,
            result: RelocateResult {
                copied: 0,
                skipped: 0,
                removed: 0,
                actions: Vec::new(),
            },
        };

        // Dry run: report what would happen
        if dry_run {
            run.preview(&plan, remove_source)?;
            return Ok(run.result);
        }

        // Phase 1: Copy all files (no metadata changes yet)
        run.copy_files(&plan)?;

        // Phase 2 + 2.5: locations, sidecars, catalog rows
        let naming = self.config.writeback.sidecar_naming;
        run.apply_locations(&plan, create_sidecars.then_some(naming))?;

        // Phase 3: Remove sources (only if --remove-source)
        if remove_source {
            run.remove_sources(&plan)?;
        }

        Ok(run.result)
    }

    /// Update a file's location in the catalog after it was moved on disk.
    ///
    /// Looks up the old path as a variant file location or recipe, verifies the
    /// file at `to_path` has the same content hash, and updates catalog + sidecar.
    pub fn update_location(
        &self,
        asset_id: &str,
        from_path: &str,
        to_path: &Path,
        volume_label: Option<&str>,
    ) -> Result<UpdateLocationResult> {
        let catalog = Catalog::open(&self.catalog_root)?;
        let metadata_store = MetadataStore::new(&self.catalog_root);
        let content_store = ContentStore::new(&self.catalog_root);
        let registry = DeviceRegistry::new(&self.catalog_root);

        // Resolve volume from --to path or explicit --volume
        let volume = if let Some(label) = volume_label {
            registry.resolve_volume(label)?
        } else {
            registry.find_volume_for_path(to_path)?
        };
        let volume_id_str = volume.id.to_string();

        // Convert to_path to volume-relative
        let new_relative = to_path
            .strip_prefix(&volume.mount_point)
            .with_context(|| {
                format!(
                    "Path '{}' is not under volume '{}' ({})",
                    to_path.display(),
                    volume.label,
                    volume.mount_point.display()
                )
            })?;
        let new_relative_str = crate::path_util::forward_slashes(new_relative);

        // Convert from_path to volume-relative (strip mount point if absolute)
        let from = Path::new(from_path);
        let old_relative_str = if from.is_absolute() {
            from.strip_prefix(&volume.mount_point)
                .with_context(|| {
                    format!(
                        "Path '{}' is not under volume '{}' ({})",
                        from_path,
                        volume.label,
                        volume.mount_point.display()
                    )
                })
                .map(crate::path_util::forward_slashes)?
        } else {
            from_path.to_string()
        };

        // Resolve asset ID
        let full_id = catalog.resolve_asset_id_required(asset_id)?;

        // Try as variant file location first, then recipe
        if let Some((content_hash, _format)) =
            catalog.find_variant_by_volume_and_path(&volume_id_str, &old_relative_str)?
        {
            // Verify this variant belongs to the resolved asset
            let variant_asset_id = catalog.find_asset_id_by_variant(&content_hash)?;
            if variant_asset_id.as_deref() != Some(&full_id) {
                bail!(
                    "File at '{}' belongs to asset {}, not {}",
                    old_relative_str,
                    variant_asset_id.unwrap_or_else(|| "(unknown)".to_string()),
                    &full_id[..8]
                );
            }

            // Verify file exists at new path
            if !to_path.exists() {
                bail!("file not found at '{}'", to_path.display());
            }

            // Verify content hash matches
            let actual_hash = content_store.hash_file(to_path)?;
            if actual_hash != content_hash {
                bail!(
                    "Hash mismatch: file at '{}' has hash {} but catalog expects {}",
                    to_path.display(),
                    &actual_hash[..16],
                    &content_hash[..16]
                );
            }

            // Update catalog
            catalog.update_file_location_path(
                &content_hash,
                &volume_id_str,
                &old_relative_str,
                &new_relative_str,
            )?;

            // Update sidecar
            self.update_sidecar_file_location_path(
                &metadata_store,
                &catalog,
                &content_hash,
                volume.id,
                &old_relative_str,
                &new_relative_str,
            )?;

            Ok(UpdateLocationResult {
                asset_id: full_id,
                file_type: "variant".to_string(),
                content_hash,
                old_path: old_relative_str,
                new_path: new_relative_str,
                volume_label: volume.label,
            })
        } else if let Some((recipe_id, content_hash, variant_hash)) =
            catalog.find_recipe_by_volume_and_path(&volume_id_str, &old_relative_str)?
        {
            // Verify the recipe's variant belongs to the resolved asset
            let variant_asset_id = catalog.find_asset_id_by_variant(&variant_hash)?;
            if variant_asset_id.as_deref() != Some(&full_id) {
                bail!(
                    "Recipe at '{}' belongs to asset {}, not {}",
                    old_relative_str,
                    variant_asset_id.unwrap_or_else(|| "(unknown)".to_string()),
                    &full_id[..8]
                );
            }

            // Verify file exists at new path
            if !to_path.exists() {
                bail!("file not found at '{}'", to_path.display());
            }

            // Verify content hash matches
            let actual_hash = content_store.hash_file(to_path)?;
            if actual_hash != content_hash {
                bail!(
                    "Hash mismatch: file at '{}' has hash {} but catalog expects {}",
                    to_path.display(),
                    &actual_hash[..16],
                    &content_hash[..16]
                );
            }

            // Update catalog
            catalog.update_recipe_relative_path(&recipe_id, &new_relative_str)?;

            // Update sidecar
            self.update_sidecar_recipe_path(
                &metadata_store,
                &catalog,
                &variant_hash,
                volume.id,
                &old_relative_str,
                &new_relative_str,
            )?;

            Ok(UpdateLocationResult {
                asset_id: full_id,
                file_type: "recipe".to_string(),
                content_hash,
                old_path: old_relative_str,
                new_path: new_relative_str,
                volume_label: volume.label,
            })
        } else {
            bail!(
                "No variant or recipe found at '{}' on volume '{}'",
                old_relative_str,
                volume.label
            );
        }
    }

}

/// Plan the file copies a relocation needs: every variant location and
/// recipe that is not already on the target volume. Pure: reads the asset
/// and the registry snapshot, touches nothing. Fails when a source volume
/// is unknown or offline.
fn plan_relocation(
    asset: &Asset,
    target_volume: &Volume,
    volumes: &[Volume],
) -> Result<Vec<FileCopyPlan>> {
    let find_volume = |vol_id: Uuid| -> Result<Volume> {
        let source_vol = volumes
            .iter()
            .find(|v| v.id == vol_id)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("Source volume {} not found in registry", vol_id))?;
        if !source_vol.mount_point.exists() {
            return Err(offline_mount_point(&source_vol).into());
        }
        Ok(source_vol)
    };

    let mut plan: Vec<FileCopyPlan> = Vec::new();

    // Plan variant file copies
    for variant in &asset.variants {
        for loc in &variant.locations {
            if loc.volume_id == target_volume.id {
                continue; // Already on target
            }
            let source_vol = find_volume(loc.volume_id)?;
            plan.push(FileCopyPlan {
                content_hash: variant.content_hash.clone(),
                source_path: source_vol.mount_point.join(&loc.relative_path),
                target_path: target_volume.mount_point.join(&loc.relative_path),
                kind: FileCopyKind::Variant,
                source_volume_id: loc.volume_id,
                source_relative_path: loc.relative_path.clone(),
            });
        }
    }

    // Plan recipe file copies
    for recipe in &asset.recipes {
        if recipe.location.volume_id == target_volume.id {
            continue; // Already on target
        }
        let source_vol = find_volume(recipe.location.volume_id)?;
        plan.push(FileCopyPlan {
            content_hash: recipe.content_hash.clone(),
            source_path: source_vol.mount_point.join(&recipe.location.relative_path),
            target_path: target_volume.mount_point.join(&recipe.location.relative_path),
            kind: FileCopyKind::Recipe { recipe_id: recipe.id },
            source_volume_id: recipe.location.volume_id,
            source_relative_path: recipe.location.relative_path.clone(),
        });
    }

    Ok(plan)
}

/// One relocation in progress: the stores, the target volume, the asset
/// being moved (its locations are updated in place) and the result under
/// construction.
struct RelocateRun {
    catalog: Catalog,
    metadata_store: MetadataStore,
    content_store: ContentStore,
    target_volume: Volume,
    asset: Asset,
    result: RelocateResult,
}

impl RelocateRun {
    /// Whether the target already holds this entry's exact content.
    fn present_on_target(&self, entry: &FileCopyPlan) -> Result<bool> {
        if !entry.target_path.exists() {
            return Ok(false);
        }
        let existing_hash = self.content_store.hash_file(&entry.target_path)?;
        Ok(existing_hash == entry.content_hash)
    }

    /// Dry run: list what would be copied, moved or skipped.
    fn preview(&mut self, plan: &[FileCopyPlan], remove_source: bool) -> Result<()> {
        for entry in plan {
            if self.present_on_target(entry)? {
                self.result.actions.push(format!(
                    "SKIP {} (already exists with matching hash)",
                    entry.source_relative_path.display()
                ));
                self.result.skipped += 1;
                continue;
            }
            let verb = if remove_source { "MOVE" } else { "COPY" };
            self.result.actions.push(format!(
                "{} {} -> {}",
                verb,
                entry.source_path.display(),
                entry.target_path.display()
            ));
            self.result.copied += 1;
        }
        if remove_source {
            self.result.removed = self.result.copied;
        }
        Ok(())
    }

    /// Phase 1: copy every planned file, verifying each copy's hash.
    fn copy_files(&mut self, plan: &[FileCopyPlan]) -> Result<()> {
        for entry in plan {
            if self.present_on_target(entry)? {
                self.result.actions.push(format!(
                    "Skipped {} (already exists on target)",
                    entry.source_relative_path.display()
                ));
                self.result.skipped += 1;
                continue;
            }
            self.content_store
                .copy_and_verify(&entry.source_path, &entry.target_path, &entry.content_hash)
                .with_context(|| format!(
                    "Failed to copy {} to {}",
                    entry.source_path.display(),
                    entry.target_path.display()
                ))?;
            self.result.actions.push(format!(
                "Copied {} -> {}",
                entry.source_relative_path.display(),
                self.target_volume.label
            ));
            self.result.copied += 1;
        }
        Ok(())
    }

    /// Phase 2: apply the new locations to the in-memory asset and collect
    /// the catalog row changes they imply; Phase 2.5: persist. With
    /// `create_sidecars = Some(naming)`, `create_missing_sidecars` writes
    /// XMP files and inserts their recipe rows itself, so it runs inside
    /// the transaction; the sidecar save (source of truth) precedes the
    /// location/recipe rows, and any row failure rolls every row change
    /// back.
    fn apply_locations(
        &mut self,
        plan: &[FileCopyPlan],
        create_sidecars: Option<crate::config::SidecarNaming>,
    ) -> Result<()> {
        let target_volume = &self.target_volume;
        let asset = &mut self.asset;
        let mut new_locations: Vec<(String, FileLocation)> = Vec::new();
        let mut recipe_moves: Vec<(Uuid, PathBuf)> = Vec::new();

        for entry in plan {
            match &entry.kind {
                FileCopyKind::Variant => {
                    let new_loc = FileLocation {
                        volume_id: target_volume.id,
                        relative_path: entry.source_relative_path.clone(),
                        verified_at: None,
                    };
                    if let Some(variant) = asset.variants.iter_mut().find(|v| v.content_hash == entry.content_hash) {
                        let already_has = variant.locations.iter().any(|l| {
                            l.volume_id == target_volume.id
                                && l.relative_path == entry.source_relative_path
                        });
                        if !already_has {
                            variant.locations.push(new_loc.clone());
                            new_locations.push((entry.content_hash.clone(), new_loc));
                        }
                    }
                }
                FileCopyKind::Recipe { recipe_id } => {
                    if let Some(recipe) = asset.recipes.iter_mut().find(|r| r.id == *recipe_id) {
                        recipe.location.volume_id = target_volume.id;
                        recipe.location.relative_path = entry.source_relative_path.clone();
                    }
                    recipe_moves.push((*recipe_id, entry.source_relative_path.clone()));
                }
            }
        }

        let mut created_here = Vec::new();
        let content_store = &self.content_store;
        let metadata_store = &self.metadata_store;
        self.catalog.with_tx(|c| {
            c.ensure_volume(target_volume)?;

            if let Some(naming) = create_sidecars {
                let online: HashMap<Uuid, PathBuf> =
                    [(target_volume.id, target_volume.mount_point.clone())].into_iter().collect();
                create_missing_sidecars(
                    asset,
                    c,
                    content_store,
                    &online,
                    Some(target_volume.id),
                    naming,
                    true,
                    |path, status| {
                        let rel = path
                            .strip_prefix(&target_volume.mount_point)
                            .unwrap_or(path)
                            .display()
                            .to_string();
                        match status {
                            SidecarStatus::Created => created_here.push(format!("Created sidecar {rel}")),
                            SidecarStatus::ExistsUnregistered => created_here.push(format!(
                                "Skipped sidecar {rel} (file exists on disk, not imported)"
                            )),
                            SidecarStatus::WouldCreate => {}
                        }
                    },
                )?;
            }

            metadata_store.save(asset)?;

            for (content_hash, loc) in &new_locations {
                c.insert_file_location(content_hash, loc)?;
            }
            for (recipe_id, relative_path) in &recipe_moves {
                c.update_recipe_location(
                    &recipe_id.to_string(),
                    &target_volume.id.to_string(),
                    &relative_path.to_string_lossy(),
                )?;
            }
            Ok(())
        })?;
        self.result.copied += created_here.iter().filter(|a| a.starts_with("Created")).count();
        self.result.actions.extend(created_here);
        Ok(())
    }

    /// Phase 3: remove the source files. They are removed one by one;
    /// whatever was removed before a failure is reflected in the sidecar
    /// and the catalog (one transaction) before the error is returned, so
    /// the stores never claim a file that is gone.
    fn remove_sources(&mut self, plan: &[FileCopyPlan]) -> Result<()> {
        let mut removed_entries: Vec<&FileCopyPlan> = Vec::new();
        let mut removal_error: Option<anyhow::Error> = None;
        for entry in plan {
            if entry.source_path.exists() {
                if let Err(e) = std::fs::remove_file(&entry.source_path) {
                    removal_error = Some(anyhow::Error::new(e).context(format!(
                        "Failed to remove source file {}",
                        entry.source_path.display()
                    )));
                    break;
                }
            }
            removed_entries.push(entry);
        }

        for entry in &removed_entries {
            if let FileCopyKind::Variant = entry.kind {
                if let Some(variant) = self.asset.variants.iter_mut().find(|v| v.content_hash == entry.content_hash) {
                    variant.locations.retain(|l| {
                        !(l.volume_id == entry.source_volume_id
                            && l.relative_path == entry.source_relative_path)
                    });
                }
            }
            // Recipe locations were already moved to the target in Phase 2.
        }

        self.metadata_store.save(&self.asset)?;
        self.catalog.with_tx(|c| {
            for entry in &removed_entries {
                if let FileCopyKind::Variant = entry.kind {
                    c.delete_file_location(
                        &entry.content_hash,
                        &entry.source_volume_id.to_string(),
                        &entry.source_relative_path.to_string_lossy(),
                    )?;
                }
            }
            Ok(())
        })?;

        if let Some(e) = removal_error {
            return Err(e);
        }
        self.result.removed = removed_entries.len();

        // Update action messages
        self.result.actions = std::mem::take(&mut self.result.actions)
            .into_iter()
            .map(|a| a.replace("Copied", "Moved"))
            .collect();
        Ok(())
    }
}

/// The typed error for a volume whose mount point is not present
/// (`volume '<label>' is offline (mount point <path> not found)`).
fn offline_mount_point(volume: &Volume) -> MakiError {
    MakiError::offline(
        volume.label.clone(),
        Some(format!("mount point {} not found", volume.mount_point.display())),
    )
}

#[cfg(test)]
mod tests {
    use crate::asset_service::test_support::*;
    use crate::asset_service::*;

    /// Set up a catalog with volumes registered for relocate tests.
    /// Returns (catalog_dir, vol1_dir, vol2_dir, vol1, vol2).
    fn setup_relocate() -> (
        crate::test_support::TempCatalog,
        tempfile::TempDir,
        tempfile::TempDir,
        Volume,
        Volume,
    ) {
        let catalog_dir = crate::test_support::TempCatalog::new();
        let (vol1_dir, vol1) = catalog_dir.add_volume("vol1");
        let (vol2_dir, vol2) = catalog_dir.add_volume("vol2");
        (catalog_dir, vol1_dir, vol2_dir, vol1, vol2)
    }

    #[test]
    fn relocate_copies_variant_to_target_volume() {
        let (catalog_dir, vol1_dir, vol2_dir, vol1, _vol2) = setup_relocate();

        // Create a file on vol1
        std::fs::write(vol1_dir.path().join("photo.jpg"), "photo data").unwrap();

        let service = AssetService::new(catalog_dir.path(), crate::Verbosity::quiet(), &crate::config::PreviewConfig::default());
        service
            .import(
                &[vol1_dir.path().join("photo.jpg")],
                &vol1,
                &default_filter(),
            )
            .unwrap();

        // Get asset ID
        let metadata_store = MetadataStore::new(catalog_dir.path());
        let summaries = metadata_store.list().unwrap();
        let asset_id = summaries[0].id.to_string();

        // Relocate to vol2
        let result = service.relocate(&asset_id, "vol2", false, false, false).unwrap();
        assert_eq!(result.copied, 1);
        assert_eq!(result.removed, 0);

        // Verify file exists on vol2
        assert!(vol2_dir.path().join("photo.jpg").exists());
        // File also still on vol1
        assert!(vol1_dir.path().join("photo.jpg").exists());

        // Verify sidecar has locations on both volumes
        let asset = metadata_store.load(summaries[0].id).unwrap();
        assert_eq!(asset.variants[0].locations.len(), 2);
    }

    #[test]
    fn relocate_with_remove_source_moves_files() {
        let (catalog_dir, vol1_dir, vol2_dir, vol1, _vol2) = setup_relocate();

        std::fs::write(vol1_dir.path().join("photo.jpg"), "move me").unwrap();

        let service = AssetService::new(catalog_dir.path(), crate::Verbosity::quiet(), &crate::config::PreviewConfig::default());
        service
            .import(
                &[vol1_dir.path().join("photo.jpg")],
                &vol1,
                &default_filter(),
            )
            .unwrap();

        let metadata_store = MetadataStore::new(catalog_dir.path());
        let summaries = metadata_store.list().unwrap();
        let asset_id = summaries[0].id.to_string();

        let result = service.relocate(&asset_id, "vol2", true, false, false).unwrap();
        assert_eq!(result.copied, 1);
        assert_eq!(result.removed, 1);

        // File should be on vol2 but not on vol1
        assert!(vol2_dir.path().join("photo.jpg").exists());
        assert!(!vol1_dir.path().join("photo.jpg").exists());

        // Sidecar should have only vol2 location
        let asset = metadata_store.load(summaries[0].id).unwrap();
        assert_eq!(asset.variants[0].locations.len(), 1);
        assert_eq!(asset.variants[0].locations[0].volume_id, _vol2.id);
    }

    #[test]
    fn relocate_copies_recipes_alongside_variants() {
        let (catalog_dir, vol1_dir, vol2_dir, vol1, _vol2) = setup_relocate();

        let photos = vol1_dir.path().join("photos");
        std::fs::create_dir_all(&photos).unwrap();
        std::fs::write(photos.join("DSC.nef"), "raw data for relocate").unwrap();
        std::fs::write(photos.join("DSC.xmp"), "xmp recipe data").unwrap();

        let service = AssetService::new(catalog_dir.path(), crate::Verbosity::quiet(), &crate::config::PreviewConfig::default());
        service
            .import(&[photos], &vol1, &default_filter())
            .unwrap();

        let metadata_store = MetadataStore::new(catalog_dir.path());
        let summaries = metadata_store.list().unwrap();
        let asset_id = summaries[0].id.to_string();

        let result = service.relocate(&asset_id, "vol2", false, false, false).unwrap();
        // Should copy both variant and recipe
        assert_eq!(result.copied, 2);

        // Both files should exist on vol2
        assert!(vol2_dir.path().join("photos/DSC.nef").exists());
        assert!(vol2_dir.path().join("photos/DSC.xmp").exists());
    }

    #[test]
    fn relocate_skips_already_present_files() {
        let (catalog_dir, vol1_dir, _vol2_dir, vol1, _vol2) = setup_relocate();

        std::fs::write(vol1_dir.path().join("photo.jpg"), "skip test").unwrap();

        let service = AssetService::new(catalog_dir.path(), crate::Verbosity::quiet(), &crate::config::PreviewConfig::default());
        service
            .import(
                &[vol1_dir.path().join("photo.jpg")],
                &vol1,
                &default_filter(),
            )
            .unwrap();

        let metadata_store = MetadataStore::new(catalog_dir.path());
        let summaries = metadata_store.list().unwrap();
        let asset_id = summaries[0].id.to_string();

        // First relocate
        service.relocate(&asset_id, "vol2", false, false, false).unwrap();

        // Second relocate — vol1 location still generates a plan entry,
        // but the file already exists on vol2 with matching hash, so it's skipped
        let result = service.relocate(&asset_id, "vol2", false, false, false).unwrap();
        assert_eq!(result.copied, 0);
        assert_eq!(result.skipped, 1);
        assert!(result.actions[0].contains("already exists"));
    }

    #[test]
    fn relocate_dry_run_makes_no_changes() {
        let (catalog_dir, vol1_dir, vol2_dir, vol1, _vol2) = setup_relocate();

        std::fs::write(vol1_dir.path().join("photo.jpg"), "dry run test").unwrap();

        let service = AssetService::new(catalog_dir.path(), crate::Verbosity::quiet(), &crate::config::PreviewConfig::default());
        service
            .import(
                &[vol1_dir.path().join("photo.jpg")],
                &vol1,
                &default_filter(),
            )
            .unwrap();

        let metadata_store = MetadataStore::new(catalog_dir.path());
        let summaries = metadata_store.list().unwrap();
        let asset_id = summaries[0].id.to_string();

        let result = service.relocate(&asset_id, "vol2", false, false, true).unwrap();
        assert_eq!(result.copied, 1);

        // File should NOT exist on vol2
        assert!(!vol2_dir.path().join("photo.jpg").exists());

        // Sidecar should still only have vol1 location
        let asset = metadata_store.load(summaries[0].id).unwrap();
        assert_eq!(asset.variants[0].locations.len(), 1);
    }

    #[test]
    fn relocate_noop_when_already_on_target() {
        let (catalog_dir, vol1_dir, _vol2_dir, vol1, _vol2) = setup_relocate();

        std::fs::write(vol1_dir.path().join("photo.jpg"), "noop test").unwrap();

        let service = AssetService::new(catalog_dir.path(), crate::Verbosity::quiet(), &crate::config::PreviewConfig::default());
        service
            .import(
                &[vol1_dir.path().join("photo.jpg")],
                &vol1,
                &default_filter(),
            )
            .unwrap();

        let metadata_store = MetadataStore::new(catalog_dir.path());
        let summaries = metadata_store.list().unwrap();
        let asset_id = summaries[0].id.to_string();

        // Relocate to same volume
        let result = service.relocate(&asset_id, "vol1", false, false, false).unwrap();
        assert_eq!(result.copied, 0);
        assert_eq!(result.skipped, 0);
        assert!(result.actions[0].contains("already on target"));
    }

    #[test]
    fn relocate_fails_if_target_volume_offline() {
        let (catalog_dir, vol1_dir, _vol2_dir, vol1, _vol2) = setup_relocate();

        std::fs::write(vol1_dir.path().join("photo.jpg"), "offline test").unwrap();

        let service = AssetService::new(catalog_dir.path(), crate::Verbosity::quiet(), &crate::config::PreviewConfig::default());
        service
            .import(
                &[vol1_dir.path().join("photo.jpg")],
                &vol1,
                &default_filter(),
            )
            .unwrap();

        let metadata_store = MetadataStore::new(catalog_dir.path());
        let summaries = metadata_store.list().unwrap();
        let asset_id = summaries[0].id.to_string();

        // Register an offline volume
        let registry = crate::device_registry::DeviceRegistry::new(catalog_dir.path());
        registry
            .register(
                "offline-vol",
                std::path::Path::new("/nonexistent/mount"),
                crate::models::VolumeType::External,
                None,
            )
            .unwrap();

        let err = service
            .relocate(&asset_id, "offline-vol", false, false, false)
            .unwrap_err();
        assert!(err.to_string().contains("offline"));
    }

    #[test]
    fn relocate_fails_for_unknown_asset_id() {
        let (catalog_dir, _vol1_dir, _vol2_dir, _vol1, _vol2) = setup_relocate();

        let service = AssetService::new(catalog_dir.path(), crate::Verbosity::quiet(), &crate::config::PreviewConfig::default());
        let err = service
            .relocate("nonexistent-id", "vol2", false, false, false)
            .unwrap_err();
        assert!(err.to_string().contains("no asset found"));
    }

    #[test]
    fn relocate_fails_for_unknown_volume() {
        let (catalog_dir, vol1_dir, _vol2_dir, vol1, _vol2) = setup_relocate();

        std::fs::write(vol1_dir.path().join("photo.jpg"), "unknown vol test").unwrap();

        let service = AssetService::new(catalog_dir.path(), crate::Verbosity::quiet(), &crate::config::PreviewConfig::default());
        service
            .import(
                &[vol1_dir.path().join("photo.jpg")],
                &vol1,
                &default_filter(),
            )
            .unwrap();

        let metadata_store = MetadataStore::new(catalog_dir.path());
        let summaries = metadata_store.list().unwrap();
        let asset_id = summaries[0].id.to_string();

        let err = service
            .relocate(&asset_id, "nonexistent-vol", false, false, false)
            .unwrap_err();
        assert!(err.to_string().contains("no volume found"));
    }

    /// A catalog failure in the middle of relocate's row changes must roll
    /// every row back (architecture audit F2): the variant location row is
    /// inserted first, then the recipe row update fails because the recipe
    /// row was removed behind the sidecar's back — the location row must
    /// not survive.
    #[test]
    fn relocate_catalog_failure_leaves_catalog_locations_unchanged() {
        let (catalog_dir, vol1_dir, vol2_dir, vol1, vol2) = setup_relocate();

        let photos = vol1_dir.path().join("photos");
        std::fs::create_dir_all(&photos).unwrap();
        std::fs::write(photos.join("DSC.nef"), "raw data for rollback").unwrap();
        std::fs::write(photos.join("DSC.xmp"), "xmp recipe for rollback").unwrap();

        let service = AssetService::new(catalog_dir.path(), crate::Verbosity::quiet(), &crate::config::PreviewConfig::default());
        service.import(&[photos], &vol1, &default_filter()).unwrap();

        let metadata_store = MetadataStore::new(catalog_dir.path());
        let summaries = metadata_store.list().unwrap();
        let asset = metadata_store.load(summaries[0].id).unwrap();
        let asset_id = asset.id.to_string();
        let variant_hash = asset.variants[0].content_hash.clone();
        assert_eq!(asset.recipes.len(), 1);

        // Inject the failure: drop the recipe row from the catalog only.
        let catalog = crate::catalog::Catalog::open(catalog_dir.path()).unwrap();
        assert!(catalog.delete_recipe(&asset.recipes[0].id.to_string()).unwrap());
        let before = catalog.get_variant_file_locations(&variant_hash).unwrap();
        assert_eq!(before.len(), 1);

        let err = service
            .relocate(&asset_id, "vol2", false, false, false)
            .unwrap_err();
        assert!(err.to_string().contains("no recipe found"), "{err:#}");

        // Files were copied (phase 1 precedes any metadata change) ...
        assert!(vol2_dir.path().join("photos/DSC.nef").exists());
        // ... the sidecar (source of truth) is ahead of the catalog ...
        let after_sidecar = metadata_store.load(summaries[0].id).unwrap();
        assert_eq!(after_sidecar.variants[0].locations.len(), 2);
        // ... and the catalog rows are exactly as before: rolled back.
        let after = catalog.get_variant_file_locations(&variant_hash).unwrap();
        assert_eq!(after, before);
        assert!(!after.iter().any(|(vol, _)| *vol == vol2.id.to_string()));
    }

    /// Typed errors (architecture audit F19): an offline target classifies
    /// as `MakiError::Offline`, an unknown volume as `MakiError::NotFound`.
    #[test]
    fn relocate_errors_are_typed() {
        let (catalog_dir, vol1_dir, _vol2_dir, vol1, _vol2) = setup_relocate();
        std::fs::write(vol1_dir.path().join("photo.jpg"), "typed errors").unwrap();
        let service = AssetService::new(catalog_dir.path(), crate::Verbosity::quiet(), &crate::config::PreviewConfig::default());
        service
            .import(&[vol1_dir.path().join("photo.jpg")], &vol1, &default_filter())
            .unwrap();
        let asset_id = MetadataStore::new(catalog_dir.path()).list().unwrap()[0].id.to_string();

        let registry = crate::device_registry::DeviceRegistry::new(catalog_dir.path());
        registry
            .register("offline-vol", std::path::Path::new("/nonexistent/mount"), crate::models::VolumeType::External, None)
            .unwrap();

        let err = service.relocate(&asset_id, "offline-vol", false, false, false).unwrap_err();
        assert!(matches!(
            crate::error::classify(&err),
            Some(crate::error::MakiError::Offline { volume, .. }) if volume == "offline-vol"
        ), "{err:#}");
        assert_eq!(err.to_string(), "volume 'offline-vol' is offline (mount point /nonexistent/mount not found)");

        let err = service.relocate(&asset_id, "nonexistent-vol", false, false, false).unwrap_err();
        assert!(matches!(
            crate::error::classify(&err),
            Some(crate::error::MakiError::NotFound { kind: "volume", .. })
        ), "{err:#}");
    }

    #[test]
    fn update_location_follows_a_file_moved_by_hand() {
        let env = ServiceEnv::new();
        let id = env.add("a.jpg", "moved by hand");
        std::fs::create_dir_all(env.abs("sorted")).unwrap();
        std::fs::rename(env.abs("a.jpg"), env.abs("sorted/a.jpg")).unwrap();

        let result = env.service.update_location(&id, "a.jpg", &env.abs("sorted/a.jpg"), None).unwrap();
        assert_eq!((result.old_path.as_str(), result.new_path.as_str()), ("a.jpg", "sorted/a.jpg"));
        assert_eq!(env.location_paths(&id), ["sorted/a.jpg"]);
        assert_eq!(env.location_rows(&id), 1);
    }

    #[test]
    fn update_location_rejects_a_file_with_different_content() {
        let env = ServiceEnv::new();
        let id = env.add("a.jpg", "original bytes");
        env.write("other.jpg", "different bytes");

        assert!(env.service.update_location(&id, "a.jpg", &env.abs("other.jpg"), None).is_err());
        assert_eq!(env.location_paths(&id), ["a.jpg"]);
    }
}
