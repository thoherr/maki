//! Misc commands: `serve`, `doc`, `licenses`, `preview`, `show`.

use super::*;

/// Extracted body of `Commands::Serve`. See `run_import_command` for the
/// extraction pattern.
pub fn run_serve_command(
        port: Option<u16>,
        bind: Option<String>,
        read_only: bool,
        json: bool,
        log: bool,
        #[allow(unused_variables)] verbosity: maki::Verbosity,
) -> anyhow::Result<()> {
    #[allow(dead_code)]
    struct Ctx { json: bool, log: bool }
    let cli = Ctx { json, log };
    let (catalog_root, config) = maki::config::load_config()?;
    let port = port.unwrap_or(config.serve.port);
    let bind = bind.unwrap_or_else(|| config.serve.bind.clone());
    let rt = tokio::runtime::Runtime::new()?;
    #[cfg(feature = "ai")]
    rt.block_on(maki::web::serve(catalog_root, &bind, port, config.preview, cli.log, config.dedup.prefer, config.serve.per_page, config.serve.stroll_neighbors, config.serve.stroll_neighbors_max, config.serve.stroll_fanout, config.serve.stroll_fanout_max, config.serve.stroll_discover_pool, read_only, config.ai, config.vlm, config.browse.default_filter, verbosity))?;
    #[cfg(not(feature = "ai"))]
    rt.block_on(maki::web::serve(catalog_root, &bind, port, config.preview, cli.log, config.dedup.prefer, config.serve.per_page, config.serve.stroll_neighbors, config.serve.stroll_neighbors_max, config.serve.stroll_fanout, config.serve.stroll_fanout_max, config.serve.stroll_discover_pool, read_only, config.vlm, config.browse.default_filter, verbosity))?;
    Ok(())
}

/// Extracted body of `Commands::Doc`. See `run_import_command` for the
/// extraction pattern.
pub fn run_doc_command(
        document: String,
        json: bool,
        log: bool,
        #[allow(unused_variables)] verbosity: maki::Verbosity,
) -> anyhow::Result<()> {
    #[allow(dead_code)]
    struct Ctx { json: bool, log: bool }
    let cli = Ctx { json, log };
    let url = match document.to_lowercase().as_str() {
        "manual" | "man" | "guide" => "https://github.com/thoherr/maki/releases/latest/download/maki-manual.pdf",
        "cheatsheet" | "cheat" | "cs" => "https://github.com/thoherr/maki/releases/latest/download/cheat-sheet.pdf",
        "filters" | "search" | "filter" | "sf" => "https://github.com/thoherr/maki/releases/latest/download/search-filters.pdf",
        "tagging" | "tags" | "tag" => "https://github.com/thoherr/maki/releases/latest/download/tagging.pdf",
        _ => {
            anyhow::bail!("unknown document '{}'. Available: manual, cheatsheet, filters, tagging", document);
        }
    };
    if cli.json {
        println!("{}", serde_json::json!({ "url": url }));
    } else {
        println!("Opening {url}");
        #[cfg(target_os = "macos")]
        { let _ = std::process::Command::new("open").arg(url).spawn(); }
        #[cfg(target_os = "linux")]
        { let _ = std::process::Command::new("xdg-open").arg(url).spawn(); }
        #[cfg(target_os = "windows")]
        { let _ = std::process::Command::new("cmd").args(["/c", "start", url]).spawn(); }
    }
    Ok(())
}

/// Extracted body of `Commands::Licenses`. See `run_import_command` for the
/// extraction pattern.
pub fn run_licenses_command(
        summary: bool,
        json: bool,
        log: bool,
        #[allow(unused_variables)] verbosity: maki::Verbosity,
) -> anyhow::Result<()> {
    #[allow(dead_code)]
    struct Ctx { json: bool, log: bool }
    let cli = Ctx { json, log };
    let third_party_url = "https://github.com/thoherr/maki/releases/latest/download/THIRD_PARTY_LICENSES.md";

    if cli.json {
        println!("{}", serde_json::json!({
            "maki_license": "Apache-2.0",
            "maki_license_url": "https://github.com/thoherr/maki/blob/main/LICENSE",
            "third_party_licenses_url": third_party_url,
            "third_party_summary": "All Rust dependencies use permissive licenses (MIT, Apache-2.0, BSD, ISC, MPL-2.0, NCSA, Unicode, Zlib, BSL-1.0, CC0). See THIRD_PARTY_LICENSES.md in the release archive.",
            "ai_models": {
                "license": "Apache-2.0",
                "source": "Hugging Face (downloaded on demand)",
                "credit": "Google Research (SigLIP, SigLIP 2)",
            },
            "external_tools": "dcraw, libraw, ffmpeg, curl — installed separately by the user under their own licenses; not bundled.",
        }));
        return Ok(());
    }

    println!("MAKI — Media Asset Keeper & Indexer");
    println!("License: Apache-2.0");
    println!("https://github.com/thoherr/maki/blob/main/LICENSE");
    println!();
    println!("Third-party Rust dependencies");
    println!("─────────────────────────────");
    println!("All compiled-in Rust crates use permissive open-source licenses:");
    println!("  Apache-2.0, MIT, BSD-2/3-Clause, ISC, MPL-2.0, NCSA, Unicode, Zlib,");
    println!("  BSL-1.0, CC0, 0BSD.");
    println!();
    println!("The full license text for every dependency is in:");
    println!("  THIRD_PARTY_LICENSES.md (in your MAKI release archive)");
    println!("  {third_party_url}");
    println!();
    println!("AI models (Pro)");
    println!("───────────────");
    println!("SigLIP and SigLIP 2 image-text encoders are downloaded on demand from");
    println!("Hugging Face. Both are released by Google Research under Apache-2.0.");
    println!("Face detection/recognition models (when used) are also Apache-2.0.");
    println!("MAKI does not bundle model weights in the binary.");
    println!();
    println!("External tools");
    println!("──────────────");
    println!("dcraw, libraw, ffmpeg, ffprobe, and curl are called as separate processes");
    println!("when present on the system. They are governed by their own licenses and");
    println!("installed by the user; MAKI does not bundle their code.");

    if !summary {
        println!();
        println!("Run 'maki licenses --summary' for a shorter version, or open the");
        println!("THIRD_PARTY_LICENSES.md file in your release archive for the full");
        println!("text of every dependency's license.");
    }

    Ok(())
}

/// Extracted body of `Commands::Preview`. See `run_import_command` for the
/// extraction pattern.
pub fn run_preview_command(
        asset_id: String,
        json: bool,
        log: bool,
        #[allow(unused_variables)] verbosity: maki::Verbosity,
) -> anyhow::Result<()> {
    #[allow(dead_code)]
    struct Ctx { json: bool, log: bool }
    let cli = Ctx { json, log };
    let (catalog_root, config) = maki::config::load_config()?;
    let catalog = maki::catalog::Catalog::open(&catalog_root)?;
    let engine = QueryEngine::new(&catalog_root);
    let details = engine.show(&asset_id)?;
    let full_id = &details.id;
    let preview_gen = maki::preview::PreviewGenerator::new(&catalog_root, verbosity, &config.preview);

    // Find best preview file (smart preview > regular preview)
    let best_hash = catalog.get_asset_best_variant_hash(full_id)
        .unwrap_or(None)
        .or_else(|| {
            maki::models::variant::best_preview_index_details(&details.variants)
                .map(|i| details.variants[i].content_hash.clone())
        });

    let preview_path = best_hash.as_ref().and_then(|h| {
        let smart = preview_gen.smart_preview_path(h);
        if smart.exists() { return Some(smart); }
        let regular = preview_gen.preview_path(h);
        if regular.exists() { return Some(regular); }
        None
    });

    match preview_path {
        Some(path) => {
            maki::preview::open_in_viewer(&path)?;
            if !cli.json {
                let name = details.name.as_deref().unwrap_or(full_id);
                eprintln!("Opened preview for {name}");
            }
            if cli.json {
                println!("{}", serde_json::json!({
                    "id": full_id,
                    "preview": path.display().to_string(),
                }));
            }
        }
        None => {
            let name = details.name.as_deref().unwrap_or(full_id);
            if cli.json {
                println!("{}", serde_json::json!({
                    "id": full_id,
                    "preview": null,
                }));
            } else {
                eprintln!("No preview available for {name}");
            }
        }
    }

    Ok(())
}

/// Extracted body of `Commands::Show`. See `run_import_command` for the
/// extraction pattern.
pub fn run_show_command(
        asset_id: String,
        locations: bool,
        json: bool,
        log: bool,
        #[allow(unused_variables)] verbosity: maki::Verbosity,
) -> anyhow::Result<()> {
    #[allow(dead_code)]
    struct Ctx { json: bool, log: bool }
    let cli = Ctx { json, log };
    let (catalog_root, config) = maki::config::load_config()?;
    let engine = QueryEngine::new(&catalog_root);
    let details = engine.show(&asset_id)?;

    if locations {
        if cli.json {
            let locs: Vec<serde_json::Value> = details.variants.iter()
                .flat_map(|v| v.locations.iter().map(move |loc| {
                    serde_json::json!({
                        "volume": loc.volume_label,
                        "path": loc.relative_path,
                        "variant": v.original_filename,
                        "format": v.format,
                        "role": v.role,
                    })
                }))
                .collect();
            let recipe_locs: Vec<serde_json::Value> = details.recipes.iter()
                .filter_map(|r| {
                    let label = r.volume_label.as_deref()?;
                    let path = r.relative_path.as_deref()?;
                    Some(serde_json::json!({
                        "volume": label,
                        "path": path,
                        "variant": r.software,
                        "format": r.recipe_type,
                        "role": "recipe",
                    }))
                })
                .collect();
            let all: Vec<serde_json::Value> = locs.into_iter().chain(recipe_locs).collect();
            println!("{}", serde_json::to_string_pretty(&all)?);
        } else {
            for v in &details.variants {
                for loc in &v.locations {
                    println!("{}:{}", loc.volume_label, loc.relative_path);
                }
            }
            for r in &details.recipes {
                if let (Some(label), Some(path)) = (&r.volume_label, &r.relative_path) {
                    println!("{label}:{path}");
                }
            }
        }
        return Ok(());
    }

    if cli.json {
        println!("{}", serde_json::to_string_pretty(&details)?);
    } else {
        let preview_gen = maki::preview::PreviewGenerator::new(&catalog_root, verbosity, &config.preview);

        println!("Asset: {}", details.id);
        if let Some(name) = &details.name {
            println!("Name:  {name}");
        }
        println!("Type:  {}", details.asset_type);
        println!("Date:  {}", details.created_at);
        if !details.tags.is_empty() {
            let display_tags: Vec<String> = details.tags.iter()
                .map(|t| maki::tag_util::tag_storage_to_display(t))
                .collect();
            println!("Tags:  {}", display_tags.join(", "));
        }
        if let Some(rating) = details.rating {
            let stars: String = (1..=5).map(|i| if i <= rating { '\u{2605}' } else { '\u{2606}' }).collect();
            println!("Rating: {stars} ({rating}/5)");
        }
        if let Some(label) = &details.color_label {
            println!("Label: {label}");
        }
        if let Some(desc) = &details.description {
            println!("Description: {desc}");
        }

        // Show preview status for the best preview variant
        if let Some(idx) = maki::models::variant::best_preview_index_details(&details.variants) {
            let v = &details.variants[idx];
            let preview_path = preview_gen.preview_path(&v.content_hash);
            if preview_gen.has_preview(&v.content_hash) {
                println!("Preview: {}", preview_path.display());
            } else {
                println!("Preview: (none)");
            }
        }

        if !details.variants.is_empty() {
            println!("\nVariants:");
            for v in &details.variants {
                println!(
                    "  [{}] {} ({}, {})",
                    v.role,
                    v.original_filename,
                    v.format,
                    format_size(v.file_size)
                );
                println!("    Hash: {}", v.content_hash);
                for loc in &v.locations {
                    println!(
                        "    Location: {} \u{2192} {}",
                        loc.volume_label, loc.relative_path
                    );
                }
                if !v.source_metadata.is_empty() {
                    let mut keys: Vec<&String> = v.source_metadata.keys().collect();
                    keys.sort();
                    for key in keys {
                        println!("    {}: {}", key, v.source_metadata[key]);
                    }
                }
            }
        }

        if !details.recipes.is_empty() {
            println!("\nRecipes:");
            for r in &details.recipes {
                let short_variant = &r.variant_hash[r.variant_hash.len().saturating_sub(8)..];
                println!("  [{}] {} → …{} ({})", r.recipe_type, r.software, short_variant, r.content_hash);
                if let Some(path) = &r.relative_path {
                    let label = r.volume_label.as_deref().unwrap_or("?");
                    println!("    Location: {label}:{path}");
                }
            }
        }
    }

    Ok(())
}
