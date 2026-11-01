//! `schema` section of `Catalog` — methods extracted from the original
//! 9.2-kLOC catalog.rs into a multi-file `impl Catalog` block.
//!
//! Types, helper functions, and the connection ctor live in the parent
//! `catalog` module.
//!
//! # Migration rules (architecture audit F3 / F4)
//!
//! * One `migrate_vN` per schema version. `run_migrations` runs every
//!   version above the stored one inside its own transaction and writes
//!   the `schema_version` stamp for N **inside that same transaction**:
//!   a failing step rolls back and leaves the catalog at N-1, consistent
//!   and re-runnable. Errors name the version and the step
//!   (`schema migration v11: rename audio columns`), which is what
//!   `maki migrate` prints.
//! * Column additions go through [`Catalog::add_column_if_missing`]
//!   (idempotent via `pragma_table_info`); tables, indexes and triggers
//!   use `IF NOT EXISTS`. Nothing is `let _ =`-ignored.
//! * Backfills that `UPDATE assets` keep a narrow `WHERE`: every row an
//!   UPDATE processes fires the v9/v12 FTS5 triggers even when nothing
//!   changes (see the v11 comment). Do not widen them.
//! * All DDL is feature-independent: the face and embedding tables exist
//!   on every catalog (v13), so a catalog created by a standard build is
//!   ready for a Pro build.

use super::*;
use anyhow::Context;

/// Default SigLIP model id used by the original `embeddings` table and
/// as the fallback when migrating a single-primary-key table.
const LEGACY_EMBEDDING_MODEL: &str = "siglip-vit-b16-256";

/// Legacy ArcFace model id stamped on face rows that predate
/// `faces.recognition_model` (schema v6).
const LEGACY_RECOGNITION_MODEL: &str = "arcface-resnet100-int8";

impl Catalog {
    // ═══ SCHEMA MIGRATIONS ═══

    /// Run schema migrations, skipping work that is already done.
    ///
    /// Reads the stored version once and runs only the `migrate_vN` steps
    /// with `N > current`, each in its own transaction together with its
    /// version stamp. An up-to-date catalog costs exactly one SELECT.
    /// Called from `initialize()` (fresh catalogs, version 0, so every
    /// step runs), `open_and_migrate()` (`maki migrate`) and
    /// `rebuild-catalog`.
    pub fn run_migrations(&self) -> Result<()> {
        let current = self.schema_version();
        if current >= SCHEMA_VERSION {
            return Ok(());
        }

        type Step = fn(&Catalog) -> Result<()>;
        const STEPS: [(u32, Step); SCHEMA_VERSION as usize] = [
            (1, Catalog::migrate_v1),
            (2, Catalog::migrate_v2),
            (3, Catalog::migrate_v3),
            (4, Catalog::migrate_v4),
            (5, Catalog::migrate_v5),
            (6, Catalog::migrate_v6),
            (7, Catalog::migrate_v7),
            (8, Catalog::migrate_v8),
            (9, Catalog::migrate_v9),
            (10, Catalog::migrate_v10),
            (11, Catalog::migrate_v11),
            (12, Catalog::migrate_v12),
            (13, Catalog::migrate_v13),
        ];

        for (version, step) in STEPS {
            if current < version {
                self.with_tx(|c| {
                    step(c)?;
                    c.stamp_schema_version(version)
                })?;
            }
        }
        Ok(())
    }

    /// Initialize the database schema.
    ///
    /// Creates base tables, then delegates to `run_migrations()` for all
    /// ADD COLUMN, CREATE INDEX, backfill, and schema version stamping.
    /// Idempotent.
    pub fn initialize(&self) -> Result<()> {
        self.conn
            .execute_batch(
                "CREATE TABLE IF NOT EXISTS assets (
                id TEXT PRIMARY KEY,
                name TEXT,
                created_at TEXT NOT NULL,
                asset_type TEXT NOT NULL,
                tags TEXT NOT NULL DEFAULT '[]',
                description TEXT
            );

            CREATE TABLE IF NOT EXISTS variants (
                content_hash TEXT PRIMARY KEY,
                asset_id TEXT NOT NULL REFERENCES assets(id),
                role TEXT NOT NULL,
                format TEXT NOT NULL,
                file_size INTEGER NOT NULL,
                original_filename TEXT NOT NULL,
                source_metadata TEXT NOT NULL DEFAULT '{}'
            );

            CREATE TABLE IF NOT EXISTS file_locations (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                content_hash TEXT NOT NULL REFERENCES variants(content_hash),
                volume_id TEXT NOT NULL REFERENCES volumes(id),
                relative_path TEXT NOT NULL,
                verified_at TEXT
            );

            CREATE TABLE IF NOT EXISTS volumes (
                id TEXT PRIMARY KEY,
                label TEXT NOT NULL,
                mount_point TEXT NOT NULL,
                volume_type TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS recipes (
                id TEXT PRIMARY KEY,
                variant_hash TEXT NOT NULL REFERENCES variants(content_hash),
                software TEXT NOT NULL,
                recipe_type TEXT NOT NULL,
                content_hash TEXT NOT NULL,
                volume_id TEXT,
                relative_path TEXT,
                verified_at TEXT
            );",
            )
            .context("creating base tables")?;

        // All columns, indexes, backfills, and version stamping handled by migrations
        self.run_migrations()
    }

    // ─── helpers ───

    /// Record `version` as the current schema version (called inside the
    /// migration transaction, so the stamp commits with the step).
    fn stamp_schema_version(&self, version: u32) -> Result<()> {
        self.conn
            .execute_batch(
                "CREATE TABLE IF NOT EXISTS schema_version (version INTEGER NOT NULL);
                 DELETE FROM schema_version;",
            )
            .with_context(|| format!("schema migration v{version}: reset schema_version"))?;
        self.conn
            .execute(
                "INSERT INTO schema_version (version) VALUES (?1)",
                rusqlite::params![version],
            )
            .with_context(|| format!("schema migration v{version}: stamp schema_version"))?;
        Ok(())
    }

    /// Run one SQL batch of migration `version`, naming the step on error.
    fn step(&self, version: u32, name: &str, sql: &str) -> Result<()> {
        self.conn
            .execute_batch(sql)
            .with_context(|| format!("schema migration v{version}: {name}"))
    }

    /// `true` when `table` exists in `sqlite_master`.
    pub(super) fn table_exists(&self, table: &str) -> Result<bool> {
        let n: i64 = self
            .conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
                rusqlite::params![table],
                |r| r.get(0),
            )
            .with_context(|| format!("checking for table {table}"))?;
        Ok(n > 0)
    }

    /// `true` when `table` has a column named `column`.
    pub(super) fn has_column(&self, table: &str, column: &str) -> Result<bool> {
        let n: i64 = self
            .conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info(?1) WHERE name = ?2",
                rusqlite::params![table, column],
                |r| r.get(0),
            )
            .with_context(|| format!("checking for column {table}.{column}"))?;
        Ok(n > 0)
    }

    /// `ALTER TABLE table ADD COLUMN column decl`, skipped when the column
    /// already exists. Replaces the former `let _ = ... ADD COLUMN` idiom
    /// so a genuine failure (locked database, malformed declaration) is
    /// reported instead of swallowed.
    pub(super) fn add_column_if_missing(&self, table: &str, column: &str, decl: &str) -> Result<()> {
        if self.has_column(table, column)? {
            return Ok(());
        }
        self.conn
            .execute_batch(&format!("ALTER TABLE {table} ADD COLUMN {column} {decl}"))
            .with_context(|| format!("adding column {table}.{column}"))?;
        Ok(())
    }

    // ─── v0 → v1: base columns, indexes, denormalization, backfills ───
    fn migrate_v1(&self) -> Result<()> {
        const V: u32 = 1;
        let ctx = |name: &'static str| move || format!("schema migration v{V}: {name}");

        self.add_column_if_missing("assets", "rating", "INTEGER").with_context(ctx("rating column"))?;
        self.add_column_if_missing("assets", "color_label", "TEXT").with_context(ctx("color_label column"))?;
        for (col, decl) in [
            ("camera_model", "TEXT"),
            ("lens_model", "TEXT"),
            ("focal_length_mm", "REAL"),
            ("f_number", "REAL"),
            ("iso", "INTEGER"),
            ("image_width", "INTEGER"),
            ("image_height", "INTEGER"),
        ] {
            self.add_column_if_missing("variants", col, decl).with_context(ctx("variant metadata columns"))?;
        }
        self.step(
            V,
            "variant metadata indexes",
            "CREATE INDEX IF NOT EXISTS idx_variants_camera ON variants(camera_model);
             CREATE INDEX IF NOT EXISTS idx_variants_lens ON variants(lens_model);
             CREATE INDEX IF NOT EXISTS idx_variants_iso ON variants(iso);
             CREATE INDEX IF NOT EXISTS idx_variants_focal ON variants(focal_length_mm);",
        )?;
        // Backfill metadata columns from existing JSON
        self.step(
            V,
            "backfill variant metadata columns",
            "UPDATE variants SET
                camera_model = json_extract(source_metadata, '$.camera_model'),
                lens_model = json_extract(source_metadata, '$.lens_model'),
                iso = CAST(json_extract(source_metadata, '$.iso') AS INTEGER),
                focal_length_mm = CAST(REPLACE(json_extract(source_metadata, '$.focal_length'), ' mm', '') AS REAL),
                f_number = CAST(json_extract(source_metadata, '$.f_number') AS REAL),
                image_width = CAST(json_extract(source_metadata, '$.image_width') AS INTEGER),
                image_height = CAST(json_extract(source_metadata, '$.image_height') AS INTEGER)
            WHERE camera_model IS NULL AND source_metadata != '{}'",
        )?;
        // best_variant_hash denormalization. The SQL scoring here is the
        // historical pre-v4.12.3 rule and runs only on catalogs older than
        // schema v1; the Rust rule in `update_denormalized_variant_columns`
        // is authoritative for everything written since.
        self.add_column_if_missing("assets", "best_variant_hash", "TEXT").with_context(ctx("best_variant_hash column"))?;
        self.step(
            V,
            "variants.asset_id index",
            "CREATE INDEX IF NOT EXISTS idx_variants_asset_id ON variants(asset_id)",
        )?;
        self.step(
            V,
            "backfill best_variant_hash",
            "UPDATE assets SET best_variant_hash = (
                SELECT content_hash FROM variants WHERE asset_id = assets.id
                ORDER BY
                    CASE role WHEN 'export' THEN 300 WHEN 'processed' THEN 200
                        WHEN 'original' THEN 100 ELSE 0 END +
                    CASE WHEN LOWER(format) IN ('jpg','jpeg','png','tiff','tif','webp')
                        THEN 50 ELSE 0 END +
                    MIN(file_size / 1000000, 49)
                DESC LIMIT 1
            ) WHERE best_variant_hash IS NULL",
        )?;
        // primary_variant_format + variant_count denormalization
        self.add_column_if_missing("assets", "primary_variant_format", "TEXT").with_context(ctx("primary_variant_format column"))?;
        self.add_column_if_missing("assets", "variant_count", "INTEGER NOT NULL DEFAULT 0").with_context(ctx("variant_count column"))?;
        self.step(
            V,
            "backfill primary_variant_format",
            "UPDATE assets SET primary_variant_format = COALESCE(
                (SELECT format FROM variants WHERE asset_id = assets.id AND role = 'original'
                 AND LOWER(format) IN ('raw','cr2','cr3','nef','arw','orf','rw2','dng','raf','pef','srw')
                 LIMIT 1),
                (SELECT format FROM variants WHERE asset_id = assets.id AND role = 'original' LIMIT 1),
                (SELECT format FROM variants WHERE content_hash = assets.best_variant_hash)
            ) WHERE primary_variant_format IS NULL",
        )?;
        self.step(
            V,
            "backfill variant_count",
            "UPDATE assets SET variant_count = (
                SELECT COUNT(*) FROM variants WHERE asset_id = assets.id
            ) WHERE variant_count = 0",
        )?;
        // Stack columns on assets (the stack / collection tables themselves
        // joined the versioned schema in v13).
        self.add_column_if_missing("assets", "stack_id", "TEXT").with_context(ctx("stack_id column"))?;
        self.add_column_if_missing("assets", "stack_position", "INTEGER").with_context(ctx("stack_position column"))?;
        self.step(
            V,
            "stack_id index",
            "CREATE INDEX IF NOT EXISTS idx_assets_stack_id ON assets(stack_id);",
        )?;
        // Volume purpose
        self.add_column_if_missing("volumes", "purpose", "TEXT").with_context(ctx("volumes.purpose column"))?;
        // Performance indexes
        self.step(
            V,
            "performance indexes",
            "CREATE INDEX IF NOT EXISTS idx_fl_content_hash ON file_locations(content_hash);
             CREATE INDEX IF NOT EXISTS idx_fl_volume_id ON file_locations(volume_id);
             CREATE INDEX IF NOT EXISTS idx_assets_created_at ON assets(created_at);
             CREATE INDEX IF NOT EXISTS idx_assets_best_variant_hash ON assets(best_variant_hash);
             CREATE INDEX IF NOT EXISTS idx_variants_format ON variants(format);
             CREATE INDEX IF NOT EXISTS idx_recipes_variant_hash ON recipes(variant_hash);
             CREATE INDEX IF NOT EXISTS idx_assets_stack_browse ON assets(stack_position, created_at DESC) WHERE stack_id IS NOT NULL;",
        )?;
        // GPS coordinate columns
        self.add_column_if_missing("assets", "latitude", "REAL").with_context(ctx("latitude column"))?;
        self.add_column_if_missing("assets", "longitude", "REAL").with_context(ctx("longitude column"))?;
        self.step(
            V,
            "geo index",
            "CREATE INDEX IF NOT EXISTS idx_assets_geo ON assets(latitude, longitude) WHERE latitude IS NOT NULL",
        )?;
        // Preview rotation override
        self.add_column_if_missing("assets", "preview_rotation", "INTEGER").with_context(ctx("preview_rotation column"))?;
        self.backfill_gps_columns().with_context(ctx("backfill GPS columns"))?;
        // Face count denormalized column. The `faces` table is created by
        // v13 (historically by a lazy initialiser, so it may or may not be
        // present on a pre-v1 catalog); only backfill when it is there.
        self.add_column_if_missing("assets", "face_count", "INTEGER NOT NULL DEFAULT 0").with_context(ctx("face_count column"))?;
        if self.table_exists("faces")? {
            self.step(
                V,
                "backfill face_count",
                "UPDATE assets SET face_count = (SELECT COUNT(*) FROM faces WHERE asset_id = assets.id) \
                 WHERE face_count = 0 AND EXISTS (SELECT 1 FROM faces WHERE asset_id = assets.id)",
            )?;
        }
        self.step(
            V,
            "face_count index",
            "CREATE INDEX IF NOT EXISTS idx_assets_face_count ON assets(face_count) WHERE face_count > 0",
        )?;
        // Fix MicrosoftPhoto:Rating percentage values (1-100) → xmp:Rating scale (1-5)
        self.step(
            V,
            "normalise percentage ratings",
            "UPDATE assets SET rating = CASE
                WHEN rating BETWEEN 1 AND 12 THEN 1
                WHEN rating BETWEEN 13 AND 37 THEN 2
                WHEN rating BETWEEN 38 AND 62 THEN 3
                WHEN rating BETWEEN 63 AND 87 THEN 4
                ELSE 5
             END WHERE rating > 5",
        )?;
        Ok(())
    }

    // ─── v1 → v2: pending writeback tracking ───
    fn migrate_v2(&self) -> Result<()> {
        self.add_column_if_missing("recipes", "pending_writeback", "INTEGER NOT NULL DEFAULT 0")
            .context("schema migration v2: pending_writeback column")
    }

    // ─── v2 → v3: preview variant override ───
    fn migrate_v3(&self) -> Result<()> {
        self.add_column_if_missing("assets", "preview_variant", "TEXT")
            .context("schema migration v3: preview_variant column")
    }

    // ─── v3 → v4: video duration denormalized column ───
    fn migrate_v4(&self) -> Result<()> {
        // v11 renames this column to `duration_seconds`; on a catalog that
        // already carries the new name there is nothing to add or backfill.
        if self.has_column("assets", "duration_seconds")? {
            return Ok(());
        }
        self.add_column_if_missing("assets", "video_duration", "REAL")
            .context("schema migration v4: video_duration column")?;
        // Backfill from variant source_metadata JSON
        self.step(
            4,
            "backfill video_duration",
            "UPDATE assets SET video_duration = ( \
                SELECT CAST(json_extract(v.source_metadata, '$.video_duration') AS REAL) \
                FROM variants v WHERE v.asset_id = assets.id \
                AND json_extract(v.source_metadata, '$.video_duration') IS NOT NULL \
                LIMIT 1 \
             ) WHERE video_duration IS NULL",
        )
    }

    // ─── v4 → v5: video codec denormalized column ───
    fn migrate_v5(&self) -> Result<()> {
        self.add_column_if_missing("assets", "video_codec", "TEXT")
            .context("schema migration v5: video_codec column")?;
        self.step(
            5,
            "backfill video_codec",
            "UPDATE assets SET video_codec = ( \
                SELECT json_extract(v.source_metadata, '$.video_codec') \
                FROM variants v WHERE v.asset_id = assets.id \
                AND json_extract(v.source_metadata, '$.video_codec') IS NOT NULL \
                LIMIT 1 \
             ) WHERE video_codec IS NULL",
        )
    }

    // ─── v5 → v6: track which face recognition model produced each embedding ───
    // Existing rows are marked with the legacy INT8 model id so new FP32
    // embeddings don't silently mix with incompatible ones during clustering.
    fn migrate_v6(&self) -> Result<()> {
        // `faces` becomes part of the versioned schema in v13; a catalog
        // that never ran the lazy initialiser has no table to alter here,
        // and v13 repeats both the column and the backfill for it.
        if !self.table_exists("faces")? {
            return Ok(());
        }
        self.add_column_if_missing("faces", "recognition_model", "TEXT")
            .context("schema migration v6: faces.recognition_model column")?;
        self.step(
            6,
            "stamp legacy recognition_model",
            &format!("UPDATE faces SET recognition_model = '{LEGACY_RECOGNITION_MODEL}' WHERE recognition_model IS NULL"),
        )
    }

    // ─── v6 → v7: distinguish "never scanned for faces" from "scanned, no face found" ───
    // Without this, `maki faces detect` re-scans every zero-face asset (landscapes,
    // product shots, documents) on every run — wasting compute proportional to catalog
    // size. The new column is set to 'done' once detection completes, regardless of
    // face count, and the scan loop uses it instead of `has_faces()` to decide whether
    // to skip.
    //
    // Backfill: any asset with at least one face row already counts as "done" — if you
    // saw a face there, you obviously ran detection. Assets without faces get NULL,
    // meaning they'll be scanned on the next `detect` run (no regression vs. today).
    fn migrate_v7(&self) -> Result<()> {
        self.add_column_if_missing("assets", "face_scan_status", "TEXT")
            .context("schema migration v7: face_scan_status column")?;
        self.step(
            7,
            "backfill face_scan_status",
            "UPDATE assets SET face_scan_status = 'done' \
             WHERE face_scan_status IS NULL AND face_count > 0",
        )?;
        self.step(
            7,
            "face_scan_status index",
            "CREATE INDEX IF NOT EXISTS idx_assets_face_scan_status ON assets(face_scan_status) WHERE face_scan_status IS NULL",
        )
    }

    // ─── v7 → v8: leaf tag count ───
    // The number of "intentional" tags on each asset (tags that are not the
    // ancestor of any other tag on the same asset). Used by the `tagcount:`
    // search filter; denormalised because the alternative is a JSON-scan
    // subquery per row which gets slow on large catalogues.
    fn migrate_v8(&self) -> Result<()> {
        self.add_column_if_missing("assets", "leaf_tag_count", "INTEGER NOT NULL DEFAULT 0")
            .context("schema migration v8: leaf_tag_count column")?;
        // Backfill from existing tags. Uses a json_each correlated
        // subquery — slow but one-shot per asset on migration.
        self.step(
            8,
            "backfill leaf_tag_count",
            "UPDATE assets SET leaf_tag_count = ( \
               SELECT COUNT(*) FROM json_each(assets.tags) je1 \
               WHERE NOT EXISTS ( \
                 SELECT 1 FROM json_each(assets.tags) je2 \
                 WHERE LOWER(je2.value) LIKE LOWER(je1.value) || '|%' \
               ) \
             ) \
             WHERE tags IS NOT NULL AND tags != '[]'",
        )?;
        self.step(
            8,
            "leaf_tag_count index",
            "CREATE INDEX IF NOT EXISTS idx_assets_leaf_tag_count ON assets(leaf_tag_count)",
        )
    }

    // ─── v8 → v9: FTS5 free-text index ───
    // Full-text index over name / best-variant filename / description /
    // best-variant source metadata — the same four columns the free-text
    // LIKE filter scanned. The TRIGRAM tokenizer gives index-accelerated
    // SUBSTRING matching, so user-visible semantics stay identical to LIKE
    // `%text%` (case-insensitive, mid-word matches like "91_47" in
    // "Z91_4714"); queries shorter than 3 chars fall back to the LIKE path
    // in `add_text_filters`.
    //
    // Maintenance is by TRIGGER, not by write-path discipline — a new write
    // path can't forget the index. The assets-table triggers re-derive the
    // row on every insert/update (the best-variant columns refresh
    // automatically because `best_variant_hash` lives on the assets row);
    // the variants triggers cover the import ordering where the variant row
    // lands after the asset upsert.
    fn migrate_v9(&self) -> Result<()> {
        self.step(
            9,
            "create assets_fts and triggers",
            "CREATE VIRTUAL TABLE IF NOT EXISTS assets_fts USING fts5( \
               asset_id UNINDEXED, name, filename, description, metadata, \
               tokenize='trigram' \
             );
             CREATE TRIGGER IF NOT EXISTS assets_fts_asset_insert AFTER INSERT ON assets BEGIN \
               DELETE FROM assets_fts WHERE asset_id = new.id; \
               INSERT INTO assets_fts(asset_id, name, filename, description, metadata) \
               VALUES (new.id, IFNULL(new.name,''), \
                       IFNULL((SELECT original_filename FROM variants WHERE content_hash = new.best_variant_hash), ''), \
                       IFNULL(new.description,''), \
                       IFNULL((SELECT source_metadata FROM variants WHERE content_hash = new.best_variant_hash), '')); \
             END;
             CREATE TRIGGER IF NOT EXISTS assets_fts_asset_update AFTER UPDATE ON assets BEGIN \
               DELETE FROM assets_fts WHERE asset_id = old.id; \
               INSERT INTO assets_fts(asset_id, name, filename, description, metadata) \
               VALUES (new.id, IFNULL(new.name,''), \
                       IFNULL((SELECT original_filename FROM variants WHERE content_hash = new.best_variant_hash), ''), \
                       IFNULL(new.description,''), \
                       IFNULL((SELECT source_metadata FROM variants WHERE content_hash = new.best_variant_hash), '')); \
             END;
             CREATE TRIGGER IF NOT EXISTS assets_fts_asset_delete AFTER DELETE ON assets BEGIN \
               DELETE FROM assets_fts WHERE asset_id = old.id; \
             END;
             CREATE TRIGGER IF NOT EXISTS assets_fts_variant_insert AFTER INSERT ON variants BEGIN \
               DELETE FROM assets_fts WHERE asset_id = new.asset_id; \
               INSERT INTO assets_fts(asset_id, name, filename, description, metadata) \
               SELECT a.id, IFNULL(a.name,''), \
                      IFNULL((SELECT original_filename FROM variants WHERE content_hash = a.best_variant_hash), ''), \
                      IFNULL(a.description,''), \
                      IFNULL((SELECT source_metadata FROM variants WHERE content_hash = a.best_variant_hash), '') \
               FROM assets a WHERE a.id = new.asset_id; \
             END;
             CREATE TRIGGER IF NOT EXISTS assets_fts_variant_update AFTER UPDATE ON variants BEGIN \
               DELETE FROM assets_fts WHERE asset_id = new.asset_id; \
               INSERT INTO assets_fts(asset_id, name, filename, description, metadata) \
               SELECT a.id, IFNULL(a.name,''), \
                      IFNULL((SELECT original_filename FROM variants WHERE content_hash = a.best_variant_hash), ''), \
                      IFNULL(a.description,''), \
                      IFNULL((SELECT source_metadata FROM variants WHERE content_hash = a.best_variant_hash), '') \
               FROM assets a WHERE a.id = new.asset_id; \
             END;
             CREATE TRIGGER IF NOT EXISTS assets_fts_variant_delete AFTER DELETE ON variants BEGIN \
               DELETE FROM assets_fts WHERE asset_id = old.asset_id; \
               INSERT INTO assets_fts(asset_id, name, filename, description, metadata) \
               SELECT a.id, IFNULL(a.name,''), \
                      IFNULL((SELECT original_filename FROM variants WHERE content_hash = a.best_variant_hash), ''), \
                      IFNULL(a.description,''), \
                      IFNULL((SELECT source_metadata FROM variants WHERE content_hash = a.best_variant_hash), '') \
               FROM assets a WHERE a.id = old.asset_id; \
             END;",
        )?;
        // Backfill (idempotent: clear first, then rebuild from base tables).
        self.step(
            9,
            "backfill assets_fts",
            "DELETE FROM assets_fts;
             INSERT INTO assets_fts(asset_id, name, filename, description, metadata) \
             SELECT a.id, IFNULL(a.name,''), IFNULL(v.original_filename,''), \
                    IFNULL(a.description,''), IFNULL(v.source_metadata,'') \
             FROM assets a LEFT JOIN variants v ON v.content_hash = a.best_variant_hash;",
        )
    }

    // ─── v9 → v10: tag provenance ───
    // JSON object mapping tag value → source ("user" | "xmp-import" |
    // "auto-tag" | "vlm"). Mirrors `Asset::tag_sources`; a tag absent from
    // the object has source "user". Derived from the sidecar on every
    // insert_asset, same as `tags`.
    fn migrate_v10(&self) -> Result<()> {
        self.add_column_if_missing("assets", "tag_sources", "TEXT NOT NULL DEFAULT '{}'")
            .context("schema migration v10: tag_sources column")
    }

    // ─── v10 → v11: audio first-class phase 1 — typed audio columns +
    // duration generalization ───
    fn migrate_v11(&self) -> Result<()> {
        // Duration is a shared media fact, not a video-only one; audio
        // joins the column video pioneered. RENAME keeps existing data
        // and makes stragglers referencing the old name fail loudly.
        // (Fresh initialize() runs the v4 block first, so the source
        // column always exists here unless the rename already happened.)
        if !self.has_column("assets", "duration_seconds")? {
            self.step(
                11,
                "rename audio columns",
                "ALTER TABLE assets RENAME COLUMN video_duration TO duration_seconds",
            )?;
        }
        // Typed audio columns, derived from variant source_metadata
        // (`$.audio_*` keys) exactly like video_codec. sample_rate in
        // Hz, bitrate in kbps. audio_key / audio_bpm are filled by
        // `maki audio analyze` (opt-in external tools), the technical
        // trio by lofty at import/refresh time.
        for (col, decl) in [
            ("audio_sample_rate", "INTEGER"),
            ("audio_channels", "INTEGER"),
            ("audio_bitrate", "INTEGER"),
            ("audio_key", "TEXT"),
            ("audio_bpm", "REAL"),
        ] {
            self.add_column_if_missing("assets", col, decl)
                .context("schema migration v11: audio columns")?;
        }
        // Backfill from variant metadata where present — one UPDATE,
        // restricted to audio assets that actually carry `audio_*`
        // keys. The restriction is load-bearing, not an optimization:
        // every row an UPDATE processes fires the FTS5 trigram
        // triggers (schema v9) even when nothing changes, and each
        // firing re-tokenizes the row's full source_metadata blob.
        // An unguarded `WHERE col IS NULL` matches the whole table
        // and turned this migration into hours of no-op FTS rewrites
        // on large catalogs (6 passes × N assets). On a pre-v11
        // catalog no variant has `audio_*` keys, so this now touches
        // zero rows; it only does work when re-run over a partially
        // migrated or post-v11 catalog. The instr() prescan avoids
        // parsing JSON for assets that can't match.
        self.step(
            11,
            "backfill audio columns",
            "UPDATE assets SET \
                duration_seconds = COALESCE(duration_seconds, ( \
                    SELECT CAST(json_extract(v.source_metadata, '$.audio_duration') AS REAL) \
                    FROM variants v WHERE v.asset_id = assets.id \
                    AND json_extract(v.source_metadata, '$.audio_duration') IS NOT NULL LIMIT 1)), \
                audio_sample_rate = COALESCE(audio_sample_rate, ( \
                    SELECT CAST(json_extract(v.source_metadata, '$.audio_sample_rate') AS INTEGER) \
                    FROM variants v WHERE v.asset_id = assets.id \
                    AND json_extract(v.source_metadata, '$.audio_sample_rate') IS NOT NULL LIMIT 1)), \
                audio_channels = COALESCE(audio_channels, ( \
                    SELECT CAST(json_extract(v.source_metadata, '$.audio_channels') AS INTEGER) \
                    FROM variants v WHERE v.asset_id = assets.id \
                    AND json_extract(v.source_metadata, '$.audio_channels') IS NOT NULL LIMIT 1)), \
                audio_bitrate = COALESCE(audio_bitrate, ( \
                    SELECT CAST(json_extract(v.source_metadata, '$.audio_bitrate') AS INTEGER) \
                    FROM variants v WHERE v.asset_id = assets.id \
                    AND json_extract(v.source_metadata, '$.audio_bitrate') IS NOT NULL LIMIT 1)), \
                audio_key = COALESCE(audio_key, ( \
                    SELECT json_extract(v.source_metadata, '$.audio_key') \
                    FROM variants v WHERE v.asset_id = assets.id \
                    AND json_extract(v.source_metadata, '$.audio_key') IS NOT NULL LIMIT 1)), \
                audio_bpm = COALESCE(audio_bpm, ( \
                    SELECT CAST(json_extract(v.source_metadata, '$.audio_bpm') AS REAL) \
                    FROM variants v WHERE v.asset_id = assets.id \
                    AND json_extract(v.source_metadata, '$.audio_bpm') IS NOT NULL LIMIT 1)) \
             WHERE asset_type = 'audio' \
             AND EXISTS ( \
                SELECT 1 FROM variants v WHERE v.asset_id = assets.id \
                AND instr(v.source_metadata, '\"audio_') > 0 \
             )",
        )
    }

    // ─── v11 → v12: trigram index over the tags column ───
    // Tag filters are hierarchy-aware LIKE/GLOB patterns over the JSON
    // `assets.tags` text (`%"a|b"%`, `%|b|%`, …). Against the base table
    // those are a full scan plus a row probe per asset, which on a cold
    // cache of a multi-GB catalog took seconds per page. FTS5 with the
    // trigram tokenizer evaluates ordinary LIKE and GLOB through its
    // index, so the search builder runs the unchanged patterns against
    // this copy of the column and feeds the matching ids back with
    // `a.id IN (…)`. Semantics are identical to the old scan; only the
    // access path changes. Patterns shorter than three literal characters
    // fall back to a scan of this table, still no worse than before.
    //
    // Kept separate from `assets_fts` (v9) so this backfill only tokenizes
    // ~200 bytes per asset instead of rebuilding the source-metadata
    // index. Three triggers keep it current; like the v9 triggers they
    // fire per row on any UPDATE of `assets`, so backfills must stay
    // guarded (see the v11 comment).
    fn migrate_v12(&self) -> Result<()> {
        self.step(
            12,
            "create assets_tags_fts and triggers",
            "CREATE VIRTUAL TABLE IF NOT EXISTS assets_tags_fts USING fts5( \
               asset_id UNINDEXED, tags, tokenize='trigram' \
             );
             CREATE TRIGGER IF NOT EXISTS assets_tags_fts_insert AFTER INSERT ON assets BEGIN \
               DELETE FROM assets_tags_fts WHERE asset_id = new.id; \
               INSERT INTO assets_tags_fts(asset_id, tags) VALUES (new.id, IFNULL(new.tags,'')); \
             END;
             CREATE TRIGGER IF NOT EXISTS assets_tags_fts_update AFTER UPDATE OF id, tags ON assets BEGIN \
               DELETE FROM assets_tags_fts WHERE asset_id = old.id; \
               INSERT INTO assets_tags_fts(asset_id, tags) VALUES (new.id, IFNULL(new.tags,'')); \
             END;
             CREATE TRIGGER IF NOT EXISTS assets_tags_fts_delete AFTER DELETE ON assets BEGIN \
               DELETE FROM assets_tags_fts WHERE asset_id = old.id; \
             END;",
        )?;
        // Backfill (idempotent: clear first, then rebuild from the base table).
        self.step(
            12,
            "backfill assets_tags_fts",
            "DELETE FROM assets_tags_fts;
             INSERT INTO assets_tags_fts(asset_id, tags) \
             SELECT id, IFNULL(tags,'') FROM assets;",
        )
    }

    // ─── v12 → v13: store tables join the versioned schema ───
    // Until v13 `people`, `faces` and `embeddings` were created lazily by
    // `FaceStore::initialize` / `EmbeddingStore::initialize` at ~35 use
    // sites, only in AI builds, and the embeddings primary-key change was
    // an unversioned sniff of `pragma_table_info`; `stacks`, `collections`
    // and `collection_assets` came from `StackStore::initialize` /
    // `CollectionStore::initialize` inside the v1 step. This step owns all
    // of that DDL, feature-independently, and is written to accept every
    // starting state: no tables at all, tables from the lazy initialisers
    // (possibly missing `recognition_model`), or an `embeddings` table
    // with the original single-column primary key.
    fn migrate_v13(&self) -> Result<()> {
        const V: u32 = 13;
        self.step(
            V,
            "create stack and collection tables",
            "CREATE TABLE IF NOT EXISTS stacks (
                id TEXT PRIMARY KEY,
                created_at TEXT NOT NULL,
                member_count INTEGER NOT NULL DEFAULT 0
            );
            CREATE TABLE IF NOT EXISTS collections (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL UNIQUE,
                description TEXT,
                created_at TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS collection_assets (
                collection_id TEXT NOT NULL REFERENCES collections(id),
                asset_id TEXT NOT NULL REFERENCES assets(id),
                added_at TEXT NOT NULL,
                PRIMARY KEY (collection_id, asset_id)
            );
            CREATE INDEX IF NOT EXISTS idx_collection_assets_asset
                ON collection_assets(asset_id);",
        )?;
        self.step(
            V,
            "create people and faces tables",
            "CREATE TABLE IF NOT EXISTS people (
                id TEXT PRIMARY KEY,
                name TEXT,
                representative_face_id TEXT,
                created_at TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS faces (
                id TEXT PRIMARY KEY,
                asset_id TEXT NOT NULL,
                person_id TEXT,
                bbox_x REAL NOT NULL,
                bbox_y REAL NOT NULL,
                bbox_w REAL NOT NULL,
                bbox_h REAL NOT NULL,
                embedding BLOB NOT NULL,
                confidence REAL NOT NULL,
                created_at TEXT NOT NULL,
                recognition_model TEXT,
                FOREIGN KEY (asset_id) REFERENCES assets(id),
                FOREIGN KEY (person_id) REFERENCES people(id)
            );
            CREATE INDEX IF NOT EXISTS idx_faces_asset ON faces(asset_id);
            CREATE INDEX IF NOT EXISTS idx_faces_person ON faces(person_id);",
        )?;
        // A `faces` table created lazily before v6 existed lacks the column
        // (v6 skipped catalogs without the table); same column, same
        // legacy stamp as v6.
        self.add_column_if_missing("faces", "recognition_model", "TEXT")
            .context("schema migration v13: faces.recognition_model column")?;
        self.step(
            V,
            "stamp legacy recognition_model",
            &format!("UPDATE faces SET recognition_model = '{LEGACY_RECOGNITION_MODEL}' WHERE recognition_model IS NULL"),
        )?;
        // The v1 face_count backfill is skipped on catalogs whose `faces`
        // table did not exist yet; catch those rows up. Guarded to the
        // assets that actually have faces and a zero count — every row an
        // UPDATE processes fires the v9 FTS trigger.
        self.step(
            V,
            "backfill face_count",
            "UPDATE assets SET face_count = (SELECT COUNT(*) FROM faces WHERE asset_id = assets.id) \
             WHERE face_count = 0 AND EXISTS (SELECT 1 FROM faces WHERE asset_id = assets.id)",
        )?;

        // Embeddings: composite primary key (asset_id, model) so several
        // models can coexist. An existing table with a single-column
        // primary key (the original layout) is rebuilt and its rows kept.
        if self.table_exists("embeddings")? && self.embeddings_has_single_column_pk()? {
            let model_expr = if self.has_column("embeddings", "model")? {
                format!("COALESCE(model, '{LEGACY_EMBEDDING_MODEL}')")
            } else {
                format!("'{LEGACY_EMBEDDING_MODEL}'")
            };
            self.step(
                V,
                "migrate embeddings to composite primary key",
                &format!(
                    "ALTER TABLE embeddings RENAME TO embeddings_old;
                     CREATE TABLE embeddings (
                         asset_id TEXT NOT NULL,
                         model TEXT NOT NULL DEFAULT '{LEGACY_EMBEDDING_MODEL}',
                         embedding BLOB NOT NULL,
                         PRIMARY KEY (asset_id, model)
                     );
                     INSERT INTO embeddings (asset_id, model, embedding)
                         SELECT asset_id, {model_expr}, embedding FROM embeddings_old;
                     DROP TABLE embeddings_old;"
                ),
            )?;
        } else {
            self.step(
                V,
                "create embeddings table",
                &format!(
                    "CREATE TABLE IF NOT EXISTS embeddings (
                        asset_id TEXT NOT NULL,
                        model TEXT NOT NULL DEFAULT '{LEGACY_EMBEDDING_MODEL}',
                        embedding BLOB NOT NULL,
                        PRIMARY KEY (asset_id, model)
                    )"
                ),
            )?;
        }
        Ok(())
    }

    /// `true` when the `embeddings` table has exactly one primary-key
    /// column (the pre-composite layout). `pragma_table_info.pk` is the
    /// 1-based position within the primary key, 0 for non-key columns.
    fn embeddings_has_single_column_pk(&self) -> Result<bool> {
        let n: i64 = self
            .conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('embeddings') WHERE pk > 0",
                [],
                |r| r.get(0),
            )
            .context("inspecting embeddings primary key")?;
        Ok(n == 1)
    }

    // ═══ MIGRATION BACKFILLS ═══

    /// Backfill GPS latitude/longitude on assets from variant source_metadata.
    /// Called from migrations, idempotent via `WHERE a.latitude IS NULL`.
    pub(super) fn backfill_gps_columns(&self) -> Result<()> {
        // Try gps_latitude_decimal first, fall back to parsing DMS strings
        self.conn
            .execute_batch(
                "UPDATE assets SET
                latitude = (
                    SELECT COALESCE(
                        CAST(json_extract(v.source_metadata, '$.gps_latitude_decimal') AS REAL),
                        NULL
                    )
                    FROM variants v WHERE v.asset_id = assets.id
                    AND json_extract(v.source_metadata, '$.gps_latitude_decimal') IS NOT NULL
                    ORDER BY CASE v.role WHEN 'original' THEN 0 ELSE 1 END LIMIT 1
                ),
                longitude = (
                    SELECT COALESCE(
                        CAST(json_extract(v.source_metadata, '$.gps_longitude_decimal') AS REAL),
                        NULL
                    )
                    FROM variants v WHERE v.asset_id = assets.id
                    AND json_extract(v.source_metadata, '$.gps_longitude_decimal') IS NOT NULL
                    ORDER BY CASE v.role WHEN 'original' THEN 0 ELSE 1 END LIMIT 1
                )
            WHERE assets.latitude IS NULL
            AND EXISTS (
                SELECT 1 FROM variants v2
                WHERE v2.asset_id = assets.id
                AND json_extract(v2.source_metadata, '$.gps_latitude_decimal') IS NOT NULL
            )",
            )
            .context("decimal GPS backfill")?;

        // Fallback: parse DMS strings for rows still NULL
        // This needs Rust-side parsing, so we query and update individually
        let rows: Vec<(String, String, String)> = {
            let mut stmt = self
                .conn
                .prepare(
                    "SELECT a.id, json_extract(v.source_metadata, '$.gps_latitude'),
                            json_extract(v.source_metadata, '$.gps_longitude')
                     FROM assets a
                     JOIN variants v ON v.asset_id = a.id
                     WHERE a.latitude IS NULL
                     AND json_extract(v.source_metadata, '$.gps_latitude') IS NOT NULL
                     AND json_extract(v.source_metadata, '$.gps_longitude') IS NOT NULL
                     GROUP BY a.id",
                )
                .context("DMS GPS backfill query")?;
            let rows = stmt.query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?))
            })?;
            rows.collect::<std::result::Result<_, _>>().context("DMS GPS backfill query")?
        };

        for (id, lat_str, lon_str) in &rows {
            if let (Some(lat), Some(lon)) = (
                crate::exif_reader::parse_dms_string(lat_str),
                crate::exif_reader::parse_dms_string(lon_str),
            ) {
                self.conn
                    .execute(
                        "UPDATE assets SET latitude = ?1, longitude = ?2 WHERE id = ?3",
                        rusqlite::params![lat, lon, id],
                    )
                    .with_context(|| format!("DMS GPS backfill for asset {id}"))?;
            }
        }
        Ok(())
    }
}
