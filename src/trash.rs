//! Trash / quarantine for destructive file operations.
//!
//! Deleting commands (`delete --remove-files`, `dedup`, the web
//! duplicates page) move media and recipe files into
//! `<catalog_root>/.trash/<YYYY-MM-DD>/<volume_label>/<relative_path>`
//! instead of unlinking them, so a mistaken batch delete is recoverable
//! with `maki trash restore`. Derived files (previews, embedding
//! binaries, face crops) are exempt — they are regenerable and would
//! only bloat the trash.
//!
//! Design notes:
//! - The trash lives on the **catalog volume**. Disposing a file from
//!   another volume falls back to copy+delete when `rename` fails
//!   (cross-device); for huge files this costs a copy, which is the
//!   price of recoverability. `--no-trash` is the escape hatch.
//! - Restore puts the file back at its original volume/relative path
//!   (volume resolved by label, must be online). It does NOT touch the
//!   catalog — re-register the file with `maki import <path>` or
//!   `maki refresh` afterwards.
//! - `[trash] enabled = false` in maki.toml turns the whole mechanism
//!   off; `[trash] retention_days` (default 30) is the default age
//!   cutoff for `maki trash empty`.

use std::path::{Path, PathBuf};

use anyhow::Result;

/// Directory name under the catalog root.
pub const TRASH_DIR: &str = ".trash";

/// What `dispose` did with the file.
#[derive(Debug, PartialEq, Eq)]
pub enum Disposition {
    /// Moved into the trash at this path.
    Trashed(PathBuf),
    /// Permanently deleted (trash disabled).
    Deleted,
}

/// One file in the trash, as reported by [`Trash::list`].
#[derive(Debug, serde::Serialize)]
pub struct TrashEntry {
    /// Date directory the file was trashed under (`YYYY-MM-DD`).
    pub date: String,
    /// Volume label the file was deleted from.
    pub volume_label: String,
    /// Original path relative to the volume mount point.
    pub relative_path: String,
    /// File size in bytes.
    pub size: u64,
    /// Key for `maki trash restore` — path relative to the trash root
    /// (`<date>/<volume_label>/<relative_path>`).
    pub trash_path: String,
}

/// Summary returned by [`Trash::empty`].
#[derive(Debug, Default, serde::Serialize)]
pub struct EmptyResult {
    pub files_removed: usize,
    pub bytes_freed: u64,
    pub dry_run: bool,
}

/// Handle for disposing files. Construct per operation via
/// [`Trash::from_config`] (honors `[trash] enabled`) and the caller's
/// `--no-trash` override.
pub struct Trash {
    trash_root: PathBuf,
    enabled: bool,
    date_dir: String,
}

impl Trash {
    /// Build a trash handle. `enabled = false` makes [`Trash::dispose`]
    /// delete permanently (the `--no-trash` path).
    pub fn new(catalog_root: &Path, enabled: bool) -> Self {
        Self {
            trash_root: catalog_root.join(TRASH_DIR),
            enabled,
            date_dir: chrono::Local::now().format("%Y-%m-%d").to_string(),
        }
    }

    /// Build from `[trash]` config, AND-ed with the caller's
    /// `--no-trash` flag.
    pub fn from_config(catalog_root: &Path, no_trash: bool) -> Self {
        let enabled = crate::config::CatalogConfig::load(catalog_root)
            .map(|c| c.trash.enabled)
            .unwrap_or(true);
        Self::new(catalog_root, enabled && !no_trash)
    }

    /// Whether disposed files go to the trash (vs. permanent deletion).
    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// Dispose of a media/recipe file: move it into the trash
    /// (preserving volume label + relative path for restore), or delete
    /// it permanently when the trash is disabled.
    ///
    /// Same-filesystem disposal is a rename; cross-device falls back to
    /// copy + delete. Name collisions inside the trash get a `-N`
    /// suffix before the extension.
    pub fn dispose(
        &self,
        full_path: &Path,
        volume_label: &str,
        relative_path: &str,
    ) -> std::io::Result<Disposition> {
        if !self.enabled {
            std::fs::remove_file(full_path)?;
            return Ok(Disposition::Deleted);
        }
        let target_base = self
            .trash_root
            .join(&self.date_dir)
            .join(sanitize_label(volume_label))
            .join(relative_path);
        if let Some(parent) = target_base.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let target = unique_target(target_base);
        match std::fs::rename(full_path, &target) {
            Ok(()) => Ok(Disposition::Trashed(target)),
            Err(_) => {
                // Cross-device (trash lives on the catalog volume, the
                // file on a media volume): copy, then delete the source.
                std::fs::copy(full_path, &target)?;
                std::fs::remove_file(full_path)?;
                Ok(Disposition::Trashed(target))
            }
        }
    }

    /// List every file currently in the trash, newest date first.
    pub fn list(catalog_root: &Path) -> Result<Vec<TrashEntry>> {
        let trash_root = catalog_root.join(TRASH_DIR);
        let mut entries = Vec::new();
        if !trash_root.exists() {
            return Ok(entries);
        }
        for date_entry in std::fs::read_dir(&trash_root)? {
            let date_entry = date_entry?;
            if !date_entry.file_type()?.is_dir() {
                continue;
            }
            let date = date_entry.file_name().to_string_lossy().to_string();
            for vol_entry in std::fs::read_dir(date_entry.path())? {
                let vol_entry = vol_entry?;
                if !vol_entry.file_type()?.is_dir() {
                    continue;
                }
                let volume_label = vol_entry.file_name().to_string_lossy().to_string();
                let vol_path = vol_entry.path();
                collect_files(&vol_path, &mut |file: &Path| {
                    let relative_path = file
                        .strip_prefix(&vol_path)
                        .unwrap_or(file)
                        .to_string_lossy()
                        .to_string();
                    let size = std::fs::metadata(file).map(|m| m.len()).unwrap_or(0);
                    entries.push(TrashEntry {
                        date: date.clone(),
                        volume_label: volume_label.clone(),
                        relative_path: relative_path.clone(),
                        size,
                        trash_path: format!("{date}/{volume_label}/{relative_path}"),
                    });
                })?;
            }
        }
        entries.sort_by(|a, b| b.date.cmp(&a.date).then(a.trash_path.cmp(&b.trash_path)));
        Ok(entries)
    }

    /// Restore a trashed file to its original volume location.
    ///
    /// `trash_path` is the key shown by `maki trash list`
    /// (`<date>/<volume_label>/<relative_path>`). The volume is resolved
    /// by label and must be online; fails if the destination already
    /// exists. Returns the restored absolute path.
    ///
    /// Catalog records are NOT recreated — re-register the file with
    /// `maki import` or `maki refresh`.
    pub fn restore(catalog_root: &Path, trash_path: &str) -> Result<PathBuf> {
        let source = catalog_root.join(TRASH_DIR).join(trash_path);
        if !source.is_file() {
            anyhow::bail!("not found in trash: {trash_path}");
        }
        let mut parts = trash_path.splitn(3, '/');
        let _date = parts.next();
        let volume_label = parts
            .next()
            .ok_or_else(|| anyhow::anyhow!("invalid trash path (expected <date>/<volume>/<path>): {trash_path}"))?;
        let relative_path = parts
            .next()
            .ok_or_else(|| anyhow::anyhow!("invalid trash path (expected <date>/<volume>/<path>): {trash_path}"))?;

        let registry = crate::device_registry::DeviceRegistry::new(catalog_root);
        let volumes = registry.list()?;
        let vol = volumes
            .iter()
            .find(|v| sanitize_label(&v.label) == volume_label)
            .ok_or_else(|| anyhow::anyhow!("no registered volume matches '{volume_label}'"))?;
        if !vol.is_online {
            anyhow::bail!("volume '{}' is offline — connect it and retry", vol.label);
        }
        let target = vol.mount_point.join(relative_path);
        if target.exists() {
            anyhow::bail!("destination already exists: {}", target.display());
        }
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)?;
        }
        if std::fs::rename(&source, &target).is_err() {
            std::fs::copy(&source, &target)?;
            std::fs::remove_file(&source)?;
        }
        Ok(target)
    }

    /// Remove trashed files older than `older_than_days` (by date
    /// directory). `None` empties everything. `dry_run` only counts.
    pub fn empty(
        catalog_root: &Path,
        older_than_days: Option<u32>,
        dry_run: bool,
    ) -> Result<EmptyResult> {
        let trash_root = catalog_root.join(TRASH_DIR);
        let mut result = EmptyResult { dry_run, ..Default::default() };
        if !trash_root.exists() {
            return Ok(result);
        }
        let today = chrono::Local::now().date_naive();
        for date_entry in std::fs::read_dir(&trash_root)? {
            let date_entry = date_entry?;
            if !date_entry.file_type()?.is_dir() {
                continue;
            }
            let name = date_entry.file_name().to_string_lossy().to_string();
            let expired = match older_than_days {
                None => true,
                Some(days) => match chrono::NaiveDate::parse_from_str(&name, "%Y-%m-%d") {
                    Ok(d) => (today - d).num_days() >= days as i64,
                    // Unparseable directory name: leave it alone.
                    Err(_) => false,
                },
            };
            if !expired {
                continue;
            }
            collect_files(&date_entry.path(), &mut |file: &Path| {
                result.files_removed += 1;
                result.bytes_freed += std::fs::metadata(file).map(|m| m.len()).unwrap_or(0);
            })?;
            if !dry_run {
                std::fs::remove_dir_all(date_entry.path())?;
            }
        }
        Ok(result)
    }
}

/// Volume labels become path components — neutralize separators.
fn sanitize_label(label: &str) -> String {
    label.replace(['/', '\\', ':'], "_")
}

/// Append `-N` before the extension until the path is free.
fn unique_target(base: PathBuf) -> PathBuf {
    if !base.exists() {
        return base;
    }
    let stem = base.file_stem().and_then(|s| s.to_str()).unwrap_or("file").to_string();
    let ext = base.extension().and_then(|e| e.to_str()).map(|e| format!(".{e}")).unwrap_or_default();
    let parent = base.parent().map(|p| p.to_path_buf()).unwrap_or_default();
    for n in 1.. {
        let candidate = parent.join(format!("{stem}-{n}{ext}"));
        if !candidate.exists() {
            return candidate;
        }
    }
    unreachable!()
}

/// Depth-first walk calling `f` for every file under `dir`.
fn collect_files(dir: &Path, f: &mut impl FnMut(&Path)) -> std::io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if entry.file_type()?.is_dir() {
            collect_files(&path, f)?;
        } else {
            f(&path);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        (dir, root)
    }

    fn make_file(root: &Path, name: &str) -> PathBuf {
        let p = root.join(name);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, b"data").unwrap();
        p
    }

    #[test]
    fn dispose_moves_into_dated_volume_layout() {
        let (_d, root) = setup();
        let file = make_file(&root, "vol/2024/07/IMG_1.NEF");
        let trash = Trash::new(&root, true);
        let disp = trash.dispose(&file, "ARCHIVE", "2024/07/IMG_1.NEF").unwrap();
        assert!(!file.exists(), "source must be gone");
        match disp {
            Disposition::Trashed(p) => {
                assert!(p.exists());
                let rel = p.strip_prefix(root.join(TRASH_DIR)).unwrap();
                let parts: Vec<_> = rel.components().map(|c| c.as_os_str().to_string_lossy().to_string()).collect();
                assert_eq!(parts.len(), 5, "date/volume/2024/07/file: {parts:?}");
                assert_eq!(parts[1], "ARCHIVE");
                assert_eq!(parts[4], "IMG_1.NEF");
            }
            Disposition::Deleted => panic!("must trash, not delete"),
        }
    }

    #[test]
    fn dispose_deletes_when_disabled() {
        let (_d, root) = setup();
        let file = make_file(&root, "vol/IMG_2.NEF");
        let trash = Trash::new(&root, false);
        let disp = trash.dispose(&file, "ARCHIVE", "IMG_2.NEF").unwrap();
        assert_eq!(disp, Disposition::Deleted);
        assert!(!file.exists());
        assert!(!root.join(TRASH_DIR).exists(), "no trash dir when disabled");
    }

    #[test]
    fn dispose_collision_gets_suffix() {
        let (_d, root) = setup();
        let trash = Trash::new(&root, true);
        let f1 = make_file(&root, "vol/a/IMG.jpg");
        let f2 = make_file(&root, "vol/b/IMG.jpg");
        // Same volume + relative path (e.g. deleted, restored, deleted again)
        let d1 = trash.dispose(&f1, "V", "x/IMG.jpg").unwrap();
        let d2 = trash.dispose(&f2, "V", "x/IMG.jpg").unwrap();
        let (Disposition::Trashed(p1), Disposition::Trashed(p2)) = (d1, d2) else {
            panic!("both must trash")
        };
        assert_ne!(p1, p2);
        assert!(p2.file_name().unwrap().to_string_lossy().contains("IMG-1"));
        assert!(p1.exists() && p2.exists());
    }

    #[test]
    fn list_reports_entries_with_restore_keys() {
        let (_d, root) = setup();
        let trash = Trash::new(&root, true);
        let f = make_file(&root, "vol/2024/IMG_3.NEF");
        trash.dispose(&f, "ARCHIVE", "2024/IMG_3.NEF").unwrap();

        let entries = Trash::list(&root).unwrap();
        assert_eq!(entries.len(), 1);
        let e = &entries[0];
        assert_eq!(e.volume_label, "ARCHIVE");
        assert_eq!(e.relative_path, "2024/IMG_3.NEF");
        assert_eq!(e.size, 4);
        assert!(e.trash_path.ends_with("/ARCHIVE/2024/IMG_3.NEF"));
        assert!(root.join(TRASH_DIR).join(&e.trash_path).is_file());
    }

    #[test]
    fn empty_respects_age_cutoff() {
        let (_d, root) = setup();
        // Build two date dirs by hand: one ancient, one from today.
        let old = root.join(TRASH_DIR).join("2020-01-01").join("V");
        std::fs::create_dir_all(&old).unwrap();
        std::fs::write(old.join("old.jpg"), b"oldfile").unwrap();
        let trash = Trash::new(&root, true);
        let f = make_file(&root, "vol/new.jpg");
        trash.dispose(&f, "V", "new.jpg").unwrap();

        // Dry-run first: counts but removes nothing.
        let dry = Trash::empty(&root, Some(30), true).unwrap();
        assert_eq!(dry.files_removed, 1);
        assert!(old.join("old.jpg").exists());

        let res = Trash::empty(&root, Some(30), false).unwrap();
        assert_eq!(res.files_removed, 1);
        assert_eq!(res.bytes_freed, 7);
        assert!(!old.exists(), "expired date dir removed");
        assert_eq!(Trash::list(&root).unwrap().len(), 1, "today's file survives");

        // No cutoff: everything goes.
        let all = Trash::empty(&root, None, false).unwrap();
        assert_eq!(all.files_removed, 1);
        assert!(Trash::list(&root).unwrap().is_empty());
    }

    #[test]
    fn restore_round_trip() {
        let (_d, root) = setup();
        let mount = root.join("VOLMOUNT");
        std::fs::create_dir_all(&mount).unwrap();
        crate::device_registry::DeviceRegistry::init(&root).unwrap();
        let registry = crate::device_registry::DeviceRegistry::new(&root);
        registry
            .register("ARCHIVE", &mount, crate::models::VolumeType::External, None)
            .unwrap();

        let file = mount.join("2024/IMG_5.NEF");
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, b"raw-bytes").unwrap();

        let trash = Trash::new(&root, true);
        trash.dispose(&file, "ARCHIVE", "2024/IMG_5.NEF").unwrap();
        assert!(!file.exists());

        let key = Trash::list(&root).unwrap()[0].trash_path.clone();
        let restored = Trash::restore(&root, &key).unwrap();
        assert_eq!(restored, file);
        assert_eq!(std::fs::read(&file).unwrap(), b"raw-bytes");
        assert!(Trash::list(&root).unwrap().is_empty(), "trash entry consumed");

        // Restoring again must fail cleanly (gone from trash).
        assert!(Trash::restore(&root, &key).is_err());
    }

    #[test]
    fn restore_requires_known_online_volume() {
        let (_d, root) = setup();
        crate::device_registry::DeviceRegistry::init(&root).unwrap();
        let trash = Trash::new(&root, true);
        let f = make_file(&root, "vol/IMG_4.NEF");
        trash.dispose(&f, "NOSUCH", "IMG_4.NEF").unwrap();
        let key = &Trash::list(&root).unwrap()[0].trash_path.clone();
        let err = Trash::restore(&root, key).unwrap_err().to_string();
        assert!(err.contains("no registered volume"), "{err}");
    }
}
