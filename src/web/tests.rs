//! Web-layer tests: build the real axum `Router` against a temp catalog
//! and drive it in-process with `tower::ServiceExt::oneshot` — no
//! listening socket, no separate server process.
//!
//! Focus is the **mutation endpoints** and their contracts:
//! - status codes and fragment rendering
//! - the `HX-Trigger: pending-changed` header (the asset page's recipes
//!   block depends on it to refresh pending_writeback markers)
//! - the dual-store write: every metadata edit must land in BOTH the
//!   YAML sidecar (source of truth) and the SQLite catalog. These
//!   assertions double as a regression net for the v4.5.15 divergence
//!   bug class.
//!
//! Read endpoints get smoke coverage. This is the harness plus exemplary
//! coverage, not exhaustive coverage — new web features should add their
//! tests here against `TestServer`.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{HeaderMap, Method, Request, StatusCode};
use http_body_util::BodyExt;
use tower::ServiceExt;

use crate::catalog::Catalog;
use crate::metadata_store::MetadataStore;
use crate::models::{Asset, AssetType, Variant, VariantRole};

use super::{build_router, AppState};

/// A temp catalog with one seeded image asset, plus the `AppState` the
/// router runs against. Dropping it removes the temp directory.
struct TestServer {
    _dir: tempfile::TempDir,
    root: std::path::PathBuf,
    state: Arc<AppState>,
    asset_id: String,
}

impl TestServer {
    fn new() -> Self {
        Self::new_with(false, "")
    }

    /// `read_only` enables safe-sharing mode; a non-empty `maki_toml` is
    /// written to the catalog root before `AppState` loads config (used
    /// to exercise the `[serve]` auth settings through the real path).
    fn new_with(read_only: bool, maki_toml: &str) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        if !maki_toml.is_empty() {
            std::fs::write(root.join("maki.toml"), maki_toml).unwrap();
        }

        let asset_id;
        {
            let catalog = Catalog::open(&root).unwrap();
            catalog.initialize().unwrap();

            let mut asset = Asset::new(AssetType::Image, "sha256:webtest1");
            asset.name = Some("sunset photo".to_string());
            asset.tags = vec!["landscape".to_string()];
            asset.rating = Some(2);
            let variant = Variant {
                content_hash: "sha256:webtest1".to_string(),
                asset_id: asset.id,
                role: VariantRole::Original,
                format: "jpg".to_string(),
                file_size: 5000,
                original_filename: "sunset_beach.jpg".to_string(),
                source_metadata: Default::default(),
                locations: vec![],
            };
            asset.variants.push(variant.clone());

            let store = MetadataStore::new(&root);
            store.save(&asset).unwrap();
            catalog.insert_asset(&asset).unwrap();
            catalog.insert_variant(&variant).unwrap();
            asset_id = asset.id.to_string();
        }

        // Point the VLM probe at a closed port so AppState::new fails the
        // endpoint check instantly instead of contacting a real local LLM.
        let mut vlm_config = crate::config::VlmConfig::default();
        vlm_config.endpoint = "http://127.0.0.1:1".to_string();

        #[cfg(feature = "ai")]
        let state = Arc::new(AppState::new(
            root.clone(),
            crate::config::PreviewConfig::default(),
            false,
            None,
            60,
            6,
            24,
            3,
            9,
            200,
            read_only,
            crate::config::AiConfig::default(),
            vlm_config,
            None,
            crate::Verbosity::quiet(),
        ));
        #[cfg(not(feature = "ai"))]
        let state = Arc::new(AppState::new(
            root.clone(),
            crate::config::PreviewConfig::default(),
            false,
            None,
            60,
            6,
            24,
            3,
            9,
            200,
            read_only,
            vlm_config,
            None,
            crate::Verbosity::quiet(),
        ));

        Self { _dir: dir, root, state, asset_id }
    }

    /// Fire one request at a fresh router instance. Returns status,
    /// response headers, and the body as a string.
    async fn request(
        &self,
        method: Method,
        uri: &str,
        content_type: Option<&str>,
        body: &str,
    ) -> (StatusCode, HeaderMap, String) {
        let router = build_router(self.state.clone());
        let mut builder = Request::builder().method(method).uri(uri);
        if let Some(ct) = content_type {
            builder = builder.header("content-type", ct);
        }
        let request = builder.body(Body::from(body.to_string())).unwrap();
        let response = router.oneshot(request).await.unwrap();
        let status = response.status();
        let headers = response.headers().clone();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        (status, headers, String::from_utf8_lossy(&bytes).into_owned())
    }

    async fn get(&self, uri: &str) -> (StatusCode, HeaderMap, String) {
        self.request(Method::GET, uri, None, "").await
    }

    async fn form(
        &self,
        method: Method,
        uri: &str,
        body: &str,
    ) -> (StatusCode, HeaderMap, String) {
        self.request(method, uri, Some("application/x-www-form-urlencoded"), body)
            .await
    }

    async fn json(
        &self,
        method: Method,
        uri: &str,
        body: &str,
    ) -> (StatusCode, HeaderMap, String) {
        self.request(method, uri, Some("application/json"), body).await
    }

    /// Load the asset fresh from BOTH stores for dual-store assertions.
    fn asset_from_both_stores(&self) -> (Asset, crate::catalog::SearchRow) {
        let store = MetadataStore::new(&self.root);
        let uuid: uuid::Uuid = self.asset_id.parse().unwrap();
        let sidecar = store.load(uuid).unwrap();
        let catalog = Catalog::open_fast(&self.root).unwrap();
        let row = catalog
            .get_search_row(&self.asset_id)
            .unwrap()
            .expect("seeded asset must exist in catalog");
        (sidecar, row)
    }
}

fn assert_pending_trigger(headers: &HeaderMap) {
    assert_eq!(
        headers.get("HX-Trigger").map(|v| v.to_str().unwrap()),
        Some("pending-changed"),
        "metadata-edit endpoints must fire the pending-changed trigger"
    );
}

// ─── Mutation endpoints ─────────────────────────────────────────────

#[tokio::test]
async fn set_rating_updates_both_stores_and_fires_trigger() {
    let srv = TestServer::new();
    let (status, headers, body) = srv
        .form(Method::PUT, &format!("/api/asset/{}/rating", srv.asset_id), "rating=4")
        .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_pending_trigger(&headers);

    let (sidecar, row) = srv.asset_from_both_stores();
    assert_eq!(sidecar.rating, Some(4), "YAML sidecar must carry the new rating");
    assert_eq!(row.rating, Some(4), "SQLite catalog must carry the new rating");
}

#[tokio::test]
async fn clear_rating_via_zero() {
    let srv = TestServer::new();
    // Seeded with rating=2; rating=0 means "clear".
    let (status, _, body) = srv
        .form(Method::PUT, &format!("/api/asset/{}/rating", srv.asset_id), "rating=0")
        .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");

    let (sidecar, row) = srv.asset_from_both_stores();
    assert_eq!(sidecar.rating, None);
    assert_eq!(row.rating, None);
}

#[tokio::test]
async fn add_tags_updates_both_stores_and_renders_fragment() {
    let srv = TestServer::new();
    let (status, headers, body) = srv
        .form(
            Method::POST,
            &format!("/api/asset/{}/tags", srv.asset_id),
            "tags=alpha,beta",
        )
        .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_pending_trigger(&headers);
    assert!(body.contains("alpha"), "tags fragment must render the new tag: {body}");

    let (sidecar, row) = srv.asset_from_both_stores();
    for t in ["alpha", "beta", "landscape"] {
        assert!(sidecar.tags.iter().any(|x| x == t), "sidecar missing tag {t}: {:?}", sidecar.tags);
        assert!(row.tags.iter().any(|x| x == t), "catalog missing tag {t}: {:?}", row.tags);
    }
}

#[tokio::test]
async fn remove_tag_updates_both_stores() {
    let srv = TestServer::new();
    let (status, headers, body) = srv
        .request(
            Method::DELETE,
            &format!("/api/asset/{}/tags?tag=landscape", srv.asset_id),
            None,
            "",
        )
        .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_pending_trigger(&headers);

    let (sidecar, row) = srv.asset_from_both_stores();
    assert!(!sidecar.tags.iter().any(|t| t == "landscape"), "sidecar: {:?}", sidecar.tags);
    assert!(!row.tags.iter().any(|t| t == "landscape"), "catalog: {:?}", row.tags);
}

#[tokio::test]
async fn set_description_updates_both_stores() {
    let srv = TestServer::new();
    let (status, headers, body) = srv
        .form(
            Method::PUT,
            &format!("/api/asset/{}/description", srv.asset_id),
            "description=golden+hour+at+the+beach",
        )
        .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_pending_trigger(&headers);

    let (sidecar, row) = srv.asset_from_both_stores();
    assert_eq!(sidecar.description.as_deref(), Some("golden hour at the beach"));
    assert_eq!(row.description.as_deref(), Some("golden hour at the beach"));
}

#[tokio::test]
async fn set_label_updates_both_stores() {
    let srv = TestServer::new();
    let (status, headers, body) = srv
        .form(Method::PUT, &format!("/api/asset/{}/label", srv.asset_id), "label=Red")
        .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_pending_trigger(&headers);

    let (sidecar, row) = srv.asset_from_both_stores();
    assert_eq!(sidecar.color_label.as_deref(), Some("Red"));
    assert_eq!(row.color_label.as_deref(), Some("Red"));
}

#[tokio::test]
async fn set_date_does_not_fire_pending_trigger() {
    // Date has no XMP-written equivalent, so the recipes block doesn't
    // need to refresh — the endpoint deliberately omits the trigger.
    let srv = TestServer::new();
    let (status, headers, body) = srv
        .form(Method::PUT, &format!("/api/asset/{}/date", srv.asset_id), "date=2024-07-15")
        .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert!(
        headers.get("HX-Trigger").is_none(),
        "set_date must not fire pending-changed"
    );

    let (sidecar, _) = srv.asset_from_both_stores();
    assert_eq!(sidecar.created_at.format("%Y-%m-%d").to_string(), "2024-07-15");
}

#[tokio::test]
async fn batch_rating_reports_per_asset_results() {
    let srv = TestServer::new();
    let body = format!(
        r#"{{"asset_ids": ["{}", "00000000-0000-0000-0000-000000000000"], "rating": 5}}"#,
        srv.asset_id
    );
    let (status, _, resp) = srv.json(Method::PUT, "/api/batch/rating", &body).await;
    assert_eq!(status, StatusCode::OK, "body: {resp}");
    let json: serde_json::Value = serde_json::from_str(&resp).unwrap();
    assert_eq!(json["succeeded"], 1, "resp: {resp}");
    assert_eq!(json["failed"], 1, "resp: {resp}");

    let (sidecar, row) = srv.asset_from_both_stores();
    assert_eq!(sidecar.rating, Some(5));
    assert_eq!(row.rating, Some(5));
}

#[tokio::test]
async fn mutation_on_unknown_asset_is_an_error() {
    // Current contract: unknown asset surfaces as a uniform 500 from
    // spawn_catalog_blocking (not a 404). If this is ever made a 404,
    // update this test deliberately.
    let srv = TestServer::new();
    let (status, _, _) = srv
        .form(
            Method::PUT,
            "/api/asset/ffffffff-ffff-ffff-ffff-ffffffffffff/rating",
            "rating=3",
        )
        .await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
}

// ─── Read endpoints (smoke) ─────────────────────────────────────────

#[tokio::test]
async fn browse_page_renders_seeded_asset() {
    let srv = TestServer::new();
    let (status, _, body) = srv.get("/").await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("sunset photo"), "browse page must show the asset");
}

#[tokio::test]
async fn asset_page_renders() {
    let srv = TestServer::new();
    let (status, _, body) = srv.get(&format!("/asset/{}", srv.asset_id)).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("sunset_beach.jpg"), "asset page must show the variant");
}

#[tokio::test]
async fn all_ids_returns_seeded_asset() {
    let srv = TestServer::new();
    let (status, _, body) = srv.get("/api/all-ids").await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    let ids: Vec<String> = json["ids"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();
    assert!(ids.contains(&srv.asset_id), "ids: {ids:?}");
    assert_eq!(json["total"], 1);
}

// ─── Access guards: read-only mode + basic auth ─────────────────────

#[tokio::test]
async fn read_only_blocks_mutations_but_allows_reads() {
    let srv = TestServer::new_with(true, "");

    let (status, _, _) = srv.get("/").await;
    assert_eq!(status, StatusCode::OK, "reads must work in read-only mode");

    let (status, _, _) = srv
        .form(Method::PUT, &format!("/api/asset/{}/rating", srv.asset_id), "rating=4")
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    let (status, _, _) = srv
        .json(Method::POST, "/api/maintain/writeback", "{}")
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "job launches must be blocked too");

    // Nothing may have changed in either store.
    let (sidecar, row) = srv.asset_from_both_stores();
    assert_eq!(sidecar.rating, Some(2), "sidecar untouched");
    assert_eq!(row.rating, Some(2), "catalog untouched");
}

#[tokio::test]
async fn read_only_from_config_file() {
    let srv = TestServer::new_with(false, "[serve]\nread_only = true\n");
    let (status, _, _) = srv
        .form(Method::PUT, &format!("/api/asset/{}/rating", srv.asset_id), "rating=4")
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "[serve] read_only must enforce like the flag");
}

#[tokio::test]
async fn basic_auth_gates_every_route() {
    let srv = TestServer::new_with(
        false,
        "[serve]\nusername = \"thomas\"\npassword = \"secret\"\n",
    );

    // No credentials → 401 with the challenge header, on pages and API alike.
    for uri in ["/", "/api/all-ids", "/static/style.css"] {
        let (status, headers, _) = srv.get(uri).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{uri} must require auth");
        assert!(
            headers.get("WWW-Authenticate").is_some(),
            "{uri} must send the Basic challenge"
        );
    }

    // Wrong credentials → 401. base64("thomas:wrong") = dGhvbWFzOndyb25n
    let router = build_router(srv.state.clone());
    let request = Request::builder()
        .method(Method::GET)
        .uri("/")
        .header("authorization", "Basic dGhvbWFzOndyb25n")
        .body(Body::empty())
        .unwrap();
    let response = router.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

    // Correct credentials → 200. base64("thomas:secret") = dGhvbWFzOnNlY3JldA==
    let router = build_router(srv.state.clone());
    let request = Request::builder()
        .method(Method::GET)
        .uri("/")
        .header("authorization", "Basic dGhvbWFzOnNlY3JldA==")
        .body(Body::empty())
        .unwrap();
    let response = router.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn build_info_reports_read_only() {
    let srv = TestServer::new_with(true, "");
    let (status, _, body) = srv.get("/api/build-info").await;
    assert_eq!(status, StatusCode::OK);
    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(json["read_only"], true);
}

#[test]
fn base64_encode_matches_known_vectors() {
    // RFC 4648 test vectors plus the credential shape used by basic auth.
    assert_eq!(super::base64_encode(b""), "");
    assert_eq!(super::base64_encode(b"f"), "Zg==");
    assert_eq!(super::base64_encode(b"fo"), "Zm8=");
    assert_eq!(super::base64_encode(b"foo"), "Zm9v");
    assert_eq!(super::base64_encode(b"foobar"), "Zm9vYmFy");
    assert_eq!(super::base64_encode(b"thomas:secret"), "dGhvbWFzOnNlY3JldA==");
}

#[tokio::test]
async fn all_ids_respects_filters() {
    // The select-all backend must honor the same filters as the grid —
    // this is the endpoint behind the v4.6.0 text-query trap fix.
    let srv = TestServer::new();
    let (status, _, body) = srv.get("/api/all-ids?type=video").await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(json["total"], 0, "no videos seeded, select-all must see none");
}
