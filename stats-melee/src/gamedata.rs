use anyhow::{anyhow, Result};
use peppi::{self, game};
use peppi::game::immutable::Game;
use serde_json::{map, value};
use soccer::{Display, Into, TryFrom};
use std::string;

use crate::advanced::{compute_advanced_stats_1v1, AdvancedStats};
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

#[derive(Debug)]
pub struct GameData {
    pub placements: [Option<SlippiPlayer>; 4],
    /// Stocks remaining at the final recorded frame, indexed by placement slot
    /// (same ordering as `placements`). `None` if frame data was unavailable.
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
    /// peppi `port_idx`, not by placement — the `post_game` layer is
    /// responsible for translating indices to `gamePlayer.id`s.
    pub punishes: Vec<RawPunish>,
    /// Advanced per-game combat stats keyed by peppi port index (see
    /// `crate::advanced`). `None` for non-1v1 games or when frame data was
    /// too sparse to analyze. The `post_game` layer maps the port-keyed
    /// `p1`/`p2` onto each placement's `game_player_stat` row.
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
    let port_data = game.frames.ports.get(port_idx)?;
    let stocks = &port_data.leader.post.stocks;
    let n = stocks.len();
    if n == 0 {
        return None;
    }
    Some(stocks.value(n - 1) as i32)
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
    let port_data = game.frames.ports.get(port_idx)?;
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
    let port_data = game.frames.ports.get(port_idx)?;
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
        let end = game.end.as_ref().ok_or(anyhow!("no end block found"))?;
        let end_players = end
            .players
            .as_ref()
            .ok_or(anyhow!("no end players found"))?;

        // Sort end.players by placement so placements[0] is 1st place, [1] is 2nd, etc.
        // peppi's end.players is otherwise given in port order, which is why the
        // pre-refactor code treated whoever was in port 1 as the winner.
        let mut sorted: Vec<&_> = end_players.iter().collect();
        sorted.sort_by_key(|p| p.placement);

        let mut placements: [Option<SlippiPlayer>; 4] = [None, None, None, None];
        let mut stocks_remaining: [Option<i32>; 4] = [None, None, None, None];
        let mut starting_stocks: [Option<i32>; 4] = [None, None, None, None];
        let mut inputs: [Option<i32>; 4] = [None, None, None, None];
        let mut l_cancel_attempts: [Option<i32>; 4] = [None, None, None, None];
        let mut l_cancel_success: [Option<i32>; 4] = [None, None, None, None];

        for (i, player) in sorted.iter().take(4).enumerate() {
            let (port, port_idx, peppi_port) = match player.port {
                game::Port::P1 => (Port::P0, 0usize, game::Port::P1),
                game::Port::P2 => (Port::P1, 1, game::Port::P2),
                game::Port::P3 => (Port::P2, 2, game::Port::P3),
                game::Port::P4 => (Port::P3, 3, game::Port::P4),
            };
            placements[i] = SlippiPlayer::new_slippi_player(metadata, port);
            stocks_remaining[i] = final_stocks_for_port(game, port_idx);
            starting_stocks[i] = starting_stocks_for_port(game, peppi_port);
            inputs[i] = inputs_for_port(game, port_idx);
            if let Some((att, suc)) = l_cancel_counts_for_port(game, port_idx) {
                l_cancel_attempts[i] = Some(att);
                l_cancel_success[i] = Some(suc);
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
        let punishes = extract_punishes_1v1(game).unwrap_or_default();

        // Advanced combat stats, same 1v1-only best-effort contract: a non-1v1
        // game or sparse frame data yields `None` and the rows store NULLs.
        let advanced = compute_advanced_stats_1v1(game).ok();

        Ok(GameData {
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

    pub fn placements(&self) -> &[Option<SlippiPlayer>; 4] {
        &self.placements
    }

    pub fn stage(&self) -> i32 {
        self.stage
    }

    pub fn time(&self) -> i32 {
        self.time
    }

    /// Returns the 1st-place finisher, if any.
    pub fn winner(&self) -> Option<&SlippiPlayer> {
        self.placements[0].as_ref()
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, TryFrom, Into, Display)]
#[repr(i32)]
pub enum Port {
    P0,
    P1,
    P2,
    P3,
}

#[derive(Debug)]
pub struct SlippiPlayer {
    pub netplay: String,
    pub code: String,
    pub character: i32,
    pub port: Port,
}

impl SlippiPlayer {
    pub fn new_slippi_player(
        metadata: &map::Map<string::String, value::Value>,
        port: Port,
    ) -> Option<SlippiPlayer> {
        let port_int: i32 = port.into();
        let port_string = port.to_string();

        let netplay = match &metadata["players"][&port_string]["names"]["netplay"] {
            value::Value::String(n) => n.clone(),
            _ => {
                return None;
            }
        };

        let code = match &metadata["players"][&port_string]["names"]["code"] {
            value::Value::String(c) => c.clone(),
            _ => {
                return None;
            }
        };

        let characters = &metadata["players"][&port_string]["characters"].as_object();

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

    pub fn port(&self) -> Port {
        self.port
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn port_roundtrip_int() {
        for (p, expected) in [
            (Port::P0, 0),
            (Port::P1, 1),
            (Port::P2, 2),
            (Port::P3, 3),
        ] {
            let as_int: i32 = p.into();
            assert_eq!(as_int, expected);
            let back: Port = Port::try_from(expected).expect("try_from");
            assert_eq!(back, p);
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

