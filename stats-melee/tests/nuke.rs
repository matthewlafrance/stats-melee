//! Integration tests for ingestion dedup + `nuke_replays`.
//!
//! Covers the two user-visible invariants we care about:
//!
//!   1. Running `parse_new_replays` twice against the same folder ingests
//!      each replay exactly once — switching replay folders (or hitting
//!      "Scan" repeatedly) can't produce duplicate `game` rows.
//!   2. `nuke_replays` wipes every replay-scoped row, and a subsequent
//!      re-scan re-ingests the full set.

use std::fs;

use stats_melee::{is_games_empty, nuke_replay, nuke_replays, parse_new_replays};
use stats_melee::testing::{fixture_slps_or_skip, TestDb};

/// Copy a handful of fixtures into a tempdir arranged the way
/// `parse_new_replays` expects (root → session subdir → .slp files).
///
/// Returns `None` when the local-only fixture corpus is absent so the
/// calling test can skip on a clean checkout / CI.
fn stage_fixtures(session_name: &str) -> Option<(tempfile::TempDir, usize)> {
    let fixtures = fixture_slps_or_skip()?;
    let root = tempfile::tempdir().expect("tempdir");
    let session_dir = root.path().join(session_name);
    fs::create_dir_all(&session_dir).unwrap();

    let sample: Vec<_> = fixtures.iter().take(5).collect();
    for slp in &sample {
        let dest = session_dir.join(slp.file_name().unwrap());
        fs::copy(slp, &dest).expect("copy fixture");
    }
    Some((root, sample.len()))
}

#[test]
fn rescanning_same_folder_is_idempotent() {
    let mut db = TestDb::new().expect("tempdir db");
    let Some((root, expected)) = stage_fixtures("session-001") else {
        return;
    };
    let db_path = db.path.clone();

    let first = parse_new_replays(&mut db.conn, root.path(), &db_path)
        .expect("first scan");
    assert_eq!(
        first, expected,
        "first scan should ingest every copied fixture"
    );

    let second = parse_new_replays(&mut db.conn, root.path(), &db_path)
        .expect("second scan");
    assert_eq!(
        second, 0,
        "second scan should be a no-op — dedup on replay_path"
    );
}

#[test]
fn nuke_clears_replays_and_permits_reingest() {
    let mut db = TestDb::new().expect("tempdir db");
    let Some((root, expected)) = stage_fixtures("session-001") else {
        return;
    };
    let db_path = db.path.clone();

    let ingested = parse_new_replays(&mut db.conn, root.path(), &db_path)
        .expect("scan");
    assert_eq!(ingested, expected);
    assert!(!is_games_empty(&mut db.conn).unwrap());

    let deleted = nuke_replays(&mut db.conn).expect("nuke");
    assert_eq!(
        deleted, expected,
        "nuke should remove every game row we ingested"
    );
    assert!(
        is_games_empty(&mut db.conn).unwrap(),
        "games table should be empty after nuke"
    );

    // And a rescan after nuke should ingest everything fresh — the path
    // dedup keys off the game table, so a clean DB re-accepts every file.
    let reingested = parse_new_replays(&mut db.conn, root.path(), &db_path)
        .expect("rescan after nuke");
    assert_eq!(
        reingested, expected,
        "rescan after nuke should re-ingest every fixture"
    );
}

#[test]
fn switching_folders_ingests_older_replays() {
    // Simulates the real-world flow: user ingests from folder A, then
    // repoints the app at folder B (whose files happen to be older).
    // Pre-path-dedup, the mtime heuristic would skip folder B entirely;
    // now the only thing that matters is path identity.
    let mut db = TestDb::new().expect("tempdir db");

    // Folder A — ingest first, bumping the db file's mtime.
    let Some((root_a, a_count)) = stage_fixtures("session-a") else {
        return;
    };
    let db_path = db.path.clone();
    let a_ingested = parse_new_replays(&mut db.conn, root_a.path(), &db_path)
        .expect("scan a");
    assert_eq!(a_ingested, a_count);

    // Folder B — copy fixtures into a *fresh* tempdir. On most filesystems
    // these files' mtimes will land after the DB was last touched, but
    // that's fine: the guarantee we care about is that switching folders
    // works independently of mtimes.
    let Some((root_b, b_count)) = stage_fixtures("session-b") else {
        return;
    };
    let b_ingested = parse_new_replays(&mut db.conn, root_b.path(), &db_path)
        .expect("scan b");
    assert_eq!(
        b_ingested, b_count,
        "switching to a new folder should ingest all its replays"
    );

    // A second scan of folder B should be a no-op (dedup).
    let b_rescan = parse_new_replays(&mut db.conn, root_b.path(), &db_path)
        .expect("rescan b");
    assert_eq!(b_rescan, 0);
}

#[test]
fn per_row_delete_removes_one_game_and_leaves_others() {
    use diesel::prelude::*;
    use stats_melee::schema::game::dsl as g;

    let mut db = TestDb::new().expect("tempdir db");
    let Some((root, expected)) = stage_fixtures("session-001") else {
        return;
    };
    let db_path = db.path.clone();

    let ingested = parse_new_replays(&mut db.conn, root.path(), &db_path)
        .expect("scan");
    assert_eq!(ingested, expected);

    // Pick the lowest game_id to delete — deterministic for assertions.
    let mut ids: Vec<i32> = g::game.select(g::id).load(&mut db.conn).expect("ids");
    ids.sort();
    let target = ids[0];

    let deleted = nuke_replay(&mut db.conn, target).expect("nuke_replay");
    assert_eq!(deleted, 1, "should delete exactly one game row");

    // Other games survive intact.
    let after: Vec<i32> = g::game.select(g::id).load(&mut db.conn).expect("ids after");
    assert_eq!(after.len(), expected - 1);
    assert!(
        !after.contains(&target),
        "deleted game should not appear in remaining ids"
    );

    // Cascading deletes — punish + game_player_stat rows for the
    // deleted game must also be gone.
    use stats_melee::schema::{game_player_stat, punish};
    let stat_count: i64 = game_player_stat::table
        .filter(game_player_stat::game_id.eq(target))
        .count()
        .get_result(&mut db.conn)
        .expect("stat count");
    assert_eq!(stat_count, 0, "no orphaned game_player_stat rows");
    let punish_count: i64 = punish::table
        .filter(punish::game_id.eq(target))
        .count()
        .get_result(&mut db.conn)
        .expect("punish count");
    assert_eq!(punish_count, 0, "no orphaned punish rows");
}

#[test]
fn per_row_delete_returns_zero_for_missing_id() {
    let mut db = TestDb::new().expect("tempdir db");
    let deleted = nuke_replay(&mut db.conn, 999_999).expect("nuke_replay");
    assert_eq!(
        deleted, 0,
        "deleting a non-existent game should report 0 rows removed (not error)"
    );
}
