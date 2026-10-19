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
            // `maki init` also creates the (empty) volume registry; the
            // stats/volumes/backup pages and path completion read it.
            crate::device_registry::DeviceRegistry::init(&root).unwrap();

            let mut asset = Asset::new(AssetType::Image, "sha256:webtest1");
            asset.name = Some("sunset photo".to_string());
            asset.tags = vec!["landscape".to_string()];
            asset.rating = Some(2);
            // One machine-added tag for the provenance-badge tests; the
            // "landscape" tag stays user-sourced (absent from the map).
            asset.add_tags_with_source(
                &["festival".to_string()],
                crate::models::TagSource::Vlm,
            );
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

        // `[ai]` from the supplied maki.toml (tests point `model_dir` at a
        // nonexistent path so the lazy SigLIP load fails deterministically
        // even on a dev machine that has the model downloaded).
        #[cfg(feature = "ai")]
        let ai_config = if maki_toml.is_empty() {
            crate::config::AiConfig::default()
        } else {
            crate::config::CatalogConfig::load(&root)
                .map(|c| c.ai)
                .unwrap_or_default()
        };
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
            ai_config,
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

    /// Raw-body request with arbitrary headers (htmx fragments,
    /// query-image upload).
    async fn raw(
        &self,
        method: Method,
        uri: &str,
        headers: &[(&str, &str)],
        body: Vec<u8>,
    ) -> (StatusCode, HeaderMap, String) {
        let router = build_router(self.state.clone());
        let mut builder = Request::builder().method(method).uri(uri);
        for (k, v) in headers {
            builder = builder.header(*k, *v);
        }
        let request = builder.body(Body::from(body)).unwrap();
        let response = router.oneshot(request).await.unwrap();
        let status = response.status();
        let headers = response.headers().clone();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        (status, headers, String::from_utf8_lossy(&bytes).into_owned())
    }

    /// Seed a second image asset whose only variant has exactly `hash`
    /// (so a query-image upload with those bytes is a content-hash hit,
    /// and so stack/batch tests have a second member).
    fn seed_asset_with_hash(&self, hash: &str) -> String {
        let catalog = Catalog::open_fast(&self.root).unwrap();
        let mut asset = Asset::new(AssetType::Image, hash);
        asset.name = Some("the original".to_string());
        asset.variants.push(Variant {
            content_hash: hash.to_string(),
            asset_id: asset.id,
            role: VariantRole::Original,
            format: "jpg".to_string(),
            file_size: 11,
            original_filename: "original.jpg".to_string(),
            source_metadata: Default::default(),
            locations: vec![],
        });
        MetadataStore::new(&self.root).save(&asset).unwrap();
        catalog.insert_asset(&asset).unwrap();
        catalog.insert_variant(&asset.variants[0]).unwrap();
        asset.id.to_string()
    }

    /// Register an online volume at the catalog root and attach a file
    /// location for `hash` under it. Returns the volume ID.
    fn seed_volume_and_location(&self, label: &str, hash: &str, relative_path: &str) -> String {
        let registry = crate::device_registry::DeviceRegistry::new(&self.root);
        let volume = registry
            .register(label, &self.root, crate::models::VolumeType::Local, None)
            .unwrap();
        let catalog = Catalog::open_fast(&self.root).unwrap();
        catalog.ensure_volume(&volume).unwrap();
        catalog
            .insert_file_location(
                hash,
                &crate::models::FileLocation {
                    volume_id: volume.id,
                    relative_path: std::path::PathBuf::from(relative_path),
                    verified_at: None,
                },
            )
            .unwrap();
        volume.id.to_string()
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
async fn asset_page_shows_provenance_badge_on_machine_tags_only() {
    let srv = TestServer::new();
    let (status, _, body) = srv.get(&format!("/asset/{}", srv.asset_id)).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains(">vlm</span>"), "vlm tag must carry a badge");
    assert_eq!(
        body.matches("tag-source-badge").count(),
        1,
        "exactly one badge — the user tag must not get one"
    );
}

#[tokio::test]
async fn tags_fragment_keeps_badges_after_edit() {
    // Adding a user tag re-renders the fragment; the vlm badge on the
    // pre-existing machine tag must survive (TagResult carries sources).
    let srv = TestServer::new();
    let (status, _, body) = srv
        .form(
            Method::POST,
            &format!("/api/asset/{}/tags", srv.asset_id),
            "tags=newone",
        )
        .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert!(body.contains("newone"));
    assert!(body.contains(">vlm</span>"), "badge must survive the edit: {body}");
    assert_eq!(body.matches("tag-source-badge").count(), 1);
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

#[tokio::test]
async fn build_info_reports_import_defaults_from_config() {
    // The import dialog reads these to set its optional-step checkboxes;
    // they must reflect [import] config. Default (no config) is false.
    let srv = TestServer::new();
    let (status, _, body) = srv.get("/api/build-info").await;
    assert_eq!(status, StatusCode::OK);
    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(json["import_smart_previews"], false);
    assert_eq!(json["import_embeddings"], false);

    // …and true when the config opts in.
    let srv = TestServer::new_with(
        false,
        "[import]\nsmart_previews = true\nembeddings = true\n",
    );
    let (status, _, body) = srv.get("/api/build-info").await;
    assert_eq!(status, StatusCode::OK);
    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(json["import_smart_previews"], true, "body: {body}");
    assert_eq!(json["import_embeddings"], true, "body: {body}");
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

#[cfg(feature = "ai")]
#[tokio::test]
async fn all_ids_honors_person_filters_including_exclude() {
    // `-person:` was silently ignored by the web layer until v4.7.1 —
    // this locks the closed gap end-to-end through the select-all path.
    let srv = TestServer::new();
    {
        let catalog = Catalog::open_fast(&srv.root).unwrap();
        crate::face_store::FaceStore::initialize(catalog.conn()).unwrap();
        let fs = crate::face_store::FaceStore::new(catalog.conn());
        fs.store_face("face-1", &srv.asset_id, 0.1, 0.1, 0.2, 0.2, &[0.5; 4], 0.9, "arcface")
            .unwrap();
        let pid = fs.create_person(Some("Alice")).unwrap();
        fs.assign_face_to_person("face-1", &pid).unwrap();
    }

    let (status, _, body) = srv.get("/api/all-ids?q=person%3AAlice").await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(json["total"], 1, "positive person filter must match: {body}");

    let (status, _, body) = srv.get("/api/all-ids?q=-person%3AAlice").await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(json["total"], 0, "-person: must exclude the asset: {body}");
}

#[tokio::test]
async fn stale_similarity_sort_without_scores_falls_back_to_date() {
    // A similarity sort left in the URL after the `similar:` filter is
    // gone must not order the grid by the SQL fallback (asset id).
    let srv = TestServer::new();
    let (status, _, body) = srv.get("/?sort=similarity_desc").await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        body.contains("Date<span class=\"sort-arrow\">"),
        "date sort must be the active one:\n{body}"
    );
    assert!(!body.contains("Similarity<span class=\"sort-arrow\">"));
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

// ─── Search by query image ──────────────────────────────────────────────
//
// The model is never available in tests (`[ai] model_dir` points at a
// path that does not exist), so these exercise the content-hash fast path
// and the session/token plumbing — the parts that must work regardless of
// whether SigLIP is downloaded.

#[cfg(feature = "ai")]
const NO_MODEL_TOML: &str = "[ai]\nmodel_dir = \"/nonexistent/maki-test-models\"\n";

#[cfg(feature = "ai")]
fn sha256_of(bytes: &[u8]) -> String {
    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("q.bin");
    std::fs::write(&f, bytes).unwrap();
    crate::content_store::ContentStore::new(dir.path()).hash_file(&f).unwrap()
}

#[cfg(feature = "ai")]
#[tokio::test]
async fn query_image_exact_match_roundtrip_through_browse() {
    let srv = TestServer::new_with(false, NO_MODEL_TOML);
    let bytes = b"not really a jpeg, but byte-identical to the variant".to_vec();
    let original_id = srv.seed_asset_with_hash(&sha256_of(&bytes));

    // Upload → token + exact match, model failure tolerated with a warning
    let (status, _, body) = srv
        .raw(
            Method::POST,
            "/api/query-image",
            &[("content-type", "image/jpeg"), ("x-maki-filename", "IMG%204711.jpg")],
            bytes.clone(),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    let token = json["token"].as_str().unwrap().to_string();
    assert_eq!(token.len(), 32);
    assert_eq!(json["filename"], "IMG 4711.jpg", "filename header is percent-decoded");
    assert_eq!(json["exact_match_id"], original_id);
    assert_eq!(json["embedded"], false);
    assert!(
        json["warning"].as_str().unwrap_or("").contains("not downloaded"),
        "warning must explain the missing model: {body}"
    );

    // Session lookup for the pill
    let (status, _, body) = srv.get(&format!("/api/query-image/{token}")).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(json["exact_match_id"], original_id);

    // Select-all sees exactly the pinned exact match
    let (status, _, body) = srv.get(&format!("/api/all-ids?q=similar%3A%40{token}")).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(json["total"], 1, "only the exact match: {body}");
    assert_eq!(json["ids"][0], original_id);

    // The grid renders it as a similarity view with the 100% badge
    let (status, _, body) = srv.get(&format!("/?q=similar%3A%40{token}")).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains(&original_id), "grid must show the exact match");
    assert!(body.contains("100%"), "exact match carries a 100% similarity badge");
    assert!(!body.contains(&srv.asset_id), "unrelated seeded asset must not appear");

    // Other filters still compose: an impossible type narrows to nothing
    let (_, _, body) = srv.get(&format!("/api/all-ids?q=similar%3A%40{token}&type=video")).await;
    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(json["total"], 0);
}

#[cfg(feature = "ai")]
#[tokio::test]
async fn similarity_view_defaults_to_score_order_unless_sort_is_explicit() {
    // The browse form submits `sort=` from the URL, empty when the URL has
    // none. That empty value must count as "no sort given" so a
    // similarity view lands on score order; an explicit sort still wins.
    let srv = TestServer::new_with(false, NO_MODEL_TOML);
    let bytes = b"sort-order query bytes".to_vec();
    let _ = srv.seed_asset_with_hash(&sha256_of(&bytes));
    let (_, _, body) = srv
        .raw(
            Method::POST,
            "/api/query-image",
            &[("content-type", "image/jpeg"), ("x-maki-filename", "q.jpg")],
            bytes,
        )
        .await;
    let token = serde_json::from_str::<serde_json::Value>(&body).unwrap()["token"]
        .as_str()
        .unwrap()
        .to_string();

    for uri in [
        format!("/?q=similar%3A%40{token}"),
        format!("/?q=similar%3A%40{token}&sort="),
    ] {
        let (status, _, body) = srv.get(&uri).await;
        assert_eq!(status, StatusCode::OK, "{uri}: {body}");
        assert!(
            body.contains("Similarity<span class=\"sort-arrow\">"),
            "{uri}: similarity sort must be active\n{body}"
        );
    }
    let (_, _, body) = srv.get(&format!("/?q=similar%3A%40{token}&sort=date_desc")).await;
    assert!(
        !body.contains("Similarity<span class=\"sort-arrow\">"),
        "explicit sort=date_desc must win over the similarity default"
    );
}

#[cfg(feature = "ai")]
#[tokio::test]
async fn query_image_expired_token_is_empty_not_an_error() {
    let srv = TestServer::new_with(false, NO_MODEL_TOML);
    let (status, _, _) = srv.get("/api/query-image/deadbeefdeadbeefdeadbeefdeadbeef").await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // A stale `similar:@token` in the URL (bookmark, remembered filter)
    // must render an empty grid, not a 500 page.
    let (status, _, body) = srv.get("/?q=similar%3A%40deadbeefdeadbeefdeadbeefdeadbeef").await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert!(!body.contains(&srv.asset_id));
    let (status, _, body) = srv.get("/api/all-ids?q=similar%3A%40deadbeefdeadbeefdeadbeefdeadbeef").await;
    assert_eq!(status, StatusCode::OK);
    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(json["total"], 0);
}

#[cfg(feature = "ai")]
#[tokio::test]
async fn query_image_without_match_and_without_model_is_rejected() {
    let srv = TestServer::new_with(false, NO_MODEL_TOML);
    let (status, _, body) = srv
        .raw(
            Method::POST,
            "/api/query-image",
            &[("content-type", "image/png"), ("x-maki-filename", "unknown.png")],
            b"no such bytes in the catalog".to_vec(),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "body: {body}");
    assert!(body.contains("not downloaded"), "body: {body}");

    let (status, _, _) = srv
        .raw(Method::POST, "/api/query-image", &[("content-type", "image/png")], Vec::new())
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "empty upload");
}

#[cfg(feature = "ai")]
#[tokio::test]
async fn query_image_upload_is_allowed_in_read_only_mode() {
    // The upload is a POST but touches nothing in the catalog — a
    // read-only viewer may still search by image.
    let srv = TestServer::new_with(true, NO_MODEL_TOML);
    let bytes = b"read-only query bytes".to_vec();
    let original_id = srv.seed_asset_with_hash(&sha256_of(&bytes));
    let (status, _, body) = srv
        .raw(
            Method::POST,
            "/api/query-image",
            &[("content-type", "image/jpeg"), ("x-maki-filename", "q.jpg")],
            bytes,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(json["exact_match_id"], original_id);

    // …while real mutations stay blocked.
    let (status, _, _) = srv
        .form(Method::POST, &format!("/api/asset/{}/rating", srv.asset_id), "rating=5")
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

// ─── Route smoke coverage: JSON APIs, HTML pages, batch/tag endpoints ─

#[tokio::test]
async fn page_ids_counts_pages_unless_nocount() {
    let srv = TestServer::new();

    let (status, _, body) = srv.get("/api/page-ids").await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(json["page"], 1);
    assert_eq!(json["total_pages"], 1, "one seeded asset fits one page: {json}");
    let ids: Vec<&str> = json["ids"].as_array().unwrap().iter().map(|v| v.as_str().unwrap()).collect();
    assert_eq!(ids, vec![srv.asset_id.as_str()]);

    // nocount=1 skips the count query: total_pages is null, IDs unchanged.
    let (status, _, body) = srv.get("/api/page-ids?nocount=1").await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert!(json["total_pages"].is_null(), "nocount must suppress the count: {json}");
    assert_eq!(json["ids"].as_array().unwrap().len(), 1);

    // Filters apply: a type nobody matches yields an empty page.
    let (status, _, body) = srv.get("/api/page-ids?type=video").await;
    assert_eq!(status, StatusCode::OK);
    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(json["ids"].as_array().unwrap().len(), 0, "{json}");
    assert_eq!(json["total_pages"], 0, "{json}");
}

#[tokio::test]
async fn facets_api_reports_counts_for_the_result_set() {
    let srv = TestServer::new();
    let (status, _, body) = srv.get("/api/facets").await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    let json: serde_json::Value = serde_json::from_str(&body).unwrap();

    assert_eq!(json["total"], 1, "{json}");
    for key in ["ratings", "labels", "formats", "volumes", "tags", "years"] {
        assert!(json[key].is_array(), "{key} must be an array: {json}");
    }
    assert_eq!(json["geotagged"], 0, "{json}");

    // Seeded asset: rating 2, format jpg, tags landscape + festival.
    let ratings = json["ratings"].as_array().unwrap();
    assert!(ratings.iter().any(|r| r[0] == 2 && r[1] == 1), "ratings: {ratings:?}");
    let formats = json["formats"].as_array().unwrap();
    assert!(formats.iter().any(|f| f[0] == "jpg" && f[1] == 1), "formats: {formats:?}");
    let tags = json["tags"].as_array().unwrap();
    assert!(tags.iter().any(|t| t[0] == "landscape" && t[1] == 1), "tags: {tags:?}");
    assert!(tags.iter().any(|t| t[0] == "festival"), "tags: {tags:?}");

    // Facets follow the filter: nothing is a video.
    let (_, _, body) = srv.get("/api/facets?type=video").await;
    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(json["total"], 0, "{json}");
}

#[tokio::test]
async fn search_api_redirects_without_htmx_and_renders_fragment_with_it() {
    let srv = TestServer::new();

    // Plain GET (bookmark / direct navigation) is sent to the browse page
    // with the query string preserved.
    let (status, headers, _) = srv.get("/api/search?q=sunset&type=image").await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert_eq!(
        headers.get("location").map(|v| v.to_str().unwrap()),
        Some("/?q=sunset&type=image")
    );
    let (status, headers, _) = srv.get("/api/search").await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert_eq!(headers.get("location").map(|v| v.to_str().unwrap()), Some("/"));

    // htmx request: results fragment with the seeded card, no full page.
    let (status, _, body) = srv
        .raw(Method::GET, "/api/search?q=sunset", &[("HX-Request", "true")], Vec::new())
        .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert!(body.contains("asset-card"), "fragment must render result cards: {body}");
    assert!(body.contains(&srv.asset_id), "fragment must contain the seeded asset: {body}");
    assert!(!body.contains("<html"), "htmx fragment must not be a full document");

    let (status, _, body) = srv
        .raw(Method::GET, "/api/search?type=video", &[("HX-Request", "true")], Vec::new())
        .await;
    assert_eq!(status, StatusCode::OK);
    assert!(!body.contains(&srv.asset_id), "filtered-out asset must not render: {body}");
}

#[tokio::test]
async fn stats_api_reports_overview_counts() {
    let srv = TestServer::new();
    let (status, _, body) = srv.get("/api/stats").await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(json["assets"], 1, "{json}");
    assert_eq!(json["variants"], 1, "{json}");
    assert_eq!(json["recipes"], 0, "{json}");
    assert_eq!(json["total_size"], 5000, "{json}");
}

#[tokio::test]
async fn jobs_list_starts_empty() {
    let srv = TestServer::new();
    let (status, _, body) = srv.get("/api/jobs").await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(json["running"], 0, "{json}");
    assert_eq!(json["jobs"].as_array().unwrap().len(), 0, "{json}");

    let (status, _, _) = srv.get("/api/jobs/no-such-job").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn config_api_and_schema() {
    let srv = TestServer::new_with(false, "[serve]\nper_page = 42\n");

    let (status, _, body) = srv.get("/api/config").await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(json["config"]["serve"]["per_page"], 42, "{json}");
    assert!(json["raw_toml"].as_str().unwrap().contains("per_page = 42"));
    assert!(json["path"].as_str().unwrap().ends_with("maki.toml"));

    let (status, _, body) = srv.get("/api/config/schema").await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    let schema: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert!(schema["properties"]["serve"].is_object(), "schema must describe [serve]: {schema}");
    assert!(schema["properties"]["preview"].is_object(), "schema must describe [preview]: {schema}");
}

#[tokio::test]
async fn collections_create_list_and_batch_membership() {
    let srv = TestServer::new();

    let (status, _, body) = srv.get("/api/collections").await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(json.as_array().unwrap().len(), 0);

    let (status, _, body) = srv
        .json(Method::POST, "/api/collections", r#"{"name": "Best of", "description": "  keepers  "}"#)
        .await;
    assert_eq!(status, StatusCode::CREATED, "body: {body}");
    let created: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(created["name"], "Best of");
    assert_eq!(created["description"], "keepers", "description is trimmed: {created}");
    assert!(uuid::Uuid::parse_str(created["id"].as_str().unwrap()).is_ok());

    // Duplicate name → 409; empty name → error.
    let (status, _, _) = srv
        .json(Method::POST, "/api/collections", r#"{"name": "Best of"}"#)
        .await;
    assert_eq!(status, StatusCode::CONFLICT);
    let (status, _, _) = srv
        .json(Method::POST, "/api/collections", r#"{"name": "   "}"#)
        .await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);

    // Add the seeded asset, list shows membership, remove again.
    let body = format!(r#"{{"asset_ids": ["{}"], "collection": "Best of"}}"#, srv.asset_id);
    let (status, _, resp) = srv.json(Method::POST, "/api/batch/collection", &body).await;
    assert_eq!(status, StatusCode::OK, "body: {resp}");
    let json: serde_json::Value = serde_json::from_str(&resp).unwrap();
    assert_eq!(json["added"], 1, "{json}");

    let (_, _, body_list) = srv.get("/api/collections").await;
    let json: serde_json::Value = serde_json::from_str(&body_list).unwrap();
    let list = json.as_array().unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0]["name"], "Best of");
    assert_eq!(list[0]["asset_count"], 1, "{json}");

    // The YAML export (source of truth) must carry the collection too.
    let yaml = std::fs::read_to_string(srv.root.join("collections.yaml")).unwrap();
    assert!(yaml.contains("Best of") && yaml.contains(&srv.asset_id), "{yaml}");

    let (status, _, resp) = srv.json(Method::DELETE, "/api/batch/collection", &body).await;
    assert_eq!(status, StatusCode::OK, "body: {resp}");
    let json: serde_json::Value = serde_json::from_str(&resp).unwrap();
    assert_eq!(json["removed"], 1, "{json}");
    let (_, _, body_list) = srv.get("/api/collections").await;
    let json: serde_json::Value = serde_json::from_str(&body_list).unwrap();
    assert_eq!(json[0]["asset_count"], 0, "{json}");
}

#[tokio::test]
async fn saved_searches_create_list_delete() {
    let srv = TestServer::new();

    let (status, _, body) = srv.get("/api/saved-searches").await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(serde_json::from_str::<serde_json::Value>(&body).unwrap().as_array().unwrap().len(), 0);

    let (status, _, body) = srv
        .json(
            Method::POST,
            "/api/saved-searches",
            r#"{"name": "five stars", "query": "rating:5", "sort": "date_asc", "favorite": true}"#,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(json["status"], "saved");
    assert_eq!(json["name"], "five stars");

    // Same name updates in place rather than duplicating.
    let (status, _, _) = srv
        .json(
            Method::POST,
            "/api/saved-searches",
            r#"{"name": "five stars", "query": "rating:4+"}"#,
        )
        .await;
    assert_eq!(status, StatusCode::OK);

    let (_, _, body) = srv.get("/api/saved-searches").await;
    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    let list = json.as_array().unwrap();
    assert_eq!(list.len(), 1, "{json}");
    assert_eq!(list[0]["name"], "five stars");
    assert_eq!(list[0]["query"], "rating:4+", "update must replace the query: {json}");
    assert_eq!(list[0]["favorite"], false, "update replaces the whole entry: {json}");

    let (status, _, body) = srv.get("/saved-searches").await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("five stars"), "page must list the saved search: {body}");

    let (status, _, body) = srv
        .request(Method::DELETE, "/api/saved-searches/five%20stars", None, "")
        .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(json["status"], "deleted");

    let (status, _, _) = srv
        .request(Method::DELETE, "/api/saved-searches/five%20stars", None, "")
        .await;
    assert_ne!(status, StatusCode::OK, "deleting twice must fail");

    let (_, _, body) = srv.get("/api/saved-searches").await;
    assert_eq!(serde_json::from_str::<serde_json::Value>(&body).unwrap().as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn tag_rename_split_delete_apis_update_both_stores() {
    let srv = TestServer::new();

    // rename: dry run first, then apply.
    let (status, _, body) = srv
        .json(Method::POST, "/api/tag/rename", r#"{"old_tag": "landscape", "new_tag": "scenery"}"#)
        .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(json["dry_run"], true, "{json}");
    assert_eq!(json["matched"], 1, "{json}");
    let (sidecar, _) = srv.asset_from_both_stores();
    assert!(sidecar.tags.contains(&"landscape".to_string()), "dry run must not rename");

    let (status, _, body) = srv
        .json(
            Method::POST,
            "/api/tag/rename",
            r#"{"old_tag": "landscape", "new_tag": "scenery", "apply": true}"#,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(json["renamed"], 1, "{json}");
    let (sidecar, row) = srv.asset_from_both_stores();
    assert!(sidecar.tags.contains(&"scenery".to_string()) && !sidecar.tags.contains(&"landscape".to_string()), "{:?}", sidecar.tags);
    assert!(row.tags.contains(&"scenery".to_string()) && !row.tags.contains(&"landscape".to_string()), "{:?}", row.tags);

    // Empty target is a 400.
    let (status, _, _) = srv
        .json(Method::POST, "/api/tag/rename", r#"{"old_tag": "scenery", "new_tag": "", "apply": true}"#)
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    // split: one tag into two.
    let (status, _, body) = srv
        .json(
            Method::POST,
            "/api/tag/split",
            r#"{"old_tag": "scenery", "new_tags": ["nature", "outdoors"], "apply": true}"#,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(json["matched"], 1, "{json}");
    assert_eq!(json["split"], 1, "{json}");
    assert_eq!(json["dry_run"], false, "{json}");
    let (sidecar, row) = srv.asset_from_both_stores();
    for t in ["nature", "outdoors"] {
        assert!(sidecar.tags.contains(&t.to_string()), "sidecar missing {t}: {:?}", sidecar.tags);
        assert!(row.tags.contains(&t.to_string()), "catalog missing {t}: {:?}", row.tags);
    }
    assert!(!sidecar.tags.contains(&"scenery".to_string()));

    let (status, _, _) = srv
        .json(Method::POST, "/api/tag/split", r#"{"old_tag": "nature", "new_tags": [""], "apply": true}"#)
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    // delete: removes from every asset.
    let (status, _, body) = srv
        .json(Method::POST, "/api/tag/delete", r#"{"tag": "outdoors", "apply": true}"#)
        .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(json["matched"], 1, "{json}");
    assert_eq!(json["removed"], 1, "{json}");
    let (sidecar, row) = srv.asset_from_both_stores();
    assert!(!sidecar.tags.contains(&"outdoors".to_string()), "{:?}", sidecar.tags);
    assert!(!row.tags.contains(&"outdoors".to_string()), "{:?}", row.tags);

    let (status, _, _) = srv
        .json(Method::POST, "/api/tag/delete", r#"{"tag": "=", "apply": true}"#)
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn batch_label_and_batch_tags_report_per_asset_results() {
    let srv = TestServer::new();
    let bogus = "00000000-0000-0000-0000-000000000000";

    let body = format!(r#"{{"asset_ids": ["{}", "{bogus}"], "label": "red"}}"#, srv.asset_id);
    let (status, _, resp) = srv.json(Method::PUT, "/api/batch/label", &body).await;
    assert_eq!(status, StatusCode::OK, "body: {resp}");
    let json: serde_json::Value = serde_json::from_str(&resp).unwrap();
    assert_eq!(json["succeeded"], 1, "{json}");
    assert_eq!(json["failed"], 1, "{json}");
    assert_eq!(json["errors"][0]["asset_id"], bogus, "{json}");
    let (sidecar, row) = srv.asset_from_both_stores();
    assert_eq!(sidecar.color_label.as_deref(), Some("Red"), "label is canonicalised");
    assert_eq!(row.color_label.as_deref(), Some("Red"));

    // Invalid colour is rejected before anything is written.
    let body = format!(r#"{{"asset_ids": ["{}"], "label": "magenta"}}"#, srv.asset_id);
    let (status, _, _) = srv.json(Method::PUT, "/api/batch/label", &body).await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    let (sidecar, _) = srv.asset_from_both_stores();
    assert_eq!(sidecar.color_label.as_deref(), Some("Red"));

    // Empty label clears.
    let body = format!(r#"{{"asset_ids": ["{}"], "label": ""}}"#, srv.asset_id);
    let (status, _, _) = srv.json(Method::PUT, "/api/batch/label", &body).await;
    assert_eq!(status, StatusCode::OK);
    let (sidecar, row) = srv.asset_from_both_stores();
    assert_eq!(sidecar.color_label, None);
    assert_eq!(row.color_label, None);

    // batch tags: add, then remove.
    let body = format!(
        r#"{{"asset_ids": ["{}", "{bogus}"], "tags": ["trip|italy"], "remove": false}}"#,
        srv.asset_id
    );
    let (status, _, resp) = srv.json(Method::POST, "/api/batch/tags", &body).await;
    assert_eq!(status, StatusCode::OK, "body: {resp}");
    let json: serde_json::Value = serde_json::from_str(&resp).unwrap();
    assert_eq!(json["succeeded"], 1, "{json}");
    assert_eq!(json["failed"], 1, "{json}");
    let (sidecar, row) = srv.asset_from_both_stores();
    for t in ["trip", "trip|italy"] {
        assert!(sidecar.tags.contains(&t.to_string()), "sidecar missing {t}: {:?}", sidecar.tags);
        assert!(row.tags.contains(&t.to_string()), "catalog missing {t}: {:?}", row.tags);
    }

    let body = format!(
        r#"{{"asset_ids": ["{}"], "tags": ["trip|italy"], "remove": true}}"#,
        srv.asset_id
    );
    let (status, _, resp) = srv.json(Method::POST, "/api/batch/tags", &body).await;
    assert_eq!(status, StatusCode::OK, "body: {resp}");
    let (sidecar, row) = srv.asset_from_both_stores();
    assert!(!sidecar.tags.contains(&"trip|italy".to_string()), "{:?}", sidecar.tags);
    assert!(!row.tags.contains(&"trip|italy".to_string()), "{:?}", row.tags);
}

#[tokio::test]
async fn paths_api_completes_next_segment() {
    let srv = TestServer::new();
    let hash = "sha256:webtest1";
    let volume_id = srv.seed_volume_and_location("main", hash, "trips/2026/beach.jpg");
    {
        let catalog = Catalog::open_fast(&srv.root).unwrap();
        catalog
            .insert_file_location(
                hash,
                &crate::models::FileLocation {
                    volume_id: volume_id.parse().unwrap(),
                    relative_path: std::path::PathBuf::from("trips/2025/snow.jpg"),
                    verified_at: None,
                },
            )
            .unwrap();
    }

    let (status, _, body) = srv.get("/api/paths").await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(json, serde_json::json!(["trips/"]), "top level is the directory: {json}");

    let (_, _, body) = srv.get("/api/paths?q=trips/").await;
    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(json, serde_json::json!(["trips/2025/", "trips/2026/"]), "{json}");

    let (_, _, body) = srv.get(&format!("/api/paths?q=trips/2026/&volume={volume_id}")).await;
    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(json, serde_json::json!(["trips/2026/beach.jpg"]), "file leaf has no slash: {json}");

    // Wildcards and unknown volumes short-circuit to an empty list.
    let (_, _, body) = srv.get("/api/paths?q=trips/*").await;
    assert_eq!(serde_json::from_str::<serde_json::Value>(&body).unwrap(), serde_json::json!([]));
    let (_, _, body) = srv.get("/api/paths?q=trips/&volume=00000000-0000-0000-0000-000000000000").await;
    assert_eq!(serde_json::from_str::<serde_json::Value>(&body).unwrap(), serde_json::json!([]));
}

#[tokio::test]
async fn calendar_api_reports_year_counts() {
    let srv = TestServer::new();
    let (sidecar, _) = srv.asset_from_both_stores();
    let year = sidecar.created_at.format("%Y").to_string();
    let day = sidecar.created_at.format("%Y-%m-%d").to_string();

    let (status, _, body) = srv.get("/api/calendar").await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(json["year"].to_string(), year, "defaults to the current year: {json}");
    assert_eq!(json["counts"][&day], 1, "seeded asset counts on its day: {json}");
    let years: Vec<i64> = json["years"].as_array().unwrap().iter().map(|y| y.as_i64().unwrap()).collect();
    assert_eq!(years, vec![year.parse::<i64>().unwrap()], "{json}");

    // Explicit year without assets: empty counts, same year list.
    let (status, _, body) = srv.get("/api/calendar?year=1999").await;
    assert_eq!(status, StatusCode::OK);
    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(json["year"], 1999);
    assert_eq!(json["counts"].as_object().unwrap().len(), 0, "{json}");
}

#[tokio::test]
async fn map_api_reports_geotagged_markers() {
    let srv = TestServer::new();

    let (status, _, body) = srv.get("/api/map").await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(json["markers"].as_array().unwrap().len(), 0, "seeded asset has no GPS: {json}");
    assert_eq!(json["total"], 0, "{json}");
    assert_eq!(json["truncated"], false, "{json}");

    // Geotag the asset through the catalog and it shows up as a marker.
    {
        let catalog = Catalog::open_fast(&srv.root).unwrap();
        catalog
            .conn()
            .execute(
                "UPDATE assets SET latitude = 48.1371, longitude = 11.5754 WHERE id = ?1",
                rusqlite::params![srv.asset_id],
            )
            .unwrap();
    }
    let (status, _, body) = srv.get("/api/map?limit=10").await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    let markers = json["markers"].as_array().unwrap();
    assert_eq!(markers.len(), 1, "{json}");
    assert_eq!(markers[0]["id"], srv.asset_id, "{json}");
    assert!((markers[0]["lat"].as_f64().unwrap() - 48.1371).abs() < 1e-6, "{json}");
    assert!((markers[0]["lng"].as_f64().unwrap() - 11.5754).abs() < 1e-6, "{json}");
    assert_eq!(markers[0]["name"], "sunset photo", "{json}");
    assert_eq!(markers[0]["rating"], 2, "{json}");
    assert_eq!(json["total"], 1, "{json}");
}

#[tokio::test]
async fn volumes_api_lists_registered_volumes() {
    let srv = TestServer::new();

    let (status, _, body) = srv.get("/api/volumes").await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(serde_json::from_str::<serde_json::Value>(&body).unwrap(), serde_json::json!([]));

    let volume_id = srv.seed_volume_and_location("main", "sha256:webtest1", "sunset_beach.jpg");

    let (status, _, body) = srv.get("/api/volumes").await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    let list = json.as_array().unwrap();
    assert_eq!(list.len(), 1, "{json}");
    assert_eq!(list[0]["id"], volume_id, "{json}");
    assert_eq!(list[0]["label"], "main", "{json}");
    assert_eq!(list[0]["volume_type"], "local", "{json}");
    assert!(list[0]["purpose"].is_null(), "{json}");
    assert_eq!(list[0]["is_online"], true, "mount point is the temp dir, so online: {json}");

    let (status, _, body) = srv.get("/volumes").await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("Volumes - MAKI") && body.contains("main"), "{body}");
}

#[tokio::test]
async fn stack_members_after_batch_stack() {
    let srv = TestServer::new();
    let second = srv.seed_asset_with_hash("sha256:webtest2");

    // A stack needs two members.
    let (status, _, body) = srv
        .json(Method::POST, "/api/batch/stack", &format!(r#"{{"asset_ids": ["{}"]}}"#, srv.asset_id))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "body: {body}");

    let body = format!(r#"{{"asset_ids": ["{}", "{second}"]}}"#, srv.asset_id);
    let (status, _, resp) = srv.json(Method::POST, "/api/batch/stack", &body).await;
    assert_eq!(status, StatusCode::OK, "body: {resp}");
    let json: serde_json::Value = serde_json::from_str(&resp).unwrap();
    assert_eq!(json["member_count"], 2, "{json}");
    let stack_id = json["stack_id"].as_str().unwrap().to_string();
    assert!(uuid::Uuid::parse_str(&stack_id).is_ok());

    let (status, _, members_body) = srv.get(&format!("/api/stack/{stack_id}/members")).await;
    assert_eq!(status, StatusCode::OK, "body: {members_body}");
    let members: serde_json::Value = serde_json::from_str(&members_body).unwrap();
    let members = members.as_array().unwrap();
    assert_eq!(members.len(), 2, "{members:?}");
    assert_eq!(members[0]["asset_id"], srv.asset_id, "first selected asset leads: {members:?}");
    assert_eq!(members[0]["name"], "sunset photo");
    assert_eq!(members[0]["stack_id"], stack_id, "{members:?}");
    assert_eq!(members[0]["stack_count"], 2, "{members:?}");
    assert_eq!(members[1]["asset_id"], second, "{members:?}");

    // stacks.yaml (source of truth) and the denormalised row agree.
    let yaml = std::fs::read_to_string(srv.root.join("stacks.yaml")).unwrap();
    assert!(yaml.contains(&stack_id), "{yaml}");
    let (_, row) = srv.asset_from_both_stores();
    assert_eq!(row.stack_id.as_deref(), Some(stack_id.as_str()));

    // Stacking an already-stacked asset again is refused.
    let (status, _, resp) = srv.json(Method::POST, "/api/batch/stack", &body).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "body: {resp}");

    // Unstack both: the stack disappears.
    let (status, _, resp) = srv.json(Method::DELETE, "/api/batch/stack", &body).await;
    assert_eq!(status, StatusCode::OK, "body: {resp}");
    let (_, row) = srv.asset_from_both_stores();
    assert_eq!(row.stack_id, None);
}

#[tokio::test]
async fn html_pages_render_with_title_markers() {
    let srv = TestServer::new();
    let second = srv.seed_asset_with_hash("sha256:webtest2");
    let compare = format!("/compare?ids={},{second}", srv.asset_id);

    for (uri, marker) in [
        ("/stats", "Stats - MAKI"),
        ("/tags", "Tags — MAKI"),
        ("/collections", "Collections - MAKI"),
        ("/saved-searches", "Saved Searches - MAKI"),
        ("/volumes", "Volumes - MAKI"),
        ("/duplicates", "Duplicates - MAKI"),
        (compare.as_str(), "Compare — MAKI"),
        ("/analytics", "Analytics - MAKI"),
        ("/backup", "Backup Status - MAKI"),
    ] {
        let (status, headers, body) = srv.get(uri).await;
        assert_eq!(status, StatusCode::OK, "{uri}: {body}");
        assert!(
            headers.get("content-type").map(|v| v.to_str().unwrap().starts_with("text/html")).unwrap_or(false),
            "{uri} must be HTML"
        );
        assert!(body.contains(marker), "{uri} must carry its title marker '{marker}'");
    }

    // Page content sanity beyond the title.
    let (_, _, body) = srv.get("/tags").await;
    assert!(body.contains("landscape"), "tags page lists the seeded tag: {body}");
    let (_, _, body) = srv.get("/compare").await;
    assert!(body.contains("Select 2"), "compare without ids explains itself: {body}");
    let (status, _, _) = srv.get("/compare").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (_, _, body) = srv.get(&compare).await;
    assert!(body.contains("sunset photo") && body.contains("the original"), "{body}");
}

#[tokio::test]
async fn read_only_blocks_tag_rename_and_collection_create() {
    let srv = TestServer::new_with(true, "");

    let (status, _, _) = srv
        .json(
            Method::POST,
            "/api/tag/rename",
            r#"{"old_tag": "landscape", "new_tag": "scenery", "apply": true}"#,
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (sidecar, row) = srv.asset_from_both_stores();
    assert!(sidecar.tags.contains(&"landscape".to_string()), "sidecar untouched");
    assert!(row.tags.contains(&"landscape".to_string()), "catalog untouched");

    let (status, _, _) = srv
        .json(Method::POST, "/api/collections", r#"{"name": "nope"}"#)
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // The matching reads still work.
    let (status, _, _) = srv.get("/api/collections").await;
    assert_eq!(status, StatusCode::OK);
    let (status, _, _) = srv.get("/tags").await;
    assert_eq!(status, StatusCode::OK);
}
