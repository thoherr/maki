//! Collections and batch-group/auto-group routes.

use std::sync::Arc;

use askama::Template;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Response};
use axum::Json;

use crate::web::AppState;

/// GET /collections — collections HTML page.
pub async fn collections_page(State(state): State<Arc<AppState>>) -> Result<Response, Response> {
    let state = state.clone();
    let html = super::spawn_catalog_blocking(move || {
        let catalog = state.catalog()?;
        let col_store = crate::collection::CollectionStore::new(catalog.conn());
        let collections = col_store.list()?;
        let tmpl = crate::web::templates::CollectionsPage {
            collections,
            ai_enabled: state.ai_enabled,
            vlm_enabled: state.vlm_enabled,
        };
        Ok::<_, anyhow::Error>(tmpl.render()?)
    })
    .await?;
    Ok(Html(html).into_response())
}

#[derive(Debug, serde::Deserialize)]
pub struct CreateCollectionRequest {
    pub name: String,
    pub description: Option<String>,
}

/// POST /api/collections — create a new collection.
pub async fn create_collection_api(
    State(state): State<Arc<AppState>>,
    Json(req): Json<CreateCollectionRequest>,
) -> Response {
    let state = state.clone();
    let result = tokio::task::spawn_blocking(move || {
        let name = req.name.trim().to_string();
        if name.is_empty() {
            anyhow::bail!("collection name cannot be empty");
        }
        let description = req.description.as_deref().map(|s| s.trim()).filter(|s| !s.is_empty());
        let catalog = state.catalog()?;
        let col_store = crate::collection::CollectionStore::new(catalog.conn());
        let collection = col_store.create(&name, description)?;
        let yaml = col_store.export_all()?;
        crate::collection::save_yaml(&state.catalog_root, &yaml)?;
        state.dropdown_cache.invalidate_collections();
        Ok::<_, anyhow::Error>(serde_json::json!({
            "id": collection.id.to_string(),
            "name": collection.name,
            "description": collection.description,
        }))
    })
    .await;

    match result {
        Ok(Ok(json)) => (StatusCode::CREATED, Json(json)).into_response(),
        Ok(Err(e)) => {
            let msg = format!("{e:#}");
            if msg.contains("UNIQUE constraint") || msg.contains("already exists") {
                (StatusCode::CONFLICT, format!("Collection already exists: {msg}")).into_response()
            } else {
                (StatusCode::INTERNAL_SERVER_ERROR, format!("Error: {msg}")).into_response()
            }
        }
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, format!("Error: {e}")).into_response(),
    }
}

/// GET /api/collections — list all collections as JSON.
pub async fn list_collections_api(State(state): State<Arc<AppState>>) -> Result<Response, Response> {
    let state = state.clone();
    let collections = super::spawn_catalog_blocking(move || {
        let catalog = state.catalog()?;
        let col_store = crate::collection::CollectionStore::new(catalog.conn());
        let collections = col_store.list()?;
        Ok::<_, anyhow::Error>(collections)
    })
    .await?;
    Ok(Json(collections).into_response())
}

#[derive(Debug, serde::Deserialize)]
pub struct BatchCollectionRequest {
    pub asset_ids: Vec<String>,
    pub collection: String,
}

/// DELETE /api/batch/collection — remove assets from a collection.
pub async fn batch_remove_from_collection(
    State(state): State<Arc<AppState>>,
    Json(req): Json<BatchCollectionRequest>,
) -> Result<Response, Response> {
    let state = state.clone();
    let json = super::spawn_catalog_blocking(move || {
        let catalog = state.catalog()?;
        let col_store = crate::collection::CollectionStore::new(catalog.conn());
        let removed = col_store.remove_assets(&req.collection, &req.asset_ids)?;
        let yaml = col_store.export_all()?;
        crate::collection::save_yaml(&state.catalog_root, &yaml)?;
        state.dropdown_cache.invalidate_collections();
        Ok::<_, anyhow::Error>(serde_json::json!({
            "removed": removed,
            "collection": req.collection,
        }))
    })
    .await?;
    Ok(Json(json).into_response())
}

/// POST /api/batch/collection — add assets to a collection.
pub async fn batch_add_to_collection(
    State(state): State<Arc<AppState>>,
    Json(req): Json<BatchCollectionRequest>,
) -> Result<Response, Response> {
    let state = state.clone();
    let json = super::spawn_catalog_blocking(move || {
        let catalog = state.catalog()?;
        let col_store = crate::collection::CollectionStore::new(catalog.conn());
        let added = col_store.add_assets(&req.collection, &req.asset_ids)?;
        let yaml = col_store.export_all()?;
        crate::collection::save_yaml(&state.catalog_root, &yaml)?;
        state.dropdown_cache.invalidate_collections();
        Ok::<_, anyhow::Error>(serde_json::json!({
            "added": added,
            "collection": req.collection,
        }))
    })
    .await?;
    Ok(Json(json).into_response())
}

#[derive(serde::Deserialize)]
pub struct BatchAutoGroupRequest {
    pub asset_ids: Vec<String>,
}

/// POST /api/batch/auto-group — auto-group selected assets by filename stem.
pub async fn batch_auto_group(
    State(state): State<Arc<AppState>>,
    Json(req): Json<BatchAutoGroupRequest>,
) -> Result<Response, Response> {
    let state = state.clone();
    let json = super::spawn_catalog_blocking(move || {
        let engine = state.query_engine();
        let result = engine.auto_group(&req.asset_ids, false)?;
        Ok::<_, anyhow::Error>(serde_json::json!({
            "groups_merged": result.groups.len(),
            "donors_removed": result.total_donors_merged,
            "variants_moved": result.total_variants_moved,
        }))
    })
    .await?;
    Ok(Json(json).into_response())
}

#[derive(serde::Deserialize)]
pub struct BatchGroupRequest {
    pub asset_ids: Vec<String>,
    pub target_id: Option<String>,
}

/// POST /api/batch/group — merge selected assets into one.
pub async fn batch_group(
    State(state): State<Arc<AppState>>,
    Json(req): Json<BatchGroupRequest>,
) -> Result<Response, Response> {
    let state = state.clone();
    let json = super::spawn_catalog_blocking(move || {
        let engine = state.query_engine();
        let result = engine.group_by_asset_ids(&req.asset_ids, req.target_id.as_deref())?;
        Ok::<_, anyhow::Error>(serde_json::json!({
            "target_id": result.target_id,
            "variants_moved": result.variants_moved,
            "donors_removed": result.donors_removed,
        }))
    })
    .await?;
    Ok(Json(json).into_response())
}
