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
        // need a restart, AND to drive the in-place diff that preserves
        // comments + per-key formatting in the on-disk maki.toml.
        let current = crate::config::CatalogConfig::load(&state.catalog_root).unwrap_or_default();
        let restart_required = needs_restart(&current, &new_config);

        // Backup before write so a botched edit can be recovered by hand.
        let path = state.catalog_root.join("maki.toml");
        if path.exists() {
            let bak = state.catalog_root.join("maki.toml.bak");
            let _ = std::fs::copy(&path, &bak);
        }

        save_preserving_comments(&path, &new_config, &current)?;

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

/// Write `new_config` to `path` while keeping the existing file's
/// comments, blank lines, and per-key formatting intact wherever the
/// new value matches the old.
///
/// The mechanic is a field-level diff against `current_config` (the
/// in-memory representation of what's currently on disk). We walk both
/// configs as `serde_json::Value` trees and apply only the leaves that
/// differ to a `toml_edit::DocumentMut` parsed from the on-disk file.
/// Touched keys get their value replaced (decor — comments, whitespace
/// — stays); untouched keys are not visited at all.
///
/// No-op saves (every leaf equal) leave the document object untouched,
/// so `doc.to_string()` round-trips byte-identical to the input — the
/// `save_with_comments_*` regression tests pin this behaviour.
fn save_preserving_comments(
    path: &std::path::Path,
    new_config: &crate::config::CatalogConfig,
    current_config: &crate::config::CatalogConfig,
) -> anyhow::Result<()> {
    let existing = std::fs::read_to_string(path).unwrap_or_default();
    let mut doc: toml_edit::DocumentMut = if existing.trim().is_empty() {
        toml_edit::DocumentMut::new()
    } else {
        existing
            .parse()
            .map_err(|e| anyhow::anyhow!("existing maki.toml has invalid TOML: {e}"))?
    };

    let new_json = serde_json::to_value(new_config)?;
    let current_json = serde_json::to_value(current_config)?;

    if let (serde_json::Value::Object(new_obj), serde_json::Value::Object(cur_obj)) =
        (&new_json, &current_json)
    {
        apply_diff_to_table(doc.as_table_mut(), new_obj, cur_obj);
    }

    std::fs::write(path, doc.to_string())?;
    Ok(())
}

/// Recursive diff applicator. For each key in `new_obj`:
///   - if unchanged from `current_obj` → leave the table alone
///   - if changed (leaf) → overwrite the value in place (decor preserved)
///   - if changed (object) → recurse into the inner table, creating it
///     in `table` if it didn't exist yet
/// And for each key present in `current_obj` but absent in `new_obj` →
/// remove it from the table (handles Option<T> being cleared by the
/// user clicking the field's value down to empty).
fn apply_diff_to_table(
    table: &mut toml_edit::Table,
    new_obj: &serde_json::Map<String, serde_json::Value>,
    current_obj: &serde_json::Map<String, serde_json::Value>,
) {
    let removed: Vec<String> = current_obj
        .keys()
        .filter(|k| !new_obj.contains_key(*k))
        .cloned()
        .collect();
    for k in &removed {
        table.remove(k);
    }

    for (key, new_val) in new_obj {
        let current_val = current_obj.get(key);
        if current_val.is_some_and(|c| c == new_val) {
            continue;
        }

        match new_val {
            serde_json::Value::Object(new_inner) => {
                let cur_inner = current_val
                    .and_then(|v| v.as_object())
                    .cloned()
                    .unwrap_or_default();
                if !table.contains_key(key) {
                    table.insert(key, toml_edit::Item::Table(toml_edit::Table::new()));
                }
                if let Some(inner_table) = table[key].as_table_mut() {
                    apply_diff_to_table(inner_table, new_inner, &cur_inner);
                }
            }
            other => {
                if let Some(v) = json_value_to_toml_value(other) {
                    table[key] = toml_edit::Item::Value(v);
                } else if matches!(other, serde_json::Value::Null) {
                    table.remove(key);
                }
            }
        }
    }
}

/// Convert a leaf JSON value into the equivalent `toml_edit::Value`.
/// Objects return `None` — those are handled structurally by the
/// recursive walker in `apply_diff_to_table`. Numbers prefer integer
/// representation when the JSON number is integral (matches what the
/// form sends for `Vec<u32>`/`u16`/etc. fields).
fn json_value_to_toml_value(j: &serde_json::Value) -> Option<toml_edit::Value> {
    use toml_edit::{Array, Formatted, Value};
    match j {
        serde_json::Value::Null => None,
        serde_json::Value::Bool(b) => Some(Value::Boolean(Formatted::new(*b))),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Some(Value::Integer(Formatted::new(i)))
            } else {
                n.as_f64().map(|f| Value::Float(Formatted::new(f)))
            }
        }
        serde_json::Value::String(s) => Some(Value::String(Formatted::new(s.clone()))),
        serde_json::Value::Array(arr) => {
            let mut a = Array::new();
            for item in arr {
                if let Some(v) = json_value_to_toml_value(item) {
                    a.push(v);
                }
            }
            Some(Value::Array(a))
        }
        serde_json::Value::Object(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::CatalogConfig;

    fn write_and_load(text: &str) -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("maki.toml");
        std::fs::write(&path, text).unwrap();
        (dir, path)
    }

    /// No-op save: a save that doesn't change any value should produce
    /// byte-identical output. This is the headline guarantee comment
    /// preservation gives us — opening the dialog and clicking Save
    /// without touching anything must not rewrite the file.
    #[test]
    fn save_with_comments_no_op_is_byte_identical() {
        let original = r#"# personal notes
[ai]
# Available models: ...
#   siglip-vit-b16-256 — base
#   siglip2-large-256-multi — large
model = "siglip2-large-256-multi"
threshold = 0.1
labels = "my-labels.txt"
text_limit = 100
"#;
        let (_dir, path) = write_and_load(original);
        let current = CatalogConfig::load(path.parent().unwrap()).unwrap();
        let new_config = current.clone();
        save_preserving_comments(&path, &new_config, &current).unwrap();
        let after = std::fs::read_to_string(&path).unwrap();
        assert_eq!(after, original, "no-op save must round-trip byte-identical");
    }

    /// Editing one field preserves every comment in the file.
    #[test]
    fn save_with_comments_single_field_change_keeps_comments() {
        let original = r#"# top comment
[ai]
# Available models: ...
model = "siglip2-large-256-multi"
threshold = 0.1
labels = "my-labels.txt"
"#;
        let (_dir, path) = write_and_load(original);
        let current = CatalogConfig::load(path.parent().unwrap()).unwrap();
        let mut new_config = current.clone();
        new_config.ai.threshold = 0.2;
        save_preserving_comments(&path, &new_config, &current).unwrap();
        let after = std::fs::read_to_string(&path).unwrap();
        assert!(after.contains("# top comment"), "lost top comment: {after}");
        assert!(after.contains("# Available models"), "lost models comment: {after}");
        assert!(after.contains("threshold = 0.2"), "did not write new value: {after}");
        assert!(!after.contains("threshold = 0.1"), "left old value behind: {after}");
        assert!(after.contains("labels = \"my-labels.txt\""), "lost adjacent field: {after}");
    }

    /// Clearing an `Option<T>` field removes the line from the file.
    /// Verifies the JSON-key-absent → toml-key-remove path.
    #[test]
    fn save_with_comments_optional_clear_removes_line() {
        let original = r#"[ai]
model = "siglip2-large-256-multi"
labels = "my-labels.txt"
text_limit = 100
"#;
        let (_dir, path) = write_and_load(original);
        let current = CatalogConfig::load(path.parent().unwrap()).unwrap();
        let mut new_config = current.clone();
        new_config.ai.labels = None;
        save_preserving_comments(&path, &new_config, &current).unwrap();
        let after = std::fs::read_to_string(&path).unwrap();
        assert!(!after.contains("labels"), "labels line should be gone: {after}");
        assert!(after.contains("model = "), "model line must remain: {after}");
        assert!(after.contains("text_limit = 100"), "text_limit must remain: {after}");
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
