//! Search by query image — upload a local image file that is not in the
//! catalog and reference it from the browse URL as `similar:@<token>`.
//!
//! `POST /api/query-image` takes the raw file body (no multipart: the
//! import dialog sends server-side paths as JSON, so this is the first
//! browser→server file transfer and a plain body keeps it dependency-free),
//! runs the two-stage resolution in [`crate::query_image`] (content-hash
//! exact match, then SigLIP encoding) and stores the result in the
//! `AppState` session store. The browse pipeline resolves the token in
//! `routes::resolve_similar_filter`, so select-all, facets and export all
//! see exactly what the grid shows.
//!
//! Nothing is written to the catalog; the upload lands in a scratch
//! directory that is removed before the response goes out.

use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;

use crate::web::AppState;

/// Upper bound on an uploaded query image. Route-level `DefaultBodyLimit`
/// (registered in `web/mod.rs`) enforces it; the constant lives here so
/// the limit and the handler stay together.
pub const QUERY_IMAGE_MAX_BYTES: usize = 64 * 1024 * 1024;

/// Longest edge of the query-pill thumbnail returned to the browser.
const THUMB_EDGE: u32 = 96;

/// Header the browser uses to pass the original filename (percent-encoded).
const FILENAME_HEADER: &str = "x-maki-filename";

/// Tiny percent-decoder for the filename header (no crate needed).
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Map a `Content-Type` to a file extension for uploads without a usable
/// filename (clipboard pastes arrive as `image.png` at best).
fn extension_from_content_type(ct: &str) -> Option<&'static str> {
    match ct.split(';').next().unwrap_or("").trim() {
        "image/jpeg" | "image/jpg" => Some("jpg"),
        "image/png" => Some("png"),
        "image/webp" => Some("webp"),
        "image/tiff" => Some("tif"),
        "image/gif" => Some("gif"),
        "image/bmp" => Some("bmp"),
        "image/heic" | "image/heif" => Some("heic"),
        "image/x-adobe-dng" => Some("dng"),
        _ => None,
    }
}

/// Strip directories and anything unsafe from a client-supplied filename.
fn safe_filename(raw: &str) -> String {
    let base = raw.rsplit(['/', '\\']).next().unwrap_or("");
    let cleaned: String = base
        .chars()
        .filter(|c| !c.is_control())
        .collect();
    let trimmed = cleaned.trim();
    if trimmed.is_empty() || trimmed == "." || trimmed == ".." {
        "query".to_string()
    } else {
        trimmed.to_string()
    }
}

#[derive(serde::Serialize)]
pub struct QueryImageResponse {
    pub token: String,
    pub filename: String,
    pub exact_match_id: Option<String>,
    /// `false` when the model could not encode the file but an exact
    /// match answered the question anyway.
    pub embedded: bool,
    pub warning: Option<String>,
    /// `data:image/jpeg;base64,…` for the query pill (empty on failure).
    pub thumbnail: String,
}

/// POST /api/query-image — upload a query image, get a session token.
pub async fn upload_query_image(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if body.is_empty() {
        return (StatusCode::BAD_REQUEST, "empty upload").into_response();
    }
    let filename = headers
        .get(FILENAME_HEADER)
        .and_then(|v| v.to_str().ok())
        .map(percent_decode)
        .map(|s| safe_filename(&s))
        .unwrap_or_else(|| "query".to_string());
    let content_type = headers
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();

    let state2 = state.clone();
    let result = tokio::task::spawn_blocking(move || {
        upload_query_image_inner(&state2, &filename, &content_type, &body)
    })
    .await;

    match result {
        Ok(Ok(resp)) => Json(resp).into_response(),
        Ok(Err(e)) => (StatusCode::UNPROCESSABLE_ENTITY, format!("{e:#}")).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, format!("Error: {e}")).into_response(),
    }
}

fn upload_query_image_inner(
    state: &AppState,
    filename: &str,
    content_type: &str,
    body: &[u8],
) -> anyhow::Result<QueryImageResponse> {
    use crate::query_image::{self, QueryImageSession};

    // Scratch file: keep the client's extension (the preview generator
    // routes RAW/video by it), fall back to the content type.
    let mut ext = query_image::extension_of(std::path::Path::new(filename));
    if ext.is_empty() {
        ext = extension_from_content_type(content_type).unwrap_or("bin").to_string();
    }
    let scratch = std::env::temp_dir().join(format!("maki-upload-{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir_all(&scratch)?;
    let file = scratch.join(format!("query.{ext}"));
    let outcome = (|| -> anyhow::Result<QueryImageResponse> {
        std::fs::write(&file, body)?;

        let catalog = state.catalog()?;
        let preview_gen = state.preview_generator();
        let model_id = state.ai_config.model.clone();

        let (query, warning) = query_image::resolve_query_image(&catalog, &file, |path| {
            let model_dir = super::resolve_model_dir(&state.ai_config);
            let mgr = crate::model_manager::ModelManager::new(&model_dir, &model_id)?;
            if !mgr.model_exists() {
                anyhow::bail!("Model '{model_id}' is not downloaded.");
            }
            let mut guard = state.ai_model.blocking_lock();
            if guard.is_none() {
                *guard = Some(crate::ai::SigLipModel::load_with_provider(
                    &model_dir,
                    &model_id,
                    state.verbosity,
                    &state.ai_config.execution_provider,
                )?);
            }
            let model = guard.as_mut().expect("model loaded above");
            query_image::encode_query_image(model, &preview_gen, path)
        })?;

        let thumbnail = query_image::thumbnail_data_url(&preview_gen, &file, THUMB_EDGE)
            .unwrap_or_default();

        let session = QueryImageSession {
            query: query.clone(),
            filename: filename.to_string(),
            thumbnail: thumbnail.clone(),
            created: std::time::Instant::now(),
        };
        let token = state
            .query_images
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(session);

        Ok(QueryImageResponse {
            token,
            filename: filename.to_string(),
            exact_match_id: query.exact_asset_id,
            embedded: query.embedding.is_some(),
            warning,
            thumbnail,
        })
    })();
    let _ = std::fs::remove_dir_all(&scratch);
    outcome
}

/// GET /api/query-image/{token} — pill data for a live session, 404 once
/// it has expired (the browse page then drops the token from the query).
pub async fn query_image_info(
    State(state): State<Arc<AppState>>,
    Path(token): Path<String>,
) -> Response {
    let sessions = state.query_images.lock().unwrap_or_else(|e| e.into_inner());
    match sessions.get(&token) {
        Some(s) => Json(QueryImageResponse {
            token: token.clone(),
            filename: s.filename.clone(),
            exact_match_id: s.query.exact_asset_id.clone(),
            embedded: s.query.embedding.is_some(),
            warning: None,
            thumbnail: s.thumbnail.clone(),
        })
        .into_response(),
        None => (StatusCode::NOT_FOUND, "query image expired").into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percent_decode_roundtrip() {
        assert_eq!(percent_decode("IMG%204711.jpg"), "IMG 4711.jpg");
        assert_eq!(percent_decode("plain.png"), "plain.png");
        assert_eq!(percent_decode("bad%zz"), "bad%zz");
        assert_eq!(percent_decode("trail%2"), "trail%2");
    }

    #[test]
    fn safe_filename_strips_paths() {
        assert_eq!(safe_filename("/tmp/x/photo.jpg"), "photo.jpg");
        assert_eq!(safe_filename("C:\\Users\\me\\photo.jpg"), "photo.jpg");
        assert_eq!(safe_filename(""), "query");
        assert_eq!(safe_filename(".."), "query");
    }

    #[test]
    fn content_type_extensions() {
        assert_eq!(extension_from_content_type("image/png"), Some("png"));
        assert_eq!(extension_from_content_type("image/jpeg; charset=x"), Some("jpg"));
        assert_eq!(extension_from_content_type("application/octet-stream"), None);
    }
}
