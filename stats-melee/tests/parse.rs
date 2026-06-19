//! Integration tests that exercise the .slp parser against real fixture files.

use stats_melee::gamedata::{CHARACTERS, STAGES};
use stats_melee::parse_single_replay;
use stats_melee::testing::{fixture_slps, fixtures_dir};

/// Sanity-check: every bundled fixture should parse without error. This is the
/// cheapest regression test against peppi upgrades and parser bugs.
#[test]
fn every_fixture_parses() {
    let slps = fixture_slps().expect("failed to list fixtures");
    assert!(
        !slps.is_empty(),
        "no .slp fixtures found under {:?} — is the test_slps/ directory populated?",
        fixtures_dir()
    );

    let mut failures = Vec::new();
    for slp in &slps {
        if let Err(e) = parse_single_replay(slp) {
            failures.push(format!("{}: {e}", slp.display()));
        }
    }

    assert!(
        failures.is_empty(),
        "{} / {} fixtures failed to parse:\n{}",
        failures.len(),
        slps.len(),
        failures.join("\n")
    );
}

/// The Slippi metadata `startAt` play timestamp should be extracted for the
/// vast majority of real replays — guards the date-played column/filter
/// against a peppi metadata-shape change that would silently null it out.
#[test]
fn fixtures_carry_played_date() {
    let slps = fixture_slps().expect("failed to list fixtures");
    let mut with_date = 0usize;
    for slp in &slps {
        let gd = parse_single_replay(slp)
            .unwrap_or_else(|e| panic!("failed to parse {}: {e}", slp.display()));
        if let Some(s) = &gd.started_at {
            // Looks like an ISO-8601 date ("YYYY-MM-DD...").
            assert!(
                s.len() >= 10 && s.as_bytes()[4] == b'-' && s.as_bytes()[7] == b'-',
                "{} has a non-ISO started_at: {s:?}",
                slp.display()
            );
            with_date += 1;
        }
    }
    assert!(
        with_date * 2 >= slps.len(),
        "expected most fixtures to carry a played date, got {with_date}/{}",
        slps.len()
    );
}

/// Every parsed fixture should have at least two players (the corpus is all
/// 1v1), a stage index within the known range, and a non-negative duration.
#[test]
fn parsed_fixtures_have_sane_invariants() {
    let slps = fixture_slps().expect("failed to list fixtures");

    for slp in slps {
        let gd = parse_single_replay(&slp)
            .unwrap_or_else(|e| panic!("failed to parse {}: {e}", slp.display()));

        let player_count = gd.placements.iter().filter(|p| p.is_some()).count();
        assert!(
            player_count >= 2,
            "{} has only {} player(s)",
            slp.display(),
            player_count
        );

        assert!(
            (gd.stage as usize) < STAGES.len(),
            "{} has out-of-range stage {}",
            slp.display(),
            gd.stage
        );

        assert!(
            gd.time >= 0,
            "{} has negative duration {}",
            slp.display(),
            gd.time
        );

        for slot in gd.placements.iter().flatten() {
            assert!(
                (slot.character as usize) < CHARACTERS.len(),
                "{} has out-of-range character {}",
                slp.display(),
                slot.character
            );
        }
    }
}

/// `placements[0]` should be the winner (1st place). We don't know the winner
/// for every fixture a priori, but we can at least assert consistency: the
/// winner slot is populated whenever any slot is.
#[test]
fn winner_slot_populated_when_any_player_is() {
    let slps = fixture_slps().expect("failed to list fixtures");

    for slp in slps {
        let gd = parse_single_replay(&slp)
            .unwrap_or_else(|e| panic!("failed to parse {}: {e}", slp.display()));

        let any_populated = gd.placements.iter().any(|p| p.is_some());
        if any_populated {
            assert!(
                gd.winner().is_some(),
                "{} has players but no winner slot populated",
                slp.display()
            );
        }
    }
}

/// Stocks-remaining extraction should line up with placements: every slot that
/// has a player should (almost always) also have a stocks value, and each
/// stocks value should be 0..=4 for a normal melee game.
#[test]
fn stocks_remaining_populated_and_sane() {
    let slps = fixture_slps().expect("failed to list fixtures");

    let mut checked = 0usize;
    let mut slots_with_player = 0usize;
    let mut slots_with_stocks = 0usize;
    let mut winners_with_stocks = 0usize;
    let mut losers_with_stocks = 0usize;
    let mut winner_has_more_stocks_than_loser = 0usize;
    let mut total_1v1_comparisons = 0usize;

    for slp in slps {
        let gd = parse_single_replay(&slp)
            .unwrap_or_else(|e| panic!("failed to parse {}: {e}", slp.display()));
        checked += 1;

        for i in 0..4 {
            if gd.placements[i].is_some() {
                slots_with_player += 1;
                if let Some(s) = gd.stocks_remaining[i] {
                    slots_with_stocks += 1;
                    assert!(
                        (0..=4).contains(&s),
                        "{} placement {} stocks={} out of melee range",
                        slp.display(),
                        i,
                        s
                    );
                }
            }
        }

        // For 1v1 games we can additionally check that the winner's
        // stocks_remaining >= loser's (ties are possible but rare).
        let is_1v1 = gd.placements[0].is_some()
            && gd.placements[1].is_some()
            && gd.placements[2].is_none()
            && gd.placements[3].is_none();
        if let (true, Some(w), Some(l)) =
            (is_1v1, gd.stocks_remaining[0], gd.stocks_remaining[1])
        {
            total_1v1_comparisons += 1;
            winners_with_stocks += 1;
            losers_with_stocks += 1;
            if w >= l {
                winner_has_more_stocks_than_loser += 1;
            }
        }
    }

    assert!(checked >= 5, "need at least a handful of fixtures");
    // At least half of active slots should have stocks data (frame data present).
    assert!(
        slots_with_stocks * 2 >= slots_with_player,
        "only {}/{} populated slots had stocks_remaining",
        slots_with_stocks,
        slots_with_player
    );

    // For 1v1s with both stocks known, the winner should out-stock the loser
    // the vast majority of the time. A handful of ties (timeout wins by %)
    // are acceptable.
    if total_1v1_comparisons > 0 {
        let ratio = winner_has_more_stocks_than_loser as f64 / total_1v1_comparisons as f64;
        assert!(
            ratio >= 0.8,
            "only {}/{} ({:.0}%) 1v1 fixtures had winner stocks >= loser stocks — \
             suggests placement/port mixup in stocks extraction",
            winner_has_more_stocks_than_loser,
            total_1v1_comparisons,
            ratio * 100.0
        );
        // Sanity: suppress unused-variable lint on the per-side counters even
        // though they're only checked via ratio above.
        let _ = (winners_with_stocks, losers_with_stocks);
    }
}
