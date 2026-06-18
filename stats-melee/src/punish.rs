//! Punish / combo extractor.
//!
//! A "punish" is a run of frames where one player (the attacker) kept the
//! other (the victim) in or returning to hitstun, with only brief breaks.
//! Consumers downstream:
//! - `openings_per_kill_by_code` (Track 2 aggregates)
//! - `avg_punish_length_by_code`  (Track 2 aggregates)
//! - `most_common_kill_moves_by_code` (Track 2 aggregates)
//! - Kill-confirm stats dashboard (Track 4)
//! - Punish-tree visualization (Track 4)
//!
//! ## Algorithm
//!
//! For each (attacker, victim) direction in a 1v1 game, walk every frame and
//! track a small state machine:
//!
//! - When the victim enters hitstun, either start a fresh `RawPunish` or
//!   extend the current one. A transition from out-of-hitstun to in-hitstun
//!   while a punish is live counts as a new *hit* (increments `hit_count`).
//! - When the victim has been out of hitstun for more than
//!   [`COMBO_BREAK_FRAMES`] (~0.75 s at 60 fps, matching slippi-js's default),
//!   the current punish is finalized as a reset-to-neutral (not a kill).
//! - When the victim's stock count drops, the current punish (if any) is
//!   finalized as a kill, sampling the attacker's `last_attack_landed` at
//!   the death frame for the kill move.
//!
//! ## Known simplifications (to be iterated on)
//!
//! - Hitstun detection uses only the damage action-state range (0x4B..=0x5B),
//!   not peppi's misc bitfield. Shieldstun / grab windows aren't recognised
//!   as "advantage".
//! - Attribution is trivial (1v1). Moving to 2v2 will need
//!   `post.last_hit_by`.
//! - We track `hit_count`, not `damage_dealt`. Percent tracking lands in a
//!   follow-up that extends the schema with starting/ending percent fields.

use anyhow::{anyhow, Result};
use peppi::game::immutable::Game;

use crate::combat::is_in_hitstun;

/// Frames the victim can spend out of hitstun before we consider the punish
/// over. Mirrors slippi-js's default of 45 frames (0.75 s at 60 fps).
pub const COMBO_BREAK_FRAMES: i32 = 45;

/// One punish in attacker→victim direction, expressed in terms of peppi port
/// indices (not player codes) so the pure extractor doesn't need DB context.
#[derive(Debug, Clone, PartialEq)]
pub struct RawPunish {
    /// `game.frames.ports[attacker_port_idx]` was the punishing player.
    pub attacker_port_idx: usize,
    /// `game.frames.ports[victim_port_idx]` was the punished player.
    pub victim_port_idx: usize,
    /// Inclusive bound; 0-based frame index (not peppi's signed frame id).
    pub start_frame: i32,
    /// Inclusive bound; `end_frame >= start_frame` always.
    pub end_frame: i32,
    /// Discrete hits in the punish. Always >= 1.
    pub hit_count: i32,
    pub did_kill: bool,
    /// Slippi attack id of the attacker's `last_attack_landed` at the
    /// victim's death frame. `None` for non-kill punishes and for kill
    /// punishes where peppi couldn't read the id (e.g. old replay version).
    pub kill_move: Option<i32>,
}

/// Extract every punish from a 1v1 game in both directions, sorted by
/// `start_frame` ascending.
pub fn extract_punishes_1v1(game: &Game) -> Result<Vec<RawPunish>> {
    let populated_idx: Vec<usize> = (0..game.frames.ports.len())
        .filter(|&i| game.frames.ports[i].leader.post.state.len() > 0)
        .collect();
    if populated_idx.len() < 2 {
        return Err(anyhow!(
            "need >= 2 populated ports, got {}",
            populated_idx.len()
        ));
    }
    if populated_idx.len() > 2 {
        return Err(anyhow!(
            "2v2 / FFA not yet supported ({} ports)",
            populated_idx.len()
        ));
    }

    let a = populated_idx[0];
    let b = populated_idx[1];

    let mut out = Vec::new();
    out.extend(extract_direction(game, a, b)?);
    out.extend(extract_direction(game, b, a)?);
    out.sort_by_key(|p| p.start_frame);
    Ok(out)
}

fn extract_direction(
    game: &Game,
    attacker_idx: usize,
    victim_idx: usize,
) -> Result<Vec<RawPunish>> {
    let victim_post = &game.frames.ports[victim_idx].leader.post;
    let n = victim_post.state.len();
    if n == 0 {
        return Ok(Vec::new());
    }

    let mut out = Vec::new();
    let mut current: Option<RawPunish> = None;
    let mut was_in_hitstun = false;
    let mut frames_since_hitstun: i32 = 0;
    let mut prev_stocks: Option<u8> = None;

    for i in 0..n {
        let state = victim_post.state.get(i).unwrap_or(0);
        let victim_in_hitstun = is_in_hitstun(state);
        let stocks_now = victim_post.stocks.get(i);

        let stock_dropped = match (prev_stocks, stocks_now) {
            (Some(prev), Some(cur)) => cur < prev,
            _ => false,
        };

        if victim_in_hitstun {
            match current.as_mut() {
                None => {
                    current = Some(RawPunish {
                        attacker_port_idx: attacker_idx,
                        victim_port_idx: victim_idx,
                        start_frame: i as i32,
                        end_frame: i as i32,
                        hit_count: 1,
                        did_kill: false,
                        kill_move: None,
                    });
                }
                Some(p) => {
                    // Transition *into* hitstun from neutral = another hit
                    // landed. Staying in hitstun from the previous frame is
                    // just the same hit continuing.
                    if !was_in_hitstun {
                        p.hit_count += 1;
                    }
                    p.end_frame = i as i32;
                }
            }
            frames_since_hitstun = 0;
        } else if current.is_some() {
            frames_since_hitstun += 1;
            if frames_since_hitstun > COMBO_BREAK_FRAMES {
                // Reset to neutral — finalize as non-kill.
                out.push(current.take().unwrap());
            }
        }

        if stock_dropped {
            // Victim died. If a punish is live, this is a kill punish —
            // sample the attacker's last_attack_landed at the death frame.
            if let Some(mut p) = current.take() {
                p.did_kill = true;
                p.end_frame = i as i32;
                p.kill_move = attacker_last_attack_at(game, attacker_idx, i);
                out.push(p);
            }
        }

        was_in_hitstun = victim_in_hitstun;
        if stocks_now.is_some() {
            prev_stocks = stocks_now;
        }
    }

    // End of game — whatever's still live is a non-kill punish.
    if let Some(p) = current {
        out.push(p);
    }

    Ok(out)
}

/// Sample the attacker's `last_attack_landed` at a given frame.
/// Returns `None` if the frame index is out of bounds or the value is null at
/// that frame. (In peppi 2.x this column is always present — unlike e.g.
/// `l_cancel` which is Option-wrapped because it was added in Slippi v2.0 —
/// so we index directly and let `.get` handle nulls + OOB.)
fn attacker_last_attack_at(game: &Game, attacker_idx: usize, frame_idx: usize) -> Option<i32> {
    let post = &game.frames.ports[attacker_idx].leader.post;
    post.last_attack_landed.get(frame_idx).map(|v| v as i32)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper: run a hand-built hitstun trace through the inner loop logic.
    /// Each entry in `trace` is `(victim_in_hitstun, victim_stocks)`.
    ///
    /// This is a test-only reimplementation of the core loop so we can unit-
    /// test the state machine without constructing a full peppi Game — the
    /// real `extract_direction` requires borrowing into arrow arrays, which
    /// is hostile to synthetic input.
    fn run_trace(trace: &[(bool, u8)]) -> Vec<RawPunish> {
        let mut out = Vec::new();
        let mut current: Option<RawPunish> = None;
        let mut was_in_hitstun = false;
        let mut frames_since_hitstun: i32 = 0;
        let mut prev_stocks: Option<u8> = None;

        for (i, &(in_hs, stocks)) in trace.iter().enumerate() {
            let stock_dropped = match prev_stocks {
                Some(prev) => stocks < prev,
                None => false,
            };

            if in_hs {
                match current.as_mut() {
                    None => {
                        current = Some(RawPunish {
                            attacker_port_idx: 0,
                            victim_port_idx: 1,
                            start_frame: i as i32,
                            end_frame: i as i32,
                            hit_count: 1,
                            did_kill: false,
                            kill_move: None,
                        });
                    }
                    Some(p) => {
                        if !was_in_hitstun {
                            p.hit_count += 1;
                        }
                        p.end_frame = i as i32;
                    }
                }
                frames_since_hitstun = 0;
            } else if current.is_some() {
                frames_since_hitstun += 1;
                if frames_since_hitstun > COMBO_BREAK_FRAMES {
                    out.push(current.take().unwrap());
                }
            }

            if stock_dropped {
                if let Some(mut p) = current.take() {
                    p.did_kill = true;
                    p.end_frame = i as i32;
                    // synthetic trace doesn't provide a kill move
                    out.push(p);
                }
            }

            was_in_hitstun = in_hs;
            prev_stocks = Some(stocks);
        }

        if let Some(p) = current {
            out.push(p);
        }
        out
    }

    #[test]
    fn no_hitstun_yields_no_punishes() {
        let trace: Vec<(bool, u8)> = (0..100).map(|_| (false, 4)).collect();
        assert!(run_trace(&trace).is_empty());
    }

    #[test]
    fn single_hit_then_reset_produces_one_punish() {
        // 1 frame of hitstun, then 60 frames of neutral (> COMBO_BREAK_FRAMES).
        let mut trace = vec![(true, 4)];
        trace.extend((0..60).map(|_| (false, 4)));
        let punishes = run_trace(&trace);
        assert_eq!(punishes.len(), 1);
        let p = &punishes[0];
        assert_eq!(p.hit_count, 1);
        assert!(!p.did_kill);
        assert_eq!(p.start_frame, 0);
        assert_eq!(p.end_frame, 0);
    }

    #[test]
    fn two_hits_within_threshold_are_one_punish_with_hit_count_2() {
        // hit → 10 frames neutral (below threshold) → hit → reset
        let mut trace = vec![(true, 4)];
        trace.extend((0..10).map(|_| (false, 4)));
        trace.push((true, 4));
        trace.extend((0..60).map(|_| (false, 4)));
        let punishes = run_trace(&trace);
        assert_eq!(punishes.len(), 1);
        assert_eq!(punishes[0].hit_count, 2);
        assert!(!punishes[0].did_kill);
    }

    #[test]
    fn gap_over_threshold_splits_into_two_punishes() {
        // hit → 50 frames neutral (over threshold) → hit
        let mut trace = vec![(true, 4)];
        trace.extend((0..50).map(|_| (false, 4)));
        trace.push((true, 4));
        trace.extend((0..60).map(|_| (false, 4)));
        let punishes = run_trace(&trace);
        assert_eq!(punishes.len(), 2);
        assert_eq!(punishes[0].hit_count, 1);
        assert_eq!(punishes[1].hit_count, 1);
        assert!(!punishes[0].did_kill);
        assert!(!punishes[1].did_kill);
    }

    #[test]
    fn stock_drop_during_hitstun_is_kill_punish() {
        // 3 hits, then stock drops mid-hitstun.
        let trace = vec![
            (true, 4),
            (false, 4),
            (true, 4),
            (false, 4),
            (true, 4),
            (true, 3), // stock drop while still in hitstun — kill
        ];
        let punishes = run_trace(&trace);
        assert_eq!(punishes.len(), 1);
        let p = &punishes[0];
        assert!(p.did_kill);
        assert_eq!(p.hit_count, 3);
        assert_eq!(p.start_frame, 0);
        assert_eq!(p.end_frame, 5);
    }

    #[test]
    fn stock_drop_without_active_punish_produces_nothing() {
        // Victim dies without being in hitstun beforehand (e.g., self-destruct).
        let trace = vec![
            (false, 4),
            (false, 4),
            (false, 3), // stock drop, no active punish
            (false, 3),
        ];
        let punishes = run_trace(&trace);
        assert!(punishes.is_empty());
    }

    #[test]
    fn game_ending_with_live_punish_finalizes_as_non_kill() {
        // Victim still in hitstun when game ends — not a kill, but we should
        // record the punish anyway.
        let trace: Vec<(bool, u8)> = (0..5).map(|_| (true, 4)).collect();
        let punishes = run_trace(&trace);
        assert_eq!(punishes.len(), 1);
        assert!(!punishes[0].did_kill);
        assert_eq!(punishes[0].hit_count, 1); // no transitions, one entry
    }
}
