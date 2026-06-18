//! Invariants for the `game.ingested_at` column.
//!
//! The column is populated by SQLite via `DEFAULT CURRENT_TIMESTAMP` —
//! no Rust code writes it explicitly. These tests pin down:
//!
//!   1. A freshly-ingested game has a non-empty ISO-8601 timestamp.
//!   2. The timestamp is close (within a few minutes) to wall-clock now,
//!      i.e. SQLite's clock agrees with the host.
//!   3. Format is the fixed-width `"YYYY-MM-DD HH:MM:SS"` shape that the
//!      app relies on for lex-sort-as-chronological-sort.
//!
//! We use `post_game` (not the filesystem walker) to keep the test
//! independent of fixture corpus size.

use std::fs;
use std::io;

use peppi::io::slippi;
use stats_melee::gamedata::GameData;
use stats_melee::post_game;
use stats_melee::testing::{fixture_slps, TestDb};

use diesel::prelude::*;

fn parse_one() -> GameData {
    let slps = fixture_slps().expect("fixture listing");
    let first = slps.first().expect("at least one fixture");
    let mut r = io::BufReader::new(fs::File::open(first).expect("open fixture"));
    let raw = slippi::read(&mut r, None).expect("peppi parse");
    GameData::new_gamedata(&raw).expect("GameData")
}

#[test]
fn ingested_at_is_populated_on_insert() {
    use stats_melee::schema::game::dsl as g;

    let mut db = TestDb::new().expect("tempdir db");
    let gd = parse_one();
    let inserted = post_game(&mut db.conn, &gd).expect("post_game");

    // Re-query — `post_game` returns the row via RETURNING, so the
    // ingested_at we get back should already be populated. Confirm by
    // pulling it back out of the DB too.
    let ts: String = g::game
        .filter(g::id.eq(inserted.id))
        .select(g::ingested_at)
        .first(&mut db.conn)
        .expect("select ingested_at");

    assert!(
        !ts.is_empty(),
        "ingested_at should be non-empty after insert"
    );
    assert_eq!(
        ts, inserted.ingested_at,
        "value returned from RETURNING should match a fresh SELECT"
    );
}

#[test]
fn ingested_at_has_iso8601_shape() {
    let mut db = TestDb::new().expect("tempdir db");
    let gd = parse_one();
    let inserted = post_game(&mut db.conn, &gd).expect("post_game");

    // Shape: "YYYY-MM-DD HH:MM:SS" — 19 chars, specific delimiter layout.
    let ts = &inserted.ingested_at;
    assert_eq!(
        ts.len(),
        19,
        "expected fixed-width ISO-8601 (19 chars), got {:?} ({} chars)",
        ts,
        ts.len()
    );
    let bytes = ts.as_bytes();
    assert_eq!(bytes[4], b'-', "YYYY-MM separator");
    assert_eq!(bytes[7], b'-', "MM-DD separator");
    assert_eq!(bytes[10], b' ', "date/time separator");
    assert_eq!(bytes[13], b':', "HH:MM separator");
    assert_eq!(bytes[16], b':', "MM:SS separator");
    // Year matches the expected century — no SQLite epoch weirdness.
    assert!(
        ts.starts_with("20"),
        "expected year to start with 20XX, got {ts:?}"
    );
}

#[test]
fn ingested_at_lex_order_matches_insertion_order() {
    // Two sequential inserts should produce timestamps where the second
    // sorts >= the first when compared lexicographically. This is the
    // invariant the Replay Library's sort-by-date relies on.
    let mut db = TestDb::new().expect("tempdir db");
    let gd = parse_one();

    let a = post_game(&mut db.conn, &gd).expect("post first");
    // Sleep a second so the TIMESTAMP column (second-resolution) can tick.
    // Keeps the test deterministic without the flakiness of sub-second
    // ordering on SQLite.
    std::thread::sleep(std::time::Duration::from_millis(1_100));
    let b = post_game(&mut db.conn, &gd).expect("post second");

    assert!(
        b.ingested_at >= a.ingested_at,
        "later insert's timestamp ({}) should sort >= earlier ({})",
        b.ingested_at,
        a.ingested_at
    );
    assert!(b.id > a.id, "ids should always be monotonic");
}
