//! Test-support helpers: spin up a fresh SQLite database with migrations applied.
//!
//! These live in the library (rather than under `#[cfg(test)]`) so that
//! integration tests under `tests/` — which compile against the crate as an
//! external consumer — can share the same setup.

use anyhow::{anyhow, Result};
use diesel::prelude::*;
use diesel_migrations::{embed_migrations, EmbeddedMigrations, MigrationHarness};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use tempfile::TempDir;

/// Migrations embedded at compile time from `migrations/`.
pub const MIGRATIONS: EmbeddedMigrations = embed_migrations!("migrations");

/// A transient SQLite database that is torn down when dropped.
///
/// The backing `TempDir` keeps the file alive for the lifetime of the handle.
pub struct TestDb {
    pub conn: SqliteConnection,
    pub path: PathBuf,
    _dir: TempDir,
}

impl TestDb {
    /// Create a fresh database with all migrations applied and return a
    /// connected handle.
    pub fn new() -> Result<Self> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("stats_melee_test.db");

        let url = path
            .to_str()
            .ok_or_else(|| anyhow!("temp db path not utf-8"))?;

        let mut conn = SqliteConnection::establish(url)
            .map_err(|e| anyhow!("failed to open test db at {url}: {e}"))?;

        conn.run_pending_migrations(MIGRATIONS)
            .map_err(|e| anyhow!("failed to run migrations: {e}"))?;

        Ok(TestDb {
            conn,
            path,
            _dir: dir,
        })
    }
}

/// Absolute path to the `test_slps/` fixture directory bundled with the repo.
///
/// Tests can set `STATS_MELEE_TEST_SLPS` to override — useful in CI where
/// fixtures may live elsewhere.
pub fn fixtures_dir() -> PathBuf {
    static DIR: OnceLock<PathBuf> = OnceLock::new();
    DIR.get_or_init(|| {
        if let Ok(override_dir) = std::env::var("STATS_MELEE_TEST_SLPS") {
            return PathBuf::from(override_dir);
        }
        // CARGO_MANIFEST_DIR is stats-melee/; fixtures live at ../test_slps/
        let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        p.pop();
        p.push("test_slps");
        p
    })
    .clone()
}

/// Return every `.slp` path under `fixtures_dir()`, sorted for stability.
pub fn fixture_slps() -> Result<Vec<PathBuf>> {
    let dir = fixtures_dir();
    slps_in(&dir)
}

/// List `.slp` files directly under `root`, sorted by filename.
pub fn slps_in(root: &Path) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(root)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_file() && path.extension().and_then(|e| e.to_str()) == Some("slp") {
            out.push(path);
        }
    }
    out.sort();
    Ok(out)
}
