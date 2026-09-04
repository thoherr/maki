# Component Specification

> **As of v4.10.0 (September 2026).** Reviewed against the code at that
> version: the data model below matches `src/models/*.rs` and the SQLite
> schema in `src/catalog/schema.rs` (schema v11), the routes list is
> generated from `build_router` in `src/web/mod.rs`, and the config
> section mirrors `CatalogConfig` in `src/config.rs`. Per-release
> narrative lives in `CHANGELOG.md` and `doc/specification.md`; the user
> manual under `doc/manual/` is the reference for command flags and
> search filters.

## Data Model

### Asset
The central entity. Represents a logical asset (e.g. "photo of sunset at beach, 2024-07-15").

**Sidecar fields** (source of truth, `src/models/asset.rs`, persisted to the YAML sidecar):

| Field | Type | Description |
|---|---|---|
| id | UUID | Stable identifier (UUID v5 of the first variant's content hash) |
| name | Option<String> | User-assigned name |
| created_at | DateTime | Capture date when known (EXIF/XMP), else first import |
| asset_type | Enum | Image, Video, Audio, Document, Other |
| tags | Vec<String> | Hierarchical tags (`a|b|c`) |
| tag_sources | BTreeMap<String, TagSource> | Tag provenance: `user` / `xmp-import` / `auto-tag` / `vlm`; a tag absent from the map is `user`. Mutated only through `add_tags_with_source` / `remove_tags` / `rename_tag_value` |
| description | Option<String> | Free-text description |
| rating | Option<u8> | User/XMP rating 1–5, or unset |
| color_label | Option<String> | Red, Orange, Yellow, Green, Blue, Pink, Purple, or unset |
| face_scan_status | Option<String> | Face detection bookkeeping (`scanned` once `faces detect` ran, even with zero faces) so detection does not rescan |
| preview_rotation | Option<u16> | Manual preview rotation override (0/90/180/270) |
| preview_variant | Option<String> | Content hash of the variant to render previews from, overriding the automatic best-variant choice |
| variants | Vec<Variant> | The files of this asset |
| recipes | Vec<Recipe> | Processing sidecars attached to the variants |

**Denormalized columns** on the SQLite `assets` table (derived cache, computed at write time in `insert_asset` and kept current by every write path — see `CLAUDE.md`):

| Column | Since | Description |
|---|---|---|
| best_variant_hash | v1 | Content hash of the best display variant (Export > Processed > Original, image formats preferred, size tiebreak) |
| primary_variant_format | v1 | Identity format of the asset (Original+RAW first, then Original+any, then best variant) |
| variant_count | v1 | Number of variants |
| stack_id, stack_position | v1 | Stack membership; position 0 is the pick |
| latitude, longitude | v1 | GPS from variant metadata (`geo:` filter, map view) |
| face_count | v1 | Number of detected faces (`faces:` filter) |
| preview_rotation | v1 | Mirror of the sidecar field |
| preview_variant | v3 | Mirror of the sidecar field |
| duration_seconds | v4 (renamed v11) | Media duration, audio and video (`duration:` filter). Was `video_duration` before v11 |
| video_codec | v5 | Video codec (`codec:` filter) |
| face_scan_status | v7 | Mirror of the sidecar field |
| leaf_tag_count | v8 | Number of leaf tags (`tagcount:` filter) |
| tag_sources | v10 | JSON mirror of the sidecar map (checked by `maki doctor`) |
| audio_sample_rate, audio_channels, audio_bitrate, audio_key, audio_bpm | v11 | Typed audio properties from variant `source_metadata` `audio_*` keys (lofty at import; key/BPM from `maki audio analyze`); `key:` / `bpm:` filters |

The `assets` table additionally carries the `assets_fts` FTS5 trigram index (schema v9) over name, filename, description and source metadata, maintained by triggers; free-text terms of three or more characters go through it.

An asset groups one or more **variants**.

### Variant
A concrete file belonging to an asset. Multiple variants form a group (e.g. RAW + JPEG + edited TIFF).

| Field | Type | Description |
|---|---|---|
| content_hash | SHA-256 | Primary identity, derived from file content |
| asset_id | UUID | Parent asset |
| role | Enum | Original, Alternate, Processed, Export, Sidecar |
| format | String | File extension / MIME type |
| file_size | u64 | Size in bytes |
| original_filename | String | Filename at import time |
| source_metadata | Map | EXIF, XMP, and other embedded metadata extracted at import |
| locations | Vec<FileLocation> | Where this variant physically lives |

### FileLocation
A physical location of a variant on a specific volume.

| Field | Type | Description |
|---|---|---|
| volume_id | UUID | Which volume |
| relative_path | PathBuf | Path relative to volume root |
| verified_at | Option<DateTime> | Last time hash was verified at this location (variant and recipe locations alike; persisted to the sidecar by `maki verify`) |

### Volume
A storage device or mount point.

| Field | Type | Description |
|---|---|---|
| id | UUID | Stable identifier |
| label | String | Human-readable name (e.g. "Photos Archive 2024") |
| mount_point | PathBuf | Expected mount path (e.g. /Volumes/PhotosArchive) |
| volume_type | Enum | Local, External, Network |
| purpose | Option<Enum> | Media, Working, Archive, Backup, Cloud (`volume set-purpose`; drives `backup-status`) |
| is_online | bool | Derived at runtime from mount point availability |

### Recipe
Processing instructions associated with a variant. During import, files with recognized recipe extensions that share a filename stem with a media file in the same directory are automatically attached as recipes rather than imported as variants. Standalone recipe files (imported without a co-located media file) are resolved to their parent variant by matching filename stem and directory.

Known recipe extensions: `.xmp` (Adobe/Lightroom/CaptureOne), `.cos` / `.cot` / `.cop` (CaptureOne session/template/preset), `.pp3` (RawTherapee), `.dop` (DxO), `.on1` (ON1).

| Field | Type | Description |
|---|---|---|
| id | UUID | Stable identifier |
| variant_hash | SHA-256 | Which variant this recipe belongs to |
| software | String | e.g. "CaptureOne 23", "Photoshop 2024" |
| recipe_type | Enum | Sidecar (XMP, COS, etc.), EmbeddedExport |
| content_hash | SHA-256 | Hash of the recipe file itself (mutable — updated when file changes) |
| location | FileLocation | Where the recipe file lives (primary identity for dedup); its `verified_at` records the last hash verification |
| pending_writeback | bool | Set when an XMP write-back could not reach the file (volume offline) or after `undo`; replayed by `maki writeback` (schema v2 column) |

### Face

> Only present when built with `--features ai`.

A detected face within an asset image, stored with bounding box, confidence, and recognition embedding.

| Field | Type | Description |
|---|---|---|
| id | UUID | Stable identifier |
| asset_id | UUID | Parent asset |
| person_id | Option<UUID> | Assigned person (NULL if unassigned) |
| bbox_x, bbox_y, bbox_w, bbox_h | f32 | Bounding box in normalized coordinates [0, 1] |
| confidence | f32 | Detection confidence score |
| embedding | Vec<f32> | 512-dimensional ArcFace recognition embedding (also stored as binary file in `embeddings/arcface/`) |
| recognition_model | Option<String> | Identifier of the ArcFace variant that produced the embedding (schema v6); clustering only mixes faces from one model |
| created_at | DateTime | When the face was detected |

Face records are persisted in both SQLite and `faces.yaml` at the catalog root. ArcFace embeddings are additionally stored as raw binary files (`embeddings/arcface/<2-char prefix>/<face_id>.bin`) for rebuild resilience. The face crop thumbnail is not a stored field: it lives at `faces/<2-char prefix>/<face_id>.jpg` and is served under `/face/`.

### Person

> Only present when built with `--features ai`.

A named or unnamed person group, linking detected faces across assets.

| Field | Type | Description |
|---|---|---|
| id | UUID | Stable identifier |
| name | Option<String> | User-assigned name (NULL until named) |
| representative_face_id | Option<UUID> | Face used as the person's thumbnail |
| created_at | DateTime | When the person was created |

People records are persisted in both SQLite and `people.yaml` at the catalog root for rebuild resilience.

### Stack
A scene grouping that collapses multiple assets into a single pick in the browse grid. Anonymous (no name or description), position-based ordering. Stacks auto-dissolve when reduced to one member or fewer.

| Field | Type | Description |
|---|---|---|
| id | UUID | Stable identifier |
| created_at | DateTime | When the stack was created |
| member_count | u64 | Number of assets in the stack |

Stack membership is tracked on the `assets` table via `stack_id` and `stack_position` columns. Position 0 is the pick. Stacks are persisted in both SQLite (`stacks` table) and `stacks.yaml` at the catalog root for rebuild resilience.

**Design decision — location-based identity**: Recipes are identified by their location `(variant_hash, volume_id, relative_path)` rather than their content hash. This is because recipe files (XMP, COS, etc.) are routinely edited by external software. Re-importing after an external edit updates the recipe in place (new hash, re-extracted XMP metadata) rather than creating a duplicate. During verification, a changed recipe hash is reported as "modified" (not a failure) and the stored hash is updated.

## Components

### 1. Content Store

**Responsibility**: file identity, deduplication, and physical location tracking.

**Operations**:
- `ingest(path) -> SHA-256` — hash a file, register it. If hash already exists, skip copy (dedup).
- `locate(hash) -> Vec<FileLocation>` — find all known locations of a file.
- `relocate(hash, from_volume, to_volume)` — move/copy a file between volumes, update locations.
- `verify(hash, location) -> bool` — re-hash file at location, confirm integrity.
- `remove_location(hash, location)` — unregister a location (file moved/deleted externally).

**Storage model**: referenced mode — files stay in their original directory structure on each volume. The content store indexes their hash and location but never moves or renames originals. This preserves interoperability with tools like CaptureOne that expect a specific directory layout. Deduplication is logical (same hash → same variant) rather than physical.

### 2. Metadata Store

**Responsibility**: persist and retrieve all asset metadata as text-based sidecar files.

**Sidecar format**: YAML, one file per asset.
```yaml
# <catalog_root>/metadata/<uuid-prefix>/<uuid>.yaml
id: 550e8400-e29b-41d4-a716-446655440000
name: "Sunset at beach"
asset_type: image
tags: [landscape, sunset, beach, vacation-2024]
description: "Golden hour shot from Koh Lanta"
created_at: 2024-07-15T18:32:00Z
variants:
  - content_hash: "sha256:abcdef..."
    role: original
    format: NEF
    file_size: 52428800
    original_filename: "DSC_4521.NEF"
  - content_hash: "sha256:123456..."
    role: processed
    format: TIFF
    file_size: 104857600
    original_filename: "DSC_4521_edited.tiff"
recipes:
  - variant: "sha256:abcdef..."
    software: "CaptureOne 23"
    recipe_type: sidecar
    content_hash: "sha256:fedcba..."
```

**Operations**:
- `save(asset)` — write/update sidecar YAML.
- `load(asset_id) -> Asset` — read sidecar YAML.
- `list() -> Vec<AssetSummary>` — enumerate all known assets.
- `sync_to_catalog()` — rebuild SQLite catalog from sidecar files (source of truth → cache).

### 3. Local Catalog (SQLite)

**Responsibility**: fast queryable index over all metadata. Rebuilt from sidecar files.

**Tables** mirror the data model: `assets` (+ the `assets_fts` FTS5 virtual table and its triggers), `variants`, `file_locations`, `volumes`, `recipes`, `stacks`, `collections`, `collection_assets`, `embeddings` (created unconditionally — the `embed:` filter works in every build), `faces` and `people` (with `--features ai`), and `schema_version`. `SCHEMA_VERSION` is 11; `run_migrations()` applies version-guarded `if current < N` blocks and is executed by `maki init`, `maki migrate` and `maki rebuild-catalog` only — every other command (and the web server) just verifies the stored version at startup and exits if it is behind.

This is a **derived cache**, not the source of truth. Running `maki rebuild-catalog` regenerates it from sidecar files, `collections.yaml`, `stacks.yaml`, `faces.yaml`, `people.yaml`, and embedding binary files. This means:
- No data loss if the SQLite file is deleted.
- Sidecars can be edited manually or by external tools.
- The catalog can include denormalized fields for fast queries (e.g. extracted EXIF date, camera model).

### 4. Device Registry

**Responsibility**: volume management and online/offline detection.

**Operations**:
- `register(label, mount_point, type) -> Volume` — add a new volume.
- `list() -> Vec<Volume>` — list all volumes with online/offline status.
- `resolve_volume(label_or_id) -> Volume` — find a volume by label or UUID.
- `find_volume_for_path(path) -> Volume` — find which registered volume contains a given path.

**Online detection**: checks if the mount point directory exists (`mount_point.exists()`).

### 5. Asset Service

**Responsibility**: high-level operations that orchestrate the other components.

**Operations**:
- `import(paths, volume_id) -> ImportResult` — hash files, extract metadata (EXIF etc.), create assets, create variants, write sidecars, update catalog. Auto-groups files that share the same filename stem and reside in the same directory (e.g. `DSC_4521.NEF`, `DSC_4521.jpg`, `DSC_4521.xmp`, `DSC_4521.cos` all become one asset). Media files become variants; processing sidecars (`.xmp`, `.cos`, `.cot`, `.cop`, etc.) are attached as recipes. Standalone recipe files (no co-located media) are resolved to parent variants by matching filename stem and directory on the same volume. When a file's content hash already exists, the new file location is added to the existing variant (both sidecar and catalog) rather than being silently skipped. Only truly skips when the exact location (volume + relative path) is already tracked. Re-importing a modified recipe updates it in place (new hash, re-extracted XMP metadata). Reports per-file status as `Imported`, `LocationAdded`, `Skipped`, `RecipeAttached`, or `RecipeUpdated`. Supports `--include`/`--skip` flags for file type group filtering.
- `group(variant_hashes) -> Asset` — manually group variants into one asset.
- `tag(asset_id, tags)` — add tags to an asset.
- `relocate(asset_id, target_volume)` — move all variants of an asset to another volume. Supports `--remove-source` (move instead of copy) and `--dry-run`.
- `find_duplicates() -> Vec<DuplicateGroup>` — find variants with same hash on multiple locations.
- `verify(paths, volume, asset, config) -> VerifyResult` — re-hash files on disk and compare against stored content hashes. Reports `Ok`, `Mismatch`, `Modified` (recipe with changed hash), `Missing`, `Skipped`, `SkippedRecent`, or `Untracked`. Modified recipes are not treated as failures — their stored hash is updated. Supports path mode (verify specific files/dirs), catalog mode (verify all locations), `--volume`, `--asset`, `--include`/`--skip` filters, `--max-age` (skip files verified within N days), and `--force` (override `--max-age`). Persists `verified_at` timestamps to sidecar YAML for both variant and recipe locations.
- `refresh(paths, volume, asset_id, dry_run, media) -> RefreshResult` — re-read metadata from changed recipe/sidecar files. Iterates recipe file locations, compares on-disk hash to stored hash, and for changed files re-extracts XMP metadata and updates catalog + sidecar. Reports `Unchanged`, `Refreshed`, `Missing`, or `Offline`. When `media` is true, also scans JPEG/TIFF variant files and re-extracts embedded XMP metadata. Lighter than `sync` — only touches metadata, never file locations.
- `fix_roles(paths, volume, asset, apply) -> FixRolesResult` — scan multi-variant assets with a RAW variant and re-role non-RAW variants from `Original` to `Alternate`. Assets with only non-RAW variants are untouched. Dry-run by default; `--apply` writes changes to both sidecar YAML and SQLite catalog.
- `cleanup(volume, path_prefix, apply) -> CleanupResult` — remove stale location/recipe records, locationless variants, orphaned assets, and all orphaned derived files (previews, smart previews, SigLIP embeddings, face crops, ArcFace embeddings). `path_prefix` scopes scanning to files under a specific directory.
- `delete_assets(asset_ids, apply, remove_files) -> DeleteResult` — remove assets from the catalog. Report-only by default; `apply` executes deletion (asset rows, variants, file locations, recipes, previews, smart previews, face crops, face/embedding DB records, embedding binaries, sidecar YAML, collection memberships, stack membership). `remove_files` (requires `apply`) also deletes physical files from disk. Supports ID prefix matching and stdin piping.
- `sync_metadata(volume, asset, dry_run, media) -> SyncMetadataResult` — bidirectional XMP metadata sync. Inbound: re-reads externally modified XMP recipes. Outbound: writes pending DAM edits. Detects conflicts when both sides changed.
- `sync(paths, volume, apply, remove_stale) -> SyncResult` — reconcile catalog with disk after external file moves/renames/modifications.

### 6. Query Engine

**Responsibility**: search and filter assets via the SQLite catalog.

**Query capabilities**:
- Filter by: tags, date range, asset type, format, rating (`rating:N` exact, `rating:N+` minimum), color label (`label:Red`), date (`date:2026-02-25` prefix match, `dateFrom:` inclusive lower bound, `dateUntil:` inclusive upper bound), camera model, lens, ISO, focal length, aperture, dimensions, volume, online/offline status
- Location health filters: `orphan:true` (no file locations), `missing:true` (files missing from disk), `stale:N` (not verified in N days), `volume:none` (no locations on online volumes)
- Asset ID prefix: `id:<prefix>` (UUID prefix match)
- Negation: `-` prefix excludes matches (`-tag:rejected`, `-sunset`)
- OR within filters: comma operator (`tag:alice,bob`, `format:nef,cr3`, `label:Red,Orange`)
- Visual similarity: `similar:<asset-id>` or `similar:<asset-id>:<limit>` (feature-gated: `--features ai`)
- Search by query image: `maki search --image <file> [QUERY] [--limit N]` — a local file not in the catalog; content-hash exact match pinned first, then SigLIP ranking via the shared `embedding_store::rank_similar` (feature-gated: `--features ai`; module `src/query_image.rs`)
- Full-text search over name, filename, description, and source metadata
- Sort by: date, name, file size, import date
- Output: asset list with summary info, or detailed asset view

**Editing capabilities**:
- `tag(asset_id, tags, remove)` — add or remove tags, with XMP write-back
- `edit(asset_id, fields)` — set/clear name, description, rating, color label, and date via `EditFields` (triple-option pattern: `None` = no change, `Some(None)` = clear, `Some(Some(x))` = set). Rating, description, and label changes trigger XMP write-back.
- `set_rating(asset_id, rating)` / `set_color_label(asset_id, label)` — individual field setters used by web UI and batch operations
- `auto_group(asset_ids, apply)` — group assets by filename stem using fuzzy prefix matching

### 7. Preview Generator

**Responsibility**: create and cache thumbnails for browsing.

**Approach**:
- Images: use `image` crate for common formats, shell out to `dcraw` or `libraw` for RAW files.
- Videos: shell out to `ffmpeg` to extract a frame.
- Audio: info card with a waveform strip rendered into its top region via ffmpeg `showwavespic` (`filter=peak`); plain info card when ffmpeg is missing. Playback in the web UI via `GET /audio/{hash}` (same range-capable handler as `GET /video/{hash}`).
- Non-visual formats (documents, unknown): generate an info card — an 800x600 JPEG showing file metadata (name, format, size, and audio-specific properties like duration/bitrate via `lofty`). Uses `imageproc` for text rendering with an embedded DejaVu Sans font (`ab_glyph`).
- Fallback: when external tools (dcraw, ffmpeg) are missing, RAW and video files also get an info card instead of no preview.
- Store previews in `<catalog_root>/previews/<hash-prefix>/<hash>.jpg` at a standard size (800px longest edge for visual previews, 800x600 for info cards).
- Generate on import, regenerate on demand.

### 8. Output Formatting

**Responsibility**: flexible output for scripting, pipelines, and machine consumption.

**Module**: `src/format.rs` — template engine with `{placeholder}` substitution and escape sequences.

**Capabilities**:
- **Global `--json` flag**: available on all commands. Outputs structured JSON to stdout; human-readable messages go to stderr. All result types derive `serde::Serialize`.
- **`--format` flag** (on `search` and `duplicates`): presets (`ids`, `short`, `full`, `json`) or custom templates (`'{id}\t{name}\t{tags}'`). When `--format` is explicit, result counts are suppressed.
- **`-q`/`--quiet`** (on `search`): shorthand for `--format=ids`, outputting one UUID per line for scripting.
- **Template placeholders**: `{id}`, `{short_id}`, `{name}`, `{filename}`, `{type}`, `{format}`, `{date}`, `{tags}`, `{description}`, `{label}`, `{hash}`. Templates support `\t` and `\n` escape sequences.

### 9. Stats

**Responsibility**: aggregate and display catalog statistics from the SQLite index.

**Implementation**: query methods on `Catalog` (in `src/catalog.rs`) compute counts, breakdowns, and coverage metrics. The `build_stats()` method assembles all sections into a `CatalogStats` struct, merging catalog data with device registry (online/offline status).

**Sections** (each gated by a CLI flag):
- **Overview** (always shown): asset/variant/recipe counts, volume totals (online/offline), total file size.
- **Types** (`--types`): asset type breakdown with percentages, top variant formats, recipe format distribution.
- **Volumes** (`--volumes`): per-volume asset/variant/recipe counts, size, directory count, format list, verification coverage.
- **Tags** (`--tags`): unique tag count, tagged/untagged asset counts, top tags by frequency.
- **Verification** (`--verified`): location verification coverage, oldest/newest timestamps, per-volume breakdown.

**Flags**: `--all` enables all sections. `--limit N` controls top-N lists (default 20). `--json` outputs structured `CatalogStats` JSON.

**Edge cases**: empty catalog returns all zeros without errors. Division-by-zero for percentages is guarded. Volumes with no files are included in `--volumes` with zero counts. Recipe format is extracted from `relative_path` extension in Rust, falling back to "unknown".

### 10. Web UI

**Responsibility**: browser-based interface for browsing, searching, and editing assets.

**Module**: `src/web/` — axum server with askama templates and htmx interactivity. `mod.rs` holds `AppState`, the router and the middleware; handlers live in `routes/` split by resource (browse, assets, tags, stacks, collections, saved_search, calendar_map, duplicates, import, jobs, maintain, config, media, stats, volumes, and the `ai/` submodules tags / embed / similarity / query_image / faces / review / stroll); `jobs.rs` is the background-job registry; `templates.rs` the askama structs; `static_assets.rs` the embedded files.

**Architecture**:
- **Connections**: `CatalogPool` pre-opens four SQLite connections with `Catalog::open_fast()` (WAL, mmap, tuned pragmas; no migrations — `open_fast` is an alias of `open`). Handlers check one out per request (`state.catalog()`, RAII `PooledCatalog` returned on drop) inside `tokio::task::spawn_blocking`, since `rusqlite::Connection` is not `Send`; an exhausted pool opens a temporary extra connection. The schema version is verified once at process startup; the server never migrates.
- **`AppState`** holds the catalog root, the pool, preview config and extension, the dropdown cache (tags, formats, volumes, collections, people — warmed at startup, invalidated by write endpoints), and the serve-time config: `per_page`, the six `stroll_*` limits, `default_filter`, `slideshow_seconds` / `slideshow_loop`, `remember_latest_filter`, the import-dialog defaults `import_smart_previews` / `import_embeddings` / `import_descriptions`, `dedup_prefer`, `smart_on_demand`, `read_only`, `basic_auth`, `vlm_config` / `vlm_enabled`, and the `jobs: JobRegistry`. With `--features ai` it also carries the lazily loaded `ai_model` (SigLIP), `ai_label_cache`, `ai_embedding_index` (in-memory dot-product index, loaded on first similarity query), `face_detector`, and `query_images` (TTL-evicted sessions for search by query image). Most of these are surfaced to the browser through `GET /api/build-info`.
- **Middleware** (outermost first): `guard_request` enforces HTTP Basic auth when `[serve] username`/`password` are set (`MAKI_SERVE_PASSWORD` overrides the config password; digest comparison; `WWW-Authenticate: Basic realm="MAKI"`) on every route including previews and SSE, then read-only mode: with `--read-only` or `[serve] read_only`, every non-GET/HEAD request is rejected with 403 — enforcement is by method, so a new write endpoint cannot be forgotten — with the single exception of `POST /api/query-image`, which carries a file body but writes nothing. `log_request` prints `METHOD URI -> status (duration)` to stderr when `--log` is set and adds `Cache-Control: no-cache` to `/preview/`, `/smart-preview/` and `/face/` responses so rotated or regenerated previews revalidate.
- **Jobs**: long-running operations (import, batch embed / auto-tag / detect-faces / describe, every `/api/maintain/*` operation, the suggest-tags review) run in a background task registered in `JobRegistry`; the start endpoint returns `{job_id}` immediately, progress streams over SSE from `GET /api/jobs/{id}/progress` (ring-buffered so a re-attach after reload replays), and the terminal event carries the summary counters. The global progress toast in `base.html` consumes this.
- **Static assets** are embedded at compile time: htmx, `style.css`, the favicon and `maki-icon.svg`, plus Leaflet, Leaflet.markercluster, their CSS and five marker images for the map view.
- **Media**: previews are served from `<catalog>/previews/` via `tower-http::ServeDir`; smart previews go through a handler that can generate them on demand (`[preview] generate_on_demand`); face crops are a second `ServeDir` on `<catalog>/faces/`; video and audio originals stream through one range-capable handler.

**Routes** (from `build_router`; *(ai)* = only with `--features ai`; *(pro)* = only meaningful in a Pro build):

*Pages*
- `GET /` — browse page: search row (free text + filter tokens, **Find by image** *(ai)*), collapsible filter bar (tag chips with include/exclude and `=`/`/` markers, people chips *(ai)*, path prefix with autocomplete, rating stars, label dots, type, grouped format multi-select, volume, collection), faceted sidebar, grid / calendar heatmap / map view toggle, grid density, sort, pagination, stack collapse, lightbox, batch toolbar, saved-search chips, remembered-last-filter restore
- `GET /asset/{id}` — asset detail: preview with rotate / regenerate / preview-variant choice, editable name, description, rating, label, date, tags (with provenance badges), variants with role dropdown and preview-representative column, recipes with pending-writeback markers, stack members, collections, faces *(ai)*, similar / embed / stack-similar *(ai)*, VLM describe *(pro)*, audio player and key/BPM badges, split-variants dialog. Params `prev` / `next` carry the grid neighbours
- `GET /compare?ids=a,b[,c,d]` — side-by-side comparison of 2–4 assets
- `GET /tags` — hierarchical tag tree with counts, inline rename / split / delete modals, vocabulary export
- `GET /stats` — catalog statistics page
- `GET /analytics` — analytics dashboard (shooting frequency, camera/lens usage, rating distribution, formats, monthly import volume, storage per volume)
- `GET /backup` — backup coverage page (per-volume purpose, under-backed-up assets)
- `GET /volumes` — volume management page (register, rename, purpose, remove, browse subfolders for import)
- `GET /collections` — collections page
- `GET /saved-searches` — saved searches management page
- `GET /duplicates?mode=all|same|cross&volume&format&path` — duplicates page with summary cards, mode tabs, filters, lightbox, per-location remove and auto-resolve
- `GET /people` *(ai)* — people page: cards with rename, batch merge via checkbox selection with a merge-target badge, merge suggestions, name filter
- `GET /stroll?id&q&n&mode&skip&cross_session` *(ai)* — visual exploration page; without `id` picks a random embedded asset

*Browse support (all take the browse filter params `q, type, tag, format, volume, rating, label, collection, path, person, sort, page, stacks, nodefault`)*
- `GET /api/search` — results fragment (htmx target) with pagination; similarity queries render as one score-sorted page
- `GET /api/all-ids` — ordered asset IDs of the whole result set (select-all, remembered-filter restore)
- `GET /api/page-ids` — IDs of one page (lightbox / keyboard navigation)
- `GET /api/facets` — `FacetCounts` for the sidebar (total, ratings, labels, formats, volumes, tags, years, geotagged)
- `GET /api/paths?q&volume&limit` — directory / file completions for the path filter
- `GET /api/calendar?year…` — per-day counts for the heatmap (`{year, counts, years}`)
- `GET /api/map` — geo points for the map view
- `GET /api/tags` — all tags with counts as JSON (autocomplete)
- `GET /api/stats` — `CatalogStats` JSON
- `GET /api/build-info` — feature flags (`ai`, `pro`), read-only state, serve-time defaults (slideshow, remember-last-filter, import checkboxes)

*Per-asset edits (return the corresponding htmx fragment; every edit is journaled for `maki undo` and, where applicable, written back to XMP)*
- `POST /api/asset/{id}/tags` — add tags (form `tags=` comma-separated)
- `DELETE /api/asset/{id}/tags?tag=…` — remove one tag (query param)
- `POST /api/asset/{id}/tags/clear` — remove all tags
- `PUT /api/asset/{id}/rating` — set/clear rating (form `rating=N`, 0 clears)
- `PUT /api/asset/{id}/description` — set/clear description
- `PUT /api/asset/{id}/name` — set/clear name
- `PUT /api/asset/{id}/label` — set/clear color label
- `PUT /api/asset/{id}/date` — set the asset date
- `POST /api/asset/{id}/preview` — regenerate preview and smart preview
- `POST /api/asset/{id}/rotate` — cycle preview rotation 90° CW and regenerate
- `POST /api/asset/{id}/preview-variant` — set or clear the preview variant override
- `POST /api/asset/{id}/variant-role` — change a variant's role (JSON `{content_hash, role}`)
- `POST /api/asset/{id}/reimport-metadata` — clear metadata and re-extract from the variant files
- `GET /api/asset/{id}/recipes-fragment` — recipes block (refreshed after edits that flag `pending_writeback`)
- `POST /api/asset/{id}/split` — extract variants into new assets (JSON `{variant_hashes}`)
- `POST /api/asset/{id}/writeback` *(pro)* — write this asset's pending XMP changes now
- `POST /api/asset/{id}/vlm-describe` *(pro)* — describe via VLM (JSON `{mode?, model?}`)

*Batch (JSON `{asset_ids, …}`; return `{succeeded, failed, errors}` or `{job_id}`)*
- `PUT /api/batch/rating` — set/clear rating
- `POST /api/batch/tags` — add or remove tags (`{tags, remove}`)
- `PUT /api/batch/label` — set/clear label
- `POST /api/batch/collection` / `DELETE /api/batch/collection` — add to / remove from a collection (`{collection}`)
- `POST /api/batch/auto-group` — group selected assets by filename stem
- `POST /api/batch/group` — merge the selected assets into one (`{target_id?}`)
- `POST /api/batch/stack` / `DELETE /api/batch/stack` — create a stack from the selection / unstack
- `POST /api/batch/delete` — delete assets (`{remove_files?}`; goes through the trash unless disabled)
- `POST /api/batch/describe` *(pro)* — VLM describe job (`{mode?, model?}`)
- `POST /api/batch/export` — ZIP download (`{asset_ids?, filters?, layout, source?, all_variants, include_sidecars}`; `filters` takes the browse URL params verbatim and resolves through the grid pipeline; `source` is `originals` / `previews` / `smart`; `X-Maki-Exported` / `X-Maki-Skipped` / `X-Maki-Skipped-Offline` headers flag partial archives)

*Stacks*
- `PUT /api/asset/{id}/stack-pick` — make this asset the pick
- `DELETE /api/asset/{id}/stack` — dissolve its stack
- `POST /api/asset/{id}/stack-add` — add this asset to an existing stack
- `GET /api/stack/{id}/members` — ordered member cards (per-stack expand/collapse)

*Tags*
- `POST /api/tag/rename` — `{old_tag, new_tag, apply}` (dry-run report unless `apply`)
- `POST /api/tag/split` — `{old_tag, new_tags, keep, apply}`
- `POST /api/tag/delete` — `{tag, apply}`
- `GET /api/tags/export-vocabulary?format&counts&prune` — download the catalog tag hierarchy as a vocabulary file

*Collections and saved searches*
- `GET /api/collections` / `POST /api/collections` — list / create (`{name, description?}`)
- `GET /api/saved-searches` / `POST /api/saved-searches` — list / save (`{name, query, sort?, favorite}`)
- `DELETE /api/saved-searches/{name}`, `PUT /api/saved-searches/{name}/favorite` (`{favorite}`), `PUT /api/saved-searches/{name}/rename` (`{new_name}`)

*Volumes*
- `GET /api/volumes` / `POST /api/volumes` — list / register (`{path, label, purpose?}`)
- `PUT /api/volumes/{id}/rename` (`{label}`), `PUT /api/volumes/{id}/purpose` (`{purpose}`), `DELETE /api/volumes/{id}`
- `GET /api/volumes/{id}/browse?prefix&limit&hidden&filter` — subfolder listing for the import dialog

*Duplicates*
- `POST /api/dedup/resolve` — auto-resolve same-volume duplicates (`{min_copies?, volume?, format?, path?, prefer?, dry_run?}`), returns `DedupResult`
- `DELETE /api/dedup/location` — remove one file location and co-located recipes (`{content_hash, volume_id, relative_path}`)

*Import and jobs*
- `POST /api/import` — start an import job (`{volume_id, subfolder?, profile?, tags?, auto_group?, smart?, embed?, describe?, dry_run?}`), returns `{job_id}`
- `GET /api/import/profiles` — `[import.profiles.*]` names for the dialog
- `GET /api/jobs` — snapshot of running and recent jobs (nav badge)
- `GET /api/jobs/{id}` — one job's status and counters
- `GET /api/jobs/{id}/progress` — SSE progress stream (replays the ring buffer on re-attach)
- `GET /api/jobs/{id}/result` — the finished job's result payload (e.g. dry-run import report)

*Maintenance jobs (each returns `{job_id}`)*
- `POST /api/maintain/writeback` *(pro)* — `{query?, volume?, all, force}`
- `POST /api/maintain/sync-metadata` *(pro)* — `{volume?, media, dry_run}`
- `POST /api/maintain/verify` — `{volume?, max_age_days?}`
- `POST /api/maintain/generate-previews` — `{volume?, asset?, smart, force}`
- `POST /api/maintain/sync` — `{volume, path?, apply}`
- `POST /api/maintain/refresh` — `{volume?, media, dry_run}`
- `POST /api/maintain/cleanup` — `{volume?, path?, apply}`
- `POST /api/maintain/suggest-tags-review` *(ai)* — `{asset_ids, threshold?}` — batch suggestion job feeding the tag review page

*Settings and OS integration*
- `GET /api/config` / `POST /api/config` — read / save `maki.toml` (`{config}`; written with `toml_edit` so comments survive)
- `GET /api/config/schema` — JSON Schema (draft 2020-12) of `CatalogConfig` for the settings form
- `POST /api/open-location` — reveal a file in the OS file manager (`{volume_id, relative_path}`; local server only)
- `POST /api/open-terminal` — open a terminal at a file's directory

*AI* *(ai)*
- `POST /api/asset/{id}/suggest-tags` — zero-shot SigLIP tag suggestions
- `POST /api/batch/auto-tag` — `{asset_ids}` auto-tag job
- `POST /api/asset/{id}/embed`, `POST /api/batch/embed` — build SigLIP embeddings without tagging (single: synchronous; batch: job)
- `POST /api/asset/{id}/similar` — nearest neighbours of one asset (`[{asset_id, similarity, …}]`)
- `POST /api/asset/{id}/stack-similar` — stack this asset with its neighbours (`{threshold?, limit?}`)
- `POST /api/query-image` — search-by-image upload: raw body, `Content-Type` image/*, optional `X-Maki-Filename`; own 64 MiB body limit; allowed in read-only mode. Hashes the file for an exact variant match, encodes it with SigLIP (RAW/video via the preview generator into a scratch dir), stores the result in `AppState.query_images` (1 h TTL, 64 entries) and returns `{token, filename, exact_match_id, embedded, warning, thumbnail}`; the browse pipeline references it as `similar:@<token>`
- `GET /api/query-image/{token}` — session data for the query pill; 404 once expired
- `GET /api/asset/{id}/faces` — faces of an asset; `POST /api/asset/{id}/detect-faces`, `POST /api/batch/detect-faces` (job)
- `PUT /api/faces/{face_id}/assign` (`{person_id}`), `DELETE /api/faces/{face_id}/unassign`, `DELETE /api/faces/{face_id}`
- `POST /api/faces/cluster` — agglomerative clustering of unassigned faces into people
- `GET /api/people` / `POST /api/people`, `PUT /api/people/{id}/name`, `POST /api/people/{id}/merge` (`{source_id}` or `{source_ids}`), `GET /api/people/merge-suggestions`, `DELETE /api/people/{id}`
- `GET /api/stroll/neighbors?id&q&n&mode&skip&cross_session` — centre asset plus neighbours with preview URLs and scores; `n` is clamped to `[serve] stroll_neighbors` / `stroll_neighbors_max` (defaults 12 / 25); `mode` `similar` / `explore` (skip-N offset into the ranked list) / `discover`

*Media and static*
- `/preview/…` — `ServeDir` on `<catalog>/previews/`
- `GET /smart-preview/{prefix}/{file}` — smart preview, generated on demand when configured
- `GET /video/{hash}`, `GET /audio/{hash}` — range-capable streaming of the original file
- `/face/…` *(ai)* — `ServeDir` on `<catalog>/faces/` (crop thumbnails)
- `GET /favicon.ico`, `GET /static/{favicon.ico, maki-icon.svg, htmx.min.js, style.css, leaflet.min.js, leaflet.css, leaflet.markercluster.min.js, MarkerCluster.css, MarkerCluster.Default.css, images/marker-icon.png, images/marker-icon-2x.png, images/marker-shadow.png, images/layers.png, images/layers-2x.png}` — embedded assets

**Catalog extensions** (in `src/catalog/`):
- `SearchOptions` / `SearchSort` / `SearchPage` — paginated search with dynamic filters and sort; `build_search_where()` joins `variants` / `file_locations` only when a filter needs them
- `search_paginated()` / `search_paginated_with_count()` — paginated search queries
- `calendar_counts(year, opts)` / `calendar_years()` — heatmap data respecting all search filters
- `facet_counts(opts)` — sidebar `FacetCounts` under the current filters
- `list_all_tags()`, `list_all_formats()`, `list_volumes()` — dropdown data

### 11. Config Module

**Responsibility**: parse and provide catalog configuration from `maki.toml`.

**Module**: `src/config.rs` — `CatalogConfig` with one sub-struct per section: `PreviewConfig`, `ServeConfig`, `ImportConfig` (+ `ImportProfile`), `DedupConfig`, `VerifyConfig`, `AiConfig`, `ContactSheetDefaults`, `VlmConfig` (+ `VlmModelConfig` per-model overrides), `BrowseConfig`, `WritebackConfig`, `TrashConfig`, `HistoryConfig`, `CliDefaults`, `GroupConfig`, `WatchConfig`, `AudioConfig`. `load_config()` resolves the catalog root and parses the file; `resolve_model_dir()` expands `~/` in `[ai] model_dir`. The settings dialog reads and writes the file through `GET|POST /api/config`, with a JSON Schema derived from the same structs (`schemars`) driving the form.

**Sections** (every field optional; defaults in `src/config.rs`, full documentation in `doc/manual/reference/08-configuration.md`):
- top level: `default_volume` (UUID fallback for import)
- `[preview]`: `max_edge`, `format` (jpeg/webp), `quality`, `smart_max_edge`, `smart_quality`, `generate_on_demand`
- `[serve]`: `port`, `bind`, `per_page`, `stroll_neighbors`, `stroll_neighbors_max`, `stroll_fanout`, `stroll_fanout_max`, `stroll_discover_pool`, `read_only`, `username`, `password`
- `[import]`: `exclude`, `auto_tags`, `smart_previews`, `embeddings`, `descriptions`, `[import.profiles.<name>]` (named presets of the same fields)
- `[dedup]`: `prefer`
- `[verify]`: `max_age_days`
- `[ai]`: `model`, `model_dir`, `threshold`, `labels`, `prompt`, `execution_provider`, `text_limit`, `face_cluster_threshold`, `face_min_confidence`
- `[vlm]`: `endpoint`, `model`, `models`, `mode`, `max_tokens`, `temperature`, `timeout`, `prompt`, `concurrency`, `max_image_edge`, `num_ctx`, `top_p`, `top_k`, `repeat_penalty`, `[vlm.model_config."name"]` overrides of `max_tokens`, `temperature`, `timeout`, `max_image_edge`, `num_ctx`, `top_p`, `top_k`, `repeat_penalty`, `prompt`
- `[contact_sheet]`: `layout`, `paper`, `fields`, `label_style`, `copyright`, `margin`, `quality`
- `[browse]`: `default_filter`, `slideshow_seconds`, `slideshow_loop`, `remember_latest_filter`
- `[writeback]`: `enabled`, `mirror_tags`
- `[trash]`: `enabled`, `retention_days`
- `[history]`: `enabled`, `max_operations`
- `[cli]`: `log`, `time`, `verbose`
- `[group]`: `session_root_pattern`
- `[watch]`: `poll_seconds`, `exclude`
- `[audio]`: `key_command`, `bpm_command`

### 12. EXIF Reader

**Responsibility**: extract EXIF metadata from image files at import time.

**Module**: `src/exif_reader.rs` — uses `kamadak-exif` crate.

**Extracted fields**: camera model, lens model, ISO, focal length, aperture (f-number), image dimensions (width/height), date/time original. All stored in the variant's `source_metadata` map.

### 13. XMP Reader

**Responsibility**: extract and write back XMP metadata for bidirectional sync with photo editing tools.

**Module**: `src/xmp_reader.rs` — uses the `quick-xml` crate for parsing; write-back is an event-driven locate-and-splice pipeline (`locate()` maps the existing element layout, then the changed properties are spliced in while every other byte of the file is preserved). `src/embedded_xmp.rs` / `src/embedded_xmp_write.rs` apply the same to the APP1 XMP segment of JPEG files (`writeback --embed`).

**Read operations**: `extract_xmp_metadata(path)` — parses `dc:subject` (keywords/tags), `dc:description`, `xmp:Rating`, `xmp:Label`, `dc:creator`, `dc:rights` from XMP sidecar files. When `xmp:Rating` is absent, `MicrosoftPhoto:Rating` (percentage scale: 1, 25, 50, 75, 99) is read and normalized to 1–5 via `normalize_rating()`.

**Write operations** (all preserve existing XMP structure):
- `update_rating(path, rating)` — write `xmp:Rating` value
- `update_tags(path, added, removed)` — delta-based `dc:subject`/`rdf:Bag` editing (preserves externally-added tags)
- `update_description(path, description)` — write/clear/inject `dc:description`/`rdf:Alt`/`rdf:li`
- `update_label(path, label)` — write/clear `xmp:Label`

After each write, the file is re-hashed and the recipe's `content_hash` is updated in both catalog and sidecar.

### 14. Collection Store

**Responsibility**: manage static album collections.

**Module**: `src/collection.rs` — dual storage: SQLite tables (`collections`, `collection_assets`) for fast queries + `collections.yaml` at catalog root for persistence across `rebuild-catalog`.

**Operations**:
- `create(name, description)` — create a new collection
- `list()` — list all collections with asset counts
- `show(name)` — list asset IDs in a collection
- `add(name, asset_ids)` — add assets to a collection
- `remove(name, asset_ids)` — remove assets from a collection
- `delete(name)` — delete a collection
- `restore_from_yaml()` — rebuild SQLite tables from YAML (used during `rebuild-catalog`)

### 15. Saved Search Store

**Responsibility**: manage named search queries (smart albums).

**Module**: `src/saved_search.rs` — stored in `searches.toml` at catalog root.

**Operations**:
- `save(name, query, sort, favorite)` — save or replace a named search
- `list()` — list all saved searches with query, sort, and favorite status
- `run(name)` — execute a saved search and return results
- `delete(name)` — delete a saved search

**Favorite field**: Each saved search has a `favorite: bool` field (default `false`). Only favorites are shown as chips on the browse page. The `/saved-searches` management page shows all searches and allows toggling favorites.

### 16. Face Detection Service

> Only present when built with `--features ai`.

**Responsibility**: detect faces in asset images and compute recognition embeddings.

**Module**: `src/face.rs` — `FaceDetector` struct wrapping YuNet (detection) and ArcFace (recognition) ONNX sessions.

**Pipeline**:
1. Load and resize image to YuNet input dimensions (320×320 or 640×640)
2. Run YuNet face detection — produces bounding boxes, landmarks, and confidence scores
3. For each detected face: align and crop the face region, resize to 112×112
4. Run ArcFace recognition — produces a 512-dimensional L2-normalized embedding
5. Generate a 150×150 JPEG crop thumbnail for UI display

**Multi-stride decoding**: YuNet outputs 12 separate tensors at strides 8, 16, and 32 (4 tensors per stride: bounding boxes, landmarks, confidence logits, and class scores). The decoder handles both single-tensor and multi-stride output formats.

### 17. Face Store

> Only present when built with `--features ai`.

**Responsibility**: persist and query face detections and people in SQLite.

**Module**: `src/face_store.rs` — `FaceStore` struct backed by `faces` and `people` tables.

**Operations**:
- `insert_face(face)` — store a detected face with bbox, confidence, embedding, and recognition model
- `faces_for_asset(asset_id)` — list all faces detected in an asset
- `create_person()` — create a new unnamed person
- `name_person(id, name)` — assign a name to a person
- `assign_face(face_id, person_id)` — link a face to a person
- `unassign_face(face_id)` — remove a face from its person
- `merge_people(target_id, source_id)` — move all faces from source to target person
- `delete_person(id)` — delete a person (faces become unassigned)
- `auto_cluster(threshold, apply)` — group similar face embeddings into people using agglomerative hierarchical clustering

**Clustering algorithm**: Loads all unassigned face embeddings of the active `recognition_model` (faces from another model are skipped with a warning), then merges clusters bottom-up with **average linkage (UPGMA)** — the Lance-Williams update keeps the inter-cluster average similarity exact — until no pair exceeds `[ai] face_cluster_threshold` (default 0.35). Order-independent, unlike the greedy single-linkage it replaced in v4.4.0. Each cluster becomes a new person; singletons are skipped. Faces below `[ai] face_min_confidence` (default 0.7) are excluded.

### 18. Stack Store

**Responsibility**: manage asset stacks (scene groupings).

**Module**: `src/stack.rs` — dual storage: SQLite `stacks` table for fast queries + `stacks.yaml` at catalog root for persistence across `rebuild-catalog`.

**Operations**:
- `create(asset_ids)` — create a new stack (minimum 2 assets, first is the pick)
- `add(reference_asset_id, new_asset_ids)` — add assets to an existing stack
- `remove(asset_ids)` — remove assets from their stacks (auto-dissolves if <=1 member remains)
- `set_pick(asset_id)` — set an asset as the stack pick (position 0)
- `dissolve(asset_id)` — dissolve the entire stack
- `list()` — list all stacks with summary info
- `stack_for_asset(asset_id)` — get the stack and ordered members for an asset
- `export_all()` — export all stacks to `StacksFile` for YAML persistence
- `import_from_yaml(file)` — rebuild SQLite from YAML (used during `rebuild-catalog`)

**Browse grid integration**: When stacks are collapsed (default), only the pick (position 0) is shown. Stack badges indicate member count.

### 19. Query Image (search by image)

> Only present when built with `--features ai`.

**Responsibility**: find catalog assets similar to a local image file that is *not* in the catalog, without importing it.

**Module**: `src/query_image.rs` (core) and `src/web/routes/ai/query_image.rs` (upload endpoint).

**Two stages**: (1) hash the file (`ContentStore::hash_file`) and look for a byte-identical variant (`Catalog::find_asset_id_by_variant`) — an exact copy is answered without loading the model; (2) encode the file with the active SigLIP model (`encode_query_image`; RAW/video are rendered first with `PreviewGenerator::generate_to` into a scratch directory, non-picture formats are rejected) and rank the in-memory `EmbeddingIndex` through `embedding_store::rank_similar`, the ranker `similar:<id>` uses as well — the exact match is pinned first at 1.0, `min_sim:` floors the rest. `resolve_query_image` tolerates a missing model when an exact match exists (returns a warning), and fails otherwise. Nothing is written to the catalog.

**Consumers**: `QueryEngine::search_with_image` (`maki search --image`, rows come back score-sorted with `similarity` / `exact_match`), and the web session store `QueryImageSessions` (token → embedding, exact match, thumbnail; 1 h TTL, capped), referenced from the browse URL as `similar:@<token>` so every browse consumer resolves it like `similar:<id>`.

### 20. Audio Pipeline

**Responsibility**: index audio files like photos and videos without ever processing audio inside MAKI.

**Modules**: `src/preview.rs` (`extract_audio_source_metadata`, waveform info card), `src/asset_service/audio.rs` (`maki audio analyze`), `src/web/routes/media.rs` (`GET /audio/{hash}`).

**Extraction**: `lofty` reads duration, bitrate, sample rate, channels and embedded title / artist / album into the variant's `source_metadata` under `audio_*` keys at import (also `refresh --media` and the detail-page lazy backfill). `insert_asset` denormalizes them into the typed `assets` columns (schema v11), which back the `duration:`, `key:` and `bpm:` filters.

**Previews**: the audio info card carries a waveform strip rendered by ffmpeg `showwavespic` (peak envelope); without ffmpeg it degrades to the plain info card.

**Analysis**: `maki audio analyze [QUERY] [--force] [--dry-run]` runs the external analyzers configured in `[audio] key_command` (default `keyfinder-cli`) and `bpm_command` (default `beat_this`, BPM derived from the median inter-beat interval) and stores `audio_key` / `audio_bpm`. Missing tools warn and skip their half; analyzed assets are skipped unless `--force`.

### 21. CLI

**Global flags** (defaults for `log` / `time` / `verbose` can be set in `[cli]`):
- `--json` — output machine-readable JSON
- `-l` / `--log` — log individual file progress (import, verify, sync, refresh, cleanup, generate-previews); per-request logging for `serve`
- `-v` / `--verbose` — show operational flow (file counts, volume detection, VLM settings)
- `-d` / `--debug` — show stderr output from external tools (ffmpeg, dcraw, dcraw_emu, curl); implies `--verbose`
- `-t` / `--time` — show elapsed time after command execution

53 top-level commands (95 leaf commands counting subcommands); the full list with flags is in `doc/manual/reference/0[1-5]-*-commands.md` and `CLAUDE.md`. Representative forms:

**Subcommands**:
```
maki init                                          # initialize a new catalog
maki volume add <label> <path>                     # register a volume
maki volume list                                   # list volumes and status
maki import <paths...> [--volume V] [--include G] [--skip G]  # import files
maki search <query> [--format F] [-q]              # search assets (see the filter reference)
maki search --image <file> [QUERY] [--limit N]     # similar to a local image not in the catalog (ai feature)
maki show <asset-id>                               # show asset details
maki tag <asset-id> [--remove] <tags...>           # add/remove tags
maki edit <id> [--name N] [--description T] [--rating R] [--label C] [--role ROLE --variant HASH] [--clear-*]  # edit metadata
maki delete <ids...> [--apply] [--remove-files]    # remove assets from catalog
maki contact-sheet <query> <output> [--columns N] [--thumb-size N] [--title T] [--metadata]  # generate thumbnail grid image
maki group <variant-hashes...>                     # group variants into one asset
maki split <asset-id> <variant-hashes...>          # split variants into new assets
maki relocate <id> <vol> [--remove-source] [--dry-run]  # copy/move asset
maki verify [PATHS...] [--volume V] [--asset ID] [--include G] [--skip G] [--max-age N] [--force]  # check file integrity
maki sync <PATHS...> [--volume V] [--apply] [--remove-stale]  # reconcile catalog with disk
maki refresh [PATHS...] [--volume V] [--asset ID] [--dry-run] [--media]  # re-read metadata from changed sidecars
maki update-location <id> --from <old> --to <new> [--volume V]  # update path after manual move
maki cleanup [--volume V] [--list] [--apply]       # remove stale locations, orphaned assets, and previews
maki duplicates [--same-volume] [--cross-volume] [--volume V] [--filter-format F] [--path P] [--format FMT]  # find duplicates
maki dedup [--volume V] [--prefer S] [--filter-format F] [--path P] [--min-copies N] [--apply]  # remove same-volume duplicates
maki generate-previews [PATHS...] [--asset ID] [--volume V] [--include G] [--skip G] [--force]  # generate thumbnails
maki stats [--types] [--volumes] [--tags] [--verified] [--all] [--limit N]  # catalog statistics
maki auto-group [QUERY] [--apply]                  # group assets by filename stem
maki embed [--query Q] [--asset ID] [--volume V] [--model M] [--force]  # generate embeddings (ai feature)
maki faces detect|cluster|people|name|merge|delete-person|unassign|download  # face recognition (ai feature)
maki fix-roles [PATHS...] [--volume V] [--asset ID] [--apply]  # fix variant roles in RAW+non-RAW groups
maki saved-search save|list|run|delete             # manage saved searches (alias: ss, save supports --favorite)
maki collection create|list|show|add|remove|delete # manage collections (alias: col)
maki rebuild-catalog                               # rebuild SQLite from sidecars
maki describe [--query Q] [--asset ID] [--volume V] [--mode M] [--apply]  # VLM image descriptions/tags
maki serve [--port P] [--bind ADDR] [--read-only] [--log]  # start web UI server (--log for request logging)
maki watch <paths...> [--interval S] [--volume V]  # poll directories and auto-import new files
maki undo | history [--asset ID]                   # edit-history journal
maki doctor [--sample N] [--repair]                 # sidecar ↔ SQLite consistency check
maki trash list|restore|empty                       # deletion quarantine
maki audio analyze [QUERY] [--force] [--dry-run]    # external key/BPM detection
maki auto-stack [QUERY] [--threshold N] [--apply]  # similarity clustering into stacks (ai feature)
maki status                                         # catalog health summary
```

## Catalog Directory Structure

```
<catalog_root>/                       # wherever `maki init` was run
  maki.toml                           # catalog configuration
  catalog.db                          # SQLite index (derived, rebuildable; -wal/-shm while open)
  volumes.yaml                        # volume registry (device registry)
  searches.toml                       # saved search definitions
  collections.yaml                    # collection membership   (persist across rebuild-catalog)
  stacks.yaml                         # stack membership         (persist across rebuild-catalog)
  faces.yaml, people.yaml             # face / person records    (ai; persist across rebuild-catalog)
  vocabulary.yaml                     # default AI tag vocabulary copied by `maki init`
  .gitignore                          # ignores catalog.db*, previews/, smart-previews/, embeddings/, faces/ (for git-based sidecar backup)
  metadata/
    .write.lock                       # advisory lock serializing sidecar writes
    55/
      550e8400-e29b-41d4-...yaml      # asset sidecar files, sharded by UUID prefix (source of truth)
  previews/
    ab/
      abcdef1234....jpg               # thumbnails, sharded by content hash prefix
  smart-previews/
    ab/
      abcdef1234....jpg               # larger "smart" previews (same sharding)
  embeddings/
    <model-id>/ab/<asset-id>.bin      # SigLIP embeddings as raw f32 (rebuild resilience)
    arcface/ab/<face-id>.bin          # ArcFace face embeddings
  faces/
    ab/<face-id>.jpg                  # 150×150 face crop thumbnails, served under /face/
  history/
    <epoch-millis>-<opid>.json        # edit-history journal (maki undo / history); non-authoritative
    undone/                           # operations that were undone
  .trash/
    <date>/<volume>/...               # quarantined files and sidecars from deleting operations (maki trash)
```
