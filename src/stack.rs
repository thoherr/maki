//! Stack store — groups independent assets so they collapse into a single
//! tile in browse views (e.g. multiple shots of the same scene). Distinct
//! from variant grouping (`group` / `auto-group`), which marks files as
//! different renditions of the same asset.
//!
//! # Persistence rule (architecture audit F6)
//!
//! `stacks.yaml` in the catalog root is the source of truth; the `stacks`
//! table and the denormalised `assets.stack_id` / `stack_position` columns
//! (schema v13 / v1) are the derived cache. The store owns the dual write:
//! every mutator (`create`, `add`, `remove`, `set_pick`, `dissolve`) runs
//! its SQLite statements and the atomic YAML export in one transaction and
//! returns the error if either side fails — a failed YAML write rolls the
//! rows back. Callers never save the YAML after a mutator.
//!
//! Mutators need a store bound to the catalog root ([`StackStore::open`]).
//! [`StackStore::new`] gives a handle without a root for queries and for
//! [`StackStore::import_from_yaml`], where the YAML is the input, not the
//! output (`rebuild-catalog`); a mutator on such a handle returns an error.

use std::path::{Path, PathBuf};

use anyhow::{anyhow, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::error::MakiError;

/// A stack (scene grouping) — a set of assets collapsed into one "pick" in the browse grid.
/// Anonymous (no name/description), position-based ordering.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Stack {
    pub id: Uuid,
    pub created_at: DateTime<Utc>,
    /// Ordered list of asset IDs — index 0 is the pick.
    pub asset_ids: Vec<String>,
}

/// Summary of a stack for listing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StackSummary {
    pub id: String,
    pub member_count: u64,
    pub created_at: String,
    pub pick_asset_id: Option<String>,
}

/// Wrapper for the YAML file.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct StacksFile {
    pub stacks: Vec<Stack>,
}

const FILENAME: &str = "stacks.yaml";

/// Load `stacks.yaml` from the catalog root. A missing file is an empty
/// list; a file that does not parse is an error (it is the source of
/// truth, never silently replaced by defaults). Used by `rebuild-catalog`.
pub fn load_yaml(catalog_root: &Path) -> Result<StacksFile> {
    crate::yaml_file::load_or_default(&catalog_root.join(FILENAME))
}

/// Replace `stacks.yaml` atomically (temp file + rename).
fn save_yaml(catalog_root: &Path, file: &StacksFile) -> Result<()> {
    crate::yaml_file::save_atomic(&catalog_root.join(FILENAME), file)
}

/// Stack operations backed by the SQLite catalog, persisting the YAML
/// source of truth (see the module docs for the rule).
pub struct StackStore<'a> {
    conn: &'a rusqlite::Connection,
    /// Catalog root holding `stacks.yaml`; `None` for a query-only handle
    /// from [`StackStore::new`].
    catalog_root: Option<PathBuf>,
}

impl<'a> StackStore<'a> {
    /// Handle without a catalog root: queries and `import_from_yaml` work,
    /// the mutators return an error. Use [`StackStore::open`] wherever a
    /// user operation changes stacks. The tables are created by the
    /// schema migrations (v13), never here.
    pub fn new(conn: &'a rusqlite::Connection) -> Self {
        Self { conn, catalog_root: None }
    }

    /// Full handle bound to the catalog root that holds `stacks.yaml`;
    /// mutators persist to it (see the module docs).
    pub fn open(conn: &'a rusqlite::Connection, catalog_root: &Path) -> Self {
        Self { conn, catalog_root: Some(catalog_root.to_path_buf()) }
    }

    /// Write `stacks.yaml` from the current SQLite rows (atomic temp-file
    /// + rename). The mutators call it themselves.
    fn persist(&self) -> Result<()> {
        let root = self.catalog_root.as_deref().ok_or_else(|| {
            anyhow!("StackStore opened without a catalog root (StackStore::new); use StackStore::open to change stacks")
        })?;
        let file = self.export_all()?;
        save_yaml(root, &file)
    }

    /// Run `f` inside one SQLite transaction (joining an enclosing one),
    /// committing on `Ok` and rolling back on `Err` — the same contract as
    /// `Catalog::with_tx`, for a store that only holds the connection.
    fn with_tx<T>(&self, f: impl FnOnce() -> Result<T>) -> Result<T> {
        if !self.conn.is_autocommit() {
            return f();
        }
        let tx = self.conn.unchecked_transaction()?;
        let out = f()?;
        tx.commit()?;
        Ok(out)
    }

    /// A mutation: SQLite rows via `f`, then the YAML export, all inside
    /// one transaction so a failed YAML write rolls the rows back and
    /// neither store is left ahead of the other.
    fn mutate<T>(&self, f: impl FnOnce() -> Result<T>) -> Result<T> {
        self.with_tx(|| {
            let out = f()?;
            self.persist()?;
            Ok(out)
        })
    }

    /// `stack_id` of an asset, `None` when it is not stacked. An unknown
    /// asset id also yields `None` (the row does not exist).
    fn current_stack_id(&self, asset_id: &str) -> Result<Option<String>> {
        use rusqlite::OptionalExtension;
        Ok(self
            .conn
            .query_row(
                "SELECT stack_id FROM assets WHERE id = ?1",
                rusqlite::params![asset_id],
                |row| row.get::<_, Option<String>>(0),
            )
            .optional()?
            .flatten())
    }

    /// The stack `asset_id` belongs to, typed not-found when it is in none.
    fn require_stack(&self, asset_id: &str) -> Result<(String, Vec<String>)> {
        self.stack_for_asset(asset_id)?.ok_or_else(|| {
            MakiError::not_found_with_hint("stack", asset_id, "asset is not in a stack").into()
        })
    }

    /// Create a new stack from the given asset IDs.
    /// The first asset becomes the pick (position 0).
    pub fn create(&self, asset_ids: &[String]) -> Result<Stack> {
        if asset_ids.len() < 2 {
            return Err(MakiError::InvalidInput("a stack requires at least 2 assets".into()).into());
        }

        self.mutate(|| {
            // Check none are already stacked
            for id in asset_ids {
                if let Some(sid) = self.current_stack_id(id)? {
                    return Err(MakiError::Conflict(format!("asset {id} is already in stack {sid}")).into());
                }
            }

            let id = Uuid::new_v4();
            let now = Utc::now();
            let member_count = asset_ids.len() as i64;

            self.conn.execute(
                "INSERT INTO stacks (id, created_at, member_count) VALUES (?1, ?2, ?3)",
                rusqlite::params![id.to_string(), now.to_rfc3339(), member_count],
            )?;

            for (pos, asset_id) in asset_ids.iter().enumerate() {
                let changed = self.conn.execute(
                    "UPDATE assets SET stack_id = ?1, stack_position = ?2 WHERE id = ?3",
                    rusqlite::params![id.to_string(), pos as i64, asset_id],
                )?;
                if changed == 0 {
                    return Err(MakiError::not_found("asset", asset_id).into());
                }
            }

            Ok(Stack {
                id,
                created_at: now,
                asset_ids: asset_ids.to_vec(),
            })
        })
    }

    /// Add assets to an existing stack (identified by any current member).
    /// Returns the number of assets actually added.
    pub fn add(&self, reference_asset_id: &str, new_asset_ids: &[String]) -> Result<u32> {
        self.mutate(|| {
            let (stack_id, mut members) = self.require_stack(reference_asset_id)?;

            let mut added = 0u32;
            for id in new_asset_ids {
                // Check not already stacked
                if self.current_stack_id(id)?.is_some() {
                    eprintln!("Warning: asset {id} is already in a stack, skipping");
                    continue;
                }

                let pos = members.len() as i64;
                let changed = self.conn.execute(
                    "UPDATE assets SET stack_id = ?1, stack_position = ?2 WHERE id = ?3",
                    rusqlite::params![stack_id, pos, id],
                )?;
                if changed == 0 {
                    return Err(MakiError::not_found("asset", id).into());
                }
                members.push(id.clone());
                added += 1;
            }

            if added > 0 {
                self.conn.execute(
                    "UPDATE stacks SET member_count = ?1 WHERE id = ?2",
                    rusqlite::params![members.len() as i64, stack_id],
                )?;
            }

            Ok(added)
        })
    }

    /// Remove assets from their stack. Dissolves the stack if ≤1 member remains.
    /// Returns the number of assets actually removed. Assets that are not
    /// stacked (or unknown) are skipped, and when nothing changes the YAML
    /// is not rewritten — so batch purges can call this per asset cheaply.
    pub fn remove(&self, asset_ids: &[String]) -> Result<u32> {
        self.with_tx(|| {
            let mut removed = 0u32;
            // Group by stack_id to handle dissolves
            let mut affected_stacks: std::collections::HashSet<String> = std::collections::HashSet::new();

            for id in asset_ids {
                if let Some(sid) = self.current_stack_id(id)? {
                    self.conn.execute(
                        "UPDATE assets SET stack_id = NULL, stack_position = NULL WHERE id = ?1",
                        rusqlite::params![id],
                    )?;
                    affected_stacks.insert(sid);
                    removed += 1;
                }
            }

            // For each affected stack, renumber positions and possibly dissolve
            for sid in &affected_stacks {
                self.renumber_and_maybe_dissolve(sid)?;
            }

            if removed > 0 {
                self.persist()?;
            }
            Ok(removed)
        })
    }

    /// Set the pick of a stack (move this asset to position 0).
    pub fn set_pick(&self, asset_id: &str) -> Result<()> {
        let (stack_id, members) = self.require_stack(asset_id)?;

        let current_pos = members.iter().position(|id| id == asset_id)
            .ok_or_else(|| anyhow!("asset {asset_id} not found in stack members"))?;

        if current_pos == 0 {
            return Ok(()); // Already the pick
        }

        self.mutate(|| {
            // Swap positions: current pick (pos 0) gets this asset's position
            let current_pick = &members[0];
            self.conn.execute(
                "UPDATE assets SET stack_position = ?1 WHERE id = ?2 AND stack_id = ?3",
                rusqlite::params![current_pos as i64, current_pick, stack_id],
            )?;
            self.conn.execute(
                "UPDATE assets SET stack_position = 0 WHERE id = ?1 AND stack_id = ?2",
                rusqlite::params![asset_id, stack_id],
            )?;
            Ok(())
        })
    }

    /// Dissolve an entire stack (unstack all members).
    pub fn dissolve(&self, asset_id: &str) -> Result<()> {
        self.mutate(|| {
            let (stack_id, _) = self.require_stack(asset_id)?;

            self.conn.execute(
                "UPDATE assets SET stack_id = NULL, stack_position = NULL WHERE stack_id = ?1",
                rusqlite::params![stack_id],
            )?;
            self.conn.execute(
                "DELETE FROM stacks WHERE id = ?1",
                rusqlite::params![stack_id],
            )?;
            Ok(())
        })
    }

    /// List all stacks with summary info.
    pub fn list(&self) -> Result<Vec<StackSummary>> {
        let mut stmt = self.conn.prepare(
            "SELECT s.id, s.member_count, s.created_at,
                    (SELECT a.id FROM assets a WHERE a.stack_id = s.id AND a.stack_position = 0 LIMIT 1)
             FROM stacks s ORDER BY s.created_at DESC",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(StackSummary {
                id: row.get(0)?,
                member_count: row.get::<_, i64>(1)? as u64,
                created_at: row.get(2)?,
                pick_asset_id: row.get(3)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Get the stack and ordered members for an asset. Returns None if not stacked.
    pub fn stack_for_asset(&self, asset_id: &str) -> Result<Option<(String, Vec<String>)>> {
        match self.current_stack_id(asset_id)? {
            Some(sid) => {
                let members = self.ordered_members(&sid)?;
                Ok(Some((sid, members)))
            }
            None => Ok(None),
        }
    }

    /// Get ordered member asset IDs for a stack.
    pub fn ordered_members(&self, stack_id: &str) -> Result<Vec<String>> {
        let mut stmt = self.conn.prepare(
            "SELECT id FROM assets WHERE stack_id = ?1 ORDER BY stack_position",
        )?;
        let rows = stmt.query_map(rusqlite::params![stack_id], |row| {
            row.get::<_, String>(0)
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Renumber positions after removal. Dissolves if ≤1 member remains.
    fn renumber_and_maybe_dissolve(&self, stack_id: &str) -> Result<()> {
        let members = self.ordered_members(stack_id)?;

        if members.len() <= 1 {
            // Dissolve
            self.conn.execute(
                "UPDATE assets SET stack_id = NULL, stack_position = NULL WHERE stack_id = ?1",
                rusqlite::params![stack_id],
            )?;
            self.conn.execute(
                "DELETE FROM stacks WHERE id = ?1",
                rusqlite::params![stack_id],
            )?;
            return Ok(());
        }

        // Renumber
        for (pos, id) in members.iter().enumerate() {
            self.conn.execute(
                "UPDATE assets SET stack_position = ?1 WHERE id = ?2 AND stack_id = ?3",
                rusqlite::params![pos as i64, id, stack_id],
            )?;
        }
        self.conn.execute(
            "UPDATE stacks SET member_count = ?1 WHERE id = ?2",
            rusqlite::params![members.len() as i64, stack_id],
        )?;

        Ok(())
    }

    /// Export all stacks to a StacksFile for YAML persistence.
    pub fn export_all(&self) -> Result<StacksFile> {
        let mut stmt = self.conn.prepare(
            "SELECT id, created_at FROM stacks ORDER BY created_at",
        )?;
        let mut stacks = Vec::new();
        let mut rows = stmt.query([])?;
        while let Some(row) = rows.next()? {
            let id_str: String = row.get(0)?;
            let id: Uuid = id_str.parse().map_err(|e| anyhow!("invalid UUID: {e}"))?;
            let created_at_str: String = row.get(1)?;
            let created_at: DateTime<Utc> = created_at_str.parse().map_err(|e| anyhow!("invalid date: {e}"))?;
            let asset_ids = self.ordered_members(&id_str)?;
            stacks.push(Stack {
                id,
                created_at,
                asset_ids,
            });
        }
        Ok(StacksFile { stacks })
    }

    /// Import stacks from YAML into SQLite (used by rebuild-catalog).
    /// SQLite-only: the YAML is the input here. A member whose asset row
    /// does not exist (sidecar gone) is simply not linked; a failed
    /// statement is an error (formerly swallowed, so a rebuild could lose
    /// stack membership silently).
    pub fn import_from_yaml(&self, file: &StacksFile) -> Result<u32> {
        self.with_tx(|| {
            let mut count = 0u32;
            for stack in &file.stacks {
                let stack_id = stack.id.to_string();
                self.conn.execute(
                    "INSERT OR REPLACE INTO stacks (id, created_at, member_count) VALUES (?1, ?2, ?3)",
                    rusqlite::params![stack_id, stack.created_at.to_rfc3339(), stack.asset_ids.len() as i64],
                )?;
                for (pos, asset_id) in stack.asset_ids.iter().enumerate() {
                    self.conn.execute(
                        "UPDATE assets SET stack_id = ?1, stack_position = ?2 WHERE id = ?3",
                        rusqlite::params![stack_id, pos as i64, asset_id],
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

    fn setup_db() -> (rusqlite::Connection, tempfile::TempDir) {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS assets (
                id TEXT PRIMARY KEY,
                name TEXT,
                created_at TEXT NOT NULL,
                asset_type TEXT NOT NULL,
                tags TEXT NOT NULL DEFAULT '[]',
                description TEXT,
                stack_id TEXT,
                stack_position INTEGER
            );
            CREATE TABLE IF NOT EXISTS stacks (
                id TEXT PRIMARY KEY,
                created_at TEXT NOT NULL,
                member_count INTEGER NOT NULL DEFAULT 0
            );",
        )
        .unwrap();
        (conn, tempfile::tempdir().unwrap())
    }

    fn insert_test_asset(conn: &rusqlite::Connection, id: &str) {
        conn.execute(
            "INSERT INTO assets (id, created_at, asset_type) VALUES (?1, '2026-01-01T00:00:00Z', 'image')",
            rusqlite::params![id],
        )
        .unwrap();
    }

    #[test]
    fn create_stack() {
        let (conn, dir) = setup_db();
        insert_test_asset(&conn, "a1");
        insert_test_asset(&conn, "a2");
        insert_test_asset(&conn, "a3");

        let store = StackStore::open(&conn, dir.path());
        let stack = store.create(&["a1".into(), "a2".into(), "a3".into()]).unwrap();

        assert_eq!(stack.asset_ids, vec!["a1", "a2", "a3"]);

        let list = store.list().unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].member_count, 3);
        assert_eq!(list[0].pick_asset_id.as_deref(), Some("a1"));

        // The YAML side was written by the mutator itself.
        let yaml = load_yaml(dir.path()).unwrap();
        assert_eq!(yaml.stacks.len(), 1);
        assert_eq!(yaml.stacks[0].asset_ids, vec!["a1", "a2", "a3"]);
    }

    #[test]
    fn create_stack_too_few() {
        let (conn, dir) = setup_db();
        insert_test_asset(&conn, "a1");

        let store = StackStore::open(&conn, dir.path());
        let err = store.create(&["a1".into()]).unwrap_err();
        assert!(matches!(crate::error::classify(&err), Some(MakiError::InvalidInput(_))));
    }

    #[test]
    fn create_stack_already_stacked() {
        let (conn, dir) = setup_db();
        insert_test_asset(&conn, "a1");
        insert_test_asset(&conn, "a2");
        insert_test_asset(&conn, "a3");

        let store = StackStore::open(&conn, dir.path());
        store.create(&["a1".into(), "a2".into()]).unwrap();

        let err = store.create(&["a2".into(), "a3".into()]).unwrap_err();
        assert!(err.to_string().contains("already in stack"));
        assert!(matches!(crate::error::classify(&err), Some(MakiError::Conflict(_))));
        // Rolled back: a3 is still unstacked and only one stack exists.
        assert!(store.stack_for_asset("a3").unwrap().is_none());
        assert_eq!(store.list().unwrap().len(), 1);
    }

    #[test]
    fn create_stack_unknown_asset_is_not_found_and_rolled_back() {
        let (conn, dir) = setup_db();
        insert_test_asset(&conn, "a1");

        let store = StackStore::open(&conn, dir.path());
        let err = store.create(&["a1".into(), "ghost".into()]).unwrap_err();
        assert!(matches!(crate::error::classify(&err), Some(MakiError::NotFound { kind: "asset", .. })));
        assert!(store.list().unwrap().is_empty());
        assert!(store.stack_for_asset("a1").unwrap().is_none());
    }

    #[test]
    fn mutator_on_read_only_handle_fails_without_touching_rows() {
        let (conn, _dir) = setup_db();
        insert_test_asset(&conn, "a1");
        insert_test_asset(&conn, "a2");

        let store = StackStore::new(&conn);
        let err = store.create(&["a1".into(), "a2".into()]).unwrap_err();
        assert!(format!("{err:#}").contains("StackStore::open"), "{err:#}");
        assert!(store.list().unwrap().is_empty());
        assert!(store.stack_for_asset("a1").unwrap().is_none());
    }

    #[test]
    fn failed_yaml_write_rolls_the_rows_back() {
        let (conn, dir) = setup_db();
        insert_test_asset(&conn, "a1");
        insert_test_asset(&conn, "a2");
        insert_test_asset(&conn, "a3");

        // A good root first, so there is a stack to mutate.
        let good = StackStore::open(&conn, dir.path());
        good.create(&["a1".into(), "a2".into()]).unwrap();

        // A "catalog root" that is a file: the atomic YAML write cannot
        // create the temp file next to it.
        let bogus_root = dir.path().join("not-a-dir");
        std::fs::write(&bogus_root, "x").unwrap();
        let broken = StackStore::open(&conn, &bogus_root);

        assert!(broken.add("a1", &["a3".into()]).is_err());
        assert!(broken.dissolve("a1").is_err());
        assert!(broken.set_pick("a2").is_err());
        assert!(broken.remove(&["a2".into()]).is_err());

        // SQLite is exactly as the good store left it.
        let (_, members) = good.stack_for_asset("a1").unwrap().unwrap();
        assert_eq!(members, vec!["a1", "a2"]);
        assert!(good.stack_for_asset("a3").unwrap().is_none());
        assert_eq!(good.list().unwrap().len(), 1);
        assert_eq!(good.list().unwrap()[0].member_count, 2);
        // And the YAML still describes that state.
        let yaml = load_yaml(dir.path()).unwrap();
        assert_eq!(yaml.stacks.len(), 1);
        assert_eq!(yaml.stacks[0].asset_ids, vec!["a1", "a2"]);
    }

    #[test]
    fn set_pick() {
        let (conn, dir) = setup_db();
        insert_test_asset(&conn, "a1");
        insert_test_asset(&conn, "a2");
        insert_test_asset(&conn, "a3");

        let store = StackStore::open(&conn, dir.path());
        store.create(&["a1".into(), "a2".into(), "a3".into()]).unwrap();

        store.set_pick("a2").unwrap();

        let (_, members) = store.stack_for_asset("a2").unwrap().unwrap();
        assert_eq!(members[0], "a2");
        assert_eq!(members[1], "a1");
        assert_eq!(members[2], "a3");
        let yaml = load_yaml(dir.path()).unwrap();
        assert_eq!(yaml.stacks[0].asset_ids[0], "a2");
    }

    #[test]
    fn set_pick_already_pick_is_noop() {
        let (conn, dir) = setup_db();
        insert_test_asset(&conn, "a1");
        insert_test_asset(&conn, "a2");

        let store = StackStore::open(&conn, dir.path());
        store.create(&["a1".into(), "a2".into()]).unwrap();

        store.set_pick("a1").unwrap(); // already pick
        let (_, members) = store.stack_for_asset("a1").unwrap().unwrap();
        assert_eq!(members[0], "a1");
    }

    #[test]
    fn remove_from_stack() {
        let (conn, dir) = setup_db();
        insert_test_asset(&conn, "a1");
        insert_test_asset(&conn, "a2");
        insert_test_asset(&conn, "a3");

        let store = StackStore::open(&conn, dir.path());
        store.create(&["a1".into(), "a2".into(), "a3".into()]).unwrap();

        let removed = store.remove(&["a2".into()]).unwrap();
        assert_eq!(removed, 1);

        let (_, members) = store.stack_for_asset("a1").unwrap().unwrap();
        assert_eq!(members.len(), 2);
        assert_eq!(members, vec!["a1", "a3"]);

        // a2 should not be in a stack
        assert!(store.stack_for_asset("a2").unwrap().is_none());
        let yaml = load_yaml(dir.path()).unwrap();
        assert_eq!(yaml.stacks[0].asset_ids, vec!["a1", "a3"]);
    }

    #[test]
    fn remove_nothing_stacked_is_a_cheap_noop() {
        let (conn, dir) = setup_db();
        insert_test_asset(&conn, "a1");
        // No root needed when nothing changes (batch purges rely on this).
        let store = StackStore::new(&conn);
        assert_eq!(store.remove(&["a1".into(), "ghost".into()]).unwrap(), 0);
        assert!(!dir.path().join(FILENAME).exists());
    }

    #[test]
    fn remove_dissolves_when_single() {
        let (conn, dir) = setup_db();
        insert_test_asset(&conn, "a1");
        insert_test_asset(&conn, "a2");

        let store = StackStore::open(&conn, dir.path());
        store.create(&["a1".into(), "a2".into()]).unwrap();

        store.remove(&["a1".into()]).unwrap();

        // Stack should be dissolved — a2 not in a stack anymore
        assert!(store.stack_for_asset("a2").unwrap().is_none());
        assert!(store.list().unwrap().is_empty());
        assert!(load_yaml(dir.path()).unwrap().stacks.is_empty());
    }

    #[test]
    fn dissolve_stack() {
        let (conn, dir) = setup_db();
        insert_test_asset(&conn, "a1");
        insert_test_asset(&conn, "a2");
        insert_test_asset(&conn, "a3");

        let store = StackStore::open(&conn, dir.path());
        store.create(&["a1".into(), "a2".into(), "a3".into()]).unwrap();

        store.dissolve("a2").unwrap();

        assert!(store.stack_for_asset("a1").unwrap().is_none());
        assert!(store.stack_for_asset("a2").unwrap().is_none());
        assert!(store.stack_for_asset("a3").unwrap().is_none());
        assert!(store.list().unwrap().is_empty());
    }

    #[test]
    fn add_to_stack() {
        let (conn, dir) = setup_db();
        insert_test_asset(&conn, "a1");
        insert_test_asset(&conn, "a2");
        insert_test_asset(&conn, "a3");

        let store = StackStore::open(&conn, dir.path());
        store.create(&["a1".into(), "a2".into()]).unwrap();

        let added = store.add("a1", &["a3".into()]).unwrap();
        assert_eq!(added, 1);

        let (_, members) = store.stack_for_asset("a1").unwrap().unwrap();
        assert_eq!(members.len(), 3);
        assert_eq!(members, vec!["a1", "a2", "a3"]);

        let list = store.list().unwrap();
        assert_eq!(list[0].member_count, 3);
    }

    #[test]
    fn export_and_import() {
        let (conn, dir) = setup_db();
        insert_test_asset(&conn, "a1");
        insert_test_asset(&conn, "a2");

        let store = StackStore::open(&conn, dir.path());
        store.create(&["a1".into(), "a2".into()]).unwrap();

        let exported = store.export_all().unwrap();
        assert_eq!(exported.stacks.len(), 1);
        assert_eq!(exported.stacks[0].asset_ids, vec!["a1", "a2"]);

        // Wipe and reimport through a root-less handle (rebuild's shape).
        conn.execute_batch("UPDATE assets SET stack_id = NULL, stack_position = NULL; DELETE FROM stacks;").unwrap();
        assert!(store.list().unwrap().is_empty());

        let importer = StackStore::new(&conn);
        let imported = importer.import_from_yaml(&exported).unwrap();
        assert_eq!(imported, 1);

        let list = store.list().unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].member_count, 2);

        let (_, members) = store.stack_for_asset("a1").unwrap().unwrap();
        assert_eq!(members, vec!["a1", "a2"]);
    }

    #[test]
    fn yaml_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let file = StacksFile {
            stacks: vec![Stack {
                id: Uuid::new_v4(),
                created_at: Utc::now(),
                asset_ids: vec!["abc".to_string(), "def".to_string()],
            }],
        };
        save_yaml(dir.path(), &file).unwrap();
        let loaded = load_yaml(dir.path()).unwrap();
        assert_eq!(loaded.stacks.len(), 1);
        assert_eq!(loaded.stacks[0].asset_ids.len(), 2);
    }

    #[test]
    fn corrupt_yaml_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(FILENAME), "stacks: [unclosed").unwrap();
        assert!(load_yaml(dir.path()).is_err());
    }

    #[test]
    fn dissolve_not_stacked_errors() {
        let (conn, dir) = setup_db();
        insert_test_asset(&conn, "a1");

        let store = StackStore::open(&conn, dir.path());
        let err = store.dissolve("a1").unwrap_err();
        assert!(matches!(crate::error::classify(&err), Some(MakiError::NotFound { kind: "stack", .. })));
        assert!(err.to_string().contains("not in a stack"), "{err}");
    }

    #[test]
    fn stack_for_asset_not_stacked() {
        let (conn, _dir) = setup_db();
        insert_test_asset(&conn, "a1");

        let store = StackStore::new(&conn);
        assert!(store.stack_for_asset("a1").unwrap().is_none());
    }
}
