# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

A digital asset manager designed for large collections of images and videos (terabytes across multiple offline storage devices). Content-addressable storage, text-based metadata in sidecar files, variant grouping, deduplication, transparent file relocation, recipe management, and location-independent navigation.

## Technology

- **Language**: Rust
- **Platforms**: macOS, Linux, Windows
- **Interface**: CLI-first (`maki` command), web UI (`maki serve`)
- **Catalog**: SQLite (cache/index), YAML sidecar files (source of truth)
- **Key crates**: clap, rustyline (shell), anyhow, thiserror, chrono, sha2, serde, serde_norway (YAML sidecars), serde_json, rusqlite, kamadak-exif, quick-xml, regex, unicode-normalization (NFC tag normalization), image, imageproc, ab_glyph, lofty, printpdf (contact sheets), zip (export), uuid, axum, askama, tokio, tokio-stream (SSE jobs), tower-http, toml, toml_edit, schemars, glob-match; **optional (ai feature)**: ort, ndarray, tokenizers; **optional (ai-gpu feature)**: ort with CoreML execution provider
- **External tools**: dcraw or dcraw_emu/libraw (RAW previews), ffmpeg (video thumbnails, audio waveforms), ffprobe (video metadata: duration, codec, resolution, framerate; included with ffmpeg), curl (AI model download, VLM calls), keyfinder-cli + beat_this (optional: `audio analyze` key/BPM detection, commands configurable via `[audio]`), platform opener `open`/`xdg-open`/`explorer` (reveal/open actions; Windows uses `cmd /c start`), Linux "open terminal" tries `x-terminal-emulator`/`gnome-terminal`/`konsole`/`xterm` in that order, `which`/`where` (tool availability probing), `less` (help pager)

## Architecture

Core layers: CLI → Core Library (Asset Service, Content Store, Metadata Store, Device Registry, Query Engine, Preview Generator) → Storage (Local Catalog + Media Volumes).

See `doc/architecture-overview.md` for design decisions, `doc/component-specification.md` for data model, components, routes, and directory structure.

## Implemented Commands

53 top-level commands (95 leaf commands counting subcommands; 96 if `tag` itself, which takes arguments as well as subcommands, is counted): `init`, `volume` (add/list/combine/split/rename/remove/set-purpose), `import`, `watch`, `delete`, `export`, `contact-sheet`, `describe`, `search`, `show`, `preview`, `tag` (rename/split/delete/clear [`--source` provenance filter]/scan/expand-ancestors/fix-unicode/export-vocabulary), `edit`, `group`, `split`, `auto-group`, `auto-tag`, `auto-stack`, `embed`, `ai` (export-vocabulary), `audio` (analyze), `faces` (detect/download/status/cluster/clean/similarity/dump-aligned/people/name/merge/delete-person/unassign/export), `stack` (create/add/remove/pick/dissolve/list/show/from-tag), `duplicates`, `dedup`, `trash` (list/restore/empty), `generate-previews`, `fix-roles`, `fix-dates`, `fix-recipes`, `create-sidecars`, `rebuild-catalog`, `undo`, `history`, `doctor`, `migrate`, `relocate`, `update-location`, `verify`, `sync`, `sync-metadata`, `refresh`, `cleanup`, `writeback`, `backup-status`, `status`, `stats`, `serve`, `doc`, `licenses`, `saved-search` (save/list/run/delete), `collection` (create/list/show/add/remove/delete), `shell`

See `doc/specification.md` for detailed command behavior, flags, and search filter documentation.

## Key Patterns

### Dual Storage
YAML sidecars are the source of truth; SQLite catalog is a derived cache rebuilt via `maki rebuild-catalog`. All write paths must update both stores, sidecar first, SQLite rows in one `Catalog::with_tx`. Never `let _ =` a store write: propagate it, or push it into the operation's `errors` vector; only best-effort removal of derived files (previews, crops, `.bin`) may be ignored, with a comment.

### Tag Provenance
`Asset.tag_sources: BTreeMap<String, TagSource>` maps tag value → source (`user`/`xmp-import`/`auto-tag`/`vlm`); a tag absent from the map is `user`. First source wins (an existing tag keeps its source), except that an explicit human add (`QueryEngine::tag`, source `user`) promotes an existing machine tag and its ancestors to `user` via `Asset::promote_tags_to_user` (`TagResult.promoted`; no XMP write-back). Mutate tags only via `Asset::add_tags_with_source`/`remove_tags`/`rename_tag_value`/`promote_tags_to_user` (keeps the map consistent; `MetadataStore::save` self-heals stale entries). Mirrored in SQLite `assets.tag_sources` (schema v10), checked by doctor.

### Schema Migrations
- **Version-guarded, per-version transactions**: `run_migrations() -> Result<()>` reads the stored version once and runs one `migrate_vN()` per version `> current`, each inside `with_tx` with the `schema_version` stamp written in the same transaction. A failing step rolls that version back and the error names it (`schema migration v12: backfill assets_tags_fts`); `maki migrate` prints it. On an up-to-date catalog, startup is a single SELECT query.
- Idempotent by construction: `add_column_if_missing(table, column, decl)` (via `pragma_table_info`), `table_exists`, `has_column`, and `CREATE … IF NOT EXISTS`. No `let _ =` in migrations — a statement that may legitimately fail needs a guard, not a swallowed error.
- Backfill only rows that will actually change: `WHERE column IS NULL` at minimum, narrower when possible (v11: `asset_type='audio' AND EXISTS(... instr(...))`) — every row an `UPDATE assets` touches re-tokenises its FTS rows through the v9 and v12 triggers, even when the value does not change
- All DDL lives in `schema.rs`, feature-independent (v13 added faces/people/embeddings and the stack/collection tables; there are no lazy `*Store::initialize` calls any more). `schema_version` table with `SCHEMA_VERSION` constant in `catalog.rs`. All commands (except `init`/`migrate`) check version at startup; exit with error if outdated
- `initialize()` creates base tables then delegates to `run_migrations()` for all columns, indexes, backfills, and version stamping
- Bump `SCHEMA_VERSION` whenever a migration is added; add a new `migrate_vN` and register it in `run_migrations()`

### Denormalized Columns on `assets` Table
`best_variant_hash`, `primary_variant_format`, `variant_count`, `face_count`, `face_scan_status`, `leaf_tag_count`, `stack_id`, `stack_position`, `latitude`, `longitude`, `preview_rotation`, `preview_variant`, `tag_sources`, `duration_seconds` (audio+video), `video_codec`, `audio_sample_rate`/`audio_channels`/`audio_bitrate`/`audio_key`/`audio_bpm` — computed at write time, must be updated in all write paths (`insert_asset`, `update_denormalized_variant_columns`, `fix_roles`, `StackStore` operations, `FaceStore::update_face_count`).

### SQLite Connection Pool (Web Server)
`CatalogPool` holds pre-opened connections (RAII `PooledCatalog` returned on drop), opened with `Catalog::open_fast()` — an alias of `open()`: pragmas only, no migrations. Nothing migrates at serve startup: the CLI entry point only checks the stored schema version and exits if outdated; migrations run in `init`, `migrate`, and `rebuild-catalog`.

### Tag Filter Index
`tag:` LIKE/GLOB patterns run against `assets_tags_fts` (FTS5 trigram copy of `assets.tags`, schema v12, three triggers on `assets`) via `a.id IN (SELECT asset_id FROM assets_tags_fts WHERE …)` when a bounded probe (`plan_tag_index`, `TAG_INDEX_MAX_MATCHES` = 10k, memoised per connection) shows the entry is selective; broader entries keep the inline `a.tags LIKE` scan. Same semantics either way; `tag_like_parts` takes the column name. Its triggers (like the v9 `assets_fts` ones) fire per row on any `UPDATE assets`, so migration backfills must keep their WHERE guards.

### Conditional JOINs in Search
`build_search_where()` returns `(where, params, needs_fl_join, needs_v_join)` — variant/location tables only joined when filters need them.

### XMP Write-Back
Rating, tag, description, and label changes are written back to `.xmp` recipe files on disk. After writing, the file is re-hashed and the recipe's `content_hash` is updated in both SQLite and YAML sidecar. Offline volumes are silently skipped with `pending_writeback=true` flag. `maki writeback` replays pending writes when volumes come online. `maki writeback --embed` instead splices the metadata into JPEG variant files' embedded APP1 XMP segment (trash-copy → temp-file → verify → atomic-rename safety ladder, then `Catalog::migrate_variant_hash` migrates the changed content-hash identity across SQLite, sidecar, and preview files).

### Edit-History Journal (undo / history)
`<catalog>/history/<epoch-millis>-<opid>.json` — one file per metadata-edit operation, backing `maki undo` (LIFO; undone ops move to `history/undone/`) and `maki history`. An **independent non-authoritative log**: NOT a sidecar (not source of truth), NOT in `catalog.db` (not derived cache). Survives `rebuild-catalog`, ignored by `maki doctor`, freely deletable (loses only undo capability). Recording happens at the QueryEngine mutation layer via an optional `BatchContext.recorder` (so CLI/web/shell edits all journal; a batch = one operation). `restore_asset` undoes by writing the prior full `Asset` state back through sidecar + `insert_asset` + `insert_recipe` + denormalized columns, flagging xmp recipes `pending_writeback` (XMP files reconciled later by `maki writeback`, never live during undo). Config `[history]` (`enabled`, `max_operations`). v4.9.0 covers field-level edits (rating/label/description/name/date/tags single+batch); source labeled `cli` for all surfaces (web/shell threading + structural-op undo are follow-ons).

### Feature Gates
- `--features pro`: MAKI Pro edition (includes `ai`, plus "Pro" branding, VLM describe, writeback, sync-metadata, and future pro-only features)
- `--features ai`: SigLIP embeddings, auto-tag, face detection/recognition, stroll page, similarity search, text-to-image search, search by query image (`search --image`, web `similar:@<token>` sessions in `src/query_image.rs`)
- `--features ai-gpu`: CoreML execution provider on macOS (additive to `ai`)

### Output Formatting Conventions
- All commands support `--json` (structured JSON to stdout, messages to stderr)
- Multi-file commands support `--log` (per-file progress to stderr)
- `--verbose` shows operational flow (file counts, volume detection, VLM settings); `--debug` shows external tool stderr and implies `--verbose`; `--time` shows elapsed time
- `search --format` supports presets (`ids`, `short`, `full`, `json`) and custom templates (`'{id}\t{name}'`)

### Shared Helpers (use these, do not re-implement)
- `CliContext { json, log, verbosity, config, catalog root }` (`cli_output.rs`) is the single trailing parameter (`&CliContext`) of every `run_*_command`; `ctx.emit(&value, || human)` for plain JSON-or-human output. `maki --help` is rendered from clap metadata via `HELP_GROUPS` (unit-tested against `Cli::command()`).
- `maki.toml` is loaded once per invocation in `main.rs` and travels in `CliContext` (`ctx.catalog()` → root + config); `QueryEngine::with_config` / `with_catalog(root, config, conn)` and `AssetService::with_config` take the `Arc<CatalogConfig>` (their `new` constructors are test-only); `HistoryStore::new_with` / `Trash::new_with`. `CatalogConfig::load` belongs only in `main.rs`, `config.rs` and the web config editor.
- `QueryEngine` holds one `Catalog` connection (lent via `with_catalog`, else opened lazily; `engine.catalog()`); `src/query.rs` is types + constructors, the `impl` blocks live per concern in `src/query/{search,grouping,tags,fields,reimport,context,writeback,writeback_embed}.rs`. All field setters and `edit` go through `mutate_asset`; `MutationContext` (alias `BatchContext`) is built only by `build_context`. Search on a connection you already hold with `query::search_in`.
- Long-running service operations take a request struct and emit typed events (`DescribeRequest`/`DescribeEvent` → `describe_with`, `PreviewOptions`/`PreviewEvent` → `generate_previews`, `WritebackRequest`/`WritebackEvent`, `DedupOptions`, `AutoGroupScope`); CLI and web are adapters that render the events — no `log: bool` parameters or string callbacks in the library. Branding/edition checks use `features::{AI, PRO, edition()}`; gate implementations with `cfg(feature)`, not data types.
- `Catalog::with_tx(|c| …)` around every write that touches more than one row (commit on `Ok`, rollback on `Err`, nests). Writer order: compute the new `Asset` in memory → save the sidecar → all SQLite rows in one `with_tx` → non-transactional side effects (files, previews, history). `Catalog::resync_asset_from_sidecar(&asset)` rebuilds every derived row set of one asset (doctor, reimport, undo, per-asset rebuild all use it); `Catalog::delete_asset_rows(id)` is the one FK-ordered full removal.
- `error::MakiError` (`NotFound{kind,key,hint}`, `Offline`, `Conflict`, `InvalidInput`, `CheckFailed`) is raised at the engine for outcomes callers act on; `anyhow` stays the carrier. `error::classify` walks the context chain; `exit_code_for` gives the CLI exit code (3 not found, 4 offline), `http_status_for` the web status — never match on error text.
- `yaml_file::{load_or_default, save_atomic}` for every catalog-level YAML file; stores own their persistence: `FaceStore::open(&catalog, root)` / `StackStore::open` / `CollectionStore::open` are the mutating handles (SQLite + YAML export in one transaction per mutator; batch primitives end with one `persist()`), `::new(&catalog)` is read-only (`EmbeddingStore::new(&catalog)` likewise). `Catalog::conn()` is `pub(crate)`: the binary (`main.rs`, `commands/`) never sees a connection, and new SQL belongs in a named `Catalog` method (`src/catalog/<section>.rs`), not at the call site. `Catalog::with_tx` and the stores share `tx::run`; a store's YAML export requested inside a transaction (`tx::export_at_commit`) is written once, by the outermost transaction just before COMMIT — never write a store YAML file directly inside a transaction.
- `DeviceRegistry::online_map()` / `label_map()`; `Catalog::resolve_asset_id_required(prefix)` (one error text); `session_root::find_session_root` (also behind the SQL `session_root()` function); `path_util::forward_slashes` (every volume-relative path string); `platform::{open_path, reveal_path, open_terminal, open_url}` (all OS-specific process spawning).
- Web: one `ServeOptions` → `AppState::new` / `web::serve`; `AppState::config()` (swapped on config-editor save), `query_engine()` (`Result`, connection from the pool), `embedding_index()`, `with_siglip()`; `run_browse_search` for every browse-derived endpoint; `jobs::launch_job(state, kind, LaunchOptions, run)` for every background job; `routes::spawn_catalog_blocking` / `spawn_blocking_with` for every blocking handler body.
- Front end: `src/web/static/maki-core.js` (embedded by `base.html`) owns `MAKI.escHtml/escAttr/toast/api/dialog/pathAutocomplete/attachJobStream/quoteFilterValue` and the colour-label maps; the vocabulary itself is `models::asset::COLOR_LABELS`.

### Test Helpers
CLI integration tests live in `tests/cli/` (one module per command area under `main.rs`, shared helpers in `common.rs`, one binary: `cargo test --test cli`). Catalog tests sit in the `#[cfg(test)] mod tests` of the section file whose functions they exercise (`src/catalog/<section>.rs`); fixtures used by more than one section (`setup_search_catalog()`, `setup_metadata_catalog()`, `catalog_with_tags()`) are in `src/catalog/test_support.rs`. `asset.variants` must be populated before `catalog.insert_asset()` (because of denormalized columns). Service-operation tests use `ServiceEnv` from `src/asset_service/test_support.rs` (temp catalog + online volume + service; `env.add(rel, content)`). Unit tests share `src/test_support.rs` (`TempCatalog::new()` + `cat.seed(test_asset(hash).file(..).tags(..))`, `memory_catalog()`, `insert_rows`, `test_variant`); `query/test_support.rs` re-exports it and adds the query fixtures. Build test assets with these, not hand-written `Variant { … }` literals.

## Configuration (`maki.toml`)

All sections optional; missing fields use defaults. Top-level `default_volume` (UUID). Sections: `[preview]` (max_edge, format, quality, smart_max_edge, smart_quality, generate_on_demand), `[serve]` (port, bind, per_page, stroll_neighbors, stroll_neighbors_max, stroll_fanout, stroll_fanout_max, stroll_discover_pool, read_only, username, password), `[import]` (exclude, auto_tags, smart_previews, embeddings, descriptions, profiles via `[import.profiles.<name>]`), `[dedup]` (prefer), `[verify]` (max_age_days), `[ai]` (model, threshold, labels, model_dir, prompt, execution_provider, face_cluster_threshold, face_min_confidence, text_limit), `[vlm]` (endpoint, model, max_tokens, prompt, timeout, temperature, mode, concurrency, max_image_edge, models, num_ctx, top_p, top_k, repeat_penalty; per-model overrides via `[vlm.model_config."name"]`), `[contact_sheet]` (layout, paper, fields, margin, quality, label_style, copyright), `[browse]` (default_filter, slideshow_seconds, slideshow_loop, remember_latest_filter), `[writeback]` (enabled, mirror_tags, sidecar_naming — `stem`/`full` name for created `.xmp` files, create_sidecars — auto-create on first edit when enabled), `[trash]` (enabled, retention_days), `[history]` (enabled, max_operations — edit-history journal for undo/history), `[cli]` (log, time, verbose — default global flags), `[group]` (session_root_pattern — regex for auto-group session root detection), `[watch]` (poll_seconds, exclude — additive to `[import]` exclude), `[audio]` (key_command, bpm_command — external analyzers for `maki audio analyze`). Config structs: `CatalogConfig` in `src/config.rs` with sub-structs.

See `doc/manual/reference/08-configuration.md` for full documentation of every option.

## Documentation References

| Topic | Document |
|-------|----------|
| Feature spec & command behavior | `doc/specification.md` |
| Architecture & design decisions | `doc/architecture-overview.md` |
| Data model, components, routes | `doc/component-specification.md` |
| User manual (guides + reference) | `doc/manual/index.md` |
| Search filters reference | `doc/manual/reference/06-search-filters.md` |
| Configuration reference | `doc/manual/reference/08-configuration.md` |
| Roadmap & proposals | `doc/proposals/roadmap.md` |
