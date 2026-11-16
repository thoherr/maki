//! One transaction primitive for the catalog connection, shared by
//! `Catalog::with_tx` and the stores that only hold a `&Connection`
//! (`StackStore`, `CollectionStore`, `FaceStore`).
//!
//! Besides commit-on-`Ok` / rollback-on-`Err` and nesting, it owns the
//! ordering of the stores' YAML exports: a mutator asks for its export
//! with [`export_at_commit`], and the *outermost* [`run`] writes each
//! requested file once, after every row change of the whole transaction
//! and immediately before COMMIT. A store mutator that joins an enclosing
//! transaction therefore never writes YAML that a later failure of the
//! enclosing work would roll back in SQLite, and a batch of nested
//! mutators exports each file once instead of once per call.

use std::cell::RefCell;
use std::path::{Path, PathBuf};

use anyhow::Result;
use rusqlite::Connection;

/// Writes one store's YAML file(s) from the current SQLite rows.
pub(crate) type ExportFn = fn(&Connection, &Path) -> Result<()>;

struct Pending {
    conn: usize,
    key: &'static str,
    root: PathBuf,
    export: ExportFn,
}

#[derive(Default)]
struct State {
    /// Connections whose outermost transaction was opened by [`run`] on
    /// this thread and is still in flight.
    active: Vec<usize>,
    pending: Vec<Pending>,
}

thread_local! {
    // A transaction lives entirely inside one synchronous closure, so it
    // never leaves the thread that opened it.
    static STATE: RefCell<State> = RefCell::new(State::default());
}

fn conn_id(conn: &Connection) -> usize {
    conn as *const Connection as usize
}

/// Forget everything recorded for `id` and return its pending exports.
fn finish(id: usize) -> Vec<Pending> {
    STATE.with(|s| {
        let mut s = s.borrow_mut();
        s.active.retain(|c| *c != id);
        let (mine, rest) = std::mem::take(&mut s.pending).into_iter().partition(|p| p.conn == id);
        s.pending = rest;
        mine
    })
}

/// Run `f` inside one SQLite transaction: COMMIT when it returns `Ok`,
/// ROLLBACK when it returns `Err`. When the connection is already inside
/// a transaction the closure joins it. The outermost call performs the
/// YAML exports requested through [`export_at_commit`] just before
/// COMMIT; a failing export rolls the transaction back.
pub(crate) fn run<T>(conn: &Connection, f: impl FnOnce() -> Result<T>) -> Result<T> {
    if !conn.is_autocommit() {
        return f();
    }
    let id = conn_id(conn);
    // Leftovers of a closure that panicked are dropped here.
    finish(id);
    let tx = conn.unchecked_transaction()?;
    STATE.with(|s| s.borrow_mut().active.push(id));
    let out = match f() {
        Ok(out) => out,
        Err(e) => {
            finish(id);
            return Err(e);
        }
    };
    for p in finish(id) {
        (p.export)(conn, &p.root)?;
    }
    tx.commit()?;
    Ok(out)
}

/// Request the export of one store's YAML (`key` names the store, `root`
/// the catalog) for the transaction `conn` is in. Inside a [`run`]
/// transaction the export is deferred to its commit and de-duplicated;
/// otherwise (autocommit, or a transaction opened by other means) it is
/// written immediately.
pub(crate) fn export_at_commit(conn: &Connection, key: &'static str, root: &Path, export: ExportFn) -> Result<()> {
    let id = conn_id(conn);
    let deferred = STATE.with(|s| {
        let mut s = s.borrow_mut();
        if !s.active.contains(&id) {
            return false;
        }
        if !s.pending.iter().any(|p| p.conn == id && p.key == key && p.root == root) {
            s.pending.push(Pending { conn: id, key, root: root.to_path_buf(), export });
        }
        true
    });
    if deferred {
        Ok(())
    } else {
        export(conn, root)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static WRITES: AtomicUsize = AtomicUsize::new(0);

    fn counting_export(_: &Connection, root: &Path) -> Result<()> {
        WRITES.fetch_add(1, Ordering::SeqCst);
        std::fs::write(root.join("export.txt"), "x")?;
        Ok(())
    }

    #[test]
    fn exports_run_once_at_outermost_commit_and_not_on_rollback() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE t (x INTEGER)").unwrap();
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("export.txt");

        // Rolled back: nothing is written.
        let r: Result<()> = run(&conn, || {
            run(&conn, || {
                conn.execute("INSERT INTO t VALUES (1)", [])?;
                export_at_commit(&conn, "t", dir.path(), counting_export)
            })?;
            assert!(!file.exists(), "export must wait for the outer commit");
            anyhow::bail!("later step fails")
        });
        assert!(r.is_err());
        assert!(!file.exists());
        assert_eq!(WRITES.load(Ordering::SeqCst), 0);

        // Committed: two nested requests, one write, after the rows.
        run(&conn, || {
            for _ in 0..2 {
                run(&conn, || {
                    conn.execute("INSERT INTO t VALUES (1)", [])?;
                    export_at_commit(&conn, "t", dir.path(), counting_export)
                })?;
            }
            assert!(!file.exists());
            Ok(())
        })
        .unwrap();
        assert!(file.exists());
        assert_eq!(WRITES.load(Ordering::SeqCst), 1);
        let n: i64 = conn.query_row("SELECT COUNT(*) FROM t", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 2);

        // Outside any transaction the export is immediate.
        export_at_commit(&conn, "t", dir.path(), counting_export).unwrap();
        assert_eq!(WRITES.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn failing_export_rolls_the_transaction_back() {
        fn broken(_: &Connection, _: &Path) -> Result<()> {
            anyhow::bail!("disk full")
        }
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE t (x INTEGER)").unwrap();
        let r = run(&conn, || {
            conn.execute("INSERT INTO t VALUES (1)", [])?;
            export_at_commit(&conn, "t", Path::new("."), broken)
        });
        assert!(r.is_err());
        assert!(conn.is_autocommit());
        let n: i64 = conn.query_row("SELECT COUNT(*) FROM t", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 0);
    }
}
