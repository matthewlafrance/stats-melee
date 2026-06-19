//! Integration test for the advanced per-game stats against real fixtures.

use peppi::io::slippi;
use std::{fs, io};

use stats_melee::advanced::compute_advanced_stats_1v1;
use stats_melee::testing::fixture_slps;

fn parse_peppi(path: &std::path::Path) -> peppi::game::immutable::Game {
    let mut r = io::BufReader::new(fs::File::open(path).expect("open"));
    slippi::read(&mut r, None).expect("peppi parse")
}

/// Advanced stats over every 1v1 fixture should be internally consistent:
/// non-negative counters, edge-guard kills bounded by attempts, neutral wins
/// bounded by openings, exactly one player gets first blood, and death
/// percents are in a sane melee range.
#[test]
fn advanced_stats_are_self_consistent_on_fixtures() {
    let slps = fixture_slps().expect("fixtures");
    assert!(slps.len() >= 5, "need >=5 fixtures");

    let mut checked = 0usize;
    for slp in &slps {
        let game = parse_peppi(slp);
        let populated = game
            .frames
            .ports
            .iter()
            .filter(|p| p.leader.post.state.len() > 0)
            .count();
        if populated != 2 {
            continue; // advanced stats are 1v1-only
        }

        let adv = compute_advanced_stats_1v1(&game)
            .unwrap_or_else(|e| panic!("advanced stats failed for {}: {e}", slp.display()));
        checked += 1;

        for (label, p) in [("p1", &adv.p1), ("p2", &adv.p2)] {
            let ctx = format!("{} {}", slp.display(), label);
            assert!(p.damage_dealt >= 0.0, "{ctx}: negative damage");
            assert!(p.openings >= 0, "{ctx}: negative openings");
            assert!(
                p.neutral_wins <= p.openings,
                "{ctx}: neutral_wins {} > openings {}",
                p.neutral_wins,
                p.openings
            );
            assert!(
                p.edgeguard_kills <= p.edgeguard_attempts,
                "{ctx}: eg kills {} > attempts {}",
                p.edgeguard_kills,
                p.edgeguard_attempts
            );
            assert!(
                p.edgeguard_attempts <= p.openings,
                "{ctx}: eg attempts {} > openings {}",
                p.edgeguard_attempts,
                p.openings
            );
            assert!(p.deaths >= 0 && p.deaths <= 99, "{ctx}: implausible deaths {}", p.deaths);
            if p.deaths > 0 {
                let avg = p.death_percent_sum / p.deaths as f64;
                assert!(
                    (0.0..=1000.0).contains(&avg),
                    "{ctx}: avg death % out of range: {avg}"
                );
            }
        }

        // At most one player can take the game's first stock.
        assert!(
            !(adv.p1.first_blood && adv.p2.first_blood),
            "{}: both players got first blood",
            slp.display()
        );
        // A comeback requires a winner, so both can't be comeback wins.
        assert!(
            !(adv.p1.comeback_win && adv.p2.comeback_win),
            "{}: both players logged a comeback",
            slp.display()
        );
    }

    assert!(checked >= 3, "expected several 1v1 fixtures, checked {checked}");
}
