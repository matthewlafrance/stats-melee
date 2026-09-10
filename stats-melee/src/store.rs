//! Writing games into the database.
//!
//! [`post_game_full`] is the entry point ingestion uses: one transaction per
//! game covering the player upserts, the game row, per-player stats, and the
//! punish rows. The `insert_or_get_*` helpers give the upsert semantics the
//! `player` and `gamePlayer` tables need, and the `nuke_*` functions are the
//! matching teardown.

use anyhow::Result;
use diesel::prelude::*;

use crate::models::*;
use crate::gamedata::{GameData, SlippiPlayer};

pub fn post_player(conn: &mut SqliteConnection, slippi_player: &SlippiPlayer) -> Result<Player> {
    

    let new_player = NewPlayer {
        netplay: slippi_player.netplay(),
        code: slippi_player.code(),
    };

    Ok(insert_or_get_player(conn, &new_player)?)
}

pub fn post_game_player(conn: &mut SqliteConnection, slippi_player: &SlippiPlayer) -> Result<GamePlayer> {
    

    post_player(conn, slippi_player)?;


    let new_game_player = NewGamePlayer {
        code: slippi_player.code(),
        character: slippi_player.character(),
        port: slippi_player.port().into(),
    };

    Ok(insert_or_get_game_player(conn, &new_game_player)?)
}

/// Insert a game + its derived rows. See [`post_game_with_path`] — this is
/// the legacy signature that doesn't record a canonical replay path. Used
/// by tests that synthesize `GameData` directly; production ingestion goes
/// through `post_game_full` so duplicates are caught by the UNIQUE
/// index on `game.replay_path`.
pub fn post_game(conn: &mut SqliteConnection, gamedata: &GameData) -> Result<Game> {
    post_game_full(conn, gamedata, None, None)
}

/// Same as [`post_game`] but records `replay_path` on the game row.
/// Convenience wrapper for callers that don't have a content_hash —
/// production ingestion should use [`post_game_full`].
pub fn post_game_with_path(
    conn: &mut SqliteConnection,
    gamedata: &GameData,
    replay_path: Option<&str>,
) -> Result<Game> {
    post_game_full(conn, gamedata, replay_path, None)
}

/// Insert a game + its derived rows, recording both `replay_path` and
/// `content_hash`. The full ingestion path. Callers should canonicalize
/// the path (e.g. via `fs::canonicalize` or `std::path::absolute`) before
/// calling so dedup works across differing relative-path representations
/// of the same file. `content_hash` should be the hex-encoded SHA-256 of
/// the .slp file's bytes — see [`hash_slp_file`].
pub fn post_game_full(
    conn: &mut SqliteConnection,
    gamedata: &GameData,
    replay_path: Option<&str>,
    content_hash: Option<&str>,
) -> Result<Game> {
    use crate::schema::game;

    // One transaction for the whole game, for two reasons.
    //
    // Speed: a game writes two player upserts, two gamePlayer upserts, the
    // game row, two stat rows, and one row per punish — on the order of fifty
    // statements. Left in autocommit each is its own WAL commit, and those
    // commits dominate the cost of ingestion.
    //
    // Atomicity: a game either lands whole or not at all. Callers log and
    // skip a failed game and move on, which is only safe if failure leaves
    // nothing behind — no game row with half its stats and punishes.
    conn.transaction::<Game, anyhow::Error, _>(|conn| {

    // Insert (or fetch) the gamePlayer row for every populated port up
    // front, so we can thread the ids through the `game` row, the
    // `game_player_stat` rows, and the punish rows below. Keyed by port,
    // which is what the frame-derived data (`advanced`, `punishes`) uses.
    let gp_by_port: [Option<i32>; 4] = std::array::from_fn(|port| {
        gamedata.players[port]
            .as_ref()
            .and_then(|p| post_game_player(conn, p).ok())
            .map(|gp| gp.id)
    });

    // The `game` row stores players by finishing position, so translate
    // through `placements` (rank -> port) exactly once, here.
    let player_ids: [Option<i32>; 4] =
        std::array::from_fn(|rank| gamedata.placements[rank].and_then(|port| gp_by_port[port]));

    let new_game = NewGame {
        first: player_ids[0],
        second: player_ids[1],
        third: player_ids[2],
        fourth: player_ids[3],
        stage: gamedata.stage(),
        time: gamedata.time(),
        replay_path,
        content_hash,
        started_at: gamedata.started_at.as_deref(),
    };

    let inserted_game: Game = diesel::insert_into(game::table)
        .values(&new_game)
        .returning(Game::as_returning())
        .get_result(conn)
        ?;

    // One game_player_stat row per populated port. Everything read here is
    // port-indexed, so there's no placement/port juggling left — the single
    // `placement_of_port` call is the only crossing, and it feeds the column
    // that is by definition placement-shaped.
    for (port, gp_id) in gp_by_port.iter().enumerate() {
        let Some(gp_id) = *gp_id else {
            continue;
        };
        let Some(placement) = gamedata.placement_of_port(port) else {
            continue;
        };

        // Advanced stats carry their own port indices; match this port
        // against them. `None` for non-1v1 games (no advanced analysis)
        // stores NULLs across the board.
        let adv = gamedata.advanced.as_ref().and_then(|a| {
            if a.p1_port_idx == port {
                Some(a.p1)
            } else if a.p2_port_idx == port {
                Some(a.p2)
            } else {
                None
            }
        });

        let stat = NewGamePlayerStat {
            game_id: inserted_game.id,
            game_player_id: gp_id,
            placement: placement as i32,
            stocks_remaining: gamedata.stocks_remaining[port],
            starting_stocks: gamedata.starting_stocks[port],
            inputs: gamedata.inputs[port],
            l_cancel_attempts: gamedata.l_cancel_attempts[port],
            l_cancel_success: gamedata.l_cancel_success[port],
            damage_dealt: adv.map(|a| a.damage_dealt),
            openings: adv.map(|a| a.openings),
            neutral_wins: adv.map(|a| a.neutral_wins),
            adv_frames: adv.map(|a| a.adv_frames),
            edgeguard_attempts: adv.map(|a| a.edgeguard_attempts),
            edgeguard_kills: adv.map(|a| a.edgeguard_kills),
            first_blood: adv.map(|a| i32::from(a.first_blood)),
            deaths: adv.map(|a| a.deaths),
            death_percent_sum: adv.map(|a| a.death_percent_sum),
            comeback_win: adv.map(|a| i32::from(a.comeback_win)),
        };
        post_game_player_stat(conn, &stat)?;
    }

    // Persist each RawPunish as a punish row. Skip any punish whose attacker
    // or victim port didn't resolve to a gamePlayer (shouldn't happen in
    // practice for 1v1 replays, but belt-and-suspenders).
    //
    // Collected and inserted as one multi-row statement rather than one
    // statement per punish. A game carries tens of these, and they were the
    // bulk of the round trips in this function.
    let new_punishes: Vec<NewPunish> = gamedata
        .punishes
        .iter()
        .filter_map(|raw| {
            let attacker = gp_by_port.get(raw.attacker_port_idx).copied().flatten()?;
            let victim = gp_by_port.get(raw.victim_port_idx).copied().flatten()?;
            Some(NewPunish {
                game_id: inserted_game.id,
                attacker_id: attacker,
                victim_id: victim,
                start_frame: raw.start_frame,
                end_frame: raw.end_frame,
                hit_count: raw.hit_count,
                did_kill: if raw.did_kill { 1 } else { 0 },
                kill_move: raw.kill_move,
            })
        })
        .collect();

    if !new_punishes.is_empty() {
        // `execute` rather than `get_result`: nothing here needs the
        // inserted rows back, so don't pay to read them.
        diesel::insert_into(crate::schema::punish::table)
            .values(&new_punishes)
            .execute(conn)
            ?;
    }

    Ok(inserted_game)
    })
}

/// Insert a row into `game_player_stat`. Does not attempt conflict resolution —
/// callers should ensure uniqueness via the `(game_id, game_player_id)` pair.
pub fn post_game_player_stat(
    conn: &mut SqliteConnection,
    new_stat: &NewGamePlayerStat,
) -> Result<GamePlayerStat> {
    use crate::schema::game_player_stat;

    diesel::insert_into(game_player_stat::table)
        .values(new_stat)
        .returning(GamePlayerStat::as_returning())
        .get_result(conn)
        .map_err(Into::into)
}


pub fn post_stage(conn: &mut SqliteConnection, id: i32, name: String) -> Result<Stage> {
    use crate::schema::stage;

    let new_stage = NewStage{ id, name };

    diesel::insert_into(stage::table)
        .values(&new_stage)
        .returning(Stage::as_returning())
        .get_result(conn)
        .map_err(Into::into)
}

pub fn post_character(conn: &mut SqliteConnection, id: i32, name: String) -> Result<Character> {
    use crate::schema::character;

    let new_character = NewCharacter{ id, name };

    diesel::insert_into(character::table)
        .values(&new_character)
        .returning(Character::as_returning())
        .get_result(conn)
        .map_err(Into::into)
}

pub fn insert_or_get_player(conn: &mut SqliteConnection, new_player: &NewPlayer) -> diesel::result::QueryResult<Player> {
    use crate::schema::player::dsl::*;

    if let Some(inserted) = diesel::insert_into(player)
        .values(new_player)
        .on_conflict(code)
        .do_nothing()
        .returning(Player::as_returning())
        .get_result(conn)
        .optional()? 
    {
        Ok(inserted)
    } else {
        player
            .filter(netplay.eq(&new_player.netplay))
            .first::<Player>(conn)
    }
}

pub fn insert_or_get_game_player(conn: &mut SqliteConnection, new_game_player: &NewGamePlayer) -> diesel::result::QueryResult<GamePlayer> {
    use crate::schema::gamePlayer::dsl::*;

    if let Some(inserted) = diesel::insert_into(gamePlayer)
        .values(new_game_player)
        .on_conflict((code, character, port))
        .do_nothing()
        .returning(GamePlayer::as_returning())
        .get_result(conn)
        .optional()? 
    {
        Ok(inserted)
    } else {
        gamePlayer
            .filter(code.eq(&new_game_player.code))
            .filter(character.eq(new_game_player.character))
            .filter(port.eq(new_game_player.port))
            .first::<GamePlayer>(conn)
    }
}

/// Delete every replay-scoped row: `punish`, `game_player_stat`,
/// `gamePlayer`, and `game`. Metadata tables (`character`, `stage`,
/// `player`) are intentionally preserved — they're shared lookup data
/// that survives multiple ingestions.
///
/// Returns the number of `game` rows removed. Runs inside a single
/// transaction so a partial nuke can't leave the DB with orphaned
/// gamePlayer rows referenced by a missing game, etc.
///
/// This is a destructive operation — the caller (e.g. the GUI) should
/// put a confirmation prompt in front of it.
pub fn nuke_replays(conn: &mut SqliteConnection) -> Result<usize> {
    use crate::schema::{gamePlayer, game, game_player_stat, punish};

    conn.transaction::<usize, anyhow::Error, _>(|conn| {
        // Delete order is load-bearing: FK enforcement IS on. Stock SQLite
        // defaults `foreign_keys` to OFF, but we build it through
        // `libsqlite3-sys`'s `bundled` feature, which compiles with
        // `-DSQLITE_DEFAULT_FOREIGN_KEYS=1`. So every declared constraint is
        // checked, and each table must go after everything referencing it.
        //
        //   punish            -> game, gamePlayer
        //   game_player_stat  -> game, gamePlayer
        //   game              -> gamePlayer   (first/second/third/fourth)
        //   gamePlayer        -> player
        //
        // `game` therefore has to be deleted BEFORE `gamePlayer`; the reverse
        // fails with "FOREIGN KEY constraint failed" against any non-empty
        // library. `player` rows are intentionally left alone — they outlive
        // any individual replay, same as in `nuke_replay`.
        diesel::delete(punish::table).execute(conn)?;
        diesel::delete(game_player_stat::table).execute(conn)?;
        let deleted = diesel::delete(game::table).execute(conn)?;
        diesel::delete(gamePlayer::table).execute(conn)?;

        Ok(deleted)
    })
}

/// Delete a single replay's rows from `punish`, `game_player_stat`,
/// and `game`. Unlike [`nuke_replays`], this is the per-row delete
/// path used by the library table's "🗑" button.
///
/// `gamePlayer` rows are deliberately *not* touched — they're
/// shared cross-game identities (the same `(code, character, port)`
/// tuple gets reused across many games), and removing them here
/// would either orphan other games' joins or require a "is this
/// gamePlayer still referenced anywhere?" check we don't need.
/// Leaving them around is correct; the next ingestion of the same
/// player just looks them up by the unique constraint and reuses.
///
/// Returns the number of `game` rows removed (`1` on success, `0`
/// when no game with that id existed). Runs inside a single
/// transaction so a partial delete can't leave punish rows referring
/// to a missing game.
pub fn nuke_replay(conn: &mut SqliteConnection, target_game_id: i32) -> Result<usize> {
    use crate::schema::{game, game_player_stat, punish};

    conn.transaction::<usize, anyhow::Error, _>(|conn| {
        diesel::delete(punish::table.filter(punish::game_id.eq(target_game_id)))
            .execute(conn)?;
        diesel::delete(
            game_player_stat::table.filter(game_player_stat::game_id.eq(target_game_id)),
        )
        .execute(conn)?;
        let deleted = diesel::delete(game::table.filter(game::id.eq(target_game_id)))
            .execute(conn)?;
        Ok(deleted)
    })
}
