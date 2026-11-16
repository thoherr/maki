//! `refresh` section of `AssetService` — methods extracted from the original
//! 8.9-kLOC asset_service.rs into a multi-file `impl AssetService` block.
//!
//! Free functions and result types live in the parent `asset_service` module.

use super::*;

/// What a refresh / sync-metadata run covers: one asset, the files under
/// some paths on a volume, a whole volume, or every online volume.
struct RefreshScope<'a> {
    paths: &'a [PathBuf],
    volume: Option<&'a Volume>,
    asset_id: Option<&'a str>,
    exclude_patterns: &'a [String],
}

/// A recipe row to refresh.
struct RecipeEntry {
    recipe_id: String,
    stored_hash: String,
    variant_hash: String,
    relative_path: String,
    volume_id: String,
}

/// A media file location whose embedded XMP may be re-read.
struct MediaEntry {
    content_hash: String,
    relative_path: String,
    volume_id: String,
}

impl AssetService {
    // ═══ REFRESH & SYNC METADATA ═══

    /// Re-read metadata from changed recipe/sidecar files, and optionally
    /// re-extract embedded XMP from JPEG/TIFF media files (`--media`).
    #[allow(clippy::too_many_arguments)]
    pub fn refresh(
        &self,
        paths: &[PathBuf],
        volume: Option<&Volume>,
        asset_id: Option<&str>,
        dry_run: bool,
        media: bool,
        exclude_patterns: &[String],
        on_file: impl Fn(&Path, RefreshStatus, Duration),
    ) -> Result<RefreshResult> {
        let registry = DeviceRegistry::new(&self.catalog_root);
        let scope = RefreshScope {
            paths,
            volume,
            asset_id,
            exclude_patterns,
        };
        let mut run = RefreshRun {
            service: self,
            content_store: ContentStore::new(&self.catalog_root),
            metadata_store: MetadataStore::new(&self.catalog_root),
            catalog: Catalog::open(&self.catalog_root)?,
            all_volumes: registry.list()?,
            dry_run,
            result: RefreshResult {
                unchanged: 0,
                refreshed: 0,
                missing: 0,
                skipped: 0,
                errors: Vec::new(),
            },
            on_file,
        };

        for entry in &run.collect_recipes(&registry, &scope)? {
            run.refresh_recipe(entry)?;
        }

        // --- Media file processing (embedded XMP re-extraction) ---
        if media {
            for entry in &run.collect_media(&registry, &scope)? {
                run.refresh_media(entry)?;
            }
        }

        Ok(run.result)
    }

    /// Re-read the embedded XMP of a JPEG/TIFF variant into its asset
    /// (sidecar first, then the asset and variant rows in one
    /// transaction; nothing is written on `dry_run`). `Ok(false)` when
    /// the file carries no metadata, `Ok(true)` when it was reapplied.
    /// Shared by `refresh --media` and `sync-metadata --media`.
    fn reextract_embedded_xmp(
        &self,
        catalog: &Catalog,
        metadata_store: &MetadataStore,
        asset_id_str: &str,
        content_hash: &str,
        full_path: &Path,
        dry_run: bool,
    ) -> Result<bool> {
        let embedded_xmp = crate::embedded_xmp::extract_embedded_xmp(full_path);
        if !xmp_has_metadata(&embedded_xmp) {
            return Ok(false);
        }

        let uuid: Uuid = asset_id_str.parse()?;
        let mut asset = metadata_store.load(uuid)?;

        embedded_xmp.reapply_to(&mut asset, content_hash);

        if !dry_run {
            metadata_store.save(&asset)?;
            catalog.with_tx(|c| {
                c.insert_asset(&asset)?;
                if let Some(v) = asset.variants.iter().find(|v| v.content_hash == content_hash) {
                    c.insert_variant(v)?;
                }
                Ok(())
            })?;
        }
        Ok(true)
    }

    /// Bidirectional metadata sync: reads external XMP changes (inbound) and writes pending
    /// DAM changes back (outbound). Detects conflicts where both sides changed.
    ///
    /// Phase 1 (Inbound): For each XMP recipe on online volumes, hash the file. If the hash
    /// differs from stored AND the recipe has no pending_writeback, read external changes.
    /// If both changed, report as conflict.
    ///
    /// Phase 2 (Outbound): Write pending DAM metadata to XMP recipes that weren't conflicting.
    ///
    /// Phase 3 (Media, optional): Re-extract embedded XMP from JPEG/TIFF files.
    pub fn sync_metadata(
        &self,
        volume: Option<&Volume>,
        asset_id: Option<&str>,
        dry_run: bool,
        media: bool,
        _exclude_patterns: &[String],
        on_file: impl Fn(&Path, SyncMetadataStatus, Duration),
    ) -> Result<SyncMetadataResult> {
        let content_store = ContentStore::new(&self.catalog_root);
        let metadata_store = MetadataStore::new(&self.catalog_root);
        let catalog = Catalog::open(&self.catalog_root)?;
        let registry = DeviceRegistry::new(&self.catalog_root);
        let all_volumes = registry.list()?;

        let mut result = SyncMetadataResult {
            inbound: 0,
            outbound: 0,
            unchanged: 0,
            skipped: 0,
            conflicts: 0,
            media_refreshed: 0,
            dry_run,
            errors: Vec::new(),
        };

        let recipes = collect_xmp_recipes(&catalog, &all_volumes, volume, asset_id)?;

        // Phase 1: Inbound — read external XMP changes; collect pending recipes for Phase 2
        let mut pending_for_writeback: Vec<(String, String, String, String)> = Vec::new();

        for entry in &recipes {
            let file_start = Instant::now();
            let full_path = entry.vol.mount_point.join(&entry.relative_path);

            if !full_path.exists() {
                result.skipped += 1;
                on_file(&full_path, SyncMetadataStatus::Missing, file_start.elapsed());
                continue;
            }

            let new_hash = match content_store.hash_file(&full_path) {
                Ok(h) => h,
                Err(e) => {
                    result.errors.push(format!("{}: {}", full_path.display(), e));
                    on_file(&full_path, SyncMetadataStatus::Error, file_start.elapsed());
                    continue;
                }
            };

            let disk_changed = new_hash != entry.stored_hash;

            match (disk_changed, entry.pending) {
                (false, false) => {
                    // Nothing to do
                    result.unchanged += 1;
                    on_file(&full_path, SyncMetadataStatus::Unchanged, file_start.elapsed());
                }
                (true, false) => {
                    // External change, no pending DAM edits → inbound (refresh)
                    if !dry_run {
                        if let Err(e) = self.apply_modified_recipe(
                            &catalog,
                            &metadata_store,
                            &entry.recipe_id,
                            &new_hash,
                            &entry.variant_hash,
                            entry.vol,
                            &full_path,
                            &entry.relative_path,
                        ) {
                            result.errors.push(format!("{}: {}", full_path.display(), e));
                            on_file(&full_path, SyncMetadataStatus::Error, file_start.elapsed());
                            continue;
                        }
                    }
                    result.inbound += 1;
                    on_file(&full_path, SyncMetadataStatus::Inbound, file_start.elapsed());
                }
                (false, true) => {
                    // No external change, pending DAM edits → outbound (writeback)
                    // Look up asset_id for the writeback process
                    if let Ok(Some(aid)) = catalog.find_asset_id_by_variant(&entry.variant_hash) {
                        pending_for_writeback.push((
                            entry.recipe_id.clone(),
                            aid,
                            entry.vol.id.to_string(),
                            entry.relative_path.clone(),
                        ));
                    }
                    // Don't count here — will be counted by writeback_process
                }
                (true, true) => {
                    // Both sides changed → conflict
                    result.conflicts += 1;
                    on_file(&full_path, SyncMetadataStatus::Conflict, file_start.elapsed());
                }
            }
        }

        // Phase 2: Outbound — write pending DAM metadata via writeback
        if !pending_for_writeback.is_empty() {
            let engine = self.query_engine();
            let online = DeviceRegistry::online_map_of(&all_volumes);
            let volume_labels = DeviceRegistry::label_map_of(&all_volumes);

            // No further scope filter (already filtered above); no
            // `mirror_tags` — sync-metadata is incremental, the explicit
            // `maki writeback --all --mirror-tags` owns the destructive
            // cleanup; no `force` — never rebuild from scratch here.
            let req = crate::query::WritebackRequest { dry_run, ..Default::default() };
            let wb_result = engine.writeback_recipes(
                pending_for_writeback,
                &catalog,
                &online,
                &volume_labels,
                &req,
                &mut |event| {
                    if let crate::query::WritebackEvent::Warning(message) = &event {
                        eprintln!("{message}");
                    }
                },
            )?;

            result.outbound += wb_result.written as usize;
            result.skipped += wb_result.skipped as usize;
            result.errors.extend(wb_result.errors);
        }

        // Phase 3: Media — re-extract embedded XMP from JPEG/TIFF files (same as refresh --media)
        if media {
            self.sync_metadata_media(
                &catalog,
                &metadata_store,
                &all_volumes,
                volume,
                asset_id,
                dry_run,
                &mut result,
            )?;
        }

        Ok(result)
    }

    /// Phase 3 of `sync_metadata`: re-extract embedded XMP from every
    /// JPEG/TIFF location in scope whose asset has no XMP sidecar recipe.
    #[allow(clippy::too_many_arguments)]
    fn sync_metadata_media(
        &self,
        catalog: &Catalog,
        metadata_store: &MetadataStore,
        all_volumes: &[Volume],
        volume: Option<&Volume>,
        asset_id: Option<&str>,
        dry_run: bool,
        result: &mut SyncMetadataResult,
    ) -> Result<()> {
        let media_entries: Vec<(String, String, String)> = if let Some(aid) = asset_id {
            catalog.list_file_locations_for_asset(aid)?
        } else if let Some(vol) = volume {
            let vol_id = vol.id.to_string();
            catalog
                .list_locations_for_volume_under_prefix(&vol_id, "")?
                .into_iter()
                .map(|(ch, rp)| (ch, rp, vol_id.clone()))
                .collect()
        } else {
            let mut entries = Vec::new();
            for vol in all_volumes {
                if !vol.is_online {
                    continue;
                }
                let vol_id = vol.id.to_string();
                for (ch, rp) in catalog.list_locations_for_volume_under_prefix(&vol_id, "")? {
                    entries.push((ch, rp, vol_id.clone()));
                }
            }
            entries
        };

        for (content_hash, relative_path, volume_id_str) in &media_entries {
            let ext = Path::new(relative_path)
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("");
            if !is_embedded_xmp_extension(ext) {
                continue;
            }

            let vol = match all_volumes.iter().find(|v| v.id.to_string() == *volume_id_str) {
                Some(v) if v.is_online => v,
                _ => continue,
            };

            let full_path = vol.mount_point.join(relative_path);
            if !full_path.exists() {
                continue;
            }

            let asset_id_str = match catalog.find_asset_id_by_variant(content_hash)? {
                Some(id) => id,
                None => continue,
            };

            // Sidecar-wins: if the asset has an XMP recipe, skip
            // embedded-XMP re-extraction. See refresh_media for
            // rationale.
            if catalog.asset_has_recipe(&asset_id_str)? {
                result.skipped += 1;
                continue;
            }

            if self.reextract_embedded_xmp(
                catalog,
                metadata_store,
                &asset_id_str,
                content_hash,
                &full_path,
                dry_run,
            )? {
                result.media_refreshed += 1;
            }
        }
        Ok(())
    }
}

/// One XMP recipe of a `sync_metadata` run.
struct XmpRecipeEntry<'a> {
    recipe_id: String,
    stored_hash: String,
    variant_hash: String,
    relative_path: String,
    pending: bool,
    vol: &'a Volume,
}

/// The XMP recipes on the volumes in scope (one volume, or every online
/// one), optionally narrowed to one asset (id prefix).
fn collect_xmp_recipes<'a>(
    catalog: &Catalog,
    all_volumes: &'a [Volume],
    volume: Option<&'a Volume>,
    asset_id: Option<&str>,
) -> Result<Vec<XmpRecipeEntry<'a>>> {
    let mut recipes: Vec<XmpRecipeEntry<'a>> = Vec::new();

    // Determine which volumes to scan
    let target_volumes: Vec<&Volume> = if let Some(v) = volume {
        vec![v]
    } else {
        all_volumes.iter().filter(|v| v.is_online).collect()
    };

    for vol in &target_volumes {
        if !vol.is_online {
            continue;
        }
        let vol_id = vol.id.to_string();
        let entries = catalog.list_recipes_with_pending_for_volume(&vol_id)?;

        for (rid, ch, vh, rp, pending) in entries {
            // Only XMP files participate in metadata sync
            let is_xmp = rp.to_lowercase().ends_with(".xmp");
            if !is_xmp {
                continue;
            }

            // Filter by asset if requested
            if let Some(aid) = asset_id {
                // Look up the asset for this recipe's variant
                if let Ok(Some(recipe_asset_id)) = catalog.find_asset_id_by_variant(&vh) {
                    if !recipe_asset_id.starts_with(aid) {
                        continue;
                    }
                } else {
                    continue;
                }
            }

            recipes.push(XmpRecipeEntry {
                recipe_id: rid,
                stored_hash: ch,
                variant_hash: vh,
                relative_path: rp,
                pending,
                vol,
            });
        }
    }
    Ok(recipes)
}

/// One `refresh` in progress.
struct RefreshRun<'a, F> {
    service: &'a AssetService,
    content_store: ContentStore,
    metadata_store: MetadataStore,
    catalog: Catalog,
    all_volumes: Vec<Volume>,
    dry_run: bool,
    result: RefreshResult,
    on_file: F,
}

impl<F: Fn(&Path, RefreshStatus, Duration)> RefreshRun<'_, F> {
    /// The recipe rows a scope covers.
    fn collect_recipes(&self, registry: &DeviceRegistry, scope: &RefreshScope<'_>) -> Result<Vec<RecipeEntry>> {
        let to_entry = |(recipe_id, stored_hash, variant_hash, relative_path, volume_id): (String, String, String, String, String)| RecipeEntry {
            recipe_id,
            stored_hash,
            variant_hash,
            relative_path,
            volume_id,
        };

        if let Some(aid) = scope.asset_id {
            // Asset mode: all recipes for a specific asset
            return Ok(self
                .catalog
                .list_recipes_for_asset(aid)?
                .into_iter()
                .map(to_entry)
                .collect());
        }
        if !scope.paths.is_empty() {
            // Path mode: scan files under given paths, filter to recipes, look up each
            let files = resolve_files(scope.paths, scope.exclude_patterns);
            let filter = FileTypeFilter::default();
            let vol = scope.volume.ok_or_else(|| anyhow::anyhow!("no volume resolved for path mode"))?;
            let vol_id = vol.id.to_string();

            let mut entries = Vec::new();
            for file_path in &files {
                let ext = file_path
                    .extension()
                    .and_then(|e| e.to_str())
                    .unwrap_or("");
                if !filter.is_recipe(ext) {
                    continue;
                }
                // Normalize to forward slashes — catalog relative_path is
                // stored that way (serialize_path_forward_slash); without
                // this the find_*_by_volume_and_path lookups miss on Windows
                // (strip_prefix yields backslashes), so `maki refresh <path>`
                // and watch-driven refresh found nothing to refresh.
                let relative_path = match file_path.strip_prefix(&vol.mount_point) {
                    Ok(rp) => crate::path_util::forward_slashes(rp),
                    Err(_) => continue,
                };
                if let Some((recipe_id, content_hash, variant_hash)) =
                    self.catalog.find_recipe_by_volume_and_path(&vol_id, &relative_path)?
                {
                    entries.push(to_entry((recipe_id, content_hash, variant_hash, relative_path, vol_id.clone())));
                }
            }
            return Ok(entries);
        }
        // Volume mode: all recipes on the specified volume; all mode: every online volume
        let volumes: Vec<Volume> = match scope.volume {
            Some(vol) => vec![vol.clone()],
            None => registry.list()?.into_iter().filter(|v| v.is_online).collect(),
        };
        let mut entries = Vec::new();
        for vol in &volumes {
            let vol_id = vol.id.to_string();
            for (rid, ch, vh, rp) in self.catalog.list_recipes_for_volume_under_prefix(&vol_id, "")? {
                entries.push(to_entry((rid, ch, vh, rp, vol_id.clone())));
            }
        }
        Ok(entries)
    }

    /// The media file locations a scope covers (filtered to JPEG/TIFF by
    /// the caller).
    fn collect_media(&self, registry: &DeviceRegistry, scope: &RefreshScope<'_>) -> Result<Vec<MediaEntry>> {
        let to_entry = |(content_hash, relative_path, volume_id): (String, String, String)| MediaEntry {
            content_hash,
            relative_path,
            volume_id,
        };

        if let Some(aid) = scope.asset_id {
            return Ok(self
                .catalog
                .list_file_locations_for_asset(aid)?
                .into_iter()
                .map(to_entry)
                .collect());
        }
        if !scope.paths.is_empty() {
            let files = resolve_files(scope.paths, scope.exclude_patterns);
            let vol = scope.volume.ok_or_else(|| anyhow::anyhow!("no volume resolved for path mode"))?;
            let vol_id = vol.id.to_string();

            let mut entries = Vec::new();
            for file_path in &files {
                let ext = file_path
                    .extension()
                    .and_then(|e| e.to_str())
                    .unwrap_or("");
                if !is_embedded_xmp_extension(ext) {
                    continue;
                }
                let relative_path = match file_path.strip_prefix(&vol.mount_point) {
                    Ok(rp) => crate::path_util::forward_slashes(rp),
                    Err(_) => continue,
                };
                if let Some((content_hash, _format)) =
                    self.catalog.find_variant_by_volume_and_path(&vol_id, &relative_path)?
                {
                    entries.push(to_entry((content_hash, relative_path, vol_id.clone())));
                }
            }
            return Ok(entries);
        }
        let volumes: Vec<Volume> = match scope.volume {
            Some(vol) => vec![vol.clone()],
            None => registry.list()?.into_iter().filter(|v| v.is_online).collect(),
        };
        let mut entries = Vec::new();
        for vol in &volumes {
            let vol_id = vol.id.to_string();
            for (ch, rp) in self.catalog.list_locations_for_volume_under_prefix(&vol_id, "")? {
                entries.push(to_entry((ch, rp, vol_id.clone())));
            }
        }
        Ok(entries)
    }

    /// Resolve an entry's file: `None` (already counted and reported as
    /// offline or missing) or the index of its online volume in
    /// `all_volumes` and the full path of the existing file.
    fn locate(&mut self, volume_id: &str, relative_path: &str, file_start: Instant) -> Option<(usize, PathBuf)> {
        // Find the volume
        let Some(vol_idx) = self.all_volumes.iter().position(|v| v.id.to_string() == volume_id) else {
            self.result.skipped += 1;
            (self.on_file)(Path::new(relative_path), RefreshStatus::Offline, file_start.elapsed());
            return None;
        };
        let vol = &self.all_volumes[vol_idx];

        if !vol.is_online {
            self.result.skipped += 1;
            (self.on_file)(
                &vol.mount_point.join(relative_path),
                RefreshStatus::Offline,
                file_start.elapsed(),
            );
            return None;
        }

        let full_path = vol.mount_point.join(relative_path);

        if !full_path.exists() {
            self.result.missing += 1;
            (self.on_file)(&full_path, RefreshStatus::Missing, file_start.elapsed());
            return None;
        }
        Some((vol_idx, full_path))
    }

    /// Re-hash one recipe file; a changed hash re-reads its metadata.
    fn refresh_recipe(&mut self, entry: &RecipeEntry) -> Result<()> {
        let file_start = Instant::now();
        let Some((vol_idx, full_path)) = self.locate(&entry.volume_id, &entry.relative_path, file_start) else {
            return Ok(());
        };
        let vol = &self.all_volumes[vol_idx];

        let new_hash = match self.content_store.hash_file(&full_path) {
            Ok(h) => h,
            Err(e) => {
                self.result.errors.push(format!("{}: {}", full_path.display(), e));
                return Ok(());
            }
        };

        if new_hash == entry.stored_hash {
            self.result.unchanged += 1;
            (self.on_file)(&full_path, RefreshStatus::Unchanged, file_start.elapsed());
            return Ok(());
        }
        if !self.dry_run {
            if let Err(e) = self.service.apply_modified_recipe(
                &self.catalog,
                &self.metadata_store,
                &entry.recipe_id,
                &new_hash,
                &entry.variant_hash,
                vol,
                &full_path,
                &entry.relative_path,
            ) {
                self.result.errors.push(format!("{}: {}", full_path.display(), e));
                return Ok(());
            }
        }
        self.result.refreshed += 1;
        (self.on_file)(&full_path, RefreshStatus::Refreshed, file_start.elapsed());
        Ok(())
    }

    /// Re-extract the embedded XMP of one JPEG/TIFF location unless its
    /// asset has an XMP sidecar recipe.
    fn refresh_media(&mut self, entry: &MediaEntry) -> Result<()> {
        // Filter to JPEG/TIFF only
        let ext = Path::new(&entry.relative_path)
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("");
        if !is_embedded_xmp_extension(ext) {
            return Ok(());
        }

        let file_start = Instant::now();
        let Some((_, full_path)) = self.locate(&entry.volume_id, &entry.relative_path, file_start) else {
            return Ok(());
        };

        // Resolve owning asset; short-circuit when an XMP sidecar
        // exists. The sidecar is the authoritative metadata file
        // and `writeback` keeps it current. Embedded XMP inside
        // a JPEG/TIFF variant is frozen at import time — reading
        // it would re-inject stale flat keywords every refresh.
        let asset_id_str = match self.catalog.find_asset_id_by_variant(&entry.content_hash)? {
            Some(id) => id,
            None => {
                self.result.errors.push(format!(
                    "{}: no asset found for variant {}",
                    full_path.display(),
                    entry.content_hash
                ));
                return Ok(());
            }
        };

        if self.catalog.asset_has_recipe(&asset_id_str)? {
            self.result.skipped += 1;
            (self.on_file)(&full_path, RefreshStatus::SidecarPresent, file_start.elapsed());
            return Ok(());
        }

        let refreshed = self.service.reextract_embedded_xmp(
            &self.catalog,
            &self.metadata_store,
            &asset_id_str,
            &entry.content_hash,
            &full_path,
            self.dry_run,
        )?;
        if refreshed {
            self.result.refreshed += 1;
            (self.on_file)(&full_path, RefreshStatus::Refreshed, file_start.elapsed());
        } else {
            self.result.unchanged += 1;
            (self.on_file)(&full_path, RefreshStatus::Unchanged, file_start.elapsed());
        }
        Ok(())
    }
}
