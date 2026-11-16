//! `sync` section of `AssetService` — methods extracted from the original
//! 8.9-kLOC asset_service.rs into a multi-file `impl AssetService` block.
//!
//! Free functions and result types live in the parent `asset_service` module.

use super::*;

impl AssetService {
    // ═══ SYNC ═══

    /// Scan directories and reconcile the catalog with disk reality.
    ///
    /// Detects moved files, new files, modified recipes, and missing files.
    /// Without `apply`, runs in report-only mode. With `apply`, updates the catalog
    /// and sidecar files. `remove_stale` (requires `apply`) removes catalog locations
    /// for confirmed-missing files.
    ///
    /// Phases, each a method of [`SyncRun`]: scan the files on disk
    /// (unchanged / modified / new, with candidates for move detection),
    /// reconcile the catalog's locations under the scanned prefixes
    /// (moved / missing, optionally removing stale rows), report the
    /// leftover move candidates as new, purge assets that lost their last
    /// location, and count the variants left without one.
    pub fn sync(
        &self,
        paths: &[PathBuf],
        volume: &Volume,
        apply: bool,
        remove_stale: bool,
        exclude_patterns: &[String],
        on_file: impl Fn(&Path, SyncStatus, Duration),
    ) -> Result<SyncResult> {
        let mut run = SyncRun {
            service: self,
            content_store: ContentStore::new(&self.catalog_root),
            metadata_store: MetadataStore::new(&self.catalog_root),
            catalog: Catalog::open(&self.catalog_root)?,
            filter: FileTypeFilter::default(),
            volume,
            vol_id: volume.id.to_string(),
            apply,
            remove_stale,
            result: SyncResult {
                unchanged: 0,
                moved: 0,
                new_files: 0,
                modified: 0,
                missing: 0,
                stale_removed: 0,
                orphaned_cleaned: 0,
                locationless_after: 0,
                errors: Vec::new(),
            },
            disk_media_paths: HashSet::new(),
            disk_recipe_paths: HashSet::new(),
            media_hash_to_new_path: HashMap::new(),
            recipe_hash_to_new_path: HashMap::new(),
            on_file,
        };

        // ── Pass 1: Scan disk files ──────────────────────────────────
        let files = resolve_files(paths, exclude_patterns);
        for file_path in &files {
            run.scan_disk_file(file_path)?;
        }

        // ── Pass 2: Detect missing/moved ─────────────────────────────
        // Compute directory prefixes from scanned paths
        let prefixes = compute_prefixes(paths, &volume.mount_point);
        for prefix in &prefixes {
            run.reconcile_media_locations(prefix)?;
        }
        for prefix in &prefixes {
            run.reconcile_recipe_locations(prefix)?;
        }
        run.report_leftover_candidates();

        // Clean up assets that became locationless after stale removal
        run.purge_orphaned_assets()?;

        // After sync: count variants that have lost all their locations but
        // whose asset still has other variants (so it wasn't removed above).
        // These linger in the catalog — including, often, as the *selected*
        // best variant for preview — until `maki cleanup --apply` removes
        // them. The CLI uses this count to surface a next-step hint.
        //
        // Always computed (apply or dry-run) so dry-runs can hint about
        // pre-existing locationless variants too.
        if let Ok(locationless) = run.catalog.list_locationless_variants() {
            run.result.locationless_after = locationless.len();
        }

        Ok(run.result)
    }

    /// Apply a modified recipe: update catalog hash, re-extract XMP if applicable, update sidecar.
    pub(super) fn apply_modified_recipe(
        &self,
        catalog: &Catalog,
        metadata_store: &MetadataStore,
        recipe_id: &str,
        new_hash: &str,
        variant_hash: &str,
        volume: &Volume,
        file_path: &Path,
        relative_path: &str,
    ) -> Result<()> {
        // Compute the new asset state, save the sidecar (source of truth),
        // then apply every catalog row in one transaction.
        let mut xmp_asset: Option<Asset> = None;
        if let Some(asset_id_str) = catalog.find_asset_id_by_variant(variant_hash)? {
            let asset_uuid: Uuid = asset_id_str.parse()?;
            let mut asset = metadata_store.load(asset_uuid)?;
            if let Some(recipe) = asset.recipes.iter_mut().find(|r| {
                r.location.volume_id == volume.id
                    && r.location.relative_path.to_string_lossy() == relative_path
            }) {
                recipe.content_hash = new_hash.to_string();

                let ext = file_path
                    .extension()
                    .and_then(|e| e.to_str())
                    .unwrap_or("");
                let is_xmp = ext.eq_ignore_ascii_case("xmp");
                if is_xmp {
                    let xmp = crate::xmp_reader::extract(file_path);
                    xmp.reapply_to(&mut asset, variant_hash);
                }

                metadata_store.save(&asset)?;
                if is_xmp {
                    xmp_asset = Some(asset);
                }
            }
        }

        catalog.with_tx(|c| {
            c.update_recipe_content_hash(recipe_id, new_hash)?;
            if let Some(asset) = &xmp_asset {
                c.insert_asset(asset)?;
                if let Some(v) = asset.variants.iter().find(|v| v.content_hash == variant_hash) {
                    c.insert_variant(v)?;
                }
            }
            Ok(())
        })
    }

    /// Apply a "media file at same path, different content" change.
    /// Treats the re-exported file as a full new variant (not just a
    /// stub): EXIF / dimensions / embedded-XMP are extracted from the
    /// new bytes and the preview is regenerated, matching what `maki
    /// import` would do for the same file. The previous variant's
    /// file_location at this path is dropped; the new variant inherits
    /// `role` and `original_filename` from the previous variant (CO
    /// re-exports keep the same role=export and filename); format
    /// re-derived from the file extension.
    ///
    /// The previous variant stays in the catalog with whatever other
    /// locations it had (a backup volume that hasn't been rsynced yet
    /// still references the previous bytes). If it becomes truly
    /// locationless across all volumes, `maki cleanup --apply` removes
    /// it (and any orphaned previews/embeddings keyed by the old hash).
    pub(super) fn apply_modified_media(
        &self,
        catalog: &Catalog,
        metadata_store: &MetadataStore,
        old_hash: &str,
        new_hash: &str,
        volume: &Volume,
        file_path: &Path,
        relative_path: &str,
    ) -> Result<()> {
        let vol_id_str = volume.id.to_string();

        // Find the asset that owned the old variant. If no owning asset
        // is found (edge case: the old variant was already orphaned),
        // we still drop the stale location and return; the user can
        // re-import to pick up the new file.
        let asset_id_str = match catalog.find_asset_id_by_variant(old_hash)? {
            Some(id) => id,
            None => {
                catalog.delete_file_location(old_hash, &vol_id_str, relative_path)?;
                return Ok(());
            }
        };
        let asset_uuid: Uuid = asset_id_str.parse()?;
        let mut asset = metadata_store.load(asset_uuid)?;

        let file_size = std::fs::metadata(file_path).map(|m| m.len()).unwrap_or(0);
        let ext = file_path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_lowercase();

        // Inherit role + original_filename from the previous variant
        // (still in the sidecar even though its file_location at this
        // path is gone) — CO re-exports keep both unchanged.
        let (inherited_role, inherited_filename) = asset
            .variants
            .iter()
            .find(|v| v.content_hash == old_hash)
            .map(|v| (v.role.clone(), v.original_filename.clone()))
            .unwrap_or_else(|| {
                (
                    crate::models::variant::VariantRole::Export,
                    file_path
                        .file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or("")
                        .to_string(),
                )
            });

        // Re-extract EXIF / dimensions from the fresh bytes — same
        // path that `maki import` takes. A re-export usually carries
        // updated exposure / develop settings, so this matters for
        // facet queries (camera / lens / ISO are typically unchanged
        // but width / height may shift if the export size changed).
        let exif_data = crate::exif_reader::extract(file_path);

        let new_location = crate::models::FileLocation {
            volume_id: volume.id,
            relative_path: PathBuf::from(relative_path),
            verified_at: Some(chrono::Utc::now()),
        };
        let new_variant = crate::models::Variant {
            content_hash: new_hash.to_string(),
            asset_id: asset_uuid,
            role: inherited_role,
            format: ext.clone(),
            file_size,
            original_filename: inherited_filename,
            source_metadata: exif_data.source_metadata.into_iter().collect(),
            locations: vec![new_location.clone()],
        };

        // Sidecar side: drop the old variant's location at this path,
        // add the new variant. We preserve the old variant entry in the
        // sidecar even if its location list becomes empty — that
        // mirrors the catalog's "locationless variant" state and lets
        // `maki cleanup --apply` remove it explicitly when the user is
        // ready (multi-volume setups may still have the old bytes on a
        // backup drive that hasn't been rsynced yet).
        for v in asset.variants.iter_mut() {
            if v.content_hash == old_hash {
                v.locations.retain(|loc| {
                    !(loc.volume_id == volume.id
                        && loc.relative_path.to_string_lossy() == relative_path)
                });
            }
        }
        if !asset.variants.iter().any(|v| v.content_hash == new_hash) {
            asset.variants.push(new_variant.clone());
        }

        // Embedded XMP (JPEG/TIFF) often carries keyword/description
        // updates that should be merged into the catalog. Same path
        // `maki import` and `maki refresh --media` use.
        let embedded_xmp = crate::embedded_xmp::extract_embedded_xmp(file_path);
        if !embedded_xmp.keywords.is_empty()
            || embedded_xmp.description.is_some()
            || !embedded_xmp.source_metadata.is_empty()
        {
            embedded_xmp.apply_to(&mut asset, new_hash);
        }

        // Sidecar (source of truth) first, then every catalog row in one
        // transaction: drop the old location, add the new variant and its
        // location, refresh the asset row and denormalized columns.
        metadata_store.save(&asset)?;
        catalog.with_tx(|c| {
            c.delete_file_location(old_hash, &vol_id_str, relative_path)?;
            c.insert_variant(&new_variant)?;
            c.insert_file_location(new_hash, &new_location)?;
            c.insert_asset(&asset)?;
            Ok(())
        })?;

        // Regenerate previews for the new hash. The preview-by-hash
        // path means an old preview keyed by old_hash stays on disk
        // until cleanup; the new preview is what MAKI now serves for
        // this asset. Previews are a regenerable cache (also produced
        // on demand), so a generation failure is not an error here.
        let preview_gen = crate::preview::PreviewGenerator::new(
            &self.catalog_root,
            self.verbosity,
            &self.preview_config,
        );
        let _ = preview_gen.generate(new_hash, file_path, &ext);
        let _ = preview_gen.generate_smart(new_hash, file_path, &ext);

        Ok(())
    }

    /// Update a file location's relative_path in the sidecar YAML.
    pub(super) fn update_sidecar_file_location_path(
        &self,
        metadata_store: &MetadataStore,
        catalog: &Catalog,
        content_hash: &str,
        volume_id: Uuid,
        old_path: &str,
        new_path: &str,
    ) -> Result<()> {
        let asset_id = match catalog.find_asset_id_by_variant(content_hash)? {
            Some(id) => id,
            None => return Ok(()),
        };
        let uuid: Uuid = asset_id.parse()?;
        let mut asset = metadata_store.load(uuid)?;

        let mut changed = false;
        for variant in &mut asset.variants {
            if variant.content_hash == content_hash {
                for loc in &mut variant.locations {
                    if loc.volume_id == volume_id
                        && loc.relative_path.to_string_lossy() == old_path
                    {
                        loc.relative_path = PathBuf::from(new_path);
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

    /// Remove a file location from the sidecar YAML.
    pub fn remove_sidecar_file_location(
        &self,
        metadata_store: &MetadataStore,
        catalog: &Catalog,
        content_hash: &str,
        volume_id: Uuid,
        relative_path: &str,
    ) -> Result<()> {
        let asset_id = match catalog.find_asset_id_by_variant(content_hash)? {
            Some(id) => id,
            None => return Ok(()),
        };
        let uuid: Uuid = asset_id.parse()?;
        let mut asset = metadata_store.load(uuid)?;

        let mut changed = false;
        for variant in &mut asset.variants {
            if variant.content_hash == content_hash {
                let before = variant.locations.len();
                variant.locations.retain(|loc| {
                    !(loc.volume_id == volume_id
                        && loc.relative_path.to_string_lossy() == relative_path)
                });
                if variant.locations.len() != before {
                    changed = true;
                }
            }
        }

        if changed {
            metadata_store.save(&asset)?;
        }
        Ok(())
    }

    /// Update a recipe's relative_path in the sidecar YAML.
    pub(super) fn update_sidecar_recipe_path(
        &self,
        metadata_store: &MetadataStore,
        catalog: &Catalog,
        variant_hash: &str,
        volume_id: Uuid,
        old_path: &str,
        new_path: &str,
    ) -> Result<()> {
        let asset_id = match catalog.find_asset_id_by_variant(variant_hash)? {
            Some(id) => id,
            None => return Ok(()),
        };
        let uuid: Uuid = asset_id.parse()?;
        let mut asset = metadata_store.load(uuid)?;

        let mut changed = false;
        for recipe in &mut asset.recipes {
            if recipe.location.volume_id == volume_id
                && recipe.location.relative_path.to_string_lossy() == old_path
            {
                recipe.location.relative_path = PathBuf::from(new_path);
                changed = true;
            }
        }

        if changed {
            metadata_store.save(&asset)?;
        }
        Ok(())
    }

    /// Remove a recipe from the sidecar YAML by matching volume_id + relative_path.
    pub fn remove_sidecar_recipe(
        &self,
        metadata_store: &MetadataStore,
        catalog: &Catalog,
        variant_hash: &str,
        volume_id: Uuid,
        relative_path: &str,
    ) -> Result<()> {
        let asset_id = match catalog.find_asset_id_by_variant(variant_hash)? {
            Some(id) => id,
            None => return Ok(()),
        };
        let uuid: Uuid = asset_id.parse()?;
        let mut asset = metadata_store.load(uuid)?;

        let before = asset.recipes.len();
        asset.recipes.retain(|r| {
            !(r.location.volume_id == volume_id
                && r.location.relative_path.to_string_lossy() == relative_path)
        });

        if asset.recipes.len() != before {
            metadata_store.save(&asset)?;
        }
        Ok(())
    }

}

/// One `sync` in progress: stores, the scanned volume, the options, the
/// result under construction, and the disk-vs-catalog bookkeeping the
/// passes share.
struct SyncRun<'a, F> {
    service: &'a AssetService,
    content_store: ContentStore,
    metadata_store: MetadataStore,
    catalog: Catalog,
    filter: FileTypeFilter,
    volume: &'a Volume,
    vol_id: String,
    apply: bool,
    remove_stale: bool,
    result: SyncResult,
    /// Volume-relative paths seen on disk (media / recipes).
    disk_media_paths: HashSet<String>,
    disk_recipe_paths: HashSet<String>,
    /// Move candidates: a known content hash seen at a location the catalog
    /// does not have → (new relative path, full path).
    media_hash_to_new_path: HashMap<String, (String, PathBuf)>,
    recipe_hash_to_new_path: HashMap<String, (String, PathBuf)>,
    on_file: F,
}

impl<F: Fn(&Path, SyncStatus, Duration)> SyncRun<'_, F> {
    /// Pass 1, one file: classify it against the catalog's location at the
    /// same path. Known hashes at unknown paths become move candidates.
    fn scan_disk_file(&mut self, file_path: &Path) -> Result<()> {
        let file_start = Instant::now();

        let ext = file_path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("");

        // Skip files not in any known type group
        if !ext.is_empty() && !self.filter.is_importable(ext) {
            return Ok(());
        }

        let relative_path = match file_path.strip_prefix(&self.volume.mount_point) {
            // Normalize the OS-native separator to forward slashes
            // so the string matches the catalog's stored form
            // (`FileLocation::relative_path_str` always emits `/`,
            // regardless of platform). Without this, every Windows
            // sync against subdirectory'd content saw the disk
            // paths as `\`-separated and the catalog paths as
            // `/`-separated, causing modified files to look like
            // (new + missing) and silently breaking move detection
            // for non-root paths.
            Ok(rp) => crate::path_util::forward_slashes(rp),
            Err(_) => {
                self.result.errors.push(format!(
                    "File {} is not under volume mount point {}",
                    file_path.display(),
                    self.volume.mount_point.display()
                ));
                return Ok(());
            }
        };

        let hash = match self.content_store.hash_file(file_path) {
            Ok(h) => h,
            Err(e) => {
                self.result.errors.push(format!("{}: {}", file_path.display(), e));
                return Ok(());
            }
        };

        if self.filter.is_recipe(ext) {
            self.scan_disk_recipe(file_path, relative_path, hash, file_start)
        } else {
            self.scan_disk_media(file_path, relative_path, hash, file_start)
        }
    }

    fn scan_disk_recipe(
        &mut self,
        file_path: &Path,
        relative_path: String,
        hash: String,
        file_start: Instant,
    ) -> Result<()> {
        self.disk_recipe_paths.insert(relative_path.clone());

        // Look up recipe by location
        match self.catalog.find_recipe_by_volume_and_path(&self.vol_id, &relative_path)? {
            Some((recipe_id, stored_hash, variant_hash)) => {
                if stored_hash == hash {
                    // Unchanged recipe
                    self.result.unchanged += 1;
                    (self.on_file)(file_path, SyncStatus::Unchanged, file_start.elapsed());
                } else {
                    // Modified recipe (content changed at same path)
                    self.result.modified += 1;
                    if self.apply {
                        self.service.apply_modified_recipe(
                            &self.catalog,
                            &self.metadata_store,
                            &recipe_id,
                            &hash,
                            &variant_hash,
                            self.volume,
                            file_path,
                            &relative_path,
                        )?;
                    }
                    (self.on_file)(file_path, SyncStatus::Modified, file_start.elapsed());
                }
            }
            None => {
                // Not at this location — could be moved or new
                if self.catalog.has_recipe_by_content_hash(&hash)? {
                    // Known hash at different location → potential move
                    self.recipe_hash_to_new_path
                        .insert(hash, (relative_path, file_path.to_path_buf()));
                } else {
                    // Completely new recipe file
                    self.result.new_files += 1;
                    (self.on_file)(file_path, SyncStatus::New, file_start.elapsed());
                }
            }
        }
        Ok(())
    }

    fn scan_disk_media(
        &mut self,
        file_path: &Path,
        relative_path: String,
        hash: String,
        file_start: Instant,
    ) -> Result<()> {
        self.disk_media_paths.insert(relative_path.clone());

        // Look up media file by location
        match self.catalog.find_variant_by_volume_and_path(&self.vol_id, &relative_path)? {
            Some((stored_hash, _format)) => {
                if stored_hash == hash {
                    // Unchanged — optionally update verified_at
                    self.result.unchanged += 1;
                    if self.apply {
                        self.catalog.update_verified_at(&hash, &self.vol_id, &relative_path)?;
                    }
                    (self.on_file)(file_path, SyncStatus::Unchanged, file_start.elapsed());
                } else {
                    // Media file at the same path now has a different
                    // content hash. Common case: CaptureOne / Lightroom
                    // re-exported a JPEG over the previous one with new
                    // edits applied. Mirrors the recipe-modified path —
                    // the catalog catches up to the new on-disk state.
                    //
                    // Variants are content-addressed, so unlike recipes
                    // we can't just swap the stored hash. Instead:
                    //   - Drop the file_location row for the old hash.
                    //   - Create a stub variant for the new hash,
                    //     inheriting role/format/original_filename from
                    //     the previous variant.
                    //   - Insert a file_location for the new hash here.
                    //   - Update the asset's sidecar so the new variant
                    //     is attached and the old one loses this loc.
                    //   - `maki cleanup --apply` removes the now-
                    //     locationless old variant once the user is
                    //     ready (we don't auto-cleanup here so users on
                    //     multi-volume setups don't lose the old
                    //     variant when a backup still has the previous
                    //     bytes).
                    self.result.modified += 1;
                    if self.apply {
                        if let Err(e) = self.service.apply_modified_media(
                            &self.catalog,
                            &self.metadata_store,
                            &stored_hash,
                            &hash,
                            self.volume,
                            file_path,
                            &relative_path,
                        ) {
                            self.result.errors.push(format!(
                                "Failed to apply media change at {relative_path}: {e:#}"
                            ));
                        }
                    }
                    (self.on_file)(file_path, SyncStatus::Modified, file_start.elapsed());
                }
            }
            None => {
                // Not at this location — could be moved or new
                if self.catalog.has_variant(&hash)? {
                    // Known hash at different location → potential move
                    self.media_hash_to_new_path
                        .insert(hash, (relative_path, file_path.to_path_buf()));
                } else {
                    // Completely new file
                    self.result.new_files += 1;
                    (self.on_file)(file_path, SyncStatus::New, file_start.elapsed());
                }
            }
        }
        Ok(())
    }

    /// Pass 2, media: every catalog location under `prefix` that was not
    /// seen on disk is either moved (its hash turned up elsewhere) or
    /// missing.
    fn reconcile_media_locations(&mut self, prefix: &str) -> Result<()> {
        let catalog_locations = self
            .catalog
            .list_locations_for_volume_under_prefix(&self.vol_id, prefix)?;

        for (content_hash, cat_path) in &catalog_locations {
            if self.disk_media_paths.contains(cat_path.as_str()) {
                continue; // Already handled in Pass 1
            }

            let file_start = Instant::now();

            if let Some((new_path, full_path)) = self.media_hash_to_new_path.remove(content_hash) {
                // File was moved
                self.result.moved += 1;
                if self.apply {
                    // Sidecar (source of truth) first, then the catalog row.
                    self.service.update_sidecar_file_location_path(
                        &self.metadata_store,
                        &self.catalog,
                        content_hash,
                        self.volume.id,
                        cat_path,
                        &new_path,
                    )?;
                    self.catalog.update_file_location_path(
                        content_hash,
                        &self.vol_id,
                        cat_path,
                        &new_path,
                    )?;
                }
                (self.on_file)(&full_path, SyncStatus::Moved, file_start.elapsed());
            } else {
                // File is missing from disk
                self.result.missing += 1;
                let full_path = self.volume.mount_point.join(cat_path);
                if self.apply && self.remove_stale {
                    self.service.remove_sidecar_file_location(
                        &self.metadata_store,
                        &self.catalog,
                        content_hash,
                        self.volume.id,
                        cat_path,
                    )?;
                    self.catalog.delete_file_location(content_hash, &self.vol_id, cat_path)?;
                    self.result.stale_removed += 1;
                }
                (self.on_file)(&full_path, SyncStatus::Missing, file_start.elapsed());
            }
        }
        Ok(())
    }

    /// Pass 2, recipes: same as the media pass for recipe rows.
    fn reconcile_recipe_locations(&mut self, prefix: &str) -> Result<()> {
        let catalog_recipes = self
            .catalog
            .list_recipes_for_volume_under_prefix(&self.vol_id, prefix)?;

        for (recipe_id, content_hash, variant_hash, cat_path) in &catalog_recipes {
            if self.disk_recipe_paths.contains(cat_path.as_str()) {
                continue; // Already handled in Pass 1
            }

            let file_start = Instant::now();

            if let Some((new_path, full_path)) = self.recipe_hash_to_new_path.remove(content_hash) {
                // Recipe was moved
                self.result.moved += 1;
                if self.apply {
                    self.service.update_sidecar_recipe_path(
                        &self.metadata_store,
                        &self.catalog,
                        variant_hash,
                        self.volume.id,
                        cat_path,
                        &new_path,
                    )?;
                    self.catalog.update_recipe_relative_path(recipe_id, &new_path)?;
                }
                (self.on_file)(&full_path, SyncStatus::Moved, file_start.elapsed());
            } else {
                // Recipe is missing from disk
                self.result.missing += 1;
                let full_path = self.volume.mount_point.join(cat_path);
                if self.apply && self.remove_stale {
                    if let Err(e) = self.service.remove_sidecar_recipe(
                        &self.metadata_store,
                        &self.catalog,
                        variant_hash,
                        self.volume.id,
                        cat_path,
                    ) {
                        self.result.errors.push(format!(
                            "Failed to remove recipe from sidecar for {cat_path}: {e}"
                        ));
                    } else if let Err(e) = self.catalog.delete_recipe(recipe_id) {
                        self.result.errors.push(format!(
                            "Failed to delete stale recipe {cat_path}: {e}"
                        ));
                    } else {
                        self.result.stale_removed += 1;
                    }
                }
                (self.on_file)(&full_path, SyncStatus::Missing, file_start.elapsed());
            }
        }
        Ok(())
    }

    /// Any remaining move candidates are files that matched a hash but
    /// whose old location wasn't in our scanned prefixes — report as new.
    fn report_leftover_candidates(&mut self) {
        for (_path, full_path) in self.media_hash_to_new_path.values() {
            let file_start = Instant::now();
            self.result.new_files += 1;
            (self.on_file)(full_path, SyncStatus::New, file_start.elapsed());
        }
        for (_path, full_path) in self.recipe_hash_to_new_path.values() {
            let file_start = Instant::now();
            self.result.new_files += 1;
            (self.on_file)(full_path, SyncStatus::New, file_start.elapsed());
        }
    }

    /// Remove assets that lost their last location to stale removal
    /// (same purge as `cleanup`).
    fn purge_orphaned_assets(&mut self) -> Result<()> {
        if !(self.apply && self.remove_stale && self.result.stale_removed > 0) {
            return Ok(());
        }
        let orphaned = self.catalog.list_orphaned_asset_ids()?;
        let preview_gen = crate::preview::PreviewGenerator::new(
            &self.service.catalog_root,
            self.service.verbosity,
            &self.service.preview_config,
        );
        for asset_id in &orphaned {
            // Same purge as `cleanup`: all rows in one transaction, then
            // sidecar and derived files; failures land in `errors`
            // (formerly three swallowed `let _ =` writes).
            if self.service.purge_orphaned_asset(
                &self.catalog,
                &self.metadata_store,
                &preview_gen,
                asset_id,
                &mut self.result.errors,
            ) {
                self.result.orphaned_cleaned += 1;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use crate::asset_service::test_support::*;

    #[test]
    fn sync_apply_follows_a_moved_file_in_both_stores() {
        let env = ServiceEnv::new();
        let id = env.add("a.jpg", "moved on disk");
        std::fs::create_dir_all(env.abs("sub")).unwrap();
        std::fs::rename(env.abs("a.jpg"), env.abs("sub/a.jpg")).unwrap();

        let root = [env.vol_dir.path().to_path_buf()];
        let dry = env.service.sync(&root, &env.volume, false, false, &[], |_, _, _| {}).unwrap();
        assert_eq!(dry.moved, 1, "{dry:?}");
        assert_eq!(env.location_paths(&id), ["a.jpg"], "a dry run changes nothing");

        let applied = env.service.sync(&root, &env.volume, true, false, &[], |_, _, _| {}).unwrap();
        assert_eq!(applied.moved, 1, "{applied:?}");
        assert_eq!(env.location_paths(&id), ["sub/a.jpg"]);
        assert_eq!(env.location_rows(&id), 1);
    }

    #[test]
    fn sync_reports_new_and_missing_files_and_removes_stale_locations_on_request() {
        let env = ServiceEnv::new();
        let id = env.add("gone.jpg", "will be deleted");
        std::fs::remove_file(env.abs("gone.jpg")).unwrap();
        env.write("new.jpg", "not imported yet");

        let root = [env.vol_dir.path().to_path_buf()];
        let report = env.service.sync(&root, &env.volume, true, false, &[], |_, _, _| {}).unwrap();
        assert_eq!((report.new_files, report.missing, report.stale_removed), (1, 1, 0), "{report:?}");
        assert_eq!(env.location_rows(&id), 1, "missing files keep their location without --remove-stale");

        let removed = env.service.sync(&root, &env.volume, true, true, &[], |_, _, _| {}).unwrap();
        assert_eq!(removed.stale_removed, 1, "{removed:?}");
        assert_eq!(env.location_rows(&id), 0);
    }
}
