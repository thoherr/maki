//! Output commands: `export`, `contact-sheet`, `duplicates`, `dedup`.

use super::*;

/// Extracted body of `Commands::Export`. See `run_import_command` for the
/// extraction pattern.
pub fn run_export_command(
        query: String,
        target: String,
        layout: String,
        symlink: bool,
        all_variants: bool,
        include_sidecars: bool,
        dry_run: bool,
        overwrite: bool,
        zip: bool,
        json: bool,
        log: bool,
        #[allow(unused_variables)] verbosity: maki::Verbosity,
) -> anyhow::Result<()> {
    #[allow(dead_code)]
    struct Ctx { json: bool, log: bool }
    let cli = Ctx { json, log };
    use maki::asset_service::{ExportLayout, ExportStatus};

    let export_layout = match layout.as_str() {
        "flat" => ExportLayout::Flat,
        "mirror" => ExportLayout::Mirror,
        _ => anyhow::bail!("unknown layout '{}'. Valid layouts: flat, mirror", layout),
    };

    let (catalog_root, config) = maki::config::load_config()?;
    let service = AssetService::new(&catalog_root, verbosity, &config.preview);

    let show_log = cli.log;
    let log_callback = |path: &std::path::Path, status: &ExportStatus, elapsed: std::time::Duration| {
        if show_log {
            let name = path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy();
            match status {
                ExportStatus::Copied => {
                    item_status(&name, "added", Some(elapsed));
                }
                ExportStatus::Linked => {
                    item_status(&name, "linked", Some(elapsed));
                }
                ExportStatus::Skipped => {
                    item_status(&name, "skipped", Some(elapsed));
                }
                ExportStatus::Error(msg) => {
                    eprintln!("  {name} — error: {msg}");
                }
            }
        }
    };

    let result = if zip {
        if symlink {
            anyhow::bail!("--symlink cannot be used with --zip");
        }
        let zip_path = PathBuf::from(&target);
        // Append .zip if not already present
        let zip_path = if zip_path.extension().and_then(|e| e.to_str()) != Some("zip") {
            zip_path.with_extension("zip")
        } else {
            zip_path
        };
        service.export_zip(
            &query,
            &zip_path,
            export_layout,
            all_variants,
            include_sidecars,
            log_callback,
        )?
    } else {
        let target_path = PathBuf::from(&target);
        if !dry_run {
            std::fs::create_dir_all(&target_path)?;
        }
        service.export(
            &query,
            &target_path,
            export_layout,
            symlink,
            all_variants,
            include_sidecars,
            dry_run,
            overwrite,
            log_callback,
        )?
    };

    if cli.json {
        println!("{}", serde_json::to_string_pretty(&result)?);
    } else {
        for err in &result.errors {
            eprintln!("  {err}");
        }

        if dry_run {
            println!("Export (dry run): {} assets matched, {} files would be exported",
                result.assets_matched, result.files_exported);
            if result.sidecars_exported > 0 {
                println!("  {} sidecars would be exported", result.sidecars_exported);
            }
            if result.total_bytes > 0 {
                println!("  Total size: {}", format_size(result.total_bytes));
            }
        } else if result.assets_matched == 0 {
            println!("No assets matched the query.");
        } else if zip {
            let mut parts = vec![
                format!("{} files archived", result.files_exported),
            ];
            if result.sidecars_exported > 0 {
                parts.push(format!("{} sidecars", result.sidecars_exported));
            }
            println!("Export complete: {}", parts.join(", "));
            if result.total_bytes > 0 {
                println!("  Total size: {}", format_size(result.total_bytes));
            }
            println!("  Written to: {target}");
        } else {
            let verb = if symlink { "linked" } else { "copied" };
            let mut parts = vec![
                format!("{} files {verb}", result.files_exported),
            ];
            if result.sidecars_exported > 0 {
                parts.push(format!("{} sidecars", result.sidecars_exported));
            }
            if result.files_skipped > 0 {
                parts.push(format!("{} skipped", result.files_skipped));
            }
            println!("Export complete: {}", parts.join(", "));
            if result.total_bytes > 0 {
                println!("  Total size: {}", format_size(result.total_bytes));
            }
        }
    }

    Ok(())
}

/// Extracted body of `Commands::ContactSheet`. See `run_import_command` for the
/// extraction pattern.
pub fn run_contact_sheet_command(
        query: String,
        output: String,
        layout: String,
        columns: Option<u32>,
        rows: Option<u32>,
        paper: String,
        landscape: bool,
        title: Option<String>,
        fields: Option<String>,
        sort: Option<String>,
        no_smart: bool,
        group_by: Option<String>,
        margin: Option<f32>,
        label_style: Option<String>,
        quality: Option<u8>,
        copyright: Option<String>,
        dry_run: bool,
        json: bool,
        log: bool,
        #[allow(unused_variables)] verbosity: maki::Verbosity,
) -> anyhow::Result<()> {
    #[allow(dead_code)]
    struct Ctx { json: bool, log: bool }
    let cli = Ctx { json, log };
    use maki::contact_sheet::{
        generate_contact_sheet, ContactSheetConfig, ContactSheetLayout,
        ContactSheetStatus, GroupByField, LabelStyle, MetadataField, PaperSize,
    };

    let (catalog_root, config) = maki::config::load_config()?;
    let cs_defaults = &config.contact_sheet;

    let cs_layout: ContactSheetLayout = layout.parse()?;
    let cs_paper: PaperSize = paper.parse()?;

    let cs_fields = if let Some(ref f) = fields {
        let parsed: Vec<MetadataField> = f
            .split(',')
            .map(|s| s.trim().parse::<MetadataField>())
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Some(parsed)
    } else if cs_defaults.fields != "filename,date,rating" {
        let parsed: Vec<MetadataField> = cs_defaults
            .fields
            .split(',')
            .map(|s| s.trim().parse::<MetadataField>())
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Some(parsed)
    } else {
        None // Use layout preset default
    };

    let cs_group_by = group_by
        .map(|g| g.parse::<GroupByField>())
        .transpose()?;

    let cs_label_style: LabelStyle = label_style
        .unwrap_or_else(|| cs_defaults.label_style.clone())
        .parse()?;

    let cs_quality = quality.unwrap_or(cs_defaults.quality);
    // CLI --margin is f32; config carries f64 since v4.5.5. Cast at the boundary;
    // ContactSheetConfig.margin_mm downstream is f32.
    let cs_margin = margin.unwrap_or(cs_defaults.margin as f32);

    let cs_config = ContactSheetConfig {
        layout: cs_layout,
        columns,
        rows,
        paper: cs_paper,
        landscape,
        title,
        fields: cs_fields,
        sort,
        use_smart_previews: !no_smart,
        group_by: cs_group_by,
        margin_mm: cs_margin,
        label_style: cs_label_style,
        quality: cs_quality,
        copyright: copyright.or_else(|| cs_defaults.copyright.clone()),
    };

    let output_path = PathBuf::from(&output);
    let show_log = cli.log;

    let result = generate_contact_sheet(
        &catalog_root,
        &query,
        &output_path,
        &cs_config,
        dry_run,
        |msg, status, elapsed| {
            if show_log || matches!(status, ContactSheetStatus::Complete) {
                match status {
                    ContactSheetStatus::Rendering => {
                        eprintln!("  {} ({})", msg, format_duration(elapsed));
                    }
                    ContactSheetStatus::Complete => {
                        if !cli.json {
                            eprintln!("{}", msg);
                        }
                    }
                    ContactSheetStatus::Error => {
                        eprintln!("  Error: {}", msg);
                    }
                }
            }
        },
    )?;

    if cli.json {
        println!("{}", serde_json::to_string_pretty(&result)?);
    } else if !dry_run {
        println!(
            "Contact sheet: {} assets, {} pages → {}",
            result.assets, result.pages, result.output,
        );
    }

    Ok(())
}

/// Extracted body of `Commands::Duplicates`. See `run_import_command` for the
/// extraction pattern.
pub fn run_duplicates_command(
        format: Option<String>,
        same_volume: bool,
        cross_volume: bool,
        volume: Option<String>,
        filter_format: Option<String>,
        path: Option<String>,
        json: bool,
        log: bool,
        #[allow(unused_variables)] verbosity: maki::Verbosity,
) -> anyhow::Result<()> {
    #[allow(dead_code)]
    struct Ctx { json: bool, log: bool }
    let cli = Ctx { json, log };
    use maki::format::{self, OutputFormat};

    if same_volume && cross_volume {
        anyhow::bail!("--same-volume and --cross-volume are mutually exclusive");
    }

    let catalog_root = maki::config::find_catalog_root()?;
    let catalog = Catalog::open(&catalog_root)?;

    // Resolve volume label → ID for the SQL filter (unknown volume → empty results)
    let vol_id = if let Some(ref label) = volume {
        let registry = DeviceRegistry::new(&catalog_root);
        match registry.resolve_volume(label) {
            Ok(v) => Some(v.id.to_string()),
            Err(_) => Some("nonexistent".to_string()),
        }
    } else {
        None
    };

    let mode = if same_volume { "same" } else if cross_volume { "cross" } else { "all" };
    let has_filters = vol_id.is_some() || filter_format.is_some() || path.is_some();

    let entries = if has_filters {
        catalog.find_duplicates_filtered(
            mode,
            vol_id.as_deref(),
            filter_format.as_deref(),
            path.as_deref(),
        )?
    } else if same_volume {
        catalog.find_duplicates_same_volume()?
    } else if cross_volume {
        catalog.find_duplicates_cross_volume()?
    } else {
        catalog.find_duplicates()?
    };

    let explicit_format = format.is_some();

    let output_format = if let Some(fmt) = &format {
        format::parse_format(fmt).map_err(|e| anyhow::anyhow!(e))?
    } else if cli.json {
        OutputFormat::Json
    } else {
        OutputFormat::Short
    };

    if entries.is_empty() {
        match output_format {
            OutputFormat::Json => println!("[]"),
            _ => {
                if !explicit_format {
                    if same_volume {
                        println!("No same-volume duplicates found.");
                    } else if cross_volume {
                        println!("No cross-volume copies found.");
                    } else {
                        println!("No duplicates found.");
                    }
                }
            }
        }
    } else {
        match output_format {
            OutputFormat::Ids => {
                for entry in &entries {
                    println!("{}", entry.content_hash);
                }
            }
            OutputFormat::Short | OutputFormat::Full => {
                let is_full = matches!(output_format, OutputFormat::Full);
                for entry in &entries {
                    let display_name = entry
                        .asset_name
                        .as_deref()
                        .unwrap_or(&entry.original_filename);
                    let vol_info = if entry.volume_count > 1 {
                        format!(" [{} volumes]", entry.volume_count)
                    } else {
                        String::new()
                    };
                    println!(
                        "{} ({}, {}){}",
                        display_name,
                        entry.format,
                        format_size(entry.file_size),
                        vol_info,
                    );
                    println!("  Hash: {}", entry.content_hash);
                    for loc in &entry.locations {
                        let purpose = loc
                            .volume_purpose
                            .as_deref()
                            .map(|p| format!(" [{}]", p))
                            .unwrap_or_default();
                        if is_full {
                            let verified = loc
                                .verified_at
                                .as_deref()
                                .unwrap_or("never");
                            println!(
                                "    {}{} \u{2192} {} (verified: {})",
                                loc.volume_label, purpose, loc.relative_path, verified
                            );
                        } else {
                            println!(
                                "    {}{} \u{2192} {}",
                                loc.volume_label, purpose, loc.relative_path
                            );
                        }
                    }
                    if !entry.same_volume_groups.is_empty() {
                        println!(
                            "  \u{26a0} same-volume duplicates on: {}",
                            entry.same_volume_groups.join(", ")
                        );
                    }
                }
                if !explicit_format {
                    let label = if same_volume {
                        "same-volume duplicate(s)"
                    } else if cross_volume {
                        "cross-volume copie(s)"
                    } else {
                        "file(s) with duplicate locations"
                    };
                    println!(
                        "\n{} {}",
                        entries.len(),
                        label,
                    );
                }
            }
            OutputFormat::Json => {
                println!("{}", serde_json::to_string_pretty(&entries)?);
            }
            OutputFormat::Template(ref tpl) => {
                for entry in &entries {
                    let mut values = std::collections::HashMap::new();
                    values.insert("hash", entry.content_hash.clone());
                    values.insert("filename", entry.original_filename.clone());
                    values.insert("format", entry.format.clone());
                    values.insert("size", format_size(entry.file_size));
                    values.insert("name", entry.asset_name.as_deref()
                        .unwrap_or(&entry.original_filename).to_string());
                    let locs: Vec<String> = entry.locations.iter()
                        .map(|l| {
                            let purpose = l.volume_purpose.as_deref()
                                .map(|p| format!("[{}]", p))
                                .unwrap_or_default();
                            format!("{}{}:{}", l.volume_label, purpose, l.relative_path)
                        })
                        .collect();
                    values.insert("locations", locs.join(", "));
                    values.insert("volumes", entry.volume_count.to_string());
                    println!("{}", format::render_template(tpl, &values));
                }
            }
        }
    }
    Ok(())
}

/// Extracted body of `Commands::Dedup`. See `run_import_command` for the
/// extraction pattern.
pub fn run_dedup_command(
        volume: Option<String>,
        prefer: Option<String>,
        filter_format: Option<String>,
        path: Option<String>,
        min_copies: usize,
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

    // CLI --prefer overrides config [dedup] prefer
    let effective_prefer = prefer.or(config.dedup.prefer);

    let show_log = cli.log;
    let result = if show_log {
        use maki::asset_service::DedupStatus;
        service.dedup(
            volume.as_deref(),
            filter_format.as_deref(),
            path.as_deref(),
            effective_prefer.as_deref(),
            min_copies,
            apply,
            |filename, path, status, vol_label| {
                match status {
                    DedupStatus::Keep => {
                        eprintln!("  {} — keep ({}, {})", filename, path, vol_label);
                    }
                    DedupStatus::Remove => {
                        eprintln!("  {} — remove ({}, {})", filename, path, vol_label);
                    }
                    DedupStatus::Skipped => {
                        eprintln!("  {} — skipped, min-copies ({}, {})", filename, path, vol_label);
                    }
                }
            },
        )?
    } else {
        service.dedup(
            volume.as_deref(),
            filter_format.as_deref(),
            path.as_deref(),
            effective_prefer.as_deref(),
            min_copies,
            apply,
            |_, _, _, _| {},
        )?
    };

    if cli.json {
        println!("{}", serde_json::to_string_pretty(&result)?);
    } else {
        for err in &result.errors {
            eprintln!("  {err}");
        }

        if apply {
            let recipe_msg = if result.recipes_removed > 0 {
                format!(", {} recipes removed", result.recipes_removed)
            } else {
                String::new()
            };
            println!(
                "Dedup: {} duplicate groups, {} locations removed, {} files deleted{} ({})",
                result.duplicates_found,
                result.locations_removed,
                result.files_deleted,
                recipe_msg,
                format_size(result.bytes_freed),
            );
            // Same trap as sync: removing redundant locations leaves
            // variants that no longer have any locations. They linger
            // — sometimes as the asset's selected best-preview variant
            // — until `cleanup --apply` removes them.
            if result.locations_removed > 0 {
                if let Ok(catalog) = maki::catalog::Catalog::open(&catalog_root) {
                    if let Ok(locationless) = catalog.list_locationless_variants() {
                        if !locationless.is_empty() {
                            println!(
                                "  Tip: {} variant(s) have no remaining locations. \
                                 Run 'maki cleanup --apply' to remove them and their \
                                 orphaned previews/embeddings/face files.",
                                locationless.len()
                            );
                        }
                    }
                }
            }
        } else {
            let recipe_msg = if result.recipes_removed > 0 {
                format!(", {} recipe files", result.recipes_removed)
            } else {
                String::new()
            };
            println!(
                "Dedup: {} duplicate groups, {} redundant locations{} ({} reclaimable)",
                result.duplicates_found,
                result.locations_to_remove,
                recipe_msg,
                format_size(result.bytes_freed),
            );
            if result.locations_to_remove > 0 {
                println!("  Run with --apply to remove redundant files.");
            }
        }
    }

    Ok(())
}
