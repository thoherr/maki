//! Visual similarity search — the `/api/asset/{id}/similar` endpoint.
//!
//! Resolves an asset's stored SigLIP embedding, queries the in-memory
//! similarity index, and returns scored neighbours.

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::response::{IntoResponse, Response};
use axum::Json;

use crate::web::AppState;

// --- Visual similarity search endpoint ---

#[derive(Debug, serde::Serialize)]
pub struct SimilarAssetResponse {
    pub asset_id: String,
    pub similarity: f32,
    pub preview_url: Option<String>,
    pub name: String,
}

/// POST /api/asset/{id}/similar — find visually similar assets.
pub async fn find_similar(
    State(state): State<Arc<AppState>>,
    Path(asset_id): Path<String>,
) -> Result<Response, Response> {
    let state = state.clone();
    let results = super::super::spawn_blocking_with(
        move || find_similar_inner(&state, &asset_id).map_err(anyhow::Error::msg),
        super::super::err::internal,
    )
    .await?;
    Ok(Json(results).into_response())
}

fn find_similar_inner(
    state: &AppState,
    asset_id: &str,
) -> Result<Vec<SimilarAssetResponse>, String> {
    use crate::ai;
    use crate::device_registry::DeviceRegistry;
    use crate::embedding_store::EmbeddingStore;

    let model_id = &state.ai_config.model;
    if crate::ai::get_model_spec(model_id).is_none() {
        return Err(format!("Unknown model: {model_id}"));
    }

    let catalog = state.catalog().map_err(|e| format!("{e:#}"))?;
    let _ = EmbeddingStore::initialize(catalog.conn());
    let emb_store = EmbeddingStore::new(catalog.conn());

    let stored_emb = emb_store.get(asset_id, model_id).map_err(|e| format!("{e:#}"))?;

    let query_emb = if let Some(emb) = stored_emb {
        emb
    } else {
        let engine = state.query_engine();
        let details = engine.show(asset_id).map_err(|e| format!("{e:#}"))?;

        let preview_gen = state.preview_generator();
        let registry = DeviceRegistry::new(&state.catalog_root);
        let volumes = registry.list().map_err(|e| format!("{e:#}"))?;
        let online_volumes: std::collections::HashMap<String, &crate::models::Volume> = volumes
            .iter()
            .filter(|v| v.is_online)
            .map(|v| (v.id.to_string(), v))
            .collect();

        let service = state.asset_service();
        let image_path = service
            .find_image_for_ai(&details, &preview_gen, &online_volumes)
            .ok_or_else(|| "No processable image found for this asset".to_string())?;

        let ext = image_path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("");
        if !ai::is_supported_image(ext) {
            return Err(format!("Unsupported image format: {ext}"));
        }

        let emb = state
            .with_siglip(|model| model.encode_image(&image_path))
            .map_err(|e| format!("Failed to load AI model: {e:#}"))?
            .map_err(|e| format!("Failed to encode image: {e:#}"))?;

        emb_store
            .store(asset_id, &emb, model_id)
            .map_err(|e| format!("Failed to store embedding: {e:#}"))?;
        let _ = crate::embedding_store::write_embedding_binary(&state.catalog_root, model_id, asset_id, &emb);
        emb
    };

    let results = {
        let mut idx = state
            .embedding_index_mut(&catalog)
            .map_err(|e| format!("Failed to load embedding index: {e:#}"))?;
        idx.upsert(asset_id, &query_emb);
        idx.search(&query_emb, 20, Some(asset_id))
    };

    let preview_gen = state.preview_generator();
    let preview_ext = &state.preview_ext;
    let response: Vec<SimilarAssetResponse> = results
        .into_iter()
        .filter_map(|(id, similarity)| {
            let cat = state.catalog().ok()?;
            let d = cat.load_asset_details(&id).ok()??;
            let name = d
                .name
                .clone()
                .unwrap_or_else(|| {
                    d.variants
                        .first()
                        .and_then(|v| {
                            v.locations
                                .first()
                                .map(|fl| {
                                    std::path::Path::new(&fl.relative_path)
                                        .file_name()
                                        .unwrap_or_default()
                                        .to_string_lossy()
                                        .to_string()
                                })
                        })
                        .unwrap_or_else(|| id[..8.min(id.len())].to_string())
                });

            let preview_url = {
                let best_idx = d.best_variant_index();
                best_idx.and_then(|i| {
                    let v = &d.variants[i];
                    if preview_gen.has_preview(&v.content_hash) {
                        Some(crate::web::templates::preview_url(&v.content_hash, preview_ext))
                    } else {
                        None
                    }
                })
            };

            Some(SimilarAssetResponse {
                asset_id: id,
                similarity,
                preview_url,
                name,
            })
        })
        .collect();

    Ok(response)
}
