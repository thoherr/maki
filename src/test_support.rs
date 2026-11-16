//! Crate-wide test fixtures: a temp catalog on disk, an in-memory
//! catalog, and an asset builder. Test-only; pull them in with
//! `use crate::test_support::*;`.
//!
//! ```ignore
//! let cat = TempCatalog::new();
//! let asset = cat.seed(test_asset("sha256:a").file("DSC_001.ARW").tags(&["landscape"]));
//! let engine = QueryEngine::new(cat.path());
//! ```
//!
//! Section-specific fixtures build on these and stay next to their tests
//! (`query/test_support.rs`, the `setup_*` catalogs in `catalog.rs`).

#![allow(dead_code)]

use std::path::{Path, PathBuf};

use crate::catalog::Catalog;
use crate::device_registry::DeviceRegistry;
use crate::metadata_store::MetadataStore;
use crate::models::volume::{Volume, VolumeType};
use crate::models::{Asset, AssetType, FileLocation, Variant, VariantRole};

/// An initialised catalog in a temp directory (schema, default
/// `maki.toml`, empty volume registry). Removed on drop.
pub(crate) struct TempCatalog {
    dir: tempfile::TempDir,
}

impl TempCatalog {
    pub(crate) fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("metadata")).unwrap();
        crate::config::CatalogConfig::default().save(root).unwrap();
        DeviceRegistry::init(root).unwrap();
        Catalog::open(root).unwrap().initialize().unwrap();
        Self { dir }
    }

    /// The catalog root (named like `TempDir::path`).
    pub(crate) fn path(&self) -> &Path {
        self.dir.path()
    }

    /// A fresh connection to the catalog database.
    pub(crate) fn catalog(&self) -> Catalog {
        Catalog::open_fast(self.path()).unwrap()
    }

    pub(crate) fn store(&self) -> MetadataStore {
        MetadataStore::new(self.path())
    }

    /// Write `asset` to both stores: sidecar first, then its SQLite rows.
    /// Volumes of its file locations must be registered already.
    pub(crate) fn seed(&self, asset: impl Into<Asset>) -> Asset {
        let asset = asset.into();
        self.store().save(&asset).unwrap();
        insert_rows(&self.catalog(), &asset);
        asset
    }

    /// The asset as its sidecar has it.
    pub(crate) fn load(&self, id: &str) -> Asset {
        load_asset(self.path(), id)
    }

    /// Register `mount` as an online volume in the registry and the catalog.
    pub(crate) fn register_volume(&self, label: &str, mount: &Path) -> Volume {
        let volume = DeviceRegistry::new(self.path())
            .register(label, mount, VolumeType::Local, None)
            .unwrap();
        self.catalog().ensure_volume(&volume).unwrap();
        volume
    }

    /// A new temp directory registered as an online volume.
    pub(crate) fn add_volume(&self, label: &str) -> (tempfile::TempDir, Volume) {
        let dir = tempfile::tempdir().unwrap();
        let volume = self.register_volume(label, dir.path());
        (dir, volume)
    }
}

/// An initialised in-memory catalog (SQLite rows only, no sidecars).
pub(crate) fn memory_catalog() -> Catalog {
    let catalog = Catalog::open_in_memory().unwrap();
    catalog.initialize().unwrap();
    catalog
}

/// Insert the SQLite rows of `asset`: the asset, its variants, their file
/// locations and its recipes. No sidecar; see [`TempCatalog::seed`].
pub(crate) fn insert_rows(catalog: &Catalog, asset: &Asset) {
    catalog.insert_asset(asset).unwrap();
    for variant in &asset.variants {
        catalog.insert_variant(variant).unwrap();
        for location in &variant.locations {
            catalog.insert_file_location(&variant.content_hash, location).unwrap();
        }
    }
    for recipe in &asset.recipes {
        catalog.insert_recipe(recipe).unwrap();
    }
}

/// Start an image asset whose identity and first (original) variant are
/// `hash`. The variant is a 1000-byte `<hash>.jpg` until [`AssetBuilder::file`]
/// says otherwise.
pub(crate) fn test_asset(hash: &str) -> AssetBuilder {
    let mut asset = Asset::new(AssetType::Image, hash);
    let stem = hash.rsplit(':').next().unwrap_or(hash);
    asset.variants.push(test_variant(asset.id, hash, &format!("{stem}.jpg")));
    AssetBuilder { asset }
}

/// An original variant of `asset_id`; the format is the lower-cased
/// extension of `filename`.
pub(crate) fn test_variant(asset_id: uuid::Uuid, hash: &str, filename: &str) -> Variant {
    let format = Path::new(filename)
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    Variant {
        content_hash: hash.to_string(),
        asset_id,
        role: VariantRole::Original,
        format,
        file_size: 1000,
        original_filename: filename.to_string(),
        source_metadata: Default::default(),
        locations: vec![],
    }
}

/// Builds an [`Asset`] in memory; `file`, `size`, `role` and `at` apply to
/// the variant added last. Hand it to [`TempCatalog::seed`] or call
/// [`AssetBuilder::build`].
pub(crate) struct AssetBuilder {
    asset: Asset,
}

impl AssetBuilder {
    pub(crate) fn name(mut self, name: &str) -> Self {
        self.asset.name = Some(name.to_string());
        self
    }

    pub(crate) fn tags(mut self, tags: &[&str]) -> Self {
        self.asset.tags = tags.iter().map(|t| t.to_string()).collect();
        self
    }

    pub(crate) fn rating(mut self, rating: u8) -> Self {
        self.asset.rating = Some(rating);
        self
    }

    pub(crate) fn created_at(mut self, created_at: chrono::DateTime<chrono::Utc>) -> Self {
        self.asset.created_at = created_at;
        self
    }

    pub(crate) fn asset_type(mut self, asset_type: AssetType) -> Self {
        self.asset.asset_type = asset_type;
        self
    }

    /// File name (and, from its extension, format) of the current variant.
    pub(crate) fn file(mut self, filename: &str) -> Self {
        let current = self.current();
        let renamed = test_variant(current.asset_id, &current.content_hash, filename);
        current.original_filename = renamed.original_filename;
        current.format = renamed.format;
        self
    }

    pub(crate) fn size(mut self, bytes: u64) -> Self {
        self.current().file_size = bytes;
        self
    }

    pub(crate) fn role(mut self, role: VariantRole) -> Self {
        self.current().role = role;
        self
    }

    /// A file location of the current variant.
    pub(crate) fn at(mut self, volume: &Volume, relative_path: &str) -> Self {
        self.current().locations.push(FileLocation {
            volume_id: volume.id,
            relative_path: PathBuf::from(relative_path),
            verified_at: None,
        });
        self
    }

    /// Add another original variant, which becomes the current one.
    pub(crate) fn variant(mut self, hash: &str, filename: &str) -> Self {
        self.asset.variants.push(test_variant(self.asset.id, hash, filename));
        self
    }

    pub(crate) fn build(self) -> Asset {
        self.asset
    }

    fn current(&mut self) -> &mut Variant {
        self.asset.variants.last_mut().expect("the builder starts with one variant")
    }
}

impl From<AssetBuilder> for Asset {
    fn from(builder: AssetBuilder) -> Self {
        builder.build()
    }
}

/// The asset `id` as the sidecar under `root` has it.
pub(crate) fn load_asset(root: &Path, id: &str) -> Asset {
    MetadataStore::new(root).load(id.parse().unwrap()).unwrap()
}

/// Count rows of `sql` (one `?1` = asset id).
pub(crate) fn count(catalog: &Catalog, sql: &str, id: &str) -> i64 {
    catalog
        .conn()
        .query_row(sql, rusqlite::params![id], |r| r.get(0))
        .unwrap()
}

/// An online external volume that exists only in memory.
pub(crate) fn make_volume(label: &str, mount: &str) -> Volume {
    Volume {
        id: uuid::Uuid::new_v4(),
        label: label.to_string(),
        mount_point: PathBuf::from(mount),
        volume_type: VolumeType::External,
        purpose: None,
        is_online: true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seed_writes_sidecar_and_rows_with_locations() {
        let cat = TempCatalog::new();
        let (_vol_dir, volume) = cat.add_volume("vol");
        let seeded = cat.seed(
            test_asset("sha256:a")
                .file("DSC_001.ARW")
                .at(&volume, "2026/DSC_001.ARW")
                .variant("sha256:b", "DSC_001.jpg")
                .tags(&["landscape"])
                .rating(4),
        );
        let id = seeded.id.to_string();

        let sidecar = cat.load(&id);
        assert_eq!(sidecar.tags, ["landscape"]);
        assert_eq!(sidecar.variants[0].format, "arw");
        assert_eq!(sidecar.variants.len(), 2);

        let catalog = cat.catalog();
        assert_eq!(count(&catalog, "SELECT COUNT(*) FROM variants WHERE asset_id = ?1", &id), 2);
        assert_eq!(count(&catalog, "SELECT variant_count FROM assets WHERE id = ?1", &id), 2);
        assert_eq!(
            count(
                &catalog,
                "SELECT COUNT(*) FROM file_locations fl JOIN variants v ON v.content_hash = fl.content_hash WHERE v.asset_id = ?1",
                &id,
            ),
            1
        );
    }
}
