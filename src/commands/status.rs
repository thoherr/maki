//! Reporting commands: `status`, `stats`, `backup-status` and their human-output formatters.

use super::*;

pub fn print_stats_human(stats: &maki::catalog::CatalogStats) {
    let o = &stats.overview;
    println!("Catalog Overview");
    println!("  Assets:    {}", o.assets);
    println!("  Variants:  {}", o.variants);
    println!("  Recipes:   {}", o.recipes);
    println!("  Volumes:   {} ({} online, {} offline)", o.volumes_total, o.volumes_online, o.volumes_offline);
    println!("  Total size: {}", format_size(o.total_size));

    if let Some(types) = &stats.types {
        println!("\nAsset Types");
        for t in &types.asset_types {
            println!("  {:<12} {:>6}  ({:.1}%)", t.asset_type, t.count, t.percentage);
        }
        if !types.variant_formats.is_empty() {
            println!("\nVariant Formats");
            for f in &types.variant_formats {
                println!("  {:<12} {:>6}", f.format, f.count);
            }
        }
        if !types.recipe_formats.is_empty() {
            println!("\nRecipe Formats");
            for f in &types.recipe_formats {
                println!("  {:<12} {:>6}", f.format, f.count);
            }
        }
    }

    if let Some(volumes) = &stats.volumes {
        println!("\nVolumes");
        for v in volumes {
            let status = if v.is_online { "online" } else { "offline" };
            if let Some(purpose) = &v.purpose {
                println!("  {} [{}] [{}]", v.label, status, purpose);
            } else {
                println!("  {} [{}]", v.label, status);
            }
            println!("    Assets: {}  Variants: {}  Recipes: {}", v.assets, v.variants, v.recipes);
            println!("    Size: {}  Directories: {}", format_size(v.size), v.directories);
            if !v.formats.is_empty() {
                println!("    Formats: {}", v.formats.join(", "));
            }
            println!("    Verified: {}/{} ({:.1}%)", v.verified_count, v.total_locations, v.verification_pct);
            if let Some(oldest) = &v.oldest_verified_at {
                println!("    Oldest verification: {oldest}");
            }
        }
    }

    if let Some(tags) = &stats.tags {
        println!("\nTags");
        println!("  Unique tags:     {}", tags.unique_tags);
        println!("  Tagged assets:   {}", tags.tagged_assets);
        println!("  Untagged assets: {}", tags.untagged_assets);
        if !tags.top_tags.is_empty() {
            println!("\n  Top Tags");
            for t in &tags.top_tags {
                println!("    {:<20} {:>4}", t.tag, t.count);
            }
        }
    }

    if let Some(v) = &stats.verified {
        println!("\nVerification");
        println!("  Total locations:    {}", v.total_locations);
        println!("  Verified:           {}", v.verified_locations);
        println!("  Unverified:         {}", v.unverified_locations);
        println!("  Coverage:           {:.1}%", v.coverage_pct);
        if let Some(oldest) = &v.oldest_verified_at {
            println!("  Oldest verified:    {oldest}");
        }
        if let Some(newest) = &v.newest_verified_at {
            println!("  Newest verified:    {newest}");
        }
        if !v.per_volume.is_empty() {
            println!("\n  Per Volume");
            for pv in &v.per_volume {
                let status = if pv.is_online { "online" } else { "offline" };
                let purpose_tag = pv.purpose.as_ref().map(|p| format!(" [{}]", p)).unwrap_or_default();
                println!(
                    "    {} [{}]{}: {}/{} ({:.1}%)",
                    pv.label, status, purpose_tag, pv.verified, pv.locations, pv.coverage_pct
                );
            }
        }
    }
}

/// Render a status report as human-readable text.
///
/// Sections roll up from `StatusReport`'s nested structs in this order:
/// Catalog overview → Cleanup needs → Pending work → Backup coverage →
/// Volumes. Each item is prefixed `✓` (clean / ok) or `✗` (action item)
/// with a one-line `→ command` suggestion on every `✗` so the user knows
/// what to run next without consulting docs.
pub fn print_status_human(report: &maki::status::StatusReport) {
    use maki::cli_output::format_size;

    println!("MAKI catalog status — {}", report.catalog_root);

    // ── Catalog overview ─────────────────────────────────
    println!("\nCatalog");
    let schema = if report.catalog.schema_version == report.catalog.schema_current {
        format!("v{} (current)", report.catalog.schema_version)
    } else {
        format!(
            "v{} (run `maki migrate` — current is v{})",
            report.catalog.schema_version, report.catalog.schema_current
        )
    };
    println!("  Schema:   {schema}");
    println!(
        "  Counts:   {} assets · {} variants · {} recipes · {} file locations",
        report.catalog.assets,
        report.catalog.variants,
        report.catalog.recipes,
        report.catalog.file_locations,
    );
    let online = report.volumes.iter().filter(|v| v.is_online).count();
    let offline = report.volumes.len() - online;
    println!(
        "  Storage:  {} across {} volume(s) ({} online, {} offline)",
        format_size(report.catalog.total_bytes),
        report.volumes.len(),
        online,
        offline,
    );

    // ── Cleanup needs ────────────────────────────────────
    println!("\nCleanup");
    let c = &report.cleanup;
    let cleanup_actions = [
        (c.locationless_variants, "locationless variant(s)"),
        (c.orphaned_assets, "orphaned asset(s)"),
        (c.orphaned_previews, "orphaned preview(s) on disk"),
        (c.orphaned_smart_previews, "orphaned smart preview(s) on disk"),
        (c.orphaned_embeddings, "orphaned embedding file(s) on disk"),
        (c.orphaned_face_files, "orphaned face file(s) on disk"),
    ];
    let any_cleanup = cleanup_actions.iter().any(|(n, _)| *n > 0);
    if !any_cleanup {
        println!("  ✓ no cleanup needed");
    } else {
        for (n, label) in &cleanup_actions {
            if *n > 0 {
                // Hint sits two spaces after the message. Previously
                // padded with `{label:<42}` to attempt column-aligned
                // arrows, but the padding only worked when `n` was the
                // expected 1-3 digits; a 6-digit count blew the column
                // out and broke alignment anyway. Plain inline arrows
                // are robust to any number size.
                println!("  ✗ {n} {label}  → maki cleanup --apply");
            }
        }
    }

    // ── Pending work ─────────────────────────────────────
    println!("\nPending work");
    let p = &report.pending;
    let mut pending_lines = 0;
    if p.pending_writebacks_online > 0 {
        if p.writeback_enabled {
            println!(
                "  ✗ {} pending XMP writeback(s) on online volume(s)  → maki writeback",
                p.pending_writebacks_online
            );
        } else {
            // Auto-flush off (the safety-net default). Manual `maki
            // writeback` runs regardless of the config flag, so the hint
            // points straight at it without a config-change detour.
            println!(
                "  ✗ {} pending XMP writeback(s)  → maki writeback  (auto-flush off; this is the manual flush)",
                p.pending_writebacks_online
            );
        }
        pending_lines += 1;
    }
    if p.pending_writebacks_offline > 0 {
        println!(
            "  ✗ {} pending XMP writeback(s) on offline volume(s)  → mount the volumes, then `maki writeback`",
            p.pending_writebacks_offline
        );
        pending_lines += 1;
    }
    // Per-volume breakdown of the pending counts above. Show the
    // names whenever the catalog has more than one volume — even when
    // only one volume currently holds pending, naming it tells the
    // user which drive (online: where the queue lives; offline: which
    // drive to mount). For single-volume catalogs we skip the
    // breakdown since the top-level line already implies the only
    // volume. The list comes pre-sorted by count desc from `gather()`.
    if report.volumes.len() > 1 && !p.pending_writebacks_by_volume.is_empty() {
        for v in &p.pending_writebacks_by_volume {
            let marker = if v.is_online { "online" } else { "offline" };
            println!(
                "      └─ {:>5} on {} ({})",
                v.count, v.volume_label, marker,
            );
        }
    }
    if let Some(n) = p.assets_without_embedding {
        if n > 0 {
            println!("  ✗ {n} asset(s) without an embedding  → maki embed");
            pending_lines += 1;
        }
    }
    if let Some(n) = p.assets_without_face_scan {
        if n > 0 {
            println!("  ✗ {n} asset(s) unscanned for faces  → maki faces detect");
            pending_lines += 1;
        }
    }
    if p.missing_previews > 0 {
        println!(
            "  ✗ {} asset(s) missing previews  → maki generate-previews",
            p.missing_previews
        );
        pending_lines += 1;
    }
    if let Some(n) = p.missing_smart_previews {
        if n > 0 {
            println!(
                "  ✗ {n} asset(s) missing smart previews  → maki generate-previews --smart"
            );
            pending_lines += 1;
        }
    }
    if pending_lines == 0 {
        println!("  ✓ nothing pending");
    }

    // ── Backup coverage ──────────────────────────────────
    println!("\nBackup coverage");
    let b = &report.backup;
    if b.total_assets == 0 {
        println!("  (catalog is empty)");
    } else if b.at_risk == 0 {
        println!(
            "  ✓ all {} asset(s) have ≥{} copies",
            b.total_assets, b.min_copies
        );
    } else {
        let pct = (b.at_risk as f64 / b.total_assets as f64) * 100.0;
        println!(
            "  ✗ {} of {} asset(s) ({:.1}%) have fewer than {} copies  → maki backup-status --at-risk",
            b.at_risk, b.total_assets, pct, b.min_copies
        );
    }

    // ── Volumes ──────────────────────────────────────────
    if !report.volumes.is_empty() {
        println!("\nVolumes");
        for v in &report.volumes {
            let dot = if v.is_online { "●" } else { "○" };
            let purpose = v
                .purpose
                .as_deref()
                .map(|p| format!(" [{p}]"))
                .unwrap_or_default();
            let status = if v.is_online { "" } else { " (offline)" };
            println!(
                "  {} {:<18} {:<28} {} asset(s), {}{}{}",
                dot,
                v.label,
                v.mount_point,
                v.asset_count,
                format_size(v.size_bytes),
                purpose,
                status,
            );
        }
    }
}

pub fn print_backup_status_human(result: &maki::catalog::BackupStatusResult) {
    println!("Backup Status ({})", result.scope);
    println!("{}", "=".repeat(40));
    println!();
    println!("Total assets:          {:>8}", result.total_assets);
    println!("Total variants:        {:>8}", result.total_variants);
    println!("Total file locations:  {:>8}", result.total_file_locations);

    if !result.purpose_coverage.is_empty() {
        println!();
        println!("Coverage by volume purpose:");
        for pc in &result.purpose_coverage {
            // Capitalize first letter for display
            let display_purpose = {
                let mut chars = pc.purpose.chars();
                match chars.next() {
                    None => String::new(),
                    Some(c) => c.to_uppercase().to_string() + chars.as_str(),
                }
            };
            println!(
                "  {:<10} ({} volume{}):  {:>6} assets ({:.1}%)",
                display_purpose,
                pc.volume_count,
                if pc.volume_count == 1 { "" } else { "s" },
                pc.asset_count,
                pc.asset_percentage,
            );
        }
    }

    println!();
    println!("Volume distribution:");
    for bucket in &result.location_distribution {
        if bucket.asset_count == 0 {
            continue;
        }
        let label = match bucket.volume_count.as_str() {
            "0" => "0 volumes (orphaned):",
            "1" => "1 volume only:",
            "2" => "2 volumes:",
            _ => "3+ volumes:",
        };
        let at_risk = if bucket.volume_count == "0" || bucket.volume_count == "1" {
            "  <- AT RISK"
        } else {
            ""
        };
        println!("  {:<26} {:>6} assets{}", label, bucket.asset_count, at_risk);
    }

    if result.at_risk_count > 0 {
        println!();
        println!(
            "At-risk assets ({} on fewer than {} volume{}):",
            result.at_risk_count,
            result.min_copies,
            if result.min_copies == 1 { "" } else { "s" },
        );
        println!("  Use 'maki backup-status --at-risk' to list them");
        println!("  Use 'maki backup-status --at-risk -q' for asset IDs (pipeable)");
    } else {
        println!();
        println!(
            "All assets exist on {} or more volume{}. No at-risk assets.",
            result.min_copies,
            if result.min_copies == 1 { "" } else { "s" },
        );
    }

    if let Some(ref detail) = result.volume_detail {
        println!();
        let purpose_tag = detail.purpose.as_ref().map(|p| format!(" [{}]", p)).unwrap_or_default();
        println!("Volume detail: {}{}", detail.volume_label, purpose_tag);
        println!("  Present: {} / {} ({:.1}%)", detail.present_count, detail.total_scoped, detail.coverage_pct);
        println!("  Missing: {}", detail.missing_count);
    }

    if !result.volume_gaps.is_empty() {
        println!();
        println!("Volume gaps:");
        for gap in &result.volume_gaps {
            let purpose_tag = gap.purpose.as_ref().map(|p| format!(" [{}]", p)).unwrap_or_default();
            println!("  {}{}:  missing {} assets", gap.volume_label, purpose_tag, gap.missing_count);
        }
    }
}

/// Extracted body of `Commands::BackupStatus`. See `run_import_command` for the
/// extraction pattern.
pub fn run_backup_status_command(
        query: Option<String>,
        at_risk: bool,
        min_copies: u64,
        volume: Option<String>,
        format: Option<String>,
        quiet: bool,
        json: bool,
        log: bool,
        #[allow(unused_variables)] verbosity: maki::Verbosity,
) -> anyhow::Result<()> {
    #[allow(dead_code)]
    struct Ctx { json: bool, log: bool }
    let cli = Ctx { json, log };
    use maki::format::{self, OutputFormat};

    let catalog_root = maki::config::find_catalog_root()?;
    let catalog = Catalog::open(&catalog_root)?;
    let registry = DeviceRegistry::new(&catalog_root);
    let vol_list = registry.list()?;

    // Exclude media volumes from backup coverage (transient sources like memory cards)
    let volumes_info: Vec<(String, String, bool, Option<String>)> = vol_list
        .iter()
        .filter(|v| v.purpose.as_ref() != Some(&maki::models::VolumePurpose::Media))
        .map(|v| (v.label.clone(), v.id.to_string(), v.is_online, v.purpose.as_ref().map(|p| p.as_str().to_string())))
        .collect();

    // Resolve target volume if specified
    let target_volume = if let Some(ref vol_label) = volume {
        Some(registry.resolve_volume(vol_label)?)
    } else {
        None
    };
    let target_volume_id = target_volume.as_ref().map(|v| v.id.to_string());

    // Scope: optional query → asset IDs
    let scope_ids: Option<Vec<String>> = if let Some(ref q) = query {
        let engine = QueryEngine::new(&catalog_root);
        let results = engine.search(q)?;
        let ids: Vec<String> = results.iter().map(|r| r.asset_id.clone()).collect();
        Some(ids)
    } else {
        None
    };
    let scope_refs = scope_ids.as_deref();

    // Determine mode: at-risk listing vs overview
    let listing_mode = at_risk || quiet || format.is_some();

    if listing_mode {
        // Get at-risk IDs
        let risk_ids = if let Some(ref tvid) = target_volume_id {
            catalog.backup_status_missing_from_volume(scope_refs, tvid)?
        } else {
            catalog.backup_status_at_risk_ids(scope_refs, min_copies)?
        };

        // Fetch full SearchRow data for output formatting
        let results = if risk_ids.is_empty() {
            Vec::new()
        } else {
            let opts = maki::catalog::SearchOptions {
                collection_asset_ids: Some(&risk_ids),
                per_page: u32::MAX,
                ..Default::default()
            };
            catalog.search_paginated(&opts)?
        };

        let output_format = if quiet {
            OutputFormat::Ids
        } else if let Some(fmt) = &format {
            format::parse_format(fmt).map_err(|e| anyhow::anyhow!(e))?
        } else if cli.json {
            OutputFormat::Json
        } else {
            OutputFormat::Short
        };

        let explicit_format = quiet || format.is_some();

        if results.is_empty() {
            match output_format {
                OutputFormat::Json => println!("[]"),
                _ => {
                    if !explicit_format {
                        println!("No at-risk assets found.");
                    }
                }
            }
        } else {
            match output_format {
                OutputFormat::Ids => {
                    for row in &results {
                        println!("{}", row.asset_id);
                    }
                }
                OutputFormat::Short => {
                    for row in &results {
                        let display_name = row.name.as_deref().unwrap_or(&row.original_filename);
                        let short_id = &row.asset_id[..8];
                        println!(
                            "{}  {} [{}] ({}) — {}",
                            short_id, display_name, row.asset_type, row.display_format(), row.created_at
                        );
                    }
                    if !explicit_format {
                        println!("\n{} at-risk asset(s)", results.len());
                    }
                }
                OutputFormat::Full => {
                    for row in &results {
                        let display_name = row.name.as_deref().unwrap_or(&row.original_filename);
                        let short_id = &row.asset_id[..8];
                        let tags = if row.tags.is_empty() {
                            String::new()
                        } else {
                            format!(" tags:{}", row.tags.join(","))
                        };
                        let desc = row.description.as_deref().unwrap_or("");
                        println!(
                            "{}  {} [{}] ({}) — {}{} {}",
                            short_id, display_name, row.asset_type, row.display_format(),
                            row.created_at, tags, desc
                        );
                    }
                    if !explicit_format {
                        println!("\n{} at-risk asset(s)", results.len());
                    }
                }
                OutputFormat::Json => {
                    println!("{}", serde_json::to_string_pretty(&results)?);
                }
                OutputFormat::Template(ref tpl) => {
                    for row in &results {
                        let tags_str = row.tags.join(", ");
                        let desc = row.description.as_deref().unwrap_or("");
                        let label = row.color_label.as_deref().unwrap_or("");
                        let values = format::search_row_values(
                            &row.asset_id,
                            row.name.as_deref(),
                            &row.original_filename,
                            &row.asset_type,
                            row.display_format(),
                            &row.created_at,
                            &tags_str,
                            desc,
                            &row.content_hash,
                            label,
                        );
                        println!("{}", format::render_template(tpl, &values));
                    }
                }
            }
        }
    } else {
        // Overview mode
        let result = catalog.backup_status_overview(
            scope_refs,
            &volumes_info,
            min_copies,
            target_volume_id.as_deref(),
        )?;

        if cli.json {
            println!("{}", serde_json::to_string_pretty(&result)?);
        } else {
            print_backup_status_human(&result);
        }
    }
    Ok(())
}

/// Extracted body of `Commands::Status`. See `run_import_command` for the
/// extraction pattern.
pub fn run_status_command(
        min_copies: u64,
        json: bool,
        log: bool,
        #[allow(unused_variables)] verbosity: maki::Verbosity,
) -> anyhow::Result<()> {
    #[allow(dead_code)]
    struct Ctx { json: bool, log: bool }
    let cli = Ctx { json, log };
    let (catalog_root, config) = maki::config::load_config()?;
    let ai_enabled = cfg!(feature = "ai");
    // The orphan-on-disk scan (cleanup passes 4-7) dominates runtime
    // on real catalogs — easily 30s+ on tens of thousands of files.
    // Emit a one-line "still alive" marker to stderr so the user
    // doesn't wonder whether the command crashed. Suppressed under
    // --json so scripted output stays clean.
    if !cli.json {
        eprintln!("Gathering catalog status (scanning derived files; may take a moment)...");
    }
    let report = maki::status::gather(
        &catalog_root,
        verbosity,
        &config.preview,
        min_copies,
        ai_enabled,
    )?;
    if cli.json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        print_status_human(&report);
    }
    Ok(())
}

/// Extracted body of `Commands::Stats`. See `run_import_command` for the
/// extraction pattern.
pub fn run_stats_command(
        types: bool,
        volumes: bool,
        tags: bool,
        verified: bool,
        all: bool,
        limit: usize,
        json: bool,
        log: bool,
        #[allow(unused_variables)] verbosity: maki::Verbosity,
) -> anyhow::Result<()> {
    #[allow(dead_code)]
    struct Ctx { json: bool, log: bool }
    let cli = Ctx { json, log };
    let catalog_root = maki::config::find_catalog_root()?;
    let catalog = Catalog::open(&catalog_root)?;
    let registry = DeviceRegistry::new(&catalog_root);
    let vol_list = registry.list()?;

    let volumes_info: Vec<(String, String, bool, Option<String>)> = vol_list
        .iter()
        .map(|v| (v.label.clone(), v.id.to_string(), v.is_online, v.purpose.as_ref().map(|p| p.as_str().to_string())))
        .collect();

    let show_types = types || all;
    let show_volumes = volumes || all;
    let show_tags = tags || all;
    let show_verified = verified || all;

    let stats = catalog.build_stats(
        &volumes_info,
        show_types,
        show_volumes,
        show_tags,
        show_verified,
        limit,
    )?;

    if cli.json {
        println!("{}", serde_json::to_string_pretty(&stats)?);
    } else {
        print_stats_human(&stats);
    }
    Ok(())
}
