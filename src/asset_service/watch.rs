//! `watch` section — poll-based filesystem watching primitives.
//!
//! The scanner / debounce state machine for `maki watch` lives here as a
//! plain library type; the CLI command (`run_watch_command`) is just a
//! loop that drives it and feeds the results into the existing import
//! and refresh pipelines. Poll-based by design — no fsevents/inotify —
//! so the behavior is identical across macOS, Linux, and Windows.
//!
//! Debounce rule: a new or changed file is only *reported* once its
//! `(size, mtime)` signature is identical on two consecutive scans.
//! Files still being copied (e.g. during a tethered capture session)
//! stay "pending" until they stop growing.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use super::{resolve_files, FileTypeFilter};

/// `(size, mtime)` signature used for change detection and the
/// two-consecutive-scans stability debounce.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileSig {
    pub size: u64,
    pub mtime: Option<SystemTime>,
}

impl FileSig {
    /// Read the signature from disk. `None` when the file vanished
    /// between the directory walk and the stat.
    pub fn from_path(path: &Path) -> Option<Self> {
        let meta = std::fs::metadata(path).ok()?;
        Some(Self {
            size: meta.len(),
            mtime: meta.modified().ok(),
        })
    }
}

/// Per-file debounce state.
enum EntryState {
    /// Signature unchanged since last report (or part of the baseline).
    Stable,
    /// Appeared after the baseline; waiting for the signature to settle.
    PendingNew,
    /// Was stable, then the signature changed; waiting for it to settle.
    PendingChanged,
}

struct Entry {
    sig: FileSig,
    state: EntryState,
}

/// Outcome of one scan tick.
#[derive(Debug, Default)]
pub struct ScanOutcome {
    /// Files that appeared after the baseline and are now stable.
    pub new_stable: Vec<PathBuf>,
    /// Previously-stable files whose content changed and re-settled.
    pub changed_stable: Vec<PathBuf>,
    /// Files that disappeared since the last scan (reported, never acted on).
    pub deleted: Vec<PathBuf>,
    /// Files still waiting for their signature to settle.
    pub pending: usize,
    /// Total files currently tracked.
    pub tracked: usize,
}

impl ScanOutcome {
    /// True when the tick produced nothing to act on or wait for.
    pub fn is_quiet(&self) -> bool {
        self.new_stable.is_empty()
            && self.changed_stable.is_empty()
            && self.deleted.is_empty()
            && self.pending == 0
    }
}

/// Returns true when the path's extension marks it as a recipe sidecar
/// (`.xmp` with the default filter). Used to route changed files to the
/// refresh pipeline instead of import.
pub fn is_recipe_path(path: &Path) -> bool {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
    FileTypeFilter::default().is_recipe(ext)
}

/// Poll-based watch scanner with `(size, mtime)` stability debounce.
///
/// The first `scan()` establishes the baseline. In the default mode
/// (`baseline_pending = false`) every file present at baseline is
/// recorded as `Stable` and never reported — "watch means from now on".
/// With `baseline_pending = true` (used by `maki watch --once`) the
/// baseline files start out `PendingNew`, so the *second* scan reports
/// every file whose signature held still; the caller then filters those
/// against the catalog to find genuinely-new work.
pub struct WatchScanner {
    roots: Vec<PathBuf>,
    exclude: Vec<String>,
    /// Paths whose subtrees are never tracked (the catalog's own
    /// `previews/`, `metadata/`, … when a watched root contains the
    /// catalog directory).
    ignore_prefixes: Vec<PathBuf>,
    filter: FileTypeFilter,
    entries: HashMap<PathBuf, Entry>,
    baselined: bool,
    baseline_pending: bool,
}

impl WatchScanner {
    pub fn new(
        roots: Vec<PathBuf>,
        exclude: Vec<String>,
        ignore_prefixes: Vec<PathBuf>,
        baseline_pending: bool,
    ) -> Self {
        Self {
            roots,
            exclude,
            ignore_prefixes,
            filter: FileTypeFilter::default(),
            entries: HashMap::new(),
            baselined: false,
            baseline_pending,
        }
    }

    /// Walk the watched roots and build the current `(path → sig)`
    /// snapshot. Applies exclude patterns (same matching as import),
    /// skips hidden files, ignore-prefixes, and non-importable types.
    fn snapshot(&self) -> HashMap<PathBuf, FileSig> {
        let files = resolve_files(&self.roots, &self.exclude);
        let mut snapshot = HashMap::new();
        for path in files {
            if self.ignore_prefixes.iter().any(|p| path.starts_with(p)) {
                continue;
            }
            let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
            if !self.filter.is_importable(ext) {
                continue;
            }
            if let Some(sig) = FileSig::from_path(&path) {
                snapshot.insert(path, sig);
            }
        }
        snapshot
    }

    /// Perform one scan tick against the filesystem.
    pub fn scan(&mut self) -> ScanOutcome {
        let snapshot = self.snapshot();
        self.observe(snapshot)
    }

    /// Pure state transition for one tick, given the observed snapshot.
    /// Split from `scan()` so tests can feed synthetic snapshots without
    /// fighting filesystem mtime granularity.
    pub fn observe(&mut self, snapshot: HashMap<PathBuf, FileSig>) -> ScanOutcome {
        let mut out = ScanOutcome::default();

        if !self.baselined {
            let state_for_baseline = || {
                if self.baseline_pending {
                    EntryState::PendingNew
                } else {
                    EntryState::Stable
                }
            };
            for (path, sig) in snapshot {
                self.entries.insert(
                    path,
                    Entry {
                        sig,
                        state: state_for_baseline(),
                    },
                );
            }
            self.baselined = true;
            out.pending = self.count_pending();
            out.tracked = self.entries.len();
            return out;
        }

        // Deletions: tracked entries missing from the snapshot.
        let deleted: Vec<PathBuf> = self
            .entries
            .keys()
            .filter(|p| !snapshot.contains_key(*p))
            .cloned()
            .collect();
        for p in &deleted {
            self.entries.remove(p);
        }
        out.deleted = deleted;

        for (path, sig) in snapshot {
            match self.entries.get_mut(&path) {
                None => {
                    // Newly appeared — wait one more scan for stability.
                    self.entries.insert(
                        path,
                        Entry {
                            sig,
                            state: EntryState::PendingNew,
                        },
                    );
                }
                Some(entry) => {
                    if entry.sig == sig {
                        match entry.state {
                            EntryState::Stable => {}
                            EntryState::PendingNew => {
                                entry.state = EntryState::Stable;
                                out.new_stable.push(path);
                            }
                            EntryState::PendingChanged => {
                                entry.state = EntryState::Stable;
                                out.changed_stable.push(path);
                            }
                        }
                    } else {
                        // Still being written (or changed again) — keep
                        // pending with the latest signature.
                        if matches!(entry.state, EntryState::Stable) {
                            entry.state = EntryState::PendingChanged;
                        }
                        entry.sig = sig;
                    }
                }
            }
        }

        out.pending = self.count_pending();
        out.tracked = self.entries.len();
        out.new_stable.sort();
        out.changed_stable.sort();
        out.deleted.sort();
        out
    }

    fn count_pending(&self) -> usize {
        self.entries
            .values()
            .filter(|e| !matches!(e.state, EntryState::Stable))
            .count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sig(size: u64) -> FileSig {
        FileSig { size, mtime: None }
    }

    fn snap(files: &[(&str, u64)]) -> HashMap<PathBuf, FileSig> {
        files
            .iter()
            .map(|(name, size)| (PathBuf::from(name), sig(*size)))
            .collect()
    }

    fn scanner() -> WatchScanner {
        WatchScanner::new(vec![], vec![], vec![], false)
    }

    #[test]
    fn new_file_pending_then_stable_then_reported() {
        let mut s = scanner();
        let baseline = s.observe(snap(&[("a.jpg", 10)]));
        assert!(baseline.new_stable.is_empty());
        assert_eq!(baseline.tracked, 1);

        // New file appears — pending, not reported.
        let o = s.observe(snap(&[("a.jpg", 10), ("b.jpg", 5)]));
        assert!(o.new_stable.is_empty());
        assert_eq!(o.pending, 1);

        // Same signature on the next scan — reported as stable-new.
        let o = s.observe(snap(&[("a.jpg", 10), ("b.jpg", 5)]));
        assert_eq!(o.new_stable, vec![PathBuf::from("b.jpg")]);
        assert_eq!(o.pending, 0);

        // Never reported twice.
        let o = s.observe(snap(&[("a.jpg", 10), ("b.jpg", 5)]));
        assert!(o.new_stable.is_empty());
    }

    #[test]
    fn growing_file_stays_pending() {
        let mut s = scanner();
        s.observe(snap(&[]));
        s.observe(snap(&[("copy.nef", 100)]));
        // Still growing — must NOT be reported.
        let o = s.observe(snap(&[("copy.nef", 200)]));
        assert!(o.new_stable.is_empty());
        assert_eq!(o.pending, 1);
        // Settled — reported now.
        let o = s.observe(snap(&[("copy.nef", 200)]));
        assert_eq!(o.new_stable, vec![PathBuf::from("copy.nef")]);
    }

    #[test]
    fn changed_stable_file_reported_after_settling() {
        let mut s = scanner();
        s.observe(snap(&[("r.xmp", 10)])); // baseline → stable
        let o = s.observe(snap(&[("r.xmp", 12)]));
        assert!(o.changed_stable.is_empty());
        assert_eq!(o.pending, 1);
        let o = s.observe(snap(&[("r.xmp", 12)]));
        assert_eq!(o.changed_stable, vec![PathBuf::from("r.xmp")]);
        assert!(o.new_stable.is_empty());
        assert_eq!(o.pending, 0);
    }

    #[test]
    fn deletion_reported_and_untracked() {
        let mut s = scanner();
        s.observe(snap(&[("a.jpg", 10), ("b.jpg", 5)]));
        let o = s.observe(snap(&[("a.jpg", 10)]));
        assert_eq!(o.deleted, vec![PathBuf::from("b.jpg")]);
        assert_eq!(o.tracked, 1);
        // Re-appearing later counts as new again (pending first).
        let o = s.observe(snap(&[("a.jpg", 10), ("b.jpg", 5)]));
        assert!(o.new_stable.is_empty());
        assert_eq!(o.pending, 1);
    }

    #[test]
    fn baseline_pending_mode_promotes_on_second_scan() {
        let mut s = WatchScanner::new(vec![], vec![], vec![], true);
        let baseline = s.observe(snap(&[("old.jpg", 7)]));
        assert!(baseline.new_stable.is_empty());
        assert_eq!(baseline.pending, 1);
        let o = s.observe(snap(&[("old.jpg", 7)]));
        assert_eq!(o.new_stable, vec![PathBuf::from("old.jpg")]);
    }

    #[test]
    fn scan_applies_exclude_patterns() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("keep.jpg"), b"x").unwrap();
        std::fs::write(dir.path().join("skip.tmp"), b"x").unwrap();
        std::fs::write(dir.path().join("Thumbs.db"), b"x").unwrap();
        let mut s = WatchScanner::new(
            vec![dir.path().to_path_buf()],
            vec!["*.tmp".to_string(), "Thumbs.db".to_string()],
            vec![],
            false,
        );
        let baseline = s.scan();
        assert_eq!(baseline.tracked, 1);
    }

    #[test]
    fn scan_skips_ignore_prefixes_and_non_importable() {
        let dir = tempfile::tempdir().unwrap();
        let previews = dir.path().join("previews");
        std::fs::create_dir_all(&previews).unwrap();
        std::fs::write(previews.join("thumb.jpg"), b"x").unwrap();
        std::fs::write(dir.path().join("photo.jpg"), b"x").unwrap();
        std::fs::write(dir.path().join("notes.yaml"), b"x").unwrap();
        let mut s = WatchScanner::new(
            vec![dir.path().to_path_buf()],
            vec![],
            vec![previews],
            false,
        );
        let baseline = s.scan();
        assert_eq!(baseline.tracked, 1); // only photo.jpg
    }

    #[test]
    fn classifies_recipes_vs_media() {
        assert!(is_recipe_path(Path::new("/x/DSC_1.xmp")));
        assert!(is_recipe_path(Path::new("/x/DSC_1.XMP")));
        assert!(!is_recipe_path(Path::new("/x/DSC_1.nef")));
        assert!(!is_recipe_path(Path::new("/x/DSC_1.jpg")));
    }
}
