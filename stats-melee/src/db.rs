//! Connecting to the SQLite database.
//!
//! [`open_database`] is what the GUI calls: it resolves a path, applies the
//! connection pragmas, and runs any pending migrations. Deleting rows lives
//! in [`crate::store`].

use anyhow::{anyhow, Result};
use diesel::connection::SimpleConnection;
use diesel::prelude::*;
use diesel_migrations::MigrationHarness;
use dotenvy::dotenv;
use std::path::Path;
use std::{env, fs};

use crate::testing::MIGRATIONS;

pub fn is_games_empty(conn: &mut SqliteConnection) -> Result<bool> {

    use crate::schema::game::dsl::*;

    let count: i64 = game.select(diesel::dsl::count_star()).first(conn)?;

    Ok(count == 0)
}



pub fn establish_connection() -> Result<SqliteConnection> {
    dotenv().ok();

    let database_url = database_url()?;

    SqliteConnection::establish(&database_url)
        .map_err(|_e| anyhow!("Error connecting to {database_url}"))
}

/// Open the SQLite database at `path` and apply any pending migrations.
///
/// Parent directories are created on demand, so callers (e.g. the GUI on
/// first launch) can point at `~/Library/.../stats_melee.db` before it
/// exists and have the file + schema come up cleanly.
pub fn open_database<P: AsRef<Path>>(path: P) -> Result<SqliteConnection> {
    let path = path.as_ref();

    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)
                .map_err(|e| anyhow!("mkdir {}: {e}", parent.display()))?;
        }
    }

    let url = path
        .to_str()
        .ok_or_else(|| anyhow!("db path is not utf-8: {}", path.display()))?;

    let mut conn = SqliteConnection::establish(url)
        .map_err(|e| anyhow!("opening {url}: {e}"))?;

    // Concurrency hardening. The GUI keeps several connections open against
    // the same file at once — the UI thread plus the ingest and summary
    // workers, which each open their own handle (SqliteConnection is
    // !Send). Under SQLite's default rollback journal a writer takes an
    // exclusive lock that blocks every reader, so an in-flight scan made
    // the Library/Analytics reads fail outright with "database is locked".
    //
    //   - journal_mode = WAL: readers see the last committed snapshot and
    //     no longer block on an active writer (the common case here).
    //   - busy_timeout: when two writers *do* contend (e.g. a delete during
    //     a scan), wait up to 5 s for the lock instead of erroring at once.
    //   - synchronous = NORMAL: the safe pairing with WAL; fewer fsyncs.
    //
    // WAL is a persistent property of the file, so setting it on any open
    // sticks for all connections; busy_timeout is per-connection, hence set
    // on every open.
    conn.batch_execute(
        "PRAGMA busy_timeout = 5000; \
         PRAGMA journal_mode = WAL; \
         PRAGMA synchronous = NORMAL;",
    )
    .map_err(|e| anyhow!("configuring sqlite at {url}: {e}"))?;

    conn.run_pending_migrations(MIGRATIONS)
        .map_err(|e| anyhow!("running migrations at {url}: {e}"))?;

    Ok(conn)
}

pub fn database_url() -> Result<String> {
    env::var("DATABASE_URL").map_err(|_| anyhow!("no database url found"))
}
