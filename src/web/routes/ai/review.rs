//! `suggest-tags-review` endpoint: batch suggest-tags pass that aggregates
//! per-asset suggestions into an inverted index (`tag → candidates`) for
//! the review-modal UI.
//!
//! Distinct from `batch_auto_tag` because no tags are applied — the user
//! picks per-tag candidate sets after the job completes, then commits via
//! `POST /api/batch/apply-tags`. The aggregated payload is stashed on
//! `Job.set_result` and fetched by the modal via
//! `GET /api/jobs/{id}/result`.

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;

use crate::web::jobs::JobKind;
use crate::web::AppState;

use super::tags::{AssetCardMeta, SuggestContext};

#[derive(Debug, serde::Deserialize)]
pub struct SuggestTagsReviewRequest {
    pub asset_ids: Vec<String>,
    /// Override the collection-pass threshold. Defaults to
    /// `ai_config.threshold * 0.5` so the review-modal slider has range
    /// both above and below the user's normal auto-tag threshold.
    #[serde(default)]
    pub threshold: Option<f32>,
}

/// POST /api/maintain/suggest-tags-review — start a tag-suggestion review job.
///
/// Returns `{job_id}` immediately. Live progress flows through
/// `/api/jobs/{id}/progress`; the structured result payload (the inverted
/// `tag → candidates` index) is delivered separately via
/// `/api/jobs/{id}/result` once the job completes.
pub async fn start_suggest_tags_review_api(
    State(state): State<Arc<AppState>>,
    Json(req): Json<SuggestTagsReviewRequest>,
) -> Response {
    if let Some(latest) = state.jobs.latest(JobKind::SuggestTagsReview) {
        if !latest.is_completed() {
            return (StatusCode::CONFLICT, "A suggest-tags-review job is already running").into_response();
        }
    }

    let job = state.jobs.start(JobKind::SuggestTagsReview);
    let job_id = job.id.clone();
    let total = req.asset_ids.len();
    job.emit(&serde_json::json!({
        "phase": "suggest_tags_review",
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
            run_review(&state_for_blocking, req, &job_inner, total)
        })
        .await;

        let terminal = match result {
            Ok(Ok(payload)) => {
                let summary = serde_json::json!({
                    "phase": "suggest_tags_review",
                    "processed": total,
                    "total": total,
                    "tag_count": payload.get("tags").and_then(|t| t.as_array()).map(|a| a.len()).unwrap_or(0),
                });
                if log {
                    eprintln!(
                        "suggest_tags_review: {} assets, {} distinct tags",
                        total,
                        summary["tag_count"].as_u64().unwrap_or(0)
                    );
                }
                job_for_task.set_result(payload);
                summary
            }
            Ok(Err(msg)) => serde_json::json!({"phase": "suggest_tags_review", "error": msg}),
            Err(e) => serde_json::json!({"phase": "suggest_tags_review", "error": format!("{e}")}),
        };
        job_for_task.finish(terminal);
        state2.jobs.mark_done(&job_for_task.id);
    });

    Json(serde_json::json!({"job_id": job_id, "status": "started"})).into_response()
}

fn run_review(
    state: &AppState,
    req: SuggestTagsReviewRequest,
    job: &Arc<crate::web::jobs::Job>,
    total: usize,
) -> Result<serde_json::Value, String> {
    // Run at half the user's normal threshold by default so the review
    // modal's confidence slider has room to drag *below* what auto-tag
    // would have applied — that's the whole point of the review pass.
    let threshold = req.threshold.unwrap_or_else(|| {
        let normal = state.ai_config.threshold as f32;
        (normal * 0.5).max(0.05)
    });

    let mut ctx = SuggestContext::new_with_threshold(state, threshold)?;

    // `BTreeMap` keeps tags in a stable order in the per_asset payload;
    // we re-sort tags at the end by frequency for the left-pane list.
    let mut tag_index: BTreeMap<String, TagBucket> = BTreeMap::new();
    let mut per_asset: BTreeMap<String, Vec<serde_json::Value>> = BTreeMap::new();
    // Per-asset preview URL + filename, used by the review modal to render
    // candidate thumbnails without a second round-trip per asset.
    let mut asset_meta: BTreeMap<String, AssetCardMeta> = BTreeMap::new();
    let mut skipped: u32 = 0;
    let mut errors: Vec<String> = Vec::new();

    let emit_progress = |processed: usize, aid: &str, status: &str| {
        let short = &aid[..8.min(aid.len())];
        job.emit(&serde_json::json!({
            "phase": "suggest_tags_review",
            "done": false,
            "processed": processed,
            "total": total,
            "asset": short,
            "status": status,
        }));
    };

    let mut processed: usize = 0;
    for aid in &req.asset_ids {
        processed += 1;

        let (suggestions, meta) = match ctx.suggestions_with_meta_for(aid) {
            Ok(s) => s,
            Err(e) => {
                let is_skip = e.starts_with("No processable image")
                    || e.starts_with("Unsupported image format");
                if is_skip {
                    skipped += 1;
                    emit_progress(processed, aid, "skipped");
                } else {
                    errors.push(format!("{}: {e}", &aid[..8.min(aid.len())]));
                    emit_progress(processed, aid, "error");
                }
                continue;
            }
        };

        let mut per_asset_entry: Vec<serde_json::Value> = Vec::with_capacity(suggestions.len());
        for s in &suggestions {
            let bucket = tag_index.entry(s.tag.clone()).or_default();
            bucket.push(aid, s.confidence, s.source_label.as_deref(), s.existing);
            per_asset_entry.push(serde_json::json!({
                "tag": s.tag,
                "confidence": s.confidence,
                "existing": s.existing,
                "source_label": s.source_label,
            }));
        }
        per_asset.insert(aid.clone(), per_asset_entry);
        asset_meta.insert(aid.clone(), meta);
        emit_progress(processed, aid, "ok");
    }

    // Sort tags by descending asset_count, ties broken by mean_confidence.
    let mut tag_rows: Vec<serde_json::Value> = tag_index
        .into_iter()
        .map(|(tag, bucket)| bucket.to_json(tag))
        .collect();
    tag_rows.sort_by(|a, b| {
        let ac = a["asset_count"].as_u64().unwrap_or(0);
        let bc = b["asset_count"].as_u64().unwrap_or(0);
        bc.cmp(&ac).then_with(|| {
            let am = a["mean_confidence"].as_f64().unwrap_or(0.0);
            let bm = b["mean_confidence"].as_f64().unwrap_or(0.0);
            bm.partial_cmp(&am).unwrap_or(std::cmp::Ordering::Equal)
        })
    });

    // Serialize asset_meta to plain JSON (BTreeMap<String, {preview_url, name}>)
    // so the modal can render thumbnails by asset_id lookup.
    let assets_json: serde_json::Value = asset_meta
        .into_iter()
        .map(|(id, m)| (id, serde_json::json!({"preview_url": m.preview_url, "name": m.name})))
        .collect::<serde_json::Map<_, _>>()
        .into();

    Ok(serde_json::json!({
        "asset_count": total,
        "processed": processed,
        "skipped": skipped,
        "errors": errors,
        "threshold_used": threshold,
        "tags": tag_rows,
        "per_asset": per_asset,
        "assets": assets_json,
    }))
}

/// Per-tag accumulator. Holds candidates plus running totals so we can
/// emit the final `tags[]` row in one pass without a second iteration.
#[derive(Default)]
struct TagBucket {
    candidates: Vec<serde_json::Value>,
    confidence_sum: f64,
    max_confidence: f32,
}

impl TagBucket {
    fn push(&mut self, asset_id: &str, confidence: f32, source_label: Option<&str>, existing: bool) {
        self.confidence_sum += confidence as f64;
        if confidence > self.max_confidence {
            self.max_confidence = confidence;
        }
        self.candidates.push(serde_json::json!({
            "asset_id": asset_id,
            "confidence": confidence,
            "source_label": source_label,
            "existing": existing,
        }));
    }

    fn to_json(self, tag: String) -> serde_json::Value {
        let n = self.candidates.len();
        let mean = if n > 0 { self.confidence_sum / n as f64 } else { 0.0 };
        serde_json::json!({
            "tag": tag,
            "asset_count": n,
            "mean_confidence": mean,
            "max_confidence": self.max_confidence,
            "candidates": self.candidates,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tag_bucket_tracks_max_and_mean_confidence() {
        let mut b = TagBucket::default();
        b.push("a1", 0.4, Some("car"), false);
        b.push("a2", 0.8, None, true);
        b.push("a3", 0.6, None, false);
        let json = b.to_json("subject|vehicle|car".to_string());
        assert_eq!(json["asset_count"], 3);
        // Mean = (0.4 + 0.8 + 0.6) / 3 = 0.6 ± float noise.
        let mean = json["mean_confidence"].as_f64().unwrap();
        assert!((mean - 0.6).abs() < 1e-6, "mean={mean}");
        let max = json["max_confidence"].as_f64().unwrap();
        assert!((max - 0.8).abs() < 1e-6, "max={max}");
        assert_eq!(json["candidates"].as_array().unwrap().len(), 3);
        // Source labels preserved when supplied; null otherwise.
        assert_eq!(json["candidates"][0]["source_label"], "car");
        assert!(json["candidates"][1]["source_label"].is_null());
        // Existing flag preserved per-candidate.
        assert_eq!(json["candidates"][1]["existing"], true);
    }

    #[test]
    fn tag_bucket_empty_renders_zero_mean() {
        let b = TagBucket::default();
        let json = b.to_json("empty".to_string());
        assert_eq!(json["asset_count"], 0);
        assert_eq!(json["mean_confidence"], 0.0);
        assert_eq!(json["max_confidence"], 0.0);
        assert_eq!(json["candidates"].as_array().unwrap().len(), 0);
    }
}
