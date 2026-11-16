//! Fixtures shared by the `asset_service` section test modules.
//! Test-only; pulled in with `use crate::asset_service::test_support::*;`.

#![allow(dead_code)]

pub(crate) use crate::test_support::*;
use crate::asset_service::*;

/// Default filter for import tests.
pub(crate) fn default_filter() -> FileTypeFilter {
    FileTypeFilter::default()
}

/// Set up a minimal catalog in a temp directory for import tests.
pub(crate) fn setup_catalog(dir: &std::path::Path) {
    std::fs::create_dir_all(dir.join("metadata")).unwrap();
    crate::config::CatalogConfig::default().save(dir).unwrap();
    let catalog = crate::catalog::Catalog::open(dir).unwrap();
    catalog.initialize().unwrap();
}

/// A temp catalog with one online volume (`vol`) and a quiet service on
/// it: the fixture for unit tests of service operations.
pub(crate) struct ServiceEnv {
    pub(crate) cat: TempCatalog,
    pub(crate) vol_dir: tempfile::TempDir,
    pub(crate) volume: crate::models::Volume,
    pub(crate) service: AssetService,
}

impl ServiceEnv {
    pub(crate) fn new() -> Self {
        let cat = TempCatalog::new();
        let (vol_dir, volume) = cat.add_volume("vol");
        let service = AssetService::new(
            cat.path(),
            crate::Verbosity::quiet(),
            &crate::config::PreviewConfig::default(),
        );
        Self { cat, vol_dir, volume, service }
    }

    /// Absolute path of a volume-relative file.
    pub(crate) fn abs(&self, relative: &str) -> PathBuf {
        self.vol_dir.path().join(relative)
    }

    /// Write a file on the volume (creating its directories).
    pub(crate) fn write(&self, relative: &str, content: &str) -> PathBuf {
        let path = self.abs(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, content).unwrap();
        path
    }

    /// Import volume-relative files or directories.
    pub(crate) fn import(&self, relative: &[&str]) -> ImportResult {
        let paths: Vec<PathBuf> = relative.iter().map(|r| self.abs(r)).collect();
        self.service.import(&paths, &self.volume, &FileTypeFilter::default()).unwrap()
    }

    /// Write and import one file; returns the ID of the asset it created.
    pub(crate) fn add(&self, relative: &str, content: &str) -> String {
        self.write(relative, content);
        let result = self.import(&[relative]);
        assert_eq!(result.imported, 1, "{relative} should create an asset");
        result.new_asset_ids[0].clone()
    }

    /// The asset as its sidecar has it.
    pub(crate) fn asset(&self, id: &str) -> crate::models::Asset {
        self.cat.load(id)
    }

    /// Volume-relative paths of every file location of the asset, sorted.
    pub(crate) fn location_paths(&self, id: &str) -> Vec<String> {
        let mut paths: Vec<String> = self
            .asset(id)
            .variants
            .iter()
            .flat_map(|v| v.locations.iter())
            .map(|l| crate::path_util::forward_slashes(&l.relative_path))
            .collect();
        paths.sort();
        paths
    }

    /// File-location rows the catalog holds for the asset.
    pub(crate) fn location_rows(&self, id: &str) -> i64 {
        count(
            &self.cat.catalog(),
            "SELECT COUNT(*) FROM file_locations fl JOIN variants v ON v.content_hash = fl.content_hash WHERE v.asset_id = ?1",
            id,
        )
    }
}
