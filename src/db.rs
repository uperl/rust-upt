//! The user-specific SQLite database.
//!
//! A single file under the platform data directory
//! ([`crate::paths::data_file`]), created lazily the first time a subcommand
//! calls [`crate::Cx::open_db`]. There is no central schema: each subcommand
//! owns its own tables and registers an ordered list of migrations via
//! [`migrate`], tracked per-component in the shared `upt_migrations` table.
//!
//! ```ignore
//! let mut conn = cx.open_db()?;
//! db::migrate(&mut conn, "notes", &[
//!     "CREATE TABLE notes (id INTEGER PRIMARY KEY, body TEXT NOT NULL);",
//!     "ALTER TABLE notes ADD COLUMN created_at TEXT;",
//! ])?;
//! ```

use std::path::Path;

use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension};

/// Open the database at `path`, creating the file and any missing parent
/// directories. Applies the connection-wide pragmas every `upt` connection
/// uses (WAL journalling, enforced foreign keys, a short busy timeout).
pub fn open(path: &Path) -> Result<Connection> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }

    let conn =
        Connection::open(path).with_context(|| format!("opening database {}", path.display()))?;
    conn.execute_batch(
        "PRAGMA journal_mode = WAL;
         PRAGMA foreign_keys = ON;
         PRAGMA busy_timeout = 5000;",
    )
    .context("configuring the database connection")?;
    Ok(conn)
}

/// Bring `component`'s tables up to date by running any of `migrations` that
/// have not run yet, in order, inside a single transaction.
///
/// `migrations[i]` is migration version `i + 1`; each SQL script is executed
/// exactly once over the life of the database, and `component`'s recorded
/// version is advanced to `migrations.len()`. Calling this on every run is
/// cheap and safe — it is a no-op once the component is current. Migrations
/// must only ever be appended: changing or removing an already-released entry
/// will corrupt the schema of existing databases.
// The subcommand-facing half of the storage API; exercised by the tests, and
// by subcommands as they take on persistent state.
#[allow(dead_code)]
pub fn migrate(conn: &mut Connection, component: &str, migrations: &[&str]) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS upt_migrations (
             component TEXT NOT NULL PRIMARY KEY,
             version   INTEGER NOT NULL
         );",
    )
    .context("creating the upt_migrations table")?;

    let current: i64 = conn
        .query_row(
            "SELECT version FROM upt_migrations WHERE component = ?1",
            [component],
            |row| row.get(0),
        )
        .optional()
        .context("reading migration state")?
        .unwrap_or(0);

    let target = i64::try_from(migrations.len()).expect("migration count fits in i64");
    if current >= target {
        return Ok(());
    }

    let tx = conn
        .transaction()
        .context("starting migration transaction")?;
    for (index, sql) in migrations
        .iter()
        .enumerate()
        .skip(usize::try_from(current).unwrap_or(0))
    {
        let version = index + 1;
        tx.execute_batch(sql)
            .with_context(|| format!("applying {component} migration {version}"))?;
    }
    tx.execute(
        "INSERT INTO upt_migrations (component, version) VALUES (?1, ?2)
         ON CONFLICT(component) DO UPDATE SET version = excluded.version",
        rusqlite::params![component, target],
    )
    .context("recording migration state")?;
    tx.commit().context("committing migrations")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn component_version(conn: &Connection, component: &str) -> Option<i64> {
        conn.query_row(
            "SELECT version FROM upt_migrations WHERE component = ?1",
            [component],
            |row| row.get(0),
        )
        .optional()
        .unwrap()
    }

    #[test]
    fn applies_in_order_and_is_idempotent() {
        let mut conn = Connection::open_in_memory().unwrap();
        let m = [
            "CREATE TABLE a (x INTEGER);",
            "ALTER TABLE a ADD COLUMN y INTEGER;",
        ];

        migrate(&mut conn, "demo", &m).unwrap();
        migrate(&mut conn, "demo", &m).unwrap(); // second call does nothing

        assert_eq!(component_version(&conn, "demo"), Some(2));
        conn.execute("INSERT INTO a (x, y) VALUES (1, 2)", [])
            .unwrap();
    }

    #[test]
    fn appending_a_migration_runs_only_the_new_one() {
        let mut conn = Connection::open_in_memory().unwrap();
        migrate(&mut conn, "demo", &["CREATE TABLE a (x INTEGER);"]).unwrap();

        migrate(
            &mut conn,
            "demo",
            &[
                "CREATE TABLE a (x INTEGER);", // already applied, not re-run
                "ALTER TABLE a ADD COLUMN z INTEGER;",
            ],
        )
        .unwrap();

        assert_eq!(component_version(&conn, "demo"), Some(2));
        conn.execute("INSERT INTO a (x, z) VALUES (1, 3)", [])
            .unwrap();
    }

    #[test]
    fn open_creates_the_file_and_parent_dirs() {
        let base = std::env::temp_dir().join(format!("upt-db-test-{}", std::process::id()));
        let path = base.join("nested").join("upt.sqlite");
        let _ = std::fs::remove_dir_all(&base);

        let conn = open(&path).unwrap();
        conn.execute_batch("CREATE TABLE t (x);").unwrap();
        drop(conn);

        assert!(path.is_file());
        // Reopening an existing database keeps the data.
        let conn = open(&path).unwrap();
        conn.execute("INSERT INTO t (x) VALUES (1)", []).unwrap();

        drop(conn);
        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn components_are_tracked_independently() {
        let mut conn = Connection::open_in_memory().unwrap();
        migrate(&mut conn, "one", &["CREATE TABLE one_t (x INTEGER);"]).unwrap();
        migrate(&mut conn, "two", &["CREATE TABLE two_t (x INTEGER);"]).unwrap();

        assert_eq!(component_version(&conn, "one"), Some(1));
        assert_eq!(component_version(&conn, "two"), Some(1));
    }

    #[test]
    fn a_failing_migration_rolls_back() {
        let mut conn = Connection::open_in_memory().unwrap();
        let err = migrate(
            &mut conn,
            "demo",
            &["CREATE TABLE a (x INTEGER);", "this is not valid sql;"],
        )
        .unwrap_err();
        assert!(format!("{err:#}").contains("migration 2"));

        // Neither the table nor a recorded version survived the rollback.
        assert_eq!(component_version(&conn, "demo"), None);
        let exists: bool = conn
            .query_row(
                "SELECT 1 FROM sqlite_master WHERE type='table' AND name='a'",
                [],
                |_| Ok(true),
            )
            .optional()
            .unwrap()
            .unwrap_or(false);
        assert!(!exists);
    }
}
