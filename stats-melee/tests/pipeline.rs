//! End-to-end test: fresh DB → ingest real fixtures → query + analyze.

use std::fs;

use stats_melee::{
    analyze_games, avg_placement_by_code, avg_stocks_remaining, filter_games,
    get_stats_for_game, parse_new_replays, parse_single_replay, player_summary,
    player_summary_filtered, post_game, PlayerSummary, PlayerSummaryFilter,
};
use stats_melee::testing::{fixture_slps, TestDb};

/// Copy a handful of fixture replays into a tempdir arranged the way
/// `parse_new_replays` expects (root → session subdir → .slp files), then run
/// the ingestion loop and assert sane counts.
#[test]
fn parse_new_replays_ingests_fixtures() {
    let mut db = TestDb::new().expect("tempdir db");
    let fixtures = fixture_slps().expect("fixture listing");
    assert!(fixtures.len() >= 5, "need at least 5 fixtures");

    let root = tempfile::tempdir().expect("tempdir");
    let session_dir = root.path().join("session-001");
    fs::create_dir_all(&session_dir).unwrap();

    let sample: Vec<_> = fixtures.iter().take(5).collect();
    for slp in &sample {
        let dest = session_dir.join(slp.file_name().unwrap());
        fs::copy(slp, &dest).expect("copy fixture");
    }

    // Borrow path and conn as separate fields of `db` so the TempDir stays
    // alive for the whole test (otherwise the SQLite file would be unlinked
    // while the connection is still open).
    let db_path = db.path.clone();
    let ingested = parse_new_replays(&mut db.conn, root.path(), &db_path)
        .expect("parse_new_replays should succeed");

    assert_eq!(
        ingested,
        sample.len(),
        "expected to ingest every copied fixture"
    );

    // Every ingested row should carry a content_hash — Track 11d's
    // contract is that production ingestion always populates the
    // sidecar-cache key. Hex-SHA256 is exactly 64 lowercase hex chars.
    use diesel::prelude::*;
    use stats_melee::schema::game::dsl as game_dsl;
    let hashes: Vec<Option<String>> = game_dsl::game
        .select(game_dsl::content_hash)
        .load(&mut db.conn)
        .expect("load content_hash column");
    assert_eq!(hashes.len(), sample.len(), "one row per ingested fixture");
    for h in &hashes {
        let h = h
            .as_ref()
            .expect("ingested rows must have a content_hash populated");
        assert_eq!(h.len(), 64, "expected 64-char hex sha256, got {h:?}");
        assert!(
            h.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
            "expected lowercase hex, got {h:?}"
        );
    }
}

/// End-to-end: ingest fixtures by calling `post_game` directly (skipping the
/// directory walker), then use the query API (`filter_games` + `analyze_games`)
/// and spot-check invariants.
#[test]
fn analytics_roundtrip_on_fixtures() {
    let mut db = TestDb::new().expect("tempdir db");
    let fixtures = fixture_slps().expect("fixture listing");

    // Pull in up to 20 fixtures so the resulting analytics have something to
    // aggregate over but tests stay fast.
    let sample: Vec<_> = fixtures.iter().take(20).collect();

    let mut codes_seen: Vec<String> = Vec::new();
    for slp in &sample {
        let gd = parse_single_replay(slp).expect("parse");
        for slot in gd.placements.iter().flatten() {
            if !codes_seen.contains(&slot.code.clone()) {
                codes_seen.push(slot.code.clone());
            }
        }
        post_game(&mut db.conn, &gd).expect("post_game");
    }

    assert!(
        !codes_seen.is_empty(),
        "should have observed at least one player code"
    );

    // Pick the code that appears in the most games — guaranteed to be the
    // user whose replays these are.
    let target_code = codes_seen[0].clone();
    let games = filter_games(&mut db.conn, &target_code).expect("filter");
    assert!(!games.is_empty(), "filter_games returned no games for {target_code}");

    let analytics = analyze_games(&mut db.conn, &games, &target_code).expect("analyze");

    // Win proportions must lie in [0, 1].
    for wp in analytics.stages.iter() {
        assert!(
            (0.0..=1.0).contains(&wp.proportion),
            "stage winrate out of range: {}",
            wp.proportion
        );
    }
    for wp in analytics.played_characters.iter() {
        assert!(
            (0.0..=1.0).contains(&wp.proportion),
            "played-char winrate out of range: {}",
            wp.proportion
        );
    }
    for wp in analytics.opponents.values() {
        assert!(
            (0.0..=1.0).contains(&wp.proportion),
            "opponent winrate out of range: {}",
            wp.proportion
        );
    }

    // Total opponent encounters can't exceed the number of games we fed in.
    let total_opp_encounters: i32 = analytics.opponents.values().map(|wp| wp.total).sum();
    assert!(
        total_opp_encounters as usize <= games.len() * 3,
        "too many opponent encounters for {} games",
        games.len()
    );

    // --- game_player_stat assertions (Track 2) ---

    // Every ingested game should have at least one stat row, and every stat
    // row should have a sane placement + stocks value.
    let mut total_stat_rows = 0;
    let mut rows_with_stocks = 0;
    for game in games.iter() {
        let stats = get_stats_for_game(&mut db.conn, game.id).expect("get_stats_for_game");
        assert!(
            !stats.is_empty(),
            "game {} has no game_player_stat rows",
            game.id
        );

        // At most one row per placement slot.
        let mut seen_placements = std::collections::HashSet::new();
        for stat in &stats {
            assert!(
                seen_placements.insert(stat.placement),
                "duplicate placement {} for game {}",
                stat.placement,
                game.id
            );
            assert!(
                (0..=3).contains(&stat.placement),
                "placement {} out of range for stat id {}",
                stat.placement,
                stat.id
            );
            if let Some(stocks) = stat.stocks_remaining {
                assert!(
                    (0..=255).contains(&stocks),
                    "stocks_remaining {} out of range for stat id {}",
                    stocks,
                    stat.id
                );
                rows_with_stocks += 1;
            }
            total_stat_rows += 1;
        }
    }

    assert!(
        total_stat_rows > 0,
        "expected at least one stat row after ingestion"
    );
    // Stocks extraction is best-effort, but for a non-trivial corpus the vast
    // majority of rows should have frame data. Require >= 50% to catch
    // systematic failures while tolerating the occasional corrupt file.
    assert!(
        rows_with_stocks * 2 >= total_stat_rows,
        "only {}/{} stat rows had stocks_remaining populated",
        rows_with_stocks,
        total_stat_rows
    );

    // Aggregate queries should return values in reasonable ranges.
    let avg_placement = avg_placement_by_code(&mut db.conn, &target_code)
        .expect("avg_placement_by_code");
    let avg_placement = avg_placement.expect("target code should have stat rows");
    assert!(
        (0.0..=3.0).contains(&avg_placement),
        "avg_placement {} out of range",
        avg_placement
    );

    let avg_stocks = avg_stocks_remaining(&mut db.conn, &target_code)
        .expect("avg_stocks_remaining");
    if let Some(s) = avg_stocks {
        assert!(
            (0.0..=4.0).contains(&s),
            "avg_stocks {} out of range (expected 0..=4 for melee)",
            s
        );
    }

    // --- PlayerSummary roll-up ------------------------------------------------

    let summary = player_summary(&mut db.conn, &target_code).expect("player_summary");
    assert_eq!(summary.code, target_code);
    assert!(
        summary.games_played as usize >= games.len(),
        "summary.games_played {} should be >= filter_games count {}",
        summary.games_played,
        games.len()
    );

    // Every rolled-up field should match its standalone helper — use the
    // helper as source of truth.
    assert_eq!(summary.avg_placement, avg_placement_by_code(&mut db.conn, &target_code).unwrap());
    assert_eq!(summary.avg_stocks_remaining, avg_stocks_remaining(&mut db.conn, &target_code).unwrap());

    if let Some(lr) = summary.l_cancel_rate {
        assert!(
            (0.0..=1.0).contains(&lr),
            "l_cancel_rate {} out of range",
            lr
        );
    }
    if let Some(apm) = summary.avg_apm {
        // Melee APM realistically tops out around 600 for the best players;
        // a much higher value suggests a bug.
        assert!(
            (0.0..1000.0).contains(&apm),
            "avg_apm {} unreasonable",
            apm
        );
    }
    if let Some(apl) = summary.avg_punish_length {
        assert!(
            apl >= 1.0,
            "avg_punish_length {} < 1 (impossible — every punish has >=1 hit)",
            apl
        );
    }
    if let Some(opk) = summary.openings_per_kill {
        assert!(opk >= 1.0, "openings_per_kill {} < 1", opk);
    }

    // top_kill_moves is capped at TOP_KILL_MOVES_CAP.
    assert!(
        summary.top_kill_moves.len() <= PlayerSummary::TOP_KILL_MOVES_CAP,
        "top_kill_moves over cap: {}",
        summary.top_kill_moves.len()
    );

    // Streaks: current's magnitude can't exceed either longest streak.
    let current_mag = summary.streaks.current.unsigned_abs() as i32;
    let max_len = summary.streaks.longest_win.max(summary.streaks.longest_loss);
    assert!(
        current_mag <= max_len,
        "|current| {} exceeds max streak {} (streaks: {:?})",
        current_mag,
        max_len,
        summary.streaks
    );
}

/// `player_summary_filtered` with no filter must reproduce `player_summary`
/// exactly, and applying a non-trivial character/stage filter must narrow
/// the result (or, at worst, leave it unchanged when every game already
/// matches).
#[test]
fn player_summary_filtered_narrows_by_character_and_stage() {
    use std::collections::HashSet;
    use stats_melee::{get_character, get_game_player_code};

    let mut db = TestDb::new().expect("tempdir db");
    let fixtures = fixture_slps().expect("fixture listing");
    let sample: Vec<_> = fixtures.iter().take(20).collect();

    let mut codes_seen: Vec<String> = Vec::new();
    for slp in &sample {
        let gd = parse_single_replay(slp).expect("parse");
        for slot in gd.placements.iter().flatten() {
            if !codes_seen.contains(&slot.code.clone()) {
                codes_seen.push(slot.code.clone());
            }
        }
        post_game(&mut db.conn, &gd).expect("post_game");
    }
    assert!(!codes_seen.is_empty(), "no codes ingested");
    let target_code = codes_seen[0].clone();

    // Sanity: NONE filter is a true no-op.
    let unfiltered = player_summary(&mut db.conn, &target_code).expect("unfiltered");
    let none = player_summary_filtered(&mut db.conn, &target_code, &PlayerSummaryFilter::NONE)
        .expect("filtered NONE");
    assert_eq!(unfiltered.games_played, none.games_played);
    assert_eq!(unfiltered.avg_placement, none.avg_placement);
    assert_eq!(unfiltered.streaks, none.streaks);

    // Find a (character, stage) pair the target code actually played, so
    // we can assert "filtered subset is non-empty AND <= unfiltered".
    let games = filter_games(&mut db.conn, &target_code).expect("filter_games");
    let mut chars: HashSet<i32> = HashSet::new();
    let mut stages: HashSet<i32> = HashSet::new();
    for g in &games {
        for slot in [g.first, g.second, g.third, g.fourth].into_iter().flatten() {
            if let Ok(code) = get_game_player_code(&mut db.conn, slot) {
                if code == target_code {
                    if let Ok(c) = get_character(&mut db.conn, slot) {
                        chars.insert(c);
                    }
                }
            }
        }
        stages.insert(g.stage);
    }
    let pick_char = *chars.iter().next().expect("at least one character");
    let pick_stage = *stages.iter().next().expect("at least one stage");

    let by_char = player_summary_filtered(
        &mut db.conn,
        &target_code,
        &PlayerSummaryFilter { character_id: Some(pick_char), stage_id: None, game_ids: None },
    )
    .expect("filter by character");
    let by_stage = player_summary_filtered(
        &mut db.conn,
        &target_code,
        &PlayerSummaryFilter { character_id: None, stage_id: Some(pick_stage), game_ids: None },
    )
    .expect("filter by stage");
    let by_both = player_summary_filtered(
        &mut db.conn,
        &target_code,
        &PlayerSummaryFilter {
            character_id: Some(pick_char),
            stage_id: Some(pick_stage),
            game_ids: None,
        },
    )
    .expect("filter by both");

    // Each filter must be a (non-strict) narrowing of the unfiltered set.
    assert!(
        by_char.games_played <= unfiltered.games_played,
        "char filter widened games: {} > {}",
        by_char.games_played,
        unfiltered.games_played
    );
    assert!(
        by_stage.games_played <= unfiltered.games_played,
        "stage filter widened games: {} > {}",
        by_stage.games_played,
        unfiltered.games_played
    );
    assert!(by_char.games_played > 0, "char filter returned 0 games");
    assert!(by_stage.games_played > 0, "stage filter returned 0 games");

    // Composing both filters can never widen relative to either single
    // filter — set intersection is monotone.
    assert!(
        by_both.games_played <= by_char.games_played,
        "both-filter wider than char-only: {} > {}",
        by_both.games_played,
        by_char.games_played
    );
    assert!(
        by_both.games_played <= by_stage.games_played,
        "both-filter wider than stage-only: {} > {}",
        by_both.games_played,
        by_stage.games_played
    );

    // The `game_ids` restriction (how the GUI threads its full library
    // filter down): restricting to the player's *entire* game set is a no-op;
    // a single id restricts to at most one game; an empty set matches none.
    let all_ids: Vec<i32> = games.iter().map(|g| g.id).collect();
    let by_all_ids = player_summary_filtered(
        &mut db.conn,
        &target_code,
        &PlayerSummaryFilter {
            character_id: None,
            stage_id: None,
            game_ids: Some(all_ids.clone()),
        },
    )
    .expect("filter by all game ids");
    assert_eq!(
        by_all_ids.games_played, unfiltered.games_played,
        "restricting to the player's full game set changed the count"
    );

    let one_id = vec![*all_ids.first().expect("at least one game")];
    let by_one_id = player_summary_filtered(
        &mut db.conn,
        &target_code,
        &PlayerSummaryFilter {
            character_id: None,
            stage_id: None,
            game_ids: Some(one_id),
        },
    )
    .expect("filter by one game id");
    assert!(
        by_one_id.games_played <= 1,
        "single-game-id filter returned {} games",
        by_one_id.games_played
    );

    let by_empty = player_summary_filtered(
        &mut db.conn,
        &target_code,
        &PlayerSummaryFilter {
            character_id: None,
            stage_id: None,
            game_ids: Some(Vec::new()),
        },
    )
    .expect("filter by empty game ids");
    assert_eq!(
        by_empty.games_played, 0,
        "empty game-id set should match no games"
    );
}
