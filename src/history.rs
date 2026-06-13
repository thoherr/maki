//! Edit-history journal — a non-authoritative, prunable, append-only log
//! of field-level metadata edits, backing `maki undo` and `maki history`.
//!
//! ## What this is (and is not)
//!
//! The journal lives in `<catalog_root>/history/`, one JSON file per
//! operation. It is deliberately **neither** of the two stores MAKI's
//! dual-storage model knows about:
//!
//! - It is **not a sidecar** — not source of truth. Deleting it loses no
//!   asset metadata, only the ability to undo past edits.
//! - It is **not in `catalog.db`** — not a derived cache. `rebuild-catalog`
//!   never touches it, so it survives a full rebuild.
//!
//! Consequently `maki doctor` ignores it entirely, and a user may delete
//! the `history/` directory at any time with no consistency impact.
//!
//! ## Recording point
//!
//! Operations are recorded at the `QueryEngine` mutation layer (the
//! `*_inner` / `set_*` / `batch_*` family in `query.rs`), so CLI, web, and
//! shell edits are all journaled — the web handlers call the same methods.
//!
//! ## File layout & ordering
//!
//! Active operations: `history/<epoch_millis>-<opid>.json`. The
//! fixed-width zero-padded epoch-millis prefix guarantees that a plain
//! lexicographic filename sort is also chronological order, regardless of
//! timezone offset — so "newest" is simply the last filename after a sort.
//! Undone operations are moved into `history/undone/` (the undo marker);
//! they no longer appear in `list()` / `newest_active()`.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::models::Asset;

/// Where an edit originated. Recorded on each operation so `maki history`
/// can show provenance. v4.9.0 labels everything `Cli` (the QueryEngine
/// methods don't yet thread the caller's surface through); web/shell
/// threading is a noted follow-on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum OpSource {
    Cli,
    Web,
    Shell,
}

impl Default for OpSource {
    fn default() -> Self {
        OpSource::Cli
    }
}

/// One asset's before/after state within an operation. Stores the full
/// prior and next `Asset` — sidecars are small, and whole-state is robust:
/// undo restores exactly, and `history` can diff any field.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssetDelta {
    pub asset_id: String,
    pub before: Asset,
    pub after: Asset,
}

/// A single undoable operation: one user-facing edit action that may have
/// touched many assets (a 500-asset batch tag is one `Operation`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Operation {
    pub id: String,
    pub timestamp: String,
    pub command: String,
    pub summary: String,
    #[serde(default)]
    pub source: OpSource,
    pub assets: Vec<AssetDelta>,
}

impl Operation {
    /// Build a new operation, stamping a fresh uuid and the current local
    /// time as an RFC 3339 string.
    pub fn new(command: impl Into<String>, summary: impl Into<String>, source: OpSource, assets: Vec<AssetDelta>) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            timestamp: chrono::Local::now().to_rfc3339(),
            command: command.into(),
            summary: summary.into(),
            source,
            assets,
        }
    }
}

/// The history journal, rooted at `<catalog_root>/history/`.
pub struct HistoryStore {
    history_dir: PathBuf,
    enabled: bool,
    max_operations: usize,
}

impl HistoryStore {
    /// Construct a store from the catalog root, reading `[history]` config
    /// (enabled default true, max_operations default 200).
    pub fn from_config(catalog_root: &Path) -> Self {
        let cfg = crate::config::CatalogConfig::load(catalog_root).unwrap_or_default();
        Self {
            history_dir: catalog_root.join("history"),
            enabled: cfg.history.enabled,
            max_operations: cfg.history.max_operations,
        }
    }

    /// Construct a store with explicit settings (used by tests).
    pub fn new(history_dir: PathBuf, enabled: bool, max_operations: usize) -> Self {
        Self { history_dir, enabled, max_operations }
    }

    fn undone_dir(&self) -> PathBuf {
        self.history_dir.join("undone")
    }

    /// Filename for an operation: zero-padded epoch-millis prefix so a
    /// lexicographic sort is chronological, plus the opid for uniqueness.
    fn file_name(op: &Operation) -> String {
        // Parse the stored RFC 3339 timestamp back to epoch millis. Fall
        // back to "now" if it somehow doesn't parse (never expected — we
        // wrote it ourselves) so we always produce a sortable name.
        let millis = chrono::DateTime::parse_from_rfc3339(&op.timestamp)
            .map(|dt| dt.timestamp_millis())
            .unwrap_or_else(|_| chrono::Local::now().timestamp_millis());
        // 15 digits comfortably covers epoch-millis well past year 33000.
        format!("{millis:015}-{}.json", op.id)
    }

    /// Record an operation. Writes `history/<ts>-<opid>.json` atomically
    /// (temp + rename). Skipped entirely when journaling is disabled or the
    /// operation touched no assets (no-op edits must never journal). After
    /// writing, prunes the journal to the newest `max_operations` files.
    pub fn record(&self, op: Operation) -> Result<()> {
        if !self.enabled || op.assets.is_empty() {
            return Ok(());
        }
        std::fs::create_dir_all(&self.history_dir)
            .with_context(|| format!("creating history dir {}", self.history_dir.display()))?;

        let name = Self::file_name(&op);
        let target = self.history_dir.join(&name);
        let tmp = self.history_dir.join(format!(".{name}.tmp"));
        let json = serde_json::to_string_pretty(&op)?;
        std::fs::write(&tmp, json)?;
        std::fs::rename(&tmp, &target)?;

        self.prune()?;
        Ok(())
    }

    /// List active operation files (those directly in `history/`, not in
    /// `history/undone/`), sorted oldest→newest by filename.
    fn active_files(&self) -> Result<Vec<PathBuf>> {
        let mut files: Vec<PathBuf> = Vec::new();
        let rd = match std::fs::read_dir(&self.history_dir) {
            Ok(rd) => rd,
            Err(_) => return Ok(files), // no history yet
        };
        for entry in rd.flatten() {
            let path = entry.path();
            if path.is_file()
                && path.extension().and_then(|e| e.to_str()) == Some("json")
            {
                files.push(path);
            }
        }
        files.sort();
        Ok(files)
    }

    fn load_file(path: &Path) -> Result<Operation> {
        let data = std::fs::read_to_string(path)
            .with_context(|| format!("reading history file {}", path.display()))?;
        let op: Operation = serde_json::from_str(&data)
            .with_context(|| format!("parsing history file {}", path.display()))?;
        Ok(op)
    }

    /// Prune the active journal to the newest `max_operations` files.
    /// Deletes older active files and their `undone/` counterparts.
    fn prune(&self) -> Result<()> {
        if self.max_operations == 0 {
            return Ok(());
        }
        let files = self.active_files()?;
        if files.len() <= self.max_operations {
            return Ok(());
        }
        let drop_count = files.len() - self.max_operations;
        for path in files.iter().take(drop_count) {
            let _ = std::fs::remove_file(path);
            if let Some(name) = path.file_name() {
                let undone = self.undone_dir().join(name);
                if undone.exists() {
                    let _ = std::fs::remove_file(&undone);
                }
            }
        }
        Ok(())
    }

    /// List active operations, newest first. `limit` caps the count.
    pub fn list(&self, limit: Option<usize>) -> Result<Vec<Operation>> {
        let mut files = self.active_files()?;
        files.reverse(); // newest first
        let mut out = Vec::new();
        for path in files {
            if let Some(l) = limit {
                if out.len() >= l {
                    break;
                }
            }
            if let Ok(op) = Self::load_file(&path) {
                out.push(op);
            }
        }
        Ok(out)
    }

    /// List active operations whose assets include `asset_id`, newest
    /// first. `limit` caps the count.
    pub fn list_for_asset(&self, asset_id: &str, limit: Option<usize>) -> Result<Vec<Operation>> {
        let mut files = self.active_files()?;
        files.reverse();
        let mut out = Vec::new();
        for path in files {
            if let Some(l) = limit {
                if out.len() >= l {
                    break;
                }
            }
            if let Ok(op) = Self::load_file(&path) {
                if op.assets.iter().any(|d| d.asset_id == asset_id) {
                    out.push(op);
                }
            }
        }
        Ok(out)
    }

    /// The newest active operation, for `undo`'s LIFO behaviour.
    pub fn newest_active(&self) -> Result<Option<Operation>> {
        let files = self.active_files()?;
        match files.last() {
            Some(path) => Ok(Some(Self::load_file(path)?)),
            None => Ok(None),
        }
    }

    /// Mark an operation undone by moving its file into `history/undone/`.
    /// No-op if the file isn't found among active operations.
    pub fn mark_undone(&self, op_id: &str) -> Result<()> {
        let suffix = format!("-{op_id}.json");
        for path in self.active_files()? {
            let matches = path
                .file_name()
                .and_then(|n| n.to_str())
                .map(|n| n.ends_with(&suffix))
                .unwrap_or(false);
            if matches {
                std::fs::create_dir_all(self.undone_dir())?;
                let name = path.file_name().unwrap();
                std::fs::rename(&path, self.undone_dir().join(name))?;
                return Ok(());
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{Asset, AssetType};

    fn store(dir: &Path) -> HistoryStore {
        HistoryStore::new(dir.join("history"), true, 200)
    }

    fn sample_asset(rating: Option<u8>) -> Asset {
        let mut a = Asset::new(AssetType::Image, "deadbeef");
        a.rating = rating;
        a.tags = vec!["foo".to_string(), "bar|baz".to_string()];
        a
    }

    fn op(rating_before: Option<u8>, rating_after: Option<u8>) -> Operation {
        let before = sample_asset(rating_before);
        let after = sample_asset(rating_after);
        Operation::new(
            "rating",
            "set rating",
            OpSource::Cli,
            vec![AssetDelta {
                asset_id: before.id.to_string(),
                before,
                after,
            }],
        )
    }

    #[test]
    fn record_list_round_trip_preserves_full_asset() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path());
        let original = op(None, Some(4));
        let asset_id = original.assets[0].asset_id.clone();
        let before_tags = original.assets[0].before.tags.clone();
        s.record(original).unwrap();

        let listed = s.list(None).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].assets[0].asset_id, asset_id);
        assert_eq!(listed[0].assets[0].before.rating, None);
        assert_eq!(listed[0].assets[0].after.rating, Some(4));
        // Full Asset state survives the round-trip.
        assert_eq!(listed[0].assets[0].before.tags, before_tags);
        assert_eq!(listed[0].source, OpSource::Cli);
    }

    #[test]
    fn empty_operation_not_recorded() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path());
        let empty = Operation::new("tag", "no change", OpSource::Cli, vec![]);
        s.record(empty).unwrap();
        assert_eq!(s.list(None).unwrap().len(), 0);
        assert!(s.newest_active().unwrap().is_none());
    }

    #[test]
    fn disabled_store_records_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let s = HistoryStore::new(dir.path().join("history"), false, 200);
        s.record(op(None, Some(3))).unwrap();
        assert_eq!(s.list(None).unwrap().len(), 0);
    }

    #[test]
    fn prune_keeps_newest_n() {
        let dir = tempfile::tempdir().unwrap();
        let s = HistoryStore::new(dir.path().join("history"), true, 3);
        // Record 5 ops with strictly increasing timestamps so filenames sort.
        for i in 0..5u8 {
            let before = sample_asset(None);
            let after = sample_asset(Some(i));
            let mut o = Operation::new(
                "rating",
                format!("set rating to {i}"),
                OpSource::Cli,
                vec![AssetDelta { asset_id: before.id.to_string(), before, after }],
            );
            // Force distinct, monotonically increasing timestamps.
            o.timestamp = chrono::DateTime::from_timestamp_millis(1_000_000 + i as i64 * 1000)
                .unwrap()
                .to_rfc3339();
            s.record(o).unwrap();
        }
        let listed = s.list(None).unwrap();
        assert_eq!(listed.len(), 3, "prune should keep newest 3");
        // Newest first: the last three summaries (2, 3, 4).
        assert_eq!(listed[0].summary, "set rating to 4");
        assert_eq!(listed[2].summary, "set rating to 2");
    }

    #[test]
    fn mark_undone_removes_from_active() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path());
        let o = op(None, Some(5));
        let id = o.id.clone();
        s.record(o).unwrap();
        assert_eq!(s.list(None).unwrap().len(), 1);

        s.mark_undone(&id).unwrap();
        assert_eq!(s.list(None).unwrap().len(), 0);
        assert!(s.newest_active().unwrap().is_none());
        // The file moved into undone/.
        assert!(dir.path().join("history/undone").exists());
    }

    #[test]
    fn list_for_asset_filters_by_asset() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path());

        let target = op(None, Some(2));
        let target_id = target.assets[0].asset_id.clone();
        s.record(target).unwrap();

        // A second op on a different asset.
        let mut other_before = Asset::new(AssetType::Image, "cafef00d");
        other_before.rating = None;
        let mut other_after = other_before.clone();
        other_after.rating = Some(1);
        let other_id = other_before.id.to_string();
        let mut other = Operation::new(
            "rating",
            "set rating",
            OpSource::Cli,
            vec![AssetDelta { asset_id: other_id.clone(), before: other_before, after: other_after }],
        );
        other.timestamp = chrono::Local::now()
            .checked_add_signed(chrono::Duration::seconds(1))
            .unwrap()
            .to_rfc3339();
        s.record(other).unwrap();

        let for_target = s.list_for_asset(&target_id, None).unwrap();
        assert_eq!(for_target.len(), 1);
        assert_eq!(for_target[0].assets[0].asset_id, target_id);

        let for_other = s.list_for_asset(&other_id, None).unwrap();
        assert_eq!(for_other.len(), 1);
        assert_eq!(for_other[0].assets[0].asset_id, other_id);
    }
}
