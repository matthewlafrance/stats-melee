//! Win/loss breakdowns: who a player beat, on what stage, as which character.
//!
//! These are the queries behind the Analytics page. They read the `game`
//! row's placement-ordered player slots rather than `game_player_stat`,
//! because a win is a property of finishing position.

use anyhow::{anyhow, Result};
use diesel::prelude::*;
use std::collections::HashMap;

use crate::models::*;
use crate::analytics::{WinAnalytics, WinProportion};
use crate::{NUM_CHARACTERS, NUM_STAGES};

/// Every game the given player code appears in.
///
/// `DISTINCT` is load-bearing. The join matches a game when *any* of its four
/// player slots points at a `gamePlayer` row with this code, so a game where
/// two slots share a code matches twice and would be returned twice. Netplay
/// replays never hit that — a connect code appears once per game — but
/// anonymous replays (console recordings with no player metadata) all carry
/// the empty code, so every one of them matches on both slots.
pub fn filter_games(conn: &mut SqliteConnection, code: &str) -> Result<Vec<Game>> {
    use crate::schema::{game, gamePlayer};

    game::table
        .inner_join(
            gamePlayer::table.on(
                game::first
                    .eq(gamePlayer::id.nullable())
                    .or(game::second.eq(gamePlayer::id.nullable()))
                    .or(game::third.eq(gamePlayer::id.nullable()))
                    .or(game::fourth.eq(gamePlayer::id.nullable())),
            ),
        )
        .filter(gamePlayer::code.eq(code))
        .select(game::all_columns)
        .distinct()
        .load::<Game>(conn)
        .map_err(Into::into)
}

/// The `(code, character)` of every `gamePlayer` row in `ids`, in one query.
///
/// Chunked because SQLite caps how many parameters a single statement may
/// bind. In practice the distinct id set is small — `gamePlayer` rows are
/// deduped by `(code, character, port)` and shared across every game they
/// appear in, so a library of thousands of games references only a few
/// hundred distinct rows — but the chunking means that stays true even if
/// some future library breaks the assumption.
fn load_game_players(
    conn: &mut SqliteConnection,
    ids: &[i32],
) -> Result<HashMap<i32, (String, i32)>> {
    use crate::schema::gamePlayer::dsl as gp;

    const CHUNK: usize = 900;
    let mut out: HashMap<i32, (String, i32)> = HashMap::with_capacity(ids.len());
    for chunk in ids.chunks(CHUNK) {
        let rows: Vec<(i32, String, i32)> = gp::gamePlayer
            .filter(gp::id.eq_any(chunk))
            .select((gp::id, gp::code, gp::character))
            .load(conn)?;
        out.extend(rows.into_iter().map(|(id, code, ch)| (id, (code, ch))));
    }
    Ok(out)
}

/// The four player slots of a game, in placement order.
fn game_slots(game: &Game) -> [Option<i32>; 4] {
    [game.first, game.second, game.third, game.fourth]
}

pub fn analyze_games(conn: &mut SqliteConnection, games: &Vec<Game>, player_code: &str) -> Result<WinAnalytics> {
    use std::collections::HashSet;

    // Resolve every referenced player in one query up front. Doing it per
    // slot inside the loop would be two point lookups per player per game —
    // individually microseconds, but paid tens of thousands of times over a
    // large library.
    let ids: Vec<i32> = games
        .iter()
        .flat_map(game_slots)
        .flatten()
        .collect::<HashSet<i32>>()
        .into_iter()
        .collect();
    let players_by_id = load_game_players(conn, &ids)?;

    let mut opponents: HashMap<String, WinProportion> = HashMap::new();
    let mut stages = [WinProportion::new_winproportion(); NUM_STAGES];
    let mut played_characters = [WinProportion::new_winproportion(); NUM_CHARACTERS];
    let mut opp_characters = [WinProportion::new_winproportion(); NUM_CHARACTERS];

    for game in games {
        let mut codes: [Option<&str>; 4] = [None; 4];
        let mut characters: [Option<i32>; 4] = [None; 4];

        for (slot, gp_id) in game_slots(game).into_iter().enumerate() {
            let Some(gp_id) = gp_id else { continue };
            let (code, character) = players_by_id
                .get(&gp_id)
                .ok_or_else(|| anyhow!("game {} references missing gamePlayer {gp_id}", game.id))?;
            codes[slot] = Some(code.as_str());
            characters[slot] = Some(*character);
        }

        // `codes[0]` is the winner's slot: the `game` row stores players by
        // finishing position.
        let player_won = codes[0] == Some(player_code);

        for (code_option, character_option) in codes.iter().zip(characters.iter()) {
            if let Some(code) = code_option {
                let character = character_option.ok_or(anyhow!("no character found for player"))?;

                if *code != player_code {
                    let opps_winproportion = opponents
                        .entry((*code).to_string())
                        .or_insert(WinProportion::new_winproportion());

                    if player_won {
                        opps_winproportion.wins += 1;
                        opp_characters[character as usize].wins += 1;
                    }

                    opps_winproportion.total += 1;
                    opp_characters[character as usize].total += 1;
                } else {

                    if player_won {
                        played_characters[character as usize].wins += 1;
                    }

                    played_characters[character as usize].total += 1;
                }
            }
        }

        if player_won {
            stages[game.stage as usize].wins += 1;
        }

        stages[game.stage as usize].total += 1;
    }

    for opp_winproportion in opponents.values_mut() {
        opp_winproportion.update_proportion();
    }

    for stage_wp in stages.iter_mut() {
        stage_wp.update_proportion();
    }

    for (played, opp) in played_characters
        .iter_mut()
        .zip(opp_characters.iter_mut())
    {
        played.update_proportion();
        opp.update_proportion();
    }

    Ok(WinAnalytics {
        opponents,
        stages,
        played_characters,
        opp_characters,
    })
}

/// Convenience: fetch every game `code` appeared in and roll it up into a
/// [`WinAnalytics`] (win rates by played character, opponent-character
/// matchup, stage, and opponent code). Thin wrapper over
/// [`filter_games`] + [`analyze_games`] for callers that just want the
/// breakdown for one player.
pub fn win_analytics(conn: &mut SqliteConnection, code: &str) -> Result<WinAnalytics> {
    let games = filter_games(conn, code)?;
    analyze_games(conn, &games, code)
}

/// Per-stage win/loss split for the games where `code` played
/// `character_id`. Each entry is `(stage_id, WinProportion)`; sorted by
/// games played descending. Powers the Analytics "By stage" cross-breakdown
/// shown when a character is selected with no stage.
///
/// A "win" is `game_player_stat.placement == 0` (first place / the 1v1
/// winner), the same definition [`player_summary_filtered`] uses.
pub fn win_by_stage_for_character(
    conn: &mut SqliteConnection,
    code: &str,
    character_id: i32,
) -> Result<Vec<(i32, WinProportion)>> {
    use crate::schema::{game, gamePlayer, game_player_stat};

    let rows: Vec<(i32, i32)> = gamePlayer::table
        .inner_join(game_player_stat::table.on(game_player_stat::game_player_id.eq(gamePlayer::id)))
        .inner_join(game::table.on(game::id.eq(game_player_stat::game_id)))
        .filter(gamePlayer::code.eq(code))
        .filter(gamePlayer::character.eq(character_id))
        .select((game::stage, game_player_stat::placement))
        .load(conn)
        ?;

    Ok(group_win_proportions(rows))
}

/// Per-character win/loss split for the games `code` played on `stage_id`.
/// Each entry is `(character_id, WinProportion)`; sorted by games played
/// descending. Powers the Analytics "By character" cross-breakdown shown
/// when a stage is selected with no character.
pub fn win_by_character_for_stage(
    conn: &mut SqliteConnection,
    code: &str,
    stage_id: i32,
) -> Result<Vec<(i32, WinProportion)>> {
    use crate::schema::{game, gamePlayer, game_player_stat};

    let rows: Vec<(i32, i32)> = gamePlayer::table
        .inner_join(game_player_stat::table.on(game_player_stat::game_player_id.eq(gamePlayer::id)))
        .inner_join(game::table.on(game::id.eq(game_player_stat::game_id)))
        .filter(gamePlayer::code.eq(code))
        .filter(game::stage.eq(stage_id))
        .select((gamePlayer::character, game_player_stat::placement))
        .load(conn)
        ?;

    Ok(group_win_proportions(rows))
}

/// Fold `(group_id, placement)` rows into per-group [`WinProportion`]s,
/// sorted by games played (descending). Shared by the two cross-breakdown
/// queries above; `placement == 0` counts as a win.
fn group_win_proportions(rows: Vec<(i32, i32)>) -> Vec<(i32, WinProportion)> {
    let mut map: HashMap<i32, WinProportion> = HashMap::new();
    for (group, placement) in rows {
        let wp = map.entry(group).or_insert_with(WinProportion::new_winproportion);
        if placement == 0 {
            wp.wins += 1;
        }
        wp.total += 1;
    }
    let mut out: Vec<(i32, WinProportion)> = map
        .into_iter()
        .map(|(g, mut wp)| {
            wp.update_proportion();
            (g, wp)
        })
        .collect();
    out.sort_by(|a, b| b.1.total.cmp(&a.1.total));
    out
}

pub fn get_game_player_code(conn: &mut SqliteConnection, find_id: i32) -> Result<String> {
    use crate::schema::gamePlayer::dsl::*;

    gamePlayer.filter(id.eq(find_id)).select(code).first::<String>(conn).map_err(Into::into)
}

pub fn get_character(conn: &mut SqliteConnection, find_id: i32) -> Result<i32> {
    use crate::schema::gamePlayer::dsl::*;

    gamePlayer.filter(id.eq(find_id)).select(character).first::<i32>(conn).map_err(Into::into)
}


#[cfg(test)]
mod cross_breakdown_tests {
    use super::*;

    #[test]
    fn group_win_proportions_counts_and_sorts() {
        // (group_id, placement) rows. placement 0 = win.
        // group 1: 3 games, 2 wins. group 2: 5 games, 1 win.
        let rows = vec![
            (1, 0),
            (1, 0),
            (1, 1),
            (2, 1),
            (2, 0),
            (2, 1),
            (2, 1),
            (2, 1),
        ];
        let out = group_win_proportions(rows);
        // Sorted by total descending → group 2 (5) before group 1 (3).
        assert_eq!(out[0].0, 2);
        assert_eq!(out[0].1.wins, 1);
        assert_eq!(out[0].1.total, 5);
        assert!((out[0].1.proportion - 0.2).abs() < 1e-6);

        assert_eq!(out[1].0, 1);
        assert_eq!(out[1].1.wins, 2);
        assert_eq!(out[1].1.total, 3);
    }

    #[test]
    fn group_win_proportions_empty_input() {
        assert!(group_win_proportions(Vec::new()).is_empty());
    }
}

