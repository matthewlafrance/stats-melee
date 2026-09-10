use anyhow::{anyhow, Result};
use peppi::{self, game};
use peppi::game::immutable::Game;
use serde_json::{map, value};
use soccer::{Display, Into, TryFrom};
use std::string;

use crate::advanced::{compute_advanced_stats_1v1_with, AdvancedStats};
use crate::combat::compute_analysis_1v1;
use crate::punish::{extract_punishes_1v1, RawPunish};

pub static STAGES: [&str; 33] = [
    "Dummy",
    "Test",
    "FountainOfDreams",
    "PokemonStadium",
    "PrincessPeachsCastle",
    "KongoJungle",
    "Brinstar",
    "Corneria",
    "YoshisStory",
    "Onett",
    "MuteCity",
    "RainbowCruise",
    "JungleJapes",
    "GreatBay",
    "HyruleTemple",
    "BrinstarDepths",
    "YoshisIsland",
    "GreenGreens",
    "Fourside",
    "MushroomKingdomI",
    "MushroomKingdomII",
    "Akaneia",
    "Venom",
    "PokeFloats",
    "BigBlue",
    "IcicleMountain",
    "Icetop",
    "FlatZone",
    "DreamLandN64",
    "YoshisIslandN64",
    "KongoJungleN64",
    "Battlefield",
    "FinalDestination",
];

/// Human-readable name for a Slippi `attack_id`, or `None` if the id is
/// outside the universal ("every character has this move slot") block.
///
/// The Slippi attack id table mixes two kinds of ids:
///   - 0..=22 and 50..=71: universal — every character has a jab, ftilt,
///     nair, throw, edge attack, etc., and these slots use the same id
///     across the cast.
///   - 23..=49 and 72+: character-specific — Falcon Punch and Fox's
///     up-special live at the same id but mean different things. These
///     return `None` here, since resolving them needs a (character_id,
///     attack_id) lookup.
///
/// Names are the user-facing form ("down air", "forward tilt"), not the
/// internal action-state names ("ATTACK_AIR_LW", "ATTACK_LW3"). They render
/// directly into the Analytics page's top-kill-moves table.
pub fn attack_name(id: i32) -> Option<&'static str> {
    match id {
        // 0 = "none recorded" — the game stores 0 when the player hasn't
        // landed an attack yet this stock. Not user-facing; the kill-move
        // tracker filters these out before they reach the table, but expose
        // a name anyway so any stray rows render readably.
        0 => Some("none"),
        // 1 = miscellaneous / non-staling — items, projectiles whose
        // owner attribution Slippi can't pin down to a specific move.
        1 => Some("misc"),
        // Jabs.
        2 => Some("jab 1"),
        3 => Some("jab 2"),
        4 => Some("jab 3"),
        5 => Some("rapid jabs"),
        6 => Some("rapid jabs (end)"),
        // Ground attacks.
        7 => Some("dash attack"),
        8 => Some("forward tilt"),
        9 => Some("up tilt"),
        10 => Some("down tilt"),
        11 => Some("forward smash"),
        12 => Some("up smash"),
        13 => Some("down smash"),
        // Aerials.
        14 => Some("neutral air"),
        15 => Some("forward air"),
        16 => Some("back air"),
        17 => Some("up air"),
        18 => Some("down air"),
        // Specials — generic slot names. Character-specific aliases
        // ("falcon punch", "shine") get added in 8d once we have a
        // character filter active.
        19 => Some("neutral special"),
        20 => Some("side special"),
        21 => Some("up special"),
        22 => Some("down special"),
        // Get-up attacks (after a knockdown) — "slow" / "quick" mirror
        // the in-game distinction by knockdown duration.
        50 => Some("get-up attack (slow)"),
        51 => Some("get-up attack (quick)"),
        52 => Some("get-up attack (trip, slow)"),
        53 => Some("get-up attack (trip, quick)"),
        // Edge attacks (from hanging on the ledge).
        54 => Some("edge attack (slow)"),
        55 => Some("edge attack (quick)"),
        // Throws — id ordering matches the Slippi spec, not the
        // alphabetical "back/down/forward/up" we list them as in UIs.
        56 => Some("forward throw"),
        57 => Some("back throw"),
        58 => Some("up throw"),
        59 => Some("down throw"),
        // Pummel (the "A" tap during a grab).
        60 => Some("pummel"),
        _ => None,
    }
}

/// Display name for an attack id, falling back to `attack #N` for ids
/// the universal table doesn't cover. Use this in the UI; never leak a
/// raw integer to the user.
pub fn attack_display_name(id: i32) -> String {
    match attack_name(id) {
        Some(n) => n.to_string(),
        None => format!("attack #{id}"),
    }
}

/// Turn a CamelCase identifier from [`CHARACTERS`] / [`STAGES`] into a
/// space-separated display string: `"CaptainFalcon"` → `"Captain Falcon"`,
/// `"FountainOfDreams"` → `"Fountain Of Dreams"`, `"GameAndWatch"` →
/// `"Game And Watch"`.
///
/// A space is inserted before an uppercase letter when the previous
/// character is lowercase or a digit (the usual camelCase boundary), or
/// when the previous character is uppercase but the *next* is lowercase
/// (so an acronym run like the leading caps of `"HTMLParser"` splits as
/// `"HTML Parser"`). This keeps trailing roman numerals together —
/// `"MushroomKingdomII"` → `"Mushroom Kingdom II"`, not `"… I I"` — and
/// leaves digit suffixes attached: `"DreamLandN64"` → `"Dream Land N64"`.
pub fn spaced_name(name: &str) -> String {
    let chars: Vec<char> = name.chars().collect();
    let mut out = String::with_capacity(name.len() + 4);
    for (i, &c) in chars.iter().enumerate() {
        if i > 0 && c.is_ascii_uppercase() {
            let prev = chars[i - 1];
            let next_is_lower = chars
                .get(i + 1)
                .is_some_and(|n| n.is_ascii_lowercase());
            if prev.is_ascii_lowercase()
                || prev.is_ascii_digit()
                || (prev.is_ascii_uppercase() && next_is_lower)
            {
                out.push(' ');
            }
        }
        out.push(c);
    }
    out
}

pub static CHARACTERS: [&str; 33] = [
    "Mario",
    "Fox",
    "CaptainFalcon",
    "DonkeyKong",
    "Kirby",
    "Bowser",
    "Link",
    "Sheik",
    "Ness",
    "Peach",
    "Popo",
    "Nana",
    "Pikachu",
    "Samus",
    "Yoshi",
    "Jigglypuff",
    "Mewtwo",
    "Luigi",
    "Marth",
    "Zelda",
    "YoungLink",
    "DrMario",
    "Falco",
    "Pichu",
    "GameAndWatch",
    "Ganondorf",
    "Roy",
    "MasterHand",
    "CrazyHand",
    "WireFrameMale",
    "WireFrameFemale",
    "GigaBowser",
    "Sandbag",
];

/// Internal character id (this crate's [`CHARACTERS`] order) → the *external*
/// id, which is what the Game Start block stores and what Slippi's own asset
/// folders are named after.
///
/// Melee carries two character id spaces and they disagree almost everywhere:
/// internal 0 is Mario while external 0 is Captain Falcon. Replay *metadata*
/// keys its `characters` map by internal id, but the Game Start block reports
/// external — so any code reading characters out of Game Start has to convert
/// or it will silently label every replay with the wrong fighter.
///
/// Covers the 27 playable slots; ids past this (Master Hand, Giga Bowser,
/// Sandbag) never appear as a player's character.
pub const INTERNAL_TO_EXTERNAL_CHARACTER: [u8; 27] = [
    8, 2, 0, 1, 4, 5, 6, 19, 11, 12, 14, 14, 13, 16, 17, 15, 10, 7, 9, 18, 21, 22, 20, 24, 3, 25,
    23,
];

/// External character id → internal, the direction needed when reading the
/// Game Start block.
///
/// External 14 (Ice Climbers) maps to internal 10 (Popo) rather than 11
/// (Nana): Game Start names the *player's* character, and the player controls
/// Popo. Nana is a follower and never appears here.
pub fn external_to_internal_character(external: u8) -> Option<i32> {
    INTERNAL_TO_EXTERNAL_CHARACTER
        .iter()
        .position(|&e| e == external)
        .map(|i| i as i32)
}

/// Internal character id → external. Inverse of
/// [`external_to_internal_character`].
pub fn internal_to_external_character(internal: i32) -> Option<u8> {
    usize::try_from(internal)
        .ok()
        .and_then(|i| INTERNAL_TO_EXTERNAL_CHARACTER.get(i).copied())
}

#[derive(Debug)]
pub struct GameData {
    /// Players indexed by **0-based controller port**, not by placement.
    /// A `None` slot means that port was empty (or its metadata was
    /// unreadable). Port order is stable across every field below, and
    /// matches the `port_idx` used by [`crate::punish`] and
    /// [`crate::advanced`], so all the frame-derived data lines up
    /// without a translation step.
    pub players: [Option<SlippiPlayer>; 4],
    /// `placements[rank]` is the port of the player who finished in
    /// position `rank` — `placements[0]` is the winner's port. `None`
    /// means no player took that position.
    ///
    /// This is the *only* placement-ordered field; everything else is
    /// port-ordered. Use [`GameData::player_at_placement`] to go from a
    /// rank to a player and [`GameData::placement_of_port`] for the
    /// reverse.
    pub placements: [Option<usize>; 4],
    /// Stocks remaining at the final recorded frame, indexed by port.
    /// `None` if frame data was unavailable.
    pub stocks_remaining: [Option<i32>; 4],
    /// Stocks the player started the game with (4 in most matches, but timed /
    /// handicap matches differ). Read from `game.start.players[port].stocks`.
    /// `None` if the Start block didn't include this player.
    pub starting_stocks: [Option<i32>; 4],
    /// Best-effort per-game input count — sum of button-state transitions
    /// across pre-frames. Used as the numerator for APM.
    pub inputs: [Option<i32>; 4],
    /// Number of post-frames with a non-zero `l_cancel` flag (1=success,
    /// 2=failure). The counter increments exactly once per aerial landing
    /// that the game considered for L-canceling.
    pub l_cancel_attempts: [Option<i32>; 4],
    /// Subset of `l_cancel_attempts` where the flag was `1` (successful
    /// L-cancel).
    pub l_cancel_success: [Option<i32>; 4],
    /// Punish events extracted from frame data (see `crate::punish`). Empty
    /// for non-1v1 games (2v2 / FFA aren't supported by the extractor yet)
    /// or when frame data is too sparse to detect any punishes. Keyed by
    /// port index, same as `players`.
    pub punishes: Vec<RawPunish>,
    /// Advanced per-game combat stats keyed by port index (see
    /// `crate::advanced`). `None` for non-1v1 games or when frame data was
    /// too sparse to analyze.
    pub advanced: Option<AdvancedStats>,
    pub stage: i32,
    pub time: i32,
    /// ISO-8601 timestamp the game was played, read from the Slippi
    /// metadata `startAt` (e.g. "2025-04-01T14:39:10Z"). `None` when the
    /// metadata block lacks a usable string date.
    pub started_at: Option<String>,
}


/// Read the final-frame stocks value for `port_idx` (0-based) out of peppi's
/// columnar frame data. Returns `None` when:
/// - the port has no frame data (fewer than 4 active ports), or
/// - the `stocks` arrow array is empty (should be rare, corrupt replay).
///
/// `game.frames.ports[i].leader.post.stocks` is an `arrow2::PrimitiveArray<u8>`
/// containing one value per frame, so we just take the last one.
fn final_stocks_for_port(game: &Game, port_idx: usize) -> Option<i32> {
    let port_data = game.frames.ports.get(frame_slot_for_port(game, port_idx)?)?;
    let stocks = &port_data.leader.post.stocks;
    let n = stocks.len();
    if n == 0 {
        return None;
    }
    Some(stocks.value(n - 1) as i32)
}

/// Index of `port`'s entry in `game.frames.ports`.
///
/// `frames.ports` is dense over the ports that were *occupied*, not indexed
/// by port number: a game played on ports 1 and 3 has two entries, at slots
/// 0 and 1.
///
/// The two coincide only when players sit on the lowest ports, which Slippi
/// netplay always does and console setups frequently do not. Indexing that
/// vector with a port number therefore reads the wrong player's frames — or
/// no player at all — without erroring. Resolve through `PortData::port`
/// instead, which is correct either way.
fn frame_slot_for_port(game: &Game, port_idx: usize) -> Option<usize> {
    let want = PortIndex::from_index(port_idx)?.to_peppi();
    game.frames.ports.iter().position(|p| p.port == want)
}

/// Damage percent at the final recorded frame for one port.
///
/// Only used to break stock ties when deriving placements; `None` when the
/// port has no frame data.
fn final_percent_for_port(game: &Game, port_idx: usize) -> Option<f32> {
    let port_data = game.frames.ports.get(frame_slot_for_port(game, port_idx)?)?;
    let percent = &port_data.leader.post.percent;
    let n = percent.len();
    if n == 0 {
        return None;
    }
    Some(percent.value(n - 1))
}

/// Every port that actually had a player, from the Game Start block.
///
/// peppi only lists occupied ports here, so this is the authoritative roster
/// for replays whose GameEnd block carries no placements.
fn ports_from_game_start(game: &Game) -> Vec<PortIndex> {
    game.start
        .players
        .iter()
        .map(|p| PortIndex::from_peppi(p.port))
        .collect()
}

/// Reconstruct finishing order from end-of-game frame state, for replays
/// whose GameEnd block predates placements (Slippi replay spec < 3.13).
///
/// Ranking is most-stocks-first, ties broken by lower damage — the same
/// order Melee itself uses to decide a timeout. Returns `None` when no port
/// has readable stock data, because the alternative is inventing a winner:
/// a fabricated `placements[0]` would flow straight into win rates and
/// head-to-head records as though it were a fact.
///
/// This is best-effort even when it succeeds. A game that ended in a quit
/// (LRAS) is scored on the stocks standing at that moment, which is not
/// necessarily how a tournament would have recorded it.
fn derive_placements(game: &Game, ports: &[PortIndex]) -> Option<Vec<PortIndex>> {
    if !ports
        .iter()
        .any(|p| final_stocks_for_port(game, p.as_usize()).is_some())
    {
        return None;
    }

    let mut ranked: Vec<PortIndex> = ports.to_vec();
    ranked.sort_by(|a, b| {
        let stocks = |p: &PortIndex| final_stocks_for_port(game, p.as_usize()).unwrap_or(0);
        let percent = |p: &PortIndex| final_percent_for_port(game, p.as_usize()).unwrap_or(f32::MAX);
        stocks(b)
            .cmp(&stocks(a))
            .then_with(|| {
                percent(a)
                    .partial_cmp(&percent(b))
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            // Stable final tiebreak so a genuine tie (equal stocks *and*
            // equal damage) still produces a deterministic ordering.
            .then_with(|| a.as_usize().cmp(&b.as_usize()))
    });
    Some(ranked)
}

/// Starting stocks for the given port, from the Game Start event.
///
/// peppi exposes `game.start.players` as a `Vec<Player>` where each entry's
/// `.port` identifies which GameCube port they used. We walk the vec and match
/// on port rather than assuming a fixed ordering — 2v2 matches have 4 players
/// in arbitrary port order, and 1v1s are commonly P1/P3 or P2/P4.
fn starting_stocks_for_port(game: &Game, port: game::Port) -> Option<i32> {
    game.start
        .players
        .iter()
        .find(|p| p.port == port)
        .map(|p| p.stocks as i32)
}

/// Count button-state transitions across pre-frames for one port, as a proxy
/// for "inputs" (the numerator in APM = inputs / minutes).
///
/// A transition is any frame where `buttons` differs from the previous frame —
/// this captures press *and* release events, which matches how most Melee
/// stats tools report APM. We intentionally ignore analog stick wiggles;
/// counting micro-movements would inflate the number without adding signal.
///
/// `pre.buttons` is `arrow2::PrimitiveArray<u32>` (the 32-bit "logical" button
/// bitmask from Slippi spec).
fn inputs_for_port(game: &Game, port_idx: usize) -> Option<i32> {
    let port_data = game.frames.ports.get(frame_slot_for_port(game, port_idx)?)?;
    let buttons = &port_data.leader.pre.buttons;
    let n = buttons.len();
    if n < 2 {
        return Some(0);
    }

    let mut transitions: i32 = 0;
    let mut prev = buttons.value(0);
    for i in 1..n {
        let cur = buttons.value(i);
        if cur != prev {
            transitions += 1;
            prev = cur;
        }
    }
    Some(transitions)
}

/// Count L-cancel flag occurrences in post frames. Returns
/// `(attempts, successes)` where:
/// - attempts = frames with `l_cancel != 0`
/// - successes = frames with `l_cancel == 1`
///
/// The game only sets `l_cancel` on the frame an aerial attack lands, so one
/// attempt per landing. If the underlying arrow column carries validity bits
/// (i.e. the field is nullable), null slots count as "no attempt" and are
/// skipped via `is_null`.
fn l_cancel_counts_for_port(game: &Game, port_idx: usize) -> Option<(i32, i32)> {
    let port_data = game.frames.ports.get(frame_slot_for_port(game, port_idx)?)?;
    // `l_cancel` was added in Slippi spec v2.0 — peppi exposes it as
    // `Option<PrimitiveArray<u8>>`. Older replays simply won't have it.
    let l_cancel = port_data.leader.post.l_cancel.as_ref()?;
    let n = l_cancel.len();
    if n == 0 {
        return None;
    }

    let mut attempts: i32 = 0;
    let mut successes: i32 = 0;
    for i in 0..n {
        // `.get(i)` returns `None` for null slots (frames where the character
        // wasn't present) and `Some(v)` otherwise. `v == 0` means "no aerial
        // landing this frame" so it shouldn't count as an attempt.
        match l_cancel.get(i) {
            None | Some(0) => continue,
            Some(v) => {
                attempts += 1;
                if v == 1 {
                    successes += 1;
                }
            }
        }
    }
    Some((attempts, successes))
}

impl GameData {
    pub fn new_gamedata(game: &peppi::game::immutable::Game) -> Result<GameData> {
        let metadata = game.metadata.as_ref().ok_or(anyhow!("no metadata found"))?;

        // Who was in the game. The GameEnd placement array is the best
        // source when present, but it only exists from Slippi replay spec
        // 3.13 on; older replays (all pre-2022 tournament footage) have a
        // GameEnd block with nothing in it but the end method. Game Start
        // lists the occupied ports in every spec version, so fall back to it
        // rather than rejecting the replay.
        let end_players = game.end.as_ref().and_then(|e| e.players.as_ref());
        let ports: Vec<PortIndex> = match end_players {
            Some(eps) => eps.iter().map(|p| PortIndex::from_peppi(p.port)).collect(),
            None => ports_from_game_start(game),
        };

        // Finishing order. `None` means genuinely unknown — see
        // [`derive_placements`] on why that is preferable to guessing.
        let ranked: Option<Vec<PortIndex>> = match end_players {
            Some(eps) => {
                // peppi yields these in port order, not placement order.
                // Sort so index 0 is genuinely first place.
                let mut sorted: Vec<&_> = eps.iter().collect();
                sorted.sort_by_key(|p| p.placement);
                Some(
                    sorted
                        .iter()
                        .map(|p| PortIndex::from_peppi(p.port))
                        .collect(),
                )
            }
            None => derive_placements(game, &ports),
        };

        let mut players: [Option<SlippiPlayer>; 4] = [None, None, None, None];
        let mut placements: [Option<usize>; 4] = [None; 4];
        let mut stocks_remaining: [Option<i32>; 4] = [None; 4];
        let mut starting_stocks: [Option<i32>; 4] = [None; 4];
        let mut inputs: [Option<i32>; 4] = [None; 4];
        let mut l_cancel_attempts: [Option<i32>; 4] = [None; 4];
        let mut l_cancel_success: [Option<i32>; 4] = [None; 4];

        // Everything here is written at the player's *port*, which keeps this
        // struct aligned with the port-keyed frame analysis.
        for port in ports.iter().take(4) {
            let idx = port.as_usize();

            // Frame-derived stats are facts about the port, so record them
            // whether or not the player behind it can be identified.
            stocks_remaining[idx] = final_stocks_for_port(game, idx);
            starting_stocks[idx] = starting_stocks_for_port(game, port.to_peppi());
            inputs[idx] = inputs_for_port(game, idx);
            if let Some((att, suc)) = l_cancel_counts_for_port(game, idx) {
                l_cancel_attempts[idx] = Some(att);
                l_cancel_success[idx] = Some(suc);
            }

            players[idx] = SlippiPlayer::resolve(game, metadata, *port);
        }

        // A port only claims a placement slot once the player behind it is
        // identified. An unidentifiable player leaves the slot `None` rather
        // than recording a finishing position nobody can be attached to.
        if let Some(ranked) = ranked {
            for (rank, port) in ranked.iter().take(4).enumerate() {
                let idx = port.as_usize();
                if players[idx].is_some() {
                    placements[rank] = Some(idx);
                }
            }
        }

        let stage = game.start.stage as i32;
        let time = Self::game_len(game)?;

        // When the game was played, from the Slippi metadata `startAt`
        // (a string ISO-8601 timestamp). Best-effort: a missing/non-string
        // value just yields `None`.
        let started_at = match metadata.get("startAt") {
            Some(value::Value::String(s)) if !s.is_empty() => Some(s.clone()),
            _ => None,
        };

        // Punish extraction is 1v1-only today. For 2v2 / FFA, the extractor
        // returns `Err` which we swallow — those replays still get ingested,
        // just without any punish rows.
        let punishes_result = extract_punishes_1v1(game);

        // Advanced combat stats, same 1v1-only best-effort contract: a non-1v1
        // game or sparse frame data yields `None` and the rows store NULLs.
        //
        // The punish list computed above is threaded in rather than let the
        // stats recompute it, which would walk every frame of the replay a
        // second time for an identical answer. `advanced` stays `None` when
        // punish extraction failed: the stats are derived from those punishes,
        // so there is nothing meaningful to record without them.
        let advanced = match punishes_result.as_ref() {
            Ok(punishes) => compute_analysis_1v1(game)
                .ok()
                .and_then(|analysis| {
                    compute_advanced_stats_1v1_with(game, &analysis, punishes).ok()
                }),
            Err(_) => None,
        };

        let punishes = punishes_result.unwrap_or_default();

        Ok(GameData {
            players,
            placements,
            stocks_remaining,
            starting_stocks,
            inputs,
            l_cancel_attempts,
            l_cancel_success,
            punishes,
            advanced,
            stage,
            time,
            started_at,
        })
    }

    /// Players indexed by 0-based port. See [`GameData::players`].
    pub fn players(&self) -> &[Option<SlippiPlayer>; 4] {
        &self.players
    }

    /// Ports indexed by finishing position. See [`GameData::placements`].
    pub fn placements(&self) -> &[Option<usize>; 4] {
        &self.placements
    }

    /// The player who finished in position `rank` (0 = winner).
    pub fn player_at_placement(&self, rank: usize) -> Option<&SlippiPlayer> {
        let port = (*self.placements.get(rank)?)?;
        self.players.get(port)?.as_ref()
    }

    /// Finishing position of the player on `port`, or `None` if that port
    /// was empty. Linear over 4 elements — the inverse direction is rare
    /// enough not to warrant a second stored array that could drift.
    pub fn placement_of_port(&self, port: usize) -> Option<usize> {
        self.placements
            .iter()
            .position(|slot| *slot == Some(port))
    }

    pub fn stage(&self) -> i32 {
        self.stage
    }

    pub fn time(&self) -> i32 {
        self.time
    }

    /// Returns the 1st-place finisher, if any.
    pub fn winner(&self) -> Option<&SlippiPlayer> {
        self.player_at_placement(0)
    }

    /// Game length in whole seconds.
    ///
    /// Slippi frames start at -123 (pre-game setup); actual gameplay begins at
    /// frame 0, so we subtract the 123 pre-game frames before dividing by 60 fps.
    pub fn game_len(game: &Game) -> Result<i32> {
        let len = game.frames.len().saturating_sub(123);
        (len / 60)
            .try_into()
            .map_err(|_| anyhow!("can't parse game length"))
    }
}

/// A controller port, held **0-indexed**.
///
/// Two external contracts use the 0-based numbering and this type carries
/// it for both:
///
/// - Slippi's replay metadata keys `players` by the 0-based index
///   (`"0"`..`"3"`) — see [`PortIndex::metadata_key`].
/// - The `gamePlayer.port` column stores the same 0-based value — that's
///   the `Into<i32>` impl.
///
/// peppi's [`game::Port`], by contrast, names its variants after the
/// human-facing player number: **`game::Port::P1` is the FIRST port**,
/// i.e. `PortIndex::P0`. The two enums therefore share variant names that
/// mean *different* ports, which the compiler cannot catch. Convert only at
/// the boundary, via [`PortIndex::from_peppi`] / [`PortIndex::to_peppi`],
/// and never mix the two types in the same expression.
#[derive(Clone, Copy, Debug, PartialEq, Eq, TryFrom, Into, Display)]
#[repr(i32)]
pub enum PortIndex {
    P0,
    P1,
    P2,
    P3,
}

impl PortIndex {
    /// Every port, in index order. Handy for `for port in PortIndex::ALL`.
    pub const ALL: [PortIndex; 4] = [
        PortIndex::P0,
        PortIndex::P1,
        PortIndex::P2,
        PortIndex::P3,
    ];

    /// Convert from peppi's 1-indexed-by-name port enum. This is the only
    /// place the off-by-one between the two namings is applied.
    pub fn from_peppi(port: game::Port) -> Self {
        match port {
            game::Port::P1 => PortIndex::P0,
            game::Port::P2 => PortIndex::P1,
            game::Port::P3 => PortIndex::P2,
            game::Port::P4 => PortIndex::P3,
        }
    }

    /// Inverse of [`Self::from_peppi`], for the peppi APIs that want their
    /// own enum (e.g. indexing `game.start.players`).
    pub fn to_peppi(self) -> game::Port {
        match self {
            PortIndex::P0 => game::Port::P1,
            PortIndex::P1 => game::Port::P2,
            PortIndex::P2 => game::Port::P3,
            PortIndex::P3 => game::Port::P4,
        }
    }

    /// 0-based port number, for this crate's `[T; 4]` port-keyed arrays.
    ///
    /// Not a slot index into `game.frames.ports[..]` — that vector is dense
    /// over occupied ports. Use [`frame_slot_for_port`] to cross over.
    pub fn as_usize(self) -> usize {
        match self {
            PortIndex::P0 => 0,
            PortIndex::P1 => 1,
            PortIndex::P2 => 2,
            PortIndex::P3 => 3,
        }
    }

    /// Build from a 0-based index; `None` if out of range.
    pub fn from_index(idx: usize) -> Option<Self> {
        PortIndex::ALL.get(idx).copied()
    }

    /// The key this port has in Slippi's metadata `players` object.
    ///
    /// Spelled out rather than leaning on `to_string()`: the `Display`
    /// derive happens to emit the discriminant (`"0"`..`"3"`), but that's
    /// a property of the `soccer` derive macro, not something the metadata
    /// format guarantees will keep matching. Swapping derive crates would
    /// otherwise silently break every metadata lookup.
    pub fn metadata_key(self) -> &'static str {
        match self {
            PortIndex::P0 => "0",
            PortIndex::P1 => "1",
            PortIndex::P2 => "2",
            PortIndex::P3 => "3",
        }
    }
}

#[derive(Debug)]
pub struct SlippiPlayer {
    pub netplay: String,
    pub code: String,
    pub character: i32,
    /// 0-based controller port. Doubles as this player's index into
    /// [`GameData`]'s port-keyed arrays.
    pub port: PortIndex,
}

impl SlippiPlayer {
    /// Identify the player on `port`, preferring the metadata block and
    /// falling back to Game Start.
    ///
    /// Metadata is richer — it carries the netplay name and connect code —
    /// but console-recorded replays write `"players": {}` and keep nothing
    /// at all. Game Start always has the character, so a replay with no
    /// metadata players still yields a usable (if anonymous) player.
    pub fn resolve(
        game: &Game,
        metadata: &map::Map<string::String, value::Value>,
        port: PortIndex,
    ) -> Option<SlippiPlayer> {
        SlippiPlayer::new_slippi_player(metadata, port)
            .or_else(|| SlippiPlayer::from_game_start(game, port))
    }

    /// Build a player from the Game Start block alone.
    ///
    /// Yields an anonymous player: Game Start records the character but no
    /// identity, so `netplay` and `code` come back empty. See
    /// [`SlippiPlayer::is_anonymous`] for what that means downstream.
    pub fn from_game_start(game: &Game, port: PortIndex) -> Option<SlippiPlayer> {
        let start = game
            .start
            .players
            .iter()
            .find(|p| p.port == port.to_peppi())?;
        // Game Start stores the *external* character id; ours are internal.
        let character = external_to_internal_character(start.character)?;
        Some(SlippiPlayer {
            netplay: String::new(),
            code: String::new(),
            character,
            port,
        })
    }

    /// Whether this player carries no identity — the replay recorded a
    /// character but no netplay name or connect code.
    ///
    /// Per-player features key on `code`, so every anonymous player in a
    /// database collapses into a single bucket. That is fine for the
    /// character/stage/matchup analytics, which never look at identity, and
    /// meaningless for per-player summaries. Keep a corpus of anonymous
    /// replays in its own database rather than mixing it into one that has
    /// real connect codes in it.
    pub fn is_anonymous(&self) -> bool {
        self.code.is_empty()
    }

    pub fn new_slippi_player(
        metadata: &map::Map<string::String, value::Value>,
        port: PortIndex,
    ) -> Option<SlippiPlayer> {
        let port_string = port.metadata_key();

        let netplay = match &metadata["players"][port_string]["names"]["netplay"] {
            value::Value::String(n) => n.clone(),
            _ => {
                return None;
            }
        };

        let code = match &metadata["players"][port_string]["names"]["code"] {
            value::Value::String(c) => c.clone(),
            _ => {
                return None;
            }
        };

        let characters = &metadata["players"][port_string]["characters"].as_object();

        let characters = match characters {
            Some(c) => c,
            None => {
                return None;
            }
        };

        let mut character = None;
        let mut frames = 0;
        for c in characters.keys() {
            let current_frames = characters.get(c)?.as_u64()?;
            if current_frames > frames {
                character = Some(c);
                frames = current_frames;
            }
        }

        let character = match character {
            Some(c) => c,
            None => {
                return None;
            }
        };

        let character = character.parse::<i32>().ok()?;

        Some(SlippiPlayer {
            netplay,
            code,
            character,
            port,
        })
    }

    pub fn netplay(&self) -> &str {
        &self.netplay
    }

    pub fn code(&self) -> &str {
        &self.code
    }

    pub fn character(&self) -> i32 {
        self.character
    }

    pub fn port(&self) -> PortIndex {
        self.port
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn port_roundtrip_int() {
        for (p, expected) in [
            (PortIndex::P0, 0),
            (PortIndex::P1, 1),
            (PortIndex::P2, 2),
            (PortIndex::P3, 3),
        ] {
            let as_int: i32 = p.into();
            assert_eq!(as_int, expected);
            let back: PortIndex = PortIndex::try_from(expected).expect("try_from");
            assert_eq!(back, p);
            assert_eq!(p.as_usize(), expected as usize);
            assert_eq!(PortIndex::from_index(expected as usize), Some(p));
        }
    }

    /// peppi names ports after the human-facing player number, this crate
    /// after the 0-based index. Pin the off-by-one so a refactor that
    /// "tidies" one of the two enums fails loudly here instead of silently
    /// attributing every stat to the wrong player.
    #[test]
    fn peppi_port_is_one_indexed_by_name() {
        for (peppi, ours, idx) in [
            (game::Port::P1, PortIndex::P0, 0usize),
            (game::Port::P2, PortIndex::P1, 1),
            (game::Port::P3, PortIndex::P2, 2),
            (game::Port::P4, PortIndex::P3, 3),
        ] {
            assert_eq!(PortIndex::from_peppi(peppi), ours);
            assert_eq!(ours.to_peppi(), peppi);
            assert_eq!(ours.as_usize(), idx);
        }
    }

    /// Slippi's metadata `players` object is keyed by the 0-based port index
    /// as a string. `metadata_key` spells those keys out rather than deriving
    /// them from `Display`, and this pins the two to the same answer.
    #[test]
    fn metadata_key_is_zero_based_string() {
        for (port, key) in [
            (PortIndex::P0, "0"),
            (PortIndex::P1, "1"),
            (PortIndex::P2, "2"),
            (PortIndex::P3, "3"),
        ] {
            assert_eq!(port.metadata_key(), key);
            assert_eq!(port.to_string(), key);
        }
    }

    #[test]
    fn stages_and_characters_have_expected_counts() {
        // These back the NUM_STAGES / NUM_CHARACTERS constants used by
        // fixed-size analytics arrays — divergence would corrupt stats.
        assert_eq!(STAGES.len(), 33);
        assert_eq!(CHARACTERS.len(), 33);
    }

    #[test]
    fn attack_name_covers_universal_ids() {
        // Spot-check each band of the universal table — full enumeration
        // would just restate the match arm above, but we want a test that
        // fails loudly if any of the user-facing names get accidentally
        // renamed (e.g. "down air" → "dair").
        assert_eq!(attack_name(2), Some("jab 1"));
        assert_eq!(attack_name(7), Some("dash attack"));
        assert_eq!(attack_name(8), Some("forward tilt"));
        assert_eq!(attack_name(11), Some("forward smash"));
        assert_eq!(attack_name(14), Some("neutral air"));
        assert_eq!(attack_name(18), Some("down air"));
        assert_eq!(attack_name(21), Some("up special"));
        assert_eq!(attack_name(56), Some("forward throw"));
        assert_eq!(attack_name(60), Some("pummel"));
    }

    #[test]
    fn attack_name_returns_none_for_character_specific_ids() {
        // 23..=49 is the character-specific band (Falcon Punch et al);
        // these aren't named in the universal table — resolving them
        // needs a (character_id, attack_id) lookup.
        for id in [23, 24, 30, 40, 49] {
            assert!(
                attack_name(id).is_none(),
                "id {id} should not resolve in the universal table"
            );
        }
        // And out-of-range ids (negative, far above the table).
        for id in [-1, 100, 999, i32::MAX] {
            assert!(attack_name(id).is_none(), "id {id} unexpectedly named");
        }
    }

    #[test]
    fn attack_display_name_falls_back_for_unknowns() {
        // Universal ids round-trip through the named form...
        assert_eq!(attack_display_name(11), "forward smash");
        // ...character-specific / unknown ids get the "#N" placeholder
        // so the UI never has to special-case unknowns at the call site.
        assert_eq!(attack_display_name(23), "attack #23");
        assert_eq!(attack_display_name(-7), "attack #-7");
    }

    #[test]
    fn spaced_name_splits_camelcase_roster_names() {
        // Plain single words are unchanged.
        assert_eq!(spaced_name("Fox"), "Fox");
        assert_eq!(spaced_name("Battlefield"), "Battlefield");
        assert_eq!(spaced_name("Mewtwo"), "Mewtwo");
        // camelCase boundaries get a space.
        assert_eq!(spaced_name("CaptainFalcon"), "Captain Falcon");
        assert_eq!(spaced_name("DonkeyKong"), "Donkey Kong");
        assert_eq!(spaced_name("GameAndWatch"), "Game And Watch");
        assert_eq!(spaced_name("FountainOfDreams"), "Fountain Of Dreams");
        assert_eq!(spaced_name("FinalDestination"), "Final Destination");
        assert_eq!(spaced_name("YoungLink"), "Young Link");
        // Trailing roman numerals stay together (no "I I").
        assert_eq!(spaced_name("MushroomKingdomII"), "Mushroom Kingdom II");
        assert_eq!(spaced_name("MushroomKingdomI"), "Mushroom Kingdom I");
        // Digit suffixes stay attached to their word.
        assert_eq!(spaced_name("DreamLandN64"), "Dream Land N64");
        assert_eq!(spaced_name("KongoJungleN64"), "Kongo Jungle N64");
    }
}

