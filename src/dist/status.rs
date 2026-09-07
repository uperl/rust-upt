//! `dist_status`: which build phases have completed for each unpacked
//! distribution, kept in the user database ([`crate::db`]).
//!
//! One row per distribution directory, keyed by its absolute, symlink-resolved
//! path (`UNIQUE`). Each build phase that produces something — `pre-configure`,
//! `configure`, `build`, `test`, `install` — has a flag that starts false and
//! is set once the phase finishes successfully. `clean` and `distclean` have no
//! flag of their own: a successful `clean` clears `build` / `test` / `install`,
//! and a successful `distclean` clears everything.
//!
//! On [`Status::open`] the table is reconciled with the filesystem: rows whose
//! directory no longer exists are deleted, and a directory that has lost its
//! generated build script (`Makefile` / `Build`) is treated as having been
//! `distclean`ed.

use std::path::Path;

use anyhow::{Context, Result};
use cpan_distribution_build::{BuildTool, Distribution};
use rusqlite::{Connection, params};

/// Migrations for the `dist` component, applied through [`crate::db::migrate`].
/// Append-only: never edit or remove a released entry.
const MIGRATIONS: &[&str] = &["CREATE TABLE dist_status (
        directory     TEXT    NOT NULL UNIQUE,
        build_tool    TEXT    NOT NULL,
        pre_configure INTEGER NOT NULL DEFAULT 0,
        configure     INTEGER NOT NULL DEFAULT 0,
        build         INTEGER NOT NULL DEFAULT 0,
        test          INTEGER NOT NULL DEFAULT 0,
        install       INTEGER NOT NULL DEFAULT 0
    );"];

/// A build phase that carries a completion flag in `dist_status`, in pipeline
/// order.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    PreConfigure,
    Configure,
    Build,
    Test,
    Install,
}

impl Phase {
    /// Position in the pipeline, and the index of this phase's flag in the
    /// array returned by [`Status::flags`].
    pub fn index(self) -> usize {
        match self {
            Phase::PreConfigure => 0,
            Phase::Configure => 1,
            Phase::Build => 2,
            Phase::Test => 3,
            Phase::Install => 4,
        }
    }
}

/// A handle to the `dist_status` row for one distribution directory.
pub struct Status {
    conn: Connection,
    directory: String,
}

impl Status {
    /// Open the user database, migrate the `dist` tables, drop rows for
    /// directories that no longer exist, and ensure there is a row for `dist`
    /// recording its build tool. If `dist`'s generated build script is gone,
    /// treat that as a completed `distclean` and clear every flag.
    pub fn open(cx: &crate::Cx, dist: &Distribution) -> Result<Status> {
        let mut conn = cx.open_db()?;
        crate::db::migrate(&mut conn, "dist", MIGRATIONS)?;

        purge_vanished(&conn).context("pruning dist_status rows for removed directories")?;

        let directory = canonical(&dist.root);
        conn.execute(
            "INSERT INTO dist_status (directory, build_tool) VALUES (?1, ?2)
             ON CONFLICT(directory) DO UPDATE SET build_tool = excluded.build_tool",
            params![directory, build_tool_name(dist.build_tool)],
        )
        .context("recording the distribution in dist_status")?;

        let status = Status { conn, directory };
        if !script_present(&dist.root, dist.build_tool) {
            status
                .clear_all()
                .context("clearing dist_status after an assumed distclean")?;
        }
        Ok(status)
    }

    /// The completion flags for this directory, indexed by [`Phase::index`]:
    /// `[pre_configure, configure, build, test, install]`.
    pub fn flags(&self) -> Result<[bool; 5]> {
        let flags = self.conn.query_row(
            "SELECT pre_configure, configure, build, test, install
               FROM dist_status WHERE directory = ?1",
            params![self.directory],
            |row| {
                Ok([
                    row.get::<_, i64>(0)? != 0,
                    row.get::<_, i64>(1)? != 0,
                    row.get::<_, i64>(2)? != 0,
                    row.get::<_, i64>(3)? != 0,
                    row.get::<_, i64>(4)? != 0,
                ])
            },
        )?;
        Ok(flags)
    }

    /// Mark `phase` complete for this directory.
    pub fn mark(&self, phase: Phase) -> Result<()> {
        let sql = match phase {
            Phase::PreConfigure => "UPDATE dist_status SET pre_configure = 1 WHERE directory = ?1",
            Phase::Configure => "UPDATE dist_status SET configure = 1 WHERE directory = ?1",
            Phase::Build => "UPDATE dist_status SET build = 1 WHERE directory = ?1",
            Phase::Test => "UPDATE dist_status SET test = 1 WHERE directory = ?1",
            Phase::Install => "UPDATE dist_status SET install = 1 WHERE directory = ?1",
        };
        self.conn.execute(sql, params![self.directory])?;
        Ok(())
    }

    /// A successful `clean`: clear every flag except `pre_configure` and
    /// `configure`.
    pub fn cleared_build(&self) -> Result<()> {
        self.conn.execute(
            "UPDATE dist_status SET build = 0, test = 0, install = 0 WHERE directory = ?1",
            params![self.directory],
        )?;
        Ok(())
    }

    /// A successful `distclean` (or a directory whose generated build script
    /// has gone missing): clear every flag.
    pub fn clear_all(&self) -> Result<()> {
        self.conn.execute(
            "UPDATE dist_status
                SET pre_configure = 0, configure = 0, build = 0, test = 0, install = 0
              WHERE directory = ?1",
            params![self.directory],
        )?;
        Ok(())
    }
}

/// Delete rows whose `directory` no longer exists on disk.
fn purge_vanished(conn: &Connection) -> Result<()> {
    let directories: Vec<String> = {
        let mut stmt = conn.prepare("SELECT directory FROM dist_status")?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
        rows.collect::<rusqlite::Result<_>>()?
    };
    for directory in directories {
        if !Path::new(&directory).exists() {
            conn.execute(
                "DELETE FROM dist_status WHERE directory = ?1",
                params![directory],
            )?;
        }
    }
    Ok(())
}

/// Whether the build script the configure step generates is still present.
///
/// For EUMM, `make clean` renames `Makefile` to `Makefile.old`, so either name
/// counts as present; only `make distclean` removes both. For MB the `Build`
/// script survives `./Build clean` and is removed only by `./Build realclean`
/// (a.k.a. `distclean`).
fn script_present(root: &Path, tool: BuildTool) -> bool {
    match tool {
        BuildTool::Eumm => root.join("Makefile").exists() || root.join("Makefile.old").exists(),
        BuildTool::ModuleBuild => root.join("Build").exists(),
    }
}

fn build_tool_name(tool: BuildTool) -> &'static str {
    match tool {
        BuildTool::Eumm => "EUMM",
        BuildTool::ModuleBuild => "MB",
    }
}

/// The directory as an absolute, symlink-resolved path string for use as the
/// `dist_status` key, falling back to the path as given if it cannot be
/// canonicalized.
fn canonical(root: &Path) -> String {
    std::fs::canonicalize(root)
        .unwrap_or_else(|_| root.to_path_buf())
        .to_string_lossy()
        .into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `(pre_configure, configure, build, test, install)` for a directory.
    fn flags(conn: &Connection, directory: &str) -> (i64, i64, i64, i64, i64) {
        conn.query_row(
            "SELECT pre_configure, configure, build, test, install
               FROM dist_status WHERE directory = ?1",
            [directory],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            },
        )
        .unwrap()
    }

    fn status_with_row(directory: &str) -> Status {
        let mut conn = Connection::open_in_memory().unwrap();
        crate::db::migrate(&mut conn, "dist", MIGRATIONS).unwrap();
        conn.execute(
            "INSERT INTO dist_status (directory, build_tool) VALUES (?1, 'EUMM')",
            [directory],
        )
        .unwrap();
        Status {
            conn,
            directory: directory.to_string(),
        }
    }

    #[test]
    fn flags_start_false() {
        let s = status_with_row("/d");
        assert_eq!(flags(&s.conn, "/d"), (0, 0, 0, 0, 0));
    }

    #[test]
    fn mark_sets_only_its_own_phase_and_is_idempotent() {
        let s = status_with_row("/d");
        s.mark(Phase::Build).unwrap();
        s.mark(Phase::Build).unwrap();
        assert_eq!(flags(&s.conn, "/d"), (0, 0, 1, 0, 0));
        s.mark(Phase::Test).unwrap();
        assert_eq!(flags(&s.conn, "/d"), (0, 0, 1, 1, 0));
    }

    #[test]
    fn clean_keeps_configure_flags() {
        let s = status_with_row("/d");
        for phase in [
            Phase::PreConfigure,
            Phase::Configure,
            Phase::Build,
            Phase::Test,
            Phase::Install,
        ] {
            s.mark(phase).unwrap();
        }
        s.cleared_build().unwrap();
        assert_eq!(flags(&s.conn, "/d"), (1, 1, 0, 0, 0));
    }

    #[test]
    fn distclean_clears_everything() {
        let s = status_with_row("/d");
        for phase in [
            Phase::PreConfigure,
            Phase::Configure,
            Phase::Build,
            Phase::Test,
            Phase::Install,
        ] {
            s.mark(phase).unwrap();
        }
        s.clear_all().unwrap();
        assert_eq!(flags(&s.conn, "/d"), (0, 0, 0, 0, 0));
    }

    #[test]
    fn directory_is_unique_and_build_tool_upserts() {
        let mut conn = Connection::open_in_memory().unwrap();
        crate::db::migrate(&mut conn, "dist", MIGRATIONS).unwrap();
        let upsert = "INSERT INTO dist_status (directory, build_tool) VALUES (?1, ?2)
                      ON CONFLICT(directory) DO UPDATE SET build_tool = excluded.build_tool";
        conn.execute(upsert, params!["/d", "EUMM"]).unwrap();
        conn.execute(upsert, params!["/d", "MB"]).unwrap();

        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM dist_status", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 1);
        let tool: String = conn
            .query_row(
                "SELECT build_tool FROM dist_status WHERE directory = '/d'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(tool, "MB");
    }

    #[test]
    fn purge_vanished_drops_only_missing_directories() {
        let mut conn = Connection::open_in_memory().unwrap();
        crate::db::migrate(&mut conn, "dist", MIGRATIONS).unwrap();

        let present = std::env::current_dir().unwrap();
        let present = present.to_str().unwrap();
        conn.execute(
            "INSERT INTO dist_status (directory, build_tool) VALUES (?1, 'EUMM')",
            [present],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO dist_status (directory, build_tool)
             VALUES ('/no/such/directory/upt-dist-test', 'MB')",
            [],
        )
        .unwrap();

        purge_vanished(&conn).unwrap();

        let remaining: Vec<String> = conn
            .prepare("SELECT directory FROM dist_status")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(remaining, vec![present.to_string()]);
    }

    #[test]
    fn script_present_tracks_the_build_tool() {
        let dir = std::env::temp_dir().join(format!("upt-dist-status-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        assert!(!script_present(&dir, BuildTool::Eumm));
        assert!(!script_present(&dir, BuildTool::ModuleBuild));

        // `make clean` leaves Makefile.old behind — still "configured".
        std::fs::write(dir.join("Makefile.old"), "").unwrap();
        assert!(script_present(&dir, BuildTool::Eumm));
        assert!(!script_present(&dir, BuildTool::ModuleBuild));

        std::fs::write(dir.join("Build"), "").unwrap();
        assert!(script_present(&dir, BuildTool::ModuleBuild));

        std::fs::remove_dir_all(&dir).ok();
    }
}
