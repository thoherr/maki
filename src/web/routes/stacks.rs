//! Stack operations: split, create, add, unstack, delete, pick, dissolve, similarity-based stacking.

use std::sync::Arc;

use axum::extract::{Path, State};
#[cfg(feature = "ai")]
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;

use crate::web::AppState;

// --- Split operation ---

#[derive(serde::Deserialize)]
pub struct SplitRequest {
    pub variant_hashes: Vec<String>,
}

/// POST /api/asset/{id}/split — extract variants into new assets.
pub async fn split_asset(
    State(state): State<Arc<AppState>>,
    Path(asset_id): Path<String>,
    Json(req): Json<SplitRequest>,
) -> Result<Response, Response> {
    let state = state.clone();
    let json = super::spawn_catalog_blocking(move || {
        let engine = state.query_engine()?;
        let result = engine.split(&asset_id, &req.variant_hashes)?;
        Ok::<_, anyhow::Error>(serde_json::json!({
            "source_id": result.source_id,
            "new_assets": result.new_assets,
        }))
    })
    .await?;
    Ok(Json(json).into_response())
}

// --- Stack batch operations ---

#[derive(serde::Deserialize)]
pub struct BatchStackRequest {
    pub asset_ids: Vec<String>,
}

/// POST /api/batch/stack — create a stack from selected assets.
pub async fn batch_create_stack(
    State(state): State<Arc<AppState>>,
    Json(req): Json<BatchStackRequest>,
) -> Result<Response, Response> {
    let state = state.clone();
    let json = super::spawn_blocking_with(move || {
        let catalog = state.catalog()?;
        let store = crate::stack::StackStore::open(catalog.conn(), &state.catalog_root);
        let stack = store.create(&req.asset_ids)?;
        Ok::<_, anyhow::Error>(serde_json::json!({
            "stack_id": stack.id.to_string(),
            "member_count": stack.asset_ids.len(),
        }))
    }, super::err::mapped)
    .await?;
    Ok(Json(json).into_response())
}

/// POST /api/asset/{id}/stack-add — add this asset to an existing stack.
pub async fn add_to_stack(
    State(state): State<Arc<AppState>>,
    Path(reference_id): Path<String>,
    Json(req): Json<BatchStackRequest>,
) -> Result<Response, Response> {
    let state = state.clone();
    let json = super::spawn_blocking_with(move || {
        let catalog = state.catalog()?;
        let ref_full = super::resolve_asset_id_or_err(&catalog, &reference_id)?;
        let store = crate::stack::StackStore::open(catalog.conn(), &state.catalog_root);
        let added = store.add(&ref_full, &req.asset_ids)?;
        Ok::<_, anyhow::Error>(serde_json::json!({
            "added": added,
        }))
    }, super::err::mapped)
    .await?;
    Ok(Json(json).into_response())
}

/// DELETE /api/batch/stack — unstack selected assets.
pub async fn batch_unstack(
    State(state): State<Arc<AppState>>,
    Json(req): Json<BatchStackRequest>,
) -> Result<Response, Response> {
    let state = state.clone();
    let json = super::spawn_catalog_blocking(move || {
        let catalog = state.catalog()?;
        let store = crate::stack::StackStore::open(catalog.conn(), &state.catalog_root);
        let removed = store.remove(&req.asset_ids)?;
        Ok::<_, anyhow::Error>(serde_json::json!({
            "removed": removed,
        }))
    })
    .await?;
    Ok(Json(json).into_response())
}

/// POST /api/batch/delete — delete selected assets from catalog.
#[derive(serde::Deserialize)]
pub struct BatchDeleteRequest {
    pub asset_ids: Vec<String>,
    #[serde(default)]
    pub remove_files: bool,
}

pub async fn batch_delete(
    State(state): State<Arc<AppState>>,
    Json(req): Json<BatchDeleteRequest>,
) -> Result<Response, Response> {
    let catalog_root = state.catalog_root.clone();
    let json = super::spawn_catalog_blocking(move || {
        let service = crate::asset_service::AssetService::with_config(&catalog_root, state.verbosity, state.config());
        // no_trash = false: web batch delete always honors [trash] config
        let result = service.delete_assets(&req.asset_ids, true, req.remove_files, false, |_id, _status, _elapsed| {})?;
        Ok::<_, anyhow::Error>(serde_json::json!({
            "deleted": result.deleted,
            "files_removed": result.files_removed,
            "files_trashed": result.files_trashed,
            "previews_removed": result.previews_removed,
            "not_found": result.not_found,
            "errors": result.errors,
        }))
    })
    .await?;
    Ok(Json(json).into_response())
}

/// PUT /api/asset/{id}/stack-pick — set this asset as the stack pick.
pub async fn set_stack_pick(
    State(state): State<Arc<AppState>>,
    Path(asset_id): Path<String>,
) -> Result<Response, Response> {
    let state = state.clone();
    let json = super::spawn_blocking_with(move || {
        let catalog = state.catalog()?;
        let store = crate::stack::StackStore::open(catalog.conn(), &state.catalog_root);
        store.set_pick(&asset_id)?;
        Ok::<_, anyhow::Error>(serde_json::json!({ "pick": asset_id }))
    }, super::err::mapped)
    .await?;
    Ok(Json(json).into_response())
}

/// DELETE /api/asset/{id}/stack — dissolve the stack this asset belongs to.
pub async fn dissolve_stack(
    State(state): State<Arc<AppState>>,
    Path(asset_id): Path<String>,
) -> Result<Response, Response> {
    let state = state.clone();
    let json = super::spawn_blocking_with(move || {
        let catalog = state.catalog()?;
        let store = crate::stack::StackStore::open(catalog.conn(), &state.catalog_root);
        store.dissolve(&asset_id)?;
        Ok::<_, anyhow::Error>(serde_json::json!({ "status": "dissolved" }))
    }, super::err::mapped)
    .await?;
    Ok(Json(json).into_response())
}

/// POST /api/asset/{id}/stack-similar — stack visually similar assets (AI).
#[cfg(feature = "ai")]
pub async fn stack_by_similarity(
    State(state): State<Arc<AppState>>,
    Path(asset_id): Path<String>,
    Json(params): Json<StackSimilarRequest>,
) -> Result<Response, Response> {
    let state = state.clone();
    let json = super::spawn_blocking_with(move || {
        let catalog = state.catalog()?;
        let full_id = super::resolve_asset_id_or_err(&catalog, &asset_id)?;

        let store = crate::stack::StackStore::open(catalog.conn(), &state.catalog_root);
        if store.stack_for_asset(&full_id)?.is_some() {
            anyhow::bail!("asset is already in a stack. Dissolve it first.");
        }

        let model_id = &state.ai_config.model;
        if crate::ai::get_model_spec(model_id).is_none() {
            anyhow::bail!("aI model not configured");
        }

        let emb_store = crate::embedding_store::EmbeddingStore::new(catalog.conn());
        let query_emb = emb_store
            .get(&full_id, model_id)?
            .ok_or_else(|| anyhow::anyhow!(
                "No embedding for this asset. Run `maki embed --asset {}` first.", &full_id
            ))?;

        let threshold = params.threshold.unwrap_or(85.0).clamp(0.0, 100.0) / 100.0;
        let limit = params.limit.unwrap_or(40);

        let results = state
            .embedding_index(&catalog)
            .map_err(|_| anyhow::anyhow!("embedding index not available"))?
            .search(&query_emb, limit, Some(&full_id));

        let mut stack_ids: Vec<String> = vec![full_id.clone()];
        for (id, sim) in &results {
            if *sim >= threshold && store.stack_for_asset(id)?.is_none() {
                stack_ids.push(id.clone());
            }
        }

        if stack_ids.len() < 2 {
            anyhow::bail!("no similar assets found above {}% threshold", (threshold * 100.0) as u32);
        }

        let stack = store.create(&stack_ids)?;

        Ok(serde_json::json!({
            "stack_id": stack.id.to_string(),
            "member_count": stack_ids.len(),
            "threshold": (threshold * 100.0) as u32,
        }))
    }, |e| (StatusCode::BAD_REQUEST, Json(serde_json::json!({ "error": format!("{e:#}") }))).into_response())
    .await?;
    Ok(Json(json).into_response())
}

#[cfg(feature = "ai")]
#[derive(Debug, serde::Deserialize)]
pub struct StackSimilarRequest {
    pub threshold: Option<f32>,
    pub limit: Option<usize>,
}

/// GET /api/stack/{id}/members — return stack member cards as JSON.
pub async fn stack_members_api(
    State(state): State<Arc<AppState>>,
    Path(stack_id): Path<String>,
) -> Result<Response, Response> {
    let state = state.clone();
    let json = super::spawn_blocking_with(move || {
        let catalog = state.catalog()?;
        let store = crate::stack::StackStore::new(catalog.conn());
        let member_ids = store.ordered_members(&stack_id)?;
        let preview_ext = &state.preview_ext;

        let mut members = Vec::new();
        for mid in &member_ids {
            let row = catalog.get_search_row(mid);
            if let Ok(Some(r)) = row {
                members.push(serde_json::json!({
                    "asset_id": r.asset_id,
                    "name": r.name.as_deref().unwrap_or(&r.original_filename),
                    "asset_type": r.asset_type,
                    "format": r.primary_format.as_deref().unwrap_or(&r.format),
                    "date": crate::web::templates::format_date(&r.created_at),
                    "preview_url": crate::web::templates::preview_url(&r.content_hash, preview_ext),
                    "rating": r.rating.unwrap_or(0),
                    "label": r.color_label.as_deref().unwrap_or(""),
                    "variant_count": r.variant_count,
                    "stack_id": r.stack_id,
                    "stack_count": r.stack_count,
                    "preview_rotation": r.preview_rotation.unwrap_or(0),
                    "face_count": r.face_count,
                }));
            }
        }
        Ok::<_, anyhow::Error>(serde_json::json!(members))
    }, super::err::mapped)
    .await?;
    Ok(Json(json).into_response())
}
