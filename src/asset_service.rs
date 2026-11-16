//! Service layer for write-side workflows on the catalog: import, relocate,
//! verify, sync, cleanup, dedup, refresh, fix-commands, export, AI batch ops,
//! video metadata.
//!
//! The 8.9-kLOC monolith split (2026-05) keeps this file as the module root
//! (types, struct, ctor, free helpers, tests) and farms each section out to
//! `asset_service/<section>.rs` as a multi-file `impl AssetService { ... }`
//! block. Public API unchanged.

// ═══════════════════════════════════════════════════════════════════════════════
// asset_service.rs — Core asset lifecycle operations
// ═══════════════════════════════════════════════════════════════════════════════
//
// Table of Contents:
//   1. IMPORTS & CONSTANTS .............. File type groups, use declarations
//   2. FILE TYPE FILTER ................. FileTypeFilter struct & impl
//   3. RESULT TYPES ..................... FileStatus, ImportResult, RelocateResult, etc.
//   4. ASSET SERVICE STRUCT ............. AssetService + scan_orphaned_sharded_files
//   5. IMPORT ........................... import, import_with_callback
//   6. RELOCATE & UPDATE LOCATION ....... relocate, update_location
//   7. VERIFY ........................... verify, verify_location
//   8. SYNC ............................. sync (reconcile catalog with disk)
//   9. CLEANUP & DELETE ................. cleanup, delete_assets
//  10. VOLUME OPERATIONS ................ remove_volume, combine_volume, split_volume
//  11. DEDUP ............................ dedup (same-volume duplicate removal)
//  12. REFRESH & SYNC METADATA .......... refresh, sync_metadata
//  13. FIX COMMANDS ..................... fix_roles, fix_dates, fix_recipes
//  14. EXPORT ........................... build_export_plan, export, export_zip
//  15. AI & FACES ....................... auto_tag, detect_faces (feature-gated)
//  16. VIDEO METADATA ................... backfill_video_metadata (video.rs)
//  16a. DESCRIBE ........................ describe_with + DescribeRequest (describe.rs)
//  16b. GENERATE PREVIEWS ............... generate_previews + PreviewScope (previews.rs)
//  17. FREE FUNCTIONS ................... compute_prefixes, helpers
//  18. TESTS ............................ Unit and integration tests
// ═══════════════════════════════════════════════════════════════════════════════

// ═══ IMPORTS & CONSTANTS ═══

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use uuid::Uuid;

use crate::catalog::Catalog;
use crate::content_store::ContentStore;
use crate::device_registry::DeviceRegistry;
use crate::metadata_store::MetadataStore;
use crate::models::{
    Asset, AssetType, FileLocation, Recipe, RecipeType, Variant, VariantRole, Volume,
};
use crate::models::format::{GROUPS, RECIPE_GROUPS};
pub use crate::models::format::{
    determine_asset_type, determine_recipe_software, is_embedded_xmp_extension,
    is_media_extension, is_raw_extension, normalize_rating, strip_media_extension,
};

// ═══ FILE TYPE FILTER ═══

/// Controls which file types are imported based on enabled/disabled groups.
pub struct FileTypeFilter {
    enabled_groups: HashSet<String>,
}

impl FileTypeFilter {
    /// Create a filter with only the default groups enabled.
    pub fn new() -> Self {
        let enabled = GROUPS
            .iter()
            .filter(|(_, _, default_on)| *default_on)
            .map(|(name, _, _)| name.to_string())
            .collect();
        Self {
            enabled_groups: enabled,
        }
    }

    /// Enable an additional group. Returns an error for unknown group names.
    pub fn include(&mut self, group: &str) -> Result<()> {
        if !GROUPS.iter().any(|(name, _, _)| *name == group) {
            bail!(
                "Unknown file type group '{}'. Valid groups: {}",
                group,
                Self::group_names()
                    .iter()
                    .map(|(n, _)| *n)
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
        self.enabled_groups.insert(group.to_string());
        Ok(())
    }

    /// Disable a group. Returns an error for unknown group names.
    pub fn skip(&mut self, group: &str) -> Result<()> {
        if !GROUPS.iter().any(|(name, _, _)| *name == group) {
            bail!(
                "Unknown file type group '{}'. Valid groups: {}",
                group,
                Self::group_names()
                    .iter()
                    .map(|(n, _)| *n)
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
        self.enabled_groups.remove(group);
        Ok(())
    }

    /// Returns true if the extension belongs to any enabled group.
    pub fn is_importable(&self, ext: &str) -> bool {
        let lower = ext.to_lowercase();
        for (name, extensions, _) in GROUPS {
            if self.enabled_groups.contains(*name) && extensions.contains(&lower.as_str()) {
                return true;
            }
        }
        false
    }

    /// Returns true if the extension is a recipe type AND its group is enabled.
    pub fn is_recipe(&self, ext: &str) -> bool {
        let lower = ext.to_lowercase();
        for (name, extensions, _) in GROUPS {
            if RECIPE_GROUPS.contains(name)
                && extensions.contains(&lower.as_str())
                && self.enabled_groups.contains(*name)
            {
                return true;
            }
        }
        false
    }

    /// List all group names with their default-on status (for help/error messages).
    pub fn group_names() -> Vec<(&'static str, bool)> {
        GROUPS.iter().map(|(name, _, default)| (*name, *default)).collect()
    }
}

impl Default for FileTypeFilter {
    fn default() -> Self {
        Self::new()
    }
}

// ═══ RESULT TYPES ═══

/// Status of a single file during import.
pub enum FileStatus {
    Imported,
    LocationAdded,
    Skipped,
    RecipeAttached,
    RecipeLocationAdded,
    RecipeUpdated,
}

/// Result of an import operation.
#[derive(Debug, serde::Serialize)]
pub struct ImportResult {
    pub dry_run: bool,
    pub imported: usize,
    pub locations_added: usize,
    pub skipped: usize,
    pub recipes_attached: usize,
    pub recipes_location_added: usize,
    pub recipes_updated: usize,
    pub previews_generated: usize,
    pub smart_previews_generated: usize,
    /// `.xmp` sidecars created by `[writeback] create_sidecars` for imported
    /// assets that carry metadata (e.g. `--add-tag`) but have no recipe.
    pub sidecars_created: usize,
    /// Asset IDs created during this import (for post-import auto-grouping).
    #[serde(skip)]
    pub new_asset_ids: Vec<String>,
    /// Volume-relative directory paths of imported files (for neighborhood scoping).
    #[serde(skip)]
    pub imported_directories: Vec<String>,
    /// Volume-relative directories handed to the import (a file argument
    /// contributes its parent). `--auto-group` treats each as the session
    /// boundary for files below it when no `[group] session_root_pattern`
    /// component matches.
    #[serde(skip)]
    pub import_roots: Vec<String>,
}

/// Result of a relocate operation.
#[derive(Debug, serde::Serialize)]
pub struct RelocateResult {
    pub copied: usize,
    pub skipped: usize,
    pub removed: usize,
    pub actions: Vec<String>,
}

/// What kind of file is being relocated.
enum FileCopyKind {
    Variant,
    Recipe { recipe_id: Uuid },
}

/// A planned file copy for relocation.
struct FileCopyPlan {
    content_hash: String,
    source_path: PathBuf,
    target_path: PathBuf,
    kind: FileCopyKind,
    /// The volume_id + relative_path of the source location (for removal)
    source_volume_id: Uuid,
    source_relative_path: PathBuf,
}

/// A group of files sharing the same stem in the same directory.
struct StemGroup {
    _dir: PathBuf,
    stem: String,
    media_files: Vec<PathBuf>,
    recipe_files: Vec<PathBuf>,
}

/// Status of a single file during verification.
pub enum VerifyStatus {
    Ok,
    Mismatch,
    Modified,
    Missing,
    Skipped,
    SkippedRecent,
    Untracked,
}

/// Result of a verify operation.
#[derive(Debug, serde::Serialize)]
pub struct VerifyResult {
    pub verified: usize,
    pub failed: usize,
    pub modified: usize,
    pub skipped: usize,
    pub skipped_recent: usize,
    pub errors: Vec<String>,
}

/// Check if a verified_at timestamp is within `max_age_days` of now.
fn is_recently_verified(verified_at: Option<&str>, max_age_days: u64) -> bool {
    if let Some(ts) = verified_at {
        if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(ts) {
            let age = chrono::Utc::now() - dt.with_timezone(&chrono::Utc);
            return age.num_days() < max_age_days as i64;
        }
    }
    false
}

/// Status of a single file during sync.
pub enum SyncStatus {
    Unchanged,
    Moved,
    New,
    Modified,
    Missing,
}

/// Result of a sync operation.
#[derive(Debug, serde::Serialize)]
pub struct SyncResult {
    pub unchanged: usize,
    pub moved: usize,
    pub new_files: usize,
    pub modified: usize,
    pub missing: usize,
    pub stale_removed: usize,
    pub orphaned_cleaned: usize,
    /// Number of variants that ended up with zero file_locations after sync,
    /// but whose asset still has other variants (so the asset itself wasn't
    /// orphaned and removed). These are the "selected for preview but offline"
    /// variants that confuse subsequent preview/regenerate calls — `maki
    /// cleanup --apply` removes them. The CLI uses this to surface a
    /// next-step hint after `sync --apply --remove-stale`.
    #[serde(default)]
    pub locationless_after: usize,
    pub errors: Vec<String>,
}

/// Status of a single file during cleanup.
pub enum CleanupStatus {
    Ok,
    Stale,
    Offline,
    LocationlessVariant,
    OrphanedAsset,
    OrphanedFile,
}

/// Result of an update-location operation.
#[derive(Debug, serde::Serialize)]
pub struct UpdateLocationResult {
    pub asset_id: String,
    pub file_type: String,
    pub content_hash: String,
    pub old_path: String,
    pub new_path: String,
    pub volume_label: String,
}

/// Result of a cleanup operation.
#[derive(Debug, serde::Serialize)]
pub struct CleanupResult {
    pub checked: usize,
    pub stale: usize,
    pub removed: usize,
    pub skipped_offline: usize,
    pub locationless_variants: usize,
    pub removed_variants: usize,
    pub orphaned_assets: usize,
    pub removed_assets: usize,
    pub orphaned_previews: usize,
    pub removed_previews: usize,
    pub orphaned_smart_previews: usize,
    pub removed_smart_previews: usize,
    pub orphaned_embeddings: usize,
    pub removed_embeddings: usize,
    pub orphaned_face_files: usize,
    pub removed_face_files: usize,
    pub errors: Vec<String>,
    /// `true` if the caller passed `--volume` or `--path`, causing the global
    /// orphan passes (previews, smart previews, embeddings, face files) to be
    /// skipped. The CLI uses this to print a note suggesting a scope-free run
    /// to catch those.
    #[serde(default)]
    pub skipped_global_passes: bool,
}

/// Result of a volume remove operation.
#[derive(Debug, serde::Serialize)]
pub struct VolumeRemoveResult {
    pub volume_label: String,
    pub volume_id: String,
    pub locations: usize,
    pub locations_removed: usize,
    pub recipes: usize,
    pub recipes_removed: usize,
    pub orphaned_assets: usize,
    pub removed_assets: usize,
    pub orphaned_previews: usize,
    pub removed_previews: usize,
    pub apply: bool,
    pub errors: Vec<String>,
}

/// Result of a volume combine operation.
#[derive(Debug, serde::Serialize)]
pub struct VolumeCombineResult {
    pub source_label: String,
    pub source_id: String,
    pub target_label: String,
    pub target_id: String,
    pub path_prefix: String,
    pub locations: usize,
    pub locations_moved: usize,
    pub recipes: usize,
    pub recipes_moved: usize,
    pub assets_affected: usize,
    pub apply: bool,
    pub errors: Vec<String>,
}

/// Result of a volume split operation.
#[derive(Debug, serde::Serialize)]
pub struct VolumeSplitResult {
    pub source_label: String,
    pub source_id: String,
    pub new_label: String,
    pub new_id: String,
    pub path_prefix: String,
    pub locations: usize,
    pub locations_moved: usize,
    pub recipes: usize,
    pub recipes_moved: usize,
    pub assets_affected: usize,
    pub apply: bool,
    pub errors: Vec<String>,
}

/// Result of generating SigLIP embeddings for a batch of assets.
/// Produced by `ai` builds only; the type exists in every build so the
/// import workflow's result and events have one shape.
#[derive(Debug, serde::Serialize)]
pub struct EmbedAssetsResult {
    pub embedded: u32,
    pub skipped: u32,
    pub errors: Vec<String>,
}

/// Per-asset status during embedding (see [`EmbedAssetsResult`]).
pub enum EmbedStatus {
    Embedded,
    Skipped(&'static str),
    Error(String),
}

/// Result of face detection on a batch of assets (`ai` builds only
/// produce one).
#[derive(Debug, serde::Serialize)]
pub struct DetectFacesResult {
    pub assets_processed: u32,
    pub assets_skipped: u32,
    pub faces_detected: u32,
    pub errors: Vec<String>,
}

/// Status of a single recipe during refresh.
pub enum RefreshStatus {
    Unchanged,
    Refreshed,
    Missing,
    Offline,
    /// Embedded-XMP read skipped because the asset has an XMP sidecar
    /// recipe — the sidecar is authoritative and the JPEG/TIFF's embedded
    /// XMP would otherwise re-inject stale tags on every refresh.
    SidecarPresent,
}

/// Result of a refresh operation.
#[derive(Debug, serde::Serialize)]
pub struct RefreshResult {
    pub unchanged: usize,
    pub refreshed: usize,
    pub missing: usize,
    pub skipped: usize,
    pub errors: Vec<String>,
}

/// Result of a sync-metadata operation.
#[derive(Debug, Default, serde::Serialize)]
pub struct SyncMetadataResult {
    /// Recipes where external changes were read in (inbound).
    pub inbound: usize,
    /// Recipes where pending DAM changes were written out (outbound).
    pub outbound: usize,
    /// Recipes unchanged on disk with no pending changes.
    pub unchanged: usize,
    /// Recipes skipped (offline volume or missing file).
    pub skipped: usize,
    /// Recipes with both external changes and pending DAM edits (conflict).
    pub conflicts: usize,
    /// Media files with re-extracted embedded XMP (if --media).
    pub media_refreshed: usize,
    /// Whether this was a dry run.
    pub dry_run: bool,
    /// Error messages.
    pub errors: Vec<String>,
}

/// Status of a single recipe during sync-metadata.
pub enum SyncMetadataStatus {
    Inbound,
    Outbound,
    Unchanged,
    Missing,
    Offline,
    Conflict,
    Error,
}

/// Status of a single asset during fix-roles.
pub enum FixRolesStatus {
    AlreadyCorrect,
    Fixed,
}

/// Result of a fix-roles operation.
#[derive(Debug, serde::Serialize)]
pub struct FixRolesResult {
    pub checked: usize,
    pub fixed: usize,
    pub variants_fixed: usize,
    pub already_correct: usize,
    pub dry_run: bool,
    pub errors: Vec<String>,
}

/// Result of a delete operation.
#[derive(Debug, serde::Serialize)]
pub struct DeleteResult {
    pub deleted: usize,
    pub not_found: Vec<String>,
    pub files_removed: usize,
    /// How many of `files_removed` went to `<catalog_root>/.trash/`
    /// (recoverable via `maki trash restore`) instead of being deleted.
    pub files_trashed: usize,
    pub previews_removed: usize,
    pub dry_run: bool,
    pub errors: Vec<String>,
}

/// Status of a single asset during delete.
pub enum DeleteStatus {
    Deleted,
    NotFound,
    Error(String),
}

/// Status of a single asset during fix-dates.
pub enum FixDatesStatus {
    AlreadyCorrect,
    Fixed,
    NoDate,
    SkippedOffline,
}

/// Result of a fix-dates operation.
#[derive(Debug, Default, serde::Serialize)]
pub struct FixDatesResult {
    pub checked: usize,
    pub fixed: usize,
    pub already_correct: usize,
    pub no_date: usize,
    pub skipped_offline: usize,
    pub dry_run: bool,
    pub offline_volumes: Vec<String>,
    pub errors: Vec<String>,
}

/// Status of a single asset during fix-recipes.
pub enum FixRecipesStatus {
    Reattached,
    NoParentFound,
    Skipped,
}

/// Result of a fix-recipes operation.
#[derive(Debug, Default, serde::Serialize)]
pub struct FixRecipesResult {
    pub checked: usize,
    pub reattached: usize,
    pub no_parent: usize,
    pub skipped: usize,
    pub dry_run: bool,
    pub errors: Vec<String>,
}

/// Status of a single location during dedup.
pub enum DedupStatus {
    Keep,
    Remove,
    Skipped,
}

/// Result of a dedup operation.
#[derive(Debug, serde::Serialize)]
pub struct DedupResult {
    pub duplicates_found: usize,
    pub locations_to_remove: usize,
    pub locations_removed: usize,
    pub files_deleted: usize,
    /// How many removed files (media + recipes) went to
    /// `<catalog_root>/.trash/` instead of being deleted permanently.
    pub files_trashed: usize,
    pub recipes_removed: usize,
    pub bytes_freed: u64,
    pub dry_run: bool,
    pub errors: Vec<String>,
}

/// Layout strategy for exported files.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportLayout {
    /// All files in the target root; collisions resolved by appending hash suffix.
    Flat,
    /// Preserves source volume-relative directory structure; multi-volume gets volume-label prefix.
    Mirror,
}

/// What file content to export for each asset.
///
/// Previews and smart previews live in the catalog directory, so they export
/// fine even when the original's volume is offline — useful for handing out
/// web/social-media-sized copies without mounting archive drives. Exported
/// preview files keep the original filename with the extension swapped to the
/// preview format; missing previews are reported as skipped (run
/// `maki generate-previews` first for full coverage).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportSource {
    /// The original media files from their volumes (default).
    Originals,
    /// Standard previews from `<catalog>/previews/`.
    Previews,
    /// High-resolution smart previews from `<catalog>/smart-previews/`.
    SmartPreviews,
}

/// Status of a single file during export.
pub enum ExportStatus {
    Copied,
    Linked,
    Skipped,
    Error(String),
}

/// Result of an export operation.
#[derive(Debug, serde::Serialize)]
pub struct ExportResult {
    pub dry_run: bool,
    pub assets_matched: usize,
    pub files_exported: usize,
    pub files_skipped: usize,
    pub sidecars_exported: usize,
    pub total_bytes: u64,
    pub errors: Vec<String>,
}

/// Internal plan entry for a single file to export.
pub struct ExportFilePlan {
    pub asset_id: String,
    pub content_hash: String,
    pub source_path: PathBuf,
    pub target_path: PathBuf,
    pub file_size: u64,
    pub is_sidecar: bool,
    /// Volume the entry's *original* lives on — used for the multi-volume
    /// label prefix in mirror layout. For preview sources the actual
    /// source_path is in the catalog directory, so the prefix can't be
    /// derived from it.
    pub source_volume_id: Option<String>,
    /// False for preview sources: `content_hash` is the original variant's
    /// hash there (used for dedup and flat-name collisions), so it must not
    /// be verified against the copied preview file.
    pub verify_hash: bool,
}

/// Replace a filename's extension (preview exports keep the original name
/// but carry the preview format's extension: `IMG_001.NEF` → `IMG_001.jpg`).
fn swap_extension(filename: &str, ext: &str) -> String {
    let stem = Path::new(filename)
        .file_stem()
        .unwrap_or_default()
        .to_string_lossy();
    format!("{stem}.{ext}")
}

/// Resolve a flat-mode target path, handling filename collisions.
///
/// `seen` tracks lowercase filename → content_hash. If the same filename maps to a
/// different hash, a `_<hash[..8]>` suffix is inserted before the extension.
fn resolve_flat_target(
    target_dir: &Path,
    filename: &str,
    content_hash: &str,
    seen: &mut std::collections::HashMap<String, String>,
) -> PathBuf {
    let key = filename.to_lowercase();
    match seen.get(&key) {
        Some(existing_hash) if existing_hash == content_hash => {
            // Same content, reuse the same target path
            target_dir.join(filename)
        }
        Some(_) => {
            // Different content with same filename — add hash suffix
            // content_hash is "sha256:<hex>", extract last 8 hex chars
            let hex_part = content_hash
                .strip_prefix("sha256:")
                .unwrap_or(content_hash);
            let hash_suffix = &hex_part[hex_part.len().saturating_sub(8)..];
            let path = Path::new(filename);
            let stem = path.file_stem().unwrap_or_default().to_string_lossy();
            let new_name = if let Some(ext) = path.extension() {
                format!("{}_{}.{}", stem, hash_suffix, ext.to_string_lossy())
            } else {
                format!("{}_{}", stem, hash_suffix)
            };
            seen.insert(new_name.to_lowercase(), content_hash.to_string());
            target_dir.join(new_name)
        }
        None => {
            seen.insert(key, content_hash.to_string());
            target_dir.join(filename)
        }
    }
}

/// Create a symlink (platform-gated).
#[cfg(unix)]
fn create_symlink(source: &Path, target: &Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(source, target)
}

#[cfg(windows)]
fn create_symlink(source: &Path, target: &Path) -> std::io::Result<()> {
    std::os::windows::fs::symlink_file(source, target)
}

/// Whether extracted XMP carries anything worth merging into an asset
/// (keywords, a description, or rating/label/creator-style metadata).
fn xmp_has_metadata(xmp: &crate::xmp_reader::XmpData) -> bool {
    !xmp.keywords.is_empty() || xmp.description.is_some() || !xmp.source_metadata.is_empty()
}

/// Get file modification time as DateTime<Utc>. Returns None on any error.
fn file_mtime(path: &Path) -> Option<chrono::DateTime<chrono::Utc>> {
    let metadata = std::fs::metadata(path).ok()?;
    let modified = metadata.modified().ok()?;
    Some(chrono::DateTime::<chrono::Utc>::from(modified))
}

// ═══ ASSET SERVICE STRUCT ═══

/// Service-layer entrypoint for write-side workflows on the catalog.
///
/// Holds the bare minimum (catalog root, verbosity, preview config) and
/// instantiates short-lived `Catalog`, `MetadataStore`, etc. handles per
/// operation. Methods are split across `asset_service/<section>.rs` files
/// as multi-file `impl` blocks; see the module-level doc.
pub struct AssetService {
    catalog_root: PathBuf,
    verbosity: crate::Verbosity,
    preview_config: crate::config::PreviewConfig,
    /// The catalog's `maki.toml`, loaded once per service (never per call).
    config: std::sync::Arc<crate::config::CatalogConfig>,
}

/// Walk a sharded directory (`dir/<prefix>/<id>.<ext>`) and find files whose stem
/// is not in the valid set. Counts orphaned/removed and optionally deletes.
fn scan_orphaned_sharded_files(
    dir: &Path,
    is_valid: impl Fn(&str) -> bool,
    apply: bool,
    orphaned: &mut usize,
    removed: &mut usize,
    errors: &mut Vec<String>,
    on_file: &impl Fn(&Path, CleanupStatus, Duration),
) {
    if !dir.is_dir() {
        return;
    }
    let Ok(shard_entries) = std::fs::read_dir(dir) else {
        return;
    };
    for shard_entry in shard_entries.flatten() {
        if !shard_entry.path().is_dir() {
            continue;
        }
        let Ok(file_entries) = std::fs::read_dir(shard_entry.path()) else {
            continue;
        };
        for file_entry in file_entries.flatten() {
            let path = file_entry.path();
            if !path.is_file() {
                continue;
            }
            let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
            if stem.is_empty() {
                continue;
            }
            if !is_valid(stem) {
                *orphaned += 1;
                if apply {
                    let file_start = Instant::now();
                    if let Err(e) = std::fs::remove_file(&path) {
                        errors.push(format!(
                            "Failed to remove orphaned file {}: {e}",
                            path.display()
                        ));
                    } else {
                        *removed += 1;
                        on_file(&path, CleanupStatus::OrphanedFile, file_start.elapsed());
                    }
                }
            }
        }
    }
}

impl AssetService {
    /// Create an `AssetService` rooted at the given catalog directory.
    /// Verbosity controls per-operation logging; the preview config is
    /// cloned so the service owns its own copy. Loads `maki.toml` once
    /// for the remaining sections (defaults when the file is missing or
    /// unreadable). Test fixture: library and entry-point code hold a
    /// loaded config and use [`Self::with_config`].
    #[cfg(test)]
    pub fn new(catalog_root: &Path, verbosity: crate::Verbosity, preview_config: &crate::config::PreviewConfig) -> Self {
        let config = std::sync::Arc::new(
            crate::config::CatalogConfig::load(catalog_root).unwrap_or_default(),
        );
        Self {
            catalog_root: catalog_root.to_path_buf(),
            verbosity,
            preview_config: preview_config.clone(),
            config,
        }
    }

    /// Create an `AssetService` over a configuration the caller already
    /// loaded; the preview section is taken from it.
    pub fn with_config(
        catalog_root: &Path,
        verbosity: crate::Verbosity,
        config: std::sync::Arc<crate::config::CatalogConfig>,
    ) -> Self {
        Self {
            catalog_root: catalog_root.to_path_buf(),
            verbosity,
            preview_config: config.preview.clone(),
            config,
        }
    }

    /// The loaded catalog configuration.
    pub fn config(&self) -> &crate::config::CatalogConfig {
        &self.config
    }

    /// A `QueryEngine` sharing this service's configuration; its connection
    /// opens lazily. Operations that already hold a `Catalog` use
    /// [`Self::query_engine_on`] or [`Self::search_in`] instead.
    fn query_engine(&self) -> crate::query::QueryEngine {
        crate::query::QueryEngine::with_config(&self.catalog_root, self.config.clone())
    }

    /// A `QueryEngine` over a connection this operation already opened:
    /// one connection per operation, handed back by `engine.catalog()`.
    fn query_engine_on(&self, catalog: Catalog) -> crate::query::QueryEngine {
        crate::query::QueryEngine::with_catalog(&self.catalog_root, self.config.clone(), Box::new(catalog))
    }

    /// Run a search on a connection this operation already holds.
    fn search_in(&self, catalog: &Catalog, query: &str) -> Result<Vec<crate::catalog::SearchRow>> {
        crate::query::search_in(catalog, &self.catalog_root, &self.config, None, query)
    }

    /// A `PreviewGenerator` over this service's catalog and preview config.
    fn preview_generator(&self) -> crate::preview::PreviewGenerator {
        crate::preview::PreviewGenerator::new(&self.catalog_root, self.verbosity, &self.preview_config)
    }

}

mod import;
mod relocate;
mod verify;
mod sync;
mod cleanup;
mod volume;
mod dedup;
mod refresh;
mod fix;
mod export;
mod ai;
#[cfg(feature = "ai")]
mod auto_stack;
mod audio;
mod video;
mod describe;
mod previews;
mod watch;
mod workflow;
pub use dedup::DedupOptions;
pub use describe::{DescribeEvent, DescribeRequest, DescribeScope};
pub use previews::{PreviewEvent, PreviewGenerateResult, PreviewOptions, PreviewScope, PreviewStatus};
pub use crate::sidecars::{asset_has_sidecar_metadata, create_missing_sidecars, SidecarCreation, SidecarStatus};
pub use audio::{AudioAnalyzeResult, AudioAnalyzeStatus};
#[cfg(feature = "ai")]
pub use auto_stack::{auto_stack_catalog, AutoStackCluster, AutoStackMember, AutoStackResult};
pub use watch::{is_recipe_path, FileSig, ScanOutcome, WatchScanner};
pub use workflow::{ImportEvent, ImportPhase, ImportRequest, ImportWorkflowResult};


// ═══ FREE FUNCTIONS ═══

/// Compute directory prefixes from scanned paths relative to the volume mount point.
fn compute_prefixes(paths: &[PathBuf], mount_point: &Path) -> Vec<String> {
    let mut prefixes = Vec::new();
    for path in paths {
        let canonical = path.canonicalize().unwrap_or_else(|_| path.clone());
        if let Ok(rel) = canonical.strip_prefix(mount_point) {
            if canonical.is_dir() {
                let prefix = rel.to_string_lossy().to_string();
                if prefix.is_empty() {
                    prefixes.push(String::new());
                } else {
                    prefixes.push(format!("{prefix}/"));
                }
            } else {
                // Single file — use its parent directory as prefix
                if let Some(parent) = rel.parent() {
                    let prefix = parent.to_string_lossy().to_string();
                    if prefix.is_empty() {
                        prefixes.push(String::new());
                    } else {
                        prefixes.push(format!("{prefix}/"));
                    }
                }
            }
        } else {
            // Path not under mount point — use empty prefix (scan everything)
            prefixes.push(String::new());
        }
    }
    if prefixes.is_empty() {
        prefixes.push(String::new());
    }
    prefixes.sort();
    prefixes.dedup();
    prefixes
}

/// Group resolved files by (parent_directory, file_stem).
/// Media files are sorted with RAW extensions first, then alphabetically by extension.
/// Files with extensions not importable by the filter are skipped entirely.
/// Recipe files named with a double extension (`DSC_001.NEF.xmp`) join the
/// group of their media file like a plain `DSC_001.xmp` does.
fn group_by_stem(files: &[PathBuf], filter: &FileTypeFilter) -> Vec<StemGroup> {
    let mut map: BTreeMap<(PathBuf, String), StemGroup> = BTreeMap::new();

    for file in files {
        let dir = file.parent().unwrap_or_else(|| Path::new("")).to_path_buf();
        let raw_stem = file
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("");
        let ext = file
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("");

        if !filter.is_importable(ext) {
            continue;
        }
        let stem = if filter.is_recipe(ext) {
            strip_media_extension(raw_stem).to_string()
        } else {
            raw_stem.to_string()
        };

        let key = (dir.clone(), stem.clone());
        let group = map.entry(key).or_insert_with(|| StemGroup {
            _dir: dir,
            stem,
            media_files: Vec::new(),
            recipe_files: Vec::new(),
        });

        if filter.is_recipe(ext) {
            group.recipe_files.push(file.clone());
        } else {
            group.media_files.push(file.clone());
        }
    }

    // Sort media files: RAW first, then by extension alphabetically
    for group in map.values_mut() {
        group.media_files.sort_by(|a, b| {
            let ext_a = a.extension().and_then(|e| e.to_str()).unwrap_or("");
            let ext_b = b.extension().and_then(|e| e.to_str()).unwrap_or("");
            let a_raw = is_raw_extension(ext_a);
            let b_raw = is_raw_extension(ext_b);
            match (a_raw, b_raw) {
                (true, false) => std::cmp::Ordering::Less,
                (false, true) => std::cmp::Ordering::Greater,
                _ => ext_a.to_lowercase().cmp(&ext_b.to_lowercase()),
            }
        });
        group.recipe_files.sort();
    }

    map.into_values().collect()
}

/// Expand paths: if a path is a directory, recurse into it collecting files.
/// Skips hidden files/directories (starting with '.') and files matching exclude patterns.
pub fn resolve_files(paths: &[PathBuf], exclude_patterns: &[String]) -> Vec<PathBuf> {
    let mut result = Vec::new();
    for path in paths {
        if path.is_dir() {
            collect_files_recursive(path, exclude_patterns, &mut result);
        } else if path.is_file() {
            let name_str = path.file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            if !is_excluded_name(&name_str, exclude_patterns) {
                result.push(path.clone());
            }
        }
    }
    result
}

fn collect_files_recursive(dir: &Path, exclude_patterns: &[String], result: &mut Vec<PathBuf>) {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if name_str.starts_with('.') {
            continue;
        }
        if is_excluded_name(&name_str, exclude_patterns) {
            continue;
        }
        if path.is_dir() {
            collect_files_recursive(&path, exclude_patterns, result);
        } else if path.is_file() {
            result.push(path);
        }
    }
}

/// Check if a filename matches any of the exclude patterns.
fn is_excluded_name(name: &str, patterns: &[String]) -> bool {
    for pattern in patterns {
        if glob_match::glob_match(pattern, name) {
            return true;
        }
    }
    false
}

// ═══ TESTS ═══

#[cfg(test)]
pub(crate) mod test_support;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn determine_asset_type_images() {
        assert_eq!(determine_asset_type("jpg"), AssetType::Image);
        assert_eq!(determine_asset_type("CR2"), AssetType::Image);
        assert_eq!(determine_asset_type("PNG"), AssetType::Image);
        // Newly added RAW formats
        assert_eq!(determine_asset_type("nrw"), AssetType::Image);
        assert_eq!(determine_asset_type("crw"), AssetType::Image);
        assert_eq!(determine_asset_type("3fr"), AssetType::Image);
        assert_eq!(determine_asset_type("iiq"), AssetType::Image);
        assert_eq!(determine_asset_type("x3f"), AssetType::Image);
    }

    #[test]
    fn determine_asset_type_video() {
        assert_eq!(determine_asset_type("mp4"), AssetType::Video);
        assert_eq!(determine_asset_type("MOV"), AssetType::Video);
    }

    #[test]
    fn determine_asset_type_unknown() {
        assert_eq!(determine_asset_type("xyz"), AssetType::Other);
        assert_eq!(determine_asset_type(""), AssetType::Other);
    }

    #[test]
    fn filter_default_includes_standard_types() {
        let f = FileTypeFilter::default();
        // Images
        assert!(f.is_importable("jpg"));
        assert!(f.is_importable("nef"));
        assert!(f.is_importable("PNG"));
        // Newly added RAW formats
        assert!(f.is_importable("nrw"));
        assert!(f.is_importable("crw"));
        assert!(f.is_importable("3fr"));
        assert!(f.is_importable("iiq"));
        assert!(f.is_importable("mrw"));
        assert!(f.is_importable("x3f"));
        // Video
        assert!(f.is_importable("mp4"));
        // Audio
        assert!(f.is_importable("mp3"));
        // XMP
        assert!(f.is_importable("xmp"));
        // Non-default groups should NOT be importable
        assert!(!f.is_importable("cos"));
        assert!(!f.is_importable("pp3"));
        assert!(!f.is_importable("dop"));
        assert!(!f.is_importable("on1"));
        assert!(!f.is_importable("pdf"));
    }

    #[test]
    fn filter_include_adds_group() {
        let mut f = FileTypeFilter::default();
        assert!(!f.is_importable("cos"));
        f.include("captureone").unwrap();
        assert!(f.is_importable("cos"));
        assert!(f.is_importable("cot"));
        assert!(f.is_importable("cop"));
    }

    #[test]
    fn filter_skip_removes_group() {
        let mut f = FileTypeFilter::default();
        assert!(f.is_importable("mp3"));
        f.skip("audio").unwrap();
        assert!(!f.is_importable("mp3"));
        assert!(!f.is_importable("wav"));
        // Other defaults still work
        assert!(f.is_importable("jpg"));
    }

    #[test]
    fn filter_unknown_group_errors() {
        let mut f = FileTypeFilter::default();
        assert!(f.include("bogus").is_err());
        assert!(f.skip("nonexistent").is_err());
    }

    #[test]
    fn filter_is_recipe_respects_enabled() {
        let f = FileTypeFilter::default();
        // xmp is recipe when enabled by default
        assert!(f.is_recipe("xmp"));
        assert!(f.is_recipe("XMP"));
        // cos is NOT recipe when captureone not enabled
        assert!(!f.is_recipe("cos"));
        assert!(!f.is_recipe("pp3"));

        let mut f2 = FileTypeFilter::default();
        f2.include("captureone").unwrap();
        assert!(f2.is_recipe("cos"));
        assert!(f2.is_recipe("cot"));
        // pp3 still not recipe (rawtherapee not enabled)
        assert!(!f2.is_recipe("pp3"));
    }

    #[test]
    fn filter_group_names_lists_all() {
        let names = FileTypeFilter::group_names();
        assert!(names.iter().any(|(n, _)| *n == "images"));
        assert!(names.iter().any(|(n, _)| *n == "captureone"));
        assert!(names.iter().any(|(n, d)| *n == "images" && *d));
        assert!(names.iter().any(|(n, d)| *n == "captureone" && !*d));
    }

    #[test]
    fn is_raw_extension_works() {
        // Original set
        assert!(is_raw_extension("nef"));
        assert!(is_raw_extension("CR2"));
        assert!(is_raw_extension("dng"));
        assert!(is_raw_extension("arw"));
        assert!(is_raw_extension("raf"));
        // Newly added formats
        assert!(is_raw_extension("nrw"));
        assert!(is_raw_extension("NRW"));
        assert!(is_raw_extension("crw"));
        assert!(is_raw_extension("mrw"));
        assert!(is_raw_extension("sr2"));
        assert!(is_raw_extension("srf"));
        assert!(is_raw_extension("3fr"));
        assert!(is_raw_extension("fff"));
        assert!(is_raw_extension("iiq"));
        assert!(is_raw_extension("erf"));
        assert!(is_raw_extension("kdc"));
        assert!(is_raw_extension("dcr"));
        assert!(is_raw_extension("mef"));
        assert!(is_raw_extension("mos"));
        assert!(is_raw_extension("rwl"));
        assert!(is_raw_extension("bay"));
        assert!(is_raw_extension("x3f"));
        // Non-RAW
        assert!(!is_raw_extension("jpg"));
        assert!(!is_raw_extension("xmp"));
        assert!(!is_raw_extension("png"));
        assert!(!is_raw_extension("tiff"));
    }

    #[test]
    fn determine_recipe_software_works() {
        assert_eq!(determine_recipe_software("xmp"), "Adobe/CaptureOne");
        assert_eq!(determine_recipe_software("cos"), "CaptureOne");
        assert_eq!(determine_recipe_software("pp3"), "RawTherapee");
        assert_eq!(determine_recipe_software("dop"), "DxO");
        assert_eq!(determine_recipe_software("on1"), "ON1");
        assert_eq!(determine_recipe_software("txt"), "Unknown");
    }

    #[test]
    fn strip_media_extension_only_strips_known_media_suffixes() {
        assert_eq!(strip_media_extension("DSC_001.NEF"), "DSC_001");
        assert_eq!(strip_media_extension("DSC_001.jpg"), "DSC_001");
        assert_eq!(strip_media_extension("clip.MP4"), "clip");
        assert_eq!(strip_media_extension("DSC_001"), "DSC_001");
        assert_eq!(strip_media_extension("photo.2024"), "photo.2024");
        assert_eq!(strip_media_extension(".nef"), ".nef");
    }

    #[test]
    fn group_by_stem_joins_double_extension_sidecar_with_media() {
        let filter = FileTypeFilter::new();
        let files = vec![
            PathBuf::from("/photos/DSC_002.NEF"),
            PathBuf::from("/photos/DSC_002.NEF.xmp"),
        ];
        let groups = group_by_stem(&files, &filter);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].stem, "DSC_002");
        assert_eq!(groups[0].media_files.len(), 1);
        assert_eq!(groups[0].recipe_files.len(), 1);
    }

    #[test]
    fn group_by_stem_basic() {
        let filter = FileTypeFilter::default();
        let files = vec![
            PathBuf::from("/photos/DSC_001.nef"),
            PathBuf::from("/photos/DSC_001.jpg"),
            PathBuf::from("/photos/DSC_001.xmp"),
            PathBuf::from("/photos/DSC_002.jpg"),
        ];
        let groups = group_by_stem(&files, &filter);
        assert_eq!(groups.len(), 2);

        let g1 = groups.iter().find(|g| g.stem == "DSC_001").unwrap();
        assert_eq!(g1.media_files.len(), 2);
        assert_eq!(g1.recipe_files.len(), 1);
        // RAW should be first
        assert!(g1.media_files[0].to_str().unwrap().ends_with(".nef"));
        assert!(g1.media_files[1].to_str().unwrap().ends_with(".jpg"));

        let g2 = groups.iter().find(|g| g.stem == "DSC_002").unwrap();
        assert_eq!(g2.media_files.len(), 1);
        assert_eq!(g2.recipe_files.len(), 0);
    }

    #[test]
    fn group_by_stem_different_dirs_separate() {
        let filter = FileTypeFilter::default();
        let files = vec![
            PathBuf::from("/a/photo.jpg"),
            PathBuf::from("/b/photo.jpg"),
        ];
        let groups = group_by_stem(&files, &filter);
        assert_eq!(groups.len(), 2);
    }

    #[test]
    fn group_by_stem_skips_non_importable() {
        let filter = FileTypeFilter::default();
        let files = vec![
            PathBuf::from("/photos/DSC_001.nef"),
            PathBuf::from("/photos/DSC_001.cos"), // captureone not enabled
            PathBuf::from("/photos/readme.pdf"),   // documents not enabled
        ];
        let groups = group_by_stem(&files, &filter);
        assert_eq!(groups.len(), 1);
        let g = &groups[0];
        assert_eq!(g.media_files.len(), 1);
        assert_eq!(g.recipe_files.len(), 0);
    }

    #[test]
    fn resolve_files_skips_hidden() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("visible.txt"), "hello").unwrap();
        std::fs::write(dir.path().join(".hidden"), "secret").unwrap();

        let files = resolve_files(&[dir.path().to_path_buf()], &[]);
        assert_eq!(files.len(), 1);
        assert!(files[0].file_name().unwrap().to_str().unwrap() == "visible.txt");
    }

    #[test]
    fn resolve_files_recurses_directories() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("subdir");
        std::fs::create_dir(&sub).unwrap();
        std::fs::write(dir.path().join("a.txt"), "a").unwrap();
        std::fs::write(sub.join("b.txt"), "b").unwrap();

        let files = resolve_files(&[dir.path().to_path_buf()], &[]);
        assert_eq!(files.len(), 2);
    }

    #[test]
    fn resolve_files_excludes_patterns() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("photo.jpg"), "image").unwrap();
        std::fs::write(dir.path().join("Thumbs.db"), "thumbs").unwrap();
        std::fs::write(dir.path().join("cache.tmp"), "temp").unwrap();
        std::fs::write(dir.path().join("notes.txt"), "notes").unwrap();

        let exclude = vec!["Thumbs.db".to_string(), "*.tmp".to_string()];
        let files = resolve_files(&[dir.path().to_path_buf()], &exclude);
        let names: Vec<String> = files
            .iter()
            .map(|f| f.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert!(names.contains(&"photo.jpg".to_string()));
        assert!(names.contains(&"notes.txt".to_string()));
        assert!(!names.contains(&"Thumbs.db".to_string()));
        assert!(!names.contains(&"cache.tmp".to_string()));
    }

    #[test]
    fn resolve_files_exclude_in_subdirs() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("subdir");
        std::fs::create_dir(&sub).unwrap();
        std::fs::write(sub.join("photo.jpg"), "image").unwrap();
        std::fs::write(sub.join("Thumbs.db"), "thumbs").unwrap();

        let exclude = vec!["Thumbs.db".to_string()];
        let files = resolve_files(&[dir.path().to_path_buf()], &exclude);
        assert_eq!(files.len(), 1);
        assert!(files[0].file_name().unwrap().to_str().unwrap() == "photo.jpg");
    }

    #[test]
    fn is_excluded_name_works() {
        assert!(is_excluded_name("Thumbs.db", &["Thumbs.db".to_string()]));
        assert!(is_excluded_name("cache.tmp", &["*.tmp".to_string()]));
        assert!(is_excluded_name(".DS_Store", &[".DS_Store".to_string()]));
        assert!(!is_excluded_name("photo.jpg", &["*.tmp".to_string()]));
        assert!(!is_excluded_name("photo.jpg", &[]));
    }

    #[test]
    fn is_recently_verified_none_returns_false() {
        assert!(!is_recently_verified(None, 30));
    }

    #[test]
    fn is_recently_verified_recent_returns_true() {
        let now = chrono::Utc::now().to_rfc3339();
        assert!(is_recently_verified(Some(&now), 30));
    }

    #[test]
    fn is_recently_verified_old_returns_false() {
        let old = (chrono::Utc::now() - chrono::Duration::days(60)).to_rfc3339();
        assert!(!is_recently_verified(Some(&old), 30));
    }

    #[test]
    fn is_recently_verified_invalid_returns_false() {
        assert!(!is_recently_verified(Some("not-a-date"), 30));
    }

    #[test]
    fn resolve_flat_target_no_collision() {
        let dir = PathBuf::from("/tmp/export");
        let mut seen = std::collections::HashMap::new();
        let result = resolve_flat_target(&dir, "photo.jpg", "sha256:aabb", &mut seen);
        assert_eq!(result, dir.join("photo.jpg"));
    }

    #[test]
    fn resolve_flat_target_same_hash_reuses_name() {
        let dir = PathBuf::from("/tmp/export");
        let mut seen = std::collections::HashMap::new();
        let r1 = resolve_flat_target(&dir, "photo.jpg", "sha256:aabb", &mut seen);
        let r2 = resolve_flat_target(&dir, "photo.jpg", "sha256:aabb", &mut seen);
        assert_eq!(r1, r2);
    }

    #[test]
    fn resolve_flat_target_collision_adds_suffix() {
        let dir = PathBuf::from("/tmp/export");
        let mut seen = std::collections::HashMap::new();
        resolve_flat_target(&dir, "photo.jpg", "sha256:aaaa1111", &mut seen);
        let r2 = resolve_flat_target(&dir, "photo.jpg", "sha256:bbbb2222", &mut seen);
        assert_eq!(r2, dir.join("photo_bbbb2222.jpg"));
    }

    #[test]
    fn resolve_flat_target_no_extension() {
        let dir = PathBuf::from("/tmp/export");
        let mut seen = std::collections::HashMap::new();
        resolve_flat_target(&dir, "README", "sha256:aaaa", &mut seen);
        let r2 = resolve_flat_target(&dir, "README", "sha256:bbbb", &mut seen);
        assert_eq!(r2, dir.join("README_bbbb"));
    }

    #[test]
    fn resolve_flat_target_case_insensitive() {
        let dir = PathBuf::from("/tmp/export");
        let mut seen = std::collections::HashMap::new();
        resolve_flat_target(&dir, "Photo.JPG", "sha256:aaaa", &mut seen);
        let r2 = resolve_flat_target(&dir, "photo.jpg", "sha256:bbbb", &mut seen);
        // Different hash with same name (case-insensitive) should get suffix
        assert!(r2.file_name().unwrap().to_str().unwrap().contains("bbbb"));
    }

    #[test]
    fn normalize_rating_passthrough_1_to_5() {
        assert_eq!(normalize_rating(0), 0);
        assert_eq!(normalize_rating(1), 1);
        assert_eq!(normalize_rating(2), 2);
        assert_eq!(normalize_rating(3), 3);
        assert_eq!(normalize_rating(4), 4);
        assert_eq!(normalize_rating(5), 5);
    }

    #[test]
    fn normalize_rating_microsoft_percentage_scale() {
        // MicrosoftPhoto:Rating values: 1→1★, 25→2★, 50→3★, 75→4★, 99/100→5★
        assert_eq!(normalize_rating(1), 1); // edge: also valid as xmp:Rating
        assert_eq!(normalize_rating(20), 2);
        assert_eq!(normalize_rating(25), 2);
        assert_eq!(normalize_rating(40), 3);
        assert_eq!(normalize_rating(50), 3);
        assert_eq!(normalize_rating(60), 3);
        assert_eq!(normalize_rating(75), 4);
        assert_eq!(normalize_rating(80), 4);
        assert_eq!(normalize_rating(99), 5);
        assert_eq!(normalize_rating(100), 5);
        assert_eq!(normalize_rating(255), 5);
    }

    #[test]
    fn normalize_rating_boundary_values() {
        assert_eq!(normalize_rating(6), 1);   // just above xmp range
        assert_eq!(normalize_rating(12), 1);  // upper boundary of 1★
        assert_eq!(normalize_rating(13), 2);  // lower boundary of 2★
        assert_eq!(normalize_rating(37), 2);  // upper boundary of 2★
        assert_eq!(normalize_rating(38), 3);  // lower boundary of 3★
        assert_eq!(normalize_rating(62), 3);  // upper boundary of 3★
        assert_eq!(normalize_rating(63), 4);  // lower boundary of 4★
        assert_eq!(normalize_rating(87), 4);  // upper boundary of 4★
        assert_eq!(normalize_rating(88), 5);  // lower boundary of 5★
    }
}
