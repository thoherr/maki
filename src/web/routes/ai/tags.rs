//! `suggest-tags` and batch `auto-tag` route handlers.
//!
//! Both rely on the SigLIP model loaded into AppState's cached slot, with
//! the active label list (config-defined or `DEFAULT_LABELS`) embedded once
//! and reused across requests.
//!
//! Single-asset (`suggest_tags`) and batch (`batch_auto_tag`,
//! `batch_suggest_tags_review`) paths share a [`SuggestContext`] — model
//! guard, encoded labels, vocabulary, threshold, online volumes are hoisted
//! once and per-asset work loops on [`SuggestContext::suggestions_for`].

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;

use crate::web::AppState;

// --- AI auto-tag endpoints ---

#[derive(Debug, Clone, serde::Serialize)]
pub struct SuggestTagsResponse {
    /// Hierarchical tag MAKI would apply when this suggestion is accepted.
    pub tag: String,
    pub confidence: f32,
    pub existing: bool,
    /// When the active vocabulary mapped a non-identity label → tag,
    /// the original label the SigLIP model actually picked. Surfaced
    /// as a small "(from: sunset)" subtitle in the dropdown so the
    /// user can see what the AI classified the image as underneath
    /// the hierarchical tag. `None` for identity mappings.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_label: Option<String>,
}

/// Per-batch context for suggest-tags work.
///
/// Hoisted once across whatever batch of asset IDs the caller wants to
/// process. Holds the SigLIP model guard, the encoded label embeddings, the
/// active vocabulary, the threshold, and the online-volume map. Per-asset
/// work goes through [`SuggestContext::suggestions_for`].
///
/// Three callers share this:
///
/// - [`suggest_tags`] — single asset, builds a context, calls
///   `suggestions_for` once.
/// - [`batch_auto_tag`] — many assets, builds a context, loops
///   `suggestions_for` and applies non-`existing` suggestions.
/// - `batch_suggest_tags_review` (separate module) — many assets, builds a
///   context, loops `suggestions_for`, aggregates the suggestions into an
///   inverted index without applying anything.
pub(crate) struct SuggestContext<'a> {
    state: &'a AppState,
    engine: crate::query::QueryEngine,
    service: crate::asset_service::AssetService,
    preview_gen: crate::preview::PreviewGenerator,
    volumes: Vec<crate::models::Volume>,
    model_guard: tokio::sync::MutexGuard<'a, Option<crate::ai::SigLipModel>>,
    label_list: Vec<String>,
    label_embs: Vec<Vec<f32>>,
    vocabulary: crate::ai_vocabulary::Vocabulary,
    threshold: f32,
    verbose: bool,
}

impl<'a> SuggestContext<'a> {
    /// Build a fresh context for the given AppState.
    ///
    /// Lazily loads the SigLIP model into `state.ai_model` if not already
    /// cached, and encodes the active vocabulary labels (or reuses the
    /// `ai_label_cache` if the resolved labels match).
    pub(crate) fn new(state: &'a AppState) -> Result<Self, String> {
        Self::new_with_threshold(state, state.ai_config.threshold as f32)
    }

    /// Same as [`Self::new`] but with an explicit confidence threshold —
    /// the review job runs lower than `ai_config.threshold` so the UI
    /// slider has room to drag both ways.
    pub(crate) fn new_with_threshold(
        state: &'a AppState,
        threshold: f32,
    ) -> Result<Self, String> {
        use crate::ai;
        use crate::device_registry::DeviceRegistry;

        let verbose = state.verbosity.verbose();
        let t_start = std::time::Instant::now();
        macro_rules! vlog {
            ($($t:tt)*) => {
                if verbose {
                    eprintln!("[suggest-ctx] {}", format!($($t)*));
                }
            };
        }

        let engine = state.query_engine();
        let preview_gen = state.preview_generator();
        let service = state.asset_service();

        let registry = DeviceRegistry::new(&state.catalog_root);
        let volumes = registry.list().map_err(|e| format!("{e:#}"))?;

        let model_dir = super::resolve_model_dir(&state.ai_config);
        let model_id = state.ai_config.model.clone();
        vlog!("acquiring model lock…");
        let t_lock = std::time::Instant::now();
        let mut model_guard = state.ai_model.blocking_lock();
        vlog!("model lock acquired ({:?})", t_lock.elapsed());
        if model_guard.is_none() {
            vlog!("loading SigLIP model: {model_id} (provider={})", state.ai_config.execution_provider);
            let t_load = std::time::Instant::now();
            let m = ai::SigLipModel::load_with_provider(&model_dir, &model_id, state.verbosity, &state.ai_config.execution_provider)
                .map_err(|e| format!("Failed to load AI model: {e:#}"))?;
            vlog!("model loaded ({:?})", t_load.elapsed());
            *model_guard = Some(m);
        }

        let vocabulary = super::resolve_vocabulary(&state.ai_config)?;
        // Cache hit requires the cached label list to match the currently-
        // resolved labels exactly. If the user edits `[ai].labels` (or the
        // YAML file behind it) between requests, the stale embeddings can't
        // be reused — the indices wouldn't line up with the new labels.
        let cached_match = {
            let guard = state.ai_label_cache.blocking_read();
            guard.as_ref().is_some_and(|(l, _)| l.as_slice() == vocabulary.labels.as_slice())
        };

        let (label_list, label_embs) = if cached_match {
            let guard = state.ai_label_cache.blocking_read();
            let (l, e) = guard.as_ref().unwrap();
            (l.clone(), e.clone())
        } else {
            vlog!("encoding {} labels (cold cache)…", vocabulary.labels.len());
            let t_enc = std::time::Instant::now();
            let prompt_template = &state.ai_config.prompt;
            let prompted: Vec<String> = vocabulary
                .labels
                .iter()
                .map(|l| ai::apply_prompt_template(prompt_template, l))
                .collect();
            let model = model_guard.as_mut().unwrap();
            let embs = model
                .encode_texts(&prompted)
                .map_err(|e| format!("Failed to encode labels: {e:#}"))?;
            vlog!("labels encoded ({:?})", t_enc.elapsed());
            let mut guard = state.ai_label_cache.blocking_write();
            *guard = Some((vocabulary.labels.clone(), embs.clone()));
            (vocabulary.labels.clone(), embs)
        };

        vlog!("context ready ({:?})", t_start.elapsed());

        Ok(SuggestContext {
            state,
            engine,
            service,
            preview_gen,
            volumes,
            model_guard,
            label_list,
            label_embs,
            vocabulary,
            threshold,
            verbose,
        })
    }

    /// Map of online volume id → volume reference, freshly computed for the
    /// caller. Cheap (a handful of entries, no I/O); recomputing per asset
    /// dodges the self-referential-struct headache of caching it alongside
    /// the owning `volumes` Vec.
    fn online_volumes(&self) -> std::collections::HashMap<String, &crate::models::Volume> {
        self.volumes
            .iter()
            .filter(|v| v.is_online)
            .map(|v| (v.id.to_string(), v))
            .collect()
    }

    /// Compute tag suggestions for a single asset, mirroring the side
    /// effects of the historical `suggest_tags_inner`:
    ///
    /// - Resolves the best image for AI via [`AssetService::find_image_for_ai`].
    /// - Encodes the image into a SigLIP embedding.
    /// - Persists that embedding to the catalog, the on-disk binary, and
    ///   the in-memory similarity index (best-effort; failures swallowed).
    /// - Classifies against the cached label embeddings.
    /// - Applies vocabulary mapping (fan-out + max-confidence dedup).
    /// - Flags each suggestion's `existing` against `details.tags`.
    pub(crate) fn suggestions_for(
        &mut self,
        asset_id: &str,
    ) -> Result<Vec<SuggestTagsResponse>, String> {
        self.suggestions_with_meta_for(asset_id).map(|(s, _)| s)
    }

    /// Same as [`Self::suggestions_for`] but also returns lightweight
    /// metadata (preview URL + asset name) that callers building a UI on
    /// top of the suggestions can use without re-fetching `AssetDetails`.
    /// Both are derived from the same `engine.show()` call, so this costs
    /// nothing extra over the suggestion path.
    pub(crate) fn suggestions_with_meta_for(
        &mut self,
        asset_id: &str,
    ) -> Result<(Vec<SuggestTagsResponse>, AssetCardMeta), String> {
        use crate::ai;

        let t_start = std::time::Instant::now();
        let verbose = self.verbose;
        macro_rules! vlog {
            ($aid:expr, $($t:tt)*) => {
                if verbose {
                    eprintln!("[suggest-tags {}] {}",
                        &$aid[..8.min($aid.len())],
                        format!($($t)*));
                }
            };
        }

        let details = self.engine.show(asset_id).map_err(|e| format!("{e:#}"))?;

        let online_volumes = self.online_volumes();
        let image_path = self.service
            .find_image_for_ai(&details, &self.preview_gen, &online_volumes)
            .ok_or_else(|| "No processable image found for this asset".to_string())?;
        vlog!(asset_id, "image resolved: {} ({:?})", image_path.display(), t_start.elapsed());

        let ext = image_path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("");
        if !ai::is_supported_image(ext) {
            return Err(format!("Unsupported image format: {ext}"));
        }

        let model = self.model_guard.as_mut().expect("model loaded in SuggestContext::new");

        vlog!(asset_id, "encoding image…");
        let t_img = std::time::Instant::now();
        let image_emb = model
            .encode_image(&image_path)
            .map_err(|e| format!("Failed to encode image: {e:#}"))?;
        vlog!(asset_id, "image encoded ({:?})", t_img.elapsed());

        // Persist the embedding everywhere downstream consumers expect it.
        // Best-effort: failures here don't fail the suggestion request.
        let model_id = self.state.ai_config.model.clone();
        {
            let catalog = crate::catalog::Catalog::open_fast(&self.state.catalog_root);
            if let Ok(catalog) = catalog {
                let _ = crate::embedding_store::EmbeddingStore::initialize(catalog.conn());
                let emb_store = crate::embedding_store::EmbeddingStore::new(catalog.conn());
                let _ = emb_store.store(asset_id, &image_emb, &model_id);
            }
            let _ = crate::embedding_store::write_embedding_binary(&self.state.catalog_root, &model_id, asset_id, &image_emb);
            if let Ok(mut idx_guard) = self.state.ai_embedding_index.write() {
                if let Some(ref mut idx) = *idx_guard {
                    idx.upsert(asset_id, &image_emb);
                }
            }
        }

        let flat_suggestions = model.classify(&image_emb, &self.label_list, &self.label_embs, self.threshold);
        // Fan out into hierarchical tags + dedup by max confidence.
        let mapped = self.vocabulary.apply(flat_suggestions);

        let existing: std::collections::HashSet<String> = details
            .tags
            .iter()
            .map(|t| t.to_lowercase())
            .collect();

        let result: Vec<SuggestTagsResponse> = mapped
            .into_iter()
            .map(|s| SuggestTagsResponse {
                existing: existing.contains(&s.tag.to_lowercase()),
                tag: s.tag,
                confidence: s.confidence,
                source_label: s.source_label,
            })
            .collect();

        vlog!(asset_id, "done: {} suggestions, total {:?}", result.len(), t_start.elapsed());

        // Pick the best-preview variant for the candidate thumbnail. Prefer
        // a `primary` role variant; fall back to the first one. Preview
        // files share the on-disk extension MAKI generates (from
        // `state.preview_ext`, derived from [preview] format). Empty
        // preview URL means "no preview available"; the modal renders a
        // placeholder cell rather than failing the suggestion.
        let meta = build_card_meta(&details, &self.state.preview_ext);

        Ok((result, meta))
    }
}

/// Minimal asset metadata used by review-flow UI to render a candidate
/// thumbnail without an extra API round-trip. Populated by
/// [`SuggestContext::suggestions_with_meta_for`].
#[derive(Debug, Clone)]
pub(crate) struct AssetCardMeta {
    pub preview_url: String,
    pub name: String,
}

fn build_card_meta(
    details: &crate::catalog::AssetDetails,
    preview_ext: &str,
) -> AssetCardMeta {
    let variant = details
        .variants
        .iter()
        .find(|v| v.role == "primary")
        .or_else(|| details.variants.first());
    let preview_url = variant
        .map(|v| crate::web::templates::preview_url(&v.content_hash, preview_ext))
        .unwrap_or_default();
    let name = details
        .name
        .clone()
        .or_else(|| variant.map(|v| v.original_filename.clone()))
        .unwrap_or_default();
    AssetCardMeta { preview_url, name }
}

/// POST /api/asset/{id}/suggest-tags — suggest tags for an asset using AI.
pub async fn suggest_tags(
    State(state): State<Arc<AppState>>,
    Path(asset_id): Path<String>,
) -> Response {
    let state = state.clone();
    let result: Result<Result<Vec<SuggestTagsResponse>, String>, _> =
        tokio::task::spawn_blocking(move || -> Result<Vec<SuggestTagsResponse>, String> {
            let mut ctx = SuggestContext::new(&state)?;
            ctx.suggestions_for(&asset_id)
        })
        .await;

    match result {
        Ok(Ok(suggestions)) => Json(suggestions).into_response(),
        Ok(Err(msg)) => (StatusCode::INTERNAL_SERVER_ERROR, msg).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, format!("Error: {e}")).into_response(),
    }
}

#[derive(Debug, serde::Deserialize)]
pub struct BatchAutoTagRequest {
    pub asset_ids: Vec<String>,
}

#[derive(Debug, serde::Serialize)]
pub struct BatchAutoTagResponse {
    pub succeeded: u32,
    pub failed: u32,
    pub tags_applied: u32,
    pub errors: Vec<String>,
}

/// POST /api/batch/auto-tag — start an auto-tag job for selected assets.
///
/// Returns `{job_id}` immediately. Progress flows through `/api/jobs/{id}/progress`;
/// the terminal event carries `{succeeded, failed, tags_applied, errors, done: true}`.
pub async fn batch_auto_tag(
    State(state): State<Arc<AppState>>,
    Json(req): Json<BatchAutoTagRequest>,
) -> Response {
    use crate::web::jobs::JobKind;

    let job = state.jobs.start(JobKind::AutoTag);
    let job_id = job.id.clone();
    let total = req.asset_ids.len();
    job.emit(&serde_json::json!({
        "phase": "auto_tag",
        "done": false,
        "processed": 0,
        "total": total,
        "status": "starting",
    }));

    let state2 = state.clone();
    let job_for_task = job.clone();
    tokio::spawn(async move {
        let log = state2.log_requests;
        let job_inner = job_for_task.clone();
        let state_for_blocking = state2.clone();
        let result = tokio::task::spawn_blocking(move || {
            batch_auto_tag_inner(&state_for_blocking, req.asset_ids, &job_inner, total)
        })
        .await;

        let terminal = match result {
            Ok(Ok(resp)) => {
                if log {
                    eprintln!(
                        "batch_auto_tag: {} assets ({} ok, {} err, {} tags)",
                        total, resp.succeeded, resp.failed, resp.tags_applied
                    );
                }
                if resp.succeeded > 0 {
                    state2.dropdown_cache.invalidate_tags();
                }
                serde_json::json!({
                    "phase": "auto_tag",
                    "succeeded": resp.succeeded,
                    "failed": resp.failed,
                    "tags_applied": resp.tags_applied,
                    "errors": resp.errors,
                })
            }
            Ok(Err(msg)) => serde_json::json!({"phase": "auto_tag", "error": msg}),
            Err(e) => serde_json::json!({"phase": "auto_tag", "error": format!("{e}")}),
        };
        job_for_task.finish(terminal);
        state2.jobs.mark_done(&job_for_task.id);
    });

    Json(serde_json::json!({"job_id": job_id, "status": "started"})).into_response()
}

fn batch_auto_tag_inner(
    state: &AppState,
    asset_ids: Vec<String>,
    job: &std::sync::Arc<crate::web::jobs::Job>,
    total: usize,
) -> Result<BatchAutoTagResponse, String> {
    let mut ctx = SuggestContext::new(state)?;

    let mut resp = BatchAutoTagResponse {
        succeeded: 0,
        failed: 0,
        tags_applied: 0,
        errors: Vec::new(),
    };

    let emit_progress = |processed: usize, aid: &str, status: &str, tags_applied: u32| {
        let short = &aid[..8.min(aid.len())];
        job.emit(&serde_json::json!({
            "phase": "auto_tag",
            "done": false,
            "processed": processed,
            "total": total,
            "asset": short,
            "status": status,
            "tags_applied": tags_applied,
        }));
    };

    let mut processed: usize = 0;
    for aid in &asset_ids {
        processed += 1;

        let suggestions = match ctx.suggestions_for(aid) {
            Ok(s) => s,
            Err(e) => {
                // "No processable image" and "Unsupported image format" are
                // soft skips in the historical path; everything else (asset
                // not found, encode failure) is an error.
                let is_skip = e.starts_with("No processable image")
                    || e.starts_with("Unsupported image format");
                if is_skip {
                    emit_progress(processed, aid, "skipped", resp.tags_applied);
                } else {
                    resp.failed += 1;
                    resp.errors.push(format!("{}: {e}", &aid[..8.min(aid.len())]));
                    emit_progress(processed, aid, "error", resp.tags_applied);
                }
                continue;
            }
        };

        let new_tags: Vec<String> = suggestions
            .into_iter()
            .filter(|s| !s.existing)
            .map(|s| s.tag)
            .collect();

        if new_tags.is_empty() {
            resp.succeeded += 1;
            emit_progress(processed, aid, "no-new-tags", resp.tags_applied);
            continue;
        }

        match ctx.engine.tag(aid, &new_tags, false) {
            Ok(_) => {
                resp.tags_applied += new_tags.len() as u32;
                resp.succeeded += 1;
                emit_progress(processed, aid, "tagged", resp.tags_applied);
            }
            Err(e) => {
                resp.failed += 1;
                resp.errors.push(format!("{}: {e:#}", &aid[..8.min(aid.len())]));
                emit_progress(processed, aid, "error", resp.tags_applied);
            }
        }
    }

    Ok(resp)
}
