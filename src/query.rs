//! Query engine — search execution and write-path operations on assets.
//!
//! This file holds the request/result types, the [`QueryEngine`] struct
//! with its constructors and connection handling, and the `mod`
//! declarations. The engine's methods live in one `impl QueryEngine`
//! block per concern (the layout `AssetService` uses):
//!
//! | file | concern |
//! |------|---------|
//! | `query/parse.rs` | `ParsedSearch`, `parse_search_query`, `NumericFilter`, date and path parsing (re-exported here) |
//! | `query/resolve.rs` | collection / person / missing-file filter resolution (re-exported here) |
//! | `query/search.rs` | `search`, `search_with_image`, `show`, `resolve_scope` |
//! | `query/grouping.rs` | `group`, `split`, `auto_group`, `stack_from_tag` |
//! | `query/tags.rs` | `tag`, `tag_rename`, `tag_split`, `tag_delete`, `tag_fix_unicode` |
//! | `query/fields.rs` | `edit`, `set_*` field setters, batch entry points |
//! | `query/reimport.rs` | `reimport_metadata`, `restore_asset` |
//! | `query/context.rs` | `BatchContext`, edit-history recording |
//! | `query/writeback.rs` | XMP sidecar write-back |
//! | `query/writeback_embed.rs` | embedded-XMP write-back (`writeback --embed`) |

use std::cell::OnceCell;
use std::ops::Deref;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Result;
use chrono::{DateTime, Utc};

use crate::catalog::Catalog;
use crate::config::CatalogConfig;
use crate::content_store::ContentStore;
use crate::metadata_store::MetadataStore;

mod parse;
pub use parse::*;

mod resolve;
pub use resolve::*;

mod context;
pub use context::{BatchContext, MutationContext};

mod fields;
mod grouping;
pub use grouping::{AutoGroupScope, FromTagEvent};
mod reimport;
mod search;
pub use search::{run_search, search_in, SearchScope};
mod tags;
mod writeback;
pub use writeback::{WritebackEvent, WritebackRequest};
mod writeback_embed;

#[cfg(test)]
pub(crate) mod test_support;

pub(crate) use crate::session_root::find_session_root;

// ═══ RESULT TYPES ═══

/// Result of a group operation.
#[derive(Debug)]
pub struct GroupResult {
    /// The asset ID that all variants were merged into.
    pub target_id: String,
    /// Number of variants moved from donor assets.
    pub variants_moved: usize,
    /// Number of donor assets removed.
    pub donors_removed: usize,
}

/// Result of a split operation.
#[derive(Debug, serde::Serialize)]
pub struct SplitResult {
    /// The source asset ID (that lost variants).
    pub source_id: String,
    /// New assets created from the extracted variants.
    pub new_assets: Vec<NewSplitAsset>,
}

/// Info about one newly created asset from a split.
#[derive(Debug, serde::Serialize)]
pub struct NewSplitAsset {
    pub asset_id: String,
    pub variant_hash: String,
    pub original_filename: String,
}

/// One stem group found by `auto_group`.
#[derive(Debug, serde::Serialize)]
pub struct StemGroupEntry {
    pub stem: String,
    pub target_id: String,
    pub asset_ids: Vec<String>,
    pub donor_count: usize,
}

/// Result of an auto-group operation.
#[derive(Debug, serde::Serialize)]
pub struct AutoGroupResult {
    pub groups: Vec<StemGroupEntry>,
    pub total_donors_merged: usize,
    pub total_variants_moved: usize,
    pub dry_run: bool,
}

/// Result of converting tags into stacks via `stack_from_tag`.
#[derive(Debug, serde::Serialize)]
pub struct FromTagResult {
    pub tags_matched: u32,
    pub tags_skipped: u32,
    pub stacks_created: u32,
    pub assets_stacked: u32,
    pub assets_skipped: u32,
    pub tags_removed: u32,
    pub dry_run: bool,
    pub details: Vec<FromTagDetail>,
    /// Per-asset failures while removing the source tag (`--remove-tags`);
    /// the stacks themselves were still created.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub errors: Vec<String>,
}

/// One matched tag in a `stack_from_tag` operation.
#[derive(Debug, serde::Serialize)]
pub struct FromTagDetail {
    pub tag: String,
    pub assets_found: u32,
    pub assets_stacked: u32,
    pub assets_skipped: u32,
    pub stack_id: Option<String>,
}

/// Fields to edit on an asset. `None` = no change, `Some(None)` = clear, `Some(Some(x))` = set.
pub struct EditFields {
    pub name: Option<Option<String>>,
    pub description: Option<Option<String>>,
    pub rating: Option<Option<u8>>,
    pub color_label: Option<Option<String>>,
    /// `None` = no change, `Some(Some(dt))` = set to dt, `Some(None)` = reset to now.
    pub created_at: Option<Option<DateTime<Utc>>>,
}

/// Result of an edit operation.
#[derive(Debug, serde::Serialize)]
pub struct EditResult {
    pub asset_id: String,
    pub name: Option<String>,
    pub description: Option<String>,
    pub rating: Option<u8>,
    pub color_label: Option<String>,
    pub created_at: String,
}

/// Result of a tag add/remove operation.
pub struct TagResult {
    /// Tags that were actually added or removed.
    pub changed: Vec<String>,
    /// The full set of tags after the operation.
    pub current_tags: Vec<String>,
    /// Provenance map for `current_tags` (absent = user) — lets the web
    /// tags fragment render machine-tag badges without a second load.
    pub tag_sources: std::collections::BTreeMap<String, crate::models::TagSource>,
}

/// Action taken for a single asset during tag rename.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TagRenameAction {
    /// Old tag replaced with new tag
    Renamed,
    /// Asset already had target tag; old tag removed (merge)
    Removed,
    /// Asset already has the exact target tag; no change needed
    Skipped,
}

/// Result of a `maki tag-rename` operation.
#[derive(Debug, Default, serde::Serialize)]
pub struct TagRenameResult {
    pub dry_run: bool,
    pub matched: usize,
    pub renamed: usize,
    pub removed: usize,
    pub skipped: usize,
    /// Per-asset failures (e.g. "no such file or directory" if the YAML
    /// sidecar got removed out of band). The bulk operation continues
    /// past them rather than aborting; entries here are surfaced to the
    /// caller for follow-up.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub errors: Vec<String>,
}

/// Action taken for a single asset during tag split.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TagSplitAction {
    /// Old tag replaced (or augmented in --keep mode) with the new tags.
    Split,
    /// No change needed (asset already has all targets, and the source was
    /// either absent or `--keep` was set). Always a no-op.
    Skipped,
}

/// Result of a `maki tag split` operation.
#[derive(Debug, Default, serde::Serialize)]
pub struct TagSplitResult {
    pub dry_run: bool,
    pub matched: usize,
    pub split: usize,
    pub skipped: usize,
    /// Per-asset failures (e.g. missing YAML sidecar); see TagRenameResult::errors.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub errors: Vec<String>,
}

/// Action taken for a single asset during tag delete.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TagDeleteAction {
    /// Tag (and any cascaded descendants + newly-orphaned ancestors) removed.
    Removed,
    /// Asset matched but had no removable tag values (e.g. leaf-only mode and
    /// the tag had descendants on this asset).
    Skipped,
}

/// Result of a `maki tag delete` operation.
#[derive(Debug, Default, serde::Serialize)]
pub struct TagDeleteResult {
    pub dry_run: bool,
    pub matched: usize,
    pub removed: usize,
    pub skipped: usize,
    /// Per-asset failures (e.g. missing YAML sidecar); see TagRenameResult::errors.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub errors: Vec<String>,
}

/// Result of a `maki tag fix-unicode` operation.
#[derive(Debug, Default, serde::Serialize)]
pub struct TagFixUnicodeResult {
    pub dry_run: bool,
    /// Total assets visited.
    pub scanned: usize,
    /// Assets where at least one tag value was rewritten to NFC, or where
    /// dedup collapsed two encoding-variants of the same tag into one.
    pub fixed: usize,
    /// Total tag values that changed form (NFC-normalised). Useful as a
    /// scale signal — a single asset can contribute many.
    pub tags_normalized: usize,
    /// Assets that had two encoding-variants of the same logical tag and
    /// where one of them was dropped during dedup.
    pub merged: usize,
    /// Per-asset failures (e.g. missing YAML sidecar); see TagRenameResult::errors.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub errors: Vec<String>,
}

/// Result of a `maki writeback` operation.
#[derive(Debug, Default, serde::Serialize)]
pub struct WritebackResult {
    /// Number of XMP files written (or that would be written in dry-run).
    pub written: u32,
    /// Number of recipes whose XMP on disk already matched the catalog
    /// (no file write needed). Surfaces the "external sync got here
    /// first" case — typically after an `rsync` from a primary volume
    /// to a backup volume that's a registered MAKI volume. The
    /// catalog's stored content_hash is silently reconciled if the
    /// disk content was externally modified to match what we'd have
    /// written, so a subsequent `verify` won't flag stale hashes.
    pub already_in_sync: u32,
    /// Number of recipes skipped (volume offline or file missing).
    pub skipped: u32,
    /// Number of recipes that failed.
    pub failed: u32,
    /// Error messages.
    pub errors: Vec<String>,
    /// Whether this was a dry run.
    pub dry_run: bool,
    /// Sorted, deduped labels of offline volumes that held at least one
    /// skipped recipe. Surfaced in the CLI summary as
    /// "(offline volumes: foo, bar)" so the user knows which drives to
    /// reconnect before the next flush. Empty when nothing was skipped
    /// due to an offline volume.
    #[serde(skip_serializing_if = "std::collections::BTreeSet::is_empty")]
    pub skipped_offline_volumes: std::collections::BTreeSet<String>,
}

/// Result of a `maki writeback --embed` operation (embedded XMP written
/// into JPEG variant files). Separate from [`WritebackResult`] so the
/// sidecar path's `--json` shape stays stable. All counters are
/// per-FILE (a variant with copies on two online volumes counts twice).
#[derive(Debug, Default, serde::Serialize)]
pub struct EmbedWritebackResult {
    /// JPEG files rewritten (or that would be in dry-run).
    pub written: u32,
    /// Files whose embedded XMP already matched the catalog (no write).
    pub already_in_sync: u32,
    /// Files skipped: volume offline, file missing, or TIFF variant
    /// (embedded write-back doesn't support TIFF yet).
    pub skipped: u32,
    /// Files that failed (oversize packet, I/O error, post-write
    /// verification failure — original left untouched in every case).
    pub failed: u32,
    /// Error messages, one per failed file.
    pub errors: Vec<String>,
    /// Whether this was a dry run.
    pub dry_run: bool,
    /// Originals copied into the catalog trash before rewriting
    /// (`maki trash list` / `maki trash restore` to recover). 0 when
    /// `--no-trash` or `[trash] enabled = false`.
    pub trashed_originals: u32,
    /// Sorted, deduped labels of offline volumes that held at least one
    /// skipped JPEG location (mirror of
    /// [`WritebackResult::skipped_offline_volumes`]).
    #[serde(skip_serializing_if = "std::collections::BTreeSet::is_empty")]
    pub skipped_offline_volumes: std::collections::BTreeSet<String>,
}

/// Query-image input for [`QueryEngine::search_with_image`] — a local
/// file that is not in the catalog.
#[derive(Debug, Clone)]
pub struct ImageSearch<'a> {
    pub path: &'a Path,
    /// Result-set size (counts the exact match); defaults to 40.
    pub limit: Option<usize>,
}

/// What a query-image search learned about the query file, alongside
/// the rows (which carry per-row `similarity` / `exact_match`).
#[derive(Debug, Clone, serde::Serialize)]
pub struct ImageSearchInfo {
    pub content_hash: String,
    /// Asset owning a byte-identical variant, if any.
    pub exact_asset_id: Option<String>,
    /// Set when the model could not encode the query but an exact match
    /// answered the question anyway (e.g. model not downloaded).
    pub warning: Option<String>,
}

/// Query engine — search execution and write-path operations on assets.
///
/// Holds the catalog root, the loaded configuration, an optional default
/// filter (e.g. `[browse] default_filter`) that is appended to every
/// search, and **one** SQLite connection: lent by the caller
/// ([`Self::with_catalog`]) or opened lazily on first use and kept for
/// the engine's lifetime ([`Self::catalog`]). Methods cover search/show,
/// group/split, tag operations, edit, XMP writeback, and stack/batch
/// helpers; see the module-level doc for the file layout.
///
/// The engine is `Send` but not `Sync` (the connection cell): share it
/// between threads by handing each its own engine, never by `&` across
/// threads.
pub struct QueryEngine {
    catalog_root: PathBuf,
    default_filter: Option<String>,
    /// The catalog's `maki.toml`, loaded once per engine (never per call).
    config: Arc<CatalogConfig>,
    /// The engine's connection — see [`Self::catalog`].
    catalog: OnceCell<LentCatalog>,
    /// Sidecar store rooted at `catalog_root` (a path; cheap to hold).
    meta_store: MetadataStore,
    content_store: ContentStore,
}

/// A connection the engine can borrow: either one it opened itself
/// (`OwnedCatalog`) or one a caller lent (the web pool's
/// `PooledCatalog`, a service's open `Catalog` behind a guard).
type LentCatalog = Box<dyn Deref<Target = Catalog> + Send>;

/// Adapter for the engine-opened case (`Catalog` itself is not `Deref`).
struct OwnedCatalog(Catalog);

impl Deref for OwnedCatalog {
    type Target = Catalog;
    fn deref(&self) -> &Catalog {
        &self.0
    }
}

impl QueryEngine {
    /// Test fixture: an engine over the catalog's `maki.toml` (defaults
    /// when the file is missing). Library and entry-point code hold a
    /// loaded config and use [`Self::with_config`] / [`Self::with_catalog`].
    #[cfg(test)]
    pub fn new(catalog_root: &Path) -> Self {
        let config = Arc::new(CatalogConfig::load(catalog_root).unwrap_or_default());
        Self::with_config(catalog_root, config)
    }

    /// Create a `QueryEngine` over a configuration the caller already
    /// loaded (CLI entry point, web `AppState`, services). The catalog
    /// connection is opened lazily on first use.
    pub fn with_config(catalog_root: &Path, config: Arc<CatalogConfig>) -> Self {
        Self {
            catalog_root: catalog_root.to_path_buf(),
            default_filter: None,
            config,
            catalog: OnceCell::new(),
            meta_store: MetadataStore::new(catalog_root),
            content_store: ContentStore::new(catalog_root),
        }
    }

    /// Create a `QueryEngine` over a connection the caller already holds,
    /// so no second connection is opened for the engine's lifetime.
    ///
    /// Who should use it: the web server (`AppState::query_engine()`
    /// hands in a `PooledCatalog` — it derefs to `Catalog` and returns to
    /// the pool when the engine is dropped) and `AssetService` operations
    /// that already have a `Catalog` open (wrap it in a type that derefs
    /// to it, e.g. a `Box<Catalog>`-like guard). CLI commands keep using
    /// [`Self::new`] / [`Self::with_config`]; the lazy open costs one
    /// connection per command.
    pub fn with_catalog(
        catalog_root: &Path,
        config: Arc<CatalogConfig>,
        catalog: impl Deref<Target = Catalog> + Send + 'static,
    ) -> Self {
        let lent: LentCatalog = Box::new(catalog);
        Self {
            catalog: OnceCell::from(lent),
            ..Self::with_config(catalog_root, config)
        }
    }

    /// Builder: AND `[browse] default_filter` (or any filter) into every
    /// search of this engine.
    pub fn default_filter(mut self, default_filter: Option<String>) -> Self {
        self.default_filter = default_filter;
        self
    }

    /// Replace the default filter (AND-ed into every search). Lets a
    /// caller that built the engine with [`Self::with_catalog`] apply
    /// `[browse] default_filter` too.
    pub fn set_default_filter(&mut self, default_filter: Option<String>) {
        self.default_filter = default_filter;
    }

    /// The loaded catalog configuration.
    pub fn config(&self) -> &CatalogConfig {
        &self.config
    }

    /// The catalog directory this engine operates on.
    pub fn catalog_root(&self) -> &Path {
        &self.catalog_root
    }

    /// The engine's catalog connection: the lent one, or — on first use —
    /// one opened here and cached for the engine's lifetime. This is the
    /// only `Catalog::open` in the `query` module tree.
    pub fn catalog(&self) -> Result<&Catalog> {
        if let Some(lent) = self.catalog.get() {
            return Ok(lent);
        }
        let opened: LentCatalog = Box::new(OwnedCatalog(self.open_catalog()?));
        // `OnceCell` is `!Sync`, so nothing can have filled the cell
        // between the `get` above and this `get_or_init`.
        Ok(self.catalog.get_or_init(|| opened))
    }

    fn open_catalog(&self) -> Result<Catalog> {
        Catalog::open(&self.catalog_root)
    }
}
