//! Per-player statistics, filterable by character, stage, and game set.
//!
//! Every query here narrows the same `gamePlayer -> game_player_stat ->
//! game` join with [`PlayerSummaryFilter`], applied through the
//! `narrow_by_filter!` macro so the three optional predicates are written
//! once. [`player_summary_filtered`] rolls the individual figures into the
//! bundle the Career page renders.

use anyhow::Result;
use diesel::prelude::*;
use std::collections::HashMap;

use crate::models::*;

/// All per-player stats recorded for a given game.
pub fn get_stats_for_game(
    conn: &mut SqliteConnection,
    game_id_filter: i32,
) -> Result<Vec<GamePlayerStat>> {
    use crate::schema::game_player_stat::dsl::*;

    game_player_stat
        .filter(game_id.eq(game_id_filter))
        .order(placement.asc())
        .load::<GamePlayerStat>(conn)
        .map_err(Into::into)
}
/// Narrow a boxed query by a [`PlayerSummaryFilter`].
///
/// Every filterable stat query builds the same `gamePlayer -> game_player_stat
/// -> game` join and narrows it by the same three optional predicates; only
/// the select and the roll-up differ. Keeping the predicates here means a
/// fourth filter dimension is added once rather than at a dozen call sites.
///
/// Takes the query binding by name and rebinds it, so it drops into a
/// `let mut q = ...;` whatever that binding is called.
macro_rules! narrow_by_filter {
    ($q:ident, $filter:expr) => {
        if let Some(cid) = $filter.character_id {
            $q = $q.filter(crate::schema::gamePlayer::character.eq(cid));
        }
        if let Some(sid) = $filter.stage_id {
            $q = $q.filter(crate::schema::game::stage.eq(sid));
        }
        if let Some(ids) = &$filter.game_ids {
            $q = $q.filter(crate::schema::game::id.eq_any(ids.clone()));
        }
    };
}


/// Optional `(character, stage)` filter applied across every per-code
/// aggregate. `None` for either field means "any" — `Default::default()`
/// is "no filter".
///
/// Used by [`player_summary_filtered`] and the `_filtered` variant of
/// each per-code aggregate. The Analytics page selectors translate
/// directly into one of these structs.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PlayerSummaryFilter {
    /// `gamePlayer.character` value to require, or `None` for any.
    /// Index into [`gamedata::CHARACTERS`].
    pub character_id: Option<i32>,
    /// `game.stage` value to require, or `None` for any.
    /// Index into [`gamedata::STAGES`].
    pub stage_id: Option<i32>,
    /// Restrict every aggregate to this explicit set of `game.id`s, or
    /// `None` for no restriction. This is how the GUI threads its full
    /// multi-dimensional library filter (opponent character, outcome, date
    /// ranges, opponent tag — none of which this struct models directly)
    /// into the per-code aggregates: the GUI computes the matching game-id
    /// set in-memory and hands it down, so the stats reflect exactly the
    /// games the library is showing. Combined with `character_id`/`stage_id`
    /// via AND when all are set.
    pub game_ids: Option<Vec<i32>>,
}

impl PlayerSummaryFilter {
    /// No filter (matches any character on any stage, any game). Equivalent
    /// to [`PlayerSummaryFilter::default()`] but lets callers write a
    /// `const`-friendly literal at the call site.
    pub const NONE: Self = Self {
        character_id: None,
        stage_id: None,
        game_ids: None,
    };
}

/// Average placement (0-indexed; 0 = first place) across every game where
/// `player_code` appeared. Returns `None` if the player has no stat rows yet.
pub fn avg_placement_by_code(
    conn: &mut SqliteConnection,
    player_code: &str,
) -> Result<Option<f64>> {
    avg_placement_filtered(conn, player_code, &PlayerSummaryFilter::NONE)
}

/// Filterable variant of [`avg_placement_by_code`].
///
/// `filter.character_id` constrains to games where the player used that
/// character; `filter.stage_id` constrains to games on that stage. Both
/// `None` reproduces [`avg_placement_by_code`] exactly.
pub fn avg_placement_filtered(
    conn: &mut SqliteConnection,
    player_code: &str,
    filter: &PlayerSummaryFilter,
) -> Result<Option<f64>> {
    use crate::schema::{game, gamePlayer, game_player_stat};

    // Averaging Rust-side to avoid pulling in the BigDecimal-backed
    // `diesel::dsl::avg`, which requires the `numeric` feature.
    //
    // The `game` join is unconditional even when no stage filter is set —
    // the cost of the extra index lookup is negligible and it keeps the
    // boxed query type uniform across both filter cases.
    let mut q = gamePlayer::table
        .inner_join(game_player_stat::table.on(game_player_stat::game_player_id.eq(gamePlayer::id)))
        .inner_join(game::table.on(game::id.eq(game_player_stat::game_id)))
        .filter(gamePlayer::code.eq(player_code))
        .into_boxed();
    narrow_by_filter!(q, filter);
    let placements: Vec<i32> = q
        .select(game_player_stat::placement)
        .load(conn)
        ?;

    if placements.is_empty() {
        return Ok(None);
    }
    let sum: i64 = placements.iter().map(|&p| p as i64).sum();
    Ok(Some(sum as f64 / placements.len() as f64))
}

/// Average stocks remaining at game end for `player_code`. NULL rows (no frame
/// data) are excluded from the average. Returns `None` if the player has no
/// rows with populated stocks.
pub fn avg_stocks_remaining(
    conn: &mut SqliteConnection,
    player_code: &str,
) -> Result<Option<f64>> {
    avg_stocks_remaining_filtered(conn, player_code, &PlayerSummaryFilter::NONE)
}

/// Filterable variant of [`avg_stocks_remaining`].
pub fn avg_stocks_remaining_filtered(
    conn: &mut SqliteConnection,
    player_code: &str,
    filter: &PlayerSummaryFilter,
) -> Result<Option<f64>> {
    use crate::schema::{game, gamePlayer, game_player_stat};

    let mut q = gamePlayer::table
        .inner_join(game_player_stat::table.on(game_player_stat::game_player_id.eq(gamePlayer::id)))
        .inner_join(game::table.on(game::id.eq(game_player_stat::game_id)))
        .filter(gamePlayer::code.eq(player_code))
        .into_boxed();
    narrow_by_filter!(q, filter);
    let stocks: Vec<Option<i32>> = q
        .select(game_player_stat::stocks_remaining)
        .load(conn)
        ?;

    let valid: Vec<i32> = stocks.into_iter().flatten().collect();
    if valid.is_empty() {
        return Ok(None);
    }
    let sum: i64 = valid.iter().map(|&s| s as i64).sum();
    Ok(Some(sum as f64 / valid.len() as f64))
}

/// Average APM (inputs per minute) for `player_code`.
///
/// APM = total_inputs / total_minutes, where each game contributes its own
/// input count and duration. Rows missing either field are skipped.
pub fn avg_apm_by_code(
    conn: &mut SqliteConnection,
    player_code: &str,
) -> Result<Option<f64>> {
    avg_apm_filtered(conn, player_code, &PlayerSummaryFilter::NONE)
}

/// Filterable variant of [`avg_apm_by_code`].
pub fn avg_apm_filtered(
    conn: &mut SqliteConnection,
    player_code: &str,
    filter: &PlayerSummaryFilter,
) -> Result<Option<f64>> {
    use crate::schema::{game, gamePlayer, game_player_stat};

    let mut q = game_player_stat::table
        .inner_join(gamePlayer::table.on(gamePlayer::id.eq(game_player_stat::game_player_id)))
        .inner_join(game::table.on(game::id.eq(game_player_stat::game_id)))
        .filter(gamePlayer::code.eq(player_code))
        .into_boxed();
    narrow_by_filter!(q, filter);
    let rows: Vec<(Option<i32>, i32)> = q
        .select((game_player_stat::inputs, game::time))
        .load(conn)
        ?;

    let mut total_inputs: i64 = 0;
    let mut total_seconds: i64 = 0;
    for (maybe_inputs, time_seconds) in rows {
        if let Some(n) = maybe_inputs {
            total_inputs += n as i64;
            total_seconds += time_seconds as i64;
        }
    }
    if total_seconds <= 0 {
        return Ok(None);
    }
    let minutes = total_seconds as f64 / 60.0;
    Ok(Some(total_inputs as f64 / minutes))
}

/// Overall L-cancel success rate for `player_code` (0.0..=1.0).
///
/// Sums attempts + successes across every game; returns `None` if the player
/// never landed an aerial (attempts == 0) or has no stat rows.
pub fn l_cancel_rate_by_code(
    conn: &mut SqliteConnection,
    player_code: &str,
) -> Result<Option<f64>> {
    l_cancel_rate_filtered(conn, player_code, &PlayerSummaryFilter::NONE)
}

/// Filterable variant of [`l_cancel_rate_by_code`].
pub fn l_cancel_rate_filtered(
    conn: &mut SqliteConnection,
    player_code: &str,
    filter: &PlayerSummaryFilter,
) -> Result<Option<f64>> {
    use crate::schema::{game, gamePlayer, game_player_stat};

    let mut q = gamePlayer::table
        .inner_join(game_player_stat::table.on(game_player_stat::game_player_id.eq(gamePlayer::id)))
        .inner_join(game::table.on(game::id.eq(game_player_stat::game_id)))
        .filter(gamePlayer::code.eq(player_code))
        .into_boxed();
    narrow_by_filter!(q, filter);
    let rows: Vec<(Option<i32>, Option<i32>)> = q
        .select((
            game_player_stat::l_cancel_attempts,
            game_player_stat::l_cancel_success,
        ))
        .load(conn)
        ?;

    let mut attempts: i64 = 0;
    let mut successes: i64 = 0;
    for (a, s) in rows {
        if let (Some(a), Some(s)) = (a, s) {
            attempts += a as i64;
            successes += s as i64;
        }
    }
    if attempts == 0 {
        return Ok(None);
    }
    Ok(Some(successes as f64 / attempts as f64))
}

/// Average stocks *taken* (i.e. opponent stocks removed) per 1v1 game for
/// `player_code`.
///
/// Only 1v1 games contribute — we pair the player's row with the single other
/// row and compute `opponent.starting_stocks - opponent.stocks_remaining`.
/// 3v and 4v games are ambiguous (who took which stock?) so they're skipped.
pub fn avg_stocks_taken_by_code(
    conn: &mut SqliteConnection,
    player_code: &str,
) -> Result<Option<f64>> {
    avg_stocks_taken_filtered(conn, player_code, &PlayerSummaryFilter::NONE)
}

/// Per-game stock swings for every 1v1 game matching the filter.
///
/// Each entry is `(stocks the opponent lost, stocks the player lost)` for one
/// game, with `None` on a side whose stock data is missing. Games that aren't
/// exactly two players are skipped — stocks taken is only meaningful when
/// there is one opponent to take them from.
///
/// Both callers need the same two queries to get here: one to find the games
/// the filter selects (matched against the *player's* row, not the
/// opponent's), and one to pull both sides of each of those games. They
/// differ only in how they roll the results up.
/// One row of `(player code, starting stocks, stocks remaining)` — the shape
/// [`stock_swings_filtered`] reads out of `game_player_stat` for each side of
/// a game.
type StockRow = (String, Option<i32>, Option<i32>);

fn stock_swings_filtered(
    conn: &mut SqliteConnection,
    player_code: &str,
    filter: &PlayerSummaryFilter,
) -> Result<Vec<(Option<i64>, Option<i64>)>> {
    use crate::schema::{game, gamePlayer, game_player_stat};

    // Step 1: find every game the player appeared in, with the filter
    // applied to their row.
    let mut id_q = gamePlayer::table
        .inner_join(game_player_stat::table.on(game_player_stat::game_player_id.eq(gamePlayer::id)))
        .inner_join(game::table.on(game::id.eq(game_player_stat::game_id)))
        .filter(gamePlayer::code.eq(player_code))
        .into_boxed();
    narrow_by_filter!(id_q, filter);
    let game_ids: Vec<i32> = id_q
        .select(game_player_stat::game_id)
        .load(conn)
        ?;

    if game_ids.is_empty() {
        return Ok(Vec::new());
    }

    // Step 2: pull all rows for those games (both sides of the matchup) so we
    // can look at the opponent's stocks too.
    let rows: Vec<(i32, String, Option<i32>, Option<i32>)> = gamePlayer::table
        .inner_join(game_player_stat::table.on(game_player_stat::game_player_id.eq(gamePlayer::id)))
        .filter(game_player_stat::game_id.eq_any(&game_ids))
        .select((
            game_player_stat::game_id,
            gamePlayer::code,
            game_player_stat::starting_stocks,
            game_player_stat::stocks_remaining,
        ))
        .load(conn)
        ?;

    let mut by_game: HashMap<i32, Vec<StockRow>> = HashMap::new();
    for (gid, code, starting, remaining) in rows {
        by_game
            .entry(gid)
            .or_default()
            .push((code, starting, remaining));
    }

    let stocks_dropped = |row: Option<&StockRow>| -> Option<i64> {
        let (_, starting, remaining) = row?;
        let (start, rem) = (starting.as_ref()?, remaining.as_ref()?);
        Some((*start - *rem).max(0) as i64)
    };

    Ok(by_game
        .into_values()
        .filter(|group| group.len() == 2)
        .map(|group| {
            let opp = group.iter().find(|(code, _, _)| code != player_code);
            let me = group.iter().find(|(code, _, _)| code == player_code);
            (stocks_dropped(opp), stocks_dropped(me))
        })
        .collect())
}

/// Filterable variant of [`avg_stocks_taken_by_code`]: mean stocks the
/// player took off their opponent per game.
///
/// Averages over the games where the opponent's stock data is readable;
/// `None` when that is no games at all.
pub fn avg_stocks_taken_filtered(
    conn: &mut SqliteConnection,
    player_code: &str,
    filter: &PlayerSummaryFilter,
) -> Result<Option<f64>> {
    let taken: Vec<i64> = stock_swings_filtered(conn, player_code, filter)?
        .into_iter()
        .filter_map(|(taken, _lost)| taken)
        .collect();

    if taken.is_empty() {
        return Ok(None);
    }
    Ok(Some(taken.iter().sum::<i64>() as f64 / taken.len() as f64))
}


/// Lifetime stock totals for the player: `(taken, lost)`.
///
/// Counts only games where *both* sides' stock data is readable, so the two
/// figures always describe the same set of games and can be compared
/// against each other.
pub fn total_stocks_filtered(
    conn: &mut SqliteConnection,
    player_code: &str,
    filter: &PlayerSummaryFilter,
) -> Result<(i64, i64)> {
    let mut taken_sum: i64 = 0;
    let mut lost_sum: i64 = 0;
    for (taken, lost) in stock_swings_filtered(conn, player_code, filter)? {
        if let (Some(taken), Some(lost)) = (taken, lost) {
            taken_sum += taken;
            lost_sum += lost;
        }
    }
    Ok((taken_sum, lost_sum))
}

/// Average hit count per punish for `player_code` (as the attacker) — i.e.
/// how long is a typical combo once they open someone up?
///
/// Returns `None` if the player has no punish rows yet.
pub fn avg_punish_length_by_code(
    conn: &mut SqliteConnection,
    player_code: &str,
) -> Result<Option<f64>> {
    avg_punish_length_filtered(conn, player_code, &PlayerSummaryFilter::NONE)
}

/// Filterable variant of [`avg_punish_length_by_code`].
pub fn avg_punish_length_filtered(
    conn: &mut SqliteConnection,
    player_code: &str,
    filter: &PlayerSummaryFilter,
) -> Result<Option<f64>> {
    use crate::schema::{game, gamePlayer, punish};

    let mut q = gamePlayer::table
        .inner_join(punish::table.on(punish::attacker_id.eq(gamePlayer::id)))
        .inner_join(game::table.on(game::id.eq(punish::game_id)))
        .filter(gamePlayer::code.eq(player_code))
        .into_boxed();
    narrow_by_filter!(q, filter);
    let hit_counts: Vec<i32> = q
        .select(punish::hit_count)
        .load(conn)
        ?;

    if hit_counts.is_empty() {
        return Ok(None);
    }
    let sum: i64 = hit_counts.iter().map(|&h| h as i64).sum();
    Ok(Some(sum as f64 / hit_counts.len() as f64))
}

/// Openings per kill: how many punishes does the player land per stock taken,
/// on average? Lower is better (fewer dropped conversions).
///
/// `None` when the player has no kill punishes yet.
pub fn openings_per_kill_by_code(
    conn: &mut SqliteConnection,
    player_code: &str,
) -> Result<Option<f64>> {
    openings_per_kill_filtered(conn, player_code, &PlayerSummaryFilter::NONE)
}

/// Filterable variant of [`openings_per_kill_by_code`].
pub fn openings_per_kill_filtered(
    conn: &mut SqliteConnection,
    player_code: &str,
    filter: &PlayerSummaryFilter,
) -> Result<Option<f64>> {
    use crate::schema::{game, gamePlayer, punish};

    let mut q = gamePlayer::table
        .inner_join(punish::table.on(punish::attacker_id.eq(gamePlayer::id)))
        .inner_join(game::table.on(game::id.eq(punish::game_id)))
        .filter(gamePlayer::code.eq(player_code))
        .into_boxed();
    narrow_by_filter!(q, filter);
    let did_kill_flags: Vec<i32> = q
        .select(punish::did_kill)
        .load(conn)
        ?;

    if did_kill_flags.is_empty() {
        return Ok(None);
    }
    let total_punishes = did_kill_flags.len() as f64;
    let kills: i64 = did_kill_flags.iter().map(|&k| k as i64).sum();
    if kills == 0 {
        return Ok(None);
    }
    Ok(Some(total_punishes / kills as f64))
}

/// Most frequently used kill moves for `player_code`, sorted from most to
/// least common. Returns `(attack_id, count)` pairs; attack ids map to the
/// Slippi spec's "attack id" table.
pub fn most_common_kill_moves_by_code(
    conn: &mut SqliteConnection,
    player_code: &str,
) -> Result<Vec<(i32, i32)>> {
    most_common_kill_moves_filtered(conn, player_code, &PlayerSummaryFilter::NONE)
}

/// Filterable variant of [`most_common_kill_moves_by_code`].
///
/// Accepts the unfiltered case as well, so callers that want the raw
/// cross-character distribution can still get it.
pub fn most_common_kill_moves_filtered(
    conn: &mut SqliteConnection,
    player_code: &str,
    filter: &PlayerSummaryFilter,
) -> Result<Vec<(i32, i32)>> {
    use crate::schema::{game, gamePlayer, punish};

    let mut q = gamePlayer::table
        .inner_join(punish::table.on(punish::attacker_id.eq(gamePlayer::id)))
        .inner_join(game::table.on(game::id.eq(punish::game_id)))
        .filter(gamePlayer::code.eq(player_code))
        .filter(punish::did_kill.eq(1))
        .into_boxed();
    narrow_by_filter!(q, filter);
    let rows: Vec<Option<i32>> = q
        .select(punish::kill_move)
        .load(conn)
        ?;

    let mut counts: HashMap<i32, i32> = HashMap::new();
    for r in rows.into_iter().flatten() {
        *counts.entry(r).or_insert(0) += 1;
    }
    let mut pairs: Vec<(i32, i32)> = counts.into_iter().collect();
    pairs.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    Ok(pairs)
}

/// All punish rows for a single game, ordered by `start_frame` ascending.
/// Useful for tests and for rendering the punish timeline in the replay
/// viewer.
pub fn get_punishes_for_game(
    conn: &mut SqliteConnection,
    game_id_filter: i32,
) -> Result<Vec<Punish>> {
    use crate::schema::punish::dsl::*;

    punish
        .filter(game_id.eq(game_id_filter))
        .order(start_frame.asc())
        .load::<Punish>(conn)
        .map_err(Into::into)
}

/// One-stop-shop analytics roll-up for a single player code.
///
/// Every field is optional because a player's data can be sparse: a brand-new
/// code may have no punish rows yet, or their .slp files may predate the
/// l_cancel column. The GUI is expected to render "—" (or skip the row) when
/// a field is `None`.
///
/// `top_kill_moves` is capped at [`PlayerSummary::TOP_KILL_MOVES_CAP`] entries
/// — the full list is available via [`most_common_kill_moves_by_code`].
#[derive(Debug, Clone)]
pub struct PlayerSummary {
    /// Player code this summary is for, preserved for convenience when
    /// threading summaries through the UI.
    pub code: String,
    /// Total games the player appeared in (rows in `game_player_stat`).
    pub games_played: i32,
    /// Total time played across those games, in seconds (sum of `game.time`).
    /// Pairs with `games_played` for a career playtime read-out.
    pub total_seconds: i64,
    /// Games won (placement == 0 — first place / the 1v1 winner). Pairs
    /// with `games_played` for a win-rate: `wins as f64 / games_played`.
    pub wins: i32,
    /// 0-indexed average placement — 0.0 means "always first", 3.0 means
    /// "always last in a 4-player game". `None` if the player has no rows.
    pub avg_placement: Option<f64>,
    /// Average stocks remaining at game end across every game.
    pub avg_stocks_remaining: Option<f64>,
    /// Average stocks taken from the opponent in 1v1 games (None if the
    /// player has no 1v1 data).
    pub avg_stocks_taken: Option<f64>,
    /// Total stocks taken from opponents across all filtered 1v1 games.
    pub total_stocks_taken: i64,
    /// Total stocks lost to opponents across all filtered 1v1 games.
    pub total_stocks_lost: i64,
    /// Actions per minute across all games.
    pub avg_apm: Option<f64>,
    /// L-cancel success rate in `[0.0, 1.0]`. `None` when the player has
    /// never landed an aerial.
    pub l_cancel_rate: Option<f64>,
    /// Average combo length (in hits) across every punish the player landed
    /// as attacker.
    pub avg_punish_length: Option<f64>,
    /// Average punishes landed per kill taken. Lower is better (fewer
    /// dropped conversions). `None` when the player has no kill punishes.
    pub openings_per_kill: Option<f64>,
    /// Win/loss streak info — see [`Streaks`].
    pub streaks: Streaks,
    /// Most common kill moves as `(attack_id, count)` pairs, sorted by count
    /// descending. Truncated to [`PlayerSummary::TOP_KILL_MOVES_CAP`].
    pub top_kill_moves: Vec<(i32, i32)>,
    /// Aggregate advanced-stat ratios (damage/opening, edge-guard %,
    /// first-blood win %, comeback rate, average death %) over the same
    /// filtered game set. See [`AdvancedAggregate`].
    pub advanced: AdvancedAggregate,
}

impl PlayerSummary {
    /// How many kill-move rows `player_summary` keeps in `top_kill_moves`.
    pub const TOP_KILL_MOVES_CAP: usize = 5;

    /// Win rate in `[0.0, 1.0]` (`wins / games_played`), or `None` when no
    /// games match. For a filtered summary this is the win rate on exactly
    /// that character/stage combination.
    pub fn win_rate(&self) -> Option<f64> {
        if self.games_played > 0 {
            Some(self.wins as f64 / self.games_played as f64)
        } else {
            None
        }
    }
}

/// Build a [`PlayerSummary`] for `player_code` by calling each per-code
/// aggregate helper and packaging the results together.
///
/// Individual helpers still return their own errors — if any one of them
/// fails (e.g. a diesel query error), the entire summary errors out. This
/// matches the usual "GUI-level" expectation that either we have a
/// complete-enough picture to render, or we surface the error.
///
/// Fields map directly to their same-named query helper:
///
/// - `avg_placement`         → [`avg_placement_by_code`]
/// - `avg_stocks_remaining`  → [`avg_stocks_remaining`]
/// - `avg_stocks_taken`      → [`avg_stocks_taken_by_code`]
/// - `avg_apm`               → [`avg_apm_by_code`]
/// - `l_cancel_rate`         → [`l_cancel_rate_by_code`]
/// - `avg_punish_length`     → [`avg_punish_length_by_code`]
/// - `openings_per_kill`     → [`openings_per_kill_by_code`]
/// - `streaks`               → [`streaks_by_code`]
/// - `top_kill_moves`        → [`most_common_kill_moves_by_code`] (truncated)
pub fn player_summary(
    conn: &mut SqliteConnection,
    player_code: &str,
) -> Result<PlayerSummary> {
    player_summary_filtered(conn, player_code, &PlayerSummaryFilter::NONE)
}

/// Aggregate advanced-stat ratios over the filtered games — the numbers the
/// Analytics page surfaces from the per-game [`crate::advanced`] counters.
/// Each is `None` when its denominator is zero (no qualifying games yet, or
/// legacy / non-1v1 rows that stored NULLs).
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct AdvancedAggregate {
    /// `sum(damage_dealt) / sum(openings)` — average % per conversion.
    pub avg_damage_per_opening: Option<f64>,
    /// `sum(edgeguard_kills) / sum(edgeguard_attempts)` in `[0,1]`.
    pub edgeguard_success: Option<f64>,
    /// Of games where the player took the first stock, the fraction won.
    pub first_blood_win_rate: Option<f64>,
    /// Of games won, the fraction won after trailing by >= 2 stocks.
    pub comeback_rate: Option<f64>,
    /// `sum(death_percent_sum) / sum(deaths)` — average % the player dies at.
    pub avg_death_percent: Option<f64>,
}

/// Fold the per-game advanced counters into [`AdvancedAggregate`] in one
/// query. Mirrors the boxed-query filter pattern of the other `*_filtered`
/// aggregates (character / stage / explicit game-id set).
pub fn advanced_aggregate_filtered(
    conn: &mut SqliteConnection,
    player_code: &str,
    filter: &PlayerSummaryFilter,
) -> Result<AdvancedAggregate> {
    use crate::schema::{game, gamePlayer, game_player_stat};

    let mut q = gamePlayer::table
        .inner_join(game_player_stat::table.on(game_player_stat::game_player_id.eq(gamePlayer::id)))
        .inner_join(game::table.on(game::id.eq(game_player_stat::game_id)))
        .filter(gamePlayer::code.eq(player_code))
        .into_boxed();
    narrow_by_filter!(q, filter);

    type Row = (
        i32,         // placement
        Option<f64>, // damage_dealt
        Option<i32>, // openings
        Option<i32>, // edgeguard_kills
        Option<i32>, // edgeguard_attempts
        Option<i32>, // first_blood
        Option<i32>, // deaths
        Option<f64>, // death_percent_sum
        Option<i32>, // comeback_win
    );
    let rows: Vec<Row> = q
        .select((
            game_player_stat::placement,
            game_player_stat::damage_dealt,
            game_player_stat::openings,
            game_player_stat::edgeguard_kills,
            game_player_stat::edgeguard_attempts,
            game_player_stat::first_blood,
            game_player_stat::deaths,
            game_player_stat::death_percent_sum,
            game_player_stat::comeback_win,
        ))
        .load(conn)
        ?;

    let (mut dmg, mut openings) = (0.0_f64, 0_i64);
    let (mut eg_kills, mut eg_attempts) = (0_i64, 0_i64);
    let (mut fb_games, mut fb_wins) = (0_i64, 0_i64);
    let (mut wins, mut comebacks) = (0_i64, 0_i64);
    let (mut death_sum, mut deaths) = (0.0_f64, 0_i64);

    for (placement, damage, opn, egk, ega, first_blood, d, dps, comeback) in rows {
        if let (Some(dm), Some(o)) = (damage, opn) {
            if o > 0 {
                dmg += dm;
                openings += o as i64;
            }
        }
        if let (Some(k), Some(a)) = (egk, ega) {
            if a > 0 {
                eg_kills += k as i64;
                eg_attempts += a as i64;
            }
        }
        if first_blood == Some(1) {
            fb_games += 1;
            if placement == 0 {
                fb_wins += 1;
            }
        }
        if placement == 0 {
            wins += 1;
            if comeback == Some(1) {
                comebacks += 1;
            }
        }
        if let (Some(dc), Some(s)) = (d, dps) {
            if dc > 0 {
                deaths += dc as i64;
                death_sum += s;
            }
        }
    }

    let ratio = |num: i64, den: i64| (den > 0).then(|| num as f64 / den as f64);
    Ok(AdvancedAggregate {
        avg_damage_per_opening: (openings > 0).then(|| dmg / openings as f64),
        edgeguard_success: ratio(eg_kills, eg_attempts),
        first_blood_win_rate: ratio(fb_wins, fb_games),
        comeback_rate: ratio(comebacks, wins),
        avg_death_percent: (deaths > 0).then(|| death_sum / deaths as f64),
    })
}

/// Filterable variant of [`player_summary`].
///
/// `filter` narrows every aggregate to games matching the given character
/// and/or stage. Both fields `None` is equivalent to [`player_summary`].
///
/// `games_played` is the count of stat rows that survive the filter — so
/// a Falco-on-FoD summary's `games_played` is "Falco-on-FoD games", not
/// "all games for this code".
pub fn player_summary_filtered(
    conn: &mut SqliteConnection,
    player_code: &str,
    filter: &PlayerSummaryFilter,
) -> Result<PlayerSummary> {
    use crate::schema::{game, gamePlayer, game_player_stat};

    // One query for both games_played and streaks — both look at the same
    // chronological placement vector, so folding them together avoids a
    // duplicate DB round-trip in the GUI's hot path.
    let mut q = gamePlayer::table
        .inner_join(game_player_stat::table.on(game_player_stat::game_player_id.eq(gamePlayer::id)))
        .inner_join(game::table.on(game::id.eq(game_player_stat::game_id)))
        .filter(gamePlayer::code.eq(player_code))
        .into_boxed();
    narrow_by_filter!(q, filter);
    // Pull placement + match duration in one pass: placements feed
    // games_played / wins / streaks, and the durations sum to total playtime.
    let rows: Vec<(i32, i32)> = q
        .order(game_player_stat::game_id.asc())
        .select((game_player_stat::placement, game::time))
        .load(conn)
        ?;
    let placements: Vec<i32> = rows.iter().map(|(p, _)| *p).collect();
    let total_seconds: i64 = rows.iter().map(|(_, t)| (*t).max(0) as i64).sum();
    let games_played = placements.len() as i32;
    // placement == 0 is a win (first place / the 1v1 winner).
    let wins = placements.iter().filter(|&&p| p == 0).count() as i32;
    let streaks = streaks_from_placements(&placements);

    let avg_placement = avg_placement_filtered(conn, player_code, filter)?;
    let avg_stocks_remaining_val = avg_stocks_remaining_filtered(conn, player_code, filter)?;
    let avg_stocks_taken = avg_stocks_taken_filtered(conn, player_code, filter)?;
    let (total_stocks_taken, total_stocks_lost) =
        total_stocks_filtered(conn, player_code, filter)?;
    let avg_apm = avg_apm_filtered(conn, player_code, filter)?;
    let l_cancel_rate = l_cancel_rate_filtered(conn, player_code, filter)?;
    let avg_punish_length = avg_punish_length_filtered(conn, player_code, filter)?;
    let openings_per_kill = openings_per_kill_filtered(conn, player_code, filter)?;
    let mut top_kill_moves = most_common_kill_moves_filtered(conn, player_code, filter)?;
    top_kill_moves.truncate(PlayerSummary::TOP_KILL_MOVES_CAP);
    let advanced = advanced_aggregate_filtered(conn, player_code, filter)?;

    Ok(PlayerSummary {
        code: player_code.to_string(),
        games_played,
        total_seconds,
        wins,
        avg_placement,
        avg_stocks_remaining: avg_stocks_remaining_val,
        avg_stocks_taken,
        total_stocks_taken,
        total_stocks_lost,
        avg_apm,
        l_cancel_rate,
        avg_punish_length,
        openings_per_kill,
        streaks,
        top_kill_moves,
        advanced,
    })
}

/// Streak summary for a player code.
///
/// `current` is signed: positive for an active win streak, negative for an
/// active loss streak, 0 if the player has no games.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Streaks {
    pub longest_win: i32,
    pub longest_loss: i32,
    pub current: i32,
}

/// Compute longest win streak, longest loss streak, and current streak for
/// `player_code`. Games are ordered by `game_player_stat.game_id` ascending
/// (proxy for chronological — ingestion walks the filesystem in directory
/// order, so ids line up with capture time on any sanely-named folder tree).
pub fn streaks_by_code(
    conn: &mut SqliteConnection,
    player_code: &str,
) -> Result<Streaks> {
    streaks_filtered(conn, player_code, &PlayerSummaryFilter::NONE)
}

/// Filterable variant of [`streaks_by_code`].
///
/// "Streak" is computed *within* the filtered subset — e.g. with
/// `character_id = Falco`, a 5-game win run on Falco interrupted by 3
/// non-Falco losses still reads as a 5-game win streak. The filter
/// re-defines which games count, not the gaps between them.
pub fn streaks_filtered(
    conn: &mut SqliteConnection,
    player_code: &str,
    filter: &PlayerSummaryFilter,
) -> Result<Streaks> {
    use crate::schema::{game, gamePlayer, game_player_stat};

    let mut q = gamePlayer::table
        .inner_join(game_player_stat::table.on(game_player_stat::game_player_id.eq(gamePlayer::id)))
        .inner_join(game::table.on(game::id.eq(game_player_stat::game_id)))
        .filter(gamePlayer::code.eq(player_code))
        .into_boxed();
    narrow_by_filter!(q, filter);
    let placements: Vec<i32> = q
        .order(game_player_stat::game_id.asc())
        .select(game_player_stat::placement)
        .load(conn)
        ?;

    Ok(streaks_from_placements(&placements))
}

/// Pure, stateless version of [`streaks_by_code`] — easy to unit-test without
/// standing up a database. Assumes placements are already chronologically
/// ordered; `placement == 0` is a win, anything else is a loss.
pub fn streaks_from_placements(placements: &[i32]) -> Streaks {
    let mut longest_win = 0;
    let mut longest_loss = 0;
    let mut current: i32 = 0;

    for &p in placements {
        let won = p == 0;
        if won {
            if current >= 0 {
                current += 1;
            } else {
                current = 1;
            }
            if current > longest_win {
                longest_win = current;
            }
        } else {
            if current <= 0 {
                current -= 1;
            } else {
                current = -1;
            }
            let loss_len = -current;
            if loss_len > longest_loss {
                longest_loss = loss_len;
            }
        }
    }

    Streaks {
        longest_win,
        longest_loss,
        current,
    }
}

#[cfg(test)]
mod streak_tests {
    use super::*;

    #[test]
    fn empty_input_returns_zero_streaks() {
        let s = streaks_from_placements(&[]);
        assert_eq!(s.longest_win, 0);
        assert_eq!(s.longest_loss, 0);
        assert_eq!(s.current, 0);
    }

    #[test]
    fn all_wins() {
        // 5 wins in a row → longest_win = 5, current = 5, no losses.
        let s = streaks_from_placements(&[0, 0, 0, 0, 0]);
        assert_eq!(s.longest_win, 5);
        assert_eq!(s.longest_loss, 0);
        assert_eq!(s.current, 5);
    }

    #[test]
    fn all_losses() {
        // 4 losses → longest_loss = 4, current = -4.
        let s = streaks_from_placements(&[1, 1, 2, 3]);
        assert_eq!(s.longest_win, 0);
        assert_eq!(s.longest_loss, 4);
        assert_eq!(s.current, -4);
    }

    #[test]
    fn alternating_gives_single_streaks() {
        // W-L-W-L → everything length 1, ending on a loss.
        let s = streaks_from_placements(&[0, 1, 0, 1]);
        assert_eq!(s.longest_win, 1);
        assert_eq!(s.longest_loss, 1);
        assert_eq!(s.current, -1);
    }

    #[test]
    fn longest_streaks_preserved_after_break() {
        // WWW LL WW → longest_win = 3, longest_loss = 2, current = 2 (win).
        let s = streaks_from_placements(&[0, 0, 0, 1, 1, 0, 0]);
        assert_eq!(s.longest_win, 3);
        assert_eq!(s.longest_loss, 2);
        assert_eq!(s.current, 2);
    }

    #[test]
    fn current_switches_sign_on_result_change() {
        // One win, then a loss — current should flip from +1 to -1.
        let s = streaks_from_placements(&[0, 1]);
        assert_eq!(s.current, -1);
        assert_eq!(s.longest_win, 1);
        assert_eq!(s.longest_loss, 1);
    }
}

