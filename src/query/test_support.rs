//! Fixtures for the `query` section test modules, built on the crate-wide
//! [`crate::test_support`] (re-exported here). Test-only; each
//! `query/<section>.rs` test module pulls these in with
//! `use crate::query::test_support::*;`.

#![allow(dead_code)]

pub(crate) use crate::test_support::*;

/// A temp catalog with one asset tagged `existing`, returning (catalog, asset_id).
pub(crate) fn setup_tag_env() -> (TempCatalog, String) {
    let cat = TempCatalog::new();
    let seeded = cat.seed(test_asset("sha256:tag_env").tags(&["existing"]));
    (cat, seeded.id.to_string())
}

/// A temp catalog with two single-variant assets for group tests, the
/// first two hours older. Returns (catalog, hash1, hash2, asset_id1, asset_id2).
pub(crate) fn setup_group_env() -> (TempCatalog, String, String, String, String) {
    let cat = TempCatalog::new();
    let older = cat.seed(
        test_asset("sha256:hash1")
            .file("DSC_001.ARW")
            .size(25_000_000)
            .tags(&["landscape"])
            .created_at(chrono::Utc::now() - chrono::Duration::hours(2)),
    );
    let newer = cat.seed(test_asset("sha256:hash2").file("DSC_001.JPG").size(5_000_000).tags(&["nature"]));
    (
        cat,
        "sha256:hash1".to_string(),
        "sha256:hash2".to_string(),
        older.id.to_string(),
        newer.id.to_string(),
    )
}
