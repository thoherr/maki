//! SQLite catalog — derived cache of asset/variant/recipe/location state.
//!
//! The 9.2-kLOC monolith split (2026-05) keeps this file as the module root
//! (types, struct, ctor, helpers, tests) and farms each section out to
//! `catalog/<section>.rs` as a multi-file `impl Catalog { ... }` block:
//! schema migrations, asset/variant/recipe CRUD, lookups, search builder,
//! duplicates, rebuild, stats, facets, tags, analytics, backup, cleanup.
//!
//! The catalog is rebuilt from YAML sidecars via `maki rebuild-catalog`;
//! everything in here is derived state.

// ═══════════════════════════════════════════════════════════════════════════════
// catalog.rs — SQLite catalog (derived cache of sidecar data)
// ═══════════════════════════════════════════════════════════════════════════════
//
// Table of Contents:
//   1. IMPORTS .......................... use declarations
//   2. TYPES & STRUCTS .................. MapMarker, FacetCounts, SearchRow, AssetDetails, etc.
//   3. SEARCH OPTIONS ................... SearchSort, SearchOptions, SearchPage
//   4. HELPER FUNCTIONS ................. path_pattern_to_like, next_date_bound
//   5. CATALOG STRUCT & CONNECTION ...... Catalog, open, open_fast, open_and_migrate
//   6. SCHEMA MIGRATIONS ................ run_migrations, initialize
//   7. ASSET CRUD ....................... insert_asset, update_asset_*, delete_asset
//   8. VARIANT & LOCATION CRUD .......... insert_variant, insert_file_location, etc.
//   9. RECIPE CRUD ...................... insert_recipe, update_recipe_*, writeback
//  10. VOLUME OPERATIONS ................ ensure_volume, delete_volume, bulk_move_*
//  11. ASSET LOOKUPS .................... search_assets, resolve_asset_id(_required), load_asset_details
//  12. DUPLICATES ....................... find_duplicates, find_duplicates_filtered
//  13. LOCATION & RECIPE QUERIES ........ find_variant_by_volume, list_recipes_*, etc.
//  14. REBUILD .......................... rebuild (drop + recreate)
//  15. STATS ............................ stats_overview, stats_per_volume, build_stats
//  16. SEARCH BUILDER ................... rating_clause, numeric_clause, build_search_where
//  17. SEARCH EXECUTION ................. search_paginated, search_count, get_search_row
//  18. CALENDAR & FACETS ................ calendar_counts, facet_counts, map_markers
//  19. TAG & FORMAT QUERIES ............. list_all_tags, assets_with_tag, list_all_formats
//  20. ANALYTICS ........................ build_analytics
//  21. BACKUP STATUS .................... backup_status_overview, at_risk, missing_from_volume
//  22. CLEANUP QUERIES .................. count_file_locations, list_orphaned_asset_ids, etc.
//  23. TESTS ............................ Unit tests
// ═══════════════════════════════════════════════════════════════════════════════

// ═══ IMPORTS ═══

use std::collections::HashMap;
use std::path::Path;

use anyhow::Result;
use rusqlite::Connection;

use crate::models::NumericFilter;

use crate::models::{Asset, FileLocation, Recipe, Variant};

// ═══ TYPES & STRUCTS ═══

/// Map marker data for the web UI map view.
#[derive(Debug, serde::Serialize)]
pub struct MapMarker {
    pub id: String,
    pub lat: f64,
    pub lng: f64,
    pub preview: Option<String>,
    pub name: String,
    pub rating: Option<u8>,
    pub label: Option<String>,
}

/// Facet counts for the browse sidebar.
#[derive(Debug, serde::Serialize)]
pub struct FacetCounts {
    pub total: u64,
    pub ratings: Vec<(Option<u8>, u64)>,
    pub labels: Vec<(Option<String>, u64)>,
    pub formats: Vec<(String, u64)>,
    pub volumes: Vec<(String, String, u64)>,
    pub tags: Vec<(String, u64)>,
    pub years: Vec<(String, u64)>,
    pub geotagged: u64,
}

/// A row returned from a search query.
#[derive(Debug, serde::Serialize)]
pub struct SearchRow {
    pub asset_id: String,
    pub name: Option<String>,
    pub asset_type: String,
    pub created_at: String,
    pub original_filename: String,
    pub format: String,
    pub tags: Vec<String>,
    pub description: Option<String>,
    pub content_hash: String,
    pub rating: Option<u8>,
    pub color_label: Option<String>,
    /// The "identity" format of the asset (Original RAW > Original any > best variant).
    pub primary_format: Option<String>,
    /// Number of variants belonging to this asset.
    pub variant_count: u32,
    /// Stack ID if this asset is in a stack.
    pub stack_id: Option<String>,
    /// Number of members in this asset's stack (for badge rendering).
    pub stack_count: Option<u32>,
    /// Manual preview rotation override in degrees (0/90/180/270).
    pub preview_rotation: Option<u16>,
    /// Number of detected faces in this asset.
    pub face_count: u32,
    /// Media duration in seconds (audio and video; None otherwise).
    pub duration_seconds: Option<f64>,
    /// Cosine similarity (0.0–1.0) to the query of a `similar:` /
    /// `--image` search; `None` for every other search. Filled in by the
    /// query engine after the SQL stage.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub similarity: Option<f32>,
    /// `true` when this asset owns a variant whose content hash equals
    /// the `--image` query file — the "found the original" case.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub exact_match: bool,
}

impl SearchRow {
    /// The format to display in UI/CLI — primary_format if available, else best variant format.
    pub fn display_format(&self) -> &str {
        self.primary_format.as_deref().unwrap_or(&self.format)
    }
}

/// Full asset details returned by `load_asset_details`.
#[derive(Debug, serde::Serialize)]
pub struct AssetDetails {
    pub id: String,
    pub name: Option<String>,
    pub asset_type: String,
    pub created_at: String,
    pub tags: Vec<String>,
    /// Tag provenance (tag value → source); tags absent from the map are
    /// user tags. Empty for pre-provenance assets.
    #[serde(skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub tag_sources: std::collections::BTreeMap<String, crate::models::TagSource>,
    pub description: Option<String>,
    pub rating: Option<u8>,
    pub color_label: Option<String>,
    pub variants: Vec<VariantDetails>,
    pub recipes: Vec<RecipeDetails>,
    /// The asset's stored `best_variant_hash` (user override already
    /// applied at write time). Every surface that shows "the" preview of
    /// an asset reads this through [`AssetDetails::best_variant_index`].
    pub best_variant_hash: Option<String>,
}

impl AssetDetails {
    /// Index of the variant that represents this asset for previews: the
    /// stored `best_variant_hash` (which honours the user's preview
    /// override), falling back to the scoring rule only when the stored
    /// hash is absent or no longer belongs to the asset. Using this on
    /// every surface (grid, detail, stroll, similarity, compare, contact
    /// sheets, export, shell) keeps them from disagreeing with each other.
    pub fn best_variant_index(&self) -> Option<usize> {
        self.best_variant_hash
            .as_ref()
            .and_then(|h| self.variants.iter().position(|v| &v.content_hash == h))
            .or_else(|| crate::models::variant::best_preview_index_details(&self.variants))
    }
}

pub use crate::models::variant::{LocationDetails, VariantDetails};

/// A variant that exists in multiple file locations.
#[derive(Debug, serde::Serialize)]
pub struct DuplicateEntry {
    pub content_hash: String,
    pub original_filename: String,
    pub format: String,
    pub file_size: u64,
    pub asset_name: Option<String>,
    pub asset_id: String,
    pub locations: Vec<LocationDetails>,
    /// Number of distinct volumes this variant exists on.
    pub volume_count: usize,
    /// Volume labels that have 2+ locations for this variant (same-volume dupes).
    pub same_volume_groups: Vec<String>,
    /// Pre-computed preview URL for the web UI (not serialized).
    #[serde(skip)]
    pub preview_url: String,
}

/// Recipe details within an `AssetDetails`.
#[derive(Debug, serde::Serialize)]
pub struct RecipeDetails {
    pub variant_hash: String,
    pub software: String,
    pub recipe_type: String,
    pub content_hash: String,
    pub volume_id: Option<String>,
    pub volume_label: Option<String>,
    pub relative_path: Option<String>,
    pub pending_writeback: bool,
}

/// Top-level container for catalog statistics.
#[derive(Debug, serde::Serialize)]
pub struct CatalogStats {
    pub overview: OverviewStats,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub types: Option<TypeStats>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub volumes: Option<Vec<VolumeStats>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tags: Option<TagStats>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verified: Option<VerificationStats>,
}

/// Overview counts for the entire catalog.
#[derive(Debug, serde::Serialize)]
pub struct OverviewStats {
    pub assets: u64,
    pub variants: u64,
    pub recipes: u64,
    /// Number of unique recipe content hashes (distinct XMP files).
    /// The difference `recipes - unique_recipes` is the number of
    /// duplicate recipe locations (e.g. from backup volumes).
    pub file_locations: u64,
    pub unique_recipes: u64,
    pub volumes_total: u64,
    pub volumes_online: u64,
    pub volumes_offline: u64,
    pub total_size: u64,
}

/// Breakdown by asset type and file formats.
#[derive(Debug, serde::Serialize)]
pub struct TypeStats {
    pub asset_types: Vec<TypeCount>,
    pub variant_formats: Vec<FormatCount>,
    pub recipe_formats: Vec<FormatCount>,
}

/// A single asset-type count entry.
#[derive(Debug, serde::Serialize)]
pub struct TypeCount {
    pub asset_type: String,
    pub count: u64,
    pub percentage: f64,
}

/// A single format count entry.
#[derive(Debug, serde::Serialize)]
pub struct FormatCount {
    pub format: String,
    pub count: u64,
}

/// Per-volume statistics.
#[derive(Debug, serde::Serialize)]
pub struct VolumeStats {
    pub label: String,
    pub volume_id: String,
    pub is_online: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub purpose: Option<String>,
    pub assets: u64,
    pub variants: u64,
    pub recipes: u64,
    pub formats: Vec<String>,
    pub directories: u64,
    pub size: u64,
    pub verified_count: u64,
    pub total_locations: u64,
    pub verification_pct: f64,
    pub oldest_verified_at: Option<String>,
}

/// Internal helper for per-volume data before merging with device registry.
struct VolumeStatsRaw {
    volume_id: String,
    label: String,
    assets: u64,
    variants: u64,
    recipes: u64,
    formats: Vec<String>,
    directories: u64,
    size: u64,
    verified_count: u64,
    total_locations: u64,
    oldest_verified_at: Option<String>,
}

/// Tag usage statistics.
#[derive(Debug, serde::Serialize)]
pub struct TagStats {
    pub unique_tags: u64,
    pub tagged_assets: u64,
    pub untagged_assets: u64,
    pub top_tags: Vec<TagCount>,
}

/// A single tag frequency entry.
#[derive(Debug, serde::Serialize)]
pub struct TagCount {
    pub tag: String,
    pub count: u64,
}

/// Verification health statistics.
#[derive(Debug, serde::Serialize)]
pub struct VerificationStats {
    pub total_locations: u64,
    pub verified_locations: u64,
    pub unverified_locations: u64,
    pub coverage_pct: f64,
    pub oldest_verified_at: Option<String>,
    pub newest_verified_at: Option<String>,
    pub per_volume: Vec<VolumeVerificationStats>,
}

/// Per-volume verification summary.
#[derive(Debug, serde::Serialize)]
pub struct VolumeVerificationStats {
    pub label: String,
    pub volume_id: String,
    pub is_online: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub purpose: Option<String>,
    pub locations: u64,
    pub verified: u64,
    pub coverage_pct: f64,
    pub oldest_verified_at: Option<String>,
}

/// Analytics data for the /analytics dashboard page.
#[derive(Debug, serde::Serialize)]
pub struct AnalyticsData {
    pub camera_usage: Vec<NameCount>,
    pub lens_usage: Vec<NameCount>,
    pub rating_distribution: Vec<RatingCount>,
    pub format_distribution: Vec<NameCount>,
    pub monthly_imports: Vec<MonthCount>,
    pub storage_by_volume: Vec<VolumeSize>,
    pub yearly_counts: Vec<YearCount>,
}

/// A name + count pair for analytics charts.
#[derive(Debug, serde::Serialize)]
pub struct NameCount {
    pub name: String,
    pub count: u64,
}

/// Rating distribution entry.
#[derive(Debug, serde::Serialize)]
pub struct RatingCount {
    pub rating: u8,
    pub count: u64,
}

/// Monthly import volume entry.
#[derive(Debug, serde::Serialize)]
pub struct MonthCount {
    pub month: String,
    pub count: u64,
}

/// Per-volume storage size entry.
#[derive(Debug, serde::Serialize)]
pub struct VolumeSize {
    pub label: String,
    pub size: u64,
}

/// Per-year asset count entry.
#[derive(Debug, serde::Serialize)]
pub struct YearCount {
    pub year: String,
    pub count: u64,
}

/// Top-level result for `maki backup-status`.
#[derive(Debug, serde::Serialize)]
pub struct BackupStatusResult {
    pub scope: String,
    pub total_assets: u64,
    pub total_variants: u64,
    pub total_file_locations: u64,
    pub min_copies: u64,
    pub at_risk_count: u64,
    pub purpose_coverage: Vec<PurposeCoverage>,
    pub location_distribution: Vec<LocationBucket>,
    pub volume_gaps: Vec<VolumeGap>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub volume_detail: Option<VolumeGapDetail>,
}

/// Coverage of assets by volume purpose.
#[derive(Debug, serde::Serialize)]
pub struct PurposeCoverage {
    pub purpose: String,
    pub volume_count: u64,
    pub asset_count: u64,
    pub asset_percentage: f64,
}

/// A bucket in the volume-count distribution histogram.
#[derive(Debug, serde::Serialize)]
pub struct LocationBucket {
    pub volume_count: String,
    pub asset_count: u64,
}

/// Summary of how many scoped assets are missing from a volume.
#[derive(Debug, serde::Serialize)]
pub struct VolumeGap {
    pub volume_label: String,
    pub volume_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub purpose: Option<String>,
    pub missing_count: u64,
}

/// Detailed coverage info for a specific target volume.
#[derive(Debug, serde::Serialize)]
pub struct VolumeGapDetail {
    pub volume_label: String,
    pub volume_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub purpose: Option<String>,
    pub present_count: u64,
    pub missing_count: u64,
    pub total_scoped: u64,
    pub coverage_pct: f64,
}

// ═══ SEARCH OPTIONS ═══

/// Sort order for paginated search.
#[derive(Debug, Clone, Copy)]
pub enum SearchSort {
    DateDesc,
    DateAsc,
    NameAsc,
    NameDesc,
    SizeDesc,
    SizeAsc,
    /// Sort by similarity score (client-side — scores are not in SQL).
    SimilarityDesc,
    SimilarityAsc,
}

impl SearchSort {
    fn to_sql(self) -> &'static str {
        match self {
            SearchSort::DateDesc => "a.created_at DESC",
            SearchSort::DateAsc => "a.created_at ASC",
            SearchSort::NameAsc => "COALESCE(a.name, bv.original_filename) ASC",
            SearchSort::NameDesc => "COALESCE(a.name, bv.original_filename) DESC",
            SearchSort::SizeDesc => "bv.file_size DESC",
            SearchSort::SizeAsc => "bv.file_size ASC",
            // Dummy SQL order — real sorting done client-side with similarity scores
            SearchSort::SimilarityDesc | SearchSort::SimilarityAsc => "a.id",
        }
    }

    /// Parse a sort key from its URL/query-param string form (`date_desc`,
    /// `name_asc`, etc.). Unknown values fall back to `DateDesc`. (Not
    /// `FromStr`: this never fails.)
    pub fn parse_key(s: &str) -> Self {
        match s {
            "date_asc" => SearchSort::DateAsc,
            "name_asc" => SearchSort::NameAsc,
            "name_desc" => SearchSort::NameDesc,
            "size_desc" => SearchSort::SizeDesc,
            "size_asc" => SearchSort::SizeAsc,
            "similarity_desc" => SearchSort::SimilarityDesc,
            "similarity_asc" => SearchSort::SimilarityAsc,
            _ => SearchSort::DateDesc,
        }
    }
}

/// Options for paginated search.
pub struct SearchOptions<'a> {
    pub asset_ids: &'a [String],
    pub text: Option<&'a str>,
    pub text_exclude: &'a [String],
    pub asset_types: &'a [String],
    pub asset_types_exclude: &'a [String],
    pub tags: &'a [String],
    pub tags_exclude: &'a [String],
    pub formats: &'a [String],
    pub formats_exclude: &'a [String],
    pub color_labels: &'a [String],
    pub color_labels_exclude: &'a [String],
    pub color_label_none: bool,
    pub color_label_any: bool,
    pub cameras: &'a [String],
    pub cameras_exclude: &'a [String],
    pub lenses: &'a [String],
    pub lenses_exclude: &'a [String],
    pub descriptions: &'a [String],
    pub descriptions_exclude: &'a [String],
    pub collections: &'a [String],
    pub collections_exclude: &'a [String],
    pub path_prefixes: &'a [String],
    pub path_prefixes_exclude: &'a [String],
    pub volume: Option<&'a str>,
    pub volume_ids: &'a [String],
    pub volume_ids_exclude: &'a [String],
    pub rating: Option<NumericFilter>,
    pub iso: Option<NumericFilter>,
    pub focal: Option<NumericFilter>,
    pub aperture: Option<NumericFilter>,
    pub width: Option<NumericFilter>,
    pub height: Option<NumericFilter>,
    pub copies: Option<NumericFilter>,
    pub variant_count: Option<NumericFilter>,
    pub scattered: Option<NumericFilter>,
    pub session_root_pattern: &'a str,
    pub face_count: Option<NumericFilter>,
    pub tag_count: Option<NumericFilter>,
    pub duration: Option<NumericFilter>,
    pub codec: Option<String>,
    pub audio_key: Option<String>,
    pub audio_bpm: Option<NumericFilter>,
    pub stale_days: Option<NumericFilter>,
    pub stale_never: bool,
    pub meta_filters: Vec<(&'a str, &'a str)>,
    pub orphan: bool,
    pub orphan_false: bool,
    pub missing_asset_ids: Option<&'a [String]>,
    pub no_online_locations: Option<&'a [String]>,
    pub has_online_locations: Option<&'a [String]>,
    pub collection_asset_ids: Option<&'a [String]>,
    pub collection_exclude_ids: Option<&'a [String]>,
    pub date_prefix: Option<&'a str>,
    pub date_from: Option<&'a str>,
    pub date_until: Option<&'a str>,
    pub collapse_stacks: bool,
    pub stacked_filter: Option<bool>,
    pub geo_bbox: Option<(f64, f64, f64, f64)>,
    pub has_gps: Option<bool>,
    pub has_faces: Option<bool>,
    pub has_embed: Option<bool>,
    pub person_asset_ids: Option<&'a [String]>,
    pub person_exclude_ids: Option<&'a [String]>,
    pub similar_asset_ids: Option<&'a [String]>,
    pub text_search_ids: Option<&'a [String]>,
    pub sort: SearchSort,
    pub page: u32,
    pub per_page: u32,
}

impl<'a> Default for SearchOptions<'a> {
    fn default() -> Self {
        Self {
            asset_ids: &[],
            text: None,
            text_exclude: &[],
            asset_types: &[],
            asset_types_exclude: &[],
            tags: &[],
            tags_exclude: &[],
            formats: &[],
            formats_exclude: &[],
            color_labels: &[],
            color_labels_exclude: &[],
            color_label_none: false,
            color_label_any: false,
            cameras: &[],
            cameras_exclude: &[],
            lenses: &[],
            lenses_exclude: &[],
            descriptions: &[],
            descriptions_exclude: &[],
            collections: &[],
            collections_exclude: &[],
            path_prefixes: &[],
            path_prefixes_exclude: &[],
            volume: None,
            volume_ids: &[],
            volume_ids_exclude: &[],
            rating: None,
            iso: None,
            focal: None,
            aperture: None,
            width: None,
            height: None,
            copies: None,
            variant_count: None,
            scattered: None,
            session_root_pattern: r"^\d{4}-\d{2}",
            face_count: None,
            tag_count: None,
            duration: None,
            codec: None,
            audio_key: None,
            audio_bpm: None,
            stale_days: None,
            stale_never: false,
            meta_filters: Vec::new(),
            orphan: false,
            orphan_false: false,
            missing_asset_ids: None,
            no_online_locations: None,
            has_online_locations: None,
            collection_asset_ids: None,
            collection_exclude_ids: None,
            date_prefix: None,
            date_from: None,
            date_until: None,
            collapse_stacks: false,
            stacked_filter: None,
            geo_bbox: None,
            has_gps: None,
            has_faces: None,
            has_embed: None,
            person_asset_ids: None,
            person_exclude_ids: None,
            similar_asset_ids: None,
            text_search_ids: None,
            sort: SearchSort::DateDesc,
            page: 1,
            per_page: 60,
        }
    }
}

/// A page of search results with pagination metadata.
#[derive(Debug, serde::Serialize)]
pub struct SearchPage {
    pub rows: Vec<SearchRow>,
    pub total: u64,
    pub page: u32,
    pub per_page: u32,
    pub total_pages: u32,
}

// ═══ HELPER FUNCTIONS ═══

/// Convert a path pattern (with `*` wildcards) to a SQL LIKE pattern.
///
/// Rules:
/// - `*` becomes `%` (match any sequence)
/// - Literal `%`, `_`, and `\` are escaped via `ESCAPE '\'`
/// - A trailing `%` is appended if not already present, so `path:Pictures/2026`
///   keeps prefix semantics
///
/// Examples:
/// - `Pictures/2026`        → `Pictures/2026%`
/// - `Pictures/*/Capture`   → `Pictures/%/Capture%`
/// - `*/2026/*/party`       → `%/2026/%/party%`
/// - `*party`               → `%party%`
fn path_pattern_to_like(pat: &str) -> String {
    let mut out = String::with_capacity(pat.len() + 2);
    for c in pat.chars() {
        match c {
            '\\' => { out.push('\\'); out.push('\\'); }
            '%'  => { out.push('\\'); out.push('%'); }
            '_'  => { out.push('\\'); out.push('_'); }
            '*'  => out.push('%'),
            c    => out.push(c),
        }
    }
    if !out.ends_with('%') {
        out.push('%');
    }
    out
}

/// Convert an inclusive date bound to an exclusive upper bound.
///
/// - `"2026-02-25"` → `"2026-02-26"` (next day)
/// - `"2026-02"` → `"2026-03"` (next month)
/// - `"2026"` → `"2027"` (next year)
///
/// Falls back to appending a high character if parsing fails.
fn next_date_bound(s: &str) -> String {
    match s.len() {
        // Year: "2026" → "2027"
        4 => {
            if let Ok(y) = s.parse::<i32>() {
                return format!("{:04}", y + 1);
            }
        }
        // Month: "2026-02" → "2026-03"
        7 => {
            let parts: Vec<&str> = s.splitn(2, '-').collect();
            if parts.len() == 2 {
                if let (Ok(y), Ok(m)) = (parts[0].parse::<i32>(), parts[1].parse::<u32>()) {
                    if m >= 12 {
                        return format!("{:04}-01", y + 1);
                    } else {
                        return format!("{:04}-{:02}", y, m + 1);
                    }
                }
            }
        }
        // Day: "2026-02-25" → "2026-02-26"
        10 => {
            if let Ok(date) = chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d") {
                let next = date + chrono::Duration::days(1);
                return next.format("%Y-%m-%d").to_string();
            }
        }
        _ => {}
    }
    // Fallback: append a char higher than any valid timestamp character
    format!("{s}\x7f")
}

/// Current schema version. Bump this whenever `run_migrations()` changes.
pub const SCHEMA_VERSION: u32 = 13;

pub use search_builder::TAG_INDEX_MAX_MATCHES;

// ═══ CATALOG STRUCT & CONNECTION ═══

/// SQLite-backed local catalog for fast queries. This is a derived cache,
/// not the source of truth (sidecar files are).
pub struct Catalog {
    conn: Connection,
    /// Per-connection memo of `tag:` entries → "serve from the trigram
    /// index" decisions (see `search_builder::TagIndexPlan`). Keeps the
    /// selectivity probe to one run per tag per connection, so the
    /// count, page and facet queries of a browse request share it.
    tag_index_memo: std::cell::RefCell<std::collections::HashMap<String, bool>>,
    /// Probe limit: a tag entry matching fewer than this many assets is
    /// served from the index, otherwise by the original scan. Tests lower
    /// it to force the scan path.
    tag_index_limit: std::cell::Cell<u32>,
}

impl Catalog {
    /// Open the catalog at `<catalog_root>/catalog.db`, applying connection
    /// pragmas and registering the SQL helper functions — nothing else.
    ///
    /// No schema work happens here: tables and migrations are applied by
    /// [`Self::initialize`] (`init`, `rebuild-catalog`) and
    /// [`Self::open_and_migrate`] (`maki migrate`); every other command only
    /// checks the stored schema version at startup and exits if it is
    /// outdated. [`Self::open_fast`] is an alias of this function.
    pub fn open(catalog_root: &Path) -> Result<Self> {
        let db_path = catalog_root.join("catalog.db");
        let conn = Connection::open(&db_path)?;
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;
             PRAGMA cache_size = -20000;
             PRAGMA mmap_size = 268435456;
             PRAGMA temp_store = MEMORY;",
        )?;

        // Register path_dir(path, depth) — extracts the directory prefix from a path.
        // depth=0 (or NULL): returns the parent directory (everything before last '/').
        // depth=N: returns the first N path segments.
        // Examples:
        //   path_dir('2026/Selects/img.nef', 0)  → '2026/Selects'
        //   path_dir('2026/Selects/img.nef', 1)  → '2026'
        //   path_dir('2026/Selects/img.nef', 2)  → '2026/Selects'
        //   path_dir('img.nef', 0)                → ''
        conn.create_scalar_function("path_dir", 2, rusqlite::functions::FunctionFlags::SQLITE_DETERMINISTIC | rusqlite::functions::FunctionFlags::SQLITE_UTF8, |ctx| {
            let path: String = ctx.get(0)?;
            let depth: i64 = ctx.get(1)?;

            if depth <= 0 {
                // Parent directory: everything before the last '/'
                Ok(path.rfind('/').map_or(String::new(), |pos| path[..pos].to_string()))
            } else {
                // First N segments: find the Nth '/'
                let mut count = 0;
                for (i, ch) in path.char_indices() {
                    if ch == '/' {
                        count += 1;
                        if count == depth as usize {
                            return Ok(path[..i].to_string());
                        }
                    }
                }
                // Fewer than N slashes — return the whole path minus the last segment
                Ok(path.rfind('/').map_or(path.clone(), |p| path[..p].to_string()))
            }
        })?;

        // Register session_root(path, pattern) — the session root of a file
        // path by the shared rule in `crate::session_root` (the same one
        // auto-group uses): everything up to and including the deepest
        // directory component matching the regex pattern. With no match
        // (or an empty/invalid pattern) the SQL side keeps its own fallback
        // — the parent directory, or the directory itself when there is
        // only one component — because it has no import roots to consult.
        //
        // The compiled regex is cached in a RefCell because the pattern is the
        // same for every row within a query — compiling it once instead of per-row
        // is critical for performance on large catalogs.
        let regex_cache: std::cell::RefCell<Option<(String, regex::Regex)>> = std::cell::RefCell::new(None);
        conn.create_scalar_function("session_root", 2, rusqlite::functions::FunctionFlags::SQLITE_DETERMINISTIC | rusqlite::functions::FunctionFlags::SQLITE_UTF8, move |ctx| {
            let path: String = ctx.get(0)?;
            let pattern: String = ctx.get(1)?;

            // Directory part (strip filename)
            let dir = path.rfind('/').map_or("", |pos| &path[..pos]);

            if pattern.is_empty() {
                return Ok(dir.to_string());
            }

            // Cache the compiled regex — same pattern for every row in a query
            let mut cache = regex_cache.borrow_mut();
            if cache.as_ref().is_none_or(|(p, _)| p != &pattern) {
                match crate::session_root::compile_pattern(&pattern) {
                    Some(r) => *cache = Some((pattern.clone(), r)),
                    None => return Ok(dir.to_string()),
                }
            }
            let re = &cache.as_ref().expect("regex cache just populated").1;

            match crate::session_root::deepest_matching_prefix(dir, re) {
                Some(root) => Ok(root),
                None => Ok(dir.rfind('/').map_or(dir.to_string(), |p| dir[..p].to_string())),
            }
        })?;

        Ok(Self {
            conn,
            tag_index_memo: Default::default(),
            tag_index_limit: std::cell::Cell::new(search_builder::TAG_INDEX_MAX_MATCHES),
        })
    }

    /// Open and run schema migrations. Call once at program startup.
    ///
    /// After schema migrations, propagates any SQLite-only data back to YAML
    /// for migrations that touched YAML-backed columns — preserving the
    /// invariant that SQLite is fully derivable from YAML. Specifically:
    /// the v5→v6 migration backfills `faces.recognition_model`, which is
    /// now a persisted YAML field. Without this sync, a `rebuild-catalog`
    /// immediately after the v5→v6 upgrade would strip the tag.
    pub fn open_and_migrate(catalog_root: &Path) -> Result<Self> {
        let catalog = Self::open(catalog_root)?;
        let before = catalog.schema_version();
        catalog.run_migrations()?;
        catalog.post_migration_sync(catalog_root, before)?;
        Ok(catalog)
    }

    /// Post-migration YAML sync for migrations that modified YAML-backed
    /// data. Called once at catalog-open time after `run_migrations()`.
    ///
    /// `previous_version` is the schema version *before* migration ran, so
    /// we know which migrations just happened.
    fn post_migration_sync(&self, catalog_root: &Path, previous_version: u32) -> Result<()> {
        // v5 → v6: `faces.recognition_model` added and backfilled. Re-export
        // faces.yaml so the tag propagates to the source-of-truth sidecar.
        // Cheap relative to the migration itself (one small file write per
        // thousand faces), and runs exactly once per catalog.
        #[cfg(feature = "ai")]
        if previous_version < 6 {
            use anyhow::Context as _;
            crate::face_store::FaceStore::open(&self.conn, catalog_root)
                .persist()
                .context("re-exporting faces.yaml / people.yaml after the v6 migration")?;
        }
        // v6 → v7: `assets.face_scan_status` added — this lives on the Asset
        // YAML sidecar, which is per-file. Re-writing every sidecar during
        // migration would be slow on large catalogs, so we instead rely on
        // the rebuild-catalog fallback (stamps `face_scan_status='done'` on
        // any asset with face records) and on detect_faces writing the flag
        // to YAML for assets it touches from here on.
        // Silence unused-var warnings in non-AI builds, where the only active
        // branch above is gated out.
        let _ = previous_version;
        let _ = catalog_root;
        Ok(())
    }

    /// Read the stored schema version (0 if table doesn't exist yet).
    pub fn schema_version(&self) -> u32 {
        self.conn
            .query_row(
                "SELECT version FROM schema_version LIMIT 1",
                [],
                |row| row.get(0),
            )
            .unwrap_or(0)
    }

    /// Check if migrations are needed (fast — single query).
    /// Returns `true` if the schema is up to date.
    pub fn is_schema_current(&self) -> bool {
        self.schema_version() >= SCHEMA_VERSION
    }

    /// Alias for [`Self::open`] — kept for clarity at the web-server pool
    /// call sites, but identical (neither runs migrations).
    pub fn open_fast(catalog_root: &Path) -> Result<Self> {
        Self::open(catalog_root)
    }

    /// Access the underlying SQLite connection.
    pub fn conn(&self) -> &Connection {
        &self.conn
    }

    /// Run `f` inside one SQLite transaction: committed when it returns
    /// `Ok`, rolled back when it returns `Err` (or panics). Every writer
    /// that touches more than one row goes through here (audit F2).
    ///
    /// Nests: when the connection is already inside a transaction the
    /// closure simply joins it, so a transactional helper can call other
    /// transactional helpers. Foreign-key checks that must pass through a
    /// temporarily inconsistent state can defer them to COMMIT with
    /// `catalog.conn().execute_batch("PRAGMA defer_foreign_keys = ON")`
    /// inside the closure (see `migrate_variant_hash`).
    pub fn with_tx<T>(&self, f: impl FnOnce(&Self) -> Result<T>) -> Result<T> {
        crate::tx::run(&self.conn, || f(self))
    }

}

mod schema;
mod asset_crud;
mod variant_crud;
mod recipe_crud;
mod volume;
mod lookup;
mod duplicates;
mod recipe_query;
mod rebuild;
mod stats;
mod search_builder;
mod search_exec;
mod facets;
mod tags;
mod analytics;
mod backup;
mod cleanup;


// ═══ TESTS ═══

#[cfg(test)]
pub(crate) mod test_support;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn next_date_bound_day() {
        assert_eq!(next_date_bound("2026-02-25"), "2026-02-26");
        assert_eq!(next_date_bound("2026-02-28"), "2026-03-01");
        assert_eq!(next_date_bound("2026-12-31"), "2027-01-01");
    }

    #[test]
    fn next_date_bound_month() {
        assert_eq!(next_date_bound("2026-02"), "2026-03");
        assert_eq!(next_date_bound("2026-12"), "2027-01");
    }

    #[test]
    fn next_date_bound_year() {
        assert_eq!(next_date_bound("2026"), "2027");
    }

    #[test]
    fn path_pattern_plain_prefix() {
        assert_eq!(path_pattern_to_like("Pictures/2026"), "Pictures/2026%");
    }

    #[test]
    fn path_pattern_star_in_middle() {
        assert_eq!(path_pattern_to_like("Pictures/*/Capture"), "Pictures/%/Capture%");
    }

    #[test]
    fn path_pattern_leading_star() {
        assert_eq!(path_pattern_to_like("*party"), "%party%");
    }

    #[test]
    fn path_pattern_complex() {
        assert_eq!(path_pattern_to_like("*/2026/*/party"), "%/2026/%/party%");
    }

    #[test]
    fn path_pattern_escapes_sql_wildcards() {
        // Literal % and _ in user input must be escaped
        assert_eq!(path_pattern_to_like("foo%bar"), "foo\\%bar%");
        assert_eq!(path_pattern_to_like("foo_bar"), "foo\\_bar%");
    }

    #[test]
    fn path_pattern_escapes_backslash() {
        assert_eq!(path_pattern_to_like("foo\\bar"), "foo\\\\bar%");
    }

    #[test]
    fn path_pattern_double_star_collapses() {
        // ** is harmless: %% in SQL behaves like %
        assert_eq!(path_pattern_to_like("**foo"), "%%foo%");
    }

    #[test]
    fn path_pattern_only_star() {
        // Just `*` matches everything (becomes `%`, no trailing % appended since already ends in %)
        assert_eq!(path_pattern_to_like("*"), "%");
    }

    #[test]
    fn path_pattern_trailing_star_no_double() {
        // Trailing * already produces trailing %, so we don't append another
        assert_eq!(path_pattern_to_like("foo*"), "foo%");
    }
}
