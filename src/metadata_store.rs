//! YAML sidecar store — the source of truth for asset metadata.
//!
//! Each asset has a `metadata/<asset-id>.yaml` file containing tags,
//! rating, color label, description, variants, file locations, recipes.
//! The SQLite catalog is a derived cache rebuilt from these via
//! `maki rebuild-catalog`. All write paths must update both stores.

use std::path::Path;

use anyhow::Result;
use uuid::Uuid;

use crate::models::Asset;

/// Result of syncing sidecar files to the catalog.
pub struct SyncResult {
    pub synced: u64,
    pub errors: u64,
}

/// Summary of an asset for listing purposes.
pub struct AssetSummary {
    pub id: Uuid,
    pub name: Option<String>,
    pub asset_type: crate::models::AssetType,
    pub variant_count: usize,
}

/// Persists and retrieves all asset metadata as YAML sidecar files.
pub struct MetadataStore {
    metadata_dir: std::path::PathBuf,
}

impl MetadataStore {
    pub fn new(catalog_root: &Path) -> Self {
        Self {
            metadata_dir: catalog_root.join("metadata"),
        }
    }

    /// Shard directory: first 2 chars of UUID hex.
    fn shard_dir(&self, asset_id: Uuid) -> std::path::PathBuf {
        let hex = asset_id.to_string();
        let prefix = &hex[..2];
        self.metadata_dir.join(prefix)
    }

    fn sidecar_path(&self, asset_id: Uuid) -> std::path::PathBuf {
        self.shard_dir(asset_id).join(format!("{}.yaml", asset_id))
    }

    /// Write/update sidecar YAML for an asset.
    ///
    /// Writes are atomic (temp file + rename) and serialized across
    /// processes via the sidecar write lock — see
    /// [`Self::acquire_write_lock`].
    pub fn save(&self, asset: &Asset) -> Result<()> {
        let dir = self.shard_dir(asset.id);
        std::fs::create_dir_all(&dir)?;
        // Self-heal tag provenance: a mutation site that bypassed
        // `Asset::remove_tags` must never persist source entries for
        // tags that are gone (invariant: map keys ⊆ tags). Cheap in the
        // common case — clone only when a stale key actually exists.
        let yaml = if asset.has_stale_tag_sources() {
            let mut pruned = asset.clone();
            pruned.prune_tag_sources();
            serde_yaml::to_string(&pruned)?
        } else {
            serde_yaml::to_string(asset)?
        };

        let _lock = self.acquire_write_lock()?;
        // Atomic write: temp file + rename, so readers (and crashes mid-
        // write) never see a truncated sidecar. The temp name is fixed
        // per asset — safe because the write lock is held.
        let tmp = dir.join(format!(".{}.tmp", asset.id));
        std::fs::write(&tmp, yaml)?;
        std::fs::rename(&tmp, self.sidecar_path(asset.id))?;
        Ok(())
    }

    /// Acquire the cross-process sidecar write lock
    /// (`metadata/.write.lock`, advisory flock/LockFileEx via `fs2`).
    /// Released when the returned handle drops.
    ///
    /// This serializes sidecar WRITES between concurrent maki processes
    /// (`maki serve` + a CLI command is the common case), so two writers
    /// can't interleave or torn-write files. It deliberately does NOT
    /// serialize whole load-modify-save sequences — two processes
    /// editing the same asset simultaneously still resolve
    /// last-writer-wins (see roadmap-v4.6-horizons.md). Held
    /// per-operation, not per-process, so a long-running import doesn't
    /// freeze the web UI's metadata edits.
    fn acquire_write_lock(&self) -> Result<std::fs::File> {
        use fs2::FileExt;
        const TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

        std::fs::create_dir_all(&self.metadata_dir)?;
        let lock_path = self.metadata_dir.join(".write.lock");
        let file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&lock_path)?;
        let start = std::time::Instant::now();
        loop {
            match file.try_lock_exclusive() {
                Ok(()) => return Ok(file),
                Err(_) if start.elapsed() < TIMEOUT => {
                    std::thread::sleep(std::time::Duration::from_millis(25));
                }
                Err(e) => {
                    anyhow::bail!(
                        "could not acquire the sidecar write lock within {}s \
                         (another maki process seems stuck holding {}): {e}",
                        TIMEOUT.as_secs(),
                        lock_path.display()
                    );
                }
            }
        }
    }

    /// Read sidecar YAML and return the asset.
    pub fn load(&self, asset_id: Uuid) -> Result<Asset> {
        let path = self.sidecar_path(asset_id);
        let contents = std::fs::read_to_string(&path)?;
        let mut asset: Asset = serde_yaml::from_str(&contents)?;
        // Normalize MicrosoftPhoto:Rating percentage values (>5) to 1-5 scale
        if let Some(r) = asset.rating {
            if r > 5 {
                asset.rating = Some(crate::asset_service::normalize_rating(r));
            }
        }
        Ok(asset)
    }

    /// Read sidecar YAML without any normalization (for migration checks).
    pub fn load_raw(&self, asset_id: Uuid) -> Result<Asset> {
        let path = self.sidecar_path(asset_id);
        let contents = std::fs::read_to_string(&path)?;
        let asset: Asset = serde_yaml::from_str(&contents)?;
        Ok(asset)
    }

    /// Delete the sidecar YAML file for an asset. Serialized via the
    /// same write lock as [`Self::save`].
    pub fn delete(&self, asset_id: Uuid) -> Result<()> {
        let path = self.sidecar_path(asset_id);
        let _lock = self.acquire_write_lock()?;
        if path.exists() {
            std::fs::remove_file(&path)?;
        }
        Ok(())
    }

    /// Enumerate all known assets by walking sidecar YAML files.
    pub fn list(&self) -> Result<Vec<AssetSummary>> {
        let mut summaries = Vec::new();

        if !self.metadata_dir.exists() {
            return Ok(summaries);
        }

        for shard_entry in std::fs::read_dir(&self.metadata_dir)? {
            let shard_entry = shard_entry?;
            if !shard_entry.file_type()?.is_dir() {
                continue;
            }
            for file_entry in std::fs::read_dir(shard_entry.path())? {
                let file_entry = file_entry?;
                let path = file_entry.path();
                let ext = path.extension().and_then(|e| e.to_str());
                if ext != Some("yaml") {
                    continue;
                }
                let stem = match path.file_stem().and_then(|s| s.to_str()) {
                    Some(s) => s,
                    None => continue,
                };
                let id = match uuid::Uuid::parse_str(stem) {
                    Ok(id) => id,
                    Err(_) => continue,
                };
                match self.load(id) {
                    Ok(asset) => {
                        summaries.push(AssetSummary {
                            id: asset.id,
                            name: asset.name.clone(),
                            asset_type: asset.asset_type.clone(),
                            variant_count: asset.variants.len(),
                        });
                    }
                    Err(e) => {
                        eprintln!("Warning: failed to load sidecar {}: {e}", path.display());
                    }
                }
            }
        }

        Ok(summaries)
    }

    /// Rebuild SQLite catalog from sidecar files.
    pub fn sync_to_catalog(&self, catalog: &crate::catalog::Catalog) -> Result<SyncResult> {
        let summaries = self.list()?;
        let mut synced = 0u64;
        let mut errors = 0u64;

        for summary in &summaries {
            match self.load(summary.id) {
                Ok(asset) => {
                    if let Err(e) = catalog.insert_asset(&asset) {
                        eprintln!("Error inserting asset {}: {e}", summary.id);
                        errors += 1;
                        continue;
                    }
                    for variant in &asset.variants {
                        if let Err(e) = catalog.insert_variant(variant) {
                            eprintln!(
                                "Error inserting variant {} for asset {}: {e}",
                                variant.content_hash, summary.id
                            );
                            errors += 1;
                            continue;
                        }
                        for loc in &variant.locations {
                            if let Err(e) = catalog.insert_file_location(&variant.content_hash, loc)
                            {
                                eprintln!(
                                    "Error inserting location for variant {}: {e}",
                                    variant.content_hash
                                );
                                errors += 1;
                            }
                        }
                    }
                    for recipe in &asset.recipes {
                        if let Err(e) = catalog.insert_recipe(recipe) {
                            eprintln!(
                                "Error inserting recipe {} for asset {}: {e}",
                                recipe.id, summary.id
                            );
                            errors += 1;
                        }
                    }
                    synced += 1;
                }
                Err(e) => {
                    eprintln!("Error loading asset {}: {e}", summary.id);
                    errors += 1;
                }
            }
        }

        Ok(SyncResult { synced, errors })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::AssetType;

    #[test]
    fn save_and_load_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let store = MetadataStore::new(dir.path());

        let asset = Asset::new(AssetType::Image, "sha256:meta_test1");
        let id = asset.id;

        store.save(&asset).unwrap();
        let loaded = store.load(id).unwrap();

        assert_eq!(loaded.id, id);
        assert_eq!(loaded.asset_type, AssetType::Image);
    }

    #[test]
    fn list_returns_saved_assets() {
        let dir = tempfile::tempdir().unwrap();
        let store = MetadataStore::new(dir.path());

        let mut a1 = Asset::new(AssetType::Image, "sha256:list1");
        a1.name = Some("First".to_string());
        let mut a2 = Asset::new(AssetType::Video, "sha256:list2");
        a2.name = Some("Second".to_string());

        store.save(&a1).unwrap();
        store.save(&a2).unwrap();

        let summaries = store.list().unwrap();
        assert_eq!(summaries.len(), 2);

        let mut ids: Vec<_> = summaries.iter().map(|s| s.id).collect();
        ids.sort();
        let mut expected = vec![a1.id, a2.id];
        expected.sort();
        assert_eq!(ids, expected);
    }

    #[test]
    fn list_empty_returns_empty() {
        let dir = tempfile::tempdir().unwrap();
        let store = MetadataStore::new(dir.path());
        let summaries = store.list().unwrap();
        assert!(summaries.is_empty());
    }

    #[test]
    fn sync_to_catalog_inserts_assets_and_variants() {
        use crate::catalog::Catalog;
        use crate::models::{FileLocation, Variant, VariantRole, Volume, VolumeType};

        let dir = tempfile::tempdir().unwrap();
        let store = MetadataStore::new(dir.path());
        let catalog = Catalog::open_in_memory().unwrap();
        catalog.initialize().unwrap();

        // Create a volume so FK references work
        let volume = Volume::new(
            "test-vol".to_string(),
            std::path::PathBuf::from("/mnt/test"),
            VolumeType::Local,
        );
        catalog.ensure_volume(&volume).unwrap();

        // Create an asset with a variant and location
        let mut asset = Asset::new(AssetType::Image, "sha256:sync1");
        asset.name = Some("synced".to_string());
        let variant = Variant {
            content_hash: "sha256:sync1".to_string(),
            asset_id: asset.id,
            role: VariantRole::Original,
            format: "jpg".to_string(),
            file_size: 1024,
            original_filename: "photo.jpg".to_string(),
            source_metadata: Default::default(),
            locations: vec![FileLocation {
                volume_id: volume.id,
                relative_path: std::path::PathBuf::from("photos/photo.jpg"),
                verified_at: None,
            }],
        };
        asset.variants.push(variant);
        store.save(&asset).unwrap();

        let result = store.sync_to_catalog(&catalog).unwrap();
        assert_eq!(result.synced, 1);
        assert_eq!(result.errors, 0);

        // Verify asset is in the catalog
        let details = catalog.load_asset_details(&asset.id.to_string()).unwrap().unwrap();
        assert_eq!(details.name.as_deref(), Some("synced"));
        assert_eq!(details.variants.len(), 1);
        assert_eq!(details.variants[0].content_hash, "sha256:sync1");
        assert_eq!(details.variants[0].locations.len(), 1);
        assert_eq!(details.variants[0].locations[0].relative_path, "photos/photo.jpg");
    }

    #[test]
    fn concurrent_saves_produce_intact_sidecars() {
        // Hammer the write lock: 8 threads saving interleaved assets
        // (including repeated saves of the SAME asset) through separate
        // MetadataStore instances. Every resulting sidecar must parse.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();

        let shared = Asset::new(crate::models::AssetType::Image, "sha256:lock-shared");
        let shared_id = shared.id;

        let mut handles = Vec::new();
        for t in 0..8 {
            let root = root.clone();
            let shared = shared.clone();
            handles.push(std::thread::spawn(move || {
                let store = MetadataStore::new(&root);
                for i in 0..25 {
                    // Distinct asset per (thread, iteration)…
                    let mut a = Asset::new(
                        crate::models::AssetType::Image,
                        &format!("sha256:lock-{t}-{i}"),
                    );
                    a.name = Some(format!("asset {t}-{i}"));
                    a.tags = vec![format!("tag{t}"); 50];
                    store.save(&a).unwrap();
                    // …plus contention on one shared asset.
                    let mut s = shared.clone();
                    s.rating = Some(((t + i) % 5 + 1) as u8);
                    s.tags = vec![format!("thread{t}-iter{i}"); 80];
                    store.save(&s).unwrap();
                }
            }));
        }
        for h in handles {
            h.join().unwrap();
        }

        let store = MetadataStore::new(&root);
        let summaries = store.list().unwrap();
        assert_eq!(summaries.len(), 8 * 25 + 1, "all sidecars present and parseable");
        // The shared asset is intact (some thread's version, not a torn mix).
        let s = store.load(shared_id).unwrap();
        assert!(s.rating.is_some());
        assert_eq!(s.tags.len(), 80);
        let first = s.tags[0].clone();
        assert!(s.tags.iter().all(|t| *t == first), "tags from a single writer");
    }

    #[test]
    fn tag_sources_round_trip_through_sidecar() {
        use crate::models::TagSource;
        let dir = tempfile::tempdir().unwrap();
        let store = MetadataStore::new(dir.path());

        let mut asset = Asset::new(AssetType::Image, "sha256:provenance1");
        asset.add_tags_with_source(&["manual".to_string()], TagSource::User);
        asset.add_tags_with_source(&["from-xmp".to_string()], TagSource::XmpImport);
        asset.add_tags_with_source(&["from-ai".to_string()], TagSource::AutoTag);
        asset.add_tags_with_source(&["from-vlm".to_string()], TagSource::Vlm);
        store.save(&asset).unwrap();

        let loaded = store.load(asset.id).unwrap();
        assert_eq!(loaded.tags, asset.tags);
        assert_eq!(loaded.tag_sources, asset.tag_sources);
        assert_eq!(loaded.tag_source("manual"), TagSource::User);
        assert_eq!(loaded.tag_source("from-xmp"), TagSource::XmpImport);
        assert_eq!(loaded.tag_source("from-ai"), TagSource::AutoTag);
        assert_eq!(loaded.tag_source("from-vlm"), TagSource::Vlm);
        // User tags stay out of the map.
        assert!(!loaded.tag_sources.contains_key("manual"));
    }

    #[test]
    fn old_sidecar_without_tag_sources_loads_and_stays_clean() {
        let dir = tempfile::tempdir().unwrap();
        let store = MetadataStore::new(dir.path());

        // Pre-provenance sidecar: has tags, no tag_sources key.
        let mut asset = Asset::new(AssetType::Image, "sha256:legacy1");
        asset.tags = vec!["old-tag".to_string()];
        store.save(&asset).unwrap();
        let path = dir
            .path()
            .join("metadata")
            .join(&asset.id.to_string()[..2])
            .join(format!("{}.yaml", asset.id));
        let raw = std::fs::read_to_string(&path).unwrap();
        assert!(!raw.contains("tag_sources"), "{raw}");

        // Loads with an empty map; every tag defaults to user.
        let loaded = store.load(asset.id).unwrap();
        assert!(loaded.tag_sources.is_empty());
        assert_eq!(loaded.tag_source("old-tag"), crate::models::TagSource::User);

        // A rewrite without tag changes stays free of the key
        // (skip_serializing_if) — byte-identical sidecar.
        store.save(&loaded).unwrap();
        let rewritten = std::fs::read_to_string(&path).unwrap();
        assert_eq!(raw, rewritten);
    }

    #[test]
    fn save_self_heals_stale_tag_sources() {
        use crate::models::TagSource;
        let dir = tempfile::tempdir().unwrap();
        let store = MetadataStore::new(dir.path());

        let mut asset = Asset::new(AssetType::Image, "sha256:stale1");
        asset.add_tags_with_source(
            &["keep".to_string(), "gone".to_string()],
            TagSource::AutoTag,
        );
        // Simulate a mutation site that bypassed remove_tags.
        asset.tags.retain(|t| t != "gone");
        store.save(&asset).unwrap();

        let loaded = store.load(asset.id).unwrap();
        assert_eq!(loaded.tags, vec!["keep".to_string()]);
        assert!(loaded.tag_sources.contains_key("keep"));
        assert!(
            !loaded.tag_sources.contains_key("gone"),
            "save must never persist provenance for removed tags"
        );
    }

    #[test]
    fn save_leaves_no_temp_files() {
        let dir = tempfile::tempdir().unwrap();
        let store = MetadataStore::new(dir.path());
        let a = Asset::new(crate::models::AssetType::Image, "sha256:tmpcheck");
        store.save(&a).unwrap();
        store.save(&a).unwrap();
        let shard = dir.path().join("metadata").join(&a.id.to_string()[..2]);
        let leftovers: Vec<_> = std::fs::read_dir(&shard)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().and_then(|x| x.to_str()) == Some("tmp"))
            .collect();
        assert!(leftovers.is_empty(), "{leftovers:?}");
    }
}
