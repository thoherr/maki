//! Embedded static assets — CSS, JS, fonts, icons compiled into the
//! binary via `include_bytes!` so `maki serve` works without an external
//! asset directory.

use axum::http::header;
use axum::response::{IntoResponse, Response};

use axum::extract::Path;
use axum::http::StatusCode;

/// Browse-page script modules (`src/web/static/browse/*.js`), served at
/// `/static/browse/<name>`: the sections of the former inline script of
/// `browse.html`, one file each (architecture audit F22.3).
pub(crate) const BROWSE_MODULES: &[(&str, &str)] = &[("batch.js", include_str!("static/browse/batch.js"))];

/// URL of a browse module with a content fingerprint as its query, so the
/// response can be cached forever and a new binary is never paired with
/// a cached script of the old one. Unknown names get no fingerprint (and
/// a 404 when fetched); `every_browse_module_is_served` guards the list.
pub(crate) fn browse_module_url(name: &str) -> String {
    use sha2::{Digest, Sha256};
    match BROWSE_MODULES.iter().find(|(n, _)| *n == name) {
        Some((_, source)) => {
            let digest = Sha256::digest(source.as_bytes());
            let tag: String = digest[..6].iter().map(|b| format!("{b:02x}")).collect();
            format!("/static/browse/{name}?v={tag}")
        }
        None => format!("/static/browse/{name}"),
    }
}

pub async fn browse_module(Path(name): Path<String>) -> Response {
    match BROWSE_MODULES.iter().find(|(n, _)| *n == name) {
        Some((_, source)) => (
            [
                (header::CONTENT_TYPE, "application/javascript; charset=utf-8"),
                (header::CACHE_CONTROL, "public, max-age=31536000, immutable"),
            ],
            *source,
        )
            .into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

const HTMX_JS: &[u8] = include_bytes!("static/htmx.min.js");
const STYLE_CSS: &str = include_str!("static/style.css");

// Leaflet + MarkerCluster
const LEAFLET_JS: &[u8] = include_bytes!("static/leaflet.min.js");
const LEAFLET_CSS: &str = include_str!("static/leaflet.css");
const MARKERCLUSTER_JS: &[u8] = include_bytes!("static/leaflet.markercluster.min.js");
const MARKERCLUSTER_CSS: &str = include_str!("static/MarkerCluster.css");
const MARKERCLUSTER_DEFAULT_CSS: &str = include_str!("static/MarkerCluster.Default.css");

// Brand assets
const FAVICON_ICO: &[u8] = include_bytes!("static/favicon.ico");
const MAKI_ICON_SVG: &[u8] = include_bytes!("static/maki-icon.svg");

// Leaflet marker images
const MARKER_ICON: &[u8] = include_bytes!("static/leaflet-images/marker-icon.png");
const MARKER_ICON_2X: &[u8] = include_bytes!("static/leaflet-images/marker-icon-2x.png");
const MARKER_SHADOW: &[u8] = include_bytes!("static/leaflet-images/marker-shadow.png");
const LAYERS_PNG: &[u8] = include_bytes!("static/leaflet-images/layers.png");
const LAYERS_2X: &[u8] = include_bytes!("static/leaflet-images/layers-2x.png");

pub async fn htmx_js() -> Response {
    (
        [
            (header::CONTENT_TYPE, "application/javascript"),
            (header::CACHE_CONTROL, "public, max-age=31536000, immutable"),
        ],
        HTMX_JS,
    )
        .into_response()
}

pub async fn style_css() -> Response {
    (
        [
            (header::CONTENT_TYPE, "text/css"),
            (header::CACHE_CONTROL, "public, max-age=3600"),
        ],
        STYLE_CSS,
    )
        .into_response()
}

pub async fn leaflet_js() -> Response {
    (
        [
            (header::CONTENT_TYPE, "application/javascript"),
            (header::CACHE_CONTROL, "public, max-age=31536000, immutable"),
        ],
        LEAFLET_JS,
    )
        .into_response()
}

pub async fn leaflet_css() -> Response {
    (
        [
            (header::CONTENT_TYPE, "text/css"),
            (header::CACHE_CONTROL, "public, max-age=31536000, immutable"),
        ],
        LEAFLET_CSS,
    )
        .into_response()
}

pub async fn markercluster_js() -> Response {
    (
        [
            (header::CONTENT_TYPE, "application/javascript"),
            (header::CACHE_CONTROL, "public, max-age=31536000, immutable"),
        ],
        MARKERCLUSTER_JS,
    )
        .into_response()
}

pub async fn markercluster_css() -> Response {
    (
        [
            (header::CONTENT_TYPE, "text/css"),
            (header::CACHE_CONTROL, "public, max-age=31536000, immutable"),
        ],
        MARKERCLUSTER_CSS,
    )
        .into_response()
}

pub async fn markercluster_default_css() -> Response {
    (
        [
            (header::CONTENT_TYPE, "text/css"),
            (header::CACHE_CONTROL, "public, max-age=31536000, immutable"),
        ],
        MARKERCLUSTER_DEFAULT_CSS,
    )
        .into_response()
}

pub async fn favicon() -> Response {
    (
        [
            (header::CONTENT_TYPE, "image/x-icon"),
            (header::CACHE_CONTROL, "public, max-age=31536000, immutable"),
        ],
        FAVICON_ICO,
    )
        .into_response()
}

pub async fn maki_icon() -> Response {
    (
        [
            (header::CONTENT_TYPE, "image/svg+xml"),
            (header::CACHE_CONTROL, "public, max-age=31536000, immutable"),
        ],
        MAKI_ICON_SVG,
    )
        .into_response()
}

pub async fn marker_icon() -> Response {
    (
        [
            (header::CONTENT_TYPE, "image/png"),
            (header::CACHE_CONTROL, "public, max-age=31536000, immutable"),
        ],
        MARKER_ICON,
    )
        .into_response()
}

pub async fn marker_icon_2x() -> Response {
    (
        [
            (header::CONTENT_TYPE, "image/png"),
            (header::CACHE_CONTROL, "public, max-age=31536000, immutable"),
        ],
        MARKER_ICON_2X,
    )
        .into_response()
}

pub async fn marker_shadow() -> Response {
    (
        [
            (header::CONTENT_TYPE, "image/png"),
            (header::CACHE_CONTROL, "public, max-age=31536000, immutable"),
        ],
        MARKER_SHADOW,
    )
        .into_response()
}

pub async fn layers_png() -> Response {
    (
        [
            (header::CONTENT_TYPE, "image/png"),
            (header::CACHE_CONTROL, "public, max-age=31536000, immutable"),
        ],
        LAYERS_PNG,
    )
        .into_response()
}

pub async fn layers_2x() -> Response {
    (
        [
            (header::CONTENT_TYPE, "image/png"),
            (header::CACHE_CONTROL, "public, max-age=31536000, immutable"),
        ],
        LAYERS_2X,
    )
        .into_response()
}
