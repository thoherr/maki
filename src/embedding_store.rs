//! SQLite-backed storage for image embeddings.
//!
//! Stores float vectors as BLOBs for visual similarity search.
//! Composite primary key `(asset_id, model)` allows storing embeddings from
//! different models without collision.
//! Only compiled when the `ai` feature is enabled.

use anyhow::{Context, Result};
use rusqlite::Connection;

/// Store and query image embeddings in SQLite.
pub struct EmbeddingStore<'a> {
    conn: &'a Connection,
}

impl<'a> EmbeddingStore<'a> {
    /// Create a new EmbeddingStore backed by the given connection.
    pub fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }

    /// Initialize the embeddings table (idempotent).
    /// Migrates old single-PK schema to composite PK if needed.
    pub fn initialize(conn: &Connection) -> Result<()> {
        // Check if table exists and has old schema (asset_id TEXT PRIMARY KEY).
        let needs_migration = Self::has_old_schema(conn);

        if needs_migration {
            // Migrate: rename old table, create new, copy data, drop old.
            conn.execute_batch(
                "ALTER TABLE embeddings RENAME TO embeddings_old;
                 CREATE TABLE embeddings (
                     asset_id TEXT NOT NULL,
                     model TEXT NOT NULL DEFAULT 'siglip-vit-b16-256',
                     embedding BLOB NOT NULL,
                     PRIMARY KEY (asset_id, model)
                 );
                 INSERT INTO embeddings (asset_id, model, embedding)
                     SELECT asset_id, COALESCE(model, 'siglip-vit-b16-256'), embedding
                     FROM embeddings_old;
                 DROP TABLE embeddings_old;"
            )
            .context("Failed to migrate embeddings table to composite PK")?;
        } else {
            conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS embeddings (
                    asset_id TEXT NOT NULL,
                    model TEXT NOT NULL DEFAULT 'siglip-vit-b16-256',
                    embedding BLOB NOT NULL,
                    PRIMARY KEY (asset_id, model)
                )",
            )
            .context("Failed to create embeddings table")?;
        }
        Ok(())
    }

    /// Check if the old schema (asset_id TEXT PRIMARY KEY, no composite) exists.
    fn has_old_schema(conn: &Connection) -> bool {
        // Table must exist
        let table_exists: bool = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='embeddings'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .map(|c| c > 0)
            .unwrap_or(false);

        if !table_exists {
            return false;
        }

        // Check if PK is single-column (old schema) by looking at table_info.
        // Old schema: asset_id is pk=1, model is pk=0.
        // New schema: asset_id is pk=1, model is pk=2.
        let mut stmt = match conn.prepare("PRAGMA table_info(embeddings)") {
            Ok(s) => s,
            Err(_) => return false,
        };
        let pk_count: i32 = stmt
            .query_map([], |row| row.get::<_, i32>(5)) // column 5 = pk
            .ok()
            .map(|rows| rows.filter_map(|r| r.ok()).filter(|pk| *pk > 0).count() as i32)
            .unwrap_or(0);

        pk_count == 1
    }

    /// Store an embedding for an asset with a specific model (insert or replace).
    pub fn store(&self, asset_id: &str, embedding: &[f32], model: &str) -> Result<()> {
        let blob = embedding_to_blob(embedding);
        self.conn.execute(
            "INSERT OR REPLACE INTO embeddings (asset_id, model, embedding) VALUES (?1, ?2, ?3)",
            rusqlite::params![asset_id, model, blob],
        ).context("Failed to store embedding")?;
        Ok(())
    }

    /// Retrieve an embedding for an asset and model.
    pub fn get(&self, asset_id: &str, model: &str) -> Result<Option<Vec<f32>>> {
        let mut stmt = self
            .conn
            .prepare("SELECT embedding FROM embeddings WHERE asset_id = ?1 AND model = ?2")?;
        let mut rows = stmt.query(rusqlite::params![asset_id, model])?;
        match rows.next()? {
            Some(row) => {
                let blob: Vec<u8> = row.get(0)?;
                Ok(Some(blob_to_embedding(&blob)))
            }
            None => Ok(None),
        }
    }

    /// Check if an asset has a stored embedding for a specific model.
    pub fn has_embedding(&self, asset_id: &str, model: &str) -> bool {
        self.conn
            .query_row(
                "SELECT 1 FROM embeddings WHERE asset_id = ?1 AND model = ?2",
                rusqlite::params![asset_id, model],
                |_| Ok(()),
            )
            .is_ok()
    }

    /// Count total stored embeddings.
    pub fn count(&self) -> usize {
        self.conn
            .query_row("SELECT COUNT(*) FROM embeddings", [], |row| row.get(0))
            .unwrap_or(0)
    }

    /// Remove an embedding for an asset and model.
    pub fn remove(&self, asset_id: &str, model: &str) -> Result<()> {
        self.conn.execute(
            "DELETE FROM embeddings WHERE asset_id = ?1 AND model = ?2",
            rusqlite::params![asset_id, model],
        )?;
        Ok(())
    }

    /// Find the most similar assets by cosine similarity (brute-force scan).
    /// Only compares embeddings from the same model.
    /// Returns `(asset_id, similarity)` pairs sorted by similarity descending.
    pub fn find_similar(
        &self,
        query_emb: &[f32],
        limit: usize,
        exclude_id: Option<&str>,
        model: &str,
    ) -> Result<Vec<(String, f32)>> {
        let mut stmt = self
            .conn
            .prepare("SELECT asset_id, embedding FROM embeddings WHERE model = ?1")?;
        let rows = stmt.query_map(rusqlite::params![model], |row| {
            let id: String = row.get(0)?;
            let blob: Vec<u8> = row.get(1)?;
            Ok((id, blob))
        })?;

        let mut results: Vec<(String, f32)> = Vec::new();
        for row in rows {
            let (id, blob) = row?;
            if exclude_id == Some(id.as_str()) {
                continue;
            }
            let emb = blob_to_embedding(&blob);
            let sim = crate::ai::cosine_similarity(query_emb, &emb);
            results.push((id, sim));
        }

        results.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());
        results.truncate(limit);
        Ok(results)
    }
}

/// Convert a float embedding to a byte blob (little-endian).
fn embedding_to_blob(embedding: &[f32]) -> Vec<u8> {
    embedding
        .iter()
        .flat_map(|f| f.to_le_bytes())
        .collect()
}

/// Convert a byte blob back to a float embedding.
fn blob_to_embedding(blob: &[u8]) -> Vec<f32> {
    blob.chunks_exact(4)
        .map(|chunk| f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_MODEL: &str = "siglip-vit-b16-256";

    fn setup_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        EmbeddingStore::initialize(&conn).unwrap();
        conn
    }

    #[test]
    fn store_and_retrieve() {
        let conn = setup_db();
        let store = EmbeddingStore::new(&conn);

        let emb = vec![0.1f32; 768];
        store.store("asset-1", &emb, TEST_MODEL).unwrap();

        let retrieved = store.get("asset-1", TEST_MODEL).unwrap().unwrap();
        assert_eq!(retrieved.len(), 768);
        assert!((retrieved[0] - 0.1).abs() < 1e-6);
    }

    #[test]
    fn get_nonexistent() {
        let conn = setup_db();
        let store = EmbeddingStore::new(&conn);
        assert!(store.get("nonexistent", TEST_MODEL).unwrap().is_none());
    }

    #[test]
    fn has_embedding_true() {
        let conn = setup_db();
        let store = EmbeddingStore::new(&conn);
        store.store("asset-1", &vec![0.0; 768], TEST_MODEL).unwrap();
        assert!(store.has_embedding("asset-1", TEST_MODEL));
    }

    #[test]
    fn has_embedding_false() {
        let conn = setup_db();
        let store = EmbeddingStore::new(&conn);
        assert!(!store.has_embedding("asset-1", TEST_MODEL));
    }

    #[test]
    fn count_empty() {
        let conn = setup_db();
        let store = EmbeddingStore::new(&conn);
        assert_eq!(store.count(), 0);
    }

    #[test]
    fn count_after_insert() {
        let conn = setup_db();
        let store = EmbeddingStore::new(&conn);
        store.store("a", &vec![0.0; 768], TEST_MODEL).unwrap();
        store.store("b", &vec![0.0; 768], TEST_MODEL).unwrap();
        assert_eq!(store.count(), 2);
    }

    #[test]
    fn remove_embedding() {
        let conn = setup_db();
        let store = EmbeddingStore::new(&conn);
        store.store("asset-1", &vec![0.0; 768], TEST_MODEL).unwrap();
        store.remove("asset-1", TEST_MODEL).unwrap();
        assert!(!store.has_embedding("asset-1", TEST_MODEL));
    }

    #[test]
    fn store_replaces_existing() {
        let conn = setup_db();
        let store = EmbeddingStore::new(&conn);
        store.store("asset-1", &vec![1.0; 768], TEST_MODEL).unwrap();
        store.store("asset-1", &vec![2.0; 768], TEST_MODEL).unwrap();

        let retrieved = store.get("asset-1", TEST_MODEL).unwrap().unwrap();
        assert!((retrieved[0] - 2.0).abs() < 1e-6);
        assert_eq!(store.count(), 1);
    }

    #[test]
    fn find_similar_basic() {
        let conn = setup_db();
        let store = EmbeddingStore::new(&conn);

        let base = vec![1.0f32; 768];
        let similar = vec![0.9f32; 768];
        let mut different = vec![0.0f32; 768];
        different[0] = 1.0;

        store.store("base", &base, TEST_MODEL).unwrap();
        store.store("similar", &similar, TEST_MODEL).unwrap();
        store.store("different", &different, TEST_MODEL).unwrap();

        let results = store.find_similar(&base, 2, Some("base"), TEST_MODEL).unwrap();
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].0, "similar");
        assert!(results[0].1 > results[1].1);
    }

    #[test]
    fn find_similar_respects_limit() {
        let conn = setup_db();
        let store = EmbeddingStore::new(&conn);

        for i in 0..10 {
            store
                .store(&format!("asset-{i}"), &vec![i as f32; 768], TEST_MODEL)
                .unwrap();
        }

        let query = vec![5.0f32; 768];
        let results = store.find_similar(&query, 3, None, TEST_MODEL).unwrap();
        assert_eq!(results.len(), 3);
    }

    #[test]
    fn multi_model_embeddings() {
        let conn = setup_db();
        let store = EmbeddingStore::new(&conn);

        let emb_b = vec![1.0f32; 768];
        let emb_l = vec![2.0f32; 1024];

        store.store("asset-1", &emb_b, "siglip-vit-b16-256").unwrap();
        store.store("asset-1", &emb_l, "siglip-vit-l16-256").unwrap();

        // Both stored — count is 2
        assert_eq!(store.count(), 2);

        // Each model returns its own embedding
        let got_b = store.get("asset-1", "siglip-vit-b16-256").unwrap().unwrap();
        assert_eq!(got_b.len(), 768);
        assert!((got_b[0] - 1.0).abs() < 1e-6);

        let got_l = store.get("asset-1", "siglip-vit-l16-256").unwrap().unwrap();
        assert_eq!(got_l.len(), 1024);
        assert!((got_l[0] - 2.0).abs() < 1e-6);

        // has_embedding is model-specific
        assert!(store.has_embedding("asset-1", "siglip-vit-b16-256"));
        assert!(store.has_embedding("asset-1", "siglip-vit-l16-256"));
        assert!(!store.has_embedding("asset-1", "nonexistent-model"));

        // find_similar is model-scoped
        store.store("asset-2", &vec![0.9f32; 768], "siglip-vit-b16-256").unwrap();
        let results = store.find_similar(&emb_b, 10, None, "siglip-vit-b16-256").unwrap();
        // Should find asset-1 and asset-2 (both b16), not the l16 embedding
        assert_eq!(results.len(), 2);

        // Remove is model-specific
        store.remove("asset-1", "siglip-vit-b16-256").unwrap();
        assert!(!store.has_embedding("asset-1", "siglip-vit-b16-256"));
        assert!(store.has_embedding("asset-1", "siglip-vit-l16-256"));
    }

    #[test]
    fn migrate_old_schema() {
        let conn = Connection::open_in_memory().unwrap();

        // Create old-style table with single PK
        conn.execute_batch(
            "CREATE TABLE embeddings (
                asset_id TEXT PRIMARY KEY,
                embedding BLOB NOT NULL,
                model TEXT NOT NULL DEFAULT 'siglip-vit-b16-256'
            )"
        ).unwrap();

        // Insert some data
        let emb = embedding_to_blob(&vec![0.5f32; 768]);
        conn.execute(
            "INSERT INTO embeddings (asset_id, embedding) VALUES ('a1', ?1)",
            rusqlite::params![emb],
        ).unwrap();

        // Run initialize — should migrate
        EmbeddingStore::initialize(&conn).unwrap();

        // Data should be preserved
        let store = EmbeddingStore::new(&conn);
        let got = store.get("a1", "siglip-vit-b16-256").unwrap().unwrap();
        assert_eq!(got.len(), 768);
        assert!((got[0] - 0.5).abs() < 1e-6);

        // Should now support multi-model
        store.store("a1", &vec![1.0; 1024], "siglip-vit-l16-256").unwrap();
        assert_eq!(store.count(), 2);
    }

    #[test]
    fn blob_round_trip() {
        let original = vec![1.5f32, -2.3, 0.0, std::f32::consts::PI];
        let blob = embedding_to_blob(&original);
        let recovered = blob_to_embedding(&blob);
        assert_eq!(original, recovered);
    }
}
