//! Calendar heatmap and map marker API routes.

use std::sync::Arc;

use axum::extract::{Query, State};
use axum::response::{IntoResponse, Response};
use axum::Json;

use super::super::AppState;
use super::{build_parsed_search, EmptyFilterPolicy, ResolvedSearch, SearchParams};

#[derive(Debug, serde::Deserialize)]
pub struct CalendarParams {
    pub year: Option<i32>,
    pub q: Option<String>,
    #[serde(rename = "type")]
    pub asset_type: Option<String>,
    pub tag: Option<String>,
    pub format: Option<String>,
    pub volume: Option<String>,
    pub rating: Option<String>,
    pub label: Option<String>,
    pub collection: Option<String>,
    pub path: Option<String>,
    pub stacks: Option<String>,
    pub person: Option<String>,
    pub nodefault: Option<String>,
}

/// GET /api/calendar — calendar heatmap data.
pub async fn calendar_api(
    State(state): State<Arc<AppState>>,
    Query(params): Query<CalendarParams>,
) -> Result<Response, Response> {
    let state = state.clone();
    let json = super::spawn_catalog_blocking(move || {
        let catalog = state.catalog()?;

        let year = params.year.unwrap_or_else(|| {
            chrono::Utc::now().format("%Y").to_string().parse::<i32>().unwrap_or(2026)
        });

        let search_params = SearchParams {
            q: params.q.clone(),
            asset_type: params.asset_type.clone(),
            tag: params.tag.clone(),
            format: params.format.clone(),
            volume: params.volume.clone(),
            rating: params.rating.clone(),
            label: params.label.clone(),
            collection: params.collection.clone(),
            path: params.path.clone(),
            person: params.person.clone(),
            sort: None,
            page: None,
            stacks: params.stacks.clone(),
            nodefault: params.nodefault.clone(),
        };
        let bf = build_parsed_search(&search_params, &state);
        let parsed = bf.parsed;
        let collapse_stacks = bf.collapse_stacks;

        let resolved = ResolvedSearch::resolve(
            &catalog, &parsed, bf.volume, bf.path_volume_id, EmptyFilterPolicy::MatchNothing,
        );

        let mut opts = parsed.to_search_options();
        resolved.apply(&mut opts);
        opts.collapse_stacks = collapse_stacks;

        let counts = catalog.calendar_counts(year, &opts)?;
        let years = catalog.calendar_years()?;

        Ok::<_, anyhow::Error>(serde_json::json!({
            "year": year,
            "counts": counts,
            "years": years,
        }))
    })
    .await?;
    Ok(Json(json).into_response())
}

#[derive(Debug, serde::Deserialize)]
pub struct MapParams {
    pub q: Option<String>,
    #[serde(rename = "type")]
    pub asset_type: Option<String>,
    pub tag: Option<String>,
    pub format: Option<String>,
    pub volume: Option<String>,
    pub rating: Option<String>,
    pub label: Option<String>,
    pub collection: Option<String>,
    pub path: Option<String>,
    pub stacks: Option<String>,
    pub limit: Option<u32>,
    pub person: Option<String>,
    pub nodefault: Option<String>,
}

/// GET /api/map — map markers for geotagged assets.
pub async fn map_api(
    State(state): State<Arc<AppState>>,
    Query(params): Query<MapParams>,
) -> Result<Response, Response> {
    let state = state.clone();
    let json = super::spawn_catalog_blocking(move || {
        let catalog = state.catalog()?;

        let limit = params.limit.unwrap_or(10_000);

        let search_params = SearchParams {
            q: params.q.clone(),
            asset_type: params.asset_type.clone(),
            tag: params.tag.clone(),
            format: params.format.clone(),
            volume: params.volume.clone(),
            rating: params.rating.clone(),
            label: params.label.clone(),
            collection: params.collection.clone(),
            path: params.path.clone(),
            person: params.person.clone(),
            sort: None,
            page: None,
            stacks: params.stacks.clone(),
            nodefault: params.nodefault.clone(),
        };
        let bf = build_parsed_search(&search_params, &state);
        let parsed = bf.parsed;
        let collapse_stacks = bf.collapse_stacks;

        let resolved = ResolvedSearch::resolve(
            &catalog, &parsed, bf.volume, bf.path_volume_id, EmptyFilterPolicy::MatchNothing,
        );

        let mut opts = parsed.to_search_options();
        resolved.apply(&mut opts);
        opts.collapse_stacks = collapse_stacks;

        let preview_ext = &state.preview_ext;
        let (markers, total) = catalog.map_markers(&opts, limit)?;

        let markers_json: Vec<serde_json::Value> = markers.iter().map(|m| {
            let preview_url = m.preview.as_ref().map(|h| {
                crate::web::templates::preview_url(h, preview_ext)
            });
            serde_json::json!({
                "id": m.id,
                "lat": m.lat,
                "lng": m.lng,
                "preview": preview_url,
                "name": m.name,
                "rating": m.rating,
                "label": m.label,
            })
        }).collect();

        Ok::<_, anyhow::Error>(serde_json::json!({
            "markers": markers_json,
            "total": total,
            "truncated": total > limit as u64,
        }))
    })
    .await?;
    Ok(Json(json).into_response())
}
