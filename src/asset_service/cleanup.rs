//! `cleanup` section of `AssetService` — methods extracted from the original
//! 8.9-kLOC asset_service.rs into a multi-file `impl AssetService` block.
//!
//! Free functions and result types live in the parent `asset_service` module.

use super::*;

impl AssetService {
    // ═══ CLEANUP & DELETE ═══

    /// Scan all file locations and recipes across online volumes, checking for files
    /// that no longer exist on disk. Optionally remove stale records.
    ///
    /// Also scans for orphaned derived files (previews, smart previews, embeddings,
    /// face crops) and removes them.
    ///
    /// Passes, each a method of [`CleanupRun`]: stale locations and recipes
    /// per online volume (1), locationless variants (2), orphaned assets
    /// (3), and — unless the run is scoped to a volume or path — orphaned
    /// derived files under the catalog directory (4–7).
    pub fn cleanup(
        &self,
        volume_filter: Option<&str>,
        path_prefix: Option<&str>,
        apply: bool,
        on_file: impl Fn(&Path, CleanupStatus, Duration),
    ) -> Result<CleanupResult> {
        let registry = DeviceRegistry::new(&self.catalog_root);
        let volumes = if let Some(label) = volume_filter {
            vec![registry.resolve_volume(label)?]
        } else {
            registry.list()?
        };

        let mut run = CleanupRun {
            service: self,
            catalog: Catalog::open(&self.catalog_root)?,
            metadata_store: MetadataStore::new(&self.catalog_root),
            apply,
            result: CleanupResult {
                checked: 0,
                stale: 0,
                removed: 0,
                skipped_offline: 0,
                locationless_variants: 0,
                removed_variants: 0,
                orphaned_assets: 0,
                removed_assets: 0,
                orphaned_previews: 0,
                removed_previews: 0,
                orphaned_smart_previews: 0,
                removed_smart_previews: 0,
                orphaned_embeddings: 0,
                removed_embeddings: 0,
                orphaned_face_files: 0,
                removed_face_files: 0,
                errors: Vec::new(),
                skipped_global_passes: false,
            },
            stale_locations: Vec::new(),
            on_file,
        };

        // Pass 1: stale file locations and recipes, per volume
        let prefix = path_prefix.unwrap_or("");
        for volume in &volumes {
            if !volume.is_online {
                run.result.skipped_offline += 1;
                (run.on_file)(&volume.mount_point, CleanupStatus::Offline, Duration::ZERO);
                continue;
            }
            run.check_volume_locations(volume, prefix)?;
        }

        // Pass 2: Locationless variants (variant has no locations but asset has other located variants)
        run.remove_locationless_variants()?;

        // Pass 3: Orphaned assets (all variants have zero file_locations)
        run.remove_orphaned_assets()?;

        // Passes 4-7 scan all files under `<catalog_root>/{previews,smart-previews,
        // embeddings,faces}` against the entire catalog — they cannot be meaningfully
        // restricted to a single volume or path prefix, since orphaned derived files
        // live in shared catalog directories, not under a specific volume mount.
        // When the user scoped the run with `--volume` or `--path`, skip the global
        // passes so counts in the summary don't mix path-scoped and catalog-wide numbers.
        let scope_restricted = volume_filter.is_some() || path_prefix.is_some();
        if scope_restricted {
            run.result.skipped_global_passes = true;
            return Ok(run.result);
        }
        run.remove_orphaned_derived_files()?;

        Ok(run.result)
    }

    /// Delete assets from the catalog. Report-only by default; `apply` executes deletion.
    /// `remove_files` (requires `apply`) also removes physical media and recipe
    /// files from disk — moved into `<catalog_root>/.trash/` unless the trash
    /// is disabled (`[trash] enabled = false` or `no_trash`), in which case
    /// they are deleted permanently.
    pub fn delete_assets(
        &self,
        asset_ids: &[String],
        apply: bool,
        remove_files: bool,
        no_trash: bool,
        on_asset: impl Fn(&str, &DeleteStatus, Duration),
    ) -> Result<DeleteResult> {
        let catalog = Catalog::open(&self.catalog_root)?;
        let metadata_store = MetadataStore::new(&self.catalog_root);
        let registry = DeviceRegistry::new(&self.catalog_root);
        let trash = crate::trash::Trash::new_with(&self.catalog_root, &self.config.trash, no_trash);
        let preview_gen = crate::preview::PreviewGenerator::new(
            &self.catalog_root,
            self.verbosity,
            &self.preview_config,
        );

        // Build volume lookup for file deletion
        let volumes = registry.list().unwrap_or_default();
        let volume_map: std::collections::HashMap<String, &Volume> = volumes
            .iter()
            .map(|v| (v.id.to_string(), v))
            .collect();

        // Stack and collection memberships: the stores persist their YAML
        // inside the per-asset transaction below (audit F6); nothing is
        // saved after the loop.
        let stack_store = crate::stack::StackStore::open(catalog.conn(), &self.catalog_root);
        let col_store = crate::collection::CollectionStore::open(catalog.conn(), &self.catalog_root);

        let mut result = DeleteResult {
            deleted: 0,
            not_found: Vec::new(),
            files_removed: 0,
            files_trashed: 0,
            previews_removed: 0,
            dry_run: !apply,
            errors: Vec::new(),
        };

        for raw_id in asset_ids {
            let asset_start = Instant::now();

            // 1. Resolve ID (prefix match)
            let asset_id = match catalog.resolve_asset_id(raw_id) {
                Ok(Some(id)) => id,
                Ok(None) => {
                    result.not_found.push(raw_id.clone());
                    on_asset(raw_id, &DeleteStatus::NotFound, asset_start.elapsed());
                    continue;
                }
                Err(e) => {
                    let msg = format!("{raw_id}: {e}");
                    result.errors.push(msg.clone());
                    on_asset(raw_id, &DeleteStatus::Error(msg), asset_start.elapsed());
                    continue;
                }
            };

            // 2. Gather variant hashes (before deleting variants)
            let variant_hashes = catalog.list_variant_hashes_for_asset(&asset_id)
                .unwrap_or_default();

            // 3. Gather file + recipe locations (for --remove-files and report)
            let file_locations = catalog.list_file_locations_for_asset(&asset_id)
                .unwrap_or_default();
            let recipe_locations = catalog.list_recipes_for_asset(&asset_id)
                .unwrap_or_default();

            if apply {
                // 4a. Delete physical files (only if remove_files)
                if remove_files {
                    for (_hash, rel_path, vol_id) in &file_locations {
                        if let Some(vol) = volume_map.get(vol_id.as_str()) {
                            if vol.is_online {
                                let full_path = vol.mount_point.join(rel_path);
                                if full_path.exists() {
                                    match trash.dispose(&full_path, &vol.label, rel_path) {
                                        Ok(crate::trash::Disposition::Trashed(_)) => {
                                            result.files_removed += 1;
                                            result.files_trashed += 1;
                                        }
                                        Ok(crate::trash::Disposition::Deleted) => {
                                            result.files_removed += 1;
                                        }
                                        Err(e) => {
                                            result.errors.push(format!(
                                                "Failed to remove file {}: {e}",
                                                full_path.display()
                                            ));
                                        }
                                    }
                                }
                            } else {
                                eprintln!(
                                    "  Warning: volume '{}' is offline, skipping file {}",
                                    vol.label, rel_path
                                );
                            }
                        }
                    }
                    for (_recipe_id, _content_hash, _variant_hash, rel_path, vol_id) in &recipe_locations {
                        if let Some(vol) = volume_map.get(vol_id.as_str()) {
                            if vol.is_online {
                                let full_path = vol.mount_point.join(rel_path);
                                if full_path.exists() {
                                    match trash.dispose(&full_path, &vol.label, rel_path) {
                                        Ok(crate::trash::Disposition::Trashed(_)) => {
                                            result.files_removed += 1;
                                            result.files_trashed += 1;
                                        }
                                        Ok(crate::trash::Disposition::Deleted) => {
                                            result.files_removed += 1;
                                        }
                                        Err(e) => {
                                            result.errors.push(format!(
                                                "Failed to remove recipe file {}: {e}",
                                                full_path.display()
                                            ));
                                        }
                                    }
                                }
                            }
                        }
                    }
                }

                // 4b. Sidecar YAML (source of truth) first. A failure here
                //     leaves the asset fully intact in both stores.
                if let Ok(uuid) = Uuid::parse_str(&asset_id) {
                    if let Err(e) = metadata_store.delete(uuid) {
                        let msg = format!("{asset_id}: failed to delete sidecar: {e}");
                        result.errors.push(msg.clone());
                        on_asset(&asset_id, &DeleteStatus::Error(msg), asset_start.elapsed());
                        continue;
                    }
                }

                // 4c. Every catalog row of the asset in one transaction:
                //     stack membership, collection memberships, faces,
                //     embeddings, recipes, file locations, variants, asset.
                let rows = catalog.with_tx(|c| {
                    stack_store.remove(std::slice::from_ref(&asset_id))?;
                    col_store.remove_asset(&asset_id)?;
                    let face_ids = list_face_ids(c, &asset_id);
                    c.conn().execute(
                        "DELETE FROM faces WHERE asset_id = ?1",
                        rusqlite::params![&asset_id],
                    )?;
                    c.conn().execute(
                        "DELETE FROM embeddings WHERE asset_id = ?1",
                        rusqlite::params![&asset_id],
                    )?;
                    c.delete_recipes_for_asset(&asset_id)?;
                    c.delete_file_locations_for_asset(&asset_id)?;
                    c.delete_variants_for_asset(&asset_id)?;
                    c.delete_asset(&asset_id)?;
                    Ok(face_ids)
                });
                let face_ids = match rows {
                    Ok(face_ids) => face_ids,
                    Err(e) => {
                        let msg = format!(
                            "{asset_id}: catalog delete failed and was rolled back \
                             (sidecar already removed; `rebuild-catalog` reconciles): {e:#}"
                        );
                        result.errors.push(msg.clone());
                        on_asset(&asset_id, &DeleteStatus::Error(msg), asset_start.elapsed());
                        continue;
                    }
                };

                // 4d. Derived files (regenerable caches: best-effort removal).
                self.remove_face_files(&face_ids);
                self.remove_embedding_files(&asset_id);

                // 4e. Delete previews
                for hash in &variant_hashes {
                    let preview_path = preview_gen.preview_path(hash);
                    if preview_path.exists()
                        && std::fs::remove_file(&preview_path).is_ok() {
                            result.previews_removed += 1;
                        }
                    let smart_path = preview_gen.smart_preview_path(hash);
                    if smart_path.exists()
                        && std::fs::remove_file(&smart_path).is_ok() {
                            result.previews_removed += 1;
                        }
                }

                result.deleted += 1;
                on_asset(&asset_id, &DeleteStatus::Deleted, asset_start.elapsed());
            } else {
                // Report mode: count what would be affected
                result.deleted += 1;
                on_asset(&asset_id, &DeleteStatus::Deleted, asset_start.elapsed());
            }
        }

        Ok(result)
    }

    /// Hard-purge a single orphaned asset (an asset whose variants have
    /// all lost their last file location): remove it from stacks,
    /// collections, faces, and embeddings; delete its catalog rows
    /// (recipes, locations, variants, asset row), its sidecar YAML, and
    /// all derived files (previews, smart previews, face crops, arcface
    /// and SigLIP embedding binaries).
    ///
    /// Shared by `cleanup` (orphaned-assets pass) and `sync`;
    /// `remove_volume` calls the two halves itself so the row half joins
    /// its own transaction.
    ///
    /// Returns `true` when the asset row was fully removed; `false` when
    /// the catalog transaction failed (the error is pushed to `errors`
    /// and no row was touched). A sidecar-delete failure is reported but
    /// still counts as removed, matching historical behavior.
    pub(super) fn purge_orphaned_asset(
        &self,
        catalog: &Catalog,
        metadata_store: &MetadataStore,
        preview_gen: &crate::preview::PreviewGenerator,
        asset_id: &str,
        errors: &mut Vec<String>,
    ) -> bool {
        let purged = match catalog.with_tx(|c| self.purge_orphaned_asset_rows(c, asset_id)) {
            Ok(p) => p,
            Err(e) => {
                errors.push(format!("Failed to delete orphaned asset {asset_id}: {e:#}"));
                return false;
            }
        };
        self.purge_orphaned_asset_files(metadata_store, preview_gen, &purged, errors);
        true
    }

    /// Catalog half of the orphan purge. Runs inside the caller's
    /// transaction (`Catalog::with_tx`); returns what the file half needs.
    /// Stack and collection memberships go through the stores, which
    /// persist `stacks.yaml` / `collections.yaml` when they change.
    pub(super) fn purge_orphaned_asset_rows(
        &self,
        catalog: &Catalog,
        asset_id: &str,
    ) -> Result<PurgedAsset> {
        // Collect variant hashes and face IDs before deleting DB records
        let variant_hashes: Vec<String> = catalog.conn()
            .prepare("SELECT content_hash FROM variants WHERE asset_id = ?1")
            .and_then(|mut s| s.query_map(rusqlite::params![asset_id], |r| r.get(0))
                .and_then(|rows| rows.collect()))?;
        let face_ids = list_face_ids(catalog, asset_id);

        // Remove from stacks, collections, faces, and embeddings before
        // deleting the asset
        crate::stack::StackStore::open(catalog.conn(), &self.catalog_root).remove(&[asset_id.to_string()])?;
        crate::collection::CollectionStore::open(catalog.conn(), &self.catalog_root).remove_asset(asset_id)?;
        catalog.conn().execute(
            "DELETE FROM faces WHERE asset_id = ?1",
            rusqlite::params![asset_id],
        )?;
        catalog.conn().execute(
            "DELETE FROM embeddings WHERE asset_id = ?1",
            rusqlite::params![asset_id],
        )?;

        catalog.delete_recipes_for_asset(asset_id)?;
        catalog.delete_file_locations_for_asset(asset_id)?;
        catalog.delete_variants_for_asset(asset_id)?;
        catalog.delete_asset(asset_id)?;

        Ok(PurgedAsset {
            asset_id: asset_id.to_string(),
            variant_hashes,
            face_ids,
        })
    }

    /// File half of the orphan purge: sidecar YAML plus derived files.
    /// Runs after the row transaction committed.
    pub(super) fn purge_orphaned_asset_files(
        &self,
        metadata_store: &MetadataStore,
        preview_gen: &crate::preview::PreviewGenerator,
        purged: &PurgedAsset,
        errors: &mut Vec<String>,
    ) {
        let asset_id = purged.asset_id.as_str();
        if let Ok(uuid) = uuid::Uuid::parse_str(asset_id) {
            if let Err(e) = metadata_store.delete(uuid) {
                errors.push(format!(
                    "Failed to delete sidecar for orphaned asset {asset_id}: {e}"
                ));
            }
        }

        // Derived files are regenerable caches: best-effort removal.
        for hash in &purged.variant_hashes {
            let _ = std::fs::remove_file(preview_gen.preview_path(hash));
            let _ = std::fs::remove_file(preview_gen.smart_preview_path(hash));
        }
        self.remove_face_files(&purged.face_ids);
        self.remove_embedding_files(asset_id);
    }

    /// Best-effort removal of face crops and ArcFace embedding binaries.
    fn remove_face_files(&self, face_ids: &[String]) {
        for face_id in face_ids {
            let prefix = &face_id[..2.min(face_id.len())];
            let _ = std::fs::remove_file(
                self.catalog_root.join("faces").join(prefix).join(format!("{face_id}.jpg")),
            );
            let _ = std::fs::remove_file(
                self.catalog_root.join("embeddings").join("arcface").join(prefix).join(format!("{face_id}.bin")),
            );
        }
    }

    /// Best-effort removal of the per-asset embedding binaries for every
    /// model directory present on disk. Enumerating `embeddings/` at
    /// runtime (instead of a hardcoded model list) keeps this correct as
    /// new embedding models ship. The `arcface` dir is keyed by face ID
    /// (see [`Self::remove_face_files`]), so `<asset_id>.bin` simply
    /// doesn't exist there.
    fn remove_embedding_files(&self, asset_id: &str) {
        let prefix = &asset_id[..2.min(asset_id.len())];
        if let Ok(entries) = std::fs::read_dir(self.catalog_root.join("embeddings")) {
            for entry in entries.flatten() {
                if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                    let _ = std::fs::remove_file(
                        entry.path().join(prefix).join(format!("{asset_id}.bin")),
                    );
                }
            }
        }
    }
}

/// What the catalog half of an orphan purge removed; input to the file half.
pub(super) struct PurgedAsset {
    pub(super) asset_id: String,
    pub(super) variant_hashes: Vec<String>,
    pub(super) face_ids: Vec<String>,
}

/// Face IDs of an asset. A read: a catalog without the `faces` table
/// (never ran face detection) simply has none.
fn list_face_ids(catalog: &Catalog, asset_id: &str) -> Vec<String> {
    catalog.conn()
        .prepare("SELECT id FROM faces WHERE asset_id = ?1")
        .and_then(|mut s| s.query_map(rusqlite::params![asset_id], |r| r.get(0))
            .and_then(|rows| rows.collect()))
        .unwrap_or_default()
}

/// One `cleanup` in progress: stores, the apply flag, the result under
/// construction, the stale locations found in report mode (which predict
/// the orphans the later passes would produce) and the progress callback.
struct CleanupRun<'a, F> {
    service: &'a AssetService,
    catalog: Catalog,
    metadata_store: MetadataStore,
    apply: bool,
    result: CleanupResult,
    /// `(content_hash, volume_id, relative_path)` of stale locations seen in
    /// report mode, for the would-be-orphan predictions of passes 2 and 3.
    stale_locations: Vec<(String, String, String)>,
    on_file: F,
}

impl<F: Fn(&Path, CleanupStatus, Duration)> CleanupRun<'_, F> {
    /// Pass 1 for one online volume: every variant location and recipe
    /// under `prefix` whose file is gone is stale; with `apply` its row is
    /// removed (sidecar first).
    fn check_volume_locations(&mut self, volume: &Volume, prefix: &str) -> Result<()> {
        let vol_id_str = volume.id.to_string();

        // Check variant file locations
        let locations = self
            .catalog
            .list_locations_for_volume_under_prefix(&vol_id_str, prefix)?;
        for (content_hash, relative_path) in &locations {
            let file_start = Instant::now();
            let full_path = volume.mount_point.join(relative_path);

            if full_path.exists() {
                self.result.checked += 1;
                (self.on_file)(&full_path, CleanupStatus::Ok, file_start.elapsed());
                continue;
            }
            self.result.stale += 1;
            (self.on_file)(&full_path, CleanupStatus::Stale, file_start.elapsed());
            if self.apply {
                // Sidecar (source of truth) first, then the catalog row.
                if let Err(e) = self.service.remove_sidecar_file_location(
                    &self.metadata_store,
                    &self.catalog,
                    content_hash,
                    volume.id,
                    relative_path,
                ) {
                    self.result.errors.push(format!(
                        "Failed to update sidecar for {}: {e}",
                        relative_path
                    ));
                } else if let Err(e) = self.catalog.delete_file_location(
                    content_hash,
                    &vol_id_str,
                    relative_path,
                ) {
                    self.result.errors.push(format!(
                        "Failed to remove location {}: {e}",
                        relative_path
                    ));
                } else {
                    self.result.removed += 1;
                }
            } else {
                self.stale_locations.push((
                    content_hash.clone(),
                    vol_id_str.clone(),
                    relative_path.clone(),
                ));
            }
        }

        // Check recipe file locations
        let recipes = self
            .catalog
            .list_recipes_for_volume_under_prefix(&vol_id_str, prefix)?;
        for (recipe_id, _content_hash, variant_hash, relative_path) in &recipes {
            let file_start = Instant::now();
            let full_path = volume.mount_point.join(relative_path);

            if full_path.exists() {
                self.result.checked += 1;
                (self.on_file)(&full_path, CleanupStatus::Ok, file_start.elapsed());
                continue;
            }
            self.result.stale += 1;
            (self.on_file)(&full_path, CleanupStatus::Stale, file_start.elapsed());
            if self.apply {
                if let Err(e) = self.service.remove_sidecar_recipe(
                    &self.metadata_store,
                    &self.catalog,
                    variant_hash,
                    volume.id,
                    relative_path,
                ) {
                    self.result.errors.push(format!(
                        "Failed to update sidecar for recipe {}: {e}",
                        relative_path
                    ));
                } else if let Err(e) = self.catalog.delete_recipe(recipe_id) {
                    self.result.errors.push(format!(
                        "Failed to remove recipe {}: {e}",
                        relative_path
                    ));
                } else {
                    self.result.removed += 1;
                }
            }
        }
        Ok(())
    }

    /// Pass 2: variants without any location whose asset still has other
    /// located variants. In apply mode they are dropped from the sidecar
    /// and the catalog (one transaction per asset) with their previews;
    /// in report mode the count is predicted from the stale locations.
    fn remove_locationless_variants(&mut self) -> Result<()> {
        let locationless = if self.apply {
            self.catalog.list_locationless_variants()?
        } else {
            self.catalog
                .list_would_be_locationless_variants(&self.stale_locations)?
        };
        self.result.locationless_variants = locationless.len();
        if !self.apply {
            return Ok(());
        }

        let preview_gen = self.service.preview_generator();
        // Group by asset_id so we can update sidecars and denormalized columns once per asset
        let mut by_asset: HashMap<String, Vec<String>> = HashMap::new();
        for (asset_id, content_hash) in &locationless {
            by_asset
                .entry(asset_id.clone())
                .or_default()
                .push(content_hash.clone());
        }

        for (asset_id, hashes) in &by_asset {
            let asset_start = Instant::now();

            // Sidecar (source of truth) first: drop the variant(s) and
            // their recipes. An asset without a loadable sidecar is a
            // catalog-only ghost; its rows are still removed below.
            let sidecar_asset = match uuid::Uuid::parse_str(asset_id)
                .ok()
                .and_then(|uuid| self.metadata_store.load(uuid).ok())
            {
                Some(mut asset) => {
                    let hash_set: HashSet<&str> = hashes.iter().map(|h| h.as_str()).collect();
                    asset.variants.retain(|v| !hash_set.contains(v.content_hash.as_str()));
                    asset.recipes.retain(|r| !hash_set.contains(r.variant_hash.as_str()));
                    if let Err(e) = self.metadata_store.save(&asset) {
                        self.result.errors.push(format!(
                            "Failed to update sidecar for asset {asset_id}: {e}"
                        ));
                        continue;
                    }
                    Some(asset)
                }
                None => None,
            };

            // Then every catalog row of this asset in one transaction
            // (variant rows cascade to file_locations, recipes, embeddings;
            // the denormalized columns follow the new variant set).
            let tx = self.catalog.with_tx(|c| {
                for hash in hashes {
                    c.delete_variant(hash)?;
                }
                if let Some(asset) = &sidecar_asset {
                    c.update_denormalized_variant_columns(asset)?;
                }
                Ok(())
            });
            if let Err(e) = tx {
                self.result.errors.push(format!(
                    "Failed to delete locationless variants of asset {asset_id}: {e:#}"
                ));
                continue;
            }

            for hash in hashes {
                // Derived files are a regenerable cache: best-effort removal.
                let _ = std::fs::remove_file(preview_gen.preview_path(hash));
                let _ = std::fs::remove_file(preview_gen.smart_preview_path(hash));
                self.result.removed_variants += 1;
                (self.on_file)(&PathBuf::from(hash), CleanupStatus::LocationlessVariant, asset_start.elapsed());
            }
        }
        Ok(())
    }

    /// Pass 3: assets whose variants all have zero file locations. In
    /// apply mode, locations were already removed so we query directly;
    /// in report mode, we predict which assets would become orphaned.
    fn remove_orphaned_assets(&mut self) -> Result<()> {
        let orphaned_ids = if self.apply {
            self.catalog.list_orphaned_asset_ids()?
        } else {
            self.catalog
                .list_would_be_orphaned_asset_ids(&self.stale_locations)?
        };
        self.result.orphaned_assets = orphaned_ids.len();
        if !self.apply {
            return Ok(());
        }

        let preview_gen = self.service.preview_generator();
        for asset_id in &orphaned_ids {
            let file_start = Instant::now();
            let asset_id_path = PathBuf::from(asset_id);
            if !self.service.purge_orphaned_asset(
                &self.catalog,
                &self.metadata_store,
                &preview_gen,
                asset_id,
                &mut self.result.errors,
            ) {
                continue;
            }
            self.result.removed_assets += 1;
            (self.on_file)(&asset_id_path, CleanupStatus::OrphanedAsset, file_start.elapsed());
        }
        Ok(())
    }

    /// Passes 4–7: derived files under the catalog directory that no
    /// variant, asset or face refers to any more.
    fn remove_orphaned_derived_files(&mut self) -> Result<()> {
        let catalog_root = &self.service.catalog_root;

        // Pass 4: Orphaned previews (preview files with no matching variant)
        let variant_hashes = self.catalog.list_all_variant_hashes()?;
        scan_orphaned_sharded_files(
            &catalog_root.join("previews"),
            |stem| {
                let content_hash = format!("sha256:{stem}");
                variant_hashes.contains(&content_hash)
            },
            self.apply,
            &mut self.result.orphaned_previews,
            &mut self.result.removed_previews,
            &mut self.result.errors,
            &self.on_file,
        );

        // Pass 5: Orphaned smart previews (same logic, different directory)
        scan_orphaned_sharded_files(
            &catalog_root.join("smart-previews"),
            |stem| {
                let content_hash = format!("sha256:{stem}");
                variant_hashes.contains(&content_hash)
            },
            self.apply,
            &mut self.result.orphaned_smart_previews,
            &mut self.result.removed_smart_previews,
            &mut self.result.errors,
            &self.on_file,
        );

        // Pass 6: Orphaned embedding binaries (asset_id.bin under embeddings/<model>/)
        let asset_ids_set: HashSet<String> = self.catalog.list_all_asset_ids()?;
        let emb_base = catalog_root.join("embeddings");
        if emb_base.is_dir() {
            if let Ok(model_entries) = std::fs::read_dir(&emb_base) {
                for model_entry in model_entries.flatten() {
                    if !model_entry.path().is_dir() {
                        continue;
                    }
                    let model_name = model_entry.file_name().to_string_lossy().to_string();
                    if model_name == "arcface" {
                        continue; // handled separately in pass 7
                    }
                    scan_orphaned_sharded_files(
                        &model_entry.path(),
                        |stem| asset_ids_set.contains(stem),
                        self.apply,
                        &mut self.result.orphaned_embeddings,
                        &mut self.result.removed_embeddings,
                        &mut self.result.errors,
                        &self.on_file,
                    );
                }
            }
        }

        // Pass 7: Orphaned face crop thumbnails (face_id.jpg under faces/)
        let face_ids_set: HashSet<String> = self.catalog.conn()
            .prepare("SELECT id FROM faces")
            .and_then(|mut s| s.query_map([], |r| r.get(0))
                .and_then(|rows| rows.collect()))
            .unwrap_or_default();
        scan_orphaned_sharded_files(
            &catalog_root.join("faces"),
            |stem| face_ids_set.contains(stem),
            self.apply,
            &mut self.result.orphaned_face_files,
            &mut self.result.removed_face_files,
            &mut self.result.errors,
            &self.on_file,
        );

        // Pass 7: Orphaned ArcFace embedding binaries (face_id.bin under embeddings/arcface/)
        scan_orphaned_sharded_files(
            &catalog_root.join("embeddings").join("arcface"),
            |stem| face_ids_set.contains(stem),
            self.apply,
            &mut self.result.orphaned_embeddings,
            &mut self.result.removed_embeddings,
            &mut self.result.errors,
            &self.on_file,
        );
        Ok(())
    }
}
