//! Volume registry — tracks the storage volumes the catalog knows about
//! and resolves their current online/offline status by probing mount points.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::Result;
use uuid::Uuid;

use crate::models::{Volume, VolumePurpose, VolumeType};

/// Manages volume registration and online/offline detection.
pub struct DeviceRegistry {
    catalog_root: PathBuf,
}

impl DeviceRegistry {
    pub fn new(catalog_root: &Path) -> Self {
        Self {
            catalog_root: catalog_root.to_path_buf(),
        }
    }

    /// Create an empty volumes.yaml file.
    pub fn init(catalog_root: &Path) -> Result<()> {
        let path = catalog_root.join("volumes.yaml");
        std::fs::write(path, "[]\n")?;
        Ok(())
    }

    fn volumes_path(&self) -> PathBuf {
        self.catalog_root.join("volumes.yaml")
    }

    fn load(&self) -> Result<Vec<Volume>> {
        let contents = std::fs::read_to_string(self.volumes_path())?;
        let volumes: Vec<Volume> = serde_norway::from_str(&contents)?;
        Ok(volumes)
    }

    fn save(&self, volumes: &[Volume]) -> Result<()> {
        let yaml = serde_norway::to_string(volumes)?;
        std::fs::write(self.volumes_path(), yaml)?;
        Ok(())
    }

    /// Register a new volume.
    pub fn register(
        &self,
        label: &str,
        mount_point: &Path,
        volume_type: VolumeType,
        purpose: Option<VolumePurpose>,
    ) -> Result<Volume> {
        let mut volumes = self.load()?;

        if volumes.iter().any(|v| v.label == label) {
            anyhow::bail!("a volume with label '{}' already exists", label);
        }

        let mut volume = Volume::new(label.to_string(), mount_point.to_path_buf(), volume_type);
        volume.purpose = purpose;
        volumes.push(volume.clone());
        self.save(&volumes)?;

        Ok(volume)
    }

    /// Set or clear the purpose of an existing volume.
    pub fn set_purpose(&self, label_or_id: &str, purpose: Option<VolumePurpose>) -> Result<Volume> {
        let mut volumes = self.load()?;

        let vol = volumes.iter_mut().find(|v| {
            v.label == label_or_id
                || uuid::Uuid::parse_str(label_or_id)
                    .map(|u| v.id == u)
                    .unwrap_or(false)
        });

        match vol {
            Some(v) => {
                v.purpose = purpose;
                let result = v.clone();
                self.save(&volumes)?;
                Ok(result)
            }
            None => anyhow::bail!("no volume found matching '{}'", label_or_id),
        }
    }

    /// List all volumes with online/offline status.
    pub fn list(&self) -> Result<Vec<Volume>> {
        let mut volumes = self.load()?;
        for v in &mut volumes {
            v.is_online = v.mount_point.exists();
        }
        Ok(volumes)
    }

    /// Find the volume whose mount_point is a prefix of the given path.
    /// Uses longest prefix match if multiple volumes match.
    pub fn find_volume_for_path(&self, path: &Path) -> Result<Volume> {
        let volumes = self.list()?;
        let mut best: Option<&Volume> = None;
        let mut best_len = 0;

        for v in &volumes {
            if path.starts_with(&v.mount_point) {
                let len = v.mount_point.as_os_str().len();
                if len > best_len {
                    best = Some(v);
                    best_len = len;
                }
            }
        }

        best.cloned().ok_or_else(|| {
            anyhow::anyhow!(
                "No registered volume contains path: {}",
                path.display()
            )
        })
    }

    /// Find a volume by label or UUID string.
    pub fn resolve_volume(&self, label_or_id: &str) -> Result<Volume> {
        let volumes = self.list()?;

        // Try UUID match first
        if let Ok(uuid) = uuid::Uuid::parse_str(label_or_id) {
            if let Some(v) = volumes.iter().find(|v| v.id == uuid) {
                return Ok(v.clone());
            }
        }

        // Fall back to label match
        if let Some(v) = volumes.iter().find(|v| v.label == label_or_id) {
            return Ok(v.clone());
        }

        let labels: Vec<&str> = volumes.iter().map(|v| v.label.as_str()).collect();
        anyhow::bail!(
            "no volume found matching '{}'. Known volumes: {}",
            label_or_id,
            if labels.is_empty() {
                "(none)".to_string()
            } else {
                labels.join(", ")
            }
        )
    }

    /// Remove a volume by label or UUID. Returns the removed volume.
    pub fn remove(&self, label_or_id: &str) -> Result<Volume> {
        let mut volumes = self.load()?;

        let idx = volumes.iter().position(|v| {
            v.label == label_or_id
                || uuid::Uuid::parse_str(label_or_id)
                    .map(|u| v.id == u)
                    .unwrap_or(false)
        });

        match idx {
            Some(i) => {
                let removed = volumes.remove(i);
                self.save(&volumes)?;
                Ok(removed)
            }
            None => anyhow::bail!("no volume found matching '{}'", label_or_id),
        }
    }

    /// Rename a volume by label or UUID.
    pub fn rename(&self, label_or_id: &str, new_label: &str) -> Result<()> {
        let mut volumes = self.load()?;

        let vol = volumes.iter_mut().find(|v| {
            v.label == label_or_id
                || uuid::Uuid::parse_str(label_or_id)
                    .map(|u| v.id == u)
                    .unwrap_or(false)
        });

        match vol {
            Some(v) => {
                v.label = new_label.to_string();
                self.save(&volumes)?;
                Ok(())
            }
            None => anyhow::bail!("no volume found matching '{}'", label_or_id),
        }
    }

    /// Mount points of the volumes that are online right now, keyed by
    /// volume id. The one shape every write path that touches files on
    /// disk (XMP writeback, sidecar creation, relocate) consumes. A
    /// catalog without a `volumes.yaml` has no volumes (empty map); an
    /// unreadable or malformed file is an error, never "everything
    /// offline".
    pub fn online_map(&self) -> Result<HashMap<Uuid, PathBuf>> {
        Ok(Self::online_map_of(&self.list_if_present()?))
    }

    /// Labels of ALL registered volumes (online or not), keyed by id, so
    /// reports can name the offline drives a user needs to reconnect.
    /// Same missing-file rule as [`Self::online_map`].
    pub fn label_map(&self) -> Result<HashMap<Uuid, String>> {
        Ok(Self::label_map_of(&self.list_if_present()?))
    }

    /// [`Self::list`], or an empty list when `volumes.yaml` does not exist.
    fn list_if_present(&self) -> Result<Vec<Volume>> {
        if !self.volumes_path().exists() {
            return Ok(Vec::new());
        }
        self.list()
    }

    /// [`Self::online_map`] over an already-listed volume set.
    pub fn online_map_of(volumes: &[Volume]) -> HashMap<Uuid, PathBuf> {
        volumes
            .iter()
            .filter(|v| v.is_online)
            .map(|v| (v.id, v.mount_point.clone()))
            .collect()
    }

    /// [`Self::label_map`] over an already-listed volume set.
    pub fn label_map_of(volumes: &[Volume]) -> HashMap<Uuid, String> {
        volumes.iter().map(|v| (v.id, v.label.clone())).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn setup() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        DeviceRegistry::init(dir.path()).unwrap();
        dir
    }

    #[test]
    fn register_creates_volume_and_persists() {
        let dir = setup();
        let registry = DeviceRegistry::new(dir.path());

        let vol = registry
            .register("Photos", Path::new("/mnt/photos"), VolumeType::Local, None)
            .unwrap();

        assert_eq!(vol.label, "Photos");
        assert_eq!(vol.mount_point, Path::new("/mnt/photos"));
        assert_eq!(vol.volume_type, VolumeType::Local);

        // Verify it was persisted by loading from disk
        let volumes = registry.load().unwrap();
        assert_eq!(volumes.len(), 1);
        assert_eq!(volumes[0].id, vol.id);
    }

    #[test]
    fn online_map_without_registry_file_is_empty_but_malformed_file_errors() {
        let dir = tempfile::tempdir().unwrap();
        let registry = DeviceRegistry::new(dir.path());
        assert!(registry.online_map().unwrap().is_empty());
        assert!(registry.label_map().unwrap().is_empty());

        std::fs::write(dir.path().join("volumes.yaml"), "- not: [valid").unwrap();
        assert!(registry.online_map().is_err());
    }

    #[test]
    fn online_map_filters_offline_and_label_map_keeps_all() {
        let dir = setup();
        let registry = DeviceRegistry::new(dir.path());
        // The temp dir exists (online); the other mount point does not.
        let on = registry
            .register("On", dir.path(), VolumeType::Local, None)
            .unwrap();
        let off = registry
            .register("Off", Path::new("/nonexistent/maki-offline-volume"), VolumeType::External, None)
            .unwrap();

        let online = registry.online_map().unwrap();
        assert_eq!(online.len(), 1);
        assert_eq!(online.get(&on.id), Some(&dir.path().to_path_buf()));
        assert!(!online.contains_key(&off.id));

        let labels = registry.label_map().unwrap();
        assert_eq!(labels.len(), 2);
        assert_eq!(labels.get(&on.id).map(String::as_str), Some("On"));
        assert_eq!(labels.get(&off.id).map(String::as_str), Some("Off"));
    }

    #[test]
    fn register_rejects_duplicate_label() {
        let dir = setup();
        let registry = DeviceRegistry::new(dir.path());

        registry
            .register("Backup", Path::new("/mnt/backup"), VolumeType::External, None)
            .unwrap();

        let err = registry
            .register("Backup", Path::new("/mnt/other"), VolumeType::Local, None)
            .unwrap_err();

        assert!(err.to_string().contains("already exists"));
    }

    #[test]
    fn register_multiple_volumes() {
        let dir = setup();
        let registry = DeviceRegistry::new(dir.path());

        let v1 = registry
            .register("Drive A", Path::new("/mnt/a"), VolumeType::Local, None)
            .unwrap();
        let v2 = registry
            .register("Drive B", Path::new("/mnt/b"), VolumeType::External, None)
            .unwrap();

        assert_ne!(v1.id, v2.id);

        let volumes = registry.load().unwrap();
        assert_eq!(volumes.len(), 2);
    }

    #[test]
    fn resolve_volume_by_label() {
        let dir = setup();
        let registry = DeviceRegistry::new(dir.path());
        registry
            .register("Photos", Path::new("/mnt/photos"), VolumeType::Local, None)
            .unwrap();

        let vol = registry.resolve_volume("Photos").unwrap();
        assert_eq!(vol.label, "Photos");
    }

    #[test]
    fn resolve_volume_by_uuid() {
        let dir = setup();
        let registry = DeviceRegistry::new(dir.path());
        let registered = registry
            .register("Photos", Path::new("/mnt/photos"), VolumeType::Local, None)
            .unwrap();

        let vol = registry
            .resolve_volume(&registered.id.to_string())
            .unwrap();
        assert_eq!(vol.id, registered.id);
        assert_eq!(vol.label, "Photos");
    }

    #[test]
    fn resolve_volume_unknown_errors() {
        let dir = setup();
        let registry = DeviceRegistry::new(dir.path());
        registry
            .register("Photos", Path::new("/mnt/photos"), VolumeType::Local, None)
            .unwrap();

        let err = registry.resolve_volume("Nonexistent").unwrap_err();
        assert!(err.to_string().contains("no volume found"));
    }

    #[test]
    fn list_detects_online_offline() {
        let dir = setup();
        let registry = DeviceRegistry::new(dir.path());

        // Register a volume pointing at a path inside the temp dir (exists)
        let online_path = dir.path().join("online-vol");
        std::fs::create_dir(&online_path).unwrap();
        registry
            .register("Online", &online_path, VolumeType::Local, None)
            .unwrap();

        // Register a volume pointing at a path that doesn't exist
        registry
            .register("Offline", Path::new("/nonexistent/volume"), VolumeType::External, None)
            .unwrap();

        let volumes = registry.list().unwrap();
        let online = volumes.iter().find(|v| v.label == "Online").unwrap();
        let offline = volumes.iter().find(|v| v.label == "Offline").unwrap();

        assert!(online.is_online);
        assert!(!offline.is_online);
    }

    #[test]
    fn register_with_purpose() {
        let dir = setup();
        let registry = DeviceRegistry::new(dir.path());

        let vol = registry
            .register(
                "Backup",
                Path::new("/mnt/backup"),
                VolumeType::External,
                Some(VolumePurpose::Backup),
            )
            .unwrap();

        assert_eq!(vol.purpose, Some(VolumePurpose::Backup));

        // Verify persisted
        let volumes = registry.load().unwrap();
        assert_eq!(volumes[0].purpose, Some(VolumePurpose::Backup));
    }

    #[test]
    fn register_without_purpose() {
        let dir = setup();
        let registry = DeviceRegistry::new(dir.path());

        let vol = registry
            .register("Local", Path::new("/mnt/local"), VolumeType::Local, None)
            .unwrap();

        assert_eq!(vol.purpose, None);
    }

    #[test]
    fn set_purpose_by_label() {
        let dir = setup();
        let registry = DeviceRegistry::new(dir.path());

        registry
            .register("Photos", Path::new("/mnt/photos"), VolumeType::Local, None)
            .unwrap();

        let vol = registry.set_purpose("Photos", Some(VolumePurpose::Archive)).unwrap();
        assert_eq!(vol.purpose, Some(VolumePurpose::Archive));

        // Verify persisted
        let volumes = registry.load().unwrap();
        assert_eq!(volumes[0].purpose, Some(VolumePurpose::Archive));
    }

    #[test]
    fn set_purpose_clear() {
        let dir = setup();
        let registry = DeviceRegistry::new(dir.path());

        registry
            .register(
                "Backup",
                Path::new("/mnt/backup"),
                VolumeType::External,
                Some(VolumePurpose::Backup),
            )
            .unwrap();

        let vol = registry.set_purpose("Backup", None).unwrap();
        assert_eq!(vol.purpose, None);
    }

    #[test]
    fn set_purpose_unknown_volume_errors() {
        let dir = setup();
        let registry = DeviceRegistry::new(dir.path());

        let err = registry
            .set_purpose("Nonexistent", Some(VolumePurpose::Working))
            .unwrap_err();
        assert!(err.to_string().contains("no volume found"));
    }

    #[test]
    fn volume_purpose_parse() {
        assert_eq!(VolumePurpose::parse("working"), Some(VolumePurpose::Working));
        assert_eq!(VolumePurpose::parse("Archive"), Some(VolumePurpose::Archive));
        assert_eq!(VolumePurpose::parse("BACKUP"), Some(VolumePurpose::Backup));
        assert_eq!(VolumePurpose::parse("cloud"), Some(VolumePurpose::Cloud));
        assert_eq!(VolumePurpose::parse("Media"), Some(VolumePurpose::Media));
        assert_eq!(VolumePurpose::parse("invalid"), None);
    }
}
