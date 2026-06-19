use crate::schema::{game, gamePlayer, game_player_stat, player, punish, stage, character};
use diesel::prelude::*;

#[derive(Queryable, Selectable, Debug)]
#[diesel(table_name = crate::schema::game)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
pub struct Game {
    pub id: i32,
    pub first: Option<i32>,
    pub second: Option<i32>,
    pub third: Option<i32>,
    pub fourth: Option<i32>,
    pub stage: i32,
    pub time: i32,
    /// Filesystem path (as a string) of the .slp this game was ingested
    /// from. `None` for rows migrated in before the column existed.
    pub replay_path: Option<String>,
    /// ISO-8601 UTC timestamp recorded when the row was inserted, e.g.
    /// `"2026-04-24 18:02:11"`. Populated by SQLite via
    /// `DEFAULT CURRENT_TIMESTAMP` — callers never write it.
    pub ingested_at: String,
    /// Hex-encoded SHA-256 of the .slp file's bytes, computed at
    /// ingestion. Cache key for the analysis sidecar (Track 11).
    /// `None` for rows ingested before the column existed and for
    /// tests that synthesize a `GameData` without a backing file.
    pub content_hash: Option<String>,
    /// ISO-8601 timestamp of when the game was played, from the Slippi
    /// metadata `startAt`. `None` for legacy rows and replays without a
    /// usable metadata date.
    pub started_at: Option<String>,
}

#[derive(Queryable, Selectable)]
#[diesel(table_name = crate::schema::player)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
pub struct Player {
    pub netplay: String,
    pub code: String,
}

#[derive(Queryable, Selectable)]
#[diesel(table_name = crate::schema::gamePlayer)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
pub struct GamePlayer {
    pub id: i32,
    pub code: String,
    pub character: i32,
    pub port: i32,
}

#[derive(Queryable, Selectable)]
#[diesel(table_name = crate::schema::stage)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
pub struct Stage {
    pub id: i32,
    pub name: String,
}

#[derive(Queryable, Selectable)]
#[diesel(table_name = crate::schema::character)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
pub struct Character {
    pub id: i32,
    pub name: String,
}

#[derive(Insertable)]
#[diesel(table_name = player)]
pub struct NewPlayer<'a> {
    pub code: &'a str,
    pub netplay: &'a str,
}

#[derive(Insertable)]
#[diesel(table_name = gamePlayer)]
pub struct NewGamePlayer<'a> {
    pub code: &'a str,
    pub character: i32,
    pub port: i32,
}

#[derive(Insertable)]
#[diesel(table_name = game)]
pub struct NewGame<'a> {
    pub first: Option<i32>,
    pub second: Option<i32>,
    pub third: Option<i32>,
    pub fourth: Option<i32>,
    pub stage: i32,
    pub time: i32,
    /// `None` for "not tracked" / tests. Real ingestion always passes
    /// `Some(canonical_path)` so the UNIQUE index can kill duplicates.
    pub replay_path: Option<&'a str>,
    /// `None` for tests / non-file-backed inserts. Real ingestion
    /// passes `Some(<hex SHA-256>)` so the analysis sidecar cache
    /// (Track 11) has a stable key.
    pub content_hash: Option<&'a str>,
    /// ISO-8601 play timestamp from the .slp metadata `startAt`, or
    /// `None` when absent.
    pub started_at: Option<&'a str>,
}

#[derive(Insertable)]
#[diesel(table_name = stage)]
pub struct NewStage {
    pub id: i32,
    pub name: String,
}


#[derive(Insertable)]
#[diesel(table_name = character)]
pub struct NewCharacter {
    pub id: i32,
    pub name: String,
}

#[derive(Queryable, Selectable, Debug, Clone)]
#[diesel(table_name = crate::schema::game_player_stat)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
pub struct GamePlayerStat {
    pub id: i32,
    pub game_id: i32,
    pub game_player_id: i32,
    pub placement: i32,
    pub stocks_remaining: Option<i32>,
    /// Stocks each player started the game with. Usually 4 in stock matches,
    /// but stored per-row because handicapped / timed matches differ.
    pub starting_stocks: Option<i32>,
    /// Count of input state transitions across pre-frames. Used as the
    /// numerator for APM: APM = inputs / (time_seconds / 60).
    pub inputs: Option<i32>,
    /// Number of aerial-landing frames where peppi's `l_cancel` flag was
    /// non-zero (i.e. the game registered an L-cancel attempt — 1 success
    /// or 2 failure).
    pub l_cancel_attempts: Option<i32>,
    /// Number of aerial-landing frames with `l_cancel == 1` (successful
    /// L-cancel). `l_cancel_success / l_cancel_attempts` is the rate.
    pub l_cancel_success: Option<i32>,
    // --- Advanced per-game combat metrics (see `crate::advanced`) ---------
    /// Total percent dealt to the opponent across this game.
    pub damage_dealt: Option<f64>,
    /// Conversions started (denominator for damage-per-opening).
    pub openings: Option<i32>,
    /// Openings that began from neutral.
    pub neutral_wins: Option<i32>,
    /// Frames this player held the advantage (for stage control %).
    pub adv_frames: Option<i32>,
    /// Punishes where the opponent was offstage (edge-guard attempts).
    pub edgeguard_attempts: Option<i32>,
    /// Edge-guard attempts that killed (successes).
    pub edgeguard_kills: Option<i32>,
    /// `1` if this player took the game's first stock, else `0`.
    pub first_blood: Option<i32>,
    /// Times this player lost a stock.
    pub deaths: Option<i32>,
    /// Sum of the percents this player died at (÷ `deaths` = avg death %).
    pub death_percent_sum: Option<f64>,
    /// `1` if this player won after trailing by >= 2 stocks, else `0`.
    pub comeback_win: Option<i32>,
}

#[derive(Insertable)]
#[diesel(table_name = game_player_stat)]
pub struct NewGamePlayerStat {
    pub game_id: i32,
    pub game_player_id: i32,
    pub placement: i32,
    pub stocks_remaining: Option<i32>,
    pub starting_stocks: Option<i32>,
    pub inputs: Option<i32>,
    pub l_cancel_attempts: Option<i32>,
    pub l_cancel_success: Option<i32>,
    pub damage_dealt: Option<f64>,
    pub openings: Option<i32>,
    pub neutral_wins: Option<i32>,
    pub adv_frames: Option<i32>,
    pub edgeguard_attempts: Option<i32>,
    pub edgeguard_kills: Option<i32>,
    pub first_blood: Option<i32>,
    pub deaths: Option<i32>,
    pub death_percent_sum: Option<f64>,
    pub comeback_win: Option<i32>,
}

/// One combo opportunity by one attacker against the lone opponent in a 1v1.
/// Built by `src/punish.rs` from frame data; stored alongside each game at
/// ingest time.
#[derive(Queryable, Selectable, Debug, Clone)]
#[diesel(table_name = crate::schema::punish)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
pub struct Punish {
    pub id: i32,
    pub game_id: i32,
    pub attacker_id: i32,
    pub victim_id: i32,
    pub start_frame: i32,
    pub end_frame: i32,
    pub hit_count: i32,
    pub did_kill: i32,
    pub kill_move: Option<i32>,
}

impl Punish {
    /// Semantic accessor — the column is stored as `INTEGER 0/1` because
    /// SQLite doesn't have a bool type.
    pub fn did_kill_bool(&self) -> bool {
        self.did_kill != 0
    }
}

#[derive(Insertable)]
#[diesel(table_name = punish)]
pub struct NewPunish {
    pub game_id: i32,
    pub attacker_id: i32,
    pub victim_id: i32,
    pub start_frame: i32,
    pub end_frame: i32,
    pub hit_count: i32,
    pub did_kill: i32,
    pub kill_move: Option<i32>,
}