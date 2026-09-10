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

/// Like [`fixture_slps`], but returns `None` when the fixture corpus is
/// absent or empty. The corpus is local-only (gitignored), so integration
/// tests use this to skip rather than fail on a clean checkout / CI.
pub fn fixture_slps_or_skip() -> Option<Vec<PathBuf>> {
    match fixture_slps() {
        Ok(slps) if !slps.is_empty() => Some(slps),
        _ => None,
    }
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

/// Every `.slp` under `root`, at any depth, sorted for determinism.
///
/// Unlike [`slps_in`], which lists one directory, this walks the whole tree —
/// real replay corpora nest by date, character, or tournament, and a
/// benchmark shouldn't care which. An unreadable subdirectory is skipped
/// rather than failing the walk.
pub fn slps_recursive(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().and_then(|e| e.to_str()) == Some("slp") {
                out.push(path);
            }
        }
    }
    out.sort();
    out
}

/// Materialize a corpus into the `root/<subdir>/*.slp` layout the ingester
/// walks, returning the tempdir holding it and how many replays it holds.
///
/// Files are hard-linked where the filesystem allows and copied otherwise,
/// so staging a large corpus costs neither time nor disk. Names are
/// index-prefixed because a recursive scan turns up the same basename in
/// several source directories.
///
/// Staging rather than pointing the ingester at the original tree is what
/// makes measurements comparable: every run then sees one identical flat
/// directory, in one identical order, whatever the source layout was.
pub fn stage_corpus(src: &Path, limit: Option<usize>) -> (TempDir, usize) {
    let mut slps = slps_recursive(src);
    if let Some(n) = limit {
        slps.truncate(n);
    }

    let root = tempfile::tempdir().expect("tempdir");
    let session = root.path().join("session-000");
    std::fs::create_dir_all(&session).expect("create session dir");

    for (i, slp) in slps.iter().enumerate() {
        let name = slp.file_name().expect("replay has a filename");
        let dest = session.join(format!("{i:06}-{}", name.to_string_lossy()));
        if std::fs::hard_link(slp, &dest).is_err() {
            std::fs::copy(slp, &dest).expect("stage replay by copy");
        }
    }

    let staged = slps.len();
    (root, staged)
}

/// Read a corpus path from an environment variable, expanding a leading
/// `~/`. Returns `None` when the variable is unset, so opt-in benchmarks can
/// skip cleanly.
///
/// The tilde handling matters: a shell does not expand `~` inside a quoted
/// variable, and passing a quoted path is the most natural way to set these.
pub fn corpus_from_env(var: &str) -> Option<PathBuf> {
    let raw = std::env::var(var).ok()?;
    let expanded = match raw.strip_prefix("~/") {
        Some(rest) => match std::env::var("HOME").ok().filter(|h| !h.is_empty()) {
            Some(home) => format!("{home}/{rest}"),
            None => raw,
        },
        None => raw,
    };
    Some(PathBuf::from(expanded))
}
