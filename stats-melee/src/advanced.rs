//! Advanced per-game combat metrics for 1v1 games.
//!
//! One frame walk per game produces a [`PlayerAdvancedStats`] for each player
//! by reusing the combat-state classifier ([`crate::combat`]) and the punish
//! extractor ([`crate::punish`]) plus the raw per-frame percent / stocks /
//! position columns. The results are *raw counters* — the UI and the
//! aggregate queries turn them into the user-facing ratios (damage per
//! opening, neutral-win %, stage control %, edge-guard %, first-blood win %,
//! comeback rate, average death %).
//!
//! These counters are persisted per `(game, player)` at ingest (see the
//! `game_player_stat` advanced columns), so the per-match view, the Analytics
//! aggregates, and the time-graphs all read the same numbers without
//! re-parsing the `.slp`.
//!
//! Everything here is **1v1-only** and best-effort: the heuristic metrics
//! (neutral win, edge-guard, comeback, stage control) are approximations, not
//! frame-perfect truth, and degrade to "no data" on non-1v1 games (the
//! reused extractors error out, which the caller maps to "skip").

use anyhow::Result;
use peppi::frame::immutable::Post;
use peppi::game::immutable::Game;

use crate::combat::{compute_analysis_1v1, CombatState};
use crate::punish::extract_punishes_1v1;
use crate::stage_bounds::{stage_bounds, StageBounds};

/// One player's raw advanced counters for a single 1v1 game. Ratios are
/// derived downstream (some need the opponent's counters too — e.g. neutral
/// win % and stage control %).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PlayerAdvancedStats {
    /// Total percent dealt to the opponent — the sum of the opponent's
    /// positive percent deltas during this player's punishes, which absorbs
    /// the death-frame reset (a reset is a negative delta, ignored).
    pub damage_dealt: f64,
    /// Conversions started (punishes initiated by this player). Denominator
    /// for damage-per-opening.
    pub openings: i32,
    /// Openings that began from neutral — the frame before the punish the
    /// combat state was [`CombatState::Neutral`] (not a combo continuation or
    /// a counter-hit).
    pub neutral_wins: i32,
    /// Frames this player held the advantage per the combat-state classifier.
    /// Stage control % = `adv_frames / (adv_frames + opp.adv_frames)`.
    pub adv_frames: i32,
    /// Punishes where the opponent was offstage at some point — edge-guard
    /// attempts. Denominator for edge-guard success.
    pub edgeguard_attempts: i32,
    /// Edge-guard attempts that killed while the opponent was offstage.
    pub edgeguard_kills: i32,
    /// Whether this player scored the game's first kill.
    pub first_blood: bool,
    /// Times this player lost a stock.
    pub deaths: i32,
    /// Sum of the percents this player died at (pairs with `deaths` for an
    /// average death percent).
    pub death_percent_sum: f64,
    /// Whether this player won the game after trailing by >= 2 stocks at some
    /// point. Numerator for comeback rate (denominator = wins).
    pub comeback_win: bool,
}

impl PlayerAdvancedStats {
    fn zeroed() -> Self {
        Self {
            damage_dealt: 0.0,
            openings: 0,
            neutral_wins: 0,
            adv_frames: 0,
            edgeguard_attempts: 0,
            edgeguard_kills: 0,
            first_blood: false,
            deaths: 0,
            death_percent_sum: 0.0,
            comeback_win: false,
        }
    }
}

/// Both players' advanced stats for one 1v1 game, tagged with the peppi port
/// indices so the caller can line them up with player codes (`p1` is the
/// lower port, `p2` the higher — same convention as [`crate::combat`]).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AdvancedStats {
    pub p1: PlayerAdvancedStats,
    pub p2: PlayerAdvancedStats,
    pub p1_port_idx: usize,
    pub p2_port_idx: usize,
}

/// True if `post`'s position at frame `i` is outside the stage's main-platform
/// bounds. `None` bounds (stage outside the legal pool) or a short position
/// column degrade to "on stage".
fn frame_offstage(bounds: Option<StageBounds>, post: &Post, i: usize) -> bool {
    bounds
        .and_then(|b| {
            let x = post.position.x.get(i)?;
            let y = post.position.y.get(i)?;
            Some(b.is_offstage(x, y))
        })
        .unwrap_or(false)
}

/// Walk a 1v1 game once and roll up both players' advanced stats. Returns
/// `Err` for non-1v1 games (the reused extractors require exactly two
/// populated ports).
pub fn compute_advanced_stats_1v1(game: &Game) -> Result<AdvancedStats> {
    let analysis = compute_analysis_1v1(game)?;
    let punishes = extract_punishes_1v1(game)?;

    let i1 = analysis.p1_port_idx;
    let i2 = analysis.p2_port_idx;
    let combat = &analysis.combat;
    let n = combat.len();

    let post1 = &game.frames.ports[i1].leader.post;
    let post2 = &game.frames.ports[i2].leader.post;
    let bounds = stage_bounds(game.start.stage as i32);

    // Per-frame columns we index repeatedly — materialize once so the punish
    // and death loops below are simple slice reads.
    let percent1: Vec<f32> = (0..n).map(|i| post1.percent.get(i).unwrap_or(0.0)).collect();
    let percent2: Vec<f32> = (0..n).map(|i| post2.percent.get(i).unwrap_or(0.0)).collect();
    let offstage1: Vec<bool> = (0..n).map(|i| frame_offstage(bounds, post1, i)).collect();
    let offstage2: Vec<bool> = (0..n).map(|i| frame_offstage(bounds, post2, i)).collect();
    let stocks1: Vec<Option<u8>> = (0..n).map(|i| post1.stocks.get(i)).collect();
    let stocks2: Vec<Option<u8>> = (0..n).map(|i| post2.stocks.get(i)).collect();

    let mut p1 = PlayerAdvancedStats::zeroed();
    let mut p2 = PlayerAdvancedStats::zeroed();

    // --- Advantage time (stage control) ------------------------------------
    for &c in combat {
        match c {
            CombatState::AdvP1 => p1.adv_frames += 1,
            CombatState::AdvP2 => p2.adv_frames += 1,
            _ => {}
        }
    }

    // --- Per-punish: openings, damage, neutral wins, edge-guards -----------
    for pun in &punishes {
        let attacker_is_p1 = pun.attacker_port_idx == i1;
        let (victim_percent, victim_offstage) = if pun.victim_port_idx == i1 {
            (&percent1, &offstage1)
        } else {
            (&percent2, &offstage2)
        };

        let start = pun.start_frame.max(0) as usize;
        let end = (pun.end_frame.max(0) as usize).min(n.saturating_sub(1));

        // Damage = sum of the victim's positive percent deltas across the
        // punish window (negative deltas = stock-reset, ignored).
        let mut damage = 0.0_f64;
        for f in (start + 1)..=end {
            let d = victim_percent[f] - victim_percent[f - 1];
            if d > 0.0 {
                damage += d as f64;
            }
        }

        // Neutral win: the frame before the punish, the game was in neutral
        // (a fresh exchange, not a combo continuation / counter-hit). An
        // opening on frame 0 counts as neutral.
        let neutral_win = if start == 0 {
            true
        } else {
            matches!(combat.get(start - 1), Some(CombatState::Neutral))
        };

        // Edge-guard: the victim was offstage at some point in the window; a
        // kill counts as a successful edge-guard when the victim was offstage
        // at (or just before) the death frame.
        let eg_attempt = (start..=end).any(|f| victim_offstage[f]);
        let eg_kill = pun.did_kill
            && (victim_offstage[end] || (end > 0 && victim_offstage[end - 1]));

        let who = if attacker_is_p1 { &mut p1 } else { &mut p2 };
        who.openings += 1;
        who.damage_dealt += damage;
        if neutral_win {
            who.neutral_wins += 1;
        }
        if eg_attempt {
            who.edgeguard_attempts += 1;
            if eg_kill {
                who.edgeguard_kills += 1;
            }
        }
    }

    // --- Deaths + death percents + first blood -----------------------------
    let mut first_blood_decided = false;
    for i in 1..n {
        let p1_died = matches!((stocks1[i - 1], stocks1[i]), (Some(a), Some(b)) if b < a);
        let p2_died = matches!((stocks2[i - 1], stocks2[i]), (Some(a), Some(b)) if b < a);

        if p1_died {
            p1.deaths += 1;
            p1.death_percent_sum += percent1[i - 1] as f64;
        }
        if p2_died {
            p2.deaths += 1;
            p2.death_percent_sum += percent2[i - 1] as f64;
        }

        // First blood: the first stock lost in the game hands the *other*
        // player first blood. A (rare) simultaneous death awards neither.
        if !first_blood_decided && (p1_died || p2_died) {
            first_blood_decided = true;
            if p1_died && !p2_died {
                p2.first_blood = true;
            } else if p2_died && !p1_died {
                p1.first_blood = true;
            }
        }
    }

    // --- Comeback: did the eventual winner ever trail by >= 2 stocks? -------
    let final1 = stocks1.iter().rev().find_map(|s| *s);
    let final2 = stocks2.iter().rev().find_map(|s| *s);
    if let (Some(f1), Some(f2)) = (final1, final2) {
        // Winner = whoever finished with more stocks (a 0-stock tie / timeout
        // by percent is left as "no comeback" — we don't have the result here).
        let winner = match f1.cmp(&f2) {
            std::cmp::Ordering::Greater => 1,
            std::cmp::Ordering::Less => 2,
            std::cmp::Ordering::Equal => 0,
        };
        if winner != 0 {
            for i in 0..n {
                if let (Some(a), Some(b)) = (stocks1[i], stocks2[i]) {
                    let (a, b) = (a as i32, b as i32);
                    if winner == 1 && b - a >= 2 {
                        p1.comeback_win = true;
                        break;
                    }
                    if winner == 2 && a - b >= 2 {
                        p2.comeback_win = true;
                        break;
                    }
                }
            }
        }
    }

    Ok(AdvancedStats {
        p1,
        p2,
        p1_port_idx: i1,
        p2_port_idx: i2,
    })
}
