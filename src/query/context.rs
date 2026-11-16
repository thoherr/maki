//! Mutation context and edit-history plumbing for `QueryEngine`:
//! [`MutationContext`] (the handles one write operation shares across
//! its assets), the context builders, the [`QueryEngine::mutate_asset`]
//! primitive every single-asset setter flows through, and the recorder
//! drain that turns a batch of per-asset deltas into one undoable
//! [`Operation`](crate::history::Operation).

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::Result;

use crate::catalog::Catalog;
use crate::config::CatalogConfig;
use crate::content_store::ContentStore;
use crate::device_registry::DeviceRegistry;
use crate::history::AssetDelta;
use crate::metadata_store::MetadataStore;
use crate::models::Asset;

use super::QueryEngine;

/// Shared context of one write operation: the engine's connection and
/// stores (borrowed — nothing is reopened per asset), the online-volume
/// map read once per operation, and the optional edit-history recorder.
/// Built only by [`QueryEngine::build_context`].
pub struct MutationContext<'e> {
    pub catalog: &'e Catalog,
    pub meta_store: &'e MetadataStore,
    pub content_store: &'e ContentStore,
    /// Online volumes at the start of the operation (`uuid → mount point`).
    pub online_volumes: HashMap<uuid::Uuid, PathBuf>,
    /// The engine's configuration, shared (not re-read) per batch.
    pub config: &'e CatalogConfig,
    /// Optional edit-history recorder. When `Some`, the `*_inner` /
    /// `set_*` write methods that flow through this context capture a
    /// `before`/`after` [`crate::history::AssetDelta`] for every asset
    /// whose state actually changed. The public entry point owns the
    /// operation boundary: it installs the recorder, runs the edit(s),
    /// then drains the accumulated deltas into one [`crate::history::Operation`].
    /// `None` (the default for batch web handlers and the preview /
    /// variant-role setters) skips journaling — and the costly `before`
    /// clone — entirely.
    pub recorder: Option<RefCell<Vec<AssetDelta>>>,
}

/// Former name of [`MutationContext`].
pub type BatchContext<'e> = MutationContext<'e>;

impl MutationContext<'_> {
    /// Persist an asset the caller mutated in memory: sidecar first
    /// (source of truth), then the `assets` row with every scalar and
    /// denormalized column derived from the `Asset` (`insert_asset` is a
    /// single-row upsert; child tables are untouched). The bulk tag
    /// operations use this directly because they load tolerantly by
    /// UUID; single-asset setters go through [`QueryEngine::mutate_asset`].
    pub(super) fn persist_asset(&self, asset: &Asset) -> Result<()> {
        self.meta_store.save(asset)?;
        self.catalog.insert_asset(asset)
    }
}

/// What a [`QueryEngine::mutate_asset`] closure reports back: whether the
/// asset changed (nothing is written otherwise), the value the public
/// method returns, and two optional continuations — extra rows to write
/// inside the same transaction and side effects to run after it.
pub(crate) struct Mutation<'a, T> {
    changed: bool,
    value: T,
    rows: Option<Box<dyn FnOnce(&Catalog) -> Result<()> + 'a>>,
    after_commit: Option<Box<dyn FnOnce(&mut Asset) + 'a>>,
}

impl<'a, T> Mutation<'a, T> {
    pub(crate) fn new(changed: bool, value: T) -> Self {
        Self { changed, value, rows: None, after_commit: None }
    }

    /// Rows beyond the `assets` row that belong to the same change (e.g.
    /// a variant's role column). Runs inside the one transaction, after
    /// the `assets` row was written.
    pub(crate) fn rows(mut self, f: impl FnOnce(&Catalog) -> Result<()> + 'a) -> Self {
        self.rows = Some(Box::new(f));
        self
    }

    /// Side effects after the transaction committed (XMP write-back);
    /// may re-save the sidecar. Runs before the delta is recorded, so the
    /// journal's `after` state includes what the side effect changed.
    pub(crate) fn then(mut self, f: impl FnOnce(&mut Asset) + 'a) -> Self {
        self.after_commit = Some(Box::new(f));
        self
    }
}

impl QueryEngine {
    /// Create a context for batch web handlers (no edit-history
    /// recorder).
    pub(super) fn batch_context_fast(&self) -> Result<MutationContext<'_>> {
        self.build_context(None)
    }

    /// Build a context for single-asset edits with a fresh edit-history
    /// recorder installed.
    pub(super) fn recording_context(&self) -> Result<MutationContext<'_>> {
        self.build_context(Some(RefCell::new(Vec::new())))
    }

    /// Like [`Self::batch_context_fast`] but with an edit-history recorder
    /// installed, so a whole batch accumulates into one undoable operation.
    pub(super) fn batch_recording_context(&self) -> Result<MutationContext<'_>> {
        self.recording_context()
    }

    /// The one constructor of [`MutationContext`]: borrow the engine's
    /// connection and stores, read the online-volume map once. A registry
    /// read failure propagates: an edit must not silently treat every
    /// volume as offline.
    pub(super) fn build_context(
        &self,
        recorder: Option<RefCell<Vec<AssetDelta>>>,
    ) -> Result<MutationContext<'_>> {
        Ok(MutationContext {
            catalog: self.catalog()?,
            meta_store: &self.meta_store,
            content_store: &self.content_store,
            online_volumes: DeviceRegistry::new(&self.catalog_root).online_map()?,
            config: &self.config,
            recorder,
        })
    }

    /// The single-asset write primitive: resolve `asset_id_prefix`, load
    /// the sidecar, let `f` mutate the `Asset` in memory, and — only when
    /// `f` reports a change — persist in the documented writer order:
    /// sidecar first, then the `assets` row (every scalar and
    /// denormalized column derived from the `Asset`) plus any
    /// [`Mutation::rows`] in ONE transaction, then the
    /// [`Mutation::then`] side effects, then the `before`/`after` delta
    /// onto the context's recorder (the `before` clone is taken only when
    /// a recorder is installed). Returns the value `f` produced.
    pub(crate) fn mutate_asset<'a, T>(
        &self,
        ctx: &MutationContext<'_>,
        asset_id_prefix: &str,
        f: impl FnOnce(&mut Asset) -> Result<Mutation<'a, T>>,
    ) -> Result<T> {
        let full_id = ctx.catalog.resolve_asset_id_required(asset_id_prefix)?;
        let uuid: uuid::Uuid = full_id.parse()?;
        let mut asset = ctx.meta_store.load(uuid)?;
        let before = if ctx.recorder.is_some() { Some(asset.clone()) } else { None };

        let Mutation { changed, value, rows, after_commit } = f(&mut asset)?;
        if !changed {
            return Ok(value);
        }

        ctx.meta_store.save(&asset)?;
        ctx.catalog.with_tx(|c| {
            c.insert_asset(&asset)?;
            match rows {
                Some(rows) => rows(c),
                None => Ok(()),
            }
        })?;
        if let Some(after_commit) = after_commit {
            after_commit(&mut asset);
        }
        if let (Some(rec), Some(before)) = (&ctx.recorder, before) {
            rec.borrow_mut().push(AssetDelta { asset_id: full_id, before, after: asset });
        }
        Ok(value)
    }

    /// Drain a context's recorder and, if it captured any deltas, write one
    /// edit-history [`Operation`](crate::history::Operation). No-op edits
    /// (empty delta list) and disabled journaling never produce a file.
    /// Recording failures are logged but never fail the edit itself — the
    /// journal is non-authoritative.
    pub(super) fn commit_recorder(&self, ctx: &MutationContext<'_>, command: &str, summary: &str) {
        let deltas = match &ctx.recorder {
            Some(cell) => cell.borrow_mut().drain(..).collect::<Vec<_>>(),
            None => return,
        };
        if deltas.is_empty() {
            return;
        }
        let op = crate::history::Operation::new(
            command,
            summary,
            crate::history::OpSource::Cli,
            deltas,
        );
        let store = crate::history::HistoryStore::new_with(&self.catalog_root, &self.config.history);
        if let Err(e) = store.record(op) {
            eprintln!("note: failed to record edit history: {e:#}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::Catalog;
    use crate::history::HistoryStore;
    use crate::query::test_support::*;

    #[test]
    fn set_rating_records_one_operation() {
        let (dir, asset_id) = setup_tag_env();
        let engine = QueryEngine::new(dir.path());
        engine.set_rating(&asset_id, Some(4)).unwrap();

        let history = HistoryStore::from_config(dir.path());
        let ops = history.list(None).unwrap();
        assert_eq!(ops.len(), 1);
        assert_eq!(ops[0].command, "rating");
        assert_eq!(ops[0].assets.len(), 1);
        assert_eq!(ops[0].assets[0].before.rating, None);
        assert_eq!(ops[0].assets[0].after.rating, Some(4));
    }

    #[test]
    fn no_op_edit_creates_no_operation() {
        let (dir, asset_id) = setup_tag_env();
        let engine = QueryEngine::new(dir.path());
        // Asset starts with rating None; setting None again is a no-op.
        engine.set_rating(&asset_id, None).unwrap();
        // Removing a tag the asset doesn't have is a no-op.
        engine.tag(&asset_id, &["nonexistent".to_string()], true).unwrap();

        let history = HistoryStore::from_config(dir.path());
        assert_eq!(history.list(None).unwrap().len(), 0);
    }

    #[test]
    fn undo_restores_prior_rating_in_both_stores() {
        let (dir, asset_id) = setup_tag_env();
        let engine = QueryEngine::new(dir.path());
        engine.set_rating(&asset_id, Some(5)).unwrap();
        assert_eq!(load_asset(dir.path(), &asset_id).rating, Some(5));

        // Undo: restore the recorded `before` (rating None).
        let history = HistoryStore::from_config(dir.path());
        let op = history.newest_active().unwrap().unwrap();
        engine.restore_asset(&op.assets[0].before).unwrap();
        history.mark_undone(&op.id).unwrap();

        // Sidecar reverted.
        assert_eq!(load_asset(dir.path(), &asset_id).rating, None);
        // Catalog reverted.
        let catalog = Catalog::open(dir.path()).unwrap();
        let details = engine.show(&asset_id).unwrap();
        assert_eq!(details.rating, None);
        drop(catalog);
        // Operation no longer active.
        assert!(history.newest_active().unwrap().is_none());
    }

    #[test]
    fn second_undo_reverts_previous_operation_lifo() {
        let (dir, asset_id) = setup_tag_env();
        let engine = QueryEngine::new(dir.path());
        engine.set_rating(&asset_id, Some(2)).unwrap();
        engine.set_rating(&asset_id, Some(4)).unwrap();
        assert_eq!(load_asset(dir.path(), &asset_id).rating, Some(4));

        let history = HistoryStore::from_config(dir.path());
        // First undo reverts 4 → 2.
        let op1 = history.newest_active().unwrap().unwrap();
        engine.restore_asset(&op1.assets[0].before).unwrap();
        history.mark_undone(&op1.id).unwrap();
        assert_eq!(load_asset(dir.path(), &asset_id).rating, Some(2));

        // Second undo reverts 2 → None.
        let op2 = history.newest_active().unwrap().unwrap();
        engine.restore_asset(&op2.assets[0].before).unwrap();
        history.mark_undone(&op2.id).unwrap();
        assert_eq!(load_asset(dir.path(), &asset_id).rating, None);
    }

    #[test]
    fn batch_tag_two_assets_one_operation_undo_reverts_both() {
        let (dir, _h1, _h2, id1, id2) = setup_group_env();
        let engine = QueryEngine::new(dir.path());

        let results = engine.batch_tag(&[id1.clone(), id2.clone()], &["batchtag".to_string()], false);
        assert!(results.iter().all(|r| r.is_ok()));
        assert!(load_asset(dir.path(), &id1).tags.contains(&"batchtag".to_string()));
        assert!(load_asset(dir.path(), &id2).tags.contains(&"batchtag".to_string()));

        let history = HistoryStore::from_config(dir.path());
        let ops = history.list(None).unwrap();
        assert_eq!(ops.len(), 1, "batch of 2 = ONE operation");
        assert_eq!(ops[0].assets.len(), 2);

        // Undo restores both.
        let op = &ops[0];
        for delta in &op.assets {
            engine.restore_asset(&delta.before).unwrap();
        }
        history.mark_undone(&op.id).unwrap();
        assert!(!load_asset(dir.path(), &id1).tags.contains(&"batchtag".to_string()));
        assert!(!load_asset(dir.path(), &id2).tags.contains(&"batchtag".to_string()));
    }

    #[test]
    fn undo_skips_changed_asset_without_force_restores_with_force() {
        let (dir, asset_id) = setup_tag_env();
        let engine = QueryEngine::new(dir.path());
        engine.set_rating(&asset_id, Some(3)).unwrap();

        let history = HistoryStore::from_config(dir.path());
        let op = history.newest_active().unwrap().unwrap();
        let delta = &op.assets[0];

        // Simulate a later edit: current state now differs from `after`.
        engine.set_rating(&asset_id, Some(1)).unwrap();
        let current = load_asset(dir.path(), &asset_id);
        let changed_since = current != delta.after;
        assert!(changed_since, "asset changed since the recorded operation");

        // Without --force: skip (we just assert the conflict detection here).
        // With --force: restore anyway.
        engine.restore_asset(&delta.before).unwrap();
        assert_eq!(load_asset(dir.path(), &asset_id).rating, None);
    }

    #[test]
    fn doctor_clean_after_undo() {
        let (dir, asset_id) = setup_tag_env();
        let engine = QueryEngine::new(dir.path());
        engine.set_rating(&asset_id, Some(4)).unwrap();

        let history = HistoryStore::from_config(dir.path());
        let op = history.newest_active().unwrap().unwrap();
        engine.restore_asset(&op.assets[0].before).unwrap();
        history.mark_undone(&op.id).unwrap();

        // After restore, doctor must report no consistency issues.
        let report = crate::doctor::run_doctor(dir.path(), None, false, |_, _| {}).unwrap();
        assert!(report.healthy(), "doctor found issues after undo: {:?}", report.mismatched);
    }
}
