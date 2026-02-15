use std::path::Path;

use anyhow::Result;
use rusqlite::Connection;

/// SQLite-backed local catalog for fast queries. This is a derived cache,
/// not the source of truth (sidecar files are).
pub struct Catalog {
    conn: Connection,
}

impl Catalog {
    pub fn open(catalog_root: &Path) -> Result<Self> {
        let db_path = catalog_root.join("catalog.db");
        let conn = Connection::open(&db_path)?;
        Ok(Self { conn })
    }

    /// Initialize the database schema.
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
                original_filename TEXT NOT NULL
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
                relative_path TEXT
            );",
        )?;
        Ok(())
    }

    /// Rebuild the entire catalog from sidecar files.
    pub fn rebuild(&self) -> Result<()> {
        anyhow::bail!("not yet implemented")
    }
}
