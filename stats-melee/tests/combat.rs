//! Integration test for the combat-state detector against real fixtures.

use peppi::io::slippi;
use std::{fs, io};

use stats_melee::combat::{compute_combat_states_1v1, CombatState, CombatSummary};
use stats_melee::testing::fixture_slps;

/// Re-parse the .slp as a peppi Game (rather than going through GameData) so
/// the combat detector can see raw frame data.
fn parse_peppi(path: &std::path::Path) -> peppi::game::immutable::Game {
    let mut r = io::BufReader::new(fs::File::open(path).expect("open"));
    slippi::read(&mut r, None).expect("peppi parse")
}

#[test]
fn combat_states_cover_every_frame() {
    let slps = fixture_slps().expect("fixtures");
    assert!(slps.len() >= 5, "need >=5 fixtures");

    let mut checked = 0;
    for slp in slps.iter().take(10) {
        let game = parse_peppi(slp);

        // Only try the 1v1 detector on fixtures that actually have 2
        // populated ports — 2v2 fixtures will error by design.
        let populated = game
            .frames
            .ports
            .iter()
            .filter(|p| p.leader.post.state.len() > 0)
            .count();
        if populated != 2 {
            continue;
        }

        let states = compute_combat_states_1v1(&game).expect("detector should succeed");
        assert_eq!(
            states.len(),
            game.frames.len(),
            "{} : state vec length should equal frame count",
            slp.display()
        );
        checked += 1;
    }
    assert!(checked >= 3, "should have tested >= 3 1v1 fixtures");
}

#[test]
fn combat_summary_proportions_are_reasonable() {
    // Sanity canary: across a batch of real games, the overall `Neutral`
    // fraction should be below 99%. If the classifier silently returned
    // Neutral for every frame (e.g. due to a peppi API mismatch), this would
    // pin it at 1.0 and we'd catch it here.
    let slps = fixture_slps().expect("fixtures");

    let mut all_states: Vec<CombatState> = Vec::new();
    for slp in slps.iter().take(10) {
        let game = parse_peppi(slp);
        let populated = game
            .frames
            .ports
            .iter()
            .filter(|p| p.leader.post.state.len() > 0)
            .count();
        if populated != 2 {
            continue;
        }
        if let Ok(states) = compute_combat_states_1v1(&game) {
            all_states.extend(states);
        }
    }

    // Guard: need a meaningful sample size before we draw conclusions.
    if all_states.len() < 1000 {
        eprintln!(
            "combat_summary_proportions_are_reasonable: only {} frames — \
             skipping proportion check",
            all_states.len()
        );
        return;
    }

    let summary = CombatSummary::from_states(&all_states);
    assert!(
        summary.neutral < 0.99,
        "suspiciously high neutral fraction ({:.4}) — detector may be stuck",
        summary.neutral
    );
    // And some non-zero fraction of frames should have *someone* in hitstun.
    let in_combat = summary.adv_p1 + summary.adv_p2 + summary.trade;
    assert!(
        in_combat > 0.005,
        "only {:.4} of frames show anyone in hitstun — unexpected for a real replay corpus",
        in_combat
    );
}
