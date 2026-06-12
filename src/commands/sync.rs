//! Sync commands: `writeback`, `sync-metadata`, `sync`, `verify`.

use super::*;

/// Extracted body of `Commands::Writeback`. See `run_import_command` for the
/// extraction pattern.
#[cfg(feature = "pro")]
pub fn run_writeback_command(
        query: Option<String>,
        volume: Option<String>,
        asset: Option<String>,
        all: bool,
        force: bool,
        mirror_tags: bool,
        embed: bool,
        no_trash: bool,
        dry_run: bool,
        asset_ids: Vec<String>,
        json: bool,
        log: bool,
        #[allow(unused_variables)] verbosity: maki::Verbosity,
) -> anyhow::Result<()> {
    #[allow(dead_code)]
    struct Ctx { json: bool, log: bool }
    let cli = Ctx { json, log };
    let catalog_root = maki::config::find_catalog_root()?;
    let engine = maki::query::QueryEngine::new(&catalog_root);
    let _start = std::time::Instant::now();

    let scope = engine.resolve_scope(query.as_deref(), asset.as_deref(), &asset_ids)?;

    // `[writeback] mirror_tags` in maki.toml turns mirror-tags on by
    // default for every writeback. The CLI `--mirror-tags` flag still
    // takes effect on top (OR semantics); there's no need for a
    // `--no-mirror-tags` opt-out because mirror semantics are
    // idempotent at the keyword level — re-running adds nothing.
    let cfg = maki::config::CatalogConfig::load(&catalog_root).unwrap_or_default();
    let effective_mirror_tags = mirror_tags || cfg.writeback.mirror_tags;

    // `--embed` REPLACES the recipe flush for this run: catalog
    // metadata is written into the JPEG variant files' embedded XMP;
    // the .xmp recipe sidecars (and their pending flags) are untouched.
    if embed {
        let result = engine.writeback_embedded(
            volume.as_deref(),
            scope.as_ref(),
            all,
            force,
            effective_mirror_tags,
            no_trash,
            dry_run,
            cli.log,
            None,
        )?;

        if cli.json {
            println!("{}", serde_json::to_string_pretty(&result)?);
        } else {
            if dry_run {
                eprint!("Dry run: ");
            }
            let mut parts = Vec::new();
            parts.push(format!("{} written", result.written));
            if result.already_in_sync > 0 {
                parts.push(format!("{} already in sync", result.already_in_sync));
            }
            if result.skipped > 0 {
                let mut s = format!("{} skipped", result.skipped);
                if !result.skipped_offline_volumes.is_empty() {
                    let labels: Vec<&str> = result
                        .skipped_offline_volumes
                        .iter()
                        .map(String::as_str)
                        .collect();
                    s.push_str(&format!(" (offline volumes: {})", labels.join(", ")));
                }
                parts.push(s);
            }
            if result.failed > 0 {
                parts.push(format!("{} failed", result.failed));
            }
            println!("Writeback (embedded): {}", parts.join(", "));
            if result.trashed_originals > 0 {
                println!(
                    "{} original(s) preserved in trash — maki trash list",
                    result.trashed_originals
                );
            }
            for e in &result.errors {
                eprintln!("  Error: {e}");
            }
        }

        return Ok(());
    }

    let result = engine.writeback(
        volume.as_deref(),
        None, // asset_filter replaced by scope
        scope.as_ref(),
        all,
        force,
        effective_mirror_tags,
        dry_run,
        cli.log,
        None,
    )?;

    if cli.json {
        println!("{}", serde_json::to_string_pretty(&result)?);
    } else {
        if dry_run {
            eprint!("Dry run: ");
        }
        let mut parts = Vec::new();
        parts.push(format!("{} written", result.written));
        if result.already_in_sync > 0 {
            // Distinct from "written": these recipes had the catalog's
            // values already on disk (typical case after an external
            // rsync from a primary volume to a registered backup
            // volume). No file writes happened; the pending flag cleared
            // and any drifted content_hash was reconciled in-place.
            parts.push(format!("{} already in sync", result.already_in_sync));
        }
        if result.skipped > 0 {
            let mut s = format!("{} skipped", result.skipped);
            if !result.skipped_offline_volumes.is_empty() {
                let labels: Vec<&str> = result
                    .skipped_offline_volumes
                    .iter()
                    .map(String::as_str)
                    .collect();
                s.push_str(&format!(" (offline volumes: {})", labels.join(", ")));
            }
            parts.push(s);
        }
        if result.failed > 0 {
            parts.push(format!("{} failed", result.failed));
        }
        println!("Writeback: {}", parts.join(", "));
        for e in &result.errors {
            eprintln!("  Error: {e}");
        }
    }

    Ok(())
}

/// Extracted body of `Commands::SyncMetadata`. See `run_import_command` for the
/// extraction pattern.
#[cfg(feature = "pro")]
pub fn run_sync_metadata_command(
        query: Option<String>,
        volume: Option<String>,
        asset: Option<String>,
        dry_run: bool,
        media: bool,
        asset_ids: Vec<String>,
        json: bool,
        log: bool,
        #[allow(unused_variables)] verbosity: maki::Verbosity,
) -> anyhow::Result<()> {
    #[allow(dead_code)]
    struct Ctx { json: bool, log: bool }
    let cli = Ctx { json, log };
    let _start = std::time::Instant::now();
    let (catalog_root, config) = maki::config::load_config()?;
    let registry = DeviceRegistry::new(&catalog_root);
    let engine = maki::query::QueryEngine::new(&catalog_root);

    // Resolve volume
    let resolved_volume = if let Some(label) = &volume {
        Some(registry.resolve_volume(label)?)
    } else {
        None
    };

    // Resolve scope (query/asset/asset_ids) to individual asset IDs
    let scope = engine.resolve_scope(query.as_deref(), asset.as_deref(), &asset_ids)?;
    let asset_id_list: Vec<Option<String>> = match scope {
        Some(set) => set.into_iter().map(Some).collect(),
        None => vec![None], // process all
    };

    let service = AssetService::new(&catalog_root, verbosity, &config.preview);
    let mut result = maki::asset_service::SyncMetadataResult { dry_run, ..Default::default() };
    for aid in &asset_id_list {
        let r = if cli.log {
            use maki::asset_service::SyncMetadataStatus;
            service.sync_metadata(
                resolved_volume.as_ref(),
                aid.as_deref(),
                dry_run,
                media,
                &config.import.exclude,
                |path, status, elapsed| {
                    let label = match status {
                        SyncMetadataStatus::Inbound => "inbound",
                        SyncMetadataStatus::Outbound => "outbound",
                        SyncMetadataStatus::Unchanged => "unchanged",
                        SyncMetadataStatus::Missing => "missing",
                        SyncMetadataStatus::Offline => "offline",
                        SyncMetadataStatus::Conflict => "CONFLICT",
                        SyncMetadataStatus::Error => "error",
                    };
                    let name = path.file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or_else(|| path.to_str().unwrap_or("?"));
                    item_status(name, label, Some(elapsed));
                },
            )?
        } else {
            service.sync_metadata(
                resolved_volume.as_ref(),
                aid.as_deref(),
                dry_run,
                media,
                &config.import.exclude,
                |_, _, _| {},
            )?
        };
        result.inbound += r.inbound;
        result.outbound += r.outbound;
        result.unchanged += r.unchanged;
        result.conflicts += r.conflicts;
        result.skipped += r.skipped;
        result.media_refreshed += r.media_refreshed;
        result.errors.extend(r.errors);
    }

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
        if result.inbound > 0 {
            parts.push(format!("{} read from disk", result.inbound));
        }
        if result.outbound > 0 {
            parts.push(format!("{} written to disk", result.outbound));
        }
        if result.conflicts > 0 {
            parts.push(format!("{} conflicts (skipped)", result.conflicts));
        }
        if result.media_refreshed > 0 {
            parts.push(format!("{} media refreshed", result.media_refreshed));
        }
        if result.unchanged > 0 {
            parts.push(format!("{} unchanged", result.unchanged));
        }
        if result.skipped > 0 {
            parts.push(format!("{} skipped", result.skipped));
        }
        if parts.is_empty() {
            println!("Sync metadata: nothing to do");
        } else {
            println!("Sync metadata: {}", parts.join(", "));
        }

        if result.conflicts > 0 {
            eprintln!("  Tip: resolve conflicts by running 'maki refresh' (accept external) or 'maki writeback' (keep DAM edits).");
        }
    }

    Ok(())
}

/// Extracted body of `Commands::Sync`. See `run_import_command` for the
/// extraction pattern.
pub fn run_sync_command(
        paths: Vec<String>,
        volume: Option<String>,
        apply: bool,
        remove_stale: bool,
        json: bool,
        log: bool,
        #[allow(unused_variables)] verbosity: maki::Verbosity,
) -> anyhow::Result<()> {
    #[allow(dead_code)]
    struct Ctx { json: bool, log: bool }
    let cli = Ctx { json, log };
    if paths.is_empty() {
        anyhow::bail!("no paths specified for sync.");
    }
    if remove_stale && !apply {
        anyhow::bail!("--remove-stale requires --apply.");
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

    let volume = if let Some(label) = &volume {
        registry.resolve_volume(label)?
    } else {
        registry.find_volume_for_path(&canonical_paths[0])?
    };

    let service = AssetService::new(&catalog_root, verbosity, &config.preview);
    let result = if cli.log {
        use maki::asset_service::SyncStatus;
        service.sync(
            &canonical_paths,
            &volume,
            apply,
            remove_stale,
            &config.import.exclude,
            |path, status, elapsed| {
                let label = match status {
                    SyncStatus::Unchanged => "unchanged",
                    SyncStatus::Moved => "moved",
                    SyncStatus::New => "new",
                    SyncStatus::Modified => "modified",
                    SyncStatus::Missing => "missing",
                };
                let name = path.file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or_else(|| path.to_str().unwrap_or("?"));
                item_status(name, label, Some(elapsed));
            },
        )?
    } else {
        service.sync(
            &canonical_paths,
            &volume,
            apply,
            remove_stale,
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

        let mut parts: Vec<String> = Vec::new();
        if result.unchanged > 0 {
            parts.push(format!("{} unchanged", result.unchanged));
        }
        if result.moved > 0 {
            parts.push(format!("{} moved", result.moved));
        }
        if result.new_files > 0 {
            parts.push(format!("{} new", result.new_files));
        }
        if result.modified > 0 {
            parts.push(format!("{} modified", result.modified));
        }
        if result.missing > 0 {
            parts.push(format!("{} missing", result.missing));
        }
        if result.stale_removed > 0 {
            parts.push(format!("{} stale removed", result.stale_removed));
        }
        if result.orphaned_cleaned > 0 {
            parts.push(format!("{} orphaned assets cleaned", result.orphaned_cleaned));
        }
        if parts.is_empty() {
            println!("Sync: nothing to sync");
        } else {
            if !apply && (result.moved > 0 || result.modified > 0 || result.missing > 0) {
                eprint!("Dry run — ");
            }
            println!("Sync complete: {}", parts.join(", "));
        }
        if !apply && (result.moved > 0 || result.modified > 0) {
            println!("  Run with --apply to apply changes.");
        }
        if result.missing > 0 && !remove_stale {
            println!("  Run with --apply --remove-stale to remove missing file records.");
        }
        if result.new_files > 0 {
            println!("  Tip: run 'maki import' to import new files.");
        }
        // After sync, variants whose only locations were removed linger
        // in the catalog (often as the asset's selected best-preview
        // variant). They confuse subsequent `preview`/`generate-previews`
        // calls — `maki cleanup --apply` removes them and their derived
        // preview/embedding/face files.
        if result.locationless_after > 0 {
            println!(
                "  Tip: {} variant(s) have no remaining locations. \
                 Run 'maki cleanup --apply' to remove them and their \
                 orphaned previews/embeddings/face files.",
                result.locationless_after
            );
        }
    }

    Ok(())
}

/// Extracted body of `Commands::Verify`. See `run_import_command` for the
/// extraction pattern.
pub fn run_verify_command(
        paths: Vec<String>,
        volume: Option<String>,
        asset: Option<String>,
        include: Vec<String>,
        skip: Vec<String>,
        max_age: Option<u64>,
        force: bool,
        json: bool,
        log: bool,
        #[allow(unused_variables)] verbosity: maki::Verbosity,
) -> anyhow::Result<()> {
    #[allow(dead_code)]
    struct Ctx { json: bool, log: bool }
    let cli = Ctx { json, log };
    use maki::asset_service::FileTypeFilter;

    let (catalog_root, config) = maki::config::load_config()?;
    let service = AssetService::new(&catalog_root, verbosity, &config.preview);

    let max_age_days: Option<u64> = if force {
        None
    } else {
        max_age.or(config.verify.max_age_days)
    };

    // Build file type filter (same logic as import)
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

    let canonical_paths: Vec<PathBuf> = paths
        .iter()
        .map(|p| {
            std::fs::canonicalize(p)
                .unwrap_or_else(|_| PathBuf::from(p))
        })
        .collect();

    let result = if cli.log {
        use maki::asset_service::VerifyStatus;
        service.verify(
            &canonical_paths,
            volume.as_deref(),
            asset.as_deref(),
            &filter,
            max_age_days,
            |path, status, elapsed| {
                let label = match status {
                    VerifyStatus::Ok => "OK",
                    VerifyStatus::Mismatch => "FAILED",
                    VerifyStatus::Modified => "MODIFIED",
                    VerifyStatus::Missing => "MISSING",
                    VerifyStatus::Skipped => "SKIPPED",
                    VerifyStatus::SkippedRecent => "RECENT",
                    VerifyStatus::Untracked => "UNTRACKED",
                };
                let name = path.file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or_else(|| path.to_str().unwrap_or("?"));
                item_status(name, label, Some(elapsed));
            },
        )?
    } else {
        service.verify(
            &canonical_paths,
            volume.as_deref(),
            asset.as_deref(),
            &filter,
            max_age_days,
            |_, _, _| {},
        )?
    };

    if cli.json {
        println!("{}", serde_json::to_string_pretty(&result)?);
    } else {
        // Print error details
        for err in &result.errors {
            eprintln!("  {err}");
        }

        // Print summary
        let mut parts: Vec<String> = Vec::new();
        if result.verified > 0 {
            parts.push(format!("{} verified", result.verified));
        }
        if result.modified > 0 {
            parts.push(format!("{} modified", result.modified));
        }
        if result.failed > 0 {
            parts.push(format!("{} FAILED", result.failed));
        }
        if result.skipped_recent > 0 {
            let age_label = max_age_days
                .map(|d| format!("{d} days"))
                .unwrap_or_else(|| "max age".to_string());
            parts.push(format!(
                "{} skipped (verified within {})",
                result.skipped_recent, age_label
            ));
        }
        if result.skipped > 0 {
            parts.push(format!("{} skipped", result.skipped));
        }
        if parts.is_empty() {
            println!("Verify: nothing to verify");
        } else {
            println!("Verify complete: {}", parts.join(", "));
        }
    }

    if result.failed > 0 {
        anyhow::bail!("verification failed for {} file(s)", result.failed);
    }

    Ok(())
}
