//! MAKI binary entrypoint — parses the CLI, resolves the catalog and its
//! `maki.toml` once, dispatches to the `commands::run_X_command` handlers,
//! and converts the result to a process exit code. `run_command` is a flat
//! dispatcher: every arm is one call.

use std::path::PathBuf;

mod commands;
use commands::*;

use clap::{Parser, Subcommand};
use maki::catalog::Catalog;
use maki::cli_output::{format_duration, CliContext};
use maki::config::CatalogConfig;

/// Long help text for the `maki search QUERY` argument. Shown on
/// `maki search --help` and `maki help search`. Short help (shown on
/// `-h`) stays compact; this expands into a categorised reference
/// sized to fit one terminal screen. Full details in
/// doc/manual/reference/06-search-filters.md and the printable PDF
/// at `maki doc filters`.
const SEARCH_QUERY_LONG_HELP: &str = "\
Free-text keywords and filter expressions.

Combining: space = AND (repeat a filter to AND), comma = OR within one filter
(not for id:, codec:, key:, date*:, meta: — those take a single value).
Prefix `-` to exclude (e.g. `-tag:rejected`) on list-style filters only: tag, type,
format, camera, lens, description, collection, path, person, volume, label, and
free text (`-label:none` = labeled, `-volume:none` = on an online volume).
Numeric, date, and boolean filters ignore the `-`.

TEXT & METADATA
  tag:landscape           tag match at any level
  tag:=Foo                whole-path match (exact tag value)
  tag:/Foo                leaf only at any level
  tag:^Foo                case-sensitive
  tag:|wed                prefix anchor (wedding, wedding-2024, ...)
  type:image              asset type (image, video, audio, document, other)
  format:nef              file format (e.g. format:jpg,jpeg)
  label:Red / label:none  colour label (or \"unlabeled\")
  camera:fuji             camera (substring match on EXIF)
  lens:56mm               lens (substring)
  description:cat         description substring (alias: desc:)
  collection:Fav          collection membership
  path:Pictures/2026      path prefix (* wildcards supported)
  id:72a0                 asset ID prefix
  meta:key=val            raw source-metadata field match

NUMERIC (all support: N exact / N+ min / A-B range / A,B OR-list)
  rating:3+               rating (0 = unrated)
  tagcount:0              number of intentional (leaf) tags on the asset
  iso:100-800             ISO range
  focal:35-70             focal length (mm)
  f:1.4-2.8               aperture
  width:4000+             minimum pixel width
  height:2000+            minimum pixel height
  copies:2+               number of file copies across volumes
  variants:2+             number of variants on the asset
  scattered:2+            distinct session roots (files spread across shoots)
  duration:30+            audio/video duration (seconds)

DATE
  date:2026               year prefix (also 2026-03, 2026-03-15)
  dateFrom:2026-01        from (inclusive)
  dateUntil:2026-12       until (inclusive)

STATUS
  orphan:true / orphan:false    with/without file locations
  missing:true                  files missing on disk
  stale:30                      not verified within N days (or never)
  stale:never                   never verified
  stacked:true / stacked:false  in a stack (collapsed in browse)
  volume:Archive                on specific volume (or volume:none)
  geo:any / geo:none            has / lacks GPS coordinates
  geo:<S,W,N,E>                 GPS bounding box
  geo:<lat>,<lng>,<km>          within radius
  codec:h264                    video codec (substring)
  key:Am / bpm:120+             audio musical key / tempo

AI (any build)
  faces:2+ / faces:any / faces:none   face count filters
  embed:any / embed:none              has / lacks SigLIP embedding

PRO (require --features pro)
  person:Alice            named person (comma = OR, repeat = AND — like tag:)
  similar:<id>[:<limit>]  visually similar to an asset (default 40)
  --image <file>          visually similar to a local image (not in catalog)
  min_sim:90              similarity threshold (0-100%) for similar:/text:/--image
  text:sunset[:<limit>]   SigLIP text-to-image search (default 50); quote multi-word

COMBINING
  tag:a,b                 a OR b (comma within one filter)
  tag:a tag:b             a AND b (repeat the filter)
  -tag:rejected           NOT

Full reference: doc/manual/reference/06-search-filters.md
Printable 2-page PDF: `maki doc filters`
";

#[derive(Parser)]
#[command(name = "maki", about = "Media Asset Keeper & Indexer",
    version = maki::features::VERSION,
    after_help = "Use maki --help for grouped overview, or maki <command> --help for details."
)]
struct Cli {
    /// Output machine-readable JSON (valid JSON on stdout, messages on stderr)
    #[arg(long, global = true, display_order = 30)]
    json: bool,

    /// Log per-file progress to stderr (all multi-file commands)
    #[arg(short = 'l', long = "log", global = true, display_order = 40)]
    log: bool,

    /// Show operational decisions and program flow
    #[arg(short = 'v', long = "verbose", global = true, display_order = 41)]
    verbose: bool,

    /// Show debug output from external tools (ffmpeg, dcraw, curl)
    #[arg(short = 'd', long = "debug", global = true, display_order = 42)]
    debug: bool,

    /// Show elapsed time after command execution
    #[arg(short = 't', long = "time", global = true, display_order = 43)]
    timing: bool,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    // --- Setup ---

    /// Initialize a new catalog in the current directory
    #[command(display_order = 1)]
    Init,

    /// Manage storage volumes
    #[command(subcommand, display_order = 2)]
    Volume(VolumeCommands),

    // --- Ingest & Edit ---

    /// Import files into the catalog
    #[command(display_order = 10)]
    Import {
        /// Paths to files or directories to import
        paths: Vec<String>,

        /// Import onto a specific volume (instead of auto-detecting from path)
        #[arg(long, display_order = 10)]
        volume: Option<String>,

        /// Use a named import profile from maki.toml [import.profiles.<name>]
        #[arg(long, display_order = 11)]
        profile: Option<String>,

        /// Include additional file type groups (e.g. captureone, documents)
        #[arg(long, display_order = 12)]
        include: Vec<String>,

        /// Skip default file type groups (e.g. audio, xmp)
        #[arg(long, display_order = 13)]
        skip: Vec<String>,

        /// Add a tag to every imported asset (repeatable)
        #[arg(long = "add-tag", display_order = 15)]
        add_tags: Vec<String>,

        /// Show what would be imported without making changes
        #[arg(long, display_order = 20)]
        dry_run: bool,

        /// Auto-group imported files with nearby catalog assets by filename stem
        #[arg(long, display_order = 21)]
        auto_group: bool,

        /// Generate smart previews (2560px) alongside regular previews
        #[arg(long, display_order = 22)]
        smart: bool,

        /// Generate image embeddings for visual similarity search (requires --features ai)
        #[cfg(feature = "ai")]
        #[arg(long, display_order = 23)]
        embed: bool,

        /// Generate VLM descriptions for imported assets (requires running Ollama)
        #[cfg(feature = "pro")]
        #[arg(long, display_order = 24)]
        describe: bool,
    },

    /// Watch directories and auto-import new files (poll-based)
    ///
    /// Polls the watched roots every N seconds. A new or changed file is
    /// acted on only once its size and modification time are identical on
    /// two consecutive scans, so files still being copied (e.g. during a
    /// tethered capture session) are never half-imported. New stable files
    /// go through the regular import pipeline; changed .xmp recipe files
    /// already tracked by the catalog are refreshed. Deleted files are
    /// ignored (run `maki cleanup` explicitly).
    ///
    /// Files already present when the watch starts form the baseline and
    /// are NOT imported — run `maki import` for the backlog. With --once a
    /// single cycle runs instead: stable files the catalog doesn't track
    /// yet are imported, tracked .xmp recipes are refreshed if changed,
    /// then the command exits (useful for scripting).
    #[command(display_order = 23)]
    Watch {
        /// Paths to watch (default: mount points of all online volumes)
        paths: Vec<String>,

        /// Watch a specific volume (paths, if given, must lie on it)
        #[arg(long, display_order = 10)]
        volume: Option<String>,

        /// Poll interval in seconds (min 2; default: [watch] poll_seconds, 30)
        #[arg(long, display_order = 11)]
        interval: Option<u64>,

        /// Run a single scan cycle (~1s stability wait) and exit
        #[arg(long, display_order = 20)]
        once: bool,
    },

    /// Delete assets from the catalog
    #[command(display_order = 11)]
    Delete {
        /// Asset IDs to delete (reads from stdin if empty)
        asset_ids: Vec<String>,
        /// Execute deletion (default: report-only)
        #[arg(long)]
        apply: bool,
        /// Also delete physical files from disk (requires --apply).
        /// Files go to `<catalog>/.trash/` (recoverable via
        /// `maki trash restore`) unless trash is disabled.
        #[arg(long)]
        remove_files: bool,
        /// Delete files permanently instead of moving them to the trash
        #[arg(long)]
        no_trash: bool,
    },

    /// Add, remove, or rename tags
    #[command(display_order = 12, args_conflicts_with_subcommands = true)]
    Tag {
        /// Asset ID
        asset_id: Option<String>,

        /// Remove the specified tags instead of adding them
        #[arg(long)]
        remove: bool,

        /// Tags to add or remove
        tags: Vec<String>,

        #[command(subcommand)]
        subcmd: Option<TagCommands>,
    },

    /// Edit asset metadata (name, description, rating)
    #[command(display_order = 13)]
    Edit {
        /// Asset ID (or unique prefix)
        asset_id: String,

        /// Set asset name
        #[arg(long)]
        name: Option<String>,

        /// Clear asset name
        #[arg(long)]
        clear_name: bool,

        /// Set asset description
        #[arg(long)]
        description: Option<String>,

        /// Clear asset description
        #[arg(long)]
        clear_description: bool,

        /// Set rating (1-5)
        #[arg(long, value_parser = clap::value_parser!(u8).range(1..=5))]
        rating: Option<u8>,

        /// Clear rating
        #[arg(long)]
        clear_rating: bool,

        /// Set color label (Red, Orange, Yellow, Green, Blue, Pink, Purple)
        #[arg(long)]
        label: Option<String>,

        /// Clear color label
        #[arg(long)]
        clear_label: bool,

        /// Remove all tags
        #[arg(long)]
        clear_tags: bool,

        /// Set date (YYYY, YYYY-MM, YYYY-MM-DD, or ISO 8601)
        #[arg(long)]
        date: Option<String>,

        /// Reset date to now
        #[arg(long)]
        clear_date: bool,

        /// Set variant role (original, alternate, processed, export, sidecar)
        #[arg(long)]
        role: Option<String>,

        /// Variant content hash (required with --role)
        #[arg(long)]
        variant: Option<String>,
    },

    /// Group variants into one asset
    #[command(display_order = 14)]
    Group {
        /// Content hashes of variants to group
        variant_hashes: Vec<String>,
    },

    /// Split variants out of an asset into new standalone assets
    #[command(display_order = 15)]
    Split {
        /// Asset ID to split
        asset_id: String,
        /// Content hashes of variants to extract
        variant_hashes: Vec<String>,
    },

    /// Auto-group assets by filename stem
    #[command(display_order = 16)]
    AutoGroup {
        /// Search query to scope assets (same syntax as maki search)
        query: Option<String>,
        /// Apply grouping (default: report-only)
        #[arg(long)]
        apply: bool,
        /// Group across all directories (DANGEROUS: may merge unrelated assets with same filename)
        #[arg(long)]
        global: bool,
    },

    /// Auto-tag assets using AI vision model (requires --features ai)
    #[cfg(feature = "ai")]
    #[command(display_order = 17)]
    AutoTag {
        /// Search query to scope assets (same syntax as `maki search`)
        #[arg(display_order = 1)]
        query: Option<String>,

        /// Process a specific asset (ID or prefix)
        #[arg(long, display_order = 2)]
        asset: Option<String>,

        /// Limit to assets on a specific volume
        #[arg(long, display_order = 3)]
        volume: Option<String>,

        /// AI model to use (default from maki.toml or siglip-vit-b16-256)
        #[arg(long, display_order = 4)]
        model: Option<String>,

        /// Confidence threshold (0.0–1.0, default from maki.toml or 0.1)
        #[arg(long, display_order = 10)]
        threshold: Option<f32>,

        /// Path to custom labels file (one label per line)
        #[arg(long, display_order = 11)]
        labels: Option<String>,

        /// Apply suggested tags (default: report-only)
        #[arg(long, display_order = 20)]
        apply: bool,

        /// Download the AI model
        #[arg(long, display_order = 30)]
        download: bool,

        /// Remove cached AI model files
        #[arg(long, display_order = 31)]
        remove_model: bool,

        /// Show available AI models
        #[arg(long, display_order = 32)]
        list_models: bool,

        /// Print the active label list (default or from --labels / config)
        #[arg(long, display_order = 33)]
        list_labels: bool,

        /// Find visually similar assets (by asset ID)
        #[arg(long, display_order = 40)]
        similar: Option<String>,

        /// Asset IDs (for shell variable expansion)
        #[arg(hide = true, trailing_var_arg = true)]
        asset_ids: Vec<String>,
    },

    /// Cluster visually similar assets into stacks (requires --features ai)
    #[cfg(feature = "ai")]
    #[command(display_order = 18)]
    AutoStack {
        /// Search query to scope candidate assets (same syntax as `maki search`)
        #[arg(display_order = 1)]
        query: Option<String>,

        /// Similarity threshold in percent (50–99, default 85)
        #[arg(long, display_order = 2)]
        threshold: Option<u32>,

        /// Minimum cluster size (default 2)
        #[arg(long, display_order = 3)]
        min_size: Option<usize>,

        /// Create the proposed stacks (default: report-only)
        #[arg(long, display_order = 10)]
        apply: bool,

        /// Asset IDs (for shell variable expansion)
        #[arg(hide = true, trailing_var_arg = true)]
        asset_ids: Vec<String>,
    },

    /// Generate embeddings for visual similarity search (requires --features ai)
    #[cfg(feature = "ai")]
    #[command(display_order = 18)]
    Embed {
        /// Search query to scope assets (same syntax as `maki search`)
        #[arg(display_order = 1)]
        query: Option<String>,

        /// Process a specific asset (ID or prefix)
        #[arg(long, display_order = 2)]
        asset: Option<String>,

        /// Limit to assets on a specific volume
        #[arg(long, display_order = 3)]
        volume: Option<String>,

        /// AI model to use (default from maki.toml or siglip-vit-b16-256)
        #[arg(long, display_order = 4)]
        model: Option<String>,

        /// Re-generate embeddings that already exist for the active model. Not needed when switching models — embeddings are stored per (asset_id, model_id), so a model switch only generates the missing ones.
        #[arg(long, display_order = 10)]
        force: bool,

        /// Export all embeddings from SQLite to binary files (no scope required)
        #[arg(long, display_order = 11)]
        export: bool,

        /// Asset IDs (for shell variable expansion)
        #[arg(hide = true, trailing_var_arg = true)]
        asset_ids: Vec<String>,
    },

    /// Face detection and recognition (requires --features ai)
    #[cfg(feature = "ai")]
    #[command(subcommand, display_order = 19)]
    Faces(FacesCommands),

    /// AI utilities — vocabulary export and (future) model management
    #[cfg(feature = "ai")]
    #[command(subcommand, display_order = 20)]
    Ai(AiCommands),

    /// Generate image descriptions using a vision-language model (VLM)
    #[cfg(feature = "pro")]
    #[command(display_order = 16)]
    Describe {
        /// Search query to scope assets (same syntax as `maki search`)
        #[arg(display_order = 1)]
        query: Option<String>,

        /// Process a specific asset (ID or prefix)
        #[arg(long, display_order = 2)]
        asset: Option<String>,

        /// Limit to assets on a specific volume
        #[arg(long, display_order = 3)]
        volume: Option<String>,

        /// VLM model name (default from maki.toml or qwen2.5vl:3b)
        #[arg(long, display_order = 4)]
        model: Option<String>,

        /// VLM server endpoint (default from maki.toml or http://localhost:11434)
        #[arg(long, display_order = 5)]
        endpoint: Option<String>,

        /// Custom prompt for the VLM
        #[arg(long, display_order = 6)]
        prompt: Option<String>,

        /// Maximum tokens in VLM response
        #[arg(long, display_order = 7)]
        max_tokens: Option<u32>,

        /// Request timeout in seconds (default from maki.toml or 300)
        #[arg(long, display_order = 8)]
        timeout: Option<u32>,

        /// What to generate: describe (prose), tags (structured), both
        /// (default from `[vlm] mode` in maki.toml, or describe)
        #[arg(long, display_order = 9)]
        mode: Option<String>,

        /// Sampling temperature (0.0 = deterministic, 1.0+ = creative)
        #[arg(long, display_order = 10)]
        temperature: Option<f32>,

        /// Context window size for Ollama (num_ctx)
        #[arg(long, display_order = 11)]
        num_ctx: Option<u32>,

        /// Top-p (nucleus) sampling threshold
        #[arg(long, display_order = 12)]
        top_p: Option<f32>,

        /// Top-k sampling: limit to k most likely tokens
        #[arg(long, display_order = 13)]
        top_k: Option<u32>,

        /// Repeat penalty (1.0 = no penalty)
        #[arg(long, display_order = 14)]
        repeat_penalty: Option<f32>,

        /// Apply descriptions to assets (default: report-only)
        #[arg(long, display_order = 20)]
        apply: bool,

        /// Overwrite existing descriptions
        #[arg(long, display_order = 21)]
        force: bool,

        /// Show what would happen without calling the VLM
        #[arg(long, display_order = 22)]
        dry_run: bool,

        /// Check VLM endpoint connectivity
        #[arg(long, display_order = 30)]
        check: bool,

        /// Asset IDs (for shell variable expansion)
        #[arg(hide = true, trailing_var_arg = true)]
        asset_ids: Vec<String>,
    },

    // --- Organize ---

    /// Manage collections (static albums)
    #[command(subcommand, alias = "col", display_order = 20)]
    Collection(CollectionCommands),

    /// Manage saved searches (smart albums)
    #[command(subcommand, alias = "ss", display_order = 21)]
    SavedSearch(SavedSearchCommands),

    /// Manage stacks (scene grouping)
    #[command(subcommand, alias = "st", display_order = 22)]
    Stack(StackCommands),

    // --- Retrieve ---

    /// Search assets
    #[command(display_order = 30)]
    Search {
        /// Free-text keywords and filter expressions. Run with --help for the full filter list.
        #[arg(long_help = SEARCH_QUERY_LONG_HELP, required_unless_present = "image")]
        query: Option<String>,

        /// Output format: ids, short, full, json, or a custom template (e.g. '{id}\t{name}')
        #[arg(long)]
        format: Option<String>,

        /// Shorthand for --format=ids (one asset ID per line, for scripting)
        #[arg(short = 'q', long = "quiet")]
        quiet: bool,

        /// Find assets visually similar to a local image file that is not
        /// in the catalog (e.g. locate the original behind a preview).
        /// Exact byte-copies are matched by content hash first; otherwise
        /// the file is encoded with the SigLIP model (requires --features ai)
        #[arg(long, value_name = "FILE", display_order = 40)]
        image: Option<PathBuf>,

        /// Result-set size for --image (default 40, counts the exact match)
        #[arg(long, value_name = "N", requires = "image", display_order = 41)]
        limit: Option<usize>,
    },

    /// Show asset details
    #[command(display_order = 31)]
    Show {
        /// Asset ID
        asset_id: String,

        /// Show only file locations (one per line: volume:path)
        #[arg(long)]
        locations: bool,
    },

    /// Open the asset's preview in the OS default image viewer
    #[command(display_order = 32)]
    Preview {
        /// Asset ID (or prefix)
        asset_id: String,
    },

    /// Find duplicate files
    #[command(display_order = 33)]
    Duplicates {
        /// Output format: ids, short, full, json, or a custom template (e.g. '{hash}\t{filename}')
        #[arg(long)]
        format: Option<String>,

        /// Show only same-volume duplicates (likely unwanted copies)
        #[arg(long, display_order = 10)]
        same_volume: bool,

        /// Show only cross-volume copies (wanted backups)
        #[arg(long, display_order = 11)]
        cross_volume: bool,

        /// Filter to entries involving this volume
        #[arg(long, display_order = 12)]
        volume: Option<String>,

        /// Filter to entries matching this file format (e.g. nef, jpg)
        #[arg(long, display_order = 13)]
        filter_format: Option<String>,

        /// Filter to entries with a location under this path prefix
        #[arg(long, display_order = 14)]
        path: Option<String>,
    },

    /// Generate a PDF contact sheet from search results
    #[command(display_order = 35)]
    ContactSheet {
        /// Search query (same syntax as maki search)
        query: String,

        /// Output PDF file path
        output: String,

        /// Layout preset: dense, standard, large
        #[arg(long, default_value = "standard")]
        layout: String,

        /// Number of columns (overrides layout preset)
        #[arg(long)]
        columns: Option<u32>,

        /// Number of rows per page (overrides layout preset)
        #[arg(long)]
        rows: Option<u32>,

        /// Paper size: a4, letter, a3
        #[arg(long, default_value = "a4")]
        paper: String,

        /// Use landscape orientation
        #[arg(long)]
        landscape: bool,

        /// Title printed on first page header
        #[arg(long)]
        title: Option<String>,

        /// Comma-separated metadata fields below each thumbnail
        #[arg(long)]
        fields: Option<String>,

        /// Override sort order: date, name, rating, filename
        #[arg(long)]
        sort: Option<String>,

        /// Use regular previews (800px) instead of smart previews (default: smart with fallback)
        #[arg(long)]
        no_smart: bool,

        /// Group by field with section headers: date, volume, collection, label
        #[arg(long)]
        group_by: Option<String>,

        /// Page margin in mm
        #[arg(long)]
        margin: Option<f32>,

        /// Color label rendering style: border, dot, none
        #[arg(long)]
        label_style: Option<String>,

        /// JPEG quality for page images (1-100)
        #[arg(long)]
        quality: Option<u8>,

        /// Copyright text displayed in the center of each page footer
        #[arg(long)]
        copyright: Option<String>,

        /// Report page count and asset count without generating
        #[arg(long)]
        dry_run: bool,
    },

    /// Export files matching a search query to a directory
    #[command(display_order = 34)]
    Export {
        /// Search query (same syntax as maki search)
        query: String,

        /// Target directory (created if needed), or ZIP file path with --zip
        target: String,

        /// Layout: flat (default) or mirror (preserves directory structure)
        #[arg(long, default_value = "flat")]
        layout: String,

        /// Create symlinks instead of copies
        #[arg(long)]
        symlink: bool,

        /// Export all variants (default: best variant only)
        #[arg(long)]
        all_variants: bool,

        /// Include recipe/sidecar files (.xmp, .cos, etc.)
        #[arg(long)]
        include_sidecars: bool,

        /// Export previews instead of originals (works with offline volumes)
        #[arg(long, conflicts_with = "smart_previews")]
        previews: bool,

        /// Export smart previews instead of originals (works with offline volumes)
        #[arg(long)]
        smart_previews: bool,

        /// Show what would be exported without writing files
        #[arg(long)]
        dry_run: bool,

        /// Re-copy even if target already has matching content
        #[arg(long)]
        overwrite: bool,

        /// Write a ZIP archive instead of copying files to a directory
        #[arg(long)]
        zip: bool,
    },

    /// Show catalog statistics
    #[command(display_order = 33)]
    Stats {
        /// Show asset type and format breakdown
        #[arg(long)]
        types: bool,

        /// Show per-volume details
        #[arg(long)]
        volumes: bool,

        /// Show tag usage statistics
        #[arg(long)]
        tags: bool,

        /// Show verification health
        #[arg(long)]
        verified: bool,

        /// Show all sections
        #[arg(long)]
        all: bool,

        /// Max entries for top-N lists (default: 20)
        #[arg(long, default_value_t = 20)]
        limit: usize,
    },

    /// Catalog health overview — pending cleanup, work, backup coverage, volumes
    #[command(display_order = 34)]
    Status {
        /// Minimum copies for backup coverage (default: 2)
        #[arg(long, default_value = "2", display_order = 10)]
        min_copies: u64,
    },

    /// Check backup coverage and find under-backed-up assets
    #[command(name = "backup-status", display_order = 35)]
    BackupStatus {
        /// Search query to scope the asset universe (same syntax as maki search)
        query: Option<String>,

        /// List under-backed-up assets (fewer than --min-copies locations)
        #[arg(long, display_order = 10)]
        at_risk: bool,

        /// Minimum copies for adequate backup (default: 2)
        #[arg(long, default_value = "2", display_order = 11)]
        min_copies: u64,

        /// Show which scoped assets are missing from this volume
        #[arg(long, display_order = 12)]
        volume: Option<String>,

        /// Output format for --at-risk listings (ids, short, full, json, or template)
        #[arg(long, display_order = 20)]
        format: Option<String>,

        /// Shorthand for --format=ids (one asset ID per line, for scripting)
        #[arg(short = 'q', long = "quiet", display_order = 21)]
        quiet: bool,
    },

    /// Start the web UI server
    #[command(display_order = 56)]
    Serve {
        /// Port to listen on (default: 8080, or from maki.toml [serve] port)
        #[arg(long, display_order = 10)]
        port: Option<u16>,

        /// Address to bind to (default: 127.0.0.1, or from maki.toml [serve] bind)
        #[arg(long, display_order = 11)]
        bind: Option<String>,

        /// Safe-sharing mode: reject every metadata edit, job, and
        /// file-deleting request (all non-GET requests return 403).
        /// Combine with `--bind` to let other people or devices browse
        /// the catalog without write access. Can be made permanent via
        /// `[serve] read_only = true` in maki.toml; HTTP basic auth is
        /// available via `[serve] username`/`password` (password may
        /// also come from the MAKI_SERVE_PASSWORD environment variable).
        #[arg(long, display_order = 12)]
        read_only: bool,
    },

    // --- Maintenance ---

    /// Undo the most recent metadata edit
    ///
    /// Reverts the newest not-yet-undone operation from the edit-history
    /// journal (`<catalog>/history/`), restoring each touched asset's prior
    /// state in the catalog and YAML sidecar. Covers field-level metadata
    /// edits (rating, label, description, name, date, tag add/remove/clear,
    /// single and batch) — not import, delete, or structural operations.
    /// Assets edited again since the recorded operation are reported as
    /// conflicts and skipped unless `--force`. XMP `.xmp` files are not
    /// touched live; affected recipes are flagged pending so a later
    /// `maki writeback` reconciles them.
    #[command(display_order = 37)]
    Undo {
        /// Show what would be undone without changing anything
        #[arg(long, display_order = 10)]
        dry_run: bool,

        /// Restore even assets that were edited since the operation
        #[arg(long, display_order = 11)]
        force: bool,
    },

    /// Show recent metadata edits (optionally for one asset)
    ///
    /// Lists operations from the edit-history journal, newest first. With
    /// an asset argument (prefix-resolved), shows only operations touching
    /// that asset together with the per-field change each made.
    #[command(display_order = 38)]
    History {
        /// Asset id (or unambiguous prefix) to scope the history to
        asset: Option<String>,

        /// Maximum number of operations to show
        #[arg(long, display_order = 10, default_value_t = 20)]
        limit: usize,
    },

    /// Check YAML-sidecar ↔ SQLite-catalog consistency
    ///
    /// The YAML sidecars are the source of truth; the SQLite catalog is
    /// a derived cache. Doctor compares them field-by-field (metadata,
    /// denormalized columns, variant/location/recipe sets including
    /// pending-writeback flags) and finds orphans in both directions.
    /// `--repair` rebuilds the SQLite side from YAML for flagged assets
    /// and deletes catalog rows that have no sidecar. Disk-file hash
    /// verification is `maki verify`'s job.
    #[command(display_order = 39)]
    Doctor {
        /// Check only ~N evenly spaced assets (presence checks always
        /// cover the full catalog)
        #[arg(long, display_order = 10)]
        sample: Option<usize>,

        /// Rebuild the SQLite rows from YAML for every flagged asset
        #[arg(long, display_order = 11)]
        repair: bool,
    },

    /// Check file integrity
    #[command(display_order = 40)]
    Verify {
        /// Files or directories to verify
        paths: Vec<String>,

        /// Limit verification to a specific volume
        #[arg(long, display_order = 10)]
        volume: Option<String>,

        /// Verify only a specific asset
        #[arg(long, display_order = 11)]
        asset: Option<String>,

        /// Include additional file type groups (e.g. captureone, documents)
        #[arg(long, display_order = 12)]
        include: Vec<String>,

        /// Skip default file type groups (e.g. audio, xmp)
        #[arg(long, display_order = 13)]
        skip: Vec<String>,

        /// Skip files verified within this many days (overrides maki.toml [verify] max_age_days)
        #[arg(long, display_order = 14)]
        max_age: Option<u64>,

        /// Verify all files regardless of last verification time
        #[arg(long, display_order = 15)]
        force: bool,
    },

    /// Sync catalog with disk changes (moved/modified/missing files)
    #[command(display_order = 41)]
    Sync {
        /// Paths to files or directories to scan
        paths: Vec<String>,

        /// Use a specific volume (instead of auto-detecting from path)
        #[arg(long, display_order = 10)]
        volume: Option<String>,

        /// Apply changes to catalog and sidecar files
        #[arg(long, display_order = 20)]
        apply: bool,

        /// Remove catalog locations for missing files (requires --apply)
        #[arg(long, display_order = 21)]
        remove_stale: bool,
    },

    /// Re-read metadata from changed sidecar/recipe files
    #[command(display_order = 42)]
    Refresh {
        /// Paths to files or directories to scan
        paths: Vec<String>,

        /// Limit to a specific volume
        #[arg(long, display_order = 10)]
        volume: Option<String>,

        /// Refresh only a specific asset
        #[arg(long, display_order = 11)]
        asset: Option<String>,

        /// Preview what would change without applying
        #[arg(long, display_order = 20)]
        dry_run: bool,

        /// Also re-extract embedded XMP from JPEG/TIFF media files
        #[arg(long, display_order = 21)]
        media: bool,

        /// Clear and re-extract all metadata from source files (XMP + EXIF)
        #[arg(long, display_order = 22)]
        reimport: bool,

        /// Re-extract only EXIF/source metadata from media files, leaving tags/description/rating/label untouched
        #[arg(long, display_order = 23)]
        exif_only: bool,
    },

    /// Bidirectional metadata sync: read external XMP changes and write back pending DAM edits
    #[cfg(feature = "pro")]
    #[command(name = "sync-metadata", display_order = 42)]
    SyncMetadata {
        /// Search query to select assets (same syntax as `maki search`)
        #[arg(display_order = 1)]
        query: Option<String>,

        /// Limit to a specific volume
        #[arg(long, display_order = 10)]
        volume: Option<String>,

        /// Limit to a specific asset
        #[arg(long, display_order = 11)]
        asset: Option<String>,

        /// Preview what would change without applying
        #[arg(long, display_order = 20)]
        dry_run: bool,

        /// Also re-extract embedded XMP from JPEG/TIFF media files
        #[arg(long, display_order = 21)]
        media: bool,

        /// Asset IDs (for shell variable expansion)
        #[arg(hide = true, trailing_var_arg = true)]
        asset_ids: Vec<String>,
    },

    /// Write pending metadata changes to XMP recipe files
    ///
    /// Manually flushes recipe edits (rating, label, description, tags)
    /// from the catalog to the .xmp files on disk. Always works regardless
    /// of `[writeback] enabled` in maki.toml — that config controls only
    /// AUTOMATIC writeback on every edit; this command is the explicit
    /// manual flush, intended for users who keep auto-flush off as a
    /// safety net but want to push staged changes for a specific set of
    /// assets.
    ///
    /// By default writes only recipes flagged `pending_writeback`. With
    /// `--all` (combined with a query/volume) it rewrites every XMP in
    /// the matching set, even those without pending markers — useful for
    /// rematerialising catalog metadata onto disk after large catalog-
    /// only edits.
    ///
    /// With `--embed`, writes embedded XMP into the JPEG variant files
    /// themselves instead of the .xmp sidecars (same scope and tag
    /// semantics; originals preserved in the catalog trash; variant
    /// content hashes migrated after the rewrite).
    #[cfg(feature = "pro")]
    #[command(display_order = 42)]
    Writeback {
        /// Search query to select assets (same syntax as `maki search`)
        #[arg(display_order = 1)]
        query: Option<String>,

        /// Limit to a specific volume
        #[arg(long, display_order = 10)]
        volume: Option<String>,

        /// Limit to a specific asset
        #[arg(long, display_order = 11)]
        asset: Option<String>,

        /// Write all XMP recipes in the matching set, not just those
        /// flagged pending. Use this together with a query/volume filter
        /// to rematerialise catalog metadata for a known asset set.
        #[arg(long, display_order = 12)]
        all: bool,

        /// Force a complete rewrite of every recipe's tag blocks in the
        /// scope: the dc:subject and lr:hierarchicalSubject blocks are
        /// rebuilt from the catalog's tag state, discarding whatever
        /// was on disk. Two things together:
        ///   1. ignores the pending-flag filter (works on recipes the
        ///      catalog reports as already in sync), and
        ///   2. drops every existing XMP keyword in the target blocks
        ///      before re-adding the catalog's tags fresh.
        ///
        /// More aggressive than `--mirror-tags`, which only drops
        /// entries the catalog doesn't have. Use this when stale
        /// entries are stuck in a file that mirror-tags didn't catch
        /// (e.g. multi-layer entity-escape leftovers, manually
        /// corrupted XMP), or just as an escape hatch for "I want this
        /// file rewritten now, regardless of state".
        ///
        /// Unlike `--all` (which expands the scope to every recipe in
        /// the catalog), `--force` keeps the explicit scope
        /// (`--asset`, query, or `--volume`).
        #[arg(long, display_order = 14)]
        force: bool,

        /// Make XMP keyword lists match the catalog exactly. The default
        /// flush is additive — it pushes catalog tags onto the XMP but
        /// never removes XMP tags the catalog no longer has, so renames
        /// and deletions made in MAKI leave stale entries on disk. With
        /// this flag, dc:subject and lr:hierarchicalSubject are read,
        /// diffed against the asset's catalog tags, and stale entries
        /// are removed. Pair with `--all <query>` for a one-shot cleanup
        /// after large catalog-only tag restructuring (rename, split,
        /// delete, fix-unicode) accumulated while auto-writeback was off.
        #[arg(long, display_order = 13, requires = "all")]
        mirror_tags: bool,

        /// Write embedded XMP into the asset's JPEG variant files
        /// instead of flushing .xmp recipe sidecars. REPLACES the
        /// recipe flush for this run — sidecar .xmp files are left
        /// untouched. For each asset in scope, every jpg/jpeg variant
        /// with an online file location gets its APP1 XMP segment
        /// updated (rating, label, description, dc:subject flat
        /// components + lr:hierarchicalSubject ancestor paths — same
        /// conventions and --force/--mirror-tags semantics as the
        /// sidecar flush). The file's bytes change, so the variant's
        /// content hash is re-computed and migrated across the catalog
        /// and sidecar. Originals are preserved in the catalog trash
        /// first (see --no-trash). TIFF variants are skipped (IFD
        /// rewriting deferred); offline volumes / missing files are
        /// counted as skipped, not queued.
        #[arg(long, display_order = 15)]
        embed: bool,

        /// (with --embed) Do not preserve original JPEGs in the
        /// catalog trash before rewriting them
        #[arg(long, display_order = 16, requires = "embed")]
        no_trash: bool,

        /// Preview what would be written without modifying files
        #[arg(long, display_order = 20)]
        dry_run: bool,

        /// Asset IDs (for shell variable expansion)
        #[arg(hide = true, trailing_var_arg = true)]
        asset_ids: Vec<String>,
    },

    /// Remove stale file location records (files no longer on disk)
    #[command(display_order = 43)]
    Cleanup {
        /// Limit to a specific volume
        #[arg(long, display_order = 10)]
        volume: Option<String>,

        /// Limit to files under this path prefix (relative to volume root)
        #[arg(long, display_order = 11)]
        path: Option<String>,

        /// List stale entries
        #[arg(long, display_order = 15)]
        list: bool,

        /// Apply changes (remove stale records from catalog and sidecar files)
        #[arg(long, display_order = 20)]
        apply: bool,
    },

    /// Remove same-volume duplicate file locations
    #[command(display_order = 44)]
    Dedup {
        /// Limit to a specific volume
        #[arg(long, display_order = 10)]
        volume: Option<String>,

        /// Prefer keeping locations whose path contains this string
        #[arg(long, display_order = 11)]
        prefer: Option<String>,

        /// Filter to a specific file format (e.g. nef, jpg)
        #[arg(long, display_order = 12)]
        filter_format: Option<String>,

        /// Filter to locations under this path prefix
        #[arg(long, display_order = 13)]
        path: Option<String>,

        /// Minimum total copies to preserve per variant (default: 1)
        #[arg(long, display_order = 14, default_value = "1")]
        min_copies: usize,

        /// Apply changes (delete files and remove location records).
        /// Removed files go to `<catalog>/.trash/` (recoverable via
        /// `maki trash restore`) unless trash is disabled.
        #[arg(long, display_order = 20)]
        apply: bool,

        /// Delete files permanently instead of moving them to the trash
        #[arg(long, display_order = 21)]
        no_trash: bool,
    },

    /// Inspect, restore, or empty the catalog trash
    ///
    /// File-deleting operations (`delete --remove-files`, `dedup`, the
    /// web duplicates page) move files into `<catalog>/.trash/` instead
    /// of deleting them permanently (unless `[trash] enabled = false`
    /// or `--no-trash`). This command manages that trash.
    #[command(display_order = 45)]
    Trash {
        #[command(subcommand)]
        command: TrashCommands,
    },

    /// Audio metadata analysis via external tools
    #[command(display_order = 47)]
    Audio {
        #[command(subcommand)]
        command: AudioCommands,
    },

    /// Copy or move asset files to another volume
    #[command(display_order = 46)]
    Relocate {
        /// Asset IDs (reads from stdin if empty and no --query)
        asset_ids: Vec<String>,

        /// Target volume label or ID
        #[arg(long)]
        target: Option<String>,

        /// Search query to select assets for batch relocate
        #[arg(long, display_order = 1)]
        query: Option<String>,

        /// Delete source files after successful copy and verification
        #[arg(long)]
        remove_source: bool,

        /// Create XMP sidecar files at the destination for assets with metadata but no existing recipe
        #[arg(long)]
        create_sidecars: bool,

        /// Show what would happen without making changes
        #[arg(long)]
        dry_run: bool,
    },

    /// Update a file's catalog path after it was moved on disk
    #[command(name = "update-location", display_order = 47)]
    UpdateLocation {
        /// Asset ID (or unique prefix)
        asset_id: String,

        /// Old path (absolute or volume-relative) where the file was before
        #[arg(long)]
        from: String,

        /// New absolute path where the file is now
        #[arg(long)]
        to: String,

        /// Volume label or ID (auto-detected from --to if omitted)
        #[arg(long)]
        volume: Option<String>,
    },

    /// Generate or regenerate preview thumbnails
    #[command(display_order = 48)]
    GeneratePreviews {
        /// Files or directories to generate previews for
        paths: Vec<String>,

        /// Limit to variants on a specific volume
        #[arg(long, display_order = 10)]
        volume: Option<String>,

        /// Only generate preview for a specific asset
        #[arg(long, display_order = 11)]
        asset: Option<String>,

        /// Include additional file type groups (e.g. captureone, documents)
        #[arg(long, display_order = 12)]
        include: Vec<String>,

        /// Skip default file type groups (e.g. audio, xmp)
        #[arg(long, display_order = 13)]
        skip: Vec<String>,

        /// Force regeneration even if previews already exist
        #[arg(long, display_order = 20)]
        force: bool,

        /// Regenerate previews for assets where a better variant (export/processed) exists
        #[arg(long, display_order = 21)]
        upgrade: bool,

        /// Also generate smart previews (high-resolution, 2560px) alongside thumbnails
        #[arg(long, display_order = 22)]
        smart: bool,
    },

    /// Fix variant roles (re-role non-RAW variants to Export in RAW+non-RAW groups)
    #[command(display_order = 49)]
    FixRoles {
        /// Files or directories to scope the fix
        paths: Vec<String>,

        /// Limit to a specific volume
        #[arg(long, display_order = 10)]
        volume: Option<String>,

        /// Fix only a specific asset
        #[arg(long, display_order = 11)]
        asset: Option<String>,

        /// Apply changes (default: report-only dry run)
        #[arg(long, display_order = 20)]
        apply: bool,
    },

    /// Fix asset dates from variant EXIF metadata and file modification times
    #[command(display_order = 50)]
    FixDates {
        /// Search query to select assets (same syntax as `maki search`)
        #[arg(display_order = 1)]
        query: Option<String>,

        /// Limit to a specific volume
        #[arg(long, display_order = 10)]
        volume: Option<String>,

        /// Fix only a specific asset
        #[arg(long, display_order = 11)]
        asset: Option<String>,

        /// Apply changes (default: report-only dry run)
        #[arg(long, display_order = 20)]
        apply: bool,

        /// Asset IDs (for shell variable expansion)
        #[arg(hide = true, trailing_var_arg = true)]
        asset_ids: Vec<String>,
    },

    /// Re-attach recipe files that were imported as standalone assets
    #[command(display_order = 51)]
    FixRecipes {
        /// Search query to select assets (same syntax as `maki search`)
        #[arg(display_order = 1)]
        query: Option<String>,

        /// Limit to a specific volume
        #[arg(long, display_order = 10)]
        volume: Option<String>,

        /// Fix only a specific asset
        #[arg(long, display_order = 11)]
        asset: Option<String>,

        /// Apply changes (default: report-only dry run)
        #[arg(long, display_order = 20)]
        apply: bool,

        /// Asset IDs (for shell variable expansion)
        #[arg(hide = true, trailing_var_arg = true)]
        asset_ids: Vec<String>,
    },

    /// Create XMP sidecar files for assets with metadata but no existing recipe
    #[command(name = "create-sidecars", display_order = 51)]
    CreateSidecars {
        /// Search query to select assets (same syntax as `maki search`)
        #[arg(display_order = 1)]
        query: Option<String>,

        /// Limit to a specific volume
        #[arg(long, display_order = 10)]
        volume: Option<String>,

        /// Create sidecars only for a specific asset
        #[arg(long, display_order = 11)]
        asset: Option<String>,

        /// Apply changes (default: report-only dry run)
        #[arg(long, display_order = 20)]
        apply: bool,

        /// Asset IDs (for shell variable expansion)
        #[arg(hide = true, trailing_var_arg = true)]
        asset_ids: Vec<String>,
    },

    /// Rebuild SQLite catalog from sidecar files
    #[command(display_order = 52)]
    RebuildCatalog {
        /// Rebuild only a specific asset (by ID or prefix)
        #[arg(long)]
        asset: Option<String>,
    },

    /// Run database schema migrations
    #[command(display_order = 53)]
    Migrate,

    /// Open documentation in the browser
    #[command(display_order = 56)]
    Doc {
        /// Which document: manual, cheatsheet, filters, tagging (default: manual)
        #[arg(default_value = "manual")]
        document: String,
    },

    /// Show MAKI license and third-party crate licenses
    #[command(display_order = 57)]
    Licenses {
        /// Show summary only (counts and where to find full text)
        #[arg(long)]
        summary: bool,
    },

    /// Start an interactive asset management shell
    #[command(display_order = 57)]
    Shell {
        /// Script file to execute (instead of interactive mode)
        script: Option<String>,

        /// Run a single command and exit
        #[arg(short = 'c', long = "command")]
        command_str: Option<String>,

        /// Exit on first error (scripts only)
        #[arg(long)]
        strict: bool,
    },
}

#[derive(Subcommand)]
enum TagCommands {
    /// Rename a tag across all assets
    Rename {
        /// Current tag name. Optional prefix markers (in any order) match the
        /// `tag:` search filter syntax: `=Foo` matches the exact level only
        /// (no descendants), `^Foo` is case-sensitive, `=^Foo` is both.
        old_tag: String,

        /// New tag name (always taken literally; no prefix parsing)
        new_tag: String,

        /// Apply changes (default: report-only)
        #[arg(long)]
        apply: bool,
    },

    /// Split one tag into two or more across all assets
    ///
    /// Replaces OLD_TAG with every tag in NEW_TAGS (two or more). Acts only on
    /// assets where OLD_TAG is a leaf — assets where OLD_TAG has descendants
    /// are skipped (use `tag rename` for cascading renames). Use `--keep` to
    /// keep OLD_TAG in place and add the new tags alongside (a pure
    /// "duplicate" / "copy" operation).
    ///
    /// Examples:
    ///   maki tag split "A & B" A B --apply
    ///   maki tag split "concert-jane-2024" "subject|performing arts|concert" "event|concert-jane-2024" --apply
    ///   maki tag split sunset "color|warm" --keep --apply
    Split {
        /// Current tag name. Accepts the same optional markers as `tag rename`
        /// (`=`, `/`, `^`). Split is always exact-tag-only, so `=` and `/`
        /// are redundant no-ops; `^` enables case-sensitive matching.
        old_tag: String,

        /// Target tag names (one or more). Always taken literally.
        #[arg(required = true, num_args = 1..)]
        new_tags: Vec<String>,

        /// Keep the source tag in place and only add the targets
        /// (additive / copy semantics instead of replace).
        #[arg(long)]
        keep: bool,

        /// Apply changes (default: report-only)
        #[arg(long)]
        apply: bool,
    },

    /// Remove all tags from an asset
    Clear {
        /// Asset ID (or unique prefix)
        asset_id: String,

        /// Only clear tags with this provenance source:
        /// user, xmp-import, auto-tag, or vlm.
        /// Without the flag, all tags are cleared.
        #[arg(long)]
        source: Option<String>,
    },

    /// Delete a tag from every asset that has it (cascades to descendants by default)
    Delete {
        /// Tag to delete. Use the same markers as `tag rename`:
        /// `=tag` or `/tag` for leaf-only (skip assets where the tag has descendants),
        /// `^tag` for case-sensitive match.
        tag: String,

        /// Apply changes (default: report-only)
        #[arg(long)]
        apply: bool,
    },

    /// Normalise tag values to Unicode NFC across the catalog (collapse NFC/NFD duplicates)
    #[command(name = "fix-unicode")]
    FixUnicode {
        /// Apply changes (default: report-only)
        #[arg(long)]
        apply: bool,
    },

    /// Scan all assets for malformed tags (leading/trailing whitespace,
    /// stray `|` separators, non-NFC byte sequences, control chars,
    /// empty segments). Read-only — reports asset IDs and the offending
    /// tag values so you can fix sidecars by hand or feed the IDs into
    /// `maki tag rename` / `maki tag delete --apply`. Equivalent check
    /// to what `normalize_tag_for_storage` applies at every tag write
    /// site; tags that round-trip through it unchanged are skipped.
    #[command(name = "scan")]
    Scan {
        /// Output JSON instead of human-readable lines.
        #[arg(long)]
        json: bool,
    },

    /// Expand all hierarchical tags to include ancestor paths
    #[command(name = "expand-ancestors")]
    ExpandAncestors {
        /// Search query to select assets (same syntax as `maki search`)
        #[arg(display_order = 1)]
        query: Option<String>,

        /// Limit to a specific asset
        #[arg(long, display_order = 10)]
        asset: Option<String>,

        /// Apply changes (default: report-only)
        #[arg(long, display_order = 20)]
        apply: bool,

        /// Asset IDs (for shell variable expansion)
        #[arg(hide = true, trailing_var_arg = true)]
        asset_ids: Vec<String>,
    },

    /// Export current tag tree as a vocabulary file
    #[command(name = "export-vocabulary")]
    ExportVocabulary {
        /// Output file (default: vocabulary.{yaml,txt,json} in catalog root, depending on --format)
        #[arg(long)]
        output: Option<String>,

        /// Output format: yaml (MAKI vocabulary, default), text (tab-indented keyword list for Lightroom/Capture One), or json (nested object with counts, for programmatic use)
        #[arg(long, default_value = "yaml")]
        format: String,

        /// Remove vocabulary entries that have no assets (only keep used tags)
        #[arg(long)]
        prune: bool,

        /// Export only the built-in default vocabulary (ignore catalog tags and existing vocabulary.yaml)
        #[arg(long)]
        default: bool,

        /// Annotate each tag with its asset count.
        ///
        /// YAML: emitted as a `# N assets` trailing comment (still valid YAML;
        /// MAKI's autocomplete loader ignores comments). JSON: every node has
        /// a `count` field already, so the flag is implied. Text format
        /// (Lightroom / Capture One keyword text) doesn't support comments —
        /// the flag is silently ignored to keep the file importable.
        #[arg(long)]
        counts: bool,
    },
}

#[derive(Subcommand)]
enum VolumeCommands {
    /// Register a new volume (LABEL PATH or just PATH to auto-derive label)
    Add {
        /// Label and path: "LABEL PATH" or just "PATH" (label derived from path)
        #[arg(required = true, num_args = 1..=2)]
        args: Vec<String>,

        /// Volume purpose (media, working, archive, backup, cloud)
        #[arg(long)]
        purpose: Option<String>,
    },

    /// List all volumes and their status
    List {
        /// Filter by volume purpose (media, working, archive, backup, cloud)
        #[arg(long)]
        purpose: Option<String>,

        /// Show only offline volumes
        #[arg(long)]
        offline: bool,

        /// Show only online volumes
        #[arg(long)]
        online: bool,
    },

    /// Set or clear the purpose of a volume
    SetPurpose {
        /// Volume label or UUID
        volume: String,

        /// Purpose (media, working, archive, backup, cloud) or "none" to clear
        purpose: String,
    },

    /// Remove a volume and all its locations, recipes, and orphaned assets
    Remove {
        /// Volume label or UUID
        volume: String,

        /// Actually remove (default is report-only)
        #[arg(long)]
        apply: bool,
    },

    /// Combine a source volume into a target volume, rewriting paths
    Combine {
        /// Source volume label or UUID (will be removed)
        source: String,

        /// Target volume label or UUID (receives locations/recipes)
        target: String,

        /// Actually combine (default is report-only)
        #[arg(long)]
        apply: bool,
    },

    /// Split a subdirectory from a volume into a new volume
    Split {
        /// Source volume label or UUID
        source: String,

        /// Label for the new volume
        new_label: String,

        /// Subdirectory path to split off (relative to volume mount point)
        #[arg(long)]
        path: String,

        /// Volume purpose for the new volume (media, working, archive, backup, cloud)
        #[arg(long)]
        purpose: Option<String>,

        /// Actually split (default is report-only)
        #[arg(long)]
        apply: bool,
    },

    /// Rename a volume
    Rename {
        /// Current volume label or UUID
        volume: String,

        /// New label
        new_label: String,
    },
}

#[derive(Subcommand)]
enum SavedSearchCommands {
    /// Save a search with a name
    Save {
        /// Name for this saved search
        name: String,

        /// Search query (same format as `maki search`)
        query: String,

        /// Sort order (e.g. date_desc, name_asc, size_desc)
        #[arg(long)]
        sort: Option<String>,

        /// Mark as favorite (shown as chip on browse page)
        #[arg(long)]
        favorite: bool,
    },

    /// List all saved searches
    List,

    /// Run a saved search by name
    Run {
        /// Name of the saved search to execute
        name: String,

        /// Output format: ids, short, full, json, or a custom template
        #[arg(long)]
        format: Option<String>,
    },

    /// Delete a saved search by name
    Delete {
        /// Name of the saved search to delete
        name: String,
    },
}

#[derive(Subcommand)]
enum CollectionCommands {
    /// Create a new collection
    Create {
        /// Collection name
        name: String,

        /// Optional description
        #[arg(long)]
        description: Option<String>,
    },

    /// List all collections
    List,

    /// Show collection contents
    Show {
        /// Collection name
        name: String,

        /// Output format: ids, short, full, json, or a custom template
        #[arg(long)]
        format: Option<String>,
    },

    /// Add assets to a collection
    Add {
        /// Collection name
        name: String,

        /// Asset IDs to add
        asset_ids: Vec<String>,
    },

    /// Remove assets from a collection
    Remove {
        /// Collection name
        name: String,

        /// Asset IDs to remove
        asset_ids: Vec<String>,
    },

    /// Delete a collection
    Delete {
        /// Collection name
        name: String,
    },
}

#[derive(Subcommand)]
enum StackCommands {
    /// Create a new stack from the given assets
    Create {
        /// Asset IDs to stack (first becomes pick)
        asset_ids: Vec<String>,
    },

    /// Add assets to an existing stack
    Add {
        /// Any asset already in the target stack
        reference: String,

        /// Asset IDs to add
        asset_ids: Vec<String>,
    },

    /// Remove assets from their stack
    Remove {
        /// Asset IDs to remove from stacks
        asset_ids: Vec<String>,
    },

    /// Set the pick (top) of a stack
    Pick {
        /// Asset ID to make the pick
        asset_id: String,
    },

    /// Dissolve an entire stack
    Dissolve {
        /// Any asset in the stack to dissolve
        asset_id: String,
    },

    /// List all stacks
    List,

    /// Show members of a stack
    Show {
        /// Any asset in the stack
        asset_id: String,

        /// Output format: ids, short, full, json, or a custom template
        #[arg(long)]
        format: Option<String>,
    },

    /// Convert matching tags into stacks
    FromTag {
        /// Tag pattern with {} as wildcard (e.g. "Aperture Stack {}")
        pattern: String,

        /// Remove the matched tag from ALL assets that carry it, including
        /// orphans (single-asset tags that can't form a stack) and already-
        /// stacked assets. Useful for cleaning up after a migration.
        #[arg(long)]
        remove_tags: bool,

        /// Actually create stacks (default: report only)
        #[arg(long)]
        apply: bool,
    },
}

#[cfg(feature = "ai")]
#[derive(Subcommand)]
enum FacesCommands {
    /// Detect faces in assets
    Detect {
        /// Search query to scope assets
        #[arg(long)]
        query: Option<String>,

        /// Process a specific asset (ID or prefix)
        #[arg(long)]
        asset: Option<String>,

        /// Limit to assets on a specific volume
        #[arg(long)]
        volume: Option<String>,

        /// Minimum detection confidence (0.0–1.0, default 0.5)
        #[arg(long, default_value = "0.5")]
        min_confidence: f32,

        /// Apply detections to catalog (default: report-only)
        #[arg(long)]
        apply: bool,

        /// Force re-detection even if faces already exist for an asset
        #[arg(long)]
        force: bool,
    },

    /// Download face detection and recognition models
    Download,

    /// Show face detection status
    Status,

    /// Auto-cluster unassigned faces into unnamed people
    Cluster {
        /// Search query to scope which assets' faces to cluster
        #[arg(long)]
        query: Option<String>,

        /// Process faces from a specific asset (ID or prefix)
        #[arg(long)]
        asset: Option<String>,

        /// Limit to faces on assets from a specific volume
        #[arg(long)]
        volume: Option<String>,

        /// Similarity threshold for clustering (0.0–1.0). Higher = stricter.
        /// Typical range: 0.55–0.75 for the bundled face model.
        #[arg(long)]
        threshold: Option<f32>,

        /// Skip face detections with confidence below this value (0.0–1.0).
        /// Low-quality detections (blurry, partial, profile) produce noisy
        /// embeddings that hurt clustering. Defaults to `[ai] face_min_confidence`
        /// from maki.toml (default 0.7).
        #[arg(long)]
        min_confidence: Option<f32>,

        /// Apply clustering (default: dry-run showing cluster sizes)
        #[arg(long)]
        apply: bool,
    },

    /// Delete face records that are not assigned to any person
    Clean {
        /// Apply the deletion (default: report-only)
        #[arg(long)]
        apply: bool,
    },

    /// Save aligned 112x112 face crops for visual debugging
    DumpAligned {
        /// Search query to scope which assets
        #[arg(long)]
        query: Option<String>,

        /// Specific asset (ID or prefix)
        #[arg(long)]
        asset: Option<String>,

        /// Output directory for aligned crops
        #[arg(long, default_value = "./aligned-faces")]
        output: PathBuf,

        /// Maximum number of faces to save (0 = unlimited)
        #[arg(long, default_value = "20")]
        limit: usize,
    },

    /// Analyze face embedding similarity distribution (diagnostic)
    Similarity {
        /// Search query to scope which assets' faces to analyze
        #[arg(long)]
        query: Option<String>,

        /// Analyze faces from a specific asset (ID or prefix)
        #[arg(long)]
        asset: Option<String>,

        /// Limit to faces on assets from a specific volume
        #[arg(long)]
        volume: Option<String>,

        /// Minimum confidence for faces to include
        #[arg(long, default_value = "0.0")]
        min_confidence: f32,

        /// Show top-N nearest neighbors for each face (0 = none, just summary)
        #[arg(long, default_value = "0")]
        top: usize,

        /// Include already-assigned faces (default: only unassigned)
        #[arg(long)]
        all: bool,
    },

    /// List all people
    People,

    /// Name (or rename) a person
    Name {
        /// Person ID (or prefix)
        person_id: String,

        /// Name to assign
        name: String,
    },

    /// Merge two people (move all faces from source to target)
    Merge {
        /// Target person ID (or prefix) — faces are moved here
        target_id: String,

        /// Source person ID (or prefix) — will be deleted after merge
        source_id: String,
    },

    /// Delete a person (unassigns all their faces)
    DeletePerson {
        /// Person ID (or prefix) to delete
        person_id: String,
    },

    /// Unassign a face from its person
    Unassign {
        /// Face ID (or prefix) to unassign
        face_id: String,
    },

    /// Export faces, people, and embeddings from SQLite to YAML + binary files
    Export,
}

/// AI utility commands. Grouped under `maki ai <subcommand>` so the
/// top-level help isn't cluttered with niche per-feature flags.
#[cfg(feature = "ai")]
#[derive(Subcommand)]
enum AiCommands {
    /// Export the AI vocabulary (labels + hierarchical mapping) as YAML.
    ///
    /// Without flags, emits the *active* vocabulary — whatever
    /// `[ai].labels` points to in `maki.toml`, or the built-in default
    /// when no labels file is configured. With `--default`, always
    /// emits the built-in default regardless of config.
    ///
    /// Use case: `maki ai export-vocabulary --default > my-labels.yaml`
    /// gives you a curated starting point; edit the right-hand sides
    /// to your own hierarchical taxonomy, point `[ai].labels` at the
    /// result, and Suggest tags / auto-tag will apply your mapping.
    ///
    /// When the active vocabulary is a legacy `.txt` flat-list file,
    /// the export converts it to YAML with identity mappings
    /// (`label: null`) so you can start adding hierarchy without
    /// reordering anything.
    #[command(name = "export-vocabulary")]
    ExportVocabulary {
        /// Output file. Default: stdout (use shell redirection to save).
        #[arg(long, short)]
        output: Option<String>,

        /// Emit the built-in default vocabulary instead of the active one.
        #[arg(long)]
        default: bool,
    },
}

#[derive(Subcommand)]
enum AudioCommands {
    /// Detect musical key and tempo of audio assets via external tools
    ///
    /// Runs the configured `[audio] key_command` (default: keyfinder-cli)
    /// and `[audio] bpm_command` (default: beat_this) on each matching
    /// audio asset and stores `audio_key` / `audio_bpm` in the catalog +
    /// sidecar, searchable via `key:` and `bpm:` filters. Explicit opt-in:
    /// key detection is only trustworthy on full mixes, not on voice
    /// recordings — scope the query accordingly. Missing tools are
    /// warned about and skipped.
    Analyze {
        /// Search query to scope the analysis (same syntax as maki search;
        /// `type:audio` is implied). Empty analyzes all audio assets.
        #[arg(default_value = "")]
        query: String,

        /// Re-analyze assets that already have key/BPM values
        #[arg(long)]
        force: bool,

        /// Report what would be analyzed without running the tools
        #[arg(long)]
        dry_run: bool,
    },
}

#[derive(Subcommand)]
enum TrashCommands {
    /// List every file currently in the trash
    List,

    /// Restore a trashed file to its original volume location
    ///
    /// PATH is the key shown by `maki trash list`
    /// (`<date>/<volume>/<relative-path>`). The volume must be online.
    /// The catalog is NOT updated — re-register the restored file with
    /// `maki import <path>` or `maki refresh` afterwards.
    Restore {
        /// Trash path (`<date>/<volume>/<relative-path>`)
        path: String,
    },

    /// Permanently delete trashed files
    ///
    /// Without flags, removes files older than `[trash] retention_days`
    /// (default 30). `--older-than` overrides the cutoff; `--all`
    /// empties everything.
    Empty {
        /// Remove files trashed at least this many days ago
        #[arg(long, conflicts_with = "all")]
        older_than: Option<u32>,

        /// Empty the entire trash regardless of age
        #[arg(long)]
        all: bool,

        /// Report what would be removed without deleting anything
        #[arg(long)]
        dry_run: bool,
    },
}

/// Print custom grouped help text through a pager.
/// Grouping for the `maki --help` overview, rendered from `Cli::command()`
/// so descriptions and aliases can never drift from clap. Every top-level
/// command must appear exactly once; `tests::help_groups_cover_every_subcommand`
/// enforces it. Feature-gated commands that are compiled out are skipped.
const HELP_GROUPS: &[(&str, &[&str])] = &[
    ("Setup", &["init", "volume"]),
    (
        "Ingest & Edit",
        &[
            "import", "watch", "delete", "tag", "edit", "group", "split", "auto-group", "audio",
            "describe", "auto-tag", "auto-stack", "embed", "faces", "ai",
        ],
    ),
    ("Organize", &["collection", "saved-search", "stack"]),
    (
        "Retrieve",
        &[
            "search", "show", "preview", "export", "contact-sheet", "duplicates", "stats",
            "status", "backup-status", "doc", "licenses",
        ],
    ),
    (
        "Maintain",
        &[
            "undo", "history", "doctor", "verify", "sync", "refresh", "sync-metadata",
            "writeback", "cleanup", "dedup", "trash", "relocate", "update-location",
            "generate-previews", "fix-roles", "fix-dates", "fix-recipes", "create-sidecars",
            "rebuild-catalog", "migrate",
        ],
    ),
    ("Interactive", &["serve", "shell"]),
];

/// Build the grouped `maki --help` overview from clap metadata.
fn render_help_overview() -> String {
    use clap::CommandFactory;
    use std::fmt::Write as _;

    let cmd = Cli::command();
    let version = env!("CARGO_PKG_VERSION");
    let edition = maki::features::edition();
    let ai_note = if maki::features::PRO { "" } else { "  (download MAKI Pro for: describe, auto-tag, auto-stack, embed, faces, writeback, sync-metadata)" };

    let mut help = format!("maki{edition} {version} — Media Asset Keeper & Indexer{ai_note}\n\nUsage: maki [OPTIONS] <COMMAND>\n");

    for (heading, names) in HELP_GROUPS {
        let _ = writeln!(help, "\n{heading}:");
        for name in *names {
            // Compiled out in this edition (feature-gated variant).
            let Some(sub) = cmd.find_subcommand(name) else { continue };
            let about = sub.get_about().map(|a| a.to_string()).unwrap_or_default();
            let aliases: Vec<&str> = sub.get_all_aliases().collect();
            let alias = if aliases.is_empty() {
                String::new()
            } else {
                format!("  [alias: {}]", aliases.join(", "))
            };
            let _ = writeln!(help, "  {name:<18} {about}{alias}");
        }
    }

    help.push_str("\nOptions:\n");
    let mut globals: Vec<&clap::Arg> = cmd.get_arguments().filter(|a| a.is_global_set()).collect();
    globals.sort_by_key(|a| a.get_display_order());
    for arg in globals {
        let Some(long) = arg.get_long() else { continue };
        let flags = match arg.get_short() {
            Some(short) => format!("-{short}, --{long}"),
            None => format!("    --{long}"),
        };
        let about = arg.get_help().map(|h| h.to_string()).unwrap_or_default();
        let _ = writeln!(help, "  {flags:<19}{about}");
    }
    help.push_str("  -h, --help         Print help (use <command> --help for details)\n");
    help.push_str("  -V, --version      Print version\n");
    help.push_str("\n  https://maki-dam.com — docs, downloads, and support\n");
    help
}

fn print_custom_help() {
    let help = render_help_overview();

    // Pipe through pager if stdout is a terminal
    if atty_stdout() {
        if let Ok(mut child) = std::process::Command::new("less")
            .args(["-FRSX"])
            .stdin(std::process::Stdio::piped())
            .spawn()
        {
            if let Some(ref mut stdin) = child.stdin {
                use std::io::Write;
                let _ = stdin.write_all(help.as_bytes());
            }
            let _ = child.wait();
            return;
        }
    }
    print!("{help}");
}

/// Check if stdout is a terminal.
fn atty_stdout() -> bool {
    use std::io::IsTerminal;
    std::io::stdout().is_terminal()
}

fn check_schema(root: &std::path::Path) {
    if let Ok(catalog) = Catalog::open(root) {
        if !catalog.is_schema_current() {
            eprintln!(
                "Error: catalog schema is outdated (v{}, expected v{}). Run `maki migrate` to update.",
                catalog.schema_version(),
                maki::catalog::SCHEMA_VERSION,
            );
            std::process::exit(1);
        }
    }
}

/// The catalog this invocation runs in: its root and the outcome of
/// reading its `maki.toml`. `None` outside a catalog (`init`, `licenses`,
/// `doc`, …). This is the one place the CLI reads the configuration from
/// disk; handlers take it from [`CliContext`].
fn resolve_catalog() -> Option<(PathBuf, anyhow::Result<CatalogConfig>)> {
    let root = maki::config::find_catalog_root().ok()?;
    let loaded = CatalogConfig::load(&root);
    Some((root, loaded))
}

/// Build the per-command context from the parsed global flags and the
/// resolved catalog.
fn build_context(cli: &Cli, catalog: Option<(PathBuf, anyhow::Result<CatalogConfig>)>) -> CliContext {
    let verbosity = maki::Verbosity::new(cli.verbose, cli.debug);
    let ctx = CliContext::new(cli.json, cli.log, verbosity);
    match catalog {
        Some((root, loaded)) => ctx.with_catalog(root, loaded),
        None => ctx,
    }
}

fn main() {
    // Intercept top-level --help / -h before clap parses
    let args: Vec<String> = std::env::args().collect();
    if args.len() == 2 && (args[1] == "--help" || args[1] == "-h") {
        print_custom_help();
        std::process::exit(0);
    }

    let mut cli = Cli::parse();
    let start = std::time::Instant::now();

    let catalog = resolve_catalog();

    // Merge [cli] defaults from maki.toml (if inside a catalog)
    if let Some((_, Ok(config))) = &catalog {
        cli.log = cli.log || config.cli.log;
        cli.timing = cli.timing || config.cli.time;
        cli.verbose = cli.verbose || config.cli.verbose;
    }

    // Handle shell command specially — it has its own loop
    if let Commands::Shell { script, command_str, strict } = &cli.command {
        let catalog_root = match catalog {
            Some((root, _)) => root,
            None => match maki::config::find_catalog_root() {
                Ok(r) => r,
                Err(e) => {
                    eprintln!("Error: {e:#}");
                    std::process::exit(1);
                }
            },
        };
        check_schema(&catalog_root);
        let opts = maki::shell::RunOptions {
            script: script.as_ref().map(PathBuf::from),
            command: command_str.clone(),
            strict: *strict,
        };
        // Each shell line is one invocation: it re-reads `maki.toml`, so
        // the shell's `reload` keeps its meaning.
        maki::shell::run(&catalog_root, opts, |args| {
            let shell_cli = Cli::try_parse_from(&args).map_err(|e| anyhow::anyhow!("{e}"))?;
            let ctx = build_context(&shell_cli, resolve_catalog());
            run_command(shell_cli, ctx)
        });
        return;
    }

    // Check schema version at startup (if inside a catalog).
    // Only `maki init` and `maki migrate` skip this check.
    if !matches!(cli.command, Commands::Init | Commands::Migrate) {
        if let Some((root, _)) = &catalog {
            check_schema(root);
        }
    }

    let timing = cli.timing;
    let ctx = build_context(&cli, catalog);
    let result = run_command(cli, ctx);

    if timing {
        eprintln!("Elapsed: {}", format_duration(start.elapsed()));
    }

    if let Err(e) = result {
        eprintln!("Error: {e:#}");
        std::process::exit(maki::error::exit_code_for(&e));
    }
}

/// Dispatch one parsed command to its handler. Returns the asset IDs the
/// command produced (`search` only; the shell's `_` variable).
fn run_command(cli: Cli, ctx: CliContext) -> anyhow::Result<Vec<String>> {
    let ctx = &ctx;
    let mut asset_ids: Vec<String> = Vec::new();

    let result: anyhow::Result<()> = (|| match cli.command {
        Commands::Init => run_init_command(ctx),
        Commands::Volume(cmd) => run_volume_command(cmd, ctx),
        Commands::Import {
            paths, volume, profile, include, skip, add_tags, dry_run, auto_group, smart,
            #[cfg(feature = "ai")] embed,
            #[cfg(feature = "pro")] describe,
        } => run_import_command(
            paths, volume, profile, include, skip, add_tags, dry_run, auto_group, smart,
            #[cfg(feature = "ai")] embed,
            #[cfg(feature = "pro")] describe,
            ctx,
        ),
        Commands::Watch { paths, volume, interval, once } => {
            run_watch_command(paths, volume, interval, once, ctx)
        }
        Commands::Search { query, format, quiet, image, limit } => {
            asset_ids = run_search_command(query, format, quiet, image, limit, ctx)?;
            Ok(())
        }
        Commands::Show { asset_id, locations } => run_show_command(asset_id, locations, ctx),
        Commands::Preview { asset_id } => run_preview_command(asset_id, ctx),
        Commands::Tag { asset_id, remove, tags, subcmd } => {
            run_tag_command(asset_id, remove, tags, subcmd, ctx)
        }
        Commands::Edit {
            asset_id, name, clear_name, description, clear_description, rating, clear_rating,
            label, clear_label, clear_tags, date, clear_date, role, variant,
        } => run_edit_command(
            asset_id, name, clear_name, description, clear_description, rating, clear_rating,
            label, clear_label, clear_tags, date, clear_date, role, variant, ctx,
        ),
        Commands::Group { variant_hashes } => run_group_command(variant_hashes, ctx),
        Commands::Split { asset_id, variant_hashes } => {
            run_split_command(asset_id, variant_hashes, ctx)
        }
        Commands::Delete { asset_ids, apply, remove_files, no_trash } => {
            run_delete_command(asset_ids, apply, remove_files, no_trash, ctx)
        }
        #[cfg(feature = "pro")]
        Commands::Describe {
            query, asset, volume, model, endpoint, prompt, max_tokens, timeout, mode,
            temperature, num_ctx, top_p, top_k, repeat_penalty, apply, force, dry_run, check,
            asset_ids,
        } => run_describe_command(
            query, asset, volume, model, endpoint, prompt, max_tokens, timeout, mode,
            temperature, num_ctx, top_p, top_k, repeat_penalty, apply, force, dry_run, check,
            asset_ids, ctx,
        ),
        #[cfg(feature = "ai")]
        Commands::AutoTag {
            query, asset, volume, model, threshold, labels, apply, download, remove_model,
            list_models, list_labels, similar, asset_ids,
        } => run_auto_tag_command(
            query, asset, volume, model, threshold, labels, apply, download, remove_model,
            list_models, list_labels, similar, asset_ids, ctx,
        ),
        #[cfg(feature = "ai")]
        Commands::AutoStack { query, threshold, min_size, apply, asset_ids } => {
            run_auto_stack_command(query, threshold, min_size, apply, asset_ids, ctx)
        }
        #[cfg(feature = "ai")]
        Commands::Embed { query, asset, volume, model, force, export, asset_ids } => {
            run_embed_command(query, asset, volume, model, force, export, asset_ids, ctx)
        }
        #[cfg(feature = "ai")]
        Commands::Faces(cmd) => run_faces_command(cmd, ctx),
        #[cfg(feature = "ai")]
        Commands::Ai(cmd) => run_ai_command(cmd, ctx),
        Commands::AutoGroup { query, apply, global } => {
            run_auto_group_command(query, apply, global, ctx)
        }
        Commands::Relocate { asset_ids, target, query, remove_source, create_sidecars, dry_run } => {
            run_relocate_command(asset_ids, target, query, remove_source, create_sidecars, dry_run, ctx)
        }
        Commands::Undo { dry_run, force } => run_undo_command(dry_run, force, ctx),
        Commands::History { asset, limit } => run_history_command(asset, limit, ctx),
        Commands::Doctor { sample, repair } => run_doctor_command(sample, repair, ctx),
        Commands::Verify { paths, volume, asset, include, skip, max_age, force } => {
            run_verify_command(paths, volume, asset, include, skip, max_age, force, ctx)
        }
        Commands::Sync { paths, volume, apply, remove_stale } => {
            run_sync_command(paths, volume, apply, remove_stale, ctx)
        }
        #[cfg(feature = "pro")]
        Commands::SyncMetadata { query, volume, asset, dry_run, media, asset_ids } => {
            run_sync_metadata_command(query, volume, asset, dry_run, media, asset_ids, ctx)
        }
        Commands::Refresh { paths, volume, asset, dry_run, media, reimport, exif_only } => {
            run_refresh_command(paths, volume, asset, dry_run, media, reimport, exif_only, ctx)
        }
        #[cfg(feature = "pro")]
        Commands::Writeback {
            query, volume, asset, all, force, mirror_tags, embed, no_trash, dry_run, asset_ids,
        } => run_writeback_command(
            query, volume, asset, all, force, mirror_tags, embed, no_trash, dry_run, asset_ids, ctx,
        ),
        Commands::Cleanup { volume, path, list, apply } => {
            run_cleanup_command(volume, path, list, apply, ctx)
        }
        Commands::Dedup { volume, prefer, filter_format, path, min_copies, apply, no_trash } => {
            run_dedup_command(volume, prefer, filter_format, path, min_copies, apply, no_trash, ctx)
        }
        Commands::Trash { command } => run_trash_command(command, ctx),
        Commands::Audio { command } => match command {
            AudioCommands::Analyze { query, force, dry_run } => {
                run_audio_analyze_command(query, force, dry_run, ctx)
            }
        },
        Commands::UpdateLocation { asset_id, from, to, volume } => {
            run_update_location_command(asset_id, from, to, volume, ctx)
        }
        Commands::Duplicates { format, same_volume, cross_volume, volume, filter_format, path } => {
            run_duplicates_command(format, same_volume, cross_volume, volume, filter_format, path, ctx)
        }
        Commands::GeneratePreviews { paths, volume, asset, include, skip, force, upgrade, smart } => {
            run_generate_previews_command(paths, volume, asset, include, skip, force, upgrade, smart, ctx)
        }
        Commands::FixRoles { paths, volume, asset, apply } => {
            run_fix_roles_command(paths, volume, asset, apply, ctx)
        }
        Commands::FixDates { query, volume, asset, apply, asset_ids } => {
            run_fix_dates_command(query, volume, asset, apply, asset_ids, ctx)
        }
        Commands::FixRecipes { query, volume, asset, apply, asset_ids } => {
            run_fix_recipes_command(query, volume, asset, apply, asset_ids, ctx)
        }
        Commands::CreateSidecars { query, volume, asset, apply, asset_ids } => {
            run_create_sidecars_command(query, volume, asset, apply, asset_ids, ctx)
        }
        Commands::RebuildCatalog { asset } => run_rebuild_catalog_command(asset, ctx),
        Commands::Licenses { summary } => run_licenses_command(summary, ctx),
        Commands::Doc { document } => run_doc_command(document, ctx),
        Commands::Serve { port, bind, read_only } => run_serve_command(port, bind, read_only, ctx),
        Commands::ContactSheet {
            query, output, layout, columns, rows, paper, landscape, title, fields, sort, no_smart,
            group_by, margin, label_style, quality, copyright, dry_run,
        } => run_contact_sheet_command(
            query, output, layout, columns, rows, paper, landscape, title, fields, sort, no_smart,
            group_by, margin, label_style, quality, copyright, dry_run, ctx,
        ),
        Commands::Export {
            query, target, layout, symlink, all_variants, include_sidecars, previews,
            smart_previews, dry_run, overwrite, zip,
        } => run_export_command(
            query, target, layout, symlink, all_variants, include_sidecars, previews,
            smart_previews, dry_run, overwrite, zip, ctx,
        ),
        Commands::Stats { types, volumes, tags, verified, all, limit } => {
            run_stats_command(types, volumes, tags, verified, all, limit, ctx)
        }
        Commands::Status { min_copies } => run_status_command(min_copies, ctx),
        Commands::BackupStatus { query, at_risk, min_copies, volume, format, quiet } => {
            run_backup_status_command(query, at_risk, min_copies, volume, format, quiet, ctx)
        }
        Commands::Collection(cmd) => run_collection_command(cmd, ctx),
        Commands::Stack(cmd) => run_stack_command(cmd, ctx),
        Commands::SavedSearch(cmd) => run_saved_search_command(cmd, ctx),
        Commands::Migrate => run_migrate_command(ctx),
        Commands::Shell { .. } => {
            unreachable!("Shell command is handled before run_command")
        }
    })();

    result.map(|()| asset_ids)
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    /// `Cli::command()` for the full-feature build is deep enough to blow
    /// the 2 MB default test-thread stack in debug builds; run clap-heavy
    /// checks on a roomier thread.
    fn with_big_stack(f: impl FnOnce() + Send + 'static) {
        std::thread::Builder::new()
            .stack_size(32 * 1024 * 1024)
            .spawn(f)
            .expect("spawn test thread")
            .join()
            .expect("test thread panicked");
    }

    fn subcommand_names() -> Vec<String> {
        Cli::command().get_subcommands().map(|c| c.get_name().to_string()).collect()
    }

    /// Every clap subcommand is listed exactly once in `HELP_GROUPS`, and
    /// (in the full-feature build) every listed name is a real subcommand.
    #[test]
    fn help_groups_cover_every_subcommand() {
        with_big_stack(|| {
            let listed: Vec<&str> = HELP_GROUPS.iter().flat_map(|(_, names)| names.iter().copied()).collect();
            let mut deduped = listed.clone();
            deduped.sort_unstable();
            deduped.dedup();
            assert_eq!(listed.len(), deduped.len(), "HELP_GROUPS lists a command twice");

            for name in subcommand_names() {
                assert!(listed.contains(&name.as_str()), "`{name}` is missing from HELP_GROUPS");
            }
            if maki::features::PRO {
                let names = subcommand_names();
                for name in &listed {
                    assert!(names.iter().any(|n| n == name), "HELP_GROUPS lists unknown command `{name}`");
                }
            }

            let help = render_help_overview();
            for name in subcommand_names() {
                assert!(help.contains(&format!("\n  {name:<18} ")), "`{name}` not rendered in --help");
            }
            assert!(help.starts_with("maki"));
            assert!(help.ends_with("  https://maki-dam.com — docs, downloads, and support\n"));
        });
    }

    /// First positional of a subcommand, if any.
    fn first_positional(sub: &clap::Command) -> Option<&clap::Arg> {
        sub.get_positionals().next()
    }

    /// `shell::SINGLE_ASSET_COMMANDS` = every command whose first positional
    /// is a single `asset_id` (`tag` keeps it optional because of its subcommands).
    #[test]
    fn shell_single_asset_commands_match_clap() {
        with_big_stack(|| {
            let cmd = Cli::command();
            let mut expected: Vec<&str> = cmd
                .get_subcommands()
                .filter(|sub| {
                    first_positional(sub).is_some_and(|arg| {
                        arg.get_id() == "asset_id"
                            && arg.get_num_args().is_none_or(|n| n.max_values() == 1)
                    })
                })
                .map(|sub| sub.get_name())
                .collect();
            expected.sort_unstable();
            let mut actual = maki::shell::SINGLE_ASSET_COMMANDS.to_vec();
            actual.sort_unstable();
            assert_eq!(actual, expected, "shell::SINGLE_ASSET_COMMANDS drifted from clap");
        });
    }

    /// `shell::QUERY_POSITIONAL_COMMANDS` = every command whose first
    /// positional is a required `query` string.
    #[test]
    fn shell_query_positional_commands_match_clap() {
        with_big_stack(|| {
            let cmd = Cli::command();
            let mut expected: Vec<&str> = cmd
                .get_subcommands()
                .filter(|sub| {
                    first_positional(sub).is_some_and(|arg| arg.get_id() == "query" && arg.is_required_set())
                })
                .map(|sub| sub.get_name())
                .collect();
            expected.sort_unstable();
            let mut actual = maki::shell::QUERY_POSITIONAL_COMMANDS.to_vec();
            actual.sort_unstable();
            assert_eq!(actual, expected, "shell::QUERY_POSITIONAL_COMMANDS drifted from clap");
        });
    }

    /// `shell::SETTABLE_FLAGS` = the global `--long` flags of `Cli`.
    #[test]
    fn shell_settable_flags_match_clap_globals() {
        with_big_stack(|| {
            let cmd = Cli::command();
            let mut expected: Vec<String> = cmd
                .get_arguments()
                .filter(|a| a.is_global_set())
                .filter_map(|a| a.get_long().map(|l| format!("--{l}")))
                .collect();
            expected.sort_unstable();
            let mut actual: Vec<String> = maki::shell::SETTABLE_FLAGS.iter().map(|s| s.to_string()).collect();
            actual.sort_unstable();
            assert_eq!(actual, expected, "shell::SETTABLE_FLAGS drifted from clap globals");
        });
    }

    /// `shell::SUBCOMMANDS` = every clap subcommand except the blocked ones.
    #[test]
    fn shell_completion_subcommands_match_clap() {
        with_big_stack(|| {
            let mut expected: Vec<String> = subcommand_names()
                .into_iter()
                .filter(|n| !maki::shell::BLOCKED_COMMANDS.contains(&n.as_str()))
                .collect();
            expected.sort_unstable();
            let actual: Vec<String> = maki::shell::SUBCOMMANDS.iter().map(|s| s.to_string()).collect();
            if maki::features::PRO {
                assert_eq!(actual, expected, "shell::SUBCOMMANDS drifted from clap");
            } else {
                // The shell lives in the library and lists the gated commands
                // unconditionally; a lean build can only check coverage.
                for name in &expected {
                    assert!(actual.contains(name), "`{name}` missing from shell::SUBCOMMANDS");
                }
            }
        });
    }
}
