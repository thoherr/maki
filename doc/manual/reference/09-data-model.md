# Data Model

This page documents every entity in the MAKI data model, their fields, relationships, and storage mechanisms.

---

## Entities

### Asset

The top-level entity. An Asset represents a single logical media item -- "photo of sunset at the beach" -- regardless of how many physical files exist for it.

| Field | Type | Description |
|-------|------|-------------|
| `id` | UUID | Primary key. Deterministic UUID v5 derived from the content hash of the first variant. Same content always produces the same asset ID. |
| `name` | Option\<String\> | User-assigned display name. When absent, the UI shows `original_filename` as a fallback. |
| `original_filename` | String | Filename of the primary variant at import time (e.g. `DSC_4521.NEF`). Used as display fallback and for stem-based grouping. |
| `asset_type` | AssetType | One of: `image`, `video`, `audio`, `document`, `other`. Inferred from file extension at import. |
| `description` | Option\<String\> | Free-text description. Extracted from XMP `dc:description` during import, or set manually. |
| `tags` | Vec\<String\> | Keyword list. Merged from XMP `dc:subject`, embedded XMP, and manual tagging. Deduplicated. |
| `tag_sources` | BTreeMap\<String, TagSource\> | Provenance per tag value: `user`, `xmp-import`, `auto-tag`, or `vlm`. A tag absent from the map is `user`. Only tags with a machine source are stored, so pre-provenance sidecars stay byte-identical. Mutated only through `Asset::add_tags_with_source` / `remove_tags` / `rename_tag_value`; mirrored in the SQLite `tag_sources` column (schema v10) and checked by `maki doctor`. |
| `rating` | Option\<u8\> | Star rating, 1--5. Extracted from XMP `xmp:Rating` during import, or set manually. |
| `color_label` | Option\<String\> | One of 7 canonical colors: Red, Orange, Yellow, Green, Blue, Pink, Purple. Extracted from XMP `xmp:Label`, or set manually. Stored as title-case English name. |
| `created_at` | DateTime\<Utc\> | Creation timestamp. Preferentially from EXIF `DateTimeOriginal`, falling back to filesystem modification time. |
| `face_scan_status` | Option\<String\> | `None` = never scanned for faces; `"done"` = scan completed (whether or not faces were found). Keeps zero-face assets from being re-scanned after a `rebuild-catalog`. *(Pro)* |
| `preview_rotation` | Option\<u16\> | Manual preview rotation override in degrees (0/90/180/270). |
| `preview_variant` | Option\<String\> | Content hash of a user-chosen preview representative, overriding the [Display Priority](#display-priority) algorithm. |
| `variants` | Vec\<Variant\> | The physical files belonging to this asset (in YAML sidecar). |
| `recipes` | Vec\<Recipe\> | Processing sidecars attached to this asset's variants (in YAML sidecar). |

**Denormalized columns** (SQLite only, computed at write time to avoid expensive JOINs):

| Column | Type | Description |
|--------|------|-------------|
| `best_variant_hash` | String | Content hash of the best display variant (see [Display Priority](#display-priority)). Used for the browse grid JOIN. |
| `primary_variant_format` | String | Identity format of the asset. Prefers Original+RAW, then Original+any, then best variant's format. Shown on browse cards (e.g. "NEF"). |
| `variant_count` | Integer | Number of variants. Shown as a badge on browse cards (e.g. "3v"). |
| `face_count` | Integer | Number of detected faces. Shown as a badge on browse cards. *(Pro)* |
| `stack_id` | Option\<UUID\> | Foreign key to the Stack this asset belongs to. `None` if unstacked. |
| `stack_position` | Option\<Integer\> | Position within the stack (0 = pick). `None` if unstacked. |
| `latitude`, `longitude` | Option\<Real\> | GPS position lifted from variant metadata (map view, `geo:` filter). |
| `preview_rotation` | Option\<Integer\> | Mirror of the sidecar field (schema v3). |
| `preview_variant` | Option\<String\> | Mirror of the sidecar field (schema v3). |
| `duration_seconds` | Option\<Real\> | Media duration for audio and video (`duration:` filter, duration badges). Added as `video_duration` in schema v4, renamed in v11 when audio started filling it. |
| `video_codec` | Option\<String\> | Video codec name (`codec:` filter, schema v5). |
| `face_scan_status` | Option\<String\> | Mirror of the sidecar field (schema v7). |
| `leaf_tag_count` | Integer | Number of leaf tags (`tagcount:` filter, schema v8). |
| `tag_sources` | Text (JSON) | Mirror of the sidecar map (schema v10). |
| `audio_sample_rate`, `audio_channels`, `audio_bitrate` | Option\<Integer\> | Typed audio properties from variant metadata (schema v11). |
| `audio_key` | Option\<String\> | Musical key (`key:` filter; from `maki audio analyze`, schema v11). |
| `audio_bpm` | Option\<Real\> | Tempo (`bpm:` filter; from `maki audio analyze`, schema v11). |

The variant-related columns are updated by `insert_asset()`, `update_denormalized_variant_columns()`, and `fix_roles`. The stack columns are updated by `StackStore` operations, `face_count` by `FaceStore::update_face_count`. All are backfilled during schema migration and rebuilt by `rebuild-catalog`, with one exception: the v6→v7 migration does not rewrite every sidecar to stamp `face_scan_status` (too slow on large catalogs); `rebuild-catalog` stamps `done` on assets that have face records, and `faces detect` writes the flag for every asset it touches from then on.

### Variant

A concrete file belonging to an Asset. A RAW file, its JPEG conversion, and a high-res TIFF export are three Variants of the same Asset.

| Field | Type | Description |
|-------|------|-------------|
| `content_hash` | String | Primary key. SHA-256 hash of the file contents. The same file always produces the same hash regardless of where it is stored. |
| `asset_id` | UUID | Foreign key to the parent Asset. |
| `role` | VariantRole | Purpose within the asset group: `original`, `alternate`, `processed`, `export`, or `sidecar`. See [Variant Roles](#variant-roles). |
| `format` | String | Lowercase file extension without dot (e.g. `nef`, `jpg`, `tif`, `mp4`). |
| `file_size` | u64 | File size in bytes. |
| `original_filename` | String | Filename at import time (e.g. `DSC_4521.NEF`). |
| `source_metadata` | HashMap\<String, String\> | Key-value pairs from EXIF and XMP extraction (camera model, lens, GPS coordinates, creator, rights, etc.). |
| `locations` | Vec\<FileLocation\> | Where this file physically exists on disk (in YAML sidecar; stored in a separate `file_locations` table in SQLite). |

**Indexed metadata columns** (SQLite only, extracted from `source_metadata` for fast filtering):

| Column | Type | Description |
|--------|------|-------------|
| `camera_model` | String | Camera body (e.g. "NIKON Z 9") |
| `lens_model` | String | Lens (e.g. "NIKKOR Z 50mm f/1.2 S") |
| `focal_length_mm` | Real | Focal length in millimeters |
| `f_number` | Real | Aperture f-number |
| `iso` | Integer | ISO sensitivity |
| `image_width` | Integer | Image width in pixels |
| `image_height` | Integer | Image height in pixels |

### FileLocation

A pointer to where a Variant physically lives on disk. A single Variant can have multiple FileLocations -- copies on different drives, backups, archives.

| Field | Type | Description |
|-------|------|-------------|
| `id` | Integer | Primary key (auto-increment, SQLite only). |
| `content_hash` | String | Foreign key to the parent Variant. |
| `volume_id` | UUID | Foreign key to the Volume where the file resides. |
| `relative_path` | String | Path relative to the volume's mount point (e.g. `Capture/2026-02-22/DSC_4521.NEF`). |
| `verified_at` | Option\<DateTime\<Utc\>\> | Timestamp of the last successful integrity check via `maki verify`. `None` if never verified. |

### Recipe

A processing sidecar file attached to a Variant. Each Recipe record represents one physical file on one volume. When the same XMP file exists on multiple volumes (e.g. backup copies), there are multiple Recipe records with the same `content_hash` but different locations — conceptually one recipe with multiple locations, mirroring how Variants work. The detail page and stats page group recipes by `(variant_hash, content_hash)` to reflect this: "1 recipe, 3 locations" rather than "3 recipes".

When importing, if a recipe's content hash is already known for the asset (from any volume), the recipe file is tracked for location purposes but its metadata is **not re-merged** — this prevents stale XMP data from backup copies from overwriting curated tags or ratings.

| Field | Type | Description |
|-------|------|-------------|
| `id` | UUID | Primary key. |
| `variant_hash` | String | Foreign key to the parent Variant. |
| `software` | String | Processing tool identifier: `XMP`, `CaptureOne`, `RawTherapee`, `DxO`, `ON1`. |
| `recipe_type` | RecipeType | Either `sidecar` (external file) or `embedded_export`. |
| `content_hash` | String | SHA-256 hash of the recipe file. Updated when the file changes on disk. |
| `volume_id` | UUID | Foreign key to the Volume where the recipe file resides. |
| `relative_path` | String | Path relative to the volume's mount point. |
| `verified_at` | Option\<DateTime\<Utc\>\> | Last verification timestamp. |
| `pending_writeback` | bool | `true` when metadata was edited in MAKI but the `.xmp` file could not be updated yet (volume offline, or `[writeback] enabled = false`). Cleared by a successful `maki writeback`. In SQLite since schema v2. |

**Supported recipe file extensions**:

| Extension | Software |
|-----------|----------|
| `.xmp` | XMP (Lightroom, CaptureOne, Adobe) |
| `.cos` | CaptureOne settings |
| `.cot` | CaptureOne output |
| `.cop` | CaptureOne process |
| `.pp3` | RawTherapee |
| `.dop` | DxO PhotoLab |
| `.on1` | ON1 Photo RAW |

### Volume

A registered storage device. Volumes give MAKI a stable reference to storage that may come and go (external drives, network shares).

| Field | Type | Description |
|-------|------|-------------|
| `id` | UUID | Primary key (random UUID v4). |
| `label` | String | Human-readable name (e.g. "Photos SSD", "Archive NAS"). |
| `mount_point` | PathBuf | Filesystem path where the volume is mounted (e.g. `/Volumes/Photos`). |
| `volume_type` | VolumeType | One of: `local`, `external`, `network`. |
| `purpose` | Option\<VolumePurpose\> | Logical role: `media` (transient source — memory cards), `working` (active editing), `archive` (long-term primary), `backup` (redundancy), `cloud` (sync folder). Optional — unclassified if not set. Used by duplicate analysis and backup coverage commands. Media volumes excluded from backup coverage. |
| `is_online` | bool | Computed at runtime -- `true` if `mount_point` exists on disk. Not persisted. |

### Collection

A manually curated list of assets (static album). Backed by both SQLite (for fast queries) and `collections.yaml` (for persistence across catalog rebuilds).

| Field | Type | Description |
|-------|------|-------------|
| `id` | UUID | Primary key. |
| `name` | String | Unique human-readable name (e.g. "Portfolio", "Client Deliverables"). |
| `description` | Option\<String\> | Optional description text. |
| `created_at` | DateTime\<Utc\> | When the collection was created. |
| `asset_ids` | Vec\<String\> | Ordered list of asset UUIDs (in YAML). In SQLite, this is a separate `collection_assets` join table with `(collection_id, asset_id, added_at)`. |

### Stack

A lightweight anonymous group of assets for visually related images (burst shots, bracketing sequences, similar scenes). In the browse grid, stacked assets are collapsed to show only the "pick" image with a count badge.

| Field | Type | Description |
|-------|------|-------------|
| `id` | UUID | Primary key (random UUID v4). |
| `created_at` | DateTime\<Utc\> | When the stack was created. |
| `asset_ids` | Vec\<String\> | Ordered list of asset UUIDs. Index 0 is the pick (displayed in browse grid). |

**Constraints**:
- Each asset can belong to at most one stack.
- A stack must have at least 2 members. Removing members that would leave fewer than 2 causes automatic dissolution.
- Stack membership is denormalized onto the `assets` table as `stack_id` (FK to the stack) and `stack_position` (integer, 0 = pick). These columns enable efficient filtering (`stacked:true/false`) and stack collapsing in browse queries without joining a separate table.

**Storage**: Stacks are persisted in `stacks.yaml` at the catalog root (alongside `collections.yaml` and `searches.toml`). The SQLite `stacks` table and the `stack_id`/`stack_position` columns on `assets` are derived from this file and rebuilt by `rebuild-catalog`.

### SavedSearch

A named query (smart album) stored in `searches.toml`. Re-evaluated every time it is run, so results update automatically as the catalog changes.

| Field | Type | Description |
|-------|------|-------------|
| `name` | String | Unique identifier. |
| `query` | String | Search filter string in the same syntax as `maki search` (e.g. `type:image tag:landscape rating:4+`). |
| `sort` | Option\<String\> | Sort order (e.g. `date_desc`, `name_asc`). Omitted means default (`date_desc`). |
| `favorite` | bool | Whether the search is shown as a chip on the browse page. |

### Embedding *(Pro)*

A stored SigLIP image embedding for an asset, produced by `maki embed`, `maki import --embed`, or as a side effect of `maki auto-tag`; it powers the `similar:` filter, `maki search --image`, the stroll page, and `maki auto-stack`.

| Field | Type | Description |
|-------|------|-------------|
| `asset_id` | String | Foreign key to the parent Asset. Primary key together with `model`. |
| `model` | String | Model identifier (default: `siglip-vit-b16-256`). Embeddings from different models are never compared; switching models only generates the missing rows. |
| `embedding` | Blob | float32 vector, stored as little-endian binary. The dimension is per model: 768 for `siglip-vit-b16-256` and `siglip2-base-256-multi` (3072 bytes), 1024 for `siglip-vit-l16-256` and `siglip2-large-256-multi` (4096 bytes). |

Storage overhead: ~3--4 KB per asset and model. For 100,000 assets with the default model: ~300 MB in SQLite, plus a binary copy under `embeddings/<model>/` for rebuild resilience.

**In-memory index**: For fast similarity search, the web server loads all embeddings into an `EmbeddingIndex` — a contiguous `Vec<f32>` buffer — on first query. Search uses dot product (SigLIP embeddings are L2-normalized) with a min-heap for top-K selection. At 100k assets, search completes in <10ms. The index is updated in-place when new embeddings are stored.

**Opportunistic storage**: Embeddings are stored not only by `maki auto-tag` and `maki embed`, but also opportunistically by the web UI "Suggest tags" and batch "Auto-tag" endpoints. This means using AI features in the web UI gradually builds up the similarity search index.

### Face *(Pro)*

A detected face within an asset image, with bounding box, confidence, recognition embedding, and optional person assignment.

| Field | Type | Description |
|-------|------|-------------|
| `id` | UUID | Primary key. |
| `asset_id` | UUID | Foreign key to the parent Asset. |
| `person_id` | Option\<UUID\> | Foreign key to the assigned Person. `None` if unassigned. |
| `bbox_x` | f32 | Bounding box X position (normalized 0–1). |
| `bbox_y` | f32 | Bounding box Y position (normalized 0–1). |
| `bbox_w` | f32 | Bounding box width (normalized 0–1). |
| `bbox_h` | f32 | Bounding box height (normalized 0–1). |
| `confidence` | f32 | Detection confidence score (0–1). |
| `embedding` | Blob | 512-dimensional float32 ArcFace vector (2048 bytes), stored as little-endian binary. The face is aligned to a canonical 112×112 template via a 5-point similarity transform before embedding. |
| `recognition_model` | Option\<String\> | Identifier of the ArcFace variant that produced this embedding (e.g. `arcface-resnet100-fp32-aligned-v2`). Added in schema v6. Clustering only mixes embeddings from the same model — older ones are skipped with a warning until re-embedded via `maki faces detect --force`. |
| `created_at` | DateTime\<Utc\> | When the face was detected. |

The 150×150 JPEG crop thumbnail is not a field: its path is derived from the face ID as `faces/<2-char prefix>/<face_id>.jpg` under the catalog root. Storage overhead: ~2 KB per face (embedding + metadata). Face crops: ~5–15 KB each as JPEG.

### Person *(Pro)*

A named or unnamed person group linking detected faces across assets.

| Field | Type | Description |
|-------|------|-------------|
| `id` | UUID | Primary key. |
| `name` | Option\<String\> | User-assigned name. `None` for unnamed clusters. |
| `representative_face_id` | Option\<UUID\> | Foreign key to the face used as the person's thumbnail. |
| `created_at` | DateTime\<Utc\> | When the person record was created. |

---

## Variant Roles

Each Variant carries a `role` that describes its purpose within the asset group.

| Role | Meaning | Examples |
|------|---------|----------|
| **Original** | Camera source file. Each asset should have exactly one. | NEF, ARW, CR3, in-camera JPEG |
| **Alternate** | Secondary variant from grouping (e.g., JPEG paired with RAW original). | In-camera JPEG grouped with RAW |
| **Processed** | An edited or intermediate version, not straight from camera. | PSD, layered TIFF, edited DNG |
| **Export** | A derivative output produced by an editing tool. | Resized JPEG, web TIFF, final deliverable |
| **Sidecar** | A non-media sidecar file imported as a variant. | Embedded metadata files |

When assets are merged via `maki group` or `maki auto-group`, donor variants with the `original` role are automatically re-roled to `alternate` to avoid having multiple originals in one asset.

---

## Display Priority

The preview selection algorithm determines which variant is shown in the browse grid and asset detail page. The scoring follows this priority:

1. **Role** (highest weight): Export (300) > Processed (200) > Original (100) > Alternate (50) > Sidecar (0)
2. **Format bonus** (+50): Standard image formats (`jpg`, `jpeg`, `png`, `tiff`, `tif`, `webp`) are preferred over RAW
3. **File size tiebreak**: Larger files score slightly higher (up to +49 points)

This means a JPEG export is preferred over a RAW original for display, showing your best-quality deliverable in the browse grid rather than the camera file.

The `best_variant_hash` denormalized column caches this computation so the browse grid can join directly to the best variant without evaluating all variants per asset.

---

## Storage

maki uses a dual-storage architecture. Neither tier alone is sufficient; together they provide both robustness and performance.

### YAML Sidecar Files (source of truth)

One `.yaml` file per Asset, stored at `metadata/<id-prefix>/<id>.yaml` within the catalog directory. Writes take an advisory lock on `metadata/.write.lock` so concurrent MAKI processes cannot interleave sidecar writes. Contains the complete Asset record: metadata, all Variants (with their FileLocations and source_metadata), and all Recipes. Human-readable, diffable, and version-control friendly.

```
catalog/
  metadata/
    3a/
      3a7b1e02-4fd2-4a6b-9c1d-e75a0bf3284c.yaml
    f1/
      f1c8d9e0-...yaml
```

### SQLite Catalog (derived cache)

A single `catalog.db` file providing fast indexed queries. Contains denormalized columns for efficient browse-grid rendering. The catalog is always rebuildable from the YAML sidecars via `maki rebuild-catalog` -- it is a performance optimization, not a source of truth.

**Tables**: `assets`, `variants`, `file_locations`, `volumes`, `recipes`, `collections`, `collection_assets`, `stacks`, `embeddings` (created in every build; only filled by Pro), `schema_version` (single row, see [Schema Migrations](#schema-migrations)), `assets_fts` (FTS5 trigram index over name, filename, description and source metadata, kept current by six triggers on `assets` and `variants` -- schema v9), and `faces`, `people` (*(Pro)* builds only)

**Performance indexes** (created automatically via schema migrations):

- `variants(asset_id)`, `variants(format)`, `variants(camera_model)`, `variants(lens_model)`, `variants(iso)`, `variants(focal_length_mm)` — variant lookups and filter queries
- `file_locations(content_hash)`, `file_locations(volume_id)` — join and volume filter performance
- `assets(created_at)`, `assets(best_variant_hash)` — sort-by-date and best-variant join
- `assets(stack_id)`, partial `assets(stack_position, created_at DESC) WHERE stack_id IS NOT NULL` — stack collapsing in the browse grid
- partial `assets(latitude, longitude) WHERE latitude IS NOT NULL` — map view and `geo:` filter
- partial `assets(face_count) WHERE face_count > 0`, partial `assets(face_scan_status) WHERE face_scan_status IS NULL` — `faces:` filter and "assets still to scan" lookups
- `assets(leaf_tag_count)` — `tagcount:` filter
- `recipes(variant_hash)` — recipe lookups by variant
- `collection_assets(asset_id)` — collection membership queries
- `faces(asset_id)`, `faces(person_id)` — face lookups per asset and per person *(Pro)*

### Schema Migrations

The `schema_version` table stores the catalog's schema version (`SCHEMA_VERSION` in `src/catalog.rs`, currently 11). Every command except `init` and `migrate` checks it at startup with one query and exits with an error if the catalog is older than the binary expects; migrations themselves run only in `maki init`, `maki migrate`, and `maki rebuild-catalog`. Migrations are idempotent `ALTER TABLE ... ADD COLUMN` / `CREATE INDEX IF NOT EXISTS` blocks with guarded backfills, executed only for versions above the stored one.

### Other Files

| File | Format | Contents |
|------|--------|----------|
| `volumes.yaml` | YAML | Registered volume definitions (id, label, mount_point, type) |
| `searches.toml` | TOML | Saved search definitions (name, query, sort) |
| `collections.yaml` | YAML | Collection definitions with ordered asset ID lists |
| `stacks.yaml` | YAML | Stack definitions with ordered asset ID lists |
| `maki.toml` | TOML | User configuration (see [Configuration](08-configuration.md)) |
| `vocabulary.yaml` | YAML | Tag vocabulary for auto-tagging, seeded with the built-in default by `maki init` |
| `.gitignore` | text | Written by `maki init`; excludes the derived files below so a catalog can be version-controlled |
| `previews/<prefix>/<hash>.jpg` | JPEG | Preview thumbnails keyed by variant content hash (`.webp` when `[preview] format = "webp"`) |
| `smart-previews/<prefix>/<hash>.jpg` | JPEG | Larger smart previews (same keying) |
| `embeddings/<model>/<prefix>/<asset_id>.bin` | binary | Image embeddings, one file per asset and model — rebuild source for the `embeddings` table *(Pro)* |
| `embeddings/arcface/<prefix>/<face_id>.bin` | binary | Face recognition embeddings — rebuild source for `faces.embedding` *(Pro)* |
| `faces.yaml`, `people.yaml` | YAML | Face and person records — rebuild source for the `faces` / `people` tables *(Pro)* |
| `faces/<prefix>/<face_id>.jpg` | JPEG | Face crop thumbnails (150×150, *(Pro)*) |
| `history/<epoch-millis>-<opid>.json`, `history/undone/` | JSON | Edit-history journal for `maki undo` / `maki history`; undone operations move to `undone/`. Independent, non-authoritative, freely deletable |
| `.trash/` | files | Quarantine for deleted files (`delete --remove-files`, `dedup`, web duplicates page); recoverable via `maki trash` |
| `metadata/.write.lock` | lock file | Advisory lock serialising sidecar writes across processes |

### Entity Relationships

```mermaid
erDiagram
    Asset ||--o{ Variant : "has variants"
    Variant ||--o{ FileLocation : "stored at"
    Variant ||--o{ Recipe : "attached recipes"
    FileLocation }o--|| Volume : "on volume"
    Recipe }o--|| Volume : "on volume"
    Collection }o--o{ Asset : "contains"
    Stack ||--o{ Asset : "groups"
    Asset ||--o{ Face : "detected faces"
    Face }o--o| Person : "assigned to"
```

### Content-Addressable Identity

Every file imported into MAKI is hashed with SHA-256. This hash is the file's identity:

- **Deduplication**: Importing the same file twice (even from different paths or drives) recognizes it as the same content and adds the new location to the existing Variant.
- **Integrity verification**: `maki verify` re-hashes files and compares against stored hashes to detect corruption or bit rot.
- **Transparent relocation**: Moving a file to a different drive does not change its identity. `maki relocate` and `maki update-location` update the catalog path.

Originals (RAW files, camera JPEGs) are immutable -- their hash is stable forever. Recipe files are the exception: they are modified by external tools, so MAKI tracks them by location and updates their stored hash when changes are detected.

---

Previous: [Configuration](08-configuration.md)
