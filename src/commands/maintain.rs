//! Maintenance commands: `rebuild-catalog`, `fix-*`, `generate-previews`, `update-location`, `cleanup`, `refresh`, `relocate`.

use super::*;

/// Extracted body of `Commands::RebuildCatalog`.
pub fn run_rebuild_catalog_command(
    asset: Option<String>,
    json: bool,
) -> anyhow::Result<()> {
    struct Ctx { json: bool }
    let cli = Ctx { json };
    let catalog_root = maki::config::find_catalog_root()?;

    if let Some(ref asset_id) = asset {
        // Per-asset rebuild: delete and re-insert a single asset from its sidecar
        let catalog = Catalog::open(&catalog_root)?;
        let store = MetadataStore::new(&catalog_root);

        // Resolve asset ID (try as UUID first, then prefix match in catalog)
        let uuid: uuid::Uuid = if let Ok(u) = asset_id.parse() {
            u
        } else if let Some(full) = catalog.resolve_asset_id(asset_id)? {
            full.parse()?
        } else {
            // Not in SQLite — try loading sidecar directly
            anyhow::bail!("asset '{}' not found in catalog. For new assets, use 'maki refresh --reimport --asset {}'", asset_id, asset_id);
        };

        let asset_obj = store.load(uuid)?;
        let id_str = uuid.to_string();

        // Delete all existing rows for this asset (FK checks off for safety)
        let _ = catalog.conn().execute_batch("PRAGMA foreign_keys = OFF");

        // Get all variant hashes (from SQLite, may differ from sidecar)
        let sqlite_hashes: Vec<String> = catalog.conn()
            .prepare("SELECT content_hash FROM variants WHERE asset_id = ?1")?
            .query_map(rusqlite::params![&id_str], |r| r.get(0))?
            .filter_map(|r| r.ok())
            .collect();

        for hash in &sqlite_hashes {
            let _ = catalog.conn().execute("DELETE FROM recipes WHERE variant_hash = ?1", rusqlite::params![hash]);
            let _ = catalog.conn().execute("DELETE FROM file_locations WHERE content_hash = ?1", rusqlite::params![hash]);
        }
        let _ = catalog.conn().execute("DELETE FROM variants WHERE asset_id = ?1", rusqlite::params![&id_str]);
        let _ = catalog.conn().execute("DELETE FROM faces WHERE asset_id = ?1", rusqlite::params![&id_str]);
        let _ = catalog.conn().execute("DELETE FROM embeddings WHERE asset_id = ?1", rusqlite::params![&id_str]);
        let _ = catalog.conn().execute("DELETE FROM collection_assets WHERE asset_id = ?1", rusqlite::params![&id_str]);
        let _ = catalog.conn().execute("DELETE FROM assets WHERE id = ?1", rusqlite::params![&id_str]);

        let _ = catalog.conn().execute_batch("PRAGMA foreign_keys = ON");

        // Re-insert from sidecar
        let registry = DeviceRegistry::new(&catalog_root);
        for volume in registry.list()? {
            catalog.ensure_volume(&volume)?;
        }

        catalog.insert_asset(&asset_obj)?;
        for variant in &asset_obj.variants {
            catalog.insert_variant(variant)?;
            for loc in &variant.locations {
                catalog.insert_file_location(&variant.content_hash, loc)?;
            }
        }
        for recipe in &asset_obj.recipes {
            catalog.insert_recipe(recipe)?;
        }
        catalog.update_denormalized_variant_columns(&asset_obj)?;

        // Restore embedding from binary file if it exists
        #[cfg(feature = "ai")]
        {
            let emb_store = maki::embedding_store::EmbeddingStore::new(catalog.conn());
            let emb_base = catalog_root.join("embeddings");
            if emb_base.exists() {
                if let Ok(entries) = std::fs::read_dir(&emb_base) {
                    for entry in entries.flatten() {
                        let name = entry.file_name().to_string_lossy().to_string();
                        if name == "arcface" || !entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                            continue;
                        }
                        let prefix = &id_str[..2];
                        let bin_path = emb_base.join(&name).join(prefix).join(format!("{id_str}.bin"));
                        if bin_path.exists() {
                            if let Ok(data) = std::fs::read(&bin_path) {
                                let embedding: Vec<f32> = data.chunks_exact(4)
                                    .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
                                    .collect();
                                let _ = emb_store.store(&id_str, &embedding, &name);
                            }
                        }
                    }
                }
            }

            // Restore faces for this asset
            let face_store = maki::face_store::FaceStore::new(catalog.conn());
            let faces_file = maki::face_store::load_faces_yaml(&catalog_root).unwrap_or_default();
            let asset_face_ids: Vec<String> = faces_file.faces.iter()
                .filter(|f| f.asset_id == id_str)
                .map(|f| f.id.clone())
                .collect();
            if !asset_face_ids.is_empty() {
                let filtered = maki::face_store::FacesFile {
                    faces: faces_file.faces.into_iter().filter(|f| f.asset_id == id_str).collect(),
                };
                let _ = face_store.import_faces_from_yaml(&filtered);
            }
            // Restore ArcFace embeddings for this asset's faces
            let asset_faces = face_store.faces_for_asset(&id_str).unwrap_or_default();
            for face in &asset_faces {
                let prefix = &face.id[..2.min(face.id.len())];
                let bin_path = emb_base.join("arcface").join(prefix).join(format!("{}.bin", face.id));
                if bin_path.exists() {
                    if let Ok(data) = std::fs::read(&bin_path) {
                        let embedding: Vec<f32> = data.chunks_exact(4)
                            .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
                            .collect();
                        let _ = face_store.import_face_embedding(&face.id, &embedding);
                    }
                }
            }

            // Update denormalized face columns (count + legacy scan-status backfill)
            let _ = catalog.update_face_count(&id_str);
            let _ = catalog.backfill_face_scan_status(&id_str);
        }

        if cli.json {
            println!("{}", serde_json::json!({
                "asset_id": id_str,
                "variants": asset_obj.variants.len(),
                "recipes": asset_obj.recipes.len(),
            }));
        } else {
            println!("Rebuilt asset {}: {} variant(s), {} recipe(s)",
                &id_str[..8], asset_obj.variants.len(), asset_obj.recipes.len());
        }
        return Ok(());
    }

    let catalog = Catalog::open(&catalog_root)?;
    catalog.initialize()?;

    // Ensure volume rows exist so FK references work
    let registry = DeviceRegistry::new(&catalog_root);
    for volume in registry.list()? {
        catalog.ensure_volume(&volume)?;
    }

    // Clear existing data rows
    catalog.rebuild()?;

    // Sync sidecar files into catalog
    let store = MetadataStore::new(&catalog_root);
    let result = store.sync_to_catalog(&catalog)?;

    // Restore collections from YAML
    let collections_restored = {
        let col_file = maki::collection::load_yaml(&catalog_root).unwrap_or_default();
        if !col_file.collections.is_empty() {
            let col_store = maki::collection::CollectionStore::new(catalog.conn());
            col_store.import_from_yaml(&col_file).unwrap_or(0)
        } else {
            0
        }
    };

    // Restore stacks from YAML
    let stacks_restored = {
        let stacks_file = maki::stack::load_yaml(&catalog_root).unwrap_or_default();
        if !stacks_file.stacks.is_empty() {
            let stack_store = maki::stack::StackStore::new(catalog.conn());
            stack_store.import_from_yaml(&stacks_file).unwrap_or(0)
        } else {
            0
        }
    };

    // Restore faces, people, and embeddings from files
    #[cfg(feature = "ai")]
    let (people_restored, faces_restored, face_embeddings_restored, embeddings_restored) = {
        let _ = maki::face_store::FaceStore::initialize(catalog.conn());
        let _ = maki::embedding_store::EmbeddingStore::initialize(catalog.conn());
        let face_store = maki::face_store::FaceStore::new(catalog.conn());

        // Import people first (faces reference people via FK)
        let people_file = maki::face_store::load_people_yaml(&catalog_root).unwrap_or_default();
        let people_restored = if !people_file.people.is_empty() {
            face_store.import_people_from_yaml(&people_file).unwrap_or(0)
        } else {
            0
        };

        // Import faces (with empty embedding placeholder)
        let faces_file = maki::face_store::load_faces_yaml(&catalog_root).unwrap_or_default();
        let faces_restored = if !faces_file.faces.is_empty() {
            face_store.import_faces_from_yaml(&faces_file).unwrap_or(0)
        } else {
            0
        };

        // Restore ArcFace embeddings from binary files
        let mut face_embeddings_restored = 0u32;
        if let Ok(arcface_entries) = maki::face_store::scan_arcface_binaries(&catalog_root) {
            for (face_id, embedding) in &arcface_entries {
                if face_store.import_face_embedding(face_id, embedding).is_ok() {
                    face_embeddings_restored += 1;
                }
            }
        }

        // Restore SigLIP embeddings from binary files
        let mut embeddings_restored = 0u32;
        let emb_store = maki::embedding_store::EmbeddingStore::new(catalog.conn());
        // Scan all model directories under embeddings/ (skip "arcface")
        let emb_base = catalog_root.join("embeddings");
        if emb_base.exists() {
            if let Ok(entries) = std::fs::read_dir(&emb_base) {
                for entry in entries.flatten() {
                    let name = entry.file_name().to_string_lossy().to_string();
                    if name == "arcface" || !entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                        continue;
                    }
                    if let Ok(model_entries) = maki::embedding_store::scan_embedding_binaries(&catalog_root, &name) {
                        for (asset_id, embedding) in &model_entries {
                            if emb_store.store(asset_id, embedding, &name).is_ok() {
                                embeddings_restored += 1;
                            }
                        }
                    }
                }
            }
        }

        // Backfill denormalized face columns. The legacy face_scan_status
        // fallback matters only for users upgrading from v4.4.2 or earlier —
        // newer writes always put face_scan_status in the sidecar, so it's
        // a no-op for fresh catalogs.
        if faces_restored > 0 {
            let _ = catalog.backfill_face_denormalization();
        }

        (people_restored, faces_restored, face_embeddings_restored, embeddings_restored)
    };

    if cli.json {
        #[allow(unused_mut)]
        let mut json = serde_json::json!({
            "synced": result.synced,
            "errors": result.errors,
            "collections_restored": collections_restored,
            "stacks_restored": stacks_restored,
        });
        #[cfg(feature = "ai")]
        {
            json["people_restored"] = serde_json::json!(people_restored);
            json["faces_restored"] = serde_json::json!(faces_restored);
            json["face_embeddings_restored"] = serde_json::json!(face_embeddings_restored);
            json["embeddings_restored"] = serde_json::json!(embeddings_restored);
        }
        println!("{}", json);
    } else {
        println!("Rebuild complete: {} asset(s) synced", result.synced);
        if collections_restored > 0 {
            println!("  {} collection(s) restored", collections_restored);
        }
        if stacks_restored > 0 {
            println!("  {} stack(s) restored", stacks_restored);
        }
        #[cfg(feature = "ai")]
        {
            if people_restored > 0 {
                println!("  {} people restored", people_restored);
            }
            if faces_restored > 0 {
                println!("  {} face(s) restored ({} embeddings)", faces_restored, face_embeddings_restored);
            }
            if embeddings_restored > 0 {
                println!("  {} embedding(s) restored", embeddings_restored);
            }
        }
        if result.errors > 0 {
            println!("  {} error(s) encountered", result.errors);
        }

        // After rebuild, count assets that ended up without AI-derived
        // data — those whose embedding binaries weren't on disk, or
        // that were imported on a build without the AI feature. The
        // user often forgets to re-run `embed` / `faces detect` after
        // a rebuild and only notices much later when similarity search
        // returns empty / face cluster is missing recent assets.
        #[cfg(feature = "ai")]
        {
            let total_assets = catalog.conn().query_row(
                "SELECT COUNT(*) FROM assets", [], |r| r.get::<_, i64>(0)
            ).unwrap_or(0);
            let with_embeddings = catalog.conn().query_row(
                "SELECT COUNT(DISTINCT asset_id) FROM embeddings", [], |r| r.get::<_, i64>(0)
            ).unwrap_or(0);
            let missing_embeddings = (total_assets - with_embeddings).max(0);
            if missing_embeddings > 0 {
                println!(
                    "  Tip: {} asset(s) have no embedding. Run 'maki embed' \
                     for visual similarity / text search.",
                    missing_embeddings
                );
            }
            let unscanned_for_faces = catalog.conn().query_row(
                "SELECT COUNT(*) FROM assets \
                 WHERE (face_scan_status IS NULL OR face_scan_status = 'pending')",
                [], |r| r.get::<_, i64>(0)
            ).unwrap_or(0);
            if unscanned_for_faces > 0 {
                println!(
                    "  Tip: {} asset(s) haven't been scanned for faces. \
                     Run 'maki faces detect' to populate.",
                    unscanned_for_faces
                );
            }
        }
    }
    Ok(())
}

/// Extracted body of `Commands::FixRecipes`. See `run_import_command` for the
/// extraction pattern.
pub fn run_fix_recipes_command(
        query: Option<String>,
        volume: Option<String>,
        asset: Option<String>,
        apply: bool,
        asset_ids: Vec<String>,
        json: bool,
        log: bool,
        #[allow(unused_variables)] verbosity: maki::Verbosity,
) -> anyhow::Result<()> {
    #[allow(dead_code)]
    struct Ctx { json: bool, log: bool }
    let cli = Ctx { json, log };
    let (catalog_root, config) = maki::config::load_config()?;
    let service = AssetService::new(&catalog_root, verbosity, &config.preview);
    let engine = maki::query::QueryEngine::new(&catalog_root);

    // Resolve scope (query/asset/asset_ids) to individual asset IDs
    let scope = engine.resolve_scope(query.as_deref(), asset.as_deref(), &asset_ids)?;
    let asset_id_list: Vec<Option<String>> = match scope {
        Some(set) => set.into_iter().map(Some).collect(),
        None => vec![None], // process all
    };

    let show_log = cli.log;
    let mut result = maki::asset_service::FixRecipesResult { dry_run: !apply, ..Default::default() };
    for aid in &asset_id_list {
        let r = service.fix_recipes(
            volume.as_deref(),
            aid.as_deref(),
            apply,
            |name, status| {
                if show_log {
                    let label = match status {
                        maki::asset_service::FixRecipesStatus::Reattached => {
                            if apply { "reattached" } else { "would reattach" }
                        }
                        maki::asset_service::FixRecipesStatus::NoParentFound => "no parent found",
                        maki::asset_service::FixRecipesStatus::Skipped => "skipped",
                    };
                    eprintln!("  {} — {}", name, label);
                }
            },
        )?;
        result.checked += r.checked;
        result.reattached += r.reattached;
        result.no_parent += r.no_parent;
        result.skipped += r.skipped;
        result.errors.extend(r.errors);
    }

    if cli.json {
        println!("{}", serde_json::to_string_pretty(&result)?);
    } else {
        for err in &result.errors {
            eprintln!("  {err}");
        }

        if !apply && result.reattached > 0 {
            eprint!("Dry run — ");
        }

        let mut parts = vec![
            format!("{} checked", result.checked),
            format!("{} reattached", result.reattached),
        ];
        if result.no_parent > 0 {
            parts.push(format!("{} no parent found", result.no_parent));
        }
        if result.skipped > 0 {
            parts.push(format!("{} skipped", result.skipped));
        }

        println!("Fix-recipes: {}", parts.join(", "));

        if !apply && result.reattached > 0 {
            println!("  Run with --apply to make changes.");
        }
    }

    Ok(())
}

/// Extracted body of `Commands::FixDates`. See `run_import_command` for the
/// extraction pattern.
pub fn run_fix_dates_command(
        query: Option<String>,
        volume: Option<String>,
        asset: Option<String>,
        apply: bool,
        asset_ids: Vec<String>,
        json: bool,
        log: bool,
        #[allow(unused_variables)] verbosity: maki::Verbosity,
) -> anyhow::Result<()> {
    #[allow(dead_code)]
    struct Ctx { json: bool, log: bool }
    let cli = Ctx { json, log };
    let (catalog_root, config) = maki::config::load_config()?;
    let service = AssetService::new(&catalog_root, verbosity, &config.preview);
    let engine = maki::query::QueryEngine::new(&catalog_root);

    // Resolve scope (query/asset/asset_ids) to individual asset IDs
    let scope = engine.resolve_scope(query.as_deref(), asset.as_deref(), &asset_ids)?;
    let asset_id_list: Vec<Option<String>> = match scope {
        Some(set) => set.into_iter().map(Some).collect(),
        None => vec![None], // process all
    };

    let show_log = cli.log;
    let mut result = maki::asset_service::FixDatesResult { dry_run: !apply, ..Default::default() };
    for aid in &asset_id_list {
        let r = service.fix_dates(
            volume.as_deref(),
            aid.as_deref(),
            apply,
            |name, status, detail| {
                if show_log {
                    let label = match status {
                        maki::asset_service::FixDatesStatus::AlreadyCorrect => "ok".to_string(),
                        maki::asset_service::FixDatesStatus::NoDate => "no date available".to_string(),
                        maki::asset_service::FixDatesStatus::SkippedOffline => "skipped (volume offline)".to_string(),
                        maki::asset_service::FixDatesStatus::Fixed => {
                            let action = if apply { "fixed" } else { "would fix" };
                            if let Some(d) = detail {
                                format!("{action}: {d}")
                            } else {
                                action.to_string()
                            }
                        }
                    };
                    eprintln!("  {} — {}", name, label);
                }
            },
        )?;
        result.checked += r.checked;
        result.fixed += r.fixed;
        result.already_correct += r.already_correct;
        result.skipped_offline += r.skipped_offline;
        result.no_date += r.no_date;
        result.errors.extend(r.errors);
        for v in r.offline_volumes {
            if !result.offline_volumes.contains(&v) {
                result.offline_volumes.push(v);
            }
        }
    }

    if cli.json {
        println!("{}", serde_json::to_string_pretty(&result)?);
    } else {
        // Print offline volume warnings
        if !result.offline_volumes.is_empty() {
            for vol_label in &result.offline_volumes {
                eprintln!("Warning: volume '{}' is offline — cannot read files for date extraction", vol_label);
            }
        }

        for err in &result.errors {
            eprintln!("  {err}");
        }

        if !apply && result.fixed > 0 {
            eprint!("Dry run — ");
        }

        let mut parts = vec![
            format!("{} checked", result.checked),
            format!("{} fixed", result.fixed),
            format!("{} already correct", result.already_correct),
        ];
        if result.skipped_offline > 0 {
            parts.push(format!("{} skipped (volume offline)", result.skipped_offline));
        }
        if result.no_date > 0 {
            parts.push(format!("{} no date available", result.no_date));
        }

        println!("Fix-dates: {}", parts.join(", "));

        if !apply && result.fixed > 0 {
            println!("  Run with --apply to make changes.");
        }
        if result.skipped_offline > 0 {
            println!("  Mount offline volumes and re-run to fix remaining assets.");
        }
    }

    Ok(())
}

/// Extracted body of `Commands::FixRoles`. See `run_import_command` for the
/// extraction pattern.
pub fn run_fix_roles_command(
        paths: Vec<String>,
        volume: Option<String>,
        asset: Option<String>,
        apply: bool,
        json: bool,
        log: bool,
        #[allow(unused_variables)] verbosity: maki::Verbosity,
) -> anyhow::Result<()> {
    #[allow(dead_code)]
    struct Ctx { json: bool, log: bool }
    let cli = Ctx { json, log };
    let (catalog_root, config) = maki::config::load_config()?;
    let service = AssetService::new(&catalog_root, verbosity, &config.preview);

    let canonical_paths: Vec<PathBuf> = paths
        .iter()
        .map(|p| {
            std::fs::canonicalize(p)
                .unwrap_or_else(|_| PathBuf::from(p))
        })
        .collect();

    let show_log = cli.log;
    let result = service.fix_roles(
        &canonical_paths,
        volume.as_deref(),
        asset.as_deref(),
        apply,
        |name, status| {
            if show_log {
                let label = match status {
                    maki::asset_service::FixRolesStatus::AlreadyCorrect => "ok",
                    maki::asset_service::FixRolesStatus::Fixed => {
                        if apply { "fixed" } else { "would fix" }
                    }
                };
                eprintln!("  {} — {}", name, label);
            }
        },
    )?;

    if cli.json {
        println!("{}", serde_json::to_string_pretty(&result)?);
    } else {
        for err in &result.errors {
            eprintln!("  {err}");
        }

        if !apply && result.fixed > 0 {
            eprint!("Dry run — ");
        }

        println!(
            "Fix-roles: {} checked, {} fixed ({} variant(s)), {} already correct",
            result.checked, result.fixed, result.variants_fixed, result.already_correct
        );

        if !apply && result.fixed > 0 {
            println!("  Run with --apply to make changes.");
        }
        // Reordering variant roles changes which variant is selected for
        // preview generation. Cached previews still reflect the *old*
        // best variant — `generate-previews --upgrade` regenerates them
        // for assets whose best changed.
        if apply && result.fixed > 0 {
            println!(
                "  Tip: best-preview variant changed for {} asset(s). \
                 Run 'maki generate-previews --upgrade' to refresh their previews.",
                result.fixed
            );
        }
    }

    Ok(())
}

/// Extracted body of `Commands::GeneratePreviews`. See `run_import_command` for the
/// extraction pattern.
pub fn run_generate_previews_command(
        paths: Vec<String>,
        volume: Option<String>,
        asset: Option<String>,
        include: Vec<String>,
        skip: Vec<String>,
        force: bool,
        upgrade: bool,
        smart: bool,
        json: bool,
        log: bool,
        #[allow(unused_variables)] verbosity: maki::Verbosity,
) -> anyhow::Result<()> {
    #[allow(dead_code)]
    struct Ctx { json: bool, log: bool }
    let cli = Ctx { json, log };
    use maki::asset_service::FileTypeFilter;

    let (catalog_root, config) = maki::config::load_config()?;
    let preview_gen = maki::preview::PreviewGenerator::new(&catalog_root, verbosity, &config.preview);
    let metadata_store = MetadataStore::new(&catalog_root);
    let registry = maki::device_registry::DeviceRegistry::new(&catalog_root);
    let catalog = maki::catalog::Catalog::open(&catalog_root)?;
    let volumes = registry.list()?;

    // Build file type filter
    let mut filter = FileTypeFilter::default();
    for group in &include {
        if skip.contains(group) {
            anyhow::bail!(
                "Group '{}' cannot be both included and skipped.",
                group
            );
        }
    }
    for group in &include {
        filter.include(group)?;
    }
    for group in &skip {
        filter.skip(group)?;
    }

    let mut generated = 0usize;
    let mut skipped = 0usize;
    let mut failed = 0usize;
    let mut upgraded = 0usize;
    // Volumes that held the only locations of variants we couldn't
    // process because they're offline. Surfaced at the end so the user
    // knows which disk to mount instead of seeing a silent skip.
    let mut offline_blockers: std::collections::HashSet<String> = std::collections::HashSet::new();

    // Canonicalize input paths
    let canonical_paths: Vec<PathBuf> = paths
        .iter()
        .map(|p| {
            std::fs::canonicalize(p)
                .unwrap_or_else(|_| PathBuf::from(p))
        })
        .collect();

    if !canonical_paths.is_empty() {
        // PATHS mode: resolve files, look up each in catalog
        let files = maki::asset_service::resolve_files(&canonical_paths, &config.import.exclude);
        let content_store = maki::content_store::ContentStore::new(&catalog_root);

        for file_path in &files {
            // Filter by extension
            let ext = file_path
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("");
            if !ext.is_empty() && !filter.is_importable(ext) {
                continue;
            }

            // Look up variant in catalog: try volume+path first, fall back to content hash
            let lookup = {
                let vol = volumes.iter().find(|v| file_path.starts_with(&v.mount_point));
                if let Some(v) = vol {
                    let relative_path = file_path
                        .strip_prefix(&v.mount_point)
                        .unwrap_or(file_path);
                    catalog.find_variant_by_volume_and_path(
                        &v.id.to_string(),
                        &relative_path.to_string_lossy(),
                    )?
                } else {
                    None
                }
            };
            // Fall back to hashing the file and looking up by content hash
            let lookup = match lookup {
                Some(v) => Some(v),
                None => {
                    let hash = content_store.hash_file(file_path)?;
                    catalog.get_variant_format(&hash)?.map(|fmt| (hash, fmt))
                }
            };

            if let Some((content_hash, format)) = lookup {
                let file_start = std::time::Instant::now();
                // Generate regular preview (always)
                let result = if force {
                    preview_gen.regenerate(&content_hash, file_path, &format)
                } else {
                    preview_gen.generate(&content_hash, file_path, &format)
                };
                // Also generate smart preview when --smart is set
                if smart {
                    let _ = if force { preview_gen.regenerate_smart(&content_hash, file_path, &format) }
                    else { preview_gen.generate_smart(&content_hash, file_path, &format) };
                }
                let file_elapsed = file_start.elapsed();
                let name = file_path.file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or_else(|| file_path.to_str().unwrap_or("?"));
                match result {
                    Ok(Some(_)) => {
                        generated += 1;
                        if cli.log { item_status(name, "generated", Some(file_elapsed)); }
                    }
                    Ok(None) => {
                        skipped += 1;
                        if cli.log { item_status(name, "skipped", Some(file_elapsed)); }
                    }
                    Err(e) => {
                        eprintln!("  Failed for {}: {e:#} ({})", file_path.display(), format_duration(file_elapsed));
                        failed += 1;
                    }
                }
            }
        }
    } else {
        // Catalog mode: iterate assets
        let volume_filter = match &volume {
            Some(label) => Some(registry.resolve_volume(label)?),
            None => None,
        };

        let assets = if let Some(asset_id) = &asset {
            let engine = QueryEngine::new(&catalog_root);
            let details = engine.show(asset_id)?;
            let uuid: uuid::Uuid = details.id.parse()?;
            vec![metadata_store.load(uuid)?]
        } else {
            let summaries = metadata_store.list()?;
            summaries
                .iter()
                .map(|s| metadata_store.load(s.id))
                .collect::<Result<Vec<_>, _>>()?
        };

        for asset_data in &assets {
            // Select the best variant for preview generation (respects user override)
            let idx = asset_data.preview_variant.as_ref()
                .and_then(|h| asset_data.variants.iter().position(|v| &v.content_hash == h))
                .or_else(|| maki::models::variant::best_preview_index(&asset_data.variants))
                .unwrap_or(0);
            if let Some(variant) = asset_data.variants.get(idx) {
                // In --upgrade mode, skip assets where the best variant is already the first
                if upgrade && idx == 0 {
                    skipped += 1;
                    continue;
                }

                // Apply format filter
                let ext = &variant.format;
                if !ext.is_empty() && !filter.is_importable(ext) {
                    skipped += 1;
                    continue;
                }

                // Try to find a reachable file for this variant
                let source_path = variant.locations.iter().find_map(|loc| {
                    // Apply volume filter
                    if let Some(ref vf) = volume_filter {
                        if loc.volume_id != vf.id {
                            return None;
                        }
                    }
                    volumes.iter().find_map(|v| {
                        if v.id == loc.volume_id && v.is_online {
                            let full = v.mount_point.join(&loc.relative_path);
                            if full.exists() { Some(full) } else { None }
                        } else {
                            None
                        }
                    })
                });

                // If we couldn't reach the file, record any offline volume
                // that held a location — so the end-of-run hint can tell
                // the user which disk to mount.
                if source_path.is_none() {
                    for loc in &variant.locations {
                        if let Some(v) = volumes.iter().find(|v| v.id == loc.volume_id) {
                            if !v.is_online {
                                offline_blockers.insert(v.label.clone());
                            }
                        }
                    }
                }

                if let Some(path) = source_path {
                    // Backfill video metadata if missing
                    if maki::asset_service::determine_asset_type(&variant.format) == maki::models::AssetType::Video
                        && !variant.source_metadata.contains_key("video_duration")
                    {
                        let service = AssetService::new(&catalog_root, verbosity, &config.preview);
                        service.backfill_video_metadata(&asset_data.id.to_string(), &variant.content_hash, &path);
                    }

                    let file_start = std::time::Instant::now();
                    let rotation = asset_data.preview_rotation;
                    // Generate regular preview (always)
                    let result = if force || upgrade {
                        preview_gen.regenerate_with_rotation(&variant.content_hash, &path, &variant.format, rotation)
                    } else {
                        preview_gen.generate(&variant.content_hash, &path, &variant.format)
                    };
                    // Also generate smart preview when --smart is set
                    if smart {
                        let _ = if force || upgrade {
                            preview_gen.regenerate_smart_with_rotation(&variant.content_hash, &path, &variant.format, rotation)
                        } else {
                            preview_gen.generate_smart(&variant.content_hash, &path, &variant.format)
                        };
                    }
                    let file_elapsed = file_start.elapsed();
                    let name = path.file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or_else(|| path.to_str().unwrap_or("?"));
                    match result {
                        Ok(Some(_)) => {
                            generated += 1;
                            if upgrade { upgraded += 1; }
                            if cli.log { item_status(name, if upgrade { "upgraded" } else { "generated" }, Some(file_elapsed)); }
                        }
                        Ok(None) => {
                            skipped += 1;
                            if cli.log { item_status(name, "skipped", Some(file_elapsed)); }
                        }
                        Err(e) => {
                            eprintln!("  Failed for {}: {e:#} ({})", asset_data.id, format_duration(file_elapsed));
                            failed += 1;
                        }
                    }
                } else {
                    skipped += 1;
                }
            } else {
                skipped += 1;
            }
        }
    }

    let preview_label = if smart { "smart preview(s)" } else { "preview(s)" };
    if cli.json {
        let mut result = serde_json::json!({
            "generated": generated,
            "skipped": skipped,
            "failed": failed,
        });
        if upgrade {
            result["upgraded"] = serde_json::json!(upgraded);
        }
        if smart {
            result["smart"] = serde_json::json!(true);
        }
        println!("{result}");
    } else {
        if upgrade && upgraded > 0 {
            println!(
                "Generated {} {} ({} upgraded), {} skipped, {} failed",
                generated, preview_label, upgraded, skipped, failed
            );
        } else {
            println!(
                "Generated {} {}, {} skipped, {} failed",
                generated, preview_label, skipped, failed
            );
        }
        // Tell the user which volumes blocked some skips so they don't
        // wonder why a file count looks low.
        if !offline_blockers.is_empty() {
            let mut labels: Vec<String> = offline_blockers.into_iter().collect();
            labels.sort();
            println!(
                "  Tip: some assets were skipped because their files \
                 live on offline volume(s): {}. Mount and re-run.",
                labels.join(", ")
            );
        }
    }
    Ok(())
}

/// Extracted body of `Commands::UpdateLocation`. See `run_import_command` for the
/// extraction pattern.
pub fn run_update_location_command(
        asset_id: String,
        from: String,
        to: String,
        volume: Option<String>,
        json: bool,
        log: bool,
        #[allow(unused_variables)] verbosity: maki::Verbosity,
) -> anyhow::Result<()> {
    #[allow(dead_code)]
    struct Ctx { json: bool, log: bool }
    let cli = Ctx { json, log };
    let (catalog_root, config) = maki::config::load_config()?;
    let service = AssetService::new(&catalog_root, verbosity, &config.preview);

    let to_path = std::fs::canonicalize(&to)
        .unwrap_or_else(|_| PathBuf::from(&to));

    let result = service.update_location(
        &asset_id,
        &from,
        &to_path,
        volume.as_deref(),
    )?;

    if cli.json {
        println!("{}", serde_json::to_string_pretty(&result)?);
    } else {
        let short_id = &result.asset_id[..8];
        println!(
            "Updated {} location for asset {short_id} on volume '{}'",
            result.file_type, result.volume_label,
        );
        println!("  {} -> {}", result.old_path, result.new_path);
    }
    Ok(())
}

/// Extracted body of `Commands::Cleanup`. See `run_import_command` for the
/// extraction pattern.
pub fn run_cleanup_command(
        volume: Option<String>,
        path: Option<String>,
        list: bool,
        apply: bool,
        json: bool,
        log: bool,
        #[allow(unused_variables)] verbosity: maki::Verbosity,
) -> anyhow::Result<()> {
    #[allow(dead_code)]
    struct Ctx { json: bool, log: bool }
    let cli = Ctx { json, log };
    let (catalog_root, config) = maki::config::load_config()?;
    let service = AssetService::new(&catalog_root, verbosity, &config.preview);

    // If --path is given without --volume, try to auto-detect the volume
    let volume = if volume.is_none() && path.is_some() {
        let registry = DeviceRegistry::new(&catalog_root);
        let p = std::path::Path::new(path.as_deref().unwrap());
        if p.is_absolute() {
            // Absolute path: find which volume contains it
            match registry.find_volume_for_path(p) {
                Ok(v) => Some(v.label.clone()),
                Err(_) => None, // fall through — cleanup will check all volumes
            }
        } else {
            None
        }
    } else {
        volume
    };

    // Convert absolute --path to relative (strip volume mount point)
    let path_prefix = if let (Some(ref p), Some(ref vol_label)) = (&path, &volume) {
        let abs = std::path::Path::new(p);
        if abs.is_absolute() {
            let registry = DeviceRegistry::new(&catalog_root);
            if let Ok(vol) = registry.resolve_volume(vol_label) {
                abs.strip_prefix(&vol.mount_point)
                    .ok()
                    .and_then(|rel| rel.to_str())
                    .map(|s| s.to_string())
                    .or_else(|| path.clone())
            } else {
                path.clone()
            }
        } else {
            path.clone()
        }
    } else {
        path
    };

    if verbosity.verbose {
        if let Some(ref prefix) = path_prefix {
            eprintln!("  Cleanup: path prefix \"{}\"", prefix);
        }
    }

    let show_log = cli.log;
    let show_list = list;
    let result = if show_log || show_list {
        use maki::asset_service::CleanupStatus;
        service.cleanup(
            volume.as_deref(),
            path_prefix.as_deref(),
            apply,
            |path, status, elapsed| {
                match status {
                    CleanupStatus::Ok if show_log => {
                        let name = path.file_name()
                            .and_then(|n| n.to_str())
                            .unwrap_or_else(|| path.to_str().unwrap_or("?"));
                        item_status(name, "ok", Some(elapsed));
                    }
                    CleanupStatus::Stale => {
                        let name = path.file_name()
                            .and_then(|n| n.to_str())
                            .unwrap_or_else(|| path.to_str().unwrap_or("?"));
                        item_status(name, "stale", Some(elapsed));
                    }
                    CleanupStatus::Offline => {
                        let name = path.file_name()
                            .and_then(|n| n.to_str())
                            .unwrap_or_else(|| path.to_str().unwrap_or("?"));
                        item_status(name, "offline", None);
                    }
                    CleanupStatus::LocationlessVariant => {
                        let name = path.to_str().unwrap_or("?");
                        item_status(name, "locationless variant removed", Some(elapsed));
                    }
                    CleanupStatus::OrphanedAsset => {
                        let name = path.to_str().unwrap_or("?");
                        item_status(name, "orphaned asset removed", Some(elapsed));
                    }
                    CleanupStatus::OrphanedFile => {
                        let name = path.file_name()
                            .and_then(|n| n.to_str())
                            .unwrap_or_else(|| path.to_str().unwrap_or("?"));
                        item_status(name, "orphaned file removed", Some(elapsed));
                    }
                    _ => {}
                }
            },
        )?
    } else {
        service.cleanup(
            volume.as_deref(),
            path_prefix.as_deref(),
            apply,
            |_, _, _| {},
        )?
    };

    if cli.json {
        println!("{}", serde_json::to_string_pretty(&result)?);
    } else {
        for err in &result.errors {
            eprintln!("  {err}");
        }

        if result.skipped_offline > 0 {
            eprintln!(
                "  Skipped {} offline volume(s).",
                result.skipped_offline
            );
        }

        if apply {
            let mut parts = vec![
                format!("{} checked", result.checked),
                format!("{} stale", result.stale),
                format!("{} removed", result.removed),
            ];
            if result.removed_variants > 0 {
                parts.push(format!("{} locationless variants removed", result.removed_variants));
            }
            if result.removed_assets > 0 {
                parts.push(format!("{} orphaned assets removed", result.removed_assets));
            }
            if result.removed_previews > 0 {
                parts.push(format!("{} orphaned previews removed", result.removed_previews));
            }
            if result.removed_smart_previews > 0 {
                parts.push(format!("{} orphaned smart previews removed", result.removed_smart_previews));
            }
            if result.removed_embeddings > 0 {
                parts.push(format!("{} orphaned embeddings removed", result.removed_embeddings));
            }
            if result.removed_face_files > 0 {
                parts.push(format!("{} orphaned face files removed", result.removed_face_files));
            }
            println!("Cleanup complete: {}", parts.join(", "));
        } else {
            let mut parts = vec![
                format!("{} checked", result.checked),
                format!("{} stale", result.stale),
            ];
            if result.locationless_variants > 0 {
                parts.push(format!("{} locationless variants", result.locationless_variants));
            }
            if result.orphaned_assets > 0 {
                parts.push(format!("{} orphaned assets", result.orphaned_assets));
            }
            if result.orphaned_previews > 0 {
                parts.push(format!("{} orphaned previews", result.orphaned_previews));
            }
            if result.orphaned_smart_previews > 0 {
                parts.push(format!("{} orphaned smart previews", result.orphaned_smart_previews));
            }
            if result.orphaned_embeddings > 0 {
                parts.push(format!("{} orphaned embeddings", result.orphaned_embeddings));
            }
            if result.orphaned_face_files > 0 {
                parts.push(format!("{} orphaned face files", result.orphaned_face_files));
            }
            println!("Cleanup complete: {}", parts.join(", "));
            let has_orphans = result.stale > 0
                || result.locationless_variants > 0
                || result.orphaned_assets > 0
                || result.orphaned_previews > 0
                || result.orphaned_smart_previews > 0
                || result.orphaned_embeddings > 0
                || result.orphaned_face_files > 0;
            if has_orphans {
                println!("  Run with --apply to remove stale records and orphaned files.");
            }
        }

        if result.skipped_global_passes {
            println!(
                "  Note: --volume/--path limits the scan to catalog records under that scope;"
            );
            println!(
                "        orphaned previews, embeddings, and face files are catalog-wide —"
            );
            println!(
                "        run `maki cleanup` without --volume/--path to check for those."
            );
        }
    }

    Ok(())
}

/// Extracted body of `Commands::Refresh`. See `run_import_command` for the
/// extraction pattern.
pub fn run_refresh_command(
        paths: Vec<String>,
        volume: Option<String>,
        asset: Option<String>,
        dry_run: bool,
        media: bool,
        reimport: bool,
        exif_only: bool,
        json: bool,
        log: bool,
        #[allow(unused_variables)] verbosity: maki::Verbosity,
) -> anyhow::Result<()> {
    #[allow(dead_code)]
    struct Ctx { json: bool, log: bool }
    let cli = Ctx { json, log };
    if reimport || exif_only {
        // --reimport: clear and re-extract all metadata from source files
        // --exif-only: re-extract only EXIF, leave tags/description/rating/label
        let catalog_root = maki::config::find_catalog_root()?;
        let engine = QueryEngine::new(&catalog_root);

        if asset.is_none() && paths.is_empty() {
            anyhow::bail!("--reimport/--exif-only requires --asset <ID> or asset IDs as arguments");
        }

        let asset_ids: Vec<String> = if let Some(ref id) = asset {
            vec![id.clone()]
        } else {
            paths.clone()
        };

        let mut reimported = 0usize;
        for id in &asset_ids {
            let result = if exif_only {
                engine.reimport_exif_only(id)
            } else {
                engine.reimport_metadata(id)
            };
            match result {
                Ok(tags) => {
                    reimported += 1;
                    if cli.log {
                        let short = if id.len() > 8 { &id[..8] } else { id };
                        eprintln!("  {} — reimported ({} tags)", short, tags.len());
                    }
                }
                Err(e) => {
                    eprintln!("  {} — error: {e}", if id.len() > 8 { &id[..8] } else { id.as_str() });
                }
            }
        }

        if cli.json {
            println!("{}", serde_json::json!({ "reimported": reimported }));
        } else {
            println!("Reimport metadata: {} asset(s) refreshed", reimported);
        }
        return Ok(());
    }

    let (catalog_root, config) = maki::config::load_config()?;
    let registry = DeviceRegistry::new(&catalog_root);

    let canonical_paths: Vec<PathBuf> = paths
        .iter()
        .map(|p| {
            std::fs::canonicalize(p)
                .unwrap_or_else(|_| PathBuf::from(p))
        })
        .collect();

    // Resolve volume
    let resolved_volume = if let Some(label) = &volume {
        Some(registry.resolve_volume(label)?)
    } else if !canonical_paths.is_empty() {
        Some(registry.find_volume_for_path(&canonical_paths[0])?)
    } else {
        None
    };

    // Resolve asset ID prefix
    let resolved_asset_id = if let Some(prefix) = &asset {
        let catalog = Catalog::open(&catalog_root)?;
        match catalog.resolve_asset_id(prefix)? {
            Some(id) => Some(id),
            None => anyhow::bail!("no asset found matching '{prefix}'"),
        }
    } else {
        None
    };

    let service = AssetService::new(&catalog_root, verbosity, &config.preview);
    let result = if cli.log {
        use maki::asset_service::RefreshStatus;
        service.refresh(
            &canonical_paths,
            resolved_volume.as_ref(),
            resolved_asset_id.as_deref(),
            dry_run,
            media,
            &config.import.exclude,
            |path, status, elapsed| {
                let label = match status {
                    RefreshStatus::Unchanged => "unchanged",
                    RefreshStatus::Refreshed => "refreshed",
                    RefreshStatus::Missing => "missing",
                    RefreshStatus::Offline => "offline",
                    RefreshStatus::SidecarPresent => "skipped (sidecar present)",
                };
                let name = path.file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or_else(|| path.to_str().unwrap_or("?"));
                item_status(name, label, Some(elapsed));
            },
        )?
    } else {
        service.refresh(
            &canonical_paths,
            resolved_volume.as_ref(),
            resolved_asset_id.as_deref(),
            dry_run,
            media,
            &config.import.exclude,
            |_, _, _| {},
        )?
    };

    if cli.json {
        println!("{}", serde_json::to_string_pretty(&result)?);
    } else {
        for err in &result.errors {
            eprintln!("  {err}");
        }

        if dry_run {
            eprint!("Dry run — ");
        }

        let mut parts: Vec<String> = Vec::new();
        if result.refreshed > 0 {
            parts.push(format!("{} refreshed", result.refreshed));
        }
        if result.unchanged > 0 {
            parts.push(format!("{} unchanged", result.unchanged));
        }
        if result.missing > 0 {
            parts.push(format!("{} missing", result.missing));
        }
        if result.skipped > 0 {
            parts.push(format!("{} skipped (offline)", result.skipped));
        }
        if parts.is_empty() {
            println!("Refresh: nothing to check");
        } else {
            println!("Refresh complete: {}", parts.join(", "));
        }
    }

    Ok(())
}

/// Extracted body of `Commands::Relocate`. See `run_import_command` for the
/// extraction pattern.
pub fn run_relocate_command(
        asset_ids: Vec<String>,
        target: Option<String>,
        query: Option<String>,
        remove_source: bool,
        create_sidecars: bool,
        dry_run: bool,
        json: bool,
        log: bool,
        #[allow(unused_variables)] verbosity: maki::Verbosity,
) -> anyhow::Result<()> {
    #[allow(dead_code)]
    struct Ctx { json: bool, log: bool }
    let cli = Ctx { json, log };
    let (catalog_root, config) = maki::config::load_config()?;
    let service = AssetService::new(&catalog_root, verbosity, &config.preview);

    // Resolve asset IDs: --query, positional args, or stdin
    let ids: Vec<String> = if let Some(ref q) = query {
        let engine = QueryEngine::new(&catalog_root);
        engine.search(q)?.into_iter().map(|r| r.asset_id).collect()
    } else if asset_ids.is_empty() {
        use std::io::BufRead;
        std::io::stdin().lock().lines()
            .filter_map(|l| l.ok())
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty())
            .collect()
    } else {
        asset_ids
    };

    if ids.is_empty() {
        anyhow::bail!("no asset IDs specified. Use --query, positional args, or pipe from stdin.");
    }

    // Determine target volume: --target flag, or second positional arg for single-asset compat
    let target_volume = match target {
        Some(t) => t,
        None => {
            // Backward compat: `maki relocate <asset-id> <volume>`
            if ids.len() == 2 && query.is_none() {
                let vol = ids[1].clone();
                // Treat as single-asset mode: first arg is asset, second is volume
                let single_id = ids[0].clone();
                let result = service.relocate(&single_id, &vol, remove_source, create_sidecars, dry_run)?;

                if cli.json {
                    println!("{}", serde_json::to_string_pretty(&result)?);
                } else {
                    if dry_run {
                        println!("Dry run — no changes made:");
                    }
                    for action in &result.actions {
                        println!("  {action}");
                    }
                    let verb = if remove_source { "moved" } else { "copied" };
                    let mut parts: Vec<String> = Vec::new();
                    if result.copied > 0 {
                        parts.push(format!("{} {verb}", result.copied));
                    }
                    if result.skipped > 0 {
                        parts.push(format!("{} skipped", result.skipped));
                    }
                    if parts.is_empty() {
                        if result.actions.len() == 1 {
                            // The "already on target" message was printed above
                        } else {
                            println!("Relocate: nothing to do");
                        }
                    } else {
                        println!("Relocate complete: {}", parts.join(", "));
                    }
                }
                return Ok(());
            }
            anyhow::bail!("--target <volume> is required for batch relocate");
        }
    };

    // Batch relocate
    let total = ids.len();
    let mut total_copied: usize = 0;
    let mut total_skipped: usize = 0;
    let mut total_removed: usize = 0;
    let mut errors: Vec<String> = Vec::new();

    if dry_run && !cli.json {
        println!("Dry run — no changes will be made:");
    }

    for (i, id) in ids.iter().enumerate() {
        match service.relocate(id, &target_volume, remove_source, create_sidecars, dry_run) {
            Ok(result) => {
                total_copied += result.copied;
                total_skipped += result.skipped;
                total_removed += result.removed;

                if cli.log {
                    let verb = if remove_source { "moved" } else { "copied" };
                    eprintln!("[{}/{}] {} — {} {verb}, {} skipped",
                        i + 1, total, &id[..8.min(id.len())],
                        result.copied, result.skipped);
                }
            }
            Err(e) => {
                let msg = format!("{}: {e:#}", &id[..8.min(id.len())]);
                if cli.log {
                    eprintln!("[{}/{}] ERROR {msg}", i + 1, total);
                }
                errors.push(msg);
            }
        }
    }

    if cli.json {
        println!("{}", serde_json::json!({
            "assets": total,
            "copied": total_copied,
            "skipped": total_skipped,
            "removed": total_removed,
            "errors": errors,
            "dry_run": dry_run,
        }));
    } else {
        let verb = if remove_source { "moved" } else { "copied" };
        let mut parts: Vec<String> = Vec::new();
        parts.push(format!("{total} assets"));
        if total_copied > 0 {
            parts.push(format!("{total_copied} files {verb}"));
        }
        if total_skipped > 0 {
            parts.push(format!("{total_skipped} skipped"));
        }
        if !errors.is_empty() {
            parts.push(format!("{} errors", errors.len()));
            for e in &errors {
                eprintln!("  error: {e}");
            }
        }
        println!("Relocate complete: {}", parts.join(", "));
    }

    Ok(())
}

/// Extracted body of `Commands::Trash`. See `run_import_command` for the
/// extraction pattern.
pub fn run_trash_command(cmd: TrashCommands, json: bool) -> anyhow::Result<()> {
    let (catalog_root, config) = maki::config::load_config()?;
    match cmd {
        TrashCommands::List => {
            let entries = maki::trash::Trash::list(&catalog_root)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&entries)?);
            } else if entries.is_empty() {
                println!("Trash is empty.");
            } else {
                let total: u64 = entries.iter().map(|e| e.size).sum();
                for e in &entries {
                    println!("{:>10}  {}", format_size(e.size), e.trash_path);
                }
                println!(
                    "{} file(s), {} — restore with `maki trash restore <path>`, purge with `maki trash empty`.",
                    entries.len(),
                    format_size(total),
                );
            }
        }
        TrashCommands::Restore { path } => {
            let target = maki::trash::Trash::restore(&catalog_root, &path)?;
            if json {
                println!("{}", serde_json::json!({"restored": target.display().to_string()}));
            } else {
                println!("Restored: {}", target.display());
                println!(
                    "  The catalog was not updated — re-register the file with `maki import` or `maki refresh`."
                );
            }
        }
        TrashCommands::Empty { older_than, all, dry_run } => {
            let cutoff = if all {
                None
            } else {
                Some(older_than.unwrap_or(config.trash.retention_days))
            };
            let result = maki::trash::Trash::empty(&catalog_root, cutoff, dry_run)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&result)?);
            } else if dry_run {
                println!(
                    "Trash empty (dry run): {} file(s), {} would be removed{}",
                    result.files_removed,
                    format_size(result.bytes_freed),
                    cutoff.map(|d| format!(" (older than {d} days)")).unwrap_or_default(),
                );
            } else {
                println!(
                    "Trash emptied: {} file(s), {} freed{}",
                    result.files_removed,
                    format_size(result.bytes_freed),
                    cutoff.map(|d| format!(" (older than {d} days)")).unwrap_or_default(),
                );
            }
        }
    }
    Ok(())
}

/// Extracted body of `Commands::Doctor`. See `run_import_command` for the
/// extraction pattern.
pub fn run_doctor_command(
    sample: Option<usize>,
    repair: bool,
    json: bool,
    log: bool,
) -> anyhow::Result<()> {
    let (catalog_root, _config) = maki::config::load_config()?;

    let report = maki::doctor::run_doctor(&catalog_root, sample, repair, |asset_id, issue| {
        if log {
            let short_id = &asset_id[..8.min(asset_id.len())];
            eprintln!("  {short_id} — {issue}");
        }
    })?;

    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }

    let scope = if report.sampled {
        format!(
            "{} of {} assets checked (sampled)",
            report.assets_checked, report.sidecars_total
        )
    } else {
        format!("{} assets checked", report.assets_checked)
    };

    if report.healthy() {
        println!("Doctor: no issues found — {scope}.");
        return Ok(());
    }

    println!("Doctor: {} issue(s) found — {scope}.", report.issues());
    if !report.missing_in_catalog.is_empty() {
        println!(
            "  {} sidecar(s) without a catalog row",
            report.missing_in_catalog.len()
        );
    }
    if !report.missing_sidecar.is_empty() {
        println!(
            "  {} catalog row(s) without a sidecar (phantom rows)",
            report.missing_sidecar.len()
        );
    }
    if !report.unreadable_sidecars.is_empty() {
        println!(
            "  {} unreadable sidecar file(s)",
            report.unreadable_sidecars.len()
        );
    }
    if !report.mismatched.is_empty() {
        println!("  {} asset(s) with field mismatches:", report.mismatched.len());
        // Aggregate by field so the summary reads as "what kind of drift"
        // rather than a per-asset wall of text (use --log for per-asset).
        let mut by_field: std::collections::BTreeMap<&str, usize> =
            std::collections::BTreeMap::new();
        for m in &report.mismatched {
            for f in &m.fields {
                *by_field.entry(f.as_str()).or_default() += 1;
            }
        }
        for (field, count) in by_field {
            println!("    {field}: {count}");
        }
    }

    if repair {
        println!("Repaired {} asset(s) from YAML sidecars.", report.repaired);
        if !report.unreadable_sidecars.is_empty() {
            println!("  Unreadable sidecars were NOT touched — inspect them manually.");
        }
    } else {
        println!("Run `maki doctor --repair` to rebuild the SQLite side from the YAML sidecars.");
    }
    Ok(())
}

/// Render the timestamp portion of a history line compactly
/// (`YYYY-MM-DD HH:MM`), tolerating any RFC 3339 input.
fn short_timestamp(ts: &str) -> String {
    chrono::DateTime::parse_from_rfc3339(ts)
        .map(|dt| dt.format("%Y-%m-%d %H:%M").to_string())
        .unwrap_or_else(|_| ts.to_string())
}

/// Human-readable list of the metadata fields that differ between two
/// asset states, for `maki history <asset>`. Keeps it readable rather than
/// a full structural diff.
fn describe_asset_change(before: &maki::models::Asset, after: &maki::models::Asset) -> Vec<String> {
    let mut out = Vec::new();
    if before.rating != after.rating {
        out.push(format!(
            "rating {} → {}",
            before.rating.map(|r| r.to_string()).unwrap_or_else(|| "—".into()),
            after.rating.map(|r| r.to_string()).unwrap_or_else(|| "—".into()),
        ));
    }
    if before.color_label != after.color_label {
        out.push(format!(
            "label {} → {}",
            before.color_label.clone().unwrap_or_else(|| "—".into()),
            after.color_label.clone().unwrap_or_else(|| "—".into()),
        ));
    }
    if before.name != after.name {
        out.push(format!(
            "name {} → {}",
            before.name.clone().unwrap_or_else(|| "—".into()),
            after.name.clone().unwrap_or_else(|| "—".into()),
        ));
    }
    if before.description != after.description {
        out.push("description changed".to_string());
    }
    if before.created_at != after.created_at {
        out.push(format!(
            "date {} → {}",
            before.created_at.to_rfc3339(),
            after.created_at.to_rfc3339(),
        ));
    }
    if before.tags != after.tags {
        let bset: std::collections::HashSet<&String> = before.tags.iter().collect();
        let aset: std::collections::HashSet<&String> = after.tags.iter().collect();
        let added: Vec<&str> = after.tags.iter().filter(|t| !bset.contains(t)).map(|s| s.as_str()).collect();
        let removed: Vec<&str> = before.tags.iter().filter(|t| !aset.contains(t)).map(|s| s.as_str()).collect();
        if !added.is_empty() {
            out.push(format!("tags +[{}]", added.join(", ")));
        }
        if !removed.is_empty() {
            out.push(format!("tags -[{}]", removed.join(", ")));
        }
    }
    out
}

/// Extracted body of `Commands::Undo`.
pub fn run_undo_command(
    dry_run: bool,
    force: bool,
    json: bool,
    log: bool,
) -> anyhow::Result<()> {
    let catalog_root = maki::config::find_catalog_root()?;
    let history = maki::history::HistoryStore::from_config(&catalog_root);
    let engine = QueryEngine::new(&catalog_root);

    let op = match history.newest_active()? {
        Some(op) => op,
        None => {
            if json {
                println!("{}", serde_json::json!({ "undone": false, "reason": "nothing to undo" }));
            } else {
                println!("Nothing to undo.");
            }
            return Ok(());
        }
    };

    let store = MetadataStore::new(&catalog_root);

    let mut restored: Vec<String> = Vec::new();
    let mut conflicted: Vec<String> = Vec::new();
    let mut any_pending_xmp = false;

    for delta in &op.assets {
        // Resolve the asset's CURRENT state. If it can't be loaded
        // (sidecar gone), treat it as a conflict — we won't silently
        // recreate it here.
        let uuid: uuid::Uuid = match delta.asset_id.parse() {
            Ok(u) => u,
            Err(_) => {
                conflicted.push(delta.asset_id.clone());
                continue;
            }
        };
        let current = store.load(uuid).ok();
        let changed_since = match &current {
            Some(cur) => cur != &delta.after,
            None => true,
        };

        if changed_since && !force {
            conflicted.push(delta.asset_id.clone());
            if log {
                eprintln!("  {} — changed since this edit; skipped (use --force)", &delta.asset_id[..8.min(delta.asset_id.len())]);
            }
            continue;
        }

        if delta.before.recipes.iter().any(|r| {
            r.location.relative_path.extension().and_then(|e| e.to_str())
                .map(|e| e.eq_ignore_ascii_case("xmp")).unwrap_or(false)
        }) {
            any_pending_xmp = true;
        }

        if !dry_run {
            engine.restore_asset(&delta.before)?;
        }
        restored.push(delta.asset_id.clone());
        if log {
            eprintln!("  {} — restored", &delta.asset_id[..8.min(delta.asset_id.len())]);
        }
    }

    // Mark undone only when we actually restored something and left no
    // unforced conflicts behind.
    let will_mark = !dry_run && !restored.is_empty() && conflicted.is_empty();
    if will_mark {
        history.mark_undone(&op.id)?;
    }

    if json {
        let report = serde_json::json!({
            "undone": will_mark,
            "dry_run": dry_run,
            "operation": {
                "id": op.id,
                "command": op.command,
                "summary": op.summary,
                "timestamp": op.timestamp,
            },
            "restored": restored,
            "conflicts": conflicted,
            "writeback_pending": any_pending_xmp && !dry_run,
        });
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }

    let prefix = if dry_run { "Would undo" } else { "Undid" };
    println!(
        "{prefix}: {} ({}, {} asset(s)) [from {}]",
        op.summary,
        op.command,
        restored.len(),
        short_timestamp(&op.timestamp),
    );
    if !conflicted.is_empty() {
        println!(
            "  {} asset(s) changed since this edit — skipped{}",
            conflicted.len(),
            if force { " (unexpected with --force)" } else { "; re-run with --force to override" },
        );
        if !force && !dry_run {
            println!("  Operation left in place (not marked undone).");
        }
    }
    if any_pending_xmp && !dry_run && !restored.is_empty() {
        println!("  Run `maki writeback` to propagate the restored values to .xmp files.");
    }
    Ok(())
}

/// Extracted body of `Commands::History`.
pub fn run_history_command(
    asset: Option<String>,
    limit: usize,
    json: bool,
) -> anyhow::Result<()> {
    let catalog_root = maki::config::find_catalog_root()?;
    let history = maki::history::HistoryStore::from_config(&catalog_root);

    match asset {
        Some(prefix) => {
            let catalog = Catalog::open(&catalog_root)?;
            let full_id = catalog
                .resolve_asset_id(&prefix)?
                .ok_or_else(|| anyhow::anyhow!("no asset found matching '{prefix}'"))?;
            let ops = history.list_for_asset(&full_id, Some(limit))?;

            if json {
                println!("{}", serde_json::to_string_pretty(&ops)?);
                return Ok(());
            }

            if ops.is_empty() {
                println!("No edit history for {}.", &full_id[..8.min(full_id.len())]);
                return Ok(());
            }
            println!("History for {} ({} operation(s)):", &full_id[..8.min(full_id.len())], ops.len());
            for op in &ops {
                let changes = op
                    .assets
                    .iter()
                    .find(|d| d.asset_id == full_id)
                    .map(|d| describe_asset_change(&d.before, &d.after))
                    .unwrap_or_default();
                let detail = if changes.is_empty() {
                    op.summary.clone()
                } else {
                    changes.join("; ")
                };
                println!("  {}  {:<11} {detail}", short_timestamp(&op.timestamp), op.command);
            }
        }
        None => {
            let ops = history.list(Some(limit))?;
            if json {
                println!("{}", serde_json::to_string_pretty(&ops)?);
                return Ok(());
            }
            if ops.is_empty() {
                println!("No edit history.");
                return Ok(());
            }
            println!("Recent edits ({} operation(s)):", ops.len());
            for op in &ops {
                println!(
                    "  {}  {:<11} {}  ({} asset(s))",
                    short_timestamp(&op.timestamp),
                    op.command,
                    op.summary,
                    op.assets.len(),
                );
            }
        }
    }
    Ok(())
}
