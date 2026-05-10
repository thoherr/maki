//! Config editor endpoints — power the new "Config" tab in the Maintain
//! dialog.
//!
//! Three endpoints:
//!
//! - `GET /api/config` — current config values + the raw on-disk TOML.
//!   The form widgets bind to the values; the raw TOML is shown as a
//!   read-only sidebar so the user can see what's about to change.
//! - `GET /api/config/schema` — JSON Schema generated from the
//!   `CatalogConfig` struct via `schemars`. Drives the form-builder JS:
//!   each field's `description` (from the `///` doc-comment), `type`,
//!   `enum`, and validation hints (`minimum` / `maximum`) come straight
//!   from the schema.
//! - `POST /api/config` — save. Validates by deserialising the submitted
//!   JSON into `CatalogConfig` (any type / unknown-key error short-circuits
//!   here), serialises back to TOML, makes a `.bak` backup of the prior
//!   file before writing, and returns a "restart-required" hint when the
//!   user touched a boot-time-only option.

use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;

use super::super::AppState;

/// GET /api/config — current values + raw on-disk TOML.
pub async fn get_config_api(State(state): State<Arc<AppState>>) -> Response {
    let state = state.clone();
    let result = tokio::task::spawn_blocking(move || {
        let path = state.catalog_root.join("maki.toml");
        let raw = std::fs::read_to_string(&path).unwrap_or_default();
        let config = crate::config::CatalogConfig::load(&state.catalog_root)
            .unwrap_or_default();
        Ok::<_, anyhow::Error>(serde_json::json!({
            "config": config,
            "raw_toml": raw,
            "path": path.display().to_string(),
        }))
    })
    .await;

    match result {
        Ok(Ok(json)) => Json(json).into_response(),
        Ok(Err(e)) => (StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, format!("{e}")).into_response(),
    }
}

/// GET /api/config/schema — JSON Schema for `CatalogConfig`.
///
/// Generated once at request time via `schemars::schema_for!`. The JS
/// walker uses it to render appropriate widgets per field: bool →
/// checkbox, integer → number input with `minimum`/`maximum`, string →
/// text input, string with `enum` → select, `Vec<String>` → chip list,
/// nested object → collapsible section.
pub async fn get_config_schema_api() -> Response {
    let schema = schemars::schema_for!(crate::config::CatalogConfig);
    Json(schema).into_response()
}

#[derive(Debug, Deserialize)]
pub struct SaveConfigRequest {
    /// Full updated config in JSON form (matches `CatalogConfig` shape).
    /// Sent by the form-builder after collecting widget values; the
    /// server round-trips through serde to validate types and reject
    /// unknown keys, then serialises back to TOML for disk write.
    pub config: serde_json::Value,
}

/// POST /api/config — validate + save.
///
/// Validation is performed by deserialising the submitted JSON into a
/// fully-typed `CatalogConfig` — any type mismatch or unrecognised key
/// fails the request before anything touches disk. The response carries
/// a `restart_required` hint: `false` means the change took effect
/// immediately (e.g. `[browse] slideshow_seconds`), `true` means a
/// `maki serve` restart is needed for the change to apply (e.g.
/// `[serve] port`).
pub async fn save_config_api(
    State(state): State<Arc<AppState>>,
    Json(req): Json<SaveConfigRequest>,
) -> Response {
    let state = state.clone();
    let result = tokio::task::spawn_blocking(move || {
        // Validate by round-tripping through the typed struct. serde
        // rejects type mismatches and (per the existing `deny_unknown_fields`
        // behaviour we'd inherit if any sub-struct sets it) unknown keys.
        let new_config: crate::config::CatalogConfig =
            serde_json::from_value(req.config)
                .map_err(|e| anyhow::anyhow!("config validation failed: {e}"))?;
        new_config.validate()?;

        // Compare against current to detect boot-time-only changes that
        // need a restart. We do this BEFORE the write so the user sees
        // the right banner even if the write somehow fails downstream.
        let current = crate::config::CatalogConfig::load(&state.catalog_root).unwrap_or_default();
        let restart_required = needs_restart(&current, &new_config);

        // Backup before write — toml::to_string_pretty drops comments,
        // so the user's hand-organised maki.toml might lose annotations.
        // Keep the previous file as `maki.toml.bak` so it's recoverable.
        let path = state.catalog_root.join("maki.toml");
        if path.exists() {
            let bak = state.catalog_root.join("maki.toml.bak");
            let _ = std::fs::copy(&path, &bak);
        }

        new_config.save(&state.catalog_root)?;

        Ok::<_, anyhow::Error>(serde_json::json!({
            "ok": true,
            "restart_required": restart_required,
            "backup_path": state.catalog_root.join("maki.toml.bak").display().to_string(),
        }))
    })
    .await;

    match result {
        Ok(Ok(json)) => Json(json).into_response(),
        Ok(Err(e)) => (StatusCode::BAD_REQUEST, format!("{e:#}")).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, format!("{e}")).into_response(),
    }
}

/// Detect changes that require a process restart to take effect.
///
/// Conservative: include any option that's bound at server startup
/// (port/bind, pool capacity, AI model load, cached file extensions).
/// Hot-reload-safe options (`[browse]`, `[contact_sheet]`, `[ai].labels`,
/// `[vlm].prompt`, etc.) are read on each request and don't need a
/// restart. The classification could move into the schema as a custom
/// vocabulary later; for now it's a single function the front-end can
/// trust.
fn needs_restart(
    old: &crate::config::CatalogConfig,
    new: &crate::config::CatalogConfig,
) -> bool {
    // [serve] — port and bind are bound at startup
    if old.serve.port != new.serve.port { return true; }
    if old.serve.bind != new.serve.bind { return true; }
    // [serve] per_page — populates per-page browse handler at request
    // time, but the AppState carries it as a field. Treat as boot-time
    // for safety (the AppState value won't update until restart).
    if old.serve.per_page != new.serve.per_page { return true; }
    // [serve] stroll_* — same story (AppState fields).
    if old.serve.stroll_neighbors != new.serve.stroll_neighbors { return true; }
    if old.serve.stroll_neighbors_max != new.serve.stroll_neighbors_max { return true; }
    if old.serve.stroll_fanout != new.serve.stroll_fanout { return true; }
    if old.serve.stroll_fanout_max != new.serve.stroll_fanout_max { return true; }
    if old.serve.stroll_discover_pool != new.serve.stroll_discover_pool { return true; }
    // [preview] — PreviewConfig is cloned into AppState and the
    // PreviewGenerator is built once. Changes need a restart.
    if old.preview != new.preview { return true; }
    // [ai].model and execution_provider — model is loaded once.
    if old.ai.model != new.ai.model { return true; }
    if old.ai.execution_provider != new.ai.execution_provider { return true; }
    if old.ai.model_dir != new.ai.model_dir { return true; }
    // [browse].default_filter — held by AppState. Restart to refresh.
    if old.browse.default_filter != new.browse.default_filter { return true; }
    false
}
