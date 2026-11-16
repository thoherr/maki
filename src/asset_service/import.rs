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
