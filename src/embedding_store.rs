//! SQLite-backed storage for image embeddings.
//!
//! Stores 768-dimensional float vectors as BLOBs for visual similarity search.
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
    pub fn initialize(conn: &Connection) -> Result<()> {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS embeddings (
                asset_id TEXT PRIMARY KEY,
                embedding BLOB NOT NULL,
                model TEXT NOT NULL DEFAULT 'siglip-vit-b16-256'
            )",
        )
        .context("Failed to create embeddings table")?;
        Ok(())
    }

    /// Store an embedding for an asset (insert or replace).
    pub fn store(&self, asset_id: &str, embedding: &[f32]) -> Result<()> {
        let blob = embedding_to_blob(embedding);
        self.conn.execute(
            "INSERT OR REPLACE INTO embeddings (asset_id, embedding, model) VALUES (?1, ?2, 'siglip-vit-b16-256')",
            rusqlite::params![asset_id, blob],
        ).context("Failed to store embedding")?;
        Ok(())
    }

    /// Retrieve an embedding for an asset.
    pub fn get(&self, asset_id: &str) -> Result<Option<Vec<f32>>> {
        let mut stmt = self
            .conn
            .prepare("SELECT embedding FROM embeddings WHERE asset_id = ?1")?;
        let mut rows = stmt.query(rusqlite::params![asset_id])?;
        match rows.next()? {
            Some(row) => {
                let blob: Vec<u8> = row.get(0)?;
                Ok(Some(blob_to_embedding(&blob)))
            }
            None => Ok(None),
        }
    }

    /// Check if an asset has a stored embedding.
    pub fn has_embedding(&self, asset_id: &str) -> bool {
        self.conn
            .query_row(
                "SELECT 1 FROM embeddings WHERE asset_id = ?1",
                rusqlite::params![asset_id],
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

    /// Remove an embedding for an asset.
    pub fn remove(&self, asset_id: &str) -> Result<()> {
        self.conn.execute(
            "DELETE FROM embeddings WHERE asset_id = ?1",
            rusqlite::params![asset_id],
        )?;
        Ok(())
    }

    /// Find the most similar assets by cosine similarity (brute-force scan).
    /// Returns `(asset_id, similarity)` pairs sorted by similarity descending.
    pub fn find_similar(
        &self,
        query_emb: &[f32],
        limit: usize,
        exclude_id: Option<&str>,
    ) -> Result<Vec<(String, f32)>> {
        let mut stmt = self
            .conn
            .prepare("SELECT asset_id, embedding FROM embeddings")?;
        let rows = stmt.query_map([], |row| {
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
        store.store("asset-1", &emb).unwrap();

        let retrieved = store.get("asset-1").unwrap().unwrap();
        assert_eq!(retrieved.len(), 768);
        assert!((retrieved[0] - 0.1).abs() < 1e-6);
    }

    #[test]
    fn get_nonexistent() {
        let conn = setup_db();
        let store = EmbeddingStore::new(&conn);
        assert!(store.get("nonexistent").unwrap().is_none());
    }

    #[test]
    fn has_embedding_true() {
        let conn = setup_db();
        let store = EmbeddingStore::new(&conn);
        store.store("asset-1", &vec![0.0; 768]).unwrap();
        assert!(store.has_embedding("asset-1"));
    }

    #[test]
    fn has_embedding_false() {
        let conn = setup_db();
        let store = EmbeddingStore::new(&conn);
        assert!(!store.has_embedding("asset-1"));
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
        store.store("a", &vec![0.0; 768]).unwrap();
        store.store("b", &vec![0.0; 768]).unwrap();
        assert_eq!(store.count(), 2);
    }

    #[test]
    fn remove_embedding() {
        let conn = setup_db();
        let store = EmbeddingStore::new(&conn);
        store.store("asset-1", &vec![0.0; 768]).unwrap();
        store.remove("asset-1").unwrap();
        assert!(!store.has_embedding("asset-1"));
    }

    #[test]
    fn store_replaces_existing() {
        let conn = setup_db();
        let store = EmbeddingStore::new(&conn);
        store.store("asset-1", &vec![1.0; 768]).unwrap();
        store.store("asset-1", &vec![2.0; 768]).unwrap();

        let retrieved = store.get("asset-1").unwrap().unwrap();
        assert!((retrieved[0] - 2.0).abs() < 1e-6);
        assert_eq!(store.count(), 1);
    }

    #[test]
    fn find_similar_basic() {
        let conn = setup_db();
        let store = EmbeddingStore::new(&conn);

        // Store three embeddings: one similar, one different
        let base = vec![1.0f32; 768];
        let similar = vec![0.9f32; 768];
        let mut different = vec![0.0f32; 768];
        different[0] = 1.0;

        store.store("base", &base).unwrap();
        store.store("similar", &similar).unwrap();
        store.store("different", &different).unwrap();

        let results = store.find_similar(&base, 2, Some("base")).unwrap();
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
                .store(&format!("asset-{i}"), &vec![i as f32; 768])
                .unwrap();
        }

        let query = vec![5.0f32; 768];
        let results = store.find_similar(&query, 3, None).unwrap();
        assert_eq!(results.len(), 3);
    }

    #[test]
    fn blob_round_trip() {
        let original = vec![1.5f32, -2.3, 0.0, std::f32::consts::PI];
        let blob = embedding_to_blob(&original);
        let recovered = blob_to_embedding(&blob);
        assert_eq!(original, recovered);
    }
}
