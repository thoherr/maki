//! `import` section of `AssetService` — methods extracted from the original
//! 8.9-kLOC asset_service.rs into a multi-file `impl AssetService` block.
//!
//! Free functions and result types live in the parent `asset_service` module.
//!
//! The run is split along its phases (architecture audit F13): the public
//! entry point resolves files and groups them by stem; an [`ImportRun`]
//! then processes each group in three passes — media files (which create
//! or extend the group's asset), recipe files (which attach to the group's
//! asset, to a parent found by stem, or become a standalone asset), and the
//! optional `[writeback] create_sidecars` pass. Counters live in one
//! [`ImportTally`] and every per-file status is recorded in exactly one
//! place (`ImportRun::report`).

use super::*;

/// Running counters of one import, folded into [`ImportResult`] at the end.
#[derive(Default)]
struct ImportTally {
    imported: usize,
    locations_added: usize,
    skipped: usize,
    recipes_attached: usize,
    recipes_location_added: usize,
    recipes_updated: usize,
    previews_generated: usize,
    smart_previews_generated: usize,
    sidecars_created: usize,
    new_asset_ids: Vec<String>,
    imported_dirs: HashSet<String>,
}

impl ImportTally {
    /// Bump the counter that belongs to a per-file status.
    fn count(&mut self, status: &FileStatus) {
        match status {
            FileStatus::Imported => self.imported += 1,
            FileStatus::LocationAdded => self.locations_added += 1,
            FileStatus::Skipped => self.skipped += 1,
            FileStatus::RecipeAttached => self.recipes_attached += 1,
            FileStatus::RecipeLocationAdded => self.recipes_location_added += 1,
            FileStatus::RecipeUpdated => self.recipes_updated += 1,
        }
    }

    fn into_result(self, dry_run: bool, import_roots: Vec<String>) -> ImportResult {
        ImportResult {
            dry_run,
            imported: self.imported,
            locations_added: self.locations_added,
            skipped: self.skipped,
            recipes_attached: self.recipes_attached,
            recipes_location_added: self.recipes_location_added,
            recipes_updated: self.recipes_updated,
            previews_generated: self.previews_generated,
            smart_previews_generated: self.smart_previews_generated,
            sidecars_created: self.sidecars_created,
            new_asset_ids: self.new_asset_ids,
            imported_directories: self.imported_dirs.into_iter().collect(),
            import_roots,
        }
    }
}

/// The asset a stem group's files belong to, as far as the group has been
/// processed. Media files establish it (the first one creates or finds the
/// asset); recipe files attach to its primary variant.
enum GroupOwner {
    /// No media file of the group has been processed yet (or the group has
    /// none): recipes look for a parent by stem or become standalone assets.
    Unknown,
    /// The asset holding the group's primary variant (boxed: the variant
    /// is the whole state of a group, the `Unknown` arm carries nothing).
    Known {
        asset: Box<Asset>,
        primary_variant_hash: String,
    },
}

/// Per-run state of `import_with_callback`: the stores, the target volume
/// and the options every file sees, plus the tally and the progress
/// callback.
struct ImportRun<'a, F> {
    content_store: ContentStore,
    metadata_store: MetadataStore,
    catalog: Catalog,
    preview_gen: crate::preview::PreviewGenerator,
    volume: &'a Volume,
    auto_tags: &'a [String],
    dry_run: bool,
    smart: bool,
    tally: ImportTally,
    on_file: F,
}

impl AssetService {
    // ═══ IMPORT ═══

    /// Import files: hash, deduplicate, create assets/variants, write sidecars, insert into DB.
    pub fn import(
        &self,
        paths: &[PathBuf],
        volume: &Volume,
        filter: &FileTypeFilter,
    ) -> Result<ImportResult> {
        self.import_with_callback(paths, volume, filter, &[], &[], false, false, |_, _, _| {})
    }

    /// Import files with a per-file callback reporting path, status, and elapsed time.
    /// With `dry_run`, reports what would happen without writing to catalog, sidecar, or disk.
    /// With `smart`, generates smart previews (2560px) alongside regular previews.
    #[allow(clippy::too_many_arguments)]
    pub fn import_with_callback(
        &self,
        paths: &[PathBuf],
        volume: &Volume,
        filter: &FileTypeFilter,
        exclude_patterns: &[String],
        auto_tags: &[String],
        dry_run: bool,
        smart: bool,
        on_file: impl FnMut(&Path, FileStatus, Duration),
    ) -> Result<ImportResult> {
        let catalog = Catalog::open(&self.catalog_root)?;
        if !dry_run {
            catalog.ensure_volume(volume)?;
        }

        let files = resolve_files(paths, exclude_patterns);
        let groups = group_by_stem(&files, filter);

        // `[writeback] enabled` + `create_sidecars`: assets that leave this
        // import with metadata (auto_tags, embedded XMP) but no `.xmp`
        // recipe get a sidecar right away, so the tags assigned at import
        // time reach Capture One / Lightroom without a separate
        // `maki create-sidecars` pass.
        let wb_cfg = &self.config.writeback;
        let auto_create_sidecars = wb_cfg.enabled && wb_cfg.create_sidecars && !dry_run;

        if self.verbosity.verbose {
            eprintln!("  Import: {} file(s) resolved, {} group(s)", files.len(), groups.len());
        }

        // Volume-relative import roots for `--auto-group` session scoping:
        // a directory argument is a root as given, a file argument
        // contributes its parent directory.
        let import_roots: Vec<String> = {
            let mut roots: HashSet<String> = HashSet::new();
            for p in paths {
                let dir = if p.is_dir() { p.as_path() } else { p.parent().unwrap_or(p.as_path()) };
                if let Ok(rel) = dir.strip_prefix(&volume.mount_point) {
                    roots.insert(crate::path_util::forward_slashes(rel));
                }
            }
            roots.into_iter().collect()
        };

        let mut run = ImportRun {
            content_store: ContentStore::new(&self.catalog_root),
            metadata_store: MetadataStore::new(&self.catalog_root),
            catalog,
            preview_gen: crate::preview::PreviewGenerator::new(
                &self.catalog_root,
                self.verbosity,
                &self.preview_config,
            ),
            volume,
            auto_tags,
            dry_run,
            smart,
            tally: ImportTally::default(),
            on_file,
        };

        for group in &groups {
            let mut owner = GroupOwner::Unknown;

            // Pass 1: media files (RAW first due to sorting in group_by_stem)
            for file_path in &group.media_files {
                run.import_media_file(&group.stem, file_path, &mut owner)?;
            }

            // Pass 2: recipe files
            for file_path in &group.recipe_files {
                run.import_recipe_file(file_path, &mut owner)?;
            }

            // Pass 3: auto-create a sidecar for the group's asset (see above).
            // Idempotent: a recipe attached in Pass 2, or an unregistered
            // `.xmp` already on disk, makes the helper skip the location.
            if auto_create_sidecars {
                if let GroupOwner::Known { asset, .. } = &mut owner {
                    run.auto_create_sidecar(asset, &group.stem, wb_cfg.sidecar_naming)?;
                }
            }
        }

        Ok(run.tally.into_result(dry_run, import_roots))
    }
}

impl<F: FnMut(&Path, FileStatus, Duration)> ImportRun<'_, F> {
    /// Count a file's outcome and report it to the caller.
    fn report(&mut self, file_path: &Path, status: FileStatus, started: Instant) {
        self.tally.count(&status);
        (self.on_file)(file_path, status, started.elapsed());
    }

    /// The volume-relative location of a file under this run's volume.
    fn location_for(&self, file_path: &Path) -> Result<FileLocation> {
        let relative_path = file_path
            .strip_prefix(&self.volume.mount_point)
            .with_context(|| {
                format!(
                    "File {} is not under volume mount point {}",
                    file_path.display(),
                    self.volume.mount_point.display()
                )
            })?;
        Ok(FileLocation {
            volume_id: self.volume.id,
            relative_path: relative_path.to_path_buf(),
            verified_at: None,
        })
    }

    /// Hash a file into the content store.
    fn ingest(&self, file_path: &Path) -> Result<String> {
        self.content_store
            .ingest(file_path, self.volume)
            .with_context(|| format!("Failed to hash {}", file_path.display()))
    }

    /// Generate the regular (and, with `smart`, the smart) preview for a
    /// freshly imported variant. Previews are a regenerable cache, so a
    /// failure is a warning, not an error.
    fn generate_previews_for(&mut self, content_hash: &str, file_path: &Path, ext: &str) {
        match self.preview_gen.generate(content_hash, file_path, ext) {
            Ok(Some(_)) => self.tally.previews_generated += 1,
            Ok(None) => {}
            Err(e) => eprintln!("  Preview warning: {e:#}"),
        }
        if self.smart {
            match self.preview_gen.generate_smart(content_hash, file_path, ext) {
                Ok(Some(_)) => self.tally.smart_previews_generated += 1,
                Ok(None) => {}
                Err(e) => eprintln!("  Smart preview warning: {e:#}"),
            }
        }
    }

    // ── Pass 1: media files ──────────────────────────────────────────

    /// Process one media file of a stem group: a known variant gets this
    /// location recorded (or is skipped), a new one creates the group's
    /// asset or joins it as an additional variant.
    fn import_media_file(
        &mut self,
        stem: &str,
        file_path: &Path,
        owner: &mut GroupOwner,
    ) -> Result<()> {
        let file_start = Instant::now();

        // Track volume-relative directory for auto-group neighborhood
        if let Ok(rel) = file_path.strip_prefix(&self.volume.mount_point) {
            if let Some(parent) = rel.parent() {
                self.tally
                    .imported_dirs
                    .insert(crate::path_util::forward_slashes(parent));
            }
        }

        let content_hash = self.ingest(file_path)?;

        if self.catalog.has_variant(&content_hash)? {
            // Variant exists — check if we should add a new location. Even
            // when skipped, its asset becomes the group asset if needed.
            let asset = self.track_known_variant(file_path, &content_hash, file_start)?;
            if matches!(owner, GroupOwner::Unknown) {
                *owner = GroupOwner::Known {
                    asset: Box::new(asset),
                    primary_variant_hash: content_hash,
                };
            }
            return Ok(());
        }

        // New variant
        let ext = file_path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("");
        let filename = file_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("unknown")
            .to_string();
        let file_size = std::fs::metadata(file_path)
            .with_context(|| format!("Failed to read metadata for {}", file_path.display()))?
            .len();
        let location = self.location_for(file_path)?;
        let file = NewMediaFile {
            path: file_path,
            ext,
            filename,
            file_size,
            location,
            content_hash: &content_hash,
        };

        match owner {
            GroupOwner::Unknown => {
                // First new media file creates the asset
                let asset = self.create_asset(stem, &file)?;
                *owner = GroupOwner::Known {
                    asset: Box::new(asset),
                    primary_variant_hash: content_hash.clone(),
                };
            }
            GroupOwner::Known { asset, .. } => {
                // Additional media file → add variant to existing group asset.
                self.add_variant(asset, &file)?;
            }
        }

        self.report(file_path, FileStatus::Imported, file_start);
        Ok(())
    }

    /// A media file whose content hash is already a variant: record this
    /// location on it unless the location is already tracked. Returns the
    /// owning asset so the caller can adopt it for the group.
    fn track_known_variant(
        &mut self,
        file_path: &Path,
        content_hash: &str,
        file_start: Instant,
    ) -> Result<Asset> {
        let location = self.location_for(file_path)?;

        let asset_id = self
            .catalog
            .find_asset_id_by_variant(content_hash)?
            .with_context(|| format!("Variant {} exists but no owning asset found", content_hash))?;
        let asset_id: Uuid = asset_id
            .parse()
            .with_context(|| format!("Invalid asset UUID: {}", asset_id))?;
        let mut asset = self.metadata_store.load(asset_id)?;

        // Find the variant and check if this exact location already exists
        let status = match asset
            .variants
            .iter_mut()
            .find(|v| v.content_hash == content_hash)
        {
            Some(variant) => {
                let already_tracked = variant.locations.iter().any(|l| {
                    l.volume_id == location.volume_id && l.relative_path == location.relative_path
                });
                if already_tracked {
                    FileStatus::Skipped
                } else {
                    if !self.dry_run {
                        variant.locations.push(location.clone());
                        self.metadata_store.save(&asset)?;
                        self.catalog.insert_file_location(content_hash, &location)?;
                    }
                    FileStatus::LocationAdded
                }
            }
            None => FileStatus::Skipped,
        };
        self.report(file_path, status, file_start);
        Ok(asset)
    }

    /// The first new media file of a group creates its asset: EXIF date,
    /// auto-tags, the original variant, embedded XMP, then sidecar +
    /// catalog + previews.
    fn create_asset(&mut self, stem: &str, file: &NewMediaFile<'_>) -> Result<Asset> {
        let asset_type = determine_asset_type(file.ext);
        let exif_data = extract_file_metadata(file.path, file.ext);

        let mut asset = Asset::new(asset_type, file.content_hash);
        // Date fallback chain: EXIF DateTimeOriginal → file mtime → Utc::now()
        if let Some(date_taken) = exif_data.date_taken {
            asset.created_at = date_taken;
        } else if let Some(mtime) = file_mtime(file.path) {
            asset.created_at = mtime;
        }
        asset.name = Some(stem.to_string());

        // Apply auto_tags (merge, no duplicates). Source is
        // `user`: the user configured these deliberately.
        asset.add_tags_with_source(self.auto_tags, crate::models::TagSource::User);

        let variant = file.variant(asset.id, VariantRole::Original, exif_data.source_metadata);
        asset.variants.push(variant.clone());

        // Extract embedded XMP from JPEG/TIFF
        let embedded_xmp = crate::embedded_xmp::extract_embedded_xmp(file.path);
        if xmp_has_metadata(&embedded_xmp) {
            embedded_xmp.apply_to(&mut asset, file.content_hash);
        }

        if !self.dry_run {
            // Write sidecar + catalog immediately for first variant
            self.metadata_store.save(&asset).with_context(|| {
                format!("Failed to write sidecar for {}", file.path.display())
            })?;
            self.catalog.with_tx(|c| {
                c.insert_asset(&asset)?;
                c.insert_variant(&variant)?;
                c.insert_file_location(file.content_hash, &file.location)
            })?;

            // Generate preview for the newly imported variant
            self.generate_previews_for(file.content_hash, file.path, file.ext);
        }

        self.tally.new_asset_ids.push(asset.id.to_string());
        Ok(asset)
    }

    /// An additional media file of a group joins its asset as a variant
    /// (an alternate when the primary is RAW and this file is not).
    fn add_variant(&mut self, asset: &mut Asset, file: &NewMediaFile<'_>) -> Result<()> {
        let exif_data = extract_file_metadata(file.path, file.ext);

        // If this variant has an older date, update the asset's created_at
        let variant_date = exif_data.date_taken.or_else(|| file_mtime(file.path));
        if let Some(vd) = variant_date {
            if vd < asset.created_at {
                asset.created_at = vd;
            }
        }

        // If the primary variant is RAW and this file is not, it's an alternate
        let primary_is_raw = asset
            .variants
            .first()
            .map(|v| is_raw_extension(&v.format))
            .unwrap_or(false);
        let role = if primary_is_raw && !is_raw_extension(file.ext) {
            VariantRole::Alternate
        } else {
            VariantRole::Original
        };

        let variant = file.variant(asset.id, role, exif_data.source_metadata);

        if !self.dry_run {
            asset.variants.push(variant.clone());

            // Extract embedded XMP from JPEG/TIFF
            let embedded_xmp = crate::embedded_xmp::extract_embedded_xmp(file.path);
            if xmp_has_metadata(&embedded_xmp) {
                embedded_xmp.apply_to(asset, file.content_hash);
            }

            self.metadata_store.save(asset).with_context(|| {
                format!("Failed to write sidecar for {}", file.path.display())
            })?;
            self.catalog.with_tx(|c| {
                c.insert_asset(&*asset)?;
                c.insert_variant(&variant)?;
                c.insert_file_location(file.content_hash, &file.location)
            })?;

            // Generate preview for the additional variant
            self.generate_previews_for(file.content_hash, file.path, file.ext);
        }
        Ok(())
    }

    // ── Pass 2: recipe files ─────────────────────────────────────────

    /// Process one recipe file of a stem group: attach it to the group's
    /// asset when one is known, otherwise find its parent by stem or
    /// import it as a standalone asset.
    fn import_recipe_file(&mut self, file_path: &Path, owner: &mut GroupOwner) -> Result<()> {
        let file_start = Instant::now();
        let content_hash = self.ingest(file_path)?;
        let ext = file_path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("");

        match owner {
            GroupOwner::Known {
                asset,
                primary_variant_hash,
            } => {
                let location = self.location_for(file_path)?;
                let recipe = RecipeFile {
                    path: file_path,
                    ext,
                    content_hash: &content_hash,
                    location,
                };
                self.attach_recipe(asset, primary_variant_hash, &recipe, file_start)
            }
            GroupOwner::Unknown => self.import_orphan_recipe(file_path, ext, &content_hash, file_start),
        }
    }

    /// A recipe file whose group has no media file in this import: a known
    /// variant gets its location tracked, a parent variant found by stem
    /// and directory gets the recipe attached, anything else becomes a
    /// standalone `Other` asset.
    fn import_orphan_recipe(
        &mut self,
        file_path: &Path,
        ext: &str,
        content_hash: &str,
        file_start: Instant,
    ) -> Result<()> {
        if self.catalog.has_variant(content_hash)? {
            // Same dedup logic as media
            self.track_known_variant(file_path, content_hash, file_start)?;
            return Ok(());
        }

        // Try to find a parent variant by stem + directory on this volume
        let stem = file_path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("");
        let location = self.location_for(file_path)?;
        let dir_prefix = location
            .relative_path
            .parent()
            .unwrap_or_else(|| Path::new(""))
            .to_string_lossy()
            .to_string();

        if let Some((parent_variant_hash, parent_asset_id)) =
            self.find_parent_variant(stem, &dir_prefix)?
        {
            // Found parent variant — attach recipe to it
            let asset_uuid: Uuid = parent_asset_id.parse()?;
            let mut asset = self.metadata_store.load(asset_uuid)?;
            let recipe = RecipeFile {
                path: file_path,
                ext,
                content_hash,
                location,
            };
            return self.attach_recipe(&mut asset, &parent_variant_hash, &recipe, file_start);
        }

        // No parent found — import as standalone asset
        let filename = file_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("unknown")
            .to_string();
        let file_size = std::fs::metadata(file_path)?.len();
        let mut asset = Asset::new(AssetType::Other, content_hash);
        asset.name = Some(filename.clone());
        // Configured auto_tags are user curation (see above).
        asset.add_tags_with_source(self.auto_tags, crate::models::TagSource::User);
        let variant = Variant {
            content_hash: content_hash.to_string(),
            asset_id: asset.id,
            role: VariantRole::Original,
            format: ext.to_lowercase(),
            file_size,
            original_filename: filename,
            source_metadata: Default::default(),
            locations: vec![location.clone()],
        };
        if !self.dry_run {
            asset.variants.push(variant.clone());
            self.metadata_store.save(&asset)?;
            self.catalog.with_tx(|c| {
                c.insert_asset(&asset)?;
                c.insert_variant(&variant)?;
                c.insert_file_location(content_hash, &location)
            })?;
        }
        self.tally.new_asset_ids.push(asset.id.to_string());
        self.report(file_path, FileStatus::Imported, file_start);
        Ok(())
    }

    /// Look up the variant a recipe belongs to by stem and directory on
    /// this volume; `DSC_001.NEF.xmp` also tries `DSC_001`.
    fn find_parent_variant(&self, stem: &str, dir_prefix: &str) -> Result<Option<(String, String)>> {
        let volume_id = self.volume.id.to_string();
        let mut parent =
            self.catalog
                .find_variant_hash_by_stem_and_directory(stem, dir_prefix, &volume_id, None)?;
        let stripped_stem = strip_media_extension(stem);
        if parent.is_none() && stripped_stem != stem {
            parent = self.catalog.find_variant_hash_by_stem_and_directory(
                stripped_stem,
                dir_prefix,
                &volume_id,
                None,
            )?;
        }
        Ok(parent)
    }

    /// Attach a recipe file to `variant_hash` of `asset`. Location-based
    /// dedup: an existing recipe at the same location with the same hash is
    /// skipped, with a different hash it is updated in place (the file was
    /// modified externally); otherwise a new recipe is attached, merging its
    /// XMP metadata only when the content is genuinely new to the asset.
    fn attach_recipe(
        &mut self,
        asset: &mut Asset,
        variant_hash: &str,
        recipe: &RecipeFile<'_>,
        file_start: Instant,
    ) -> Result<()> {
        let is_xmp = recipe.ext.eq_ignore_ascii_case("xmp");

        // Location-based recipe dedup: find existing recipe at same location
        let existing = asset.recipes.iter().position(|r| {
            r.location.volume_id == self.volume.id
                && r.location.relative_path == recipe.location.relative_path
        });

        if let Some(idx) = existing {
            if asset.recipes[idx].content_hash == recipe.content_hash {
                // Same location, same hash — nothing changed
                self.report(recipe.path, FileStatus::Skipped, file_start);
                return Ok(());
            }
            // Same location, different hash — recipe was modified externally
            if !self.dry_run {
                let recipe_id_str = asset.recipes[idx].id.to_string();
                asset.recipes[idx].content_hash = recipe.content_hash.to_string();

                // Re-extract XMP metadata if applicable
                if is_xmp {
                    let xmp = crate::xmp_reader::extract(recipe.path);
                    xmp.reapply_to(asset, variant_hash);
                }

                // Sidecar first, then the catalog rows in one transaction.
                self.metadata_store.save(asset)?;
                self.catalog.with_tx(|c| {
                    c.update_recipe_content_hash(&recipe_id_str, recipe.content_hash)?;
                    if is_xmp {
                        c.insert_asset(&*asset)?;
                        if let Some(v) = asset.variants.iter().find(|v| v.content_hash == variant_hash) {
                            c.insert_variant(v)?;
                        }
                    }
                    Ok(())
                })?;
            }
            self.report(recipe.path, FileStatus::RecipeUpdated, file_start);
            return Ok(());
        }

        // Check if the asset already has a recipe with the same content
        // hash (on any volume). If so, the XMP content is identical to one
        // we've already processed — there's nothing new to merge. Just
        // record the location for backup tracking. This prevents
        // re-importing old metadata from a backup copy that was made
        // before tag renames, rating changes, etc.
        let already_known_content = asset
            .recipes
            .iter()
            .any(|r| r.content_hash == recipe.content_hash);

        if !self.dry_run {
            // No existing recipe at this location — attach new recipe
            let new_recipe = Recipe {
                id: Uuid::new_v4(),
                variant_hash: variant_hash.to_string(),
                software: determine_recipe_software(recipe.ext).to_string(),
                recipe_type: RecipeType::Sidecar,
                content_hash: recipe.content_hash.to_string(),
                location: recipe.location.clone(),
                pending_writeback: false,
            };
            asset.recipes.push(new_recipe.clone());

            // Extract metadata from XMP sidecars — but only if the content
            // is genuinely new (different hash from all existing recipes).
            let merge_xmp = is_xmp && !already_known_content;
            if merge_xmp {
                let xmp = crate::xmp_reader::extract(recipe.path);
                xmp.apply_to(asset, variant_hash);
            }

            // Sidecar first, then the catalog rows in one transaction.
            self.metadata_store.save(asset)?;
            self.catalog.with_tx(|c| {
                if merge_xmp {
                    c.insert_asset(&*asset)?;
                    if let Some(v) = asset.variants.iter().find(|v| v.content_hash == variant_hash) {
                        c.insert_variant(v)?;
                    }
                }
                c.insert_recipe(&new_recipe)
            })?;
        }

        let status = if already_known_content {
            FileStatus::RecipeLocationAdded
        } else {
            FileStatus::RecipeAttached
        };
        self.report(recipe.path, status, file_start);
        Ok(())
    }

    // ── Pass 3: sidecars ─────────────────────────────────────────────

    /// `[writeback] create_sidecars`: give the group's asset an `.xmp`
    /// sidecar on this volume when it carries metadata but has no recipe.
    fn auto_create_sidecar(
        &mut self,
        asset: &mut Asset,
        stem: &str,
        naming: crate::config::SidecarNaming,
    ) -> Result<()> {
        let online_here: HashMap<Uuid, PathBuf> =
            [(self.volume.id, self.volume.mount_point.clone())].into_iter().collect();
        match create_missing_sidecars(
            asset,
            &self.catalog,
            &self.content_store,
            &online_here,
            Some(self.volume.id),
            naming,
            true,
            |_, _| {},
        ) {
            Ok(outcome) if outcome.created > 0 => {
                self.metadata_store.save(asset)?;
                self.tally.sidecars_created += outcome.created;
            }
            Ok(_) => {}
            Err(e) => eprintln!("Warning: could not create XMP sidecar for {}: {e}", stem),
        }
        Ok(())
    }
}

/// A media file that is new to the catalog, with everything its variant
/// row needs.
struct NewMediaFile<'a> {
    path: &'a Path,
    ext: &'a str,
    filename: String,
    file_size: u64,
    location: FileLocation,
    content_hash: &'a str,
}

impl NewMediaFile<'_> {
    fn variant(
        &self,
        asset_id: Uuid,
        role: VariantRole,
        source_metadata: HashMap<String, String>,
    ) -> Variant {
        Variant {
            content_hash: self.content_hash.to_string(),
            asset_id,
            role,
            format: self.ext.to_lowercase(),
            file_size: self.file_size,
            original_filename: self.filename.clone(),
            source_metadata: source_metadata.into_iter().collect(),
            locations: vec![self.location.clone()],
        }
    }
}

/// A recipe file about to be attached to a variant.
struct RecipeFile<'a> {
    path: &'a Path,
    ext: &'a str,
    content_hash: &'a str,
    location: FileLocation,
}

/// EXIF plus, for video and audio files, the container metadata that
/// ffprobe / lofty provide.
fn extract_file_metadata(file_path: &Path, ext: &str) -> crate::exif_reader::ExifData {
    let mut exif_data = crate::exif_reader::extract(file_path);
    match determine_asset_type(ext) {
        AssetType::Video => {
            let video_meta = crate::preview::extract_video_metadata(file_path);
            exif_data.source_metadata.extend(video_meta);
        }
        AssetType::Audio => {
            let audio_meta = crate::preview::extract_audio_source_metadata(file_path);
            exif_data.source_metadata.extend(audio_meta);
        }
        _ => {}
    }
    exif_data
}

#[cfg(test)]
mod tests {
    use crate::asset_service::test_support::*;
    use crate::asset_service::*;

    #[test]
    fn import_duplicate_from_different_path_adds_location() {
        let catalog_dir = tempfile::tempdir().unwrap();
        setup_catalog(catalog_dir.path());

        // Create a volume with two copies of the same file at different paths
        let vol_dir = tempfile::tempdir().unwrap();
        let dir_a = vol_dir.path().join("a");
        let dir_b = vol_dir.path().join("b");
        std::fs::create_dir_all(&dir_a).unwrap();
        std::fs::create_dir_all(&dir_b).unwrap();
        std::fs::write(dir_a.join("photo.jpg"), "identical content").unwrap();
        std::fs::write(dir_b.join("photo.jpg"), "identical content").unwrap();

        let volume = crate::models::Volume::new(
            "test-vol".into(),
            vol_dir.path().to_path_buf(),
            crate::models::VolumeType::Local,
        );

        let service = AssetService::new(catalog_dir.path(), crate::Verbosity::quiet(), &crate::config::PreviewConfig::default());

        // First import
        let r1 = service.import(&[dir_a.join("photo.jpg")], &volume, &default_filter()).unwrap();
        assert_eq!(r1.imported, 1);
        assert_eq!(r1.locations_added, 0);
        assert_eq!(r1.skipped, 0);

        // Second import — same content, different path
        let r2 = service.import(&[dir_b.join("photo.jpg")], &volume, &default_filter()).unwrap();
        assert_eq!(r2.imported, 0);
        assert_eq!(r2.locations_added, 1);
        assert_eq!(r2.skipped, 0);

        // Verify sidecar has 2 locations
        let catalog = crate::catalog::Catalog::open(catalog_dir.path()).unwrap();
        let content_hash = crate::content_store::ContentStore::new(catalog_dir.path())
            .ingest(&dir_a.join("photo.jpg"), &volume)
            .unwrap();
        let asset_id_str = catalog.find_asset_id_by_variant(&content_hash).unwrap().unwrap();
        let asset_id: uuid::Uuid = asset_id_str.parse().unwrap();
        let metadata_store = crate::metadata_store::MetadataStore::new(catalog_dir.path());
        let asset = metadata_store.load(asset_id).unwrap();
        let variant = &asset.variants[0];
        assert_eq!(variant.locations.len(), 2);
        assert_eq!(
            variant.locations[0].relative_path,
            std::path::Path::new("a/photo.jpg")
        );
        assert_eq!(
            variant.locations[1].relative_path,
            std::path::Path::new("b/photo.jpg")
        );
    }

    #[test]
    fn import_duplicate_from_same_path_is_skipped() {
        let catalog_dir = tempfile::tempdir().unwrap();
        setup_catalog(catalog_dir.path());

        let vol_dir = tempfile::tempdir().unwrap();
        std::fs::write(vol_dir.path().join("photo.jpg"), "some content").unwrap();

        let volume = crate::models::Volume::new(
            "test-vol".into(),
            vol_dir.path().to_path_buf(),
            crate::models::VolumeType::Local,
        );

        let service = AssetService::new(catalog_dir.path(), crate::Verbosity::quiet(), &crate::config::PreviewConfig::default());

        // First import
        let r1 = service
            .import(&[vol_dir.path().join("photo.jpg")], &volume, &default_filter())
            .unwrap();
        assert_eq!(r1.imported, 1);

        // Second import — exact same path
        let r2 = service
            .import(&[vol_dir.path().join("photo.jpg")], &volume, &default_filter())
            .unwrap();
        assert_eq!(r2.imported, 0);
        assert_eq!(r2.locations_added, 0);
        assert_eq!(r2.skipped, 1);
    }

    #[test]
    fn import_raw_and_jpg_groups_into_one_asset() {
        let catalog_dir = tempfile::tempdir().unwrap();
        setup_catalog(catalog_dir.path());

        let vol_dir = tempfile::tempdir().unwrap();
        let photos = vol_dir.path().join("photos");
        std::fs::create_dir_all(&photos).unwrap();
        std::fs::write(photos.join("DSC_4521.nef"), "raw file content").unwrap();
        std::fs::write(photos.join("DSC_4521.jpg"), "jpeg file content").unwrap();

        let volume = crate::models::Volume::new(
            "test-vol".into(),
            vol_dir.path().to_path_buf(),
            crate::models::VolumeType::Local,
        );

        let service = AssetService::new(catalog_dir.path(), crate::Verbosity::quiet(), &crate::config::PreviewConfig::default());
        let result = service
            .import(std::slice::from_ref(&photos), &volume, &default_filter())
            .unwrap();

        assert_eq!(result.imported, 2);
        assert_eq!(result.recipes_attached, 0);

        // Verify: one asset with two variants
        let metadata_store = crate::metadata_store::MetadataStore::new(catalog_dir.path());
        let summaries = metadata_store.list().unwrap();
        assert_eq!(summaries.len(), 1);

        let asset = metadata_store.load(summaries[0].id).unwrap();
        assert_eq!(asset.variants.len(), 2);
        // RAW should be first (defines the asset)
        assert_eq!(asset.variants[0].format, "nef");
        assert_eq!(asset.variants[1].format, "jpg");
        // RAW is original, JPG is an alternate
        assert_eq!(asset.variants[0].role, VariantRole::Original);
        assert_eq!(asset.variants[1].role, VariantRole::Alternate);
        assert_eq!(asset.name.as_deref(), Some("DSC_4521"));
    }

    #[test]
    fn import_two_jpgs_both_original() {
        let catalog_dir = tempfile::tempdir().unwrap();
        setup_catalog(catalog_dir.path());

        let vol_dir = tempfile::tempdir().unwrap();
        let photos = vol_dir.path().join("photos");
        std::fs::create_dir_all(&photos).unwrap();
        std::fs::write(photos.join("photo.jpg"), "jpeg content").unwrap();
        std::fs::write(photos.join("photo.png"), "png content").unwrap();

        let volume = crate::models::Volume::new(
            "test-vol".into(),
            vol_dir.path().to_path_buf(),
            crate::models::VolumeType::Local,
        );

        let service = AssetService::new(catalog_dir.path(), crate::Verbosity::quiet(), &crate::config::PreviewConfig::default());
        let result = service
            .import(std::slice::from_ref(&photos), &volume, &default_filter())
            .unwrap();

        assert_eq!(result.imported, 2);

        let metadata_store = crate::metadata_store::MetadataStore::new(catalog_dir.path());
        let summaries = metadata_store.list().unwrap();
        assert_eq!(summaries.len(), 1);

        let asset = metadata_store.load(summaries[0].id).unwrap();
        assert_eq!(asset.variants.len(), 2);
        // No RAW present → both variants stay Original
        assert_eq!(asset.variants[0].role, VariantRole::Original);
        assert_eq!(asset.variants[1].role, VariantRole::Original);
    }

    #[test]
    fn import_raw_and_xmp_creates_asset_with_recipe() {
        let catalog_dir = tempfile::tempdir().unwrap();
        setup_catalog(catalog_dir.path());

        let vol_dir = tempfile::tempdir().unwrap();
        let photos = vol_dir.path().join("photos");
        std::fs::create_dir_all(&photos).unwrap();
        std::fs::write(photos.join("DSC_4521.nef"), "raw file data").unwrap();
        std::fs::write(photos.join("DSC_4521.xmp"), "xmp sidecar data").unwrap();

        let volume = crate::models::Volume::new(
            "test-vol".into(),
            vol_dir.path().to_path_buf(),
            crate::models::VolumeType::Local,
        );

        let service = AssetService::new(catalog_dir.path(), crate::Verbosity::quiet(), &crate::config::PreviewConfig::default());
        let result = service
            .import(std::slice::from_ref(&photos), &volume, &default_filter())
            .unwrap();

        assert_eq!(result.imported, 1);
        assert_eq!(result.recipes_attached, 1);

        let metadata_store = crate::metadata_store::MetadataStore::new(catalog_dir.path());
        let summaries = metadata_store.list().unwrap();
        assert_eq!(summaries.len(), 1);

        let asset = metadata_store.load(summaries[0].id).unwrap();
        assert_eq!(asset.variants.len(), 1);
        assert_eq!(asset.recipes.len(), 1);
        assert_eq!(asset.recipes[0].software, "Adobe/CaptureOne");
    }

    #[test]
    fn import_raw_jpg_xmp_cos_groups_correctly() {
        let catalog_dir = tempfile::tempdir().unwrap();
        setup_catalog(catalog_dir.path());

        let vol_dir = tempfile::tempdir().unwrap();
        let photos = vol_dir.path().join("photos");
        std::fs::create_dir_all(&photos).unwrap();
        std::fs::write(photos.join("DSC_4521.nef"), "raw data").unwrap();
        std::fs::write(photos.join("DSC_4521.jpg"), "jpeg data").unwrap();
        std::fs::write(photos.join("DSC_4521.xmp"), "xmp data").unwrap();
        std::fs::write(photos.join("DSC_4521.cos"), "cos data").unwrap();

        let volume = crate::models::Volume::new(
            "test-vol".into(),
            vol_dir.path().to_path_buf(),
            crate::models::VolumeType::Local,
        );

        let service = AssetService::new(catalog_dir.path(), crate::Verbosity::quiet(), &crate::config::PreviewConfig::default());
        let mut filter = FileTypeFilter::default();
        filter.include("captureone").unwrap();
        let result = service.import(std::slice::from_ref(&photos), &volume, &filter).unwrap();

        assert_eq!(result.imported, 2);
        assert_eq!(result.recipes_attached, 2);

        let metadata_store = crate::metadata_store::MetadataStore::new(catalog_dir.path());
        let summaries = metadata_store.list().unwrap();
        assert_eq!(summaries.len(), 1);

        let asset = metadata_store.load(summaries[0].id).unwrap();
        assert_eq!(asset.variants.len(), 2);
        assert_eq!(asset.recipes.len(), 2);
    }

    #[test]
    fn import_same_stem_different_dirs_creates_separate_assets() {
        let catalog_dir = tempfile::tempdir().unwrap();
        setup_catalog(catalog_dir.path());

        let vol_dir = tempfile::tempdir().unwrap();
        let dir_a = vol_dir.path().join("a");
        let dir_b = vol_dir.path().join("b");
        std::fs::create_dir_all(&dir_a).unwrap();
        std::fs::create_dir_all(&dir_b).unwrap();
        // Different content so they don't dedup
        std::fs::write(dir_a.join("photo.jpg"), "content A").unwrap();
        std::fs::write(dir_b.join("photo.jpg"), "content B").unwrap();

        let volume = crate::models::Volume::new(
            "test-vol".into(),
            vol_dir.path().to_path_buf(),
            crate::models::VolumeType::Local,
        );

        let service = AssetService::new(catalog_dir.path(), crate::Verbosity::quiet(), &crate::config::PreviewConfig::default());
        let result = service
            .import(&[dir_a, dir_b], &volume, &default_filter())
            .unwrap();

        assert_eq!(result.imported, 2);

        let metadata_store = crate::metadata_store::MetadataStore::new(catalog_dir.path());
        let summaries = metadata_store.list().unwrap();
        assert_eq!(summaries.len(), 2);
    }

    #[test]
    fn import_solo_file_works_unchanged() {
        let catalog_dir = tempfile::tempdir().unwrap();
        setup_catalog(catalog_dir.path());

        let vol_dir = tempfile::tempdir().unwrap();
        std::fs::write(vol_dir.path().join("solo.jpg"), "solo content").unwrap();

        let volume = crate::models::Volume::new(
            "test-vol".into(),
            vol_dir.path().to_path_buf(),
            crate::models::VolumeType::Local,
        );

        let service = AssetService::new(catalog_dir.path(), crate::Verbosity::quiet(), &crate::config::PreviewConfig::default());
        let result = service
            .import(&[vol_dir.path().join("solo.jpg")], &volume, &default_filter())
            .unwrap();

        assert_eq!(result.imported, 1);
        assert_eq!(result.recipes_attached, 0);

        let metadata_store = crate::metadata_store::MetadataStore::new(catalog_dir.path());
        let summaries = metadata_store.list().unwrap();
        assert_eq!(summaries.len(), 1);
        assert_eq!(summaries[0].variant_count, 1);
    }

    /// Helper to create an XMP sidecar with given keywords, rating, label, description.
    fn make_xmp(
        keywords: &[&str],
        rating: Option<u8>,
        label: Option<&str>,
        description: Option<&str>,
        creator: Option<&str>,
        copyright: Option<&str>,
    ) -> String {
        let mut parts = Vec::new();
        parts.push(r#"<?xml version="1.0" encoding="UTF-8"?>
<x:xmpmeta xmlns:x="adobe:ns:meta/">
 <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
  <rdf:Description rdf:about=""
    xmlns:dc="http://purl.org/dc/elements/1.1/"
    xmlns:xmp="http://ns.adobe.com/xap/1.0/""#.to_string());
        if let Some(r) = rating {
            parts.push(format!("\n    xmp:Rating=\"{r}\""));
        }
        if let Some(l) = label {
            parts.push(format!("\n    xmp:Label=\"{l}\""));
        }
        parts.push(">".to_string());
        if !keywords.is_empty() {
            parts.push("   <dc:subject>\n    <rdf:Bag>".to_string());
            for kw in keywords {
                parts.push(format!("     <rdf:li>{kw}</rdf:li>"));
            }
            parts.push("    </rdf:Bag>\n   </dc:subject>".to_string());
        }
        if let Some(desc) = description {
            parts.push(format!(
                "   <dc:description>\n    <rdf:Alt>\n     <rdf:li xml:lang=\"x-default\">{desc}</rdf:li>\n    </rdf:Alt>\n   </dc:description>"
            ));
        }
        if let Some(c) = creator {
            parts.push(format!(
                "   <dc:creator>\n    <rdf:Seq>\n     <rdf:li>{c}</rdf:li>\n    </rdf:Seq>\n   </dc:creator>"
            ));
        }
        if let Some(cr) = copyright {
            parts.push(format!(
                "   <dc:rights>\n    <rdf:Alt>\n     <rdf:li xml:lang=\"x-default\">{cr}</rdf:li>\n    </rdf:Alt>\n   </dc:rights>"
            ));
        }
        parts.push("  </rdf:Description>\n </rdf:RDF>\n</x:xmpmeta>".to_string());
        parts.join("\n")
    }

    #[test]
    fn import_xmp_extracts_tags_and_metadata() {
        let catalog_dir = tempfile::tempdir().unwrap();
        setup_catalog(catalog_dir.path());

        let vol_dir = tempfile::tempdir().unwrap();
        let photos = vol_dir.path().join("photos");
        std::fs::create_dir_all(&photos).unwrap();
        std::fs::write(photos.join("DSC_001.nef"), "raw file data").unwrap();
        let xmp = make_xmp(
            &["landscape", "sunset"],
            Some(4),
            Some("Blue"),
            None,
            None,
            None,
        );
        std::fs::write(photos.join("DSC_001.xmp"), &xmp).unwrap();

        let volume = crate::models::Volume::new(
            "test-vol".into(),
            vol_dir.path().to_path_buf(),
            crate::models::VolumeType::Local,
        );

        let service = AssetService::new(catalog_dir.path(), crate::Verbosity::quiet(), &crate::config::PreviewConfig::default());
        let result = service.import(&[photos], &volume, &default_filter()).unwrap();
        assert_eq!(result.imported, 1);
        assert_eq!(result.recipes_attached, 1);

        let metadata_store = crate::metadata_store::MetadataStore::new(catalog_dir.path());
        let summaries = metadata_store.list().unwrap();
        let asset = metadata_store.load(summaries[0].id).unwrap();

        assert!(asset.tags.contains(&"landscape".to_string()));
        assert!(asset.tags.contains(&"sunset".to_string()));
        assert_eq!(
            asset.variants[0].source_metadata.get("rating").map(|s| s.as_str()),
            Some("4")
        );
        assert_eq!(
            asset.variants[0].source_metadata.get("label").map(|s| s.as_str()),
            Some("Blue")
        );
    }

    #[test]
    fn import_xmp_sets_description() {
        let catalog_dir = tempfile::tempdir().unwrap();
        setup_catalog(catalog_dir.path());

        let vol_dir = tempfile::tempdir().unwrap();
        let photos = vol_dir.path().join("photos");
        std::fs::create_dir_all(&photos).unwrap();
        std::fs::write(photos.join("DSC_002.nef"), "raw data 2").unwrap();
        let xmp = make_xmp(&[], None, None, Some("A great photo"), None, None);
        std::fs::write(photos.join("DSC_002.xmp"), &xmp).unwrap();

        let volume = crate::models::Volume::new(
            "test-vol".into(),
            vol_dir.path().to_path_buf(),
            crate::models::VolumeType::Local,
        );

        let service = AssetService::new(catalog_dir.path(), crate::Verbosity::quiet(), &crate::config::PreviewConfig::default());
        service.import(&[photos], &volume, &default_filter()).unwrap();

        let metadata_store = crate::metadata_store::MetadataStore::new(catalog_dir.path());
        let summaries = metadata_store.list().unwrap();
        let asset = metadata_store.load(summaries[0].id).unwrap();
        assert_eq!(asset.description.as_deref(), Some("A great photo"));
    }

    #[test]
    fn import_xmp_does_not_overwrite_existing_tags() {
        let catalog_dir = tempfile::tempdir().unwrap();
        setup_catalog(catalog_dir.path());

        let vol_dir = tempfile::tempdir().unwrap();
        let photos = vol_dir.path().join("photos");
        std::fs::create_dir_all(&photos).unwrap();
        std::fs::write(photos.join("DSC_003.nef"), "raw data 3").unwrap();
        let xmp = make_xmp(&["nature", "forest"], Some(5), None, None, None, None);
        std::fs::write(photos.join("DSC_003.xmp"), &xmp).unwrap();

        let volume = crate::models::Volume::new(
            "test-vol".into(),
            vol_dir.path().to_path_buf(),
            crate::models::VolumeType::Local,
        );

        let service = AssetService::new(catalog_dir.path(), crate::Verbosity::quiet(), &crate::config::PreviewConfig::default());
        service.import(&[photos], &volume, &default_filter()).unwrap();

        // Now manually add a tag to the asset
        let metadata_store = crate::metadata_store::MetadataStore::new(catalog_dir.path());
        let summaries = metadata_store.list().unwrap();
        let mut asset = metadata_store.load(summaries[0].id).unwrap();
        asset.tags.push("manual-tag".to_string());
        metadata_store.save(&asset).unwrap();

        // Verify original XMP tags + manual tag are all present
        let asset = metadata_store.load(summaries[0].id).unwrap();
        assert!(asset.tags.contains(&"nature".to_string()));
        assert!(asset.tags.contains(&"forest".to_string()));
        assert!(asset.tags.contains(&"manual-tag".to_string()));
    }

    #[test]
    fn import_auto_tags_applied() {
        let catalog_dir = tempfile::tempdir().unwrap();
        setup_catalog(catalog_dir.path());

        let vol_dir = tempfile::tempdir().unwrap();
        std::fs::write(vol_dir.path().join("photo.jpg"), "test content for tags").unwrap();

        let volume = crate::models::Volume::new(
            "test-vol".into(),
            vol_dir.path().to_path_buf(),
            crate::models::VolumeType::Local,
        );

        let service = AssetService::new(catalog_dir.path(), crate::Verbosity::quiet(), &crate::config::PreviewConfig::default());
        let auto_tags = vec!["inbox".to_string(), "unreviewed".to_string()];
        let result = service.import_with_callback(
            &[vol_dir.path().join("photo.jpg")],
            &volume,
            &default_filter(),
            &[],
            &auto_tags,
            false,
            false,
            |_, _, _| {},
        ).unwrap();

        assert_eq!(result.imported, 1);

        let metadata_store = crate::metadata_store::MetadataStore::new(catalog_dir.path());
        let summaries = metadata_store.list().unwrap();
        let asset = metadata_store.load(summaries[0].id).unwrap();
        assert!(asset.tags.contains(&"inbox".to_string()));
        assert!(asset.tags.contains(&"unreviewed".to_string()));
    }

    #[test]
    fn import_exclude_patterns_skip_files() {
        let catalog_dir = tempfile::tempdir().unwrap();
        setup_catalog(catalog_dir.path());

        let vol_dir = tempfile::tempdir().unwrap();
        std::fs::write(vol_dir.path().join("photo.jpg"), "real image").unwrap();
        std::fs::write(vol_dir.path().join("Thumbs.db"), "thumbnail cache").unwrap();

        let volume = crate::models::Volume::new(
            "test-vol".into(),
            vol_dir.path().to_path_buf(),
            crate::models::VolumeType::Local,
        );

        let service = AssetService::new(catalog_dir.path(), crate::Verbosity::quiet(), &crate::config::PreviewConfig::default());
        let exclude = vec!["Thumbs.db".to_string()];
        let result = service.import_with_callback(
            &[vol_dir.path().to_path_buf()],
            &volume,
            &default_filter(),
            &exclude,
            &[],
            false,
            false,
            |_, _, _| {},
        ).unwrap();

        assert_eq!(result.imported, 1);
    }

    #[test]
    fn import_uses_mtime_fallback_when_no_exif() {
        use chrono::Utc;

        let catalog_dir = tempfile::tempdir().unwrap();
        setup_catalog(catalog_dir.path());

        let vol_dir = tempfile::tempdir().unwrap();
        let photo_path = vol_dir.path().join("plain.jpg");
        // Write a file with no EXIF data
        std::fs::write(&photo_path, "no exif data here just plain bytes").unwrap();

        // Set a specific mtime in the past (2020-01-15)
        let target_time = std::time::SystemTime::UNIX_EPOCH
            + std::time::Duration::from_secs(1579046400); // 2020-01-15 00:00:00 UTC
        let file_times = std::fs::FileTimes::new()
            .set_modified(target_time);
        let file = std::fs::File::options().write(true).open(&photo_path).unwrap();
        file.set_times(file_times).unwrap();

        let volume = crate::models::Volume::new(
            "test-vol".into(),
            vol_dir.path().to_path_buf(),
            crate::models::VolumeType::Local,
        );

        let service = AssetService::new(catalog_dir.path(), crate::Verbosity::quiet(), &crate::config::PreviewConfig::default());
        let result = service.import(
            &[photo_path],
            &volume,
            &default_filter(),
        ).unwrap();
        assert_eq!(result.imported, 1);

        let metadata_store = crate::metadata_store::MetadataStore::new(catalog_dir.path());
        let summaries = metadata_store.list().unwrap();
        let asset = metadata_store.load(summaries[0].id).unwrap();

        // Should use mtime, not current time
        let now = Utc::now();
        let diff_from_now = (now - asset.created_at).num_hours();
        assert!(diff_from_now > 24, "created_at should be in 2020, not near now; got {}", asset.created_at);

        // Check it's close to our target mtime (2020-01-15)
        let expected = chrono::DateTime::<Utc>::from(target_time);
        let diff_from_target = (asset.created_at - expected).num_seconds().abs();
        assert!(diff_from_target <= 1, "created_at should match file mtime; got {} vs expected {}", asset.created_at, expected);
    }

    #[test]
    fn import_second_variant_updates_date_if_older() {
        use chrono::Utc;

        let catalog_dir = tempfile::tempdir().unwrap();
        setup_catalog(catalog_dir.path());

        let vol_dir = tempfile::tempdir().unwrap();

        // Create two files with the same stem (will be grouped)
        let raw_path = vol_dir.path().join("photo.arw");
        let jpg_path = vol_dir.path().join("photo.jpg");
        std::fs::write(&raw_path, "fake raw content for date test").unwrap();
        std::fs::write(&jpg_path, "fake jpg content for date test").unwrap();

        // Set RAW to newer mtime (2024), JPG to older mtime (2020)
        let newer_time = std::time::SystemTime::UNIX_EPOCH
            + std::time::Duration::from_secs(1704067200); // 2024-01-01
        let older_time = std::time::SystemTime::UNIX_EPOCH
            + std::time::Duration::from_secs(1579046400); // 2020-01-15

        let raw_file = std::fs::File::options().write(true).open(&raw_path).unwrap();
        raw_file.set_times(std::fs::FileTimes::new().set_modified(newer_time)).unwrap();
        let jpg_file = std::fs::File::options().write(true).open(&jpg_path).unwrap();
        jpg_file.set_times(std::fs::FileTimes::new().set_modified(older_time)).unwrap();

        let volume = crate::models::Volume::new(
            "test-vol".into(),
            vol_dir.path().to_path_buf(),
            crate::models::VolumeType::Local,
        );

        let service = AssetService::new(catalog_dir.path(), crate::Verbosity::quiet(), &crate::config::PreviewConfig::default());
        let result = service.import(
            &[vol_dir.path().to_path_buf()],
            &volume,
            &default_filter(),
        ).unwrap();
        assert_eq!(result.imported, 2); // RAW + JPG grouped into one asset

        let metadata_store = crate::metadata_store::MetadataStore::new(catalog_dir.path());
        let summaries = metadata_store.list().unwrap();
        assert_eq!(summaries.len(), 1, "should be one asset (grouped)");
        let asset = metadata_store.load(summaries[0].id).unwrap();

        // Asset date should be the older of the two mtimes (2020-01-15)
        let expected = chrono::DateTime::<Utc>::from(older_time);
        let diff = (asset.created_at - expected).num_seconds().abs();
        assert!(diff <= 1, "created_at should be the older variant date; got {} vs expected {}", asset.created_at, expected);
    }

    /// Catalog keys and sidecar paths are `/`-separated on every platform;
    /// on Windows this is the test that a `strip_prefix` result never
    /// reaches a store with backslashes.
    #[test]
    fn stored_paths_use_forward_slashes_on_every_platform() {
        let env = ServiceEnv::new();
        env.write("2026/shoot one/DSC_1.nef", "raw bytes");
        env.write("2026/shoot one/DSC_1.xmp", &make_xmp(&["bird"], Some(3), None, None, None, None));
        let result = env.import(&["2026"]);
        assert_eq!((result.imported, result.recipes_attached), (1, 1));
        let id = result.new_asset_ids[0].clone();

        let stored = |env: &ServiceEnv| -> Vec<String> {
            let catalog = env.cat.catalog();
            let mut paths: Vec<String> = Vec::new();
            for sql in ["SELECT relative_path FROM file_locations", "SELECT relative_path FROM recipes"] {
                let mut stmt = catalog.conn().prepare(sql).unwrap();
                paths.extend(stmt.query_map([], |r| r.get::<_, String>(0)).unwrap().map(|p| p.unwrap()));
            }
            let asset = env.asset(&id);
            paths.extend(asset.variants.iter().flat_map(|v| &v.locations).map(|l| l.relative_path.to_string_lossy().to_string()));
            paths.extend(asset.recipes.iter().map(|r| r.location.relative_path.to_string_lossy().to_string()));
            paths.sort();
            paths
        };
        assert_eq!(
            stored(&env),
            [
                "2026/shoot one/DSC_1.nef",
                "2026/shoot one/DSC_1.nef",
                "2026/shoot one/DSC_1.xmp",
                "2026/shoot one/DSC_1.xmp",
            ]
        );
        assert_eq!(result.imported_directories, ["2026/shoot one"]);

        // A move found by sync is stored the same way.
        std::fs::create_dir_all(env.abs("sorted/deep")).unwrap();
        std::fs::rename(env.abs("2026/shoot one/DSC_1.nef"), env.abs("sorted/deep/DSC_1.nef")).unwrap();
        std::fs::rename(env.abs("2026/shoot one/DSC_1.xmp"), env.abs("sorted/deep/DSC_1.xmp")).unwrap();
        let root = [env.vol_dir.path().to_path_buf()];
        env.service.sync(&root, &env.volume, true, false, &[], |_, _, _| {}).unwrap();
        let after = stored(&env);
        assert!(after.iter().all(|p| p.starts_with("sorted/deep/DSC_1.")), "{after:?}");
    }
}
