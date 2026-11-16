//! Media serving, comparison, file-manager integration, writeback, VLM describe, and ZIP export.

use std::sync::Arc;

use askama::Template;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Response};
use axum::Json;

use crate::device_registry::DeviceRegistry;
use crate::web::templates::{CompareAsset, ComparePage};
use crate::web::AppState;


#[derive(Debug, serde::Deserialize)]
pub struct CompareParams {
    pub ids: Option<String>,
}

/// GET /compare?ids=id1,id2,... — side-by-side compare page.
pub async fn compare_page(
    State(state): State<Arc<AppState>>,
    Query(params): Query<CompareParams>,
) -> Result<Response, Response> {
    let ids_str = params.ids.unwrap_or_default();
    let ids: Vec<&str> = ids_str.split(',').map(|s| s.trim()).filter(|s| !s.is_empty()).collect();

    if ids.len() < 2 || ids.len() > 4 {
        return Ok((
            StatusCode::BAD_REQUEST,
            Html("<h1>Compare</h1><p>Select 2\u{2013}4 assets to compare.</p><p><a href=\"/\">Back to browse</a></p>".to_string()),
        )
            .into_response());
    }

    let preview_ext = state.preview_ext.clone();
    let ids_owned: Vec<String> = ids.iter().map(|s| s.to_string()).collect();
    let state = state.clone();
    let html = super::spawn_blocking_with(move || {
        let engine = state.query_engine()?;
        let preview_gen = state.preview_generator();
        let mut assets = Vec::new();

        for id in &ids_owned {
            let details = engine.show(id)?;
            let best = details.best_variant_index();
            let purl = best
                .and_then(|i| {
                    let v = &details.variants[i];
                    if preview_gen.has_preview(&v.content_hash) {
                        Some(crate::web::templates::preview_url(&v.content_hash, &preview_ext))
                    } else {
                        None
                    }
                })
                .unwrap_or_default();

            assets.push(CompareAsset::from_details(&details, purl));
        }

        let tmpl = ComparePage { assets, ai_enabled: state.ai_enabled, vlm_enabled: state.vlm_enabled };
        Ok::<_, anyhow::Error>(tmpl.render()?)
    }, super::err::mapped_html)
    .await?;
    Ok(Html(html).into_response())
}

/// GET /smart-preview/{prefix}/{file} — serve smart preview, generating on-demand if configured.
pub async fn serve_smart_preview(
    State(state): State<Arc<AppState>>,
    Path((prefix, file)): Path<(String, String)>,
) -> Response {
    let smart_dir = state.catalog_root.join("smart-previews");
    let file_path = smart_dir.join(&prefix).join(&file);

    if file_path.exists() {
        return serve_smart_file(&file_path, &file).await;
    }

    if !state.smart_on_demand {
        return StatusCode::NOT_FOUND.into_response();
    }

    let hash_hex = match file.rsplit_once('.') {
        Some((stem, _ext)) => stem,
        None => &file,
    };
    let content_hash = format!("sha256:{hash_hex}");

    let state = state.clone();
    let file_path_clone = file_path.clone();
    let file_name = file.clone();
    // Every failure — including a panicked task — is a plain 404 here.
    let result = super::spawn_catalog_blocking(move || {
        let catalog = state.catalog()?;

        let format = catalog
            .get_variant_format(&content_hash)?
            .ok_or_else(|| anyhow::anyhow!("variant not found"))?;

        let locations = catalog.get_variant_file_locations(&content_hash)?;
        let registry = DeviceRegistry::new(&state.catalog_root);
        let volumes = registry.list()?;

        let source_path = locations
            .iter()
            .find_map(|(vol_id, rel_path)| {
                let vol = volumes.iter().find(|v| v.id.to_string() == *vol_id)?;
                if !vol.is_online {
                    return None;
                }
                let path = vol.mount_point.join(rel_path);
                if path.exists() {
                    Some(path)
                } else {
                    None
                }
            })
            .ok_or_else(|| anyhow::anyhow!("no online source file"))?;

        let preview_gen = state.preview_generator();
        preview_gen.generate_smart(&content_hash, &source_path, &format)?;

        Ok::<_, anyhow::Error>(())
    })
    .await;

    match result {
        Ok(()) if file_path_clone.exists() => {
            serve_smart_file(&file_path_clone, &file_name).await
        }
        _ => StatusCode::NOT_FOUND.into_response(),
    }
}

/// GET /video/{hash} and GET /audio/{hash} — serve a media file with
/// range request support for seeking. One handler for both media types:
/// the Content-Type is derived from the variant's format either way, the
/// two routes exist only so the URLs say what they serve.
pub async fn serve_video(
    State(state): State<Arc<AppState>>,
    Path(hash): Path<String>,
    headers: axum::http::HeaderMap,
) -> Response {
    let content_hash = format!("sha256:{hash}");

    let state2 = state.clone();
    // Every failure — including a panicked task — is a plain 404 here.
    let resolved = super::spawn_catalog_blocking(move || {
        let catalog = state2.catalog()?;
        let locations = catalog.get_variant_file_locations(&content_hash)?;
        let format = catalog.get_variant_format(&content_hash)?;
        let registry = DeviceRegistry::new(&state2.catalog_root);
        let volumes = registry.list()?;

        let source_path = locations
            .iter()
            .find_map(|(vol_id, rel_path)| {
                let vol = volumes.iter().find(|v| v.id.to_string() == *vol_id)?;
                if !vol.is_online { return None; }
                let path = vol.mount_point.join(rel_path);
                if path.exists() { Some(path) } else { None }
            });

        Ok::<_, anyhow::Error>((source_path, format))
    })
    .await;

    let (source_path, format) = match resolved {
        Ok((Some(path), format)) => (path, format),
        _ => return StatusCode::NOT_FOUND.into_response(),
    };

    let content_type = match format.as_deref().unwrap_or("mp4") {
        "mp4" | "m4v" => "video/mp4",
        "mov" => "video/quicktime",
        "webm" => "video/webm",
        "mkv" => "video/x-matroska",
        "avi" => "video/x-msvideo",
        "mts" | "m2ts" => "video/mp2t",
        "3gp" => "video/3gpp",
        "mp3" => "audio/mpeg",
        "m4a" | "alac" => "audio/mp4",
        "aac" => "audio/aac",
        "flac" => "audio/flac",
        "ogg" | "opus" => "audio/ogg",
        "wav" => "audio/wav",
        "aiff" | "aif" => "audio/aiff",
        "wma" => "audio/x-ms-wma",
        _ => "video/mp4",
    };

    let file = match tokio::fs::File::open(&source_path).await {
        Ok(f) => f,
        Err(_) => return StatusCode::NOT_FOUND.into_response(),
    };
    let metadata = match file.metadata().await {
        Ok(m) => m,
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    let total_size = metadata.len();

    if let Some(range_header) = headers.get(axum::http::header::RANGE) {
        if let Ok(range_str) = range_header.to_str() {
            if let Some(range) = range_str.strip_prefix("bytes=") {
                let parts: Vec<&str> = range.splitn(2, '-').collect();
                let start: u64 = parts[0].parse().unwrap_or(0);
                let end: u64 = if parts.len() > 1 && !parts[1].is_empty() {
                    parts[1].parse().unwrap_or(total_size - 1)
                } else {
                    total_size - 1
                };

                if start >= total_size {
                    return StatusCode::RANGE_NOT_SATISFIABLE.into_response();
                }
                let end = end.min(total_size - 1);
                let chunk_size = end - start + 1;

                use tokio::io::{AsyncReadExt, AsyncSeekExt};
                let mut file = file;
                if file.seek(std::io::SeekFrom::Start(start)).await.is_err() {
                    return StatusCode::INTERNAL_SERVER_ERROR.into_response();
                }
                let mut buf = vec![0u8; chunk_size as usize];
                if file.read_exact(&mut buf).await.is_err() {
                    let mut file = match tokio::fs::File::open(&source_path).await {
                        Ok(f) => f,
                        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
                    };
                    let _ = file.seek(std::io::SeekFrom::Start(start)).await;
                    buf.clear();
                    buf.resize(chunk_size as usize, 0);
                    let n = file.read(&mut buf).await.unwrap_or(0);
                    buf.truncate(n);
                }

                return Response::builder()
                    .status(StatusCode::PARTIAL_CONTENT)
                    .header("Content-Type", content_type)
                    .header("Content-Length", buf.len().to_string())
                    .header("Content-Range", format!("bytes {start}-{end}/{total_size}"))
                    .header("Accept-Ranges", "bytes")
                    .body(axum::body::Body::from(buf))
                    .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response());
            }
        }
    }

    use tokio::io::AsyncReadExt;
    let mut file = file;
    let mut buf = Vec::with_capacity(total_size as usize);
    if file.read_to_end(&mut buf).await.is_err() {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }

    Response::builder()
        .status(StatusCode::OK)
        .header("Content-Type", content_type)
        .header("Content-Length", total_size.to_string())
        .header("Accept-Ranges", "bytes")
        .body(axum::body::Body::from(buf))
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

async fn serve_smart_file(path: &std::path::Path, filename: &str) -> Response {
    match tokio::fs::read(path).await {
        Ok(bytes) => {
            let content_type = if filename.ends_with(".webp") {
                "image/webp"
            } else {
                "image/jpeg"
            };
            (
                StatusCode::OK,
                [
                    (axum::http::header::CONTENT_TYPE, content_type),
                    (axum::http::header::CACHE_CONTROL, "public, max-age=86400"),
                ],
                bytes,
            )
                .into_response()
        }
        Err(_) => StatusCode::NOT_FOUND.into_response(),
    }
}

#[derive(Debug, serde::Deserialize)]
pub struct OpenLocationRequest {
    pub volume_id: String,
    pub relative_path: String,
}

/// POST /api/open-location — reveal a file in the system file manager.
pub async fn open_location(
    State(state): State<Arc<AppState>>,
    Json(req): Json<OpenLocationRequest>,
) -> Result<Response, Response> {
    let state = state.clone();
    super::spawn_blocking_with(move || {
        let full_path = resolve_online_path(&state, &req)?;
        if !full_path.exists() {
            anyhow::bail!("file not found on disk: {}", full_path.display());
        }
        crate::platform::reveal_path(&full_path)
            .map_err(|e| anyhow::anyhow!("failed to open file manager: {e}"))
    }, open_error)
    .await?;
    Ok(Json(serde_json::json!({"ok": true})).into_response())
}

/// `400 <top-level message>` — the opener endpoints report the reason
/// (offline volume, missing file, no terminal emulator) without the
/// cause chain.
fn open_error(e: anyhow::Error) -> Response {
    (StatusCode::BAD_REQUEST, e.to_string()).into_response()
}

/// Resolve `volume_id` + `relative_path` to an absolute path on an
/// online volume (shared by the reveal / terminal endpoints).
fn resolve_online_path(
    state: &AppState,
    req: &OpenLocationRequest,
) -> anyhow::Result<std::path::PathBuf> {
    let registry = DeviceRegistry::new(&state.catalog_root);
    let volumes = registry.list()?;
    let vol = volumes
        .iter()
        .find(|v| v.id.to_string() == req.volume_id)
        .ok_or_else(|| anyhow::anyhow!("volume not found"))?;
    if !vol.is_online {
        anyhow::bail!("volume '{}' is offline", vol.label);
    }
    Ok(vol.mount_point.join(&req.relative_path))
}

/// POST /api/open-terminal — open a terminal in the file's parent directory.
pub async fn open_terminal(
    State(state): State<Arc<AppState>>,
    Json(req): Json<OpenLocationRequest>,
) -> Result<Response, Response> {
    let state = state.clone();
    super::spawn_blocking_with(move || {
        let full_path = resolve_online_path(&state, &req)?;
        let dir = if full_path.is_dir() {
            full_path
        } else {
            full_path.parent()
                .ok_or_else(|| anyhow::anyhow!("cannot determine parent directory"))?
                .to_path_buf()
        };
        if !dir.exists() {
            anyhow::bail!("directory not found on disk: {}", dir.display());
        }
        crate::platform::open_terminal(&dir)
            .map_err(|e| anyhow::anyhow!("failed to open terminal: {e}"))
    }, open_error)
    .await?;
    Ok(Json(serde_json::json!({"ok": true})).into_response())
}

// --- Writeback ---

/// POST /api/asset/{id}/writeback — write pending metadata changes to XMP recipe files.
pub async fn writeback_asset(
    State(state): State<Arc<AppState>>,
    Path(asset_id): Path<String>,
) -> Result<Response, Response> {
    let state = state.clone();
    let wb = super::spawn_catalog_blocking(move || {
        let engine = state.query_engine()?;
        let catalog = state.catalog()?;
        let full_id = super::resolve_asset_id_or_err(&catalog, &asset_id)?;

        // Only this asset's pending recipes; the web flush is incremental
        // (no `all`, no `force`, no `mirror_tags`). Warnings go to the
        // server log.
        let req = crate::query::WritebackRequest {
            asset_prefix: Some(&full_id),
            ..Default::default()
        };
        let wb_result = engine.writeback_with(&req, &mut |event| {
            if let crate::query::WritebackEvent::Warning(message) = &event {
                eprintln!("{message}");
            }
        })?;
        Ok::<_, anyhow::Error>(wb_result)
    })
    .await?;
    Ok(Json(serde_json::json!({
        "written": wb.written,
        "skipped": wb.skipped,
        "failed": wb.failed,
    }))
    .into_response())
}

// --- VLM Describe ---

#[derive(serde::Deserialize)]
pub struct VlmDescribeRequest {
    #[serde(default)]
    pub mode: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
}

#[derive(serde::Serialize)]
pub struct VlmDescribeResponse {
    pub description: Option<String>,
    pub tags: Vec<String>,
}

/// POST /api/asset/{id}/vlm-describe — describe a single asset via VLM.
pub async fn vlm_describe_asset(
    State(state): State<Arc<AppState>>,
    Path(asset_id): Path<String>,
    Json(body): Json<VlmDescribeRequest>,
) -> Result<Response, Response> {
    let state = state.clone();
    let resp = super::spawn_catalog_blocking(move || {
        vlm_describe_asset_inner(&state, &asset_id, body.mode.as_deref(), body.model.as_deref())
            .map_err(|e| anyhow::anyhow!(e))
    })
    .await?;
    Ok(Json(resp).into_response())
}

fn vlm_describe_asset_inner(
    state: &AppState,
    asset_id: &str,
    mode_str: Option<&str>,
    model_override: Option<&str>,
) -> Result<VlmDescribeResponse, String> {
    use crate::vlm::{self, DescribeMode};

    let vlm = &state.vlm_config;
    let mode = mode_str
        .map(|s| DescribeMode::parse_mode(s).map_err(|e| e.to_string()))
        .transpose()?
        .unwrap_or_else(|| DescribeMode::parse_mode(&vlm.mode).unwrap_or(DescribeMode::Describe));
    let model = model_override.unwrap_or(&vlm.model);
    let params = vlm.params_for_model(model);
    let prompt = params.prompt.as_deref()
        .unwrap_or_else(|| vlm::default_prompt_for_mode(mode));

    let engine = state.query_engine().map_err(|e| format!("{e:#}"))?;
    let service = state.asset_service();
    let preview_gen = state.preview_generator();

    let catalog = state.catalog().map_err(|e| e.to_string())?;
    let full_id = super::resolve_asset_id_or_err(&catalog, asset_id)
        .map_err(|e| format!("{e:#}"))?;

    let details = engine.show(&full_id).map_err(|e| e.to_string())?;

    let registry = DeviceRegistry::new(&state.catalog_root);
    let volumes = registry.list().map_err(|e| e.to_string())?;
    let online_volumes: std::collections::HashMap<String, &crate::models::Volume> = volumes
        .iter()
        .filter(|v| v.is_online)
        .map(|v| (v.id.to_string(), v))
        .collect();

    let image_path = service.find_image_for_vlm(&details, &preview_gen, &online_volumes)
        .ok_or_else(|| "No preview image available. Run `maki generate-previews` first.".to_string())?;

    let max_edge = if params.max_image_edge > 0 { Some(params.max_image_edge) } else { None };
    let image_base64 = vlm::encode_image_base64(&image_path, max_edge).map_err(|e| e.to_string())?;
    let output = vlm::call_vlm_with_mode(
        &vlm.endpoint,
        model,
        &image_base64,
        prompt,
        &params,
        mode,
        state.verbosity,
    )
    .map_err(|e| e.to_string())?;

    if let Some(ref desc) = output.description {
        if !desc.is_empty() {
            let edit_fields = crate::query::EditFields {
                name: None,
                description: Some(Some(desc.clone())),
                rating: None,
                color_label: None,
                created_at: None,
            };
            engine.edit(&full_id, edit_fields).map_err(|e| e.to_string())?;
        }
    }

    if !output.tags.is_empty() {
        let existing_tags: std::collections::HashSet<String> = details
            .tags
            .iter()
            .map(|t| t.to_lowercase())
            .collect();
        let new_tags: Vec<String> = output
            .tags
            .iter()
            .filter(|t| !existing_tags.contains(&t.to_lowercase()))
            .cloned()
            .collect();
        if !new_tags.is_empty() {
            engine
                .tag_with_source(&full_id, &new_tags, false, crate::models::TagSource::Vlm)
                .map_err(|e| e.to_string())?;
        }
    }

    Ok(VlmDescribeResponse {
        description: output.description,
        tags: output.tags,
    })
}

#[derive(serde::Deserialize)]
pub struct BatchVlmDescribeRequest {
    pub asset_ids: Vec<String>,
    #[serde(default)]
    pub mode: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
}

#[derive(serde::Serialize)]
pub struct BatchVlmDescribeResponse {
    pub succeeded: u32,
    pub failed: u32,
    pub descriptions_set: u32,
    pub tags_applied: u32,
    pub errors: Vec<String>,
}

/// POST /api/batch/describe — start a VLM describe job for selected assets.
///
/// Returns `{job_id}` immediately. Progress flows through `/api/jobs/{id}/progress`;
/// the terminal event carries `{succeeded, failed, descriptions_set, tags_applied,
/// errors, done: true}`.
pub async fn batch_vlm_describe(
    State(state): State<Arc<AppState>>,
    Json(body): Json<BatchVlmDescribeRequest>,
) -> Response {
    use crate::web::jobs::{launch_job, JobKind, LaunchOptions};

    let total = body.asset_ids.len();
    // No conflict gate: several describe batches may run at once.
    let opts = LaunchOptions::default().with_starting_event("describe", total);
    launch_job(&state, JobKind::Describe, opts, move |state, job| {
        let resp = batch_vlm_describe_inner(
            state,
            &body.asset_ids,
            body.mode.as_deref(),
            body.model.as_deref(),
            job,
            total,
        )
        .map_err(anyhow::Error::msg)?;
        if resp.tags_applied > 0 {
            state.dropdown_cache.invalidate_tags();
        }
        Ok(serde_json::json!({
            "phase": "describe",
            "succeeded": resp.succeeded,
            "failed": resp.failed,
            "descriptions_set": resp.descriptions_set,
            "tags_applied": resp.tags_applied,
            "errors": resp.errors,
        }))
    })
}

/// The batch loop is [`AssetService::describe_assets`] (shared with
/// `maki describe` and the post-import phase); this adds what the web UI
/// needs around it: ID-prefix resolution for the selection, the per-asset
/// SSE progress events, and the response counters the job toast reads.
fn batch_vlm_describe_inner(
    state: &AppState,
    asset_ids: &[String],
    mode_str: Option<&str>,
    model_override: Option<&str>,
    job: &std::sync::Arc<crate::web::jobs::Job>,
    total: usize,
) -> Result<BatchVlmDescribeResponse, String> {
    use crate::vlm::{DescribeError, DescribeMode, DescribeStatus, SkipReason};

    let vlm = &state.vlm_config;
    let mode = mode_str
        .map(|s| DescribeMode::parse_mode(s).map_err(|e| e.to_string()))
        .transpose()?
        .unwrap_or_else(|| DescribeMode::parse_mode(&vlm.mode).unwrap_or(DescribeMode::Describe));
    let vlm_model = model_override.unwrap_or(&vlm.model);
    let params = vlm.params_for_model(vlm_model);

    let mut result = BatchVlmDescribeResponse {
        succeeded: 0,
        failed: 0,
        descriptions_set: 0,
        tags_applied: 0,
        errors: Vec::new(),
    };

    // Per-asset progress with a running `processed` count, so the toast
    // advances smoothly even through pre-flight outcomes (not found,
    // already described, no preview).
    let processed = std::sync::atomic::AtomicUsize::new(0);
    let emit_progress = |aid: &str, status: &str| {
        let n = processed.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
        job.emit(&serde_json::json!({
            "phase": "describe",
            "done": false,
            "processed": n,
            "total": total,
            "asset": &aid[..8.min(aid.len())],
            "status": status,
        }));
    };

    // `state.verbosity` (not the quiet `asset_service()`) so `--debug`
    // shows the VLM exchange on stderr as the CLI does. The selection may
    // carry ID prefixes; `DescribeScope::Ids` resolves them.
    let service = crate::asset_service::AssetService::with_config(
        &state.catalog_root,
        state.verbosity,
        state.config(),
    );
    let request = crate::asset_service::DescribeRequest {
        scope: crate::asset_service::DescribeScope::Ids(asset_ids.to_vec()),
        endpoint: vlm.endpoint.clone(),
        model: vlm_model.to_string(),
        params,
        mode,
        apply: true,
        force: false,
        dry_run: false,
        concurrency: vlm.concurrency,
    };
    let mut descriptions_set = 0u32;
    let batch = service
        .describe_with(&request, |event| {
            if event.description_set {
                descriptions_set += 1;
            }
            emit_progress(event.asset_id, match event.status {
                DescribeStatus::Described => "described",
                DescribeStatus::Skipped(SkipReason::NoImage { .. }) => "no-image",
                DescribeStatus::Skipped(_) => "skipped",
                DescribeStatus::Error(DescribeError::NotFound { .. }) => "not-found",
                DescribeStatus::Error(_) => "error",
            });
        })
        .map_err(|e| format!("{e:#}"))?;

    // `succeeded` is "finished without error": described, already
    // described, or nothing to describe (no preview).
    result.succeeded += (batch.described + batch.skipped) as u32;
    result.failed += batch.failed as u32;
    result.tags_applied += batch.tags_applied as u32;
    result.descriptions_set = descriptions_set;
    result.errors.extend(batch.errors);
    Ok(result)
}

// ─── Export ZIP ─────────────────────────────────────────────────────

#[derive(serde::Deserialize)]
pub struct ExportZipRequest {
    #[serde(default)]
    pub asset_ids: Vec<String>,
    /// Browse URL params, passed through verbatim by the export dialog so
    /// "Export all" matches the grid's result set exactly (including
    /// `stacks` collapse and `nodefault`).
    #[serde(default)]
    pub filters: Option<super::SearchParams>,
    #[serde(default = "default_layout")]
    pub layout: String,
    #[serde(default)]
    pub all_variants: bool,
    #[serde(default)]
    pub include_sidecars: bool,
    /// "originals" (default) | "previews" | "smart" — what file content to
    /// export. Preview sources come from the catalog directory, so they
    /// work for assets whose volumes are offline.
    #[serde(default)]
    pub source: String,
}

fn default_layout() -> String {
    "flat".to_string()
}

/// Stream a ZIP archive of exported assets as a download.
pub async fn export_zip(
    State(state): State<Arc<AppState>>,
    Json(req): Json<ExportZipRequest>,
) -> Result<Response, Response> {
    use axum::http::header;
    use crate::asset_service::{AssetService, ExportLayout, ExportSource};

    let catalog_root = state.catalog_root.clone();

    let asset_ids = if !req.asset_ids.is_empty() {
        req.asset_ids
    } else {
        let state2 = state.clone();
        let params = req.filters.unwrap_or_default();
        super::spawn_blocking_with(move || -> anyhow::Result<Vec<String>> {
            let catalog = state2.catalog()?;

            // Same pipeline as browse_page / search_api, so "Export all"
            // covers exactly the grid's result set: default filter from
            // [browse] config (honoring `nodefault`), AI text-query /
            // `similar:` resolution, and `stacks` collapse. Only deliberate
            // difference: EmptyFilterPolicy::MatchNothing — an unresolvable
            // collection/person exports nothing rather than everything
            // (fail-closed; see the drift-matrix history in the resolver
            // docs for why export-zip is stricter here).
            let query = super::BrowseQuery {
                empty_filter_policy: crate::query::EmptyFilterPolicy::MatchNothing,
                scope: super::BrowseScope::All,
                count: false,
                count_deltas: false,
            };
            Ok(super::run_browse_search(&state2, &catalog, &params, query)?.ids())
        }, |e| (StatusCode::BAD_REQUEST, format!("Search failed: {e}")).into_response())
        .await?
    };

    if asset_ids.is_empty() {
        return Ok((StatusCode::BAD_REQUEST, "No assets matched").into_response());
    }

    let layout = if req.layout == "mirror" { ExportLayout::Mirror } else { ExportLayout::Flat };
    let source = match req.source.as_str() {
        "previews" => ExportSource::Previews,
        "smart" | "smart-previews" => ExportSource::SmartPreviews,
        _ => ExportSource::Originals,
    };
    let all_variants = req.all_variants;
    let include_sidecars = req.include_sidecars;
    let count = asset_ids.len();

    let ids = asset_ids;
    let root = catalog_root.clone();
    let tmp = std::env::temp_dir().join(format!("maki-export-{}.zip", std::process::id()));
    let tmp2 = tmp.clone();
    let (zip_path, exported, skipped, skipped_offline) = super::spawn_blocking_with(move || {
        let service = AssetService::with_config(&root, state.verbosity, state.config());
        let result = service.export_zip_for_ids(&ids, &tmp2, layout, all_variants, include_sidecars, source, |_, _, _| {})
            .map_err(|e| anyhow::anyhow!("Export failed: {e}"))?;
        if result.files_exported == 0 && result.sidecars_exported == 0 {
            let _ = std::fs::remove_file(&tmp2);
            anyhow::bail!(if source == ExportSource::Originals {
                "No exportable files found (volumes may be offline)"
            } else {
                "No exportable files found (previews not generated yet — run maki generate-previews)"
            });
        }
        let offline = result.errors.iter().filter(|e| e.contains("offline")).count();
        Ok((tmp2, result.files_exported + result.sidecars_exported, result.errors.len(), offline))
    }, |e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response())
    .await?;

    let body = super::stream_file_then_remove(zip_path);

    let filename = format!("maki-export-{}-assets.zip", count);

    // Skip counts as headers so the export dialog can warn about files that
    // were left out (offline volumes, unreadable files) — the ZIP itself
    // downloads fine either way, which used to make partial exports look
    // like complete ones.
    Ok(Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/zip")
        .header(header::CONTENT_DISPOSITION, format!("attachment; filename=\"{filename}\""))
        .header("X-Maki-Exported", exported.to_string())
        .header("X-Maki-Skipped", skipped.to_string())
        .header("X-Maki-Skipped-Offline", skipped_offline.to_string())
        .body(body)
        .unwrap_or_else(|_| (StatusCode::INTERNAL_SERVER_ERROR, "Failed to build response").into_response()))
}
