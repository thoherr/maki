//! `schema` section of `Catalog` — methods extracted from the original
//! 9.2-kLOC catalog.rs into a multi-file `impl Catalog` block.
//!
//! Types, helper functions, and the connection ctor live in the parent
//! `catalog` module.

use super::*;

impl Catalog {
    // ═══ SCHEMA MIGRATIONS ═══

    /// Run schema migrations, skipping work that is already done.
    ///
    /// Checks the current schema version and only executes migration blocks
    /// for versions newer than what the database already has.  Called once at
    /// startup (server init, `maki migrate`) and from `initialize()` for fresh
    /// catalogs (where version is 0, so everything runs).
    pub fn run_migrations(&self) {
        let current = self.schema_version();
        if current >= SCHEMA_VERSION {
            return;
        }

        // ── v0 → v1: base columns, indexes, denormalization, backfills ──
        if current < 1 {
            let _ = self.conn.execute_batch("ALTER TABLE assets ADD COLUMN rating INTEGER");
            let _ = self.conn.execute_batch("ALTER TABLE assets ADD COLUMN color_label TEXT");
            let _ = self.conn.execute_batch("ALTER TABLE variants ADD COLUMN camera_model TEXT");
            let _ = self.conn.execute_batch("ALTER TABLE variants ADD COLUMN lens_model TEXT");
            let _ = self.conn.execute_batch("ALTER TABLE variants ADD COLUMN focal_length_mm REAL");
            let _ = self.conn.execute_batch("ALTER TABLE variants ADD COLUMN f_number REAL");
            let _ = self.conn.execute_batch("ALTER TABLE variants ADD COLUMN iso INTEGER");
            let _ = self.conn.execute_batch("ALTER TABLE variants ADD COLUMN image_width INTEGER");
            let _ = self.conn.execute_batch("ALTER TABLE variants ADD COLUMN image_height INTEGER");
            let _ = self.conn.execute_batch(
                "CREATE INDEX IF NOT EXISTS idx_variants_camera ON variants(camera_model);
                 CREATE INDEX IF NOT EXISTS idx_variants_lens ON variants(lens_model);
                 CREATE INDEX IF NOT EXISTS idx_variants_iso ON variants(iso);
                 CREATE INDEX IF NOT EXISTS idx_variants_focal ON variants(focal_length_mm);",
            );
            // Backfill metadata columns from existing JSON
            let _ = self.conn.execute_batch(
                "UPDATE variants SET
                    camera_model = json_extract(source_metadata, '$.camera_model'),
                    lens_model = json_extract(source_metadata, '$.lens_model'),
                    iso = CAST(json_extract(source_metadata, '$.iso') AS INTEGER),
                    focal_length_mm = CAST(REPLACE(json_extract(source_metadata, '$.focal_length'), ' mm', '') AS REAL),
                    f_number = CAST(json_extract(source_metadata, '$.f_number') AS REAL),
                    image_width = CAST(json_extract(source_metadata, '$.image_width') AS INTEGER),
                    image_height = CAST(json_extract(source_metadata, '$.image_height') AS INTEGER)
                WHERE camera_model IS NULL AND source_metadata != '{}'"
            );
            // best_variant_hash denormalization
            let _ = self.conn.execute_batch("ALTER TABLE assets ADD COLUMN best_variant_hash TEXT");
            let _ = self.conn.execute_batch(
                "CREATE INDEX IF NOT EXISTS idx_variants_asset_id ON variants(asset_id)",
            );
            let _ = self.conn.execute_batch(
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
            );
            // primary_variant_format + variant_count denormalization
            let _ = self.conn.execute_batch("ALTER TABLE assets ADD COLUMN primary_variant_format TEXT");
            let _ = self.conn.execute_batch("ALTER TABLE assets ADD COLUMN variant_count INTEGER NOT NULL DEFAULT 0");
            let _ = self.conn.execute_batch(
                "UPDATE assets SET primary_variant_format = COALESCE(
                    (SELECT format FROM variants WHERE asset_id = assets.id AND role = 'original'
                     AND LOWER(format) IN ('raw','cr2','cr3','nef','arw','orf','rw2','dng','raf','pef','srw')
                     LIMIT 1),
                    (SELECT format FROM variants WHERE asset_id = assets.id AND role = 'original' LIMIT 1),
                    (SELECT format FROM variants WHERE content_hash = assets.best_variant_hash)
                ) WHERE primary_variant_format IS NULL",
            );
            let _ = self.conn.execute_batch(
                "UPDATE assets SET variant_count = (
                    SELECT COUNT(*) FROM variants WHERE asset_id = assets.id
                ) WHERE variant_count = 0",
            );
            // Collection and stack tables
            let _ = crate::collection::CollectionStore::initialize(&self.conn);
            let _ = crate::stack::StackStore::initialize(&self.conn);
            let _ = self.conn.execute_batch("ALTER TABLE assets ADD COLUMN stack_id TEXT");
            let _ = self.conn.execute_batch("ALTER TABLE assets ADD COLUMN stack_position INTEGER");
            let _ = self.conn.execute_batch(
                "CREATE INDEX IF NOT EXISTS idx_assets_stack_id ON assets(stack_id);",
            );
            // Volume purpose
            let _ = self.conn.execute_batch("ALTER TABLE volumes ADD COLUMN purpose TEXT");
            // Performance indexes
            let _ = self.conn.execute_batch(
                "CREATE INDEX IF NOT EXISTS idx_fl_content_hash ON file_locations(content_hash);
                 CREATE INDEX IF NOT EXISTS idx_fl_volume_id ON file_locations(volume_id);
                 CREATE INDEX IF NOT EXISTS idx_assets_created_at ON assets(created_at);
                 CREATE INDEX IF NOT EXISTS idx_assets_best_variant_hash ON assets(best_variant_hash);
                 CREATE INDEX IF NOT EXISTS idx_variants_format ON variants(format);
                 CREATE INDEX IF NOT EXISTS idx_recipes_variant_hash ON recipes(variant_hash);
                 CREATE INDEX IF NOT EXISTS idx_assets_stack_browse ON assets(stack_position, created_at DESC) WHERE stack_id IS NOT NULL;",
            );
            // GPS coordinate columns
            let _ = self.conn.execute_batch("ALTER TABLE assets ADD COLUMN latitude REAL");
            let _ = self.conn.execute_batch("ALTER TABLE assets ADD COLUMN longitude REAL");
            let _ = self.conn.execute_batch(
                "CREATE INDEX IF NOT EXISTS idx_assets_geo ON assets(latitude, longitude) WHERE latitude IS NOT NULL",
            );
            // Preview rotation override
            let _ = self.conn.execute_batch("ALTER TABLE assets ADD COLUMN preview_rotation INTEGER");
            self.backfill_gps_columns();
            // Face count denormalized column
            let _ = self.conn.execute_batch("ALTER TABLE assets ADD COLUMN face_count INTEGER NOT NULL DEFAULT 0");
            #[cfg(feature = "ai")]
            {
                let _ = self.conn.execute_batch(
                    "UPDATE assets SET face_count = (SELECT COUNT(*) FROM faces WHERE asset_id = assets.id) WHERE face_count = 0 AND EXISTS (SELECT 1 FROM sqlite_master WHERE type='table' AND name='faces')",
                );
            }
            let _ = self.conn.execute_batch(
                "CREATE INDEX IF NOT EXISTS idx_assets_face_count ON assets(face_count) WHERE face_count > 0",
            );
            // Embeddings table
            let _ = self.conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS embeddings (
                    asset_id TEXT NOT NULL,
                    model TEXT NOT NULL DEFAULT 'siglip-vit-b16-256',
                    embedding BLOB NOT NULL,
                    PRIMARY KEY (asset_id, model)
                )",
            );
            #[cfg(feature = "ai")]
            {
                let _ = crate::embedding_store::EmbeddingStore::initialize(&self.conn);
                let _ = crate::face_store::FaceStore::initialize(&self.conn);
            }
            // Fix MicrosoftPhoto:Rating percentage values (1-100) → xmp:Rating scale (1-5)
            let _ = self.conn.execute_batch(
                "UPDATE assets SET rating = CASE
                    WHEN rating BETWEEN 1 AND 12 THEN 1
                    WHEN rating BETWEEN 13 AND 37 THEN 2
                    WHEN rating BETWEEN 38 AND 62 THEN 3
                    WHEN rating BETWEEN 63 AND 87 THEN 4
                    ELSE 5
                 END WHERE rating > 5",
            );
        }

        // ── v1 → v2: pending writeback tracking ──
        if current < 2 {
            let _ = self.conn.execute_batch(
                "ALTER TABLE recipes ADD COLUMN pending_writeback INTEGER NOT NULL DEFAULT 0",
            );
        }

        // ── v2 → v3: preview variant override ──
        if current < 3 {
            let _ = self.conn.execute_batch("ALTER TABLE assets ADD COLUMN preview_variant TEXT");
        }

        // ── v3 → v4: video duration denormalized column ──
        if current < 4 {
            let _ = self.conn.execute_batch("ALTER TABLE assets ADD COLUMN video_duration REAL");
            // Backfill from variant source_metadata JSON
            let _ = self.conn.execute_batch(
                "UPDATE assets SET video_duration = ( \
                    SELECT CAST(json_extract(v.source_metadata, '$.video_duration') AS REAL) \
                    FROM variants v WHERE v.asset_id = assets.id \
                    AND json_extract(v.source_metadata, '$.video_duration') IS NOT NULL \
                    LIMIT 1 \
                 ) WHERE video_duration IS NULL",
            );
        }

        // ── v4 → v5: video codec denormalized column ──
        if current < 5 {
            let _ = self.conn.execute_batch("ALTER TABLE assets ADD COLUMN video_codec TEXT");
            let _ = self.conn.execute_batch(
                "UPDATE assets SET video_codec = ( \
                    SELECT json_extract(v.source_metadata, '$.video_codec') \
                    FROM variants v WHERE v.asset_id = assets.id \
                    AND json_extract(v.source_metadata, '$.video_codec') IS NOT NULL \
                    LIMIT 1 \
                 ) WHERE video_codec IS NULL",
            );
        }

        // ── v5 → v6: track which face recognition model produced each embedding ──
        // Existing rows are marked with the legacy INT8 model id so new FP32
        // embeddings don't silently mix with incompatible ones during clustering.
        if current < 6 {
            let _ = self.conn.execute_batch("ALTER TABLE faces ADD COLUMN recognition_model TEXT");
            let _ = self.conn.execute_batch(
                "UPDATE faces SET recognition_model = 'arcface-resnet100-int8' WHERE recognition_model IS NULL",
            );
        }

        // ── v6 → v7: distinguish "never scanned for faces" from "scanned, no face found" ──
        // Without this, `maki faces detect` re-scans every zero-face asset (landscapes,
        // product shots, documents) on every run — wasting compute proportional to catalog
        // size. The new column is set to 'done' once detection completes, regardless of
        // face count, and the scan loop uses it instead of `has_faces()` to decide whether
        // to skip.
        //
        // Backfill: any asset with at least one face row already counts as "done" — if you
        // saw a face there, you obviously ran detection. Assets without faces get NULL,
        // meaning they'll be scanned on the next `detect` run (no regression vs. today).
        if current < 7 {
            let _ = self.conn.execute_batch("ALTER TABLE assets ADD COLUMN face_scan_status TEXT");
            let _ = self.conn.execute_batch(
                "UPDATE assets SET face_scan_status = 'done' \
                 WHERE face_scan_status IS NULL AND face_count > 0",
            );
            let _ = self.conn.execute_batch(
                "CREATE INDEX IF NOT EXISTS idx_assets_face_scan_status ON assets(face_scan_status) WHERE face_scan_status IS NULL",
            );
        }
        if current < 8 {
            // Leaf tag count — the number of "intentional" tags on each asset
            // (tags that are not the ancestor of any other tag on the same
            // asset). Used by the `tagcount:` search filter; denormalised
            // because the alternative is a JSON-scan subquery per row which
            // gets slow on large catalogues.
            let _ = self.conn.execute_batch(
                "ALTER TABLE assets ADD COLUMN leaf_tag_count INTEGER NOT NULL DEFAULT 0",
            );
            // Backfill from existing tags. Uses a json_each correlated
            // subquery — slow but one-shot per asset on migration.
            let _ = self.conn.execute_batch(
                "UPDATE assets SET leaf_tag_count = ( \
                   SELECT COUNT(*) FROM json_each(assets.tags) je1 \
                   WHERE NOT EXISTS ( \
                     SELECT 1 FROM json_each(assets.tags) je2 \
                     WHERE LOWER(je2.value) LIKE LOWER(je1.value) || '|%' \
                   ) \
                 ) \
                 WHERE tags IS NOT NULL AND tags != '[]'",
            );
            let _ = self.conn.execute_batch(
                "CREATE INDEX IF NOT EXISTS idx_assets_leaf_tag_count ON assets(leaf_tag_count)",
            );
        }

        // ── v8 → v9: FTS5 free-text index ──
        if current < 9 {
            // Full-text index over name / best-variant filename /
            // description / best-variant source metadata — the same four
            // columns the free-text LIKE filter scanned. The TRIGRAM
            // tokenizer gives index-accelerated SUBSTRING matching, so
            // user-visible semantics stay identical to LIKE `%text%`
            // (case-insensitive, mid-word matches like "91_47" in
            // "Z91_4714"); queries shorter than 3 chars fall back to the
            // LIKE path in `add_text_filters`.
            //
            // Maintenance is by TRIGGER, not by write-path discipline —
            // a new write path can't forget the index. The assets-table
            // triggers re-derive the row on every insert/update (the
            // best-variant columns refresh automatically because
            // `best_variant_hash` lives on the assets row); the variants
            // triggers cover the import ordering where the variant row
            // lands after the asset upsert.
            let _ = self.conn.execute_batch(
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
            );
            // Backfill (idempotent: clear first, then rebuild from base tables).
            let _ = self.conn.execute_batch(
                "DELETE FROM assets_fts;
                 INSERT INTO assets_fts(asset_id, name, filename, description, metadata) \
                 SELECT a.id, IFNULL(a.name,''), IFNULL(v.original_filename,''), \
                        IFNULL(a.description,''), IFNULL(v.source_metadata,'') \
                 FROM assets a LEFT JOIN variants v ON v.content_hash = a.best_variant_hash;",
            );
        }

        // ── v9 → v10: tag provenance ──
        if current < 10 {
            // JSON object mapping tag value → source ("user" | "xmp-import"
            // | "auto-tag" | "vlm"). Mirrors `Asset::tag_sources`; a tag
            // absent from the object has source "user". Derived from the
            // sidecar on every insert_asset, same as `tags`.
            let _ = self.conn.execute_batch(
                "ALTER TABLE assets ADD COLUMN tag_sources TEXT NOT NULL DEFAULT '{}'",
            );
        }

        // ── v10 → v11: audio first-class phase 1 — typed audio columns +
        // duration generalization ──
        if current < 11 {
            // Duration is a shared media fact, not a video-only one; audio
            // joins the column video pioneered. RENAME keeps existing data
            // and makes stragglers referencing the old name fail loudly.
            // (Fresh initialize() runs the v4 block first, so the source
            // column always exists here.)
            let _ = self.conn.execute_batch(
                "ALTER TABLE assets RENAME COLUMN video_duration TO duration_seconds",
            );
            // Typed audio columns, derived from variant source_metadata
            // (`$.audio_*` keys) exactly like video_codec. sample_rate in
            // Hz, bitrate in kbps. audio_key / audio_bpm are filled by
            // `maki audio analyze` (opt-in external tools), the technical
            // trio by lofty at import/refresh time.
            let _ = self.conn.execute_batch("ALTER TABLE assets ADD COLUMN audio_sample_rate INTEGER");
            let _ = self.conn.execute_batch("ALTER TABLE assets ADD COLUMN audio_channels INTEGER");
            let _ = self.conn.execute_batch("ALTER TABLE assets ADD COLUMN audio_bitrate INTEGER");
            let _ = self.conn.execute_batch("ALTER TABLE assets ADD COLUMN audio_key TEXT");
            let _ = self.conn.execute_batch("ALTER TABLE assets ADD COLUMN audio_bpm REAL");
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
            let _ = self.conn.execute_batch(
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
            );
        }

        // Stamp the new schema version
        let _ = self.conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS schema_version (version INTEGER NOT NULL);
             DELETE FROM schema_version;",
        );
        let _ = self.conn.execute(
            "INSERT INTO schema_version (version) VALUES (?1)",
            rusqlite::params![SCHEMA_VERSION],
        );
    }

    /// Initialize the database schema.
    ///
    /// Creates base tables, then delegates to `run_migrations()` for all
    /// ADD COLUMN, CREATE INDEX, backfill, and schema version stamping.
    pub fn initialize(&self) -> Result<()> {
        self.conn.execute_batch(
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
        )?;

        // All columns, indexes, backfills, and version stamping handled by migrations
        self.run_migrations();

        Ok(())
    }

}
