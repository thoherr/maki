//! Shared fixtures for the `query` section test modules (temp catalogs,
//! seeded assets, row counters). Test-only; each `query/<section>.rs`
//! test module pulls these in with `use crate::query::test_support::*;`.

#![allow(dead_code)]

use crate::catalog::Catalog;
use crate::metadata_store::MetadataStore;
use crate::models::volume::{Volume, VolumeType};
use crate::models::{Asset, AssetType, Variant, VariantRole};

/// Set up a temp catalog with one asset and its sidecar, returning (dir, asset_id).
pub(crate) fn setup_tag_env() -> (tempfile::TempDir, String) {
    let dir = tempfile::tempdir().unwrap();
    let catalog_root = dir.path();

    // Init catalog
    let catalog = Catalog::open(catalog_root).unwrap();
    catalog.initialize().unwrap();

    // Create and persist an asset
    let mut asset = Asset::new(AssetType::Image, "sha256:tag_env");
    asset.tags = vec!["existing".to_string()];
    catalog.insert_asset(&asset).unwrap();

    let store = MetadataStore::new(catalog_root);
    store.save(&asset).unwrap();

    (dir, asset.id.to_string())
}

/// Set up a temp catalog with two assets, each with one variant, for group tests.
/// Returns (dir, hash1, hash2, asset_id1, asset_id2).
pub(crate) fn setup_group_env() -> (tempfile::TempDir, String, String, String, String) {
    let dir = tempfile::tempdir().unwrap();
    let catalog_root = dir.path();

    let catalog = Catalog::open(catalog_root).unwrap();
    catalog.initialize().unwrap();
    let store = MetadataStore::new(catalog_root);

    // Create first asset (older)
    let mut asset1 = Asset::new(AssetType::Image, "sha256:hash1");
    asset1.created_at = chrono::Utc::now() - chrono::Duration::hours(2);
    asset1.tags = vec!["landscape".to_string()];
    let variant1 = Variant {
        content_hash: "sha256:hash1".to_string(),
        asset_id: asset1.id,
        role: VariantRole::Original,
        format: "arw".to_string(),
        file_size: 25_000_000,
        original_filename: "DSC_001.ARW".to_string(),
        source_metadata: Default::default(),
        locations: vec![],
    };
    asset1.variants.push(variant1.clone());
    catalog.insert_asset(&asset1).unwrap();
    catalog.insert_variant(&variant1).unwrap();
    store.save(&asset1).unwrap();

    // Create second asset (newer)
    let mut asset2 = Asset::new(AssetType::Image, "sha256:hash2");
    asset2.tags = vec!["nature".to_string()];
    let variant2 = Variant {
        content_hash: "sha256:hash2".to_string(),
        asset_id: asset2.id,
        role: VariantRole::Original,
        format: "jpg".to_string(),
        file_size: 5_000_000,
        original_filename: "DSC_001.JPG".to_string(),
        source_metadata: Default::default(),
        locations: vec![],
    };
    asset2.variants.push(variant2.clone());
    catalog.insert_asset(&asset2).unwrap();
    catalog.insert_variant(&variant2).unwrap();
    store.save(&asset2).unwrap();

    let id1 = asset1.id.to_string();
    let id2 = asset2.id.to_string();
    (dir, "sha256:hash1".to_string(), "sha256:hash2".to_string(), id1, id2)
}


/// Count rows of `sql` (one `?1` = asset id).
pub(crate) fn count(catalog: &Catalog, sql: &str, id: &str) -> i64 {
    catalog
        .conn()
        .query_row(sql, rusqlite::params![id], |r| r.get(0))
        .unwrap()
}


pub(crate) fn make_volume(label: &str, mount: &str) -> Volume {
    Volume {
        id: uuid::Uuid::new_v4(),
        label: label.to_string(),
        mount_point: std::path::PathBuf::from(mount),
        volume_type: VolumeType::External,
        purpose: None,
        is_online: true,
    }
}

pub(crate) fn load_asset(dir: &std::path::Path, id: &str) -> Asset {
    let store = MetadataStore::new(dir);
    store.load(id.parse().unwrap()).unwrap()
}
