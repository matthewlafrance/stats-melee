//! Punish / combo extractor.
//!
//! A "punish" is a run of frames where one player (the attacker) kept the
//! other (the victim) in or returning to hitstun, with only brief breaks.
//! Punishes back the per-code aggregates: `openings_per_kill_by_code`,
//! `avg_punish_length_by_code`, and `most_common_kill_moves_by_code`.
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
//! - When the victim's stock count drops, we attribute the death to this
//!   attacker when the victim was *last hit by* them (peppi's `last_hit_by`
//!   port) within a recency window — see [`KILL_ATTRIBUTION_FRAMES`]. This
//!   catches deaths that land *after* the combo's hitstun already ended:
//!   edgeguards, gimps, and late blast-zone deaths where the victim was
//!   knocked off, left hitstun, then died seconds later. The older logic
//!   only credited a kill when a punish was still "live" (victim ≤ 45
//!   frames out of hitstun) at the death frame, which missed most
//!   edgeguard kills. The kill move is still sampled from the attacker's
//!   `last_attack_landed` at the death frame.
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

/// How long after our last hit on the victim a death still counts as our
/// kill (300 frames = 5 s at 60 fps). This is deliberately much larger
/// than [`COMBO_BREAK_FRAMES`]: a punish "ends" for combo-counting after
/// 0.75 s of neutral, but the *kill* it set up can land far later — an
/// off-stage hit that leads to a failed recovery, an edgeguard, a gimp.
///
/// We AND this window with the victim's `last_hit_by` matching the
/// attacker so that a self-destruct long after the last exchange isn't
/// mis-credited: `last_hit_by` isn't reliably cleared between stocks, so
/// the recency window is what rules out stale attributions.
pub const KILL_ATTRIBUTION_FRAMES: i32 = 300;

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

    // Slippi port number of the attacker, compared against the victim's
    // `last_hit_by` to decide whether a death is this attacker's kill.
    let attacker_port = u8::from(game.frames.ports[attacker_idx].port);

    let mut out = Vec::new();
    let mut current: Option<RawPunish> = None;
    let mut was_in_hitstun = false;
    let mut frames_since_hitstun: i32 = 0;
    let mut prev_stocks: Option<u8> = None;
    // Frame of the victim's most recent hitstun caused by this attacker.
    // Anchors the kill-attribution recency window for deaths that land
    // after the combo's hitstun ended. Reset to `None` on each death so a
    // fresh stock starts clean.
    let mut last_hitstun_frame: Option<i32> = None;

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
            last_hitstun_frame = Some(i as i32);
        } else if current.is_some() {
            frames_since_hitstun += 1;
            if frames_since_hitstun > COMBO_BREAK_FRAMES {
                // Reset to neutral — finalize as non-kill (it may still be
                // upgraded to a kill below if the victim dies shortly after).
                out.push(current.take().unwrap());
            }
        }

        if stock_dropped {
            // Attribute the death to this attacker when the victim was last
            // hit by them, and that hit was recent enough to plausibly have
            // led to the death. The recency window is what separates a real
            // kill (including delayed edgeguard/gimp deaths, where the combo
            // already finalized) from a self-destruct long after the last
            // exchange — `last_hit_by` alone can be stale.
            let last_hit_by = victim_last_hit_by_at(game, victim_idx, i);
            let recent = last_hitstun_frame
                .is_some_and(|f| (i as i32) - f <= KILL_ATTRIBUTION_FRAMES);
            let killed_by_attacker = last_hit_by == Some(attacker_port) && recent;

            if killed_by_attacker {
                let kill_move = attacker_last_attack_at(game, attacker_idx, i);
                if let Some(mut p) = current.take() {
                    // Live punish at the death frame — the classic
                    // combo-into-kill.
                    p.did_kill = true;
                    p.end_frame = i as i32;
                    p.kill_move = kill_move;
                    out.push(p);
                } else if let Some(last) = out.last_mut() {
                    // The combo's hitstun already ended (edgeguard / gimp /
                    // late death). Upgrade the most recent punish — the
                    // sequence that set up the kill — rather than dropping it.
                    last.did_kill = true;
                    last.end_frame = i as i32;
                    last.kill_move = kill_move;
                } else {
                    // Defensive: we registered a hit (last_hitstun_frame is
                    // Some) but have no punish object to attach to. Synthesize
                    // a minimal kill so the stock is still credited.
                    out.push(RawPunish {
                        attacker_port_idx: attacker_idx,
                        victim_port_idx: victim_idx,
                        start_frame: last_hitstun_frame.unwrap_or(i as i32),
                        end_frame: i as i32,
                        hit_count: 1,
                        did_kill: true,
                        kill_move,
                    });
                }
            } else if let Some(p) = current.take() {
                // Death not attributable to this attacker (self-destruct /
                // timeout / killed by someone else): finalize any live
                // punish as a non-kill.
                out.push(p);
            }

            // Victim respawns after a death; drop the recency anchor so the
            // next stock can't borrow this stock's last hit.
            last_hitstun_frame = None;
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

/// Sample the *victim's* `last_hit_by` (the Slippi port of whoever last
/// damaged them) at a given frame. Used at a death frame to decide which
/// player's kill the stock loss was. Returns `None` if the frame index is
/// out of bounds; the Slippi "nobody" sentinel comes back as `Some(6)` (or
/// whatever the raw value is) and simply won't match any real port.
fn victim_last_hit_by_at(game: &Game, victim_idx: usize, frame_idx: usize) -> Option<u8> {
    let post = &game.frames.ports[victim_idx].leader.post;
    post.last_hit_by.get(frame_idx)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The attacker's Slippi port in the synthetic traces below.
    const ATK: u8 = 0;
    /// Slippi "nobody hit me" sentinel for `last_hit_by`.
    const NONE: u8 = 6;

    /// Helper: run a hand-built trace through a faithful reimplementation of
    /// [`extract_direction`]'s inner loop. Each entry is
    /// `(victim_in_hitstun, victim_stocks, victim_last_hit_by)`.
    ///
    /// This is a test-only reimplementation so we can unit-test the state
    /// machine without constructing a full peppi Game — the real
    /// `extract_direction` borrows into arrow arrays, which is hostile to
    /// synthetic input. The attacker is always port [`ATK`].
    fn run_trace(trace: &[(bool, u8, u8)]) -> Vec<RawPunish> {
        let attacker_port = ATK;
        let mut out = Vec::new();
        let mut current: Option<RawPunish> = None;
        let mut was_in_hitstun = false;
        let mut frames_since_hitstun: i32 = 0;
        let mut prev_stocks: Option<u8> = None;
        let mut last_hitstun_frame: Option<i32> = None;

        for (i, &(in_hs, stocks, last_hit_by)) in trace.iter().enumerate() {
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
                last_hitstun_frame = Some(i as i32);
            } else if current.is_some() {
                frames_since_hitstun += 1;
                if frames_since_hitstun > COMBO_BREAK_FRAMES {
                    out.push(current.take().unwrap());
                }
            }

            if stock_dropped {
                let recent = last_hitstun_frame
                    .is_some_and(|f| (i as i32) - f <= KILL_ATTRIBUTION_FRAMES);
                let killed_by_attacker = last_hit_by == attacker_port && recent;

                if killed_by_attacker {
                    if let Some(mut p) = current.take() {
                        p.did_kill = true;
                        p.end_frame = i as i32;
                        out.push(p);
                    } else if let Some(last) = out.last_mut() {
                        last.did_kill = true;
                        last.end_frame = i as i32;
                    } else {
                        out.push(RawPunish {
                            attacker_port_idx: 0,
                            victim_port_idx: 1,
                            start_frame: last_hitstun_frame.unwrap_or(i as i32),
                            end_frame: i as i32,
                            hit_count: 1,
                            did_kill: true,
                            kill_move: None,
                        });
                    }
                } else if let Some(p) = current.take() {
                    out.push(p);
                }

                last_hitstun_frame = None;
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
        let trace: Vec<(bool, u8, u8)> = (0..100).map(|_| (false, 4, NONE)).collect();
        assert!(run_trace(&trace).is_empty());
    }

    #[test]
    fn single_hit_then_reset_produces_one_punish() {
        // 1 frame of hitstun, then 60 frames of neutral (> COMBO_BREAK_FRAMES).
        let mut trace = vec![(true, 4, ATK)];
        trace.extend((0..60).map(|_| (false, 4, ATK)));
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
        let mut trace = vec![(true, 4, ATK)];
        trace.extend((0..10).map(|_| (false, 4, ATK)));
        trace.push((true, 4, ATK));
        trace.extend((0..60).map(|_| (false, 4, ATK)));
        let punishes = run_trace(&trace);
        assert_eq!(punishes.len(), 1);
        assert_eq!(punishes[0].hit_count, 2);
        assert!(!punishes[0].did_kill);
    }

    #[test]
    fn gap_over_threshold_splits_into_two_punishes() {
        // hit → 50 frames neutral (over threshold) → hit
        let mut trace = vec![(true, 4, ATK)];
        trace.extend((0..50).map(|_| (false, 4, ATK)));
        trace.push((true, 4, ATK));
        trace.extend((0..60).map(|_| (false, 4, ATK)));
        let punishes = run_trace(&trace);
        assert_eq!(punishes.len(), 2);
        assert_eq!(punishes[0].hit_count, 1);
        assert_eq!(punishes[1].hit_count, 1);
        assert!(!punishes[0].did_kill);
        assert!(!punishes[1].did_kill);
    }

    #[test]
    fn stock_drop_during_hitstun_is_kill_punish() {
        // 3 hits, then stock drops mid-hitstun, last hit by the attacker.
        let trace = vec![
            (true, 4, ATK),
            (false, 4, ATK),
            (true, 4, ATK),
            (false, 4, ATK),
            (true, 4, ATK),
            (true, 3, ATK), // stock drop while still in hitstun — kill
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
        // `last_hit_by` is the "nobody" sentinel, so it's not our kill.
        let trace = vec![
            (false, 4, NONE),
            (false, 4, NONE),
            (false, 3, NONE), // stock drop, no active punish, self-destruct
            (false, 3, NONE),
        ];
        let punishes = run_trace(&trace);
        assert!(punishes.is_empty());
    }

    #[test]
    fn edgeguard_death_after_hitstun_ends_is_kill() {
        // Hit once (knocked off-stage), leave hitstun long enough that the
        // combo finalizes as a non-kill, then die ~2 s later still flagged
        // as last-hit-by the attacker. The finalized punish should be
        // upgraded to a kill.
        let mut trace = vec![(true, 4, ATK)];
        // 60 frames of neutral → punish finalizes (> COMBO_BREAK_FRAMES).
        trace.extend((0..60).map(|_| (false, 4, ATK)));
        // ~1 s more of falling, then the stock drops, still last-hit-by us.
        trace.extend((0..60).map(|_| (false, 4, ATK)));
        trace.push((false, 3, ATK)); // death, within KILL_ATTRIBUTION_FRAMES
        let punishes = run_trace(&trace);
        assert_eq!(punishes.len(), 1);
        assert!(
            punishes[0].did_kill,
            "an edgeguard death after hitstun ended should still be our kill"
        );
        assert_eq!(punishes[0].hit_count, 1);
    }

    #[test]
    fn late_death_outside_window_is_not_our_kill() {
        // Hit once, then the victim survives for well over
        // KILL_ATTRIBUTION_FRAMES before dying. Even though `last_hit_by`
        // is stale-set to us, the recency window rules it out.
        let mut trace = vec![(true, 4, ATK)];
        trace.extend((0..(KILL_ATTRIBUTION_FRAMES as usize + 60)).map(|_| (false, 4, ATK)));
        trace.push((false, 3, ATK)); // death far after the last hit
        let punishes = run_trace(&trace);
        assert_eq!(punishes.len(), 1);
        assert!(
            !punishes[0].did_kill,
            "a death long after our last hit is a self-destruct, not our kill"
        );
    }

    #[test]
    fn death_attributed_to_other_port_is_not_our_kill() {
        // Live combo, but the death frame reports the victim was last hit by
        // a different port — not our kill. (Wouldn't happen in a real 1v1,
        // but the attribution must be port-correct.)
        let trace = vec![
            (true, 4, ATK),
            (true, 4, ATK),
            (true, 3, 1), // stock drop, last hit by port 1, not us
        ];
        let punishes = run_trace(&trace);
        assert_eq!(punishes.len(), 1);
        assert!(!punishes[0].did_kill);
    }

    #[test]
    fn game_ending_with_live_punish_finalizes_as_non_kill() {
        // Victim still in hitstun when game ends — not a kill, but we should
        // record the punish anyway.
        let trace: Vec<(bool, u8, u8)> = (0..5).map(|_| (true, 4, ATK)).collect();
        let punishes = run_trace(&trace);
        assert_eq!(punishes.len(), 1);
        assert!(!punishes[0].did_kill);
        assert_eq!(punishes[0].hit_count, 1); // no transitions, one entry
    }
}
