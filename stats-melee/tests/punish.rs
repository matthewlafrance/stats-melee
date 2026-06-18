//! End-to-end test: parse .slp fixtures, ingest punishes into a fresh DB,
//! query them back, and spot-check invariants + aggregate helpers.
//!
//! The combat/punish detectors are only defined for 1v1 games, so this test
//! filters the fixture set down to 1v1 replays before ingesting.

use std::collections::HashSet;

use stats_melee::testing::{fixture_slps, TestDb};
use stats_melee::{
    avg_punish_length_by_code, get_punishes_for_game, most_common_kill_moves_by_code,
    openings_per_kill_by_code, parse_single_replay, post_game,
};

/// Ingest every 1v1 fixture, then walk the resulting punish rows and check
/// that each row satisfies the invariants the extractor promises.
#[test]
fn punishes_ingest_and_satisfy_invariants() {
    let mut db = TestDb::new().expect("tempdir db");
    let fixtures = fixture_slps().expect("fixture listing");
    assert!(fixtures.len() >= 5, "need at least 5 fixtures");

    // Keep the target-code as the code that appears in the most games — it's
    // guaranteed to be the user whose replays these are.
    let mut code_counts: std::collections::HashMap<String, usize> =
        std::collections::HashMap::new();
    let mut ingested_game_ids: Vec<i32> = Vec::new();
    let mut ingested_1v1 = 0usize;

    for slp in fixtures.iter().take(25) {
        let gd = match parse_single_replay(slp) {
            Ok(g) => g,
            Err(_) => continue,
        };

        // Count populated placements as a proxy for "1v1-ness". The punish
        // extractor returns empty for non-1v1 via `unwrap_or_default()` in
        // `new_gamedata`, but we can only meaningfully assert non-empty punish
        // output on actual 1v1 games, so filter here.
        let populated = gd.placements.iter().filter(|p| p.is_some()).count();
        if populated != 2 {
            continue;
        }

        // Bump the per-code appearance count so we can pick a reasonable
        // target for the aggregate queries below.
        for slot in gd.placements.iter().flatten() {
            *code_counts.entry(slot.code().to_string()).or_insert(0) += 1;
        }

        let inserted = post_game(&mut db.conn, &gd).expect("post_game");
        ingested_game_ids.push(inserted.id);
        ingested_1v1 += 1;
    }

    assert!(
        ingested_1v1 >= 3,
        "expected to ingest >= 3 1v1 fixtures, got {ingested_1v1}"
    );

    // --- invariant checks on the punish table --------------------------------

    let mut total_punishes = 0usize;
    let mut games_with_punishes = 0usize;
    let mut any_kill = false;
    let mut any_kill_move_populated = false;

    for gid in &ingested_game_ids {
        let punishes = get_punishes_for_game(&mut db.conn, *gid).expect("get_punishes_for_game");
        if !punishes.is_empty() {
            games_with_punishes += 1;
        }

        // Ordered by start_frame ascending per the query contract.
        let mut prev_start = i32::MIN;
        for p in &punishes {
            assert!(
                p.start_frame >= prev_start,
                "punishes for game {gid} not ordered: {} < {prev_start}",
                p.start_frame
            );
            prev_start = p.start_frame;

            assert_eq!(p.game_id, *gid, "punish row has wrong game_id");
            assert!(
                p.end_frame >= p.start_frame,
                "end_frame {} < start_frame {} for punish {}",
                p.end_frame,
                p.start_frame,
                p.id
            );
            assert!(
                p.hit_count >= 1,
                "hit_count {} < 1 for punish {}",
                p.hit_count,
                p.id
            );
            assert!(
                p.did_kill == 0 || p.did_kill == 1,
                "did_kill {} not a boolean for punish {}",
                p.did_kill,
                p.id
            );
            assert_ne!(
                p.attacker_id, p.victim_id,
                "attacker == victim for punish {}",
                p.id
            );

            if p.did_kill_bool() {
                any_kill = true;
                if p.kill_move.is_some() {
                    any_kill_move_populated = true;
                }
            } else {
                // Non-kill punishes shouldn't carry a kill_move — it would be
                // misleading to downstream analytics.
                assert!(
                    p.kill_move.is_none(),
                    "non-kill punish {} has kill_move {:?}",
                    p.id,
                    p.kill_move
                );
            }

            total_punishes += 1;
        }
    }

    // Across a handful of real 1v1 fixtures we expect at least one punish and
    // at least one kill. If either is zero the extractor is almost certainly
    // broken.
    assert!(
        total_punishes > 0,
        "no punishes ingested across {ingested_1v1} 1v1 games"
    );
    assert!(
        games_with_punishes > 0,
        "none of the {ingested_1v1} games had any punishes"
    );
    assert!(
        any_kill,
        "no punish was flagged as a kill — stock-drop detection may be broken"
    );
    // kill_move is best-effort (depends on last_attack_landed being populated,
    // which is frame-format version dependent), so don't hard-require it.
    // Just warn via eprintln! for debuggability.
    if !any_kill_move_populated {
        eprintln!(
            "warning: every kill punish had kill_move = None — \
             last_attack_landed may not be populated in these fixtures"
        );
    }

    // --- aggregate query spot-checks -----------------------------------------

    // Pick the most frequently occurring code as the "target".
    let (target_code, _) = code_counts
        .iter()
        .max_by_key(|(_, n)| *n)
        .map(|(c, n)| (c.clone(), *n))
        .expect("at least one code seen");

    // If the target happens to have no attacker-side punishes (e.g. they were
    // only on the receiving end in these fixtures), the helpers return None —
    // that's fine, we just shouldn't blow up. Otherwise, ranges should hold.
    if let Some(avg_len) =
        avg_punish_length_by_code(&mut db.conn, &target_code).expect("avg_punish_length")
    {
        assert!(
            avg_len >= 1.0,
            "avg_punish_length {avg_len} < 1 (every punish has >= 1 hit)"
        );
        // Combos in melee can be long but the per-game average over many
        // games shouldn't exceed a realistic ceiling.
        assert!(
            avg_len < 50.0,
            "avg_punish_length {avg_len} unreasonably high"
        );
    }

    if let Some(opk) =
        openings_per_kill_by_code(&mut db.conn, &target_code).expect("openings_per_kill")
    {
        assert!(
            opk >= 1.0,
            "openings_per_kill {opk} < 1 (kills are a subset of punishes)"
        );
    }

    let kill_moves =
        most_common_kill_moves_by_code(&mut db.conn, &target_code).expect("kill_moves");
    // Sorted desc by count — verify.
    let mut prev_count = i32::MAX;
    let mut seen_ids: HashSet<i32> = HashSet::new();
    for (attack_id, count) in &kill_moves {
        assert!(count >= &1, "zero-count row in kill_moves");
        assert!(
            *count <= prev_count,
            "kill_moves not sorted desc: {count} > {prev_count}"
        );
        prev_count = *count;
        assert!(
            seen_ids.insert(*attack_id),
            "duplicate attack_id {attack_id} in kill_moves"
        );
    }
}

/// Ingesting the same replay twice must not crash, and must produce a
/// sensible punish count on the second game. We can't assert exact equality
/// across the two games because `game.id` differs, but the per-game counts
/// should match.
#[test]
fn repeated_ingestion_yields_matching_punish_counts() {
    let mut db = TestDb::new().expect("tempdir db");
    let fixtures = fixture_slps().expect("fixture listing");

    // Find the first fixture that's 1v1 and has at least one punish.
    for slp in fixtures.iter().take(20) {
        let gd = match parse_single_replay(slp) {
            Ok(g) => g,
            Err(_) => continue,
        };
        let populated = gd.placements.iter().filter(|p| p.is_some()).count();
        if populated != 2 || gd.punishes.is_empty() {
            continue;
        }

        let first = post_game(&mut db.conn, &gd).expect("first ingest");
        let second = post_game(&mut db.conn, &gd).expect("second ingest");

        let first_punishes =
            get_punishes_for_game(&mut db.conn, first.id).expect("punishes first");
        let second_punishes =
            get_punishes_for_game(&mut db.conn, second.id).expect("punishes second");

        assert_eq!(
            first_punishes.len(),
            second_punishes.len(),
            "re-ingesting the same replay should produce the same punish count"
        );
        // Structural equality on the non-id fields.
        for (a, b) in first_punishes.iter().zip(second_punishes.iter()) {
            assert_eq!(a.start_frame, b.start_frame);
            assert_eq!(a.end_frame, b.end_frame);
            assert_eq!(a.hit_count, b.hit_count);
            assert_eq!(a.did_kill, b.did_kill);
            assert_eq!(a.kill_move, b.kill_move);
        }
        return;
    }

    panic!("no suitable 1v1 fixture with non-empty punishes found in the first 20 fixtures");
}
