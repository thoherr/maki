//! Collection store — named, manually-curated lists of assets persisted
//! alongside the catalog. Distinct from saved searches (which are query
//! definitions) and from tags (which are content metadata).
//!
//! # Persistence rule (architecture audit F6)
//!
//! `collections.yaml` in the catalog root is the source of truth; the
//! `collections` / `collection_assets` tables (schema v13) are the derived
//! cache. The store owns the dual write: every mutator (`create`,
//! `add_assets`, `remove_assets`, `remove_asset`, `delete`) runs its SQLite
//! statements and the atomic YAML export in one transaction and returns
//! the error if either side fails — a failed YAML write rolls the rows
//! back. Callers never save the YAML after a mutator.
//!
//! Mutators need a store bound to the catalog root
//! ([`CollectionStore::open`]). [`CollectionStore::new`] gives a handle
//! without a root for queries and for [`CollectionStore::import_from_yaml`],
//! where the YAML is the input, not the output (`rebuild-catalog`); a
//! mutator on such a handle returns an error.

use std::path::{Path, PathBuf};

use anyhow::{anyhow, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::error::MakiError;

/// A collection (static album) — a manually curated list of asset IDs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Collection {
    pub id: Uuid,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub created_at: DateTime<Utc>,
    pub asset_ids: Vec<String>,
}

/// Summary of a collection for listing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CollectionSummary {
    pub id: String,
    pub name: String,
    pub description: Option<String>,
    pub asset_count: u64,
    pub created_at: String,
}

/// Wrapper for the YAML file.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CollectionsFile {
    pub collections: Vec<Collection>,
}

const FILENAME: &str = "collections.yaml";

/// Load `collections.yaml` from the catalog root. A missing file is an
/// empty list; a file that does not parse is an error (it is the source
/// of truth, never silently replaced by defaults). Used by `rebuild-catalog`.
pub fn load_yaml(catalog_root: &Path) -> Result<CollectionsFile> {
    crate::yaml_file::load_or_default(&catalog_root.join(FILENAME))
}

/// Replace `collections.yaml` atomically (temp file + rename).
fn save_yaml(catalog_root: &Path, file: &CollectionsFile) -> Result<()> {
    crate::yaml_file::save_atomic(&catalog_root.join(FILENAME), file)
}

/// Collection operations backed by the SQLite catalog, persisting the
/// YAML source of truth (see the module docs for the rule).
pub struct CollectionStore<'a> {
    conn: &'a rusqlite::Connection,
    /// Catalog root holding `collections.yaml`; `None` for a query-only
    /// handle from [`CollectionStore::new`].
    catalog_root: Option<PathBuf>,
}

impl<'a> CollectionStore<'a> {
    /// Handle without a catalog root: queries and `import_from_yaml` work,
    /// the mutators return an error. Use [`CollectionStore::open`]
    /// wherever a user operation changes collections. The tables are
    /// created by the schema migrations (v13), never here.
    pub fn new(catalog: &'a crate::catalog::Catalog) -> Self {
        Self::on(catalog.conn())
    }

    /// Read-only handle on a bare connection (the YAML export runs on the
    /// connection of the committing transaction).
    fn on(conn: &'a rusqlite::Connection) -> Self {
        Self { conn, catalog_root: None }
    }

    /// Full handle bound to the catalog root that holds
    /// `collections.yaml`; mutators persist to it (see the module docs).
    pub fn open(catalog: &'a crate::catalog::Catalog, catalog_root: &Path) -> Self {
        Self { conn: catalog.conn(), catalog_root: Some(catalog_root.to_path_buf()) }
    }

    /// Write `collections.yaml` from the current SQLite rows (atomic
    /// temp-file + rename) — immediately, or at the commit of the
    /// transaction the connection is in (`crate::tx`). The mutators call
    /// it themselves.
    fn persist(&self) -> Result<()> {
        let root = self.catalog_root.as_deref().ok_or_else(|| {
            anyhow!("CollectionStore opened without a catalog root (CollectionStore::new); use CollectionStore::open to change collections")
        })?;
        crate::tx::export_at_commit(self.conn, "collections", root, |conn, root| {
            save_yaml(root, &CollectionStore::on(conn).export_all()?)
        })
    }

    /// Run `f` inside one SQLite transaction (joining an enclosing one),
    /// committing on `Ok` and rolling back on `Err` — the same contract as
    /// `Catalog::with_tx`, for a store that only holds the connection.
    fn with_tx<T>(&self, f: impl FnOnce() -> Result<T>) -> Result<T> {
        crate::tx::run(self.conn, f)
    }

    /// A mutation: SQLite rows via `f`, then the YAML export, all inside
    /// one transaction so a failed YAML write rolls the rows back and
    /// neither store is left ahead of the other. Joined to an enclosing
    /// transaction, the export happens at that transaction's commit.
    fn mutate<T>(&self, f: impl FnOnce() -> Result<T>) -> Result<T> {
        self.with_tx(|| {
            let out = f()?;
            self.persist()?;
            Ok(out)
        })
    }

    /// The collection called `name`, typed not-found otherwise.
    fn require(&self, name: &str) -> Result<Collection> {
        self.get_by_name(name)?
            .ok_or_else(|| MakiError::not_found("collection", name).into())
    }

    /// `Ok` when `assets.id = asset_id` exists.
    fn require_asset(&self, asset_id: &str) -> Result<()> {
        let exists: bool = self.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM assets WHERE id = ?1)",
            rusqlite::params![asset_id],
            |row| row.get(0),
        )?;
        if exists {
            Ok(())
        } else {
            Err(MakiError::not_found("asset", asset_id).into())
        }
    }

    /// Create a new collection. A second collection with the same name is
    /// a [`MakiError::Conflict`] (the `UNIQUE` constraint, mapped here so no
    /// caller has to recognise SQLite's message).
    pub fn create(&self, name: &str, description: Option<&str>) -> Result<Collection> {
        self.mutate(|| {
            let id = Uuid::new_v4();
            let now = Utc::now();
            self.conn
                .execute(
                    "INSERT INTO collections (id, name, description, created_at) VALUES (?1, ?2, ?3, ?4)",
                    rusqlite::params![id.to_string(), name, description, now.to_rfc3339()],
                )
                .map_err(|e| match e {
                    rusqlite::Error::SqliteFailure(ref f, _)
                        if f.code == rusqlite::ErrorCode::ConstraintViolation =>
                    {
                        anyhow::Error::from(MakiError::Conflict(format!(
                            "a collection named '{name}' already exists"
                        )))
                    }
                    other => other.into(),
                })?;
            Ok(Collection {
                id,
                name: name.to_string(),
                description: description.map(|s| s.to_string()),
                created_at: now,
                asset_ids: Vec::new(),
            })
        })
    }

    /// List all collections with summary info.
    pub fn list(&self) -> Result<Vec<CollectionSummary>> {
        let mut stmt = self.conn.prepare(
            "SELECT c.id, c.name, c.description, c.created_at,
                    (SELECT COUNT(*) FROM collection_assets ca WHERE ca.collection_id = c.id)
             FROM collections c ORDER BY c.name",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(CollectionSummary {
                id: row.get(0)?,
                name: row.get(1)?,
                description: row.get(2)?,
                created_at: row.get(3)?,
                asset_count: row.get::<_, i64>(4)? as u64,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Get a collection by name.
    pub fn get_by_name(&self, name: &str) -> Result<Option<Collection>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, name, description, created_at FROM collections WHERE name = ?1",
        )?;
        let mut rows = stmt.query(rusqlite::params![name])?;
        if let Some(row) = rows.next()? {
            let id_str: String = row.get(0)?;
            let id: Uuid = id_str.parse().map_err(|e| anyhow!("invalid UUID: {e}"))?;
            let name: String = row.get(1)?;
            let description: Option<String> = row.get(2)?;
            let created_at_str: String = row.get(3)?;
            let created_at: DateTime<Utc> = created_at_str.parse().map_err(|e| anyhow!("invalid date: {e}"))?;

            // Load asset IDs
            let asset_ids = self.get_asset_ids(&id_str)?;

            Ok(Some(Collection {
                id,
                name,
                description,
                created_at,
                asset_ids,
            }))
        } else {
            Ok(None)
        }
    }

    /// Get asset IDs for a collection.
    pub fn get_asset_ids(&self, collection_id: &str) -> Result<Vec<String>> {
        let mut stmt = self.conn.prepare(
            "SELECT asset_id FROM collection_assets WHERE collection_id = ?1 ORDER BY added_at",
        )?;
        let rows = stmt.query_map(rusqlite::params![collection_id], |row| {
            row.get::<_, String>(0)
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Add assets to a collection. Returns the number of new additions
    /// (assets already in the collection are skipped). An unknown
    /// collection or asset id is a typed not-found and nothing is added.
    pub fn add_assets(&self, collection_name: &str, asset_ids: &[String]) -> Result<u32> {
        self.mutate(|| {
            let col = self.require(collection_name)?;
            let col_id = col.id.to_string();
            let now = Utc::now().to_rfc3339();
            let mut added = 0u32;
            for id in asset_ids {
                self.require_asset(id)?;
                let n = self.conn.execute(
                    "INSERT OR IGNORE INTO collection_assets (collection_id, asset_id, added_at) VALUES (?1, ?2, ?3)",
                    rusqlite::params![col_id, id, now],
                )?;
                if n > 0 {
                    added += 1;
                }
            }
            Ok(added)
        })
    }

    /// Remove assets from a collection. Returns the number of removals.
    pub fn remove_assets(&self, collection_name: &str, asset_ids: &[String]) -> Result<u32> {
        self.mutate(|| {
            let col = self.require(collection_name)?;
            let col_id = col.id.to_string();
            let mut removed = 0u32;
            for id in asset_ids {
                let n = self.conn.execute(
                    "DELETE FROM collection_assets WHERE collection_id = ?1 AND asset_id = ?2",
                    rusqlite::params![col_id, id],
                )?;
                if n > 0 {
                    removed += 1;
                }
            }
            Ok(removed)
        })
    }

    /// Remove an asset from every collection it is in (asset deletion /
    /// orphan purge). Returns the number of memberships removed; when it is
    /// zero the YAML is not rewritten, so batch purges can call this per
    /// asset cheaply.
    pub fn remove_asset(&self, asset_id: &str) -> Result<u32> {
        self.with_tx(|| {
            let n = self.conn.execute(
                "DELETE FROM collection_assets WHERE asset_id = ?1",
                rusqlite::params![asset_id],
            )? as u32;
            if n > 0 {
                self.persist()?;
            }
            Ok(n)
        })
    }

    /// Delete a collection and its membership records.
    pub fn delete(&self, name: &str) -> Result<()> {
        self.mutate(|| {
            let col = self.require(name)?;
            let col_id = col.id.to_string();
            self.conn.execute("DELETE FROM collection_assets WHERE collection_id = ?1", rusqlite::params![col_id])?;
            self.conn.execute("DELETE FROM collections WHERE id = ?1", rusqlite::params![col_id])?;
            Ok(())
        })
    }

    /// Get all collection names an asset belongs to.
    pub fn collections_for_asset(&self, asset_id: &str) -> Result<Vec<String>> {
        let mut stmt = self.conn.prepare(
            "SELECT c.name FROM collections c
             JOIN collection_assets ca ON ca.collection_id = c.id
             WHERE ca.asset_id = ?1
             ORDER BY c.name",
        )?;
        let rows = stmt.query_map(rusqlite::params![asset_id], |row| {
            row.get::<_, String>(0)
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Get all asset IDs in a collection by name (for use as search filter).
    pub fn asset_ids_for_collection(&self, name: &str) -> Result<Vec<String>> {
        let col = self.require(name)?;
        self.get_asset_ids(&col.id.to_string())
    }

    /// Export all collections to a CollectionsFile for YAML persistence.
    pub fn export_all(&self) -> Result<CollectionsFile> {
        let mut stmt = self.conn.prepare(
            "SELECT id, name, description, created_at FROM collections ORDER BY name",
        )?;
        let mut collections = Vec::new();
        let mut rows = stmt.query([])?;
        while let Some(row) = rows.next()? {
            let id_str: String = row.get(0)?;
            let id: Uuid = id_str.parse().map_err(|e| anyhow!("invalid UUID: {e}"))?;
            let name: String = row.get(1)?;
            let description: Option<String> = row.get(2)?;
            let created_at_str: String = row.get(3)?;
            let created_at: DateTime<Utc> = created_at_str.parse().map_err(|e| anyhow!("invalid date: {e}"))?;
            let asset_ids = self.get_asset_ids(&id_str)?;
            collections.push(Collection {
                id,
                name,
                description,
                created_at,
                asset_ids,
            });
        }
        Ok(CollectionsFile { collections })
    }

    /// Import collections from YAML into SQLite (used by rebuild-catalog).
    /// SQLite-only: the YAML is the input here. Members whose asset row no
    /// longer exists are skipped (their sidecar is gone); a failed
    /// statement is an error (formerly swallowed, so a rebuild could lose
    /// memberships silently).
    pub fn import_from_yaml(&self, file: &CollectionsFile) -> Result<u32> {
        self.with_tx(|| {
            let mut count = 0u32;
            for col in &file.collections {
                self.conn.execute(
                    "INSERT OR REPLACE INTO collections (id, name, description, created_at) VALUES (?1, ?2, ?3, ?4)",
                    rusqlite::params![col.id.to_string(), col.name, col.description, col.created_at.to_rfc3339()],
                )?;
                for asset_id in &col.asset_ids {
                    self.conn.execute(
                        "INSERT OR IGNORE INTO collection_assets (collection_id, asset_id, added_at)
                         SELECT ?1, id, ?3 FROM assets WHERE id = ?2",
                        rusqlite::params![col.id.to_string(), asset_id, col.created_at.to_rfc3339()],
                    )?;
                }
                count += 1;
            }
            Ok(count)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A real in-memory catalog (so the tables are the migrated ones)
    /// plus a temp dir standing in for the catalog root that receives the
    /// YAML file.
    fn setup_db() -> (crate::catalog::Catalog, tempfile::TempDir) {
        (crate::test_support::memory_catalog(), tempfile::tempdir().unwrap())
    }

    fn insert_test_asset(conn: &crate::catalog::Catalog, id: &str) {
        conn.conn().execute(
            "INSERT INTO assets (id, created_at, asset_type) VALUES (?1, '2026-01-01T00:00:00Z', 'image')",
            rusqlite::params![id],
        )
        .unwrap();
    }

    #[test]
    fn create_and_list() {
        let (conn, dir) = setup_db();
        let store = CollectionStore::open(&conn, dir.path());

        store.create("Portfolio", Some("Best shots")).unwrap();
        store.create("Favorites", None).unwrap();

        let list = store.list().unwrap();
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].name, "Favorites");
        assert_eq!(list[1].name, "Portfolio");
        assert_eq!(list[1].description.as_deref(), Some("Best shots"));

        // The YAML side was written by the mutator itself.
        let yaml = load_yaml(dir.path()).unwrap();
        assert_eq!(yaml.collections.len(), 2);
    }

    #[test]
    fn add_and_remove_assets() {
        let (conn, dir) = setup_db();
        insert_test_asset(&conn, "asset-1");
        insert_test_asset(&conn, "asset-2");
        insert_test_asset(&conn, "asset-3");

        let store = CollectionStore::open(&conn, dir.path());
        store.create("Test", None).unwrap();

        let added = store.add_assets("Test", &["asset-1".into(), "asset-2".into()]).unwrap();
        assert_eq!(added, 2);

        // Duplicate add
        let added = store.add_assets("Test", &["asset-1".into()]).unwrap();
        assert_eq!(added, 0);

        let list = store.list().unwrap();
        assert_eq!(list[0].asset_count, 2);

        let removed = store.remove_assets("Test", &["asset-1".into()]).unwrap();
        assert_eq!(removed, 1);

        let col = store.get_by_name("Test").unwrap().unwrap();
        assert_eq!(col.asset_ids.len(), 1);
        assert_eq!(col.asset_ids[0], "asset-2");
        let yaml = load_yaml(dir.path()).unwrap();
        assert_eq!(yaml.collections[0].asset_ids, vec!["asset-2"]);
    }

    #[test]
    fn add_unknown_asset_is_not_found_and_rolled_back() {
        let (conn, dir) = setup_db();
        insert_test_asset(&conn, "asset-1");
        let store = CollectionStore::open(&conn, dir.path());
        store.create("Test", None).unwrap();

        let err = store.add_assets("Test", &["asset-1".into(), "ghost".into()]).unwrap_err();
        assert!(matches!(crate::error::classify(&err), Some(MakiError::NotFound { kind: "asset", .. })));
        assert_eq!(store.list().unwrap()[0].asset_count, 0, "partial add must roll back");
    }

    #[test]
    fn unknown_collection_is_typed_not_found() {
        let (conn, dir) = setup_db();
        let store = CollectionStore::open(&conn, dir.path());
        for err in [
            store.add_assets("Nope", &["x".into()]).unwrap_err(),
            store.remove_assets("Nope", &["x".into()]).unwrap_err(),
            store.delete("Nope").unwrap_err(),
            store.asset_ids_for_collection("Nope").unwrap_err(),
        ] {
            assert!(
                matches!(crate::error::classify(&err), Some(MakiError::NotFound { kind: "collection", .. })),
                "{err:#}"
            );
            assert_eq!(err.to_string(), "no collection found matching 'Nope'");
        }
    }

    #[test]
    fn remove_asset_everywhere() {
        let (conn, dir) = setup_db();
        insert_test_asset(&conn, "asset-1");
        let store = CollectionStore::open(&conn, dir.path());
        store.create("A", None).unwrap();
        store.create("B", None).unwrap();
        store.add_assets("A", &["asset-1".into()]).unwrap();
        store.add_assets("B", &["asset-1".into()]).unwrap();

        assert_eq!(store.remove_asset("asset-1").unwrap(), 2);
        assert!(store.collections_for_asset("asset-1").unwrap().is_empty());
        let yaml = load_yaml(dir.path()).unwrap();
        assert!(yaml.collections.iter().all(|c| c.asset_ids.is_empty()));
        // Nothing to remove: no root needed, nothing written.
        assert_eq!(CollectionStore::new(&conn).remove_asset("asset-1").unwrap(), 0);
    }

    #[test]
    fn delete_collection() {
        let (conn, dir) = setup_db();
        insert_test_asset(&conn, "asset-1");

        let store = CollectionStore::open(&conn, dir.path());
        store.create("Temp", None).unwrap();
        store.add_assets("Temp", &["asset-1".into()]).unwrap();

        store.delete("Temp").unwrap();
        assert!(store.get_by_name("Temp").unwrap().is_none());
        assert!(store.list().unwrap().is_empty());
        assert!(load_yaml(dir.path()).unwrap().collections.is_empty());
    }

    #[test]
    fn collections_for_asset() {
        let (conn, dir) = setup_db();
        insert_test_asset(&conn, "asset-1");

        let store = CollectionStore::open(&conn, dir.path());
        store.create("A", None).unwrap();
        store.create("B", None).unwrap();
        store.add_assets("A", &["asset-1".into()]).unwrap();
        store.add_assets("B", &["asset-1".into()]).unwrap();

        let cols = store.collections_for_asset("asset-1").unwrap();
        assert_eq!(cols, vec!["A", "B"]);
    }

    #[test]
    fn export_and_import() {
        let (conn, dir) = setup_db();
        insert_test_asset(&conn, "asset-1");

        let store = CollectionStore::open(&conn, dir.path());
        store.create("Test", Some("desc")).unwrap();
        store.add_assets("Test", &["asset-1".into()]).unwrap();

        let mut exported = store.export_all().unwrap();
        assert_eq!(exported.collections.len(), 1);
        assert_eq!(exported.collections[0].asset_ids.len(), 1);

        // Wipe and reimport through a root-less handle (rebuild's shape).
        // A member whose asset row is gone is skipped, not an error.
        conn.conn().execute_batch("DELETE FROM collection_assets; DELETE FROM collections;").unwrap();
        assert!(store.list().unwrap().is_empty());
        exported.collections[0].asset_ids.push("vanished".into());

        let imported = CollectionStore::new(&conn).import_from_yaml(&exported).unwrap();
        assert_eq!(imported, 1);

        let list = store.list().unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].asset_count, 1);
    }

    #[test]
    fn duplicate_name_is_a_conflict() {
        let (conn, dir) = setup_db();
        let store = CollectionStore::open(&conn, dir.path());
        store.create("Dup", None).unwrap();
        let err = store.create("Dup", None).unwrap_err();
        assert!(matches!(crate::error::classify(&err), Some(MakiError::Conflict(_))), "{err:#}");
        assert_eq!(crate::error::http_status_for(&err), Some(409));
        assert_eq!(store.list().unwrap().len(), 1);
    }

    #[test]
    fn mutator_on_read_only_handle_fails_without_touching_rows() {
        let (conn, _dir) = setup_db();
        let store = CollectionStore::new(&conn);
        let err = store.create("X", None).unwrap_err();
        assert!(format!("{err:#}").contains("CollectionStore::open"), "{err:#}");
        assert!(store.list().unwrap().is_empty());
    }

    #[test]
    fn failed_yaml_write_rolls_the_rows_back() {
        let (conn, dir) = setup_db();
        insert_test_asset(&conn, "asset-1");
        let good = CollectionStore::open(&conn, dir.path());
        good.create("Keep", None).unwrap();

        let bogus_root = dir.path().join("not-a-dir");
        std::fs::write(&bogus_root, "x").unwrap();
        let broken = CollectionStore::open(&conn, &bogus_root);

        assert!(broken.create("New", None).is_err());
        assert!(broken.add_assets("Keep", &["asset-1".into()]).is_err());
        assert!(broken.delete("Keep").is_err());

        let list = good.list().unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].name, "Keep");
        assert_eq!(list[0].asset_count, 0);
        let yaml = load_yaml(dir.path()).unwrap();
        assert_eq!(yaml.collections.len(), 1);
        assert!(yaml.collections[0].asset_ids.is_empty());
    }

    #[test]
    fn yaml_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let file = CollectionsFile {
            collections: vec![Collection {
                id: Uuid::new_v4(),
                name: "Test".to_string(),
                description: Some("A test collection".to_string()),
                created_at: Utc::now(),
                asset_ids: vec!["abc".to_string(), "def".to_string()],
            }],
        };
        save_yaml(dir.path(), &file).unwrap();
        let loaded = load_yaml(dir.path()).unwrap();
        assert_eq!(loaded.collections.len(), 1);
        assert_eq!(loaded.collections[0].name, "Test");
        assert_eq!(loaded.collections[0].asset_ids.len(), 2);
    }

    #[test]
    fn corrupt_yaml_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(FILENAME), "collections: [unclosed").unwrap();
        assert!(load_yaml(dir.path()).is_err());
    }
}
