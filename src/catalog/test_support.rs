//! Fixture catalogs shared by the `catalog` section test modules.
//! Test-only; pulled in with `use crate::catalog::test_support::*;`.

#![allow(dead_code)]

pub(crate) use crate::test_support::*;
use crate::catalog::*;

/// Helper to set up a catalog with one asset and variant for search tests.
pub(crate) fn setup_search_catalog() -> Catalog {
    let catalog = Catalog::open_in_memory().unwrap();
    catalog.initialize().unwrap();

    let mut asset = crate::models::Asset::new(crate::models::AssetType::Image, "sha256:search1");
    asset.name = Some("sunset photo".to_string());
    asset.description = Some("A beautiful sunset over the ocean".to_string());
    asset.tags = vec!["landscape".to_string(), "nature".to_string()];

    let variant = crate::models::Variant { file_size: 5000, ..test_variant(asset.id, "sha256:search1", "sunset_beach.jpg") };
    asset.variants.push(variant.clone());
    catalog.insert_asset(&asset).unwrap();
    catalog.insert_variant(&variant).unwrap();

    // Add a second asset of different type
    let mut asset2 = crate::models::Asset::new(crate::models::AssetType::Video, "sha256:search2");
    asset2.name = Some("holiday clip".to_string());

    let variant2 = crate::models::Variant { file_size: 100_000, ..test_variant(asset2.id, "sha256:search2", "holiday.mp4") };
    asset2.variants.push(variant2.clone());
    catalog.insert_asset(&asset2).unwrap();
    catalog.insert_variant(&variant2).unwrap();

    catalog
}

/// Helper: create a catalog with a single asset that has the given tags.
pub(crate) fn catalog_with_tags(tags: Vec<String>, hash_suffix: &str) -> Catalog {
    let catalog = Catalog::open_in_memory().unwrap();
    catalog.initialize().unwrap();
    let mut asset = crate::models::Asset::new(crate::models::AssetType::Image, &format!("sha256:{hash_suffix}"));
    asset.tags = tags;
    let variant = crate::models::Variant {
        content_hash: format!("sha256:{hash_suffix}"),
        asset_id: asset.id,
        role: crate::models::VariantRole::Original,
        format: "jpg".to_string(),
        file_size: 1000,
        original_filename: format!("{hash_suffix}.jpg"),
        source_metadata: Default::default(),
        locations: vec![],
    };
    asset.variants.push(variant.clone());
    catalog.insert_asset(&asset).unwrap();
    catalog.insert_variant(&variant).unwrap();
    catalog
}

/// Helper to create a catalog with metadata-rich variants for metadata search tests.
pub(crate) fn setup_metadata_catalog() -> Catalog {
    let catalog = Catalog::open_in_memory().unwrap();
    catalog.initialize().unwrap();

    // Asset 1: Fuji camera
    let mut asset1 = crate::models::Asset::new(crate::models::AssetType::Image, "sha256:meta1");

    let mut meta1 = std::collections::BTreeMap::new();
    meta1.insert("camera_model".to_string(), "X-T5".to_string());
    meta1.insert("lens_model".to_string(), "XF56mmF1.2 R".to_string());
    meta1.insert("focal_length".to_string(), "56 mm".to_string());
    meta1.insert("f_number".to_string(), "1.2".to_string());
    meta1.insert("iso".to_string(), "400".to_string());
    meta1.insert("image_width".to_string(), "6240".to_string());
    meta1.insert("image_height".to_string(), "4160".to_string());
    meta1.insert("camera_make".to_string(), "FUJIFILM".to_string());

    let variant1 = crate::models::Variant {
        content_hash: "sha256:meta1".to_string(),
        asset_id: asset1.id,
        role: crate::models::VariantRole::Original,
        format: "raf".to_string(),
        file_size: 50_000_000,
        original_filename: "DSCF0001.RAF".to_string(),
        source_metadata: meta1,
        locations: vec![],
    };
    asset1.variants.push(variant1.clone());
    catalog.insert_asset(&asset1).unwrap();
    catalog.insert_variant(&variant1).unwrap();

    // Asset 2: Nikon camera
    let mut asset2 = crate::models::Asset::new(crate::models::AssetType::Image, "sha256:meta2");

    let mut meta2 = std::collections::BTreeMap::new();
    meta2.insert("camera_model".to_string(), "Z 6II".to_string());
    meta2.insert("lens_model".to_string(), "NIKKOR Z 24-70mm f/4 S".to_string());
    meta2.insert("focal_length".to_string(), "50 mm".to_string());
    meta2.insert("f_number".to_string(), "4.0".to_string());
    meta2.insert("iso".to_string(), "3200".to_string());
    meta2.insert("image_width".to_string(), "6048".to_string());
    meta2.insert("image_height".to_string(), "4024".to_string());
    meta2.insert("camera_make".to_string(), "NIKON CORPORATION".to_string());
    meta2.insert("label".to_string(), "Red".to_string());

    let variant2 = crate::models::Variant {
        content_hash: "sha256:meta2".to_string(),
        asset_id: asset2.id,
        role: crate::models::VariantRole::Original,
        format: "nef".to_string(),
        file_size: 40_000_000,
        original_filename: "DSC_0001.NEF".to_string(),
        source_metadata: meta2,
        locations: vec![],
    };
    asset2.variants.push(variant2.clone());
    catalog.insert_asset(&asset2).unwrap();
    catalog.insert_variant(&variant2).unwrap();

    catalog
}
