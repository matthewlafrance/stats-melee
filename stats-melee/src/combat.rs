//! Frame-by-frame combat-state detection.
//!
//! A "combat state" classifies the momentum of a 1v1 game at a given frame:
//! are both players in neutral, is one of them being hit, or is it a trade?
//! This is the shared substrate for the scrub-bar coloring, openings-per-kill,
//! kill-source tracking, and the punish extractor.
//!
//! ## v1 vs v2
//!
//! The original classifier (Track 2j) was hitstun-only — `AdvP1` meant
//! literally "P2 is in a damage action state right now". That captured "true
//! combo in progress" perfectly but missed everything else competitive Melee
//! commentators call "advantage": the hitstun tail that follows a knockdown,
//! the off-stage edgeguard, the ledge-trap. Track 9 layers those signals on
//! top of the v1 hitstun classifier:
//!
//! 1. **Hitstun (v1)** — direct, highest priority. Someone is being hit
//!    *right now*; no other layer overrides this.
//! 2. **Hitstun-tail recency** — for `hitstun_tail_frames` (default 30 ≈ 0.5s)
//!    after exiting hitstun, the previously-attacked player stays on
//!    disadvantage. Captures the chase / followup commitment window.
//! 3. **Off-stage** — main-platform bounds from [`crate::stage_bounds`]. A
//!    player off the stage with the opponent on stage in neutral cedes
//!    advantage — this is the edgeguard scenario the recency tail can't
//!    cover (long recoveries outlast 30 frames).
//! 4. **Ledge state** — a player in any of the `CLIFF_*` action states is
//!    on the back foot; on-stage opponent gets advantage.
//! 5. **Neutral** — fall-through.
//!
//! Each layer is opt-out: stages outside the legal pool return `None` from
//! [`crate::stage_bounds::stage_bounds`] and the off-stage layer just
//! never activates. The hitstun-tail collapses to v1 behavior when
//! `hitstun_tail_frames = 0`.
//!
//! ## Reference — action-state ranges
//!
//! - Hitstun (damage block): `0x4B` (DAMAGE_HI_1) through `0x5B`
//!   (DAMAGE_FLY_ROLL) inclusive — being hit and knockback both fall here.
//! - Ledge / cliff: `0xFC` (CLIFF_CATCH) through `0x107` (CLIFF_JUMP_QUICK_2)
//!   inclusive — every state where the character is grabbing or hanging on
//!   or releasing the ledge.
//!
//! ## Future work
//!
//! These compose on top of the current layered classifier without changing
//! `CombatState`'s shape:
//!
//! - Shieldstun → advantage for the attacker
//! - Grab windows → advantage for the grabber before the first hit
//! - last_hit_by attribution to break Trade ties more carefully

use anyhow::{anyhow, Result};
use peppi::game::immutable::Game;
use serde::{Deserialize, Serialize};

use crate::stage_bounds::{stage_bounds, StageBounds};

/// Inclusive low bound of the "damage" (hitstun) action-state range.
pub const HITSTUN_STATE_MIN: u16 = 0x4B; // 75
/// Inclusive high bound of the "damage" (hitstun) action-state range.
pub const HITSTUN_STATE_MAX: u16 = 0x5B; // 91

/// Inclusive low bound of the `CLIFF_*` (ledge) action-state range.
pub const LEDGE_STATE_MIN: u16 = 0xFC; // 252 — CLIFF_CATCH
/// Inclusive high bound of the `CLIFF_*` (ledge) action-state range.
pub const LEDGE_STATE_MAX: u16 = 0x107; // 263 — CLIFF_JUMP_QUICK_2

/// Default tail (in frames at 60fps) during which a player who recently
/// exited hitstun still counts as being on disadvantage. ~0.5s — enough to
/// cover a typical platform-tech read or chase commitment, short enough that
/// a clean reset back to neutral wins doesn't get mis-attributed.
pub const HITSTUN_TAIL_DEFAULT_FRAMES: u32 = 30;

/// Per-frame combat state in a 1v1 match.
///
/// Ordering follows port index (lower-port player is `P1`), not placement
/// (winner-first) — it would be misleading to name these by placement when
/// each frame pre-dates the game result. Callers that need winner-relative
/// semantics should map `CombatState` → advantage-for-player in their own
/// reporting layer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CombatState {
    /// Neither player is taking damage.
    Neutral,
    /// P2 is in hitstun / off-stage / on the ledge / recently hit; P1 has
    /// the advantage.
    AdvP1,
    /// P1 is in hitstun / off-stage / on the ledge / recently hit; P2 has
    /// the advantage.
    AdvP2,
    /// Both players are in hitstun at the same time (mutual hit / crossup),
    /// or both exited hitstun on the exact same frame and neither has
    /// established positional advantage since.
    Trade,
}

/// True if `action_state` falls inside Melee's damage-animation block.
#[inline]
pub fn is_in_hitstun(action_state: u16) -> bool {
    (HITSTUN_STATE_MIN..=HITSTUN_STATE_MAX).contains(&action_state)
}

/// True if `action_state` is one of the `CLIFF_*` ledge states. Covers the
/// catch, hang, all four climb/attack/escape/jump variants — every state
/// where the character is mechanically tied to the ledge.
#[inline]
pub fn is_on_ledge(action_state: u16) -> bool {
    (LEDGE_STATE_MIN..=LEDGE_STATE_MAX).contains(&action_state)
}

/// v1 classifier — hitstun-only. Pure, fast, easy to unit-test. Kept around
/// both for backward compatibility (the punish extractor still uses it as
/// its core "is anyone being hit?" check) and as the inner-loop call from
/// [`classify_frame_layered`].
#[inline]
pub fn classify_frame(p1_state: u16, p2_state: u16) -> CombatState {
    match (is_in_hitstun(p1_state), is_in_hitstun(p2_state)) {
        (false, false) => CombatState::Neutral,
        (false, true) => CombatState::AdvP1,
        (true, false) => CombatState::AdvP2,
        (true, true) => CombatState::Trade,
    }
}

/// Tunable knobs for the v2 (layered) classifier. `Default` mirrors the
/// settings used in production — adjust per-call when running A/B
/// experiments on the recency window.
///
/// `Hash` matters: the analysis cache stores a `signature()` of the
/// active config alongside each cached `ReplayAnalysis` so that
/// changing a knob (e.g. bumping `hitstun_tail_frames`) auto-invalidates
/// the cache. See [`Self::signature`] and [`CachedAnalysis`] for the
/// invalidation flow.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CombatV2Config {
    /// How long, in frames, after exiting hitstun to keep the previously
    /// attacked player on disadvantage. Setting this to `0` collapses v2
    /// behavior back to v1 hitstun-only.
    pub hitstun_tail_frames: u32,
}

impl Default for CombatV2Config {
    fn default() -> Self {
        Self {
            hitstun_tail_frames: HITSTUN_TAIL_DEFAULT_FRAMES,
        }
    }
}

impl CombatV2Config {
    /// Stable hash of the config's field values, used as the
    /// invalidation key for [`CachedAnalysis`]. Bumping any field
    /// (or adding a new one and rebuilding) shifts the signature and
    /// triggers a lazy recompute on next viewer load.
    ///
    /// Implemented via `std::collections::hash_map::DefaultHasher`.
    /// That hasher's algorithm is documented as "may change at any
    /// time across Rust releases" — for our use case that's fine:
    /// a Rust upgrade quietly invalidates every sidecar (recompute
    /// once, refill the cache), which is the conservative outcome.
    /// False matches would be the dangerous direction; false
    /// invalidations are merely a tiny one-time CPU hit.
    pub fn signature(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        self.hash(&mut h);
        h.finish()
    }
}

/// Per-player, per-frame inputs to [`classify_frame_layered`]. Bundled into
/// a struct so the function signature doesn't grow into a 9-arg monster as
/// future overlays land.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlayerFrameSignals {
    /// Raw Slippi action-state id for this player on this frame.
    pub action_state: u16,
    /// True when the player is outside the main-platform bounds. Always
    /// `false` on stages where geometry is unavailable — the layer
    /// silently disables in that case.
    pub offstage: bool,
    /// True when [`is_on_ledge`] holds for this frame's action state.
    /// Redundant with `action_state` but pre-computed in the sweep loop
    /// so the pure classifier doesn't have to re-check.
    pub on_ledge: bool,
    /// `Some(n)` = exited hitstun `n` frames ago.
    /// `None` = currently in hitstun, or never been in hitstun this game.
    /// The sweep distinguishes "in hitstun now" from "never in hitstun"
    /// implicitly: if the player is currently in hitstun, the hitstun
    /// layer fires first and the recency layer is never consulted.
    pub frames_since_hitstun: Option<u32>,
}

/// v2 classifier — layered priority chain. Pure (no peppi, no DB), so the
/// truth-table tests below can exercise every overlay in isolation.
///
/// Priority, highest to lowest:
///
/// 1. Direct hitstun (someone is being hit *right now*).
/// 2. Hitstun-tail recency (someone was hit in the last
///    `config.hitstun_tail_frames`).
/// 3. Edge state — off-stage or on the ledge — for one player but not the
///    other.
/// 4. Neutral.
///
/// At each level a tie (both players match the predicate) collapses to
/// [`CombatState::Trade`] for the hitstun layer; for the recency layer the
/// tiebreak is "more recent hit wins"; ties at the edge layer fall through
/// to [`CombatState::Neutral`] (both off-stage in a double-edgeguard is
/// nobody's advantage).
#[inline]
pub fn classify_frame_layered(
    p1: &PlayerFrameSignals,
    p2: &PlayerFrameSignals,
    config: &CombatV2Config,
) -> CombatState {
    // Layer 1: current hitstun. Highest priority — we never override this
    // with a "recently was offstage" signal because being in hitstun IS the
    // ground truth of who's getting hit.
    let p1_hit = is_in_hitstun(p1.action_state);
    let p2_hit = is_in_hitstun(p2.action_state);
    match (p1_hit, p2_hit) {
        (true, true) => return CombatState::Trade,
        (true, false) => return CombatState::AdvP2,
        (false, true) => return CombatState::AdvP1,
        (false, false) => {}
    }

    // Layer 2: hitstun-tail recency. Only consulted when neither player is
    // currently in hitstun — direct hits dominate.
    let tail = config.hitstun_tail_frames;
    let p1_recent = p1
        .frames_since_hitstun
        .map(|n| n <= tail)
        .unwrap_or(false);
    let p2_recent = p2
        .frames_since_hitstun
        .map(|n| n <= tail)
        .unwrap_or(false);
    match (p1_recent, p2_recent) {
        (true, true) => {
            // Both recently hit — the more recently hit player is still
            // on the back foot. `unwrap` is safe because `p*_recent` is only
            // true when `frames_since_hitstun` was Some.
            let a = p1.frames_since_hitstun.unwrap();
            let b = p2.frames_since_hitstun.unwrap();
            return match a.cmp(&b) {
                std::cmp::Ordering::Less => CombatState::AdvP2, // p1 hit more recently
                std::cmp::Ordering::Greater => CombatState::AdvP1,
                std::cmp::Ordering::Equal => CombatState::Trade,
            };
        }
        (true, false) => return CombatState::AdvP2,
        (false, true) => return CombatState::AdvP1,
        (false, false) => {}
    }

    // Layer 3: edge state. Off-stage *or* on the ledge — both flavors of
    // "stuck on the perimeter, opponent has stage control".
    let p1_edge = p1.offstage || p1.on_ledge;
    let p2_edge = p2.offstage || p2.on_ledge;
    match (p1_edge, p2_edge) {
        (true, false) => return CombatState::AdvP2,
        (false, true) => return CombatState::AdvP1,
        // (true, true) double-edge / mutual ledge-trade — fall through.
        // (false, false) — same, fall through.
        _ => {}
    }

    CombatState::Neutral
}

/// Walk every frame of a 1v1 game and produce a per-frame `CombatState`
/// using the v1 (hitstun-only) classifier. Kept around for the punish
/// extractor and any caller that doesn't need the v2 overlays.
///
/// Returns `Err` if the game has fewer than 2 populated ports (we don't yet
/// support 2v2) or if the two populated ports have mismatched frame counts
/// (peppi should guarantee alignment, but we'd rather fail loudly than silently
/// truncate).
pub fn compute_combat_states_1v1(game: &Game) -> Result<Vec<CombatState>> {
    // Pick the first two ports that actually have frame data — in 1v1 peppi
    // leaves the other port slots empty/missing, but the *populated* indices
    // can be e.g. P1/P3 or P2/P4. Consumers don't need to care which.
    let populated: Vec<_> = game
        .frames
        .ports
        .iter()
        .filter(|p| p.leader.post.state.len() > 0)
        .collect();

    if populated.len() < 2 {
        return Err(anyhow!(
            "compute_combat_states_1v1: need >= 2 populated ports, got {}",
            populated.len()
        ));
    }
    if populated.len() > 2 {
        return Err(anyhow!(
            "compute_combat_states_1v1: 2v2 / FFA not yet supported (got {} ports)",
            populated.len()
        ));
    }

    let p1 = &populated[0].leader.post.state;
    let p2 = &populated[1].leader.post.state;

    let n = p1.len();
    if p2.len() != n {
        return Err(anyhow!(
            "port frame-count mismatch: {} vs {}",
            n,
            p2.len()
        ));
    }

    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        // Null action states (e.g. character absent on a given frame) fall back
        // to Neutral — they don't contribute to the advantage signal.
        let a = p1.get(i).unwrap_or(0);
        let b = p2.get(i).unwrap_or(0);
        out.push(classify_frame(a, b));
    }
    Ok(out)
}

/// Combined output of the 1v1 frame-data walk: combat states plus the
/// peppi port indices for each of the two players.
///
/// The port indices let the UI cross-reference against the per-game
/// `gamePlayer.port` column so labels and highlights line up with
/// the right character / connect code.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReplayAnalysis {
    pub combat: Vec<CombatState>,
    /// peppi port index for the lower-port player (e.g. 0 for P1, 1 for
    /// P2, etc.).
    pub p1_port_idx: usize,
    /// peppi port index for the higher-port player. Always `> p1_port_idx`.
    pub p2_port_idx: usize,
}

/// Schema version for [`CachedAnalysis`]. Bump this whenever the layout
/// of `CachedAnalysis` itself changes in a backward-incompatible way
/// (new required field, removed field, type change). A pure tweak to
/// `CombatV2Config`'s defaults does NOT need a version bump — the
/// `config_hash` field handles that.
///
/// On a version mismatch the cache treats the entry as missing and
/// recomputes; no migration logic.
pub const CACHED_ANALYSIS_VERSION: u32 = 1;

/// On-disk wrapper for a cached [`ReplayAnalysis`]. Stored bincode-
/// serialized in the analysis sidecar cache (Track 11), keyed on the
/// .slp content hash. The viewer's load path consults this *before*
/// re-parsing peppi.
///
/// Cache invalidation is two-pronged:
///
/// - `version` mismatch → schema drift, recompute.
/// - `config_hash` mismatch → classifier knob changed, recompute.
///
/// Both checks are fast (two int comparisons before any
/// deserialization decisions). The actual `analysis` field is the
/// payload — everything else is bookkeeping.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CachedAnalysis {
    /// Matches [`CACHED_ANALYSIS_VERSION`] at the time of write. A
    /// reader with a different `CACHED_ANALYSIS_VERSION` discards the
    /// entry.
    pub version: u32,
    /// [`CombatV2Config::signature`] at the time of write. A reader
    /// with a different config (different hitstun-tail, future knobs)
    /// discards the entry.
    pub config_hash: u64,
    /// The cached classification — what `compute_analysis_1v1` would
    /// have produced.
    pub analysis: ReplayAnalysis,
}

impl CachedAnalysis {
    /// Wrap an analysis with the current schema version + the given
    /// config's signature. Use this at write time so callers don't
    /// have to remember to set both bookkeeping fields by hand.
    pub fn new(analysis: ReplayAnalysis, config: &CombatV2Config) -> Self {
        Self {
            version: CACHED_ANALYSIS_VERSION,
            config_hash: config.signature(),
            analysis,
        }
    }

    /// True if this cached entry is still valid against the given
    /// classifier config. Used by the viewer's load path before it
    /// trusts the sidecar.
    pub fn is_fresh(&self, config: &CombatV2Config) -> bool {
        self.version == CACHED_ANALYSIS_VERSION && self.config_hash == config.signature()
    }
}

/// Walk a 1v1 game once and produce the v2 (layered) combat-state vector
/// tagged with each player's peppi port index. See [`ReplayAnalysis`].
///
/// The sweep maintains per-player "frames since hitstun ended" counters
/// across the loop (so each frame's classifier sees the right recency
/// state), looks up [`stage_bounds`] once for off-stage detection, and
/// feeds each frame's `PlayerFrameSignals` to [`classify_frame_layered`].
///
/// Same error shape as [`compute_combat_states_1v1`] — returns `Err` on
/// non-1v1 or mismatched frame counts.
pub fn compute_analysis_1v1(game: &Game) -> Result<ReplayAnalysis> {
    compute_analysis_1v1_with_config(game, &CombatV2Config::default())
}

/// Same as [`compute_analysis_1v1`] but with a caller-supplied
/// [`CombatV2Config`]. Use this in tests / experiments when you want to
/// poke at the recency window without round-tripping through `Default`.
pub fn compute_analysis_1v1_with_config(
    game: &Game,
    config: &CombatV2Config,
) -> Result<ReplayAnalysis> {
    // Build both the port-iter (for frame data) and the index-iter (for
    // the p*_port_idx fields) in one sweep.
    let populated: Vec<(usize, _)> = game
        .frames
        .ports
        .iter()
        .enumerate()
        .filter(|(_, p)| p.leader.post.state.len() > 0)
        .collect();

    if populated.len() < 2 {
        return Err(anyhow!(
            "compute_analysis_1v1: need >= 2 populated ports, got {}",
            populated.len()
        ));
    }
    if populated.len() > 2 {
        return Err(anyhow!(
            "compute_analysis_1v1: 2v2 / FFA not yet supported (got {} ports)",
            populated.len()
        ));
    }

    let (p1_port_idx, p1_port) = (populated[0].0, populated[0].1);
    let (p2_port_idx, p2_port) = (populated[1].0, populated[1].1);

    let p1_post = &p1_port.leader.post;
    let p2_post = &p2_port.leader.post;

    let n = p1_post.state.len();
    if p2_post.state.len() != n {
        return Err(anyhow!(
            "port frame-count mismatch: {} vs {}",
            n,
            p2_post.state.len()
        ));
    }

    // One stage_bounds lookup for the whole sweep — the value is `Copy` so
    // we capture an Option<StageBounds> and propagate it into each frame's
    // signals. Stages outside the legal pool simply leave the off-stage
    // layer dormant.
    let bounds: Option<StageBounds> = stage_bounds(game.start.stage as i32);

    // Per-player recency counters. `None` = currently in hitstun, OR has
    // never been hit yet this game. The classifier doesn't need to
    // distinguish those cases — both mean "the recency layer can't
    // attribute disadvantage based on a past hit".
    let mut p1_since: Option<u32> = None;
    let mut p2_since: Option<u32> = None;
    let mut p1_was_hit = false;
    let mut p2_was_hit = false;

    let mut combat = Vec::with_capacity(n);
    for i in 0..n {
        // Null action states (e.g. character absent on a given frame) fall
        // back to a neutral wait state (0x00) — out-of-band but valid as a
        // "neither in hitstun nor on a ledge" sentinel.
        let p1_state = p1_post.state.get(i).unwrap_or(0);
        let p2_state = p2_post.state.get(i).unwrap_or(0);

        let p1_hit_now = is_in_hitstun(p1_state);
        let p2_hit_now = is_in_hitstun(p2_state);

        // Update recency counters BEFORE classification so the classifier
        // sees this frame's resulting state. The state machine for each
        // counter:
        //   - in hitstun this frame              → None (block recency)
        //   - just exited hitstun this frame     → Some(0)
        //   - was already out of hitstun         → Some(prev + 1) if Some,
        //                                          or stay None if never hit
        if p1_hit_now {
            p1_since = None;
        } else if p1_was_hit {
            p1_since = Some(0);
        } else if let Some(ref mut n) = p1_since {
            *n = n.saturating_add(1);
        }
        if p2_hit_now {
            p2_since = None;
        } else if p2_was_hit {
            p2_since = Some(0);
        } else if let Some(ref mut n) = p2_since {
            *n = n.saturating_add(1);
        }
        p1_was_hit = p1_hit_now;
        p2_was_hit = p2_hit_now;

        // Position-based offstage detection. `position.{x,y}` are peppi's
        // per-frame f32 columns; `.get(idx)` returns Option to absorb the
        // (rare) case of a position column shorter than the state column,
        // which we degrade to "on stage" rather than panic.
        let p1_offstage = bounds
            .and_then(|b| {
                let x = p1_post.position.x.get(i)?;
                let y = p1_post.position.y.get(i)?;
                Some(b.is_offstage(x, y))
            })
            .unwrap_or(false);
        let p2_offstage = bounds
            .and_then(|b| {
                let x = p2_post.position.x.get(i)?;
                let y = p2_post.position.y.get(i)?;
                Some(b.is_offstage(x, y))
            })
            .unwrap_or(false);

        let p1_signals = PlayerFrameSignals {
            action_state: p1_state,
            offstage: p1_offstage,
            on_ledge: is_on_ledge(p1_state),
            frames_since_hitstun: p1_since,
        };
        let p2_signals = PlayerFrameSignals {
            action_state: p2_state,
            offstage: p2_offstage,
            on_ledge: is_on_ledge(p2_state),
            frames_since_hitstun: p2_since,
        };

        combat.push(classify_frame_layered(&p1_signals, &p2_signals, config));
    }

    Ok(ReplayAnalysis {
        combat,
        p1_port_idx,
        p2_port_idx,
    })
}

/// Compact summary of a combat-state vector: fraction of frames spent in each
/// state. Useful for dashboard rollups and for sanity checks in tests ("no
/// game should be > 95% Neutral or something is wrong with classification").
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CombatSummary {
    pub frames: usize,
    pub neutral: f64,
    pub adv_p1: f64,
    pub adv_p2: f64,
    pub trade: f64,
}

impl CombatSummary {
    pub fn from_states(states: &[CombatState]) -> CombatSummary {
        let n = states.len();
        if n == 0 {
            return CombatSummary {
                frames: 0,
                neutral: 0.0,
                adv_p1: 0.0,
                adv_p2: 0.0,
                trade: 0.0,
            };
        }
        let (mut neu, mut a, mut b, mut tr) = (0usize, 0usize, 0usize, 0usize);
        for s in states {
            match s {
                CombatState::Neutral => neu += 1,
                CombatState::AdvP1 => a += 1,
                CombatState::AdvP2 => b += 1,
                CombatState::Trade => tr += 1,
            }
        }
        let denom = n as f64;
        CombatSummary {
            frames: n,
            neutral: neu as f64 / denom,
            adv_p1: a as f64 / denom,
            adv_p2: b as f64 / denom,
            trade: tr as f64 / denom,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Quick constructor for the per-player signal struct used in the
    /// truth-table tests below. Defaults to a neutral wait state with
    /// no recent hit and on-stage feet-on-ground positioning.
    fn neutral_signals() -> PlayerFrameSignals {
        PlayerFrameSignals {
            action_state: 0x00,
            offstage: false,
            on_ledge: false,
            frames_since_hitstun: None,
        }
    }

    #[test]
    fn is_in_hitstun_boundaries() {
        // Outside the range.
        assert!(!is_in_hitstun(0));
        assert!(!is_in_hitstun(0x4A));
        assert!(!is_in_hitstun(0x5C));
        assert!(!is_in_hitstun(u16::MAX));

        // Boundary values — inclusive.
        assert!(is_in_hitstun(HITSTUN_STATE_MIN));
        assert!(is_in_hitstun(HITSTUN_STATE_MAX));
        // A representative interior value.
        assert!(is_in_hitstun(0x50));
    }

    #[test]
    fn is_on_ledge_boundaries() {
        // Outside the range.
        assert!(!is_on_ledge(0));
        assert!(!is_on_ledge(0xFB)); // one below CLIFF_CATCH
        assert!(!is_on_ledge(0x108)); // one above CLIFF_JUMP_QUICK_2
        assert!(!is_on_ledge(u16::MAX));

        // Boundary values — inclusive.
        assert!(is_on_ledge(LEDGE_STATE_MIN));
        assert!(is_on_ledge(LEDGE_STATE_MAX));
        // Representative interior values from the cliff block.
        assert!(is_on_ledge(0xFD)); // CLIFF_WAIT
        assert!(is_on_ledge(0x100)); // CLIFF_ATTACK_SLOW
    }

    #[test]
    fn classify_frame_truth_table() {
        let neutral = 0x00; // wait
        let dmg = 0x4E; // DAMAGE_N_1

        assert_eq!(classify_frame(neutral, neutral), CombatState::Neutral);
        assert_eq!(classify_frame(neutral, dmg), CombatState::AdvP1);
        assert_eq!(classify_frame(dmg, neutral), CombatState::AdvP2);
        assert_eq!(classify_frame(dmg, dmg), CombatState::Trade);
    }

    // --- v2 layered classifier --------------------------------------------

    #[test]
    fn layered_hitstun_overrides_everything() {
        let cfg = CombatV2Config::default();
        // P1 in hitstun + offstage + on ledge + recently hit; P2 perfectly
        // neutral. Hitstun is highest priority → AdvP2.
        let p1 = PlayerFrameSignals {
            action_state: 0x4E,
            offstage: true,
            on_ledge: true,
            frames_since_hitstun: Some(5),
        };
        let p2 = neutral_signals();
        assert_eq!(classify_frame_layered(&p1, &p2, &cfg), CombatState::AdvP2);
    }

    #[test]
    fn layered_double_hitstun_is_trade() {
        let cfg = CombatV2Config::default();
        let dmg = PlayerFrameSignals {
            action_state: 0x4E,
            ..neutral_signals()
        };
        assert_eq!(classify_frame_layered(&dmg, &dmg, &cfg), CombatState::Trade);
    }

    #[test]
    fn layered_recency_keeps_disadvantage_after_hitstun_ends() {
        let cfg = CombatV2Config::default();
        // P1 just exited hitstun a few frames ago; nobody is currently hit.
        let p1 = PlayerFrameSignals {
            frames_since_hitstun: Some(5),
            ..neutral_signals()
        };
        let p2 = neutral_signals();
        assert_eq!(classify_frame_layered(&p1, &p2, &cfg), CombatState::AdvP2);
    }

    #[test]
    fn layered_recency_expires_at_tail_window() {
        let cfg = CombatV2Config {
            hitstun_tail_frames: 10,
        };
        // Edge of the window — still on disadvantage.
        let p1 = PlayerFrameSignals {
            frames_since_hitstun: Some(10),
            ..neutral_signals()
        };
        let p2 = neutral_signals();
        assert_eq!(classify_frame_layered(&p1, &p2, &cfg), CombatState::AdvP2);

        // One past the window — recency expired, no other layer fires.
        let p1 = PlayerFrameSignals {
            frames_since_hitstun: Some(11),
            ..neutral_signals()
        };
        assert_eq!(
            classify_frame_layered(&p1, &p2, &cfg),
            CombatState::Neutral
        );
    }

    #[test]
    fn layered_recency_more_recent_hit_wins_tie() {
        let cfg = CombatV2Config::default();
        // Both players recently hit; p2 was hit more recently → p2 on dis-
        // advantage → AdvP1.
        let p1 = PlayerFrameSignals {
            frames_since_hitstun: Some(20),
            ..neutral_signals()
        };
        let p2 = PlayerFrameSignals {
            frames_since_hitstun: Some(5),
            ..neutral_signals()
        };
        assert_eq!(classify_frame_layered(&p1, &p2, &cfg), CombatState::AdvP1);

        // Same frame on both → trade.
        let p2 = PlayerFrameSignals {
            frames_since_hitstun: Some(20),
            ..neutral_signals()
        };
        assert_eq!(classify_frame_layered(&p1, &p2, &cfg), CombatState::Trade);
    }

    #[test]
    fn layered_offstage_grants_advantage_when_neutral() {
        let cfg = CombatV2Config::default();
        let p1 = PlayerFrameSignals {
            offstage: true,
            ..neutral_signals()
        };
        let p2 = neutral_signals();
        assert_eq!(classify_frame_layered(&p1, &p2, &cfg), CombatState::AdvP2);
    }

    #[test]
    fn layered_ledge_grants_advantage_when_neutral() {
        let cfg = CombatV2Config::default();
        let p1 = PlayerFrameSignals {
            action_state: 0xFD,
            on_ledge: true,
            ..neutral_signals()
        };
        let p2 = neutral_signals();
        assert_eq!(classify_frame_layered(&p1, &p2, &cfg), CombatState::AdvP2);
    }

    #[test]
    fn layered_double_offstage_falls_through_to_neutral() {
        let cfg = CombatV2Config::default();
        let p1 = PlayerFrameSignals {
            offstage: true,
            ..neutral_signals()
        };
        let p2 = PlayerFrameSignals {
            offstage: true,
            ..neutral_signals()
        };
        // Mutual edgeguard scenario — neither has positional advantage in
        // the v2 rule set. (A future precedence layer could attribute
        // momentum to whoever was on stage most recently; not needed yet.)
        assert_eq!(
            classify_frame_layered(&p1, &p2, &cfg),
            CombatState::Neutral
        );
    }

    #[test]
    fn layered_recency_overrides_offstage() {
        // Both players are off-stage — but p1 was just hit, so the recency
        // layer fires before the edge layer would collapse to Neutral.
        let cfg = CombatV2Config::default();
        let p1 = PlayerFrameSignals {
            offstage: true,
            frames_since_hitstun: Some(3),
            ..neutral_signals()
        };
        let p2 = PlayerFrameSignals {
            offstage: true,
            ..neutral_signals()
        };
        assert_eq!(classify_frame_layered(&p1, &p2, &cfg), CombatState::AdvP2);
    }

    #[test]
    fn layered_zero_tail_collapses_to_v1() {
        // Tail-frames = 0 disables the recency layer entirely. Combined
        // with no offstage/ledge signals, layered behavior matches v1
        // hitstun-only.
        let cfg = CombatV2Config {
            hitstun_tail_frames: 0,
        };
        let neutral = neutral_signals();
        let dmg = PlayerFrameSignals {
            action_state: 0x4E,
            ..neutral_signals()
        };

        assert_eq!(
            classify_frame_layered(&neutral, &neutral, &cfg),
            CombatState::Neutral
        );
        assert_eq!(
            classify_frame_layered(&dmg, &neutral, &cfg),
            CombatState::AdvP2
        );

        // Even a 1-frame-stale recency hit doesn't carry over with tail=0.
        let just_exited = PlayerFrameSignals {
            frames_since_hitstun: Some(0),
            ..neutral_signals()
        };
        // tail=0 means n <= 0 → only the literal "exited this frame" sample
        // fires. Expected: AdvP2 (p1 just exited).
        assert_eq!(
            classify_frame_layered(&just_exited, &neutral, &cfg),
            CombatState::AdvP2
        );
    }

    #[test]
    fn combat_summary_empty() {
        let s = CombatSummary::from_states(&[]);
        assert_eq!(s.frames, 0);
        assert_eq!(s.neutral, 0.0);
        assert_eq!(s.adv_p1, 0.0);
        assert_eq!(s.adv_p2, 0.0);
        assert_eq!(s.trade, 0.0);
    }

    // --- serialization / cache invalidation -------------------------------

    #[test]
    fn config_signature_changes_with_field_value() {
        let a = CombatV2Config::default();
        let b = CombatV2Config {
            hitstun_tail_frames: a.hitstun_tail_frames + 1,
        };
        assert_ne!(
            a.signature(),
            b.signature(),
            "different configs must produce different signatures"
        );

        // Same config, same signature — and stable across calls
        // within a single process. (Across rustc versions all bets
        // are off; that's the documented trade-off.)
        let a2 = CombatV2Config::default();
        assert_eq!(a.signature(), a2.signature());
    }

    #[test]
    fn cached_analysis_round_trips_through_bincode() {
        let analysis = ReplayAnalysis {
            combat: vec![
                CombatState::Neutral,
                CombatState::AdvP1,
                CombatState::AdvP2,
                CombatState::Trade,
            ],
            p1_port_idx: 0,
            p2_port_idx: 2,
        };
        let cached = CachedAnalysis::new(analysis.clone(), &CombatV2Config::default());
        let bytes = bincode::serialize(&cached).expect("serialize");
        let back: CachedAnalysis = bincode::deserialize(&bytes).expect("deserialize");

        assert_eq!(back.version, CACHED_ANALYSIS_VERSION);
        assert_eq!(back.config_hash, cached.config_hash);
        assert_eq!(back.analysis.combat, analysis.combat);
        assert_eq!(back.analysis.p1_port_idx, analysis.p1_port_idx);
        assert_eq!(back.analysis.p2_port_idx, analysis.p2_port_idx);
    }

    #[test]
    fn cached_analysis_is_fresh_iff_version_and_config_match() {
        let analysis = ReplayAnalysis {
            combat: vec![CombatState::Neutral],
            p1_port_idx: 0,
            p2_port_idx: 1,
        };
        let cfg = CombatV2Config::default();
        let cached = CachedAnalysis::new(analysis, &cfg);

        // Same config → fresh.
        assert!(cached.is_fresh(&cfg));

        // Different config → stale.
        let other = CombatV2Config {
            hitstun_tail_frames: cfg.hitstun_tail_frames + 5,
        };
        assert!(!cached.is_fresh(&other));

        // Manually-mismatched version → stale, even with matching
        // config.
        let mismatched = CachedAnalysis {
            version: CACHED_ANALYSIS_VERSION.wrapping_add(1),
            ..cached.clone()
        };
        assert!(!mismatched.is_fresh(&cfg));
    }

    #[test]
    fn combat_summary_proportions_sum_to_one() {
        let states = vec![
            CombatState::Neutral,
            CombatState::Neutral,
            CombatState::AdvP1,
            CombatState::AdvP2,
            CombatState::Trade,
        ];
        let s = CombatSummary::from_states(&states);
        let total = s.neutral + s.adv_p1 + s.adv_p2 + s.trade;
        assert!((total - 1.0).abs() < 1e-9, "proportions don't sum to 1: {total}");
        assert_eq!(s.frames, 5);
        assert!((s.neutral - 0.4).abs() < 1e-9);
        assert!((s.adv_p1 - 0.2).abs() < 1e-9);
        assert!((s.adv_p2 - 0.2).abs() < 1e-9);
        assert!((s.trade - 0.2).abs() < 1e-9);
    }
}
