//! AI commands: `ai export-vocabulary`, `auto-tag`, `embed`, `describe`.

use super::*;

/// Dispatch `maki ai <subcommand>`.
#[cfg(feature = "ai")]
pub fn run_ai_command(cmd: AiCommands) -> anyhow::Result<()> {
    match cmd {
        AiCommands::ExportVocabulary { output, default } => {
            run_ai_export_vocabulary(output, default)
        }
    }
}

/// `maki ai export-vocabulary [--default] [--output FILE]` —
/// emit the AI vocabulary as YAML. See the `AiCommands::ExportVocabulary`
/// doc-comment for behaviour.
#[cfg(feature = "ai")]
fn run_ai_export_vocabulary(output: Option<String>, default: bool) -> anyhow::Result<()> {
    use std::io::Write;

    const BUILTIN_DEFAULT: &str = include_str!("../default-vocabulary.yaml");

    let content = if default {
        BUILTIN_DEFAULT.to_string()
    } else {
        // Try to load the user's active config. If we're outside a
        // catalog (or there's no [ai].labels), fall back to the
        // built-in default — same content the runtime would use.
        let active_path: Option<std::path::PathBuf> = maki::config::find_catalog_root()
            .ok()
            .and_then(|root| maki::config::CatalogConfig::load(&root).ok())
            .and_then(|c| c.ai.labels.map(std::path::PathBuf::from));

        match active_path {
            Some(p) => {
                let ext = p
                    .extension()
                    .and_then(|e| e.to_str())
                    .unwrap_or("")
                    .to_lowercase();
                if ext == "yaml" || ext == "yml" {
                    // Verbatim — preserves the user's comments + key order.
                    std::fs::read_to_string(&p)
                        .map_err(|e| anyhow::anyhow!("read {}: {e}", p.display()))?
                } else {
                    // Convert flat .txt to YAML with identity mappings so
                    // the user can start adding hierarchy without
                    // reordering anything.
                    convert_txt_to_yaml(&p)?
                }
            }
            None => BUILTIN_DEFAULT.to_string(),
        }
    };

    match output {
        Some(path) => {
            std::fs::write(&path, &content)
                .map_err(|e| anyhow::anyhow!("write {path}: {e}"))?;
            eprintln!("Wrote {} bytes to {path}", content.len());
        }
        None => {
            std::io::stdout().write_all(content.as_bytes())?;
        }
    }
    Ok(())
}

/// Read a flat `.txt` labels file and emit a YAML vocabulary with
/// identity mappings (`label: null`). Drops blank lines / comments
/// from the source the same way the runtime loader does.
#[cfg(feature = "ai")]
fn convert_txt_to_yaml(path: &std::path::Path) -> anyhow::Result<String> {
    let vocab = maki::ai_vocabulary::load_from_path(path)?;
    let mut out = String::new();
    out.push_str(&format!(
        "# Converted from {}\n",
        path.display()
    ));
    out.push_str("# Identity mapping — each label suggests itself as a flat tag.\n");
    out.push_str("# Edit values to add hierarchical paths, e.g.\n");
    out.push_str("#   sunset: lighting|sunset\n");
    out.push_str("#   wedding:\n");
    out.push_str("#     - event|wedding\n");
    out.push_str("#     - subject|people\n\n");
    for label in &vocab.labels {
        out.push_str(&format!("{}: null\n", yaml_quote_key(label)));
    }
    Ok(out)
}

/// Quote a YAML key only when it contains characters that would
/// otherwise need escaping (spaces, punctuation, etc.). Bare
/// identifiers are emitted unquoted to keep the output readable.
#[cfg(feature = "ai")]
fn yaml_quote_key(s: &str) -> String {
    let needs_quotes = s.is_empty()
        || s.chars().any(|c| !matches!(c,
            'a'..='z' | 'A'..='Z' | '0'..='9' | '-' | '_'))
        || s.starts_with('-');
    if needs_quotes {
        // Double-quoted YAML string with escaping for " and \.
        let escaped = s.replace('\\', "\\\\").replace('"', "\\\"");
        format!("\"{escaped}\"")
    } else {
        s.to_string()
    }
}

/// Extracted body of `Commands::AutoTag`. See `run_import_command` for the
/// extraction pattern (Ctx-shadow trick + body kept verbatim).
#[cfg(feature = "ai")]
pub fn run_auto_tag_command(
    query: Option<String>,
    asset: Option<String>,
    volume: Option<String>,
    model: Option<String>,
    threshold: Option<f32>,
    labels: Option<String>,
    apply: bool,
    download: bool,
    remove_model: bool,
    list_models: bool,
    list_labels: bool,
    similar: Option<String>,
    asset_ids: Vec<String>,
    json: bool,
    log: bool,
    #[allow(unused_variables)] verbosity: maki::Verbosity,
) -> anyhow::Result<()> {
    #[allow(dead_code)]
    struct Ctx { json: bool, log: bool }
    let cli = Ctx { json, log };
    let (query, asset) = merge_trailing_ids(query, asset, &asset_ids);
    use maki::model_manager::ModelManager;

    // List labels can work without a catalog (uses defaults)
    if list_labels {
        use maki::ai::{DEFAULT_LABELS, load_labels_from_file};

        let label_list: Vec<String> = if let Some(ref path) = labels {
            load_labels_from_file(std::path::Path::new(path))?
        } else {
            // Try config if catalog exists, fall back to defaults
            let config_labels = maki::config::find_catalog_root()
                .ok()
                .and_then(|root| CatalogConfig::load(&root).ok())
                .and_then(|c| c.ai.labels.clone());
            if let Some(ref path) = config_labels {
                load_labels_from_file(std::path::Path::new(path))?
            } else {
                DEFAULT_LABELS.iter().map(|s| s.to_string()).collect()
            }
        };

        if cli.json {
            println!("{}", serde_json::to_string_pretty(&label_list)?);
        } else {
            for label in &label_list {
                println!("{label}");
            }
            eprintln!("\n{} labels", label_list.len());
        }
        return Ok(());
    }

    let (catalog_root, config) = maki::config::load_config()?;

    // Resolve model ID: CLI --model > config ai.model > default.
    // For --download/--remove-model, also accept the positional `query`
    // as a model id when --model isn't given and the positional looks
    // like a known model (this is what users naturally type).
    let model_id_owned: Option<String> = if (download || remove_model) && model.is_none() {
        query
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty() && maki::ai::get_model_spec(s).is_some())
            .map(|s| s.to_string())
    } else {
        None
    };
    let model_id = model_id_owned
        .as_deref()
        .or(model.as_deref())
        .unwrap_or(&config.ai.model);
    let _spec = maki::ai::get_model_spec(model_id)
        .ok_or_else(|| anyhow::anyhow!(
            "Unknown model: {model_id}. Run 'maki auto-tag --list-models' to see available models."
        ))?;

    let model_dir = maki::config::resolve_model_dir(&config.ai.model_dir, model_id);
    let mgr = ModelManager::new(&model_dir, model_id)?;

    // Model management commands
    if download {
        eprintln!("Downloading {} ...", mgr.spec().display_name);
        mgr.ensure_model(|file, current, total| {
            eprintln!("  [{current}/{total}] {file}");
        })?;
        let total = mgr.total_size();
        if cli.json {
            println!("{}", serde_json::json!({
                "status": "downloaded",
                "model": model_id,
                "model_dir": model_dir.display().to_string(),
                "total_size": total,
            }));
        } else {
            println!("Model downloaded to {}", model_dir.display());
            println!("  Total size: {}", format_size(total));
        }
        return Ok(());
    }

    if remove_model {
        mgr.remove_model()?;
        if cli.json {
            println!("{}", serde_json::json!({
                "status": "removed",
                "model": model_id,
                "model_dir": model_dir.display().to_string(),
            }));
        } else {
            println!("Model removed from {}", model_dir.display());
        }
        return Ok(());
    }

    if list_models {
        // model_dir is `<model_base>/<model_id>`; recover model_base for
        // listing siblings. Defaults to `.` if for some reason the path
        // has no parent (shouldn't happen for normal config).
        let model_base = model_dir.parent().map(|p| p.to_path_buf()).unwrap_or_else(|| PathBuf::from("."));
        let models = ModelManager::list_available_models(&model_base);
        if cli.json {
            let json_models: Vec<serde_json::Value> = models
                .iter()
                .map(|(spec, exists, size)| {
                    serde_json::json!({
                        "id": spec.id,
                        "name": spec.display_name,
                        "downloaded": exists,
                        "size": size,
                        "active": spec.id == model_id,
                        "embedding_dim": spec.embedding_dim,
                    })
                })
                .collect();
            println!("{}", serde_json::json!({
                "model_dir": model_base.display().to_string(),
                "active_model": model_id,
                "models": json_models,
            }));
        } else {
            println!("Available models (directory: {}):", model_base.display());
            for (spec, exists, size) in &models {
                let status = if *exists {
                    format!("downloaded ({})", format_size(*size))
                } else {
                    "not downloaded".to_string()
                };
                let active = if spec.id == model_id { " [active]" } else { "" };
                println!("  {} — {}{active}", spec.display_name, status);
                println!("    ID: {}  Embedding dim: {}  Image size: {}px", spec.id, spec.embedding_dim, spec.image_size);
            }
        }
        return Ok(());
    }

    // Similar search mode
    if let Some(ref similar_id) = similar {
        if !mgr.model_exists() {
            anyhow::bail!(
                "Model not downloaded. Run 'maki auto-tag --download --model {model_id}' first."
            );
        }

        let catalog = maki::catalog::Catalog::open(&catalog_root)?;
        let _ = maki::embedding_store::EmbeddingStore::initialize(catalog.conn());
        let emb_store = maki::embedding_store::EmbeddingStore::new(catalog.conn());

        let full_id = catalog
            .resolve_asset_id(similar_id)?
            .ok_or_else(|| anyhow::anyhow!("no asset found matching '{similar_id}'"))?;

        let query_emb = match emb_store.get(&full_id, model_id)? {
            Some(emb) => emb,
            None => {
                // No stored embedding — encode it now
                let config_preview = &config.preview;
                let service = AssetService::new(&catalog_root, verbosity, config_preview);
                let mut ai_model = maki::ai::SigLipModel::load_with_provider(&model_dir, model_id, verbosity, &config.ai.execution_provider)?;
                let registry = DeviceRegistry::new(&catalog_root);
                let volumes = registry.list()?;
                let online_volumes = maki::models::Volume::online_map(&volumes);
                let preview_gen = maki::preview::PreviewGenerator::new(
                    &catalog_root,
                    verbosity,
                    config_preview,
                );
                let details = catalog
                    .load_asset_details(&full_id)?
                    .ok_or_else(|| anyhow::anyhow!("asset not found"))?;
                let image_path = service
                    .find_image_for_ai(&details, &preview_gen, &online_volumes)
                    .ok_or_else(|| {
                        anyhow::anyhow!("no processable image for asset {}", &full_id[..8])
                    })?;
                let emb = ai_model.encode_image(&image_path)?;
                emb_store.store(&full_id, &emb, model_id)?;
                emb
            }
        };

        let results = emb_store.find_similar(&query_emb, 20, Some(&full_id), model_id)?;

        if cli.json {
            let json_results: Vec<serde_json::Value> = results
                .iter()
                .map(|(id, sim)| {
                    serde_json::json!({
                        "asset_id": id,
                        "similarity": sim,
                    })
                })
                .collect();
            println!("{}", serde_json::to_string_pretty(&json_results)?);
        } else if results.is_empty() {
            println!("No similar assets found. Run 'maki auto-tag' on more assets to build embeddings.");
        } else {
            println!(
                "Assets similar to {} ({} results):",
                &full_id[..8],
                results.len()
            );
            for (id, sim) in &results {
                let short_id = &id[..8.min(id.len())];
                println!("  {short_id}  similarity: {sim:.3}");
            }
        }
        return Ok(());
    }

    // Main auto-tag flow — require at least one scope filter
    if query.is_none() && asset.is_none() && volume.is_none() && similar.is_none() {
        anyhow::bail!(
            "No scope specified. Provide a query, --asset, or --volume to select assets.\n  \
             Examples:\n    \
             maki auto-tag ''                    # all assets\n    \
             maki auto-tag --asset <id>          # single asset\n    \
             maki auto-tag --volume <label>      # one volume\n    \
             maki auto-tag 'tag:landscape' --apply"
        );
    }

    if !mgr.model_exists() {
        anyhow::bail!(
            "Model not downloaded. Run 'maki auto-tag --download --model {model_id}' first."
        );
    }

    // CLI threshold is f32; config field is f64 since v4.5.5. Cast at the boundary.
    let threshold = threshold.unwrap_or(config.ai.threshold as f32);

    // Resolve vocabulary. CLI `--labels` wins; otherwise fall back
    // to `[ai].labels` in config; otherwise use the built-in default.
    // Either path may point at a `.yaml`/`.yml` vocabulary file
    // (rich, with hierarchical mapping) or a plain `.txt` flat list
    // (identity mapping).
    let vocabulary = if let Some(ref labels_path) = labels {
        maki::ai_vocabulary::load_from_path(std::path::Path::new(labels_path))?
    } else if let Some(ref config_labels) = config.ai.labels {
        maki::ai_vocabulary::load_from_path(std::path::Path::new(config_labels))?
    } else {
        maki::ai_vocabulary::default_vocabulary()
    };

    let prompt = &config.ai.prompt;
    let service = AssetService::new(&catalog_root, verbosity, &config.preview);

    let show_log = cli.log;
    let result = service.auto_tag(
        query.as_deref(),
        asset.as_deref(),
        volume.as_deref(),
        threshold,
        &vocabulary,
        prompt,
        apply,
        &model_dir,
        model_id,
        &config.ai.execution_provider,
        |id, status, elapsed| {
            if show_log {
                let short_id = &id[..8.min(id.len())];
                match status {
                    maki::ai::AutoTagStatus::Suggested(tags) => {
                        let tag_names: Vec<&str> =
                            tags.iter().map(|t| t.tag.as_str()).collect();
                        eprintln!(
                            "  {short_id} — {} tags suggested: {} ({})",
                            tags.len(),
                            tag_names.join(", "),
                            format_duration(elapsed)
                        );
                    }
                    maki::ai::AutoTagStatus::Applied(tags) => {
                        let tag_names: Vec<&str> =
                            tags.iter().map(|t| t.tag.as_str()).collect();
                        eprintln!(
                            "  {short_id} — {} tags applied: {} ({})",
                            tags.len(),
                            tag_names.join(", "),
                            format_duration(elapsed)
                        );
                    }
                    maki::ai::AutoTagStatus::Skipped(msg) => {
                        eprintln!("  {short_id} — skipped: {msg}");
                    }
                    maki::ai::AutoTagStatus::Error(msg) => {
                        eprintln!("  {short_id} — error: {msg}");
                    }
                }
            }
        },
    )?;

    if cli.json {
        println!("{}", serde_json::to_string_pretty(&result)?);
    } else {
        for err in &result.errors {
            eprintln!("  {err}");
        }

        let mode = if apply { "Auto-tag" } else { "Auto-tag (dry run)" };
        let mut parts = vec![
            format!("{} processed", result.assets_processed),
        ];
        if result.assets_skipped > 0 {
            parts.push(format!("{} skipped", result.assets_skipped));
        }
        parts.push(format!("{} tags suggested", result.tags_suggested));
        if apply {
            parts.push(format!("{} tags applied", result.tags_applied));
        }
        if !result.errors.is_empty() {
            parts.push(format!("{} errors", result.errors.len()));
        }
        println!("{mode}: {}", parts.join(", "));
        if !apply && result.tags_suggested > 0 {
            println!("  Run with --apply to apply suggested tags.");
        }
    }
    Ok(())
}

/// Extracted body of `Commands::Embed`. See `run_import_command` for the
/// extraction pattern.
#[cfg(feature = "ai")]
pub fn run_embed_command(
        query: Option<String>,
        asset: Option<String>,
        volume: Option<String>,
        model: Option<String>,
        force: bool,
        export: bool,
        asset_ids: Vec<String>,
        json: bool,
        log: bool,
        #[allow(unused_variables)] verbosity: maki::Verbosity,
) -> anyhow::Result<()> {
    #[allow(dead_code)]
    struct Ctx { json: bool, log: bool }
    let cli = Ctx { json, log };
    let (query, asset) = merge_trailing_ids(query, asset, &asset_ids);
    use maki::model_manager::ModelManager;

    if export {
        let catalog_root = maki::config::find_catalog_root()?;
        let catalog = maki::catalog::Catalog::open(&catalog_root)?;
        let _ = maki::embedding_store::EmbeddingStore::initialize(catalog.conn());
        let emb_store = maki::embedding_store::EmbeddingStore::new(catalog.conn());

        let mut total = 0u32;
        let models = emb_store.list_models()?;
        for m in &models {
            let embeddings = emb_store.all_embeddings_for_model(m)?;
            for (asset_id, emb) in &embeddings {
                if let Err(e) = maki::embedding_store::write_embedding_binary(&catalog_root, m, asset_id, emb) {
                    eprintln!("  Warning: {}: {e:#}", &asset_id[..8.min(asset_id.len())]);
                } else {
                    total += 1;
                }
            }
            if !embeddings.is_empty() {
                eprintln!("  {}: {} embeddings", m, embeddings.len());
            }
        }
        if cli.json {
            println!("{}", serde_json::json!({"exported": total, "models": models}));
        } else {
            println!("Exported {total} embedding binaries");
        }
        return Ok(());
    }

    if query.is_none() && asset.is_none() && volume.is_none() {
        anyhow::bail!(
            "No scope specified. Provide a query, --asset, or --volume to select assets.\n  \
             Examples:\n    \
             maki embed ''                    # all assets\n    \
             maki embed --asset <id>          # single asset\n    \
             maki embed --volume <label>      # one volume"
        );
    }

    let (catalog_root, config) = maki::config::load_config()?;

    let model_id = model.as_deref().unwrap_or(&config.ai.model);
    let _spec = maki::ai::get_model_spec(model_id)
        .ok_or_else(|| anyhow::anyhow!(
            "Unknown model: {model_id}. Run 'maki auto-tag --list-models' to see available models."
        ))?;

    let model_dir = maki::config::resolve_model_dir(&config.ai.model_dir, model_id);
    let mgr = ModelManager::new(&model_dir, model_id)?;

    if !mgr.model_exists() {
        anyhow::bail!(
            "Model not downloaded. Run 'maki auto-tag --download --model {model_id}' first."
        );
    }

    let catalog = maki::catalog::Catalog::open(&catalog_root)?;
    let engine = QueryEngine::new(&catalog_root);

    // Resolve target assets
    let asset_ids: Vec<String> = if let Some(ref id) = asset {
        let full_id = catalog
            .resolve_asset_id(id)?
            .ok_or_else(|| anyhow::anyhow!("no asset found matching '{id}'"))?;
        vec![full_id]
    } else {
        let q = if let Some(ref query) = query {
            let volume_part = volume.as_deref().map(|v| format!(" volume:{v}")).unwrap_or_default();
            format!("{query}{volume_part}")
        } else if let Some(ref v) = volume {
            format!("volume:{v}")
        } else {
            String::new()
        };
        let results = engine.search(&q)?;
        results.into_iter().map(|r| r.asset_id).collect()
    };

    let service = AssetService::new(&catalog_root, verbosity, &config.preview);
    let log = cli.log;
    let result = service.embed_assets(
        &asset_ids,
        &model_dir,
        model_id,
        &config.ai.execution_provider,
        force,
        |aid, status, elapsed| {
            if !log { return; }
            let short = &aid[..8.min(aid.len())];
            match status {
                maki::asset_service::EmbedStatus::Embedded => {
                    item_status(short, "embedded", Some(elapsed));
                }
                maki::asset_service::EmbedStatus::Skipped(reason) => {
                    item_status(short, &format!("skipped: {reason}"), Some(elapsed));
                }
                maki::asset_service::EmbedStatus::Error(msg) => {
                    eprintln!("  {short} — error: {msg}");
                }
            }
        },
    )?;

    if cli.json {
        println!("{}", serde_json::json!({
            "embedded": result.embedded,
            "skipped": result.skipped,
            "errors": result.errors,
            "model": model_id,
            "force": force,
        }));
    } else {
        for err in &result.errors {
            eprintln!("  {err}");
        }
        let mut parts = vec![
            format!("{} embedded", result.embedded),
            format!("{} skipped", result.skipped),
        ];
        if !result.errors.is_empty() {
            parts.push(format!("{} errors", result.errors.len()));
        }
        println!("Embed: {}", parts.join(", "));
    }
    Ok(())
}

/// Extracted body of `Commands::Describe`. See `run_import_command` for the
/// extraction pattern.
#[cfg(feature = "pro")]
pub fn run_describe_command(
        query: Option<String>,
        asset: Option<String>,
        volume: Option<String>,
        model: Option<String>,
        endpoint: Option<String>,
        prompt: Option<String>,
        max_tokens: Option<u32>,
        timeout: Option<u32>,
        mode: String,
        temperature: Option<f32>,
        num_ctx: Option<u32>,
        top_p: Option<f32>,
        top_k: Option<u32>,
        repeat_penalty: Option<f32>,
        apply: bool,
        force: bool,
        dry_run: bool,
        check: bool,
        asset_ids: Vec<String>,
        json: bool,
        log: bool,
        #[allow(unused_variables)] verbosity: maki::Verbosity,
) -> anyhow::Result<()> {
    #[allow(dead_code)]
    struct Ctx { json: bool, log: bool }
    let cli = Ctx { json, log };
    let (catalog_root, config) = maki::config::load_config()?;

    let endpoint = endpoint.as_deref().unwrap_or(&config.vlm.endpoint);
    let model = model.as_deref().unwrap_or(&config.vlm.model);
    let vlm_mode = maki::vlm::DescribeMode::from_str(&mode)?;

    // Build params: per-model config merged with CLI overrides
    let mut vlm_params = config.vlm.params_for_model(model);
    if let Some(v) = max_tokens { vlm_params.max_tokens = v; }
    if let Some(v) = timeout { vlm_params.timeout = v; }
    if let Some(v) = temperature { vlm_params.temperature = v; }
    if let Some(v) = num_ctx { vlm_params.num_ctx = v; }
    if let Some(v) = top_p { vlm_params.top_p = v; }
    if let Some(v) = top_k { vlm_params.top_k = v; }
    if let Some(v) = repeat_penalty { vlm_params.repeat_penalty = v; }
    if let Some(ref p) = prompt { vlm_params.prompt = Some(p.clone()); }

    if check {
        match maki::vlm::check_endpoint_status(endpoint, vlm_params.timeout, verbosity) {
            Ok(status) => {
                let model_status = if status.available_models.is_empty() {
                    format!("Configured model: {model}")
                } else {
                    match maki::vlm::find_matching_model(model, &status.available_models) {
                        Some(matched) if matched == model => {
                            format!("Model {model} is available")
                        }
                        Some(matched) => {
                            format!("Model {matched} matched (from \"{model}\")")
                        }
                        None => {
                            format!(
                                "WARNING: model \"{model}\" not found. Pull it with `ollama pull {model}` or set [vlm] model in maki.toml"
                            )
                        }
                    }
                };
                if cli.json {
                    let model_ok = status.available_models.is_empty()
                        || maki::vlm::find_matching_model(model, &status.available_models).is_some();
                    println!("{}", serde_json::json!({
                        "status": "ok",
                        "endpoint": endpoint,
                        "model": model,
                        "model_available": model_ok,
                        "available_models": status.available_models,
                        "message": status.message,
                    }));
                } else {
                    println!("{}", status.message);
                    println!("{model_status}");
                }
            }
            Err(e) => {
                if cli.json {
                    println!("{}", serde_json::json!({
                        "status": "error",
                        "endpoint": endpoint,
                        "model": model,
                        "message": format!("{e}"),
                    }));
                } else {
                    eprintln!("{e}");
                }
                anyhow::bail!("vLM endpoint check failed");
            }
        }
        return Ok(());
    }

    // Merge asset_ids from shell variable expansion into asset/query
    let (query, asset) = merge_trailing_ids(query, asset, &asset_ids);

    if query.is_none() && asset.is_none() && volume.is_none() {
        anyhow::bail!(
            "No scope specified. Use a query, --asset, or --volume to select assets.\n  \
             Examples:\n    \
             maki describe ''                    # all assets\n    \
             maki describe --asset <id>          # single asset\n    \
             maki describe 'rating:4+' --apply   # apply to rated assets"
        );
    }

    if verbosity.verbose {
        eprintln!("  VLM: endpoint={endpoint}, model={model}, mode={mode}");
        eprintln!("  VLM: max_tokens={}, timeout={}s, temperature={}, concurrency={}", vlm_params.max_tokens, vlm_params.timeout, vlm_params.temperature, config.vlm.concurrency);
    }

    let service = AssetService::new(&catalog_root, verbosity, &config.preview);

    let show_log = cli.log;
    let result = service.describe(
        query.as_deref(),
        asset.as_deref(),
        volume.as_deref(),
        endpoint,
        model,
        &vlm_params,
        vlm_mode,
        apply,
        force,
        dry_run,
        config.vlm.concurrency,
        |id, status, elapsed| {
            if show_log {
                let short_id = &id[..8.min(id.len())];
                match status {
                    maki::vlm::DescribeStatus::Described => {
                        eprintln!(
                            "  {short_id} — described ({})",
                            format_duration(elapsed)
                        );
                    }
                    maki::vlm::DescribeStatus::Skipped(msg) => {
                        eprintln!("  {short_id} — skipped: {msg}");
                    }
                    maki::vlm::DescribeStatus::Error(msg) => {
                        eprintln!("  {short_id} — error: {msg}");
                    }
                }
            }
        },
    )?;

    if cli.json {
        println!("{}", serde_json::to_string_pretty(&result)?);
    } else {
        // Print each result
        for r in &result.results {
            let short_id = &r.asset_id[..8.min(r.asset_id.len())];
            match &r.status {
                maki::vlm::DescribeStatus::Described => {
                    if let Some(ref desc) = r.description {
                        println!("{short_id}: {desc}");
                    }
                    if !r.tags.is_empty() {
                        println!("{short_id}: tags: {}", r.tags.join(", "));
                    }
                }
                maki::vlm::DescribeStatus::Skipped(msg) => {
                    if !cli.log {
                        eprintln!("{short_id}: skipped — {msg}");
                    }
                }
                maki::vlm::DescribeStatus::Error(msg) => {
                    if !cli.log {
                        eprintln!("{short_id}: error — {msg}");
                    }
                }
            }
        }

        let label = if dry_run {
            "Describe (dry run)"
        } else if apply {
            "Describe"
        } else {
            "Describe (report only)"
        };
        let mut parts = vec![format!("{} processed", result.described)];
        if result.skipped > 0 {
            parts.push(format!("{} skipped", result.skipped));
        }
        if result.failed > 0 {
            parts.push(format!("{} failed", result.failed));
        }
        if result.tags_applied > 0 {
            parts.push(format!("{} tags applied", result.tags_applied));
        }
        eprintln!("{label}: {}", parts.join(", "));
        if !apply && !dry_run && result.described > 0 {
            eprintln!("  Run with --apply to save changes to assets.");
        }
    }
    Ok(())
}
