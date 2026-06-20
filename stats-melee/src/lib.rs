pub mod gamedata;
pub mod advanced;
pub mod analytics;
pub mod analysis_cache;
pub mod combat;
pub mod file_cache;
pub mod models;
pub mod punish;
pub mod schema;
pub mod stage_bounds;
pub mod testing;

// Re-export the embedded migrations bundle so callers outside of test code
// (e.g. the GUI app) can open a fresh DB at an arbitrary path and run all
// pending migrations without reaching into the `testing` module.
pub use self::testing::MIGRATIONS;

use self::models::{
    Character, Game, GamePlayer, GamePlayerStat, NewCharacter, NewGame, NewGamePlayer,
    NewGamePlayerStat, NewPlayer, NewPunish, NewStage, Player, Punish, Stage,
};
use self::analytics::{WinProportion, WinAnalytics};
use anyhow::{anyhow, Result};
use diesel::prelude::*;
use diesel::connection::SimpleConnection;
use diesel::dsl;
use diesel_migrations::MigrationHarness;
use dotenvy::dotenv;
use gamedata::{GameData, SlippiPlayer};
use peppi::io::slippi;
use std::{env, fs};
use std::io::{self, Write};
use std::path::Path;
use std::collections::HashMap;

pub static NUM_STAGES: usize = 33;
pub static NUM_CHARACTERS: usize = 33;


/// Parse a single .slp file at `path` into a [`GameData`].
///
/// Does not touch the database — useful for tests and any caller that just
/// wants to inspect a replay.
pub fn parse_single_replay<P: AsRef<Path>>(path: P) -> Result<GameData> {
    let mut r = io::BufReader::new(fs::File::open(path.as_ref())?);
    let game = slippi::read(&mut r, None)?;
    GameData::new_gamedata(&game)
}

/// Compute the hex-encoded SHA-256 of the .slp file's bytes. Used at
/// ingestion time to populate `game.content_hash`, which the analysis
/// sidecar cache keys on.
///
/// Streams the file through the hasher in 64 KiB chunks rather than
/// reading into memory first — replays are typically a few MB but
/// tournament sets can hit 50+ MB and there's no reason to hold the
/// whole thing in RAM just to compute a digest.
pub fn hash_slp_file<P: AsRef<Path>>(path: P) -> Result<String> {
    use sha2::{Digest, Sha256};
    use std::io::Read as _;

    let mut f = fs::File::open(path.as_ref())
        .map_err(|e| anyhow!("open {}: {e}", path.as_ref().display()))?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = f
            .read(&mut buf)
            .map_err(|e| anyhow!("hash {}: {e}", path.as_ref().display()))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    let digest = hasher.finalize();
    // Hex-encode without pulling in a `hex` crate — 32 bytes per digest
    // is light enough that a manual loop reads cleaner than a dep.
    let mut hex = String::with_capacity(digest.len() * 2);
    for b in digest {
        use std::fmt::Write as _;
        let _ = write!(hex, "{:02x}", b);
    }
    Ok(hex)
}

/// Parse a .slp file and compute its per-frame combat states in one call.
///
/// Used by the GUI's replay-viewer page — the app crate doesn't take a
/// direct dependency on peppi, so exposing a `path -> Vec<CombatState>`
/// helper here keeps that boundary clean. Errors on either the parse or
/// the combat-state computation (e.g. non-1v1 replay) bubble up through
/// the single `Result`.
pub fn parse_replay_combat_states<P: AsRef<Path>>(
    path: P,
) -> Result<Vec<combat::CombatState>> {
    let analysis = parse_replay_analysis(path)?;
    Ok(analysis.combat)
}

/// Parse a .slp file and produce a full [`combat::ReplayAnalysis`] —
/// combat states + per-frame character positions + port indices — in
/// one read of the file.
///
/// This is the entry point the embedded 2D analysis view uses; the
/// older [`parse_replay_combat_states`] now wraps it and discards the
/// positional data. Keeping both surface APIs lets callers that only
/// want combat states avoid allocating the frame-snapshot vector if
/// they care about memory (the viewer doesn't; ingestion might later).
pub fn parse_replay_analysis<P: AsRef<Path>>(
    path: P,
) -> Result<combat::ReplayAnalysis> {
    let mut r = io::BufReader::new(fs::File::open(path.as_ref())?);
    let game = slippi::read(&mut r, None)?;
    combat::compute_analysis_1v1(&game)
}

/// Walk the sibling directories of the given root and ingest any `.slp` files
/// not already represented in the database.
///
/// Dedup strategy: each game row carries a `replay_path` and that column has
/// a UNIQUE index. We load every already-ingested canonical path once, up
/// front, and skip those files before parsing; the UNIQUE index is the
/// ultimate guard against a genuinely concurrent double-scan.
///
/// ## Parallelism
///
/// The per-file cost is dominated by CPU-bound, independent work — the peppi
/// parse, punish extraction (a full frame walk), and the SHA-256 of the
/// file's bytes. Only the DB inserts must be serialized (SQLite has a single
/// writer), and they're cheap next to the parse. So we fan the parse + hash
/// out across a worker pool sized to the machine's parallelism and feed the
/// results back over a bounded channel to a single inserter running on this
/// thread. The channel bound caps in-flight memory and lets the insert phase
/// overlap with parsing.
///
/// Insertion order follows parse *completion*, not directory order, so the
/// `game.id`s assigned within a single scan are not deterministic. Nothing
/// downstream relies on that (rows are surfaced by `ingested_at` then id);
/// it's noted only so a future reader isn't surprised.
///
/// `db_path` used to gate an mtime comparison; it's now accepted for API
/// compatibility but ignored. Callers can pass any path.
pub fn parse_new_replays<P: AsRef<Path>, Q: AsRef<Path>>(
    conn: &mut SqliteConnection,
    root: P,
    _db_path: Q,
) -> Result<usize> {
    use crate::schema::game::dsl as game_dsl;
    use std::collections::HashSet;
    use std::panic::{catch_unwind, AssertUnwindSafe};
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::mpsc::sync_channel;

    // 1. Collect candidate `.slp` paths: `root/<subdir>/*.slp`, skipping the
    //    non-replay directories the old walk skipped.
    let mut candidates: Vec<PathBuf> = Vec::new();
    for sub_dir in fs::read_dir(root.as_ref())? {
        let sub_dir_path = sub_dir?.path();
        if !sub_dir_path.is_dir() {
            continue;
        }
        if let Some(name) = sub_dir_path.file_name() {
            if name == "target"
                || name == "src"
                || name == "migrations"
                || name == "stats-melee"
                || name.to_string_lossy().starts_with('.')
            {
                continue;
            }
        }
        for replay in fs::read_dir(&sub_dir_path)? {
            let p = replay?.path();
            if p.extension().and_then(|e| e.to_str()) == Some("slp") {
                candidates.push(p);
            }
        }
    }

    // 2. Load every already-ingested canonical path once, so dedup is an
    //    in-memory O(1) lookup instead of a SELECT per file.
    let existing: HashSet<String> = game_dsl::game
        .select(game_dsl::replay_path)
        .filter(game_dsl::replay_path.is_not_null())
        .load::<Option<String>>(conn)
        .map_err(|e| anyhow!(e.to_string()))?
        .into_iter()
        .flatten()
        .collect();

    // 3. Canonicalize + drop already-ingested files. Canonicalizing collapses
    //    two spellings of the same path (matches the UNIQUE index); fall back
    //    to the original path if it fails (network FS quirks, permissions).
    let new_paths: Vec<(PathBuf, String)> = candidates
        .into_iter()
        .filter_map(|p| {
            let canonical = fs::canonicalize(&p).unwrap_or_else(|_| p.clone());
            let canonical_str = canonical.to_string_lossy().to_string();
            if existing.contains(&canonical_str) {
                None
            } else {
                Some((p, canonical_str))
            }
        })
        .collect();

    if new_paths.is_empty() {
        return Ok(0);
    }

    // 4. Parse + hash in parallel; insert serially on this thread.
    let total = new_paths.len();
    let n_workers = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
        .min(total)
        .max(1);
    // Shared cursor the workers claim files from (work-stealing, so a few
    // slow files don't stall a whole static chunk).
    let next = AtomicUsize::new(0);
    // Bounded so parsing can't outrun the inserter into unbounded memory.
    let (tx, rx) =
        sync_channel::<(GameData, String, Option<String>)>(n_workers.saturating_mul(4).max(8));

    let count = std::thread::scope(|scope| {
        for _ in 0..n_workers {
            let tx = tx.clone();
            let next = &next;
            let new_paths = &new_paths;
            scope.spawn(move || loop {
                let i = next.fetch_add(1, Ordering::Relaxed);
                if i >= total {
                    break;
                }
                let (path, canonical) = &new_paths[i];

                // Real-world .slp files occasionally tickle panics in peppi
                // (truncated headers, unexpected events). catch_unwind keeps
                // one bad file from taking the whole scan (and the app
                // process) down; log and move on.
                let parsed = catch_unwind(AssertUnwindSafe(|| parse_single_replay(path)));
                let gamedata = match parsed {
                    Ok(Ok(g)) => g,
                    Ok(Err(e)) => {
                        eprintln!("stats-melee: skipping {}: parse error: {e}", path.display());
                        continue;
                    }
                    Err(_panic) => {
                        eprintln!("stats-melee: skipping {}: parser panicked", path.display());
                        continue;
                    }
                };

                // SHA-256 for the analysis sidecar cache key. A failure here
                // is non-fatal — we store None and the viewer falls back to a
                // re-parse for that row.
                let content_hash = match hash_slp_file(path) {
                    Ok(h) => Some(h),
                    Err(e) => {
                        eprintln!(
                            "stats-melee: hashing {} failed: {e} (continuing without cache key)",
                            path.display()
                        );
                        None
                    }
                };

                // If the inserter has gone away the receiver is dropped; stop.
                if tx.send((gamedata, canonical.clone(), content_hash)).is_err() {
                    break;
                }
            });
        }
        // Drop our own sender so `rx` ends once every worker's clone is gone.
        drop(tx);

        // Insert as results stream in. Errors here can be UNIQUE-constraint
        // violations (a concurrent double-scan) or other schema issues —
        // log + skip rather than abort the whole scan.
        let mut count: usize = 0;
        for (gamedata, canonical, content_hash) in rx {
            if let Err(e) =
                post_game_full(conn, &gamedata, Some(&canonical), content_hash.as_deref())
            {
                eprintln!("stats-melee: skipping {canonical}: insert error: {e}");
                continue;
            }
            count += 1;
        }
        count
    });

    Ok(count)
}

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
        .load::<Game>(conn).map_err(|e| anyhow!(e.to_string()))
}

pub fn analyze_games(conn: &mut SqliteConnection, games: &Vec<Game>, player_code: &str) -> Result<WinAnalytics> {

    let mut opponents: HashMap<String, WinProportion> = HashMap::new();
    let mut stages = [WinProportion::new_winproportion(); NUM_STAGES];
    let mut played_characters = [WinProportion::new_winproportion(); NUM_CHARACTERS];
    let mut opp_characters = [WinProportion::new_winproportion(); NUM_CHARACTERS];

    for game in games {

        let mut codes = [None, None, None, None];
        let mut characters = [None, None, None, None];
        
        if let Some(f) = game.first {
            codes[0] = Some(get_game_player_code(conn, f)?);
            characters[0] = Some(get_character(conn, f)?);
        }

        if let Some(s) = game.second {
            codes[1] = Some(get_game_player_code(conn, s)?);
            characters[1] = Some(get_character(conn, s)?);
        }

        if let Some(t) = game.third {
            codes[2] = Some(get_game_player_code(conn, t)?);
            characters[2] = Some(get_character(conn, t)?);
        }

        if let Some(f) = game.fourth {
            codes[3] = Some(get_game_player_code(conn, f)?);
            characters[3] = Some(get_character(conn, f)?);
        }

        let player_won = codes[0] == Some(player_code.to_string());

        for (code_option, character_option) in codes.iter().zip(characters.iter()) {
            if let Some(code) = code_option {
                let character = character_option.ok_or(anyhow!("no character found for player"))?;

                if code != player_code {
                    let opps_winproportion = opponents.entry(code.to_string()).or_insert(WinProportion::new_winproportion());

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

    for i in 0..NUM_STAGES {
        stages[i].update_proportion();
    }

    for i in 0..NUM_CHARACTERS {
        played_characters[i].update_proportion();
        opp_characters[i].update_proportion();
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
        .map_err(|e| anyhow!(e.to_string()))?;

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
        .map_err(|e| anyhow!(e.to_string()))?;

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

    gamePlayer.filter(id.eq(find_id)).select(code).first::<String>(conn).map_err(|e| anyhow!(e.to_string()))
}

pub fn get_character(conn: &mut SqliteConnection, find_id: i32) -> Result<i32> {
    use crate::schema::gamePlayer::dsl::*;

    gamePlayer.filter(id.eq(find_id)).select(character).first::<i32>(conn).map_err(|e| anyhow!(e.to_string()))
}


pub fn post_player(conn: &mut SqliteConnection, slippi_player: &SlippiPlayer) -> Result<Player> {
    use crate::schema::player;

    let new_player = NewPlayer {
        netplay: slippi_player.netplay(),
        code: slippi_player.code(),
    };

    Ok(insert_or_get_player(conn, &new_player)?)
}

pub fn post_game_player(conn: &mut SqliteConnection, slippi_player: &SlippiPlayer) -> Result<GamePlayer> {
    use crate::schema::gamePlayer;

    post_player(conn, slippi_player)?;


    let new_game_player = NewGamePlayer {
        code: slippi_player.code(),
        character: slippi_player.character().into(),
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

    // Insert (or fetch) the gamePlayer row for every slot up front, so we can
    // thread the ids through both the `game` row and the downstream
    // `game_player_stat` rows.
    let player_ids: [Option<i32>; 4] = std::array::from_fn(|i| {
        gamedata.placements[i]
            .as_ref()
            .and_then(|p| post_game_player(conn, p).ok())
            .map(|gp| gp.id)
    });

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
        .map_err(|e| anyhow!(e.to_string()))?;

    // Write one game_player_stat row per populated placement slot.
    // Also build port_idx → gamePlayer.id while we're at it, so the punish
    // inserter below can translate peppi port indices into gp ids.
    let mut gp_by_port: [Option<i32>; 4] = [None, None, None, None];
    for (placement_idx, gp_id) in player_ids.iter().enumerate() {
        if let Some(id) = gp_id {
            // Advanced stats are keyed by peppi port; map this placement's
            // player onto the matching `p1`/`p2` counters. `None` for non-1v1
            // games (no advanced analysis) stores NULLs across the board.
            let adv = gamedata.placements[placement_idx].as_ref().and_then(|player| {
                let port_idx: i32 = player.port.into();
                let port_idx = usize::try_from(port_idx).ok()?;
                gamedata.advanced.as_ref().and_then(|a| {
                    if a.p1_port_idx == port_idx {
                        Some(a.p1)
                    } else if a.p2_port_idx == port_idx {
                        Some(a.p2)
                    } else {
                        None
                    }
                })
            });
            let stat = NewGamePlayerStat {
                game_id: inserted_game.id,
                game_player_id: *id,
                placement: placement_idx as i32,
                stocks_remaining: gamedata.stocks_remaining[placement_idx],
                starting_stocks: gamedata.starting_stocks[placement_idx],
                inputs: gamedata.inputs[placement_idx],
                l_cancel_attempts: gamedata.l_cancel_attempts[placement_idx],
                l_cancel_success: gamedata.l_cancel_success[placement_idx],
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

            if let Some(player) = gamedata.placements[placement_idx].as_ref() {
                let port_idx: i32 = player.port.into();
                if (0..4).contains(&port_idx) {
                    gp_by_port[port_idx as usize] = Some(*id);
                }
            }
        }
    }

    // Persist each RawPunish as a punish row. Skip any punish whose attacker
    // or victim port didn't resolve to a gamePlayer (shouldn't happen in
    // practice for 1v1 replays, but belt-and-suspenders).
    for raw in &gamedata.punishes {
        let attacker = gp_by_port.get(raw.attacker_port_idx).copied().flatten();
        let victim = gp_by_port.get(raw.victim_port_idx).copied().flatten();
        if let (Some(att), Some(vic)) = (attacker, victim) {
            let np = NewPunish {
                game_id: inserted_game.id,
                attacker_id: att,
                victim_id: vic,
                start_frame: raw.start_frame,
                end_frame: raw.end_frame,
                hit_count: raw.hit_count,
                did_kill: if raw.did_kill { 1 } else { 0 },
                kill_move: raw.kill_move,
            };
            post_punish(conn, &np)?;
        }
    }

    Ok(inserted_game)
}

/// Insert a punish row. No conflict resolution — callers shouldn't produce
/// duplicate punishes for the same (game, frame range, attacker).
pub fn post_punish(conn: &mut SqliteConnection, new_punish: &NewPunish) -> Result<Punish> {
    use crate::schema::punish;

    diesel::insert_into(punish::table)
        .values(new_punish)
        .returning(Punish::as_returning())
        .get_result(conn)
        .map_err(|e| anyhow!(e.to_string()))
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
        .map_err(|e| anyhow!(e.to_string()))
}

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
        .map_err(|e| anyhow!(e.to_string()))
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
    if let Some(cid) = filter.character_id {
        q = q.filter(gamePlayer::character.eq(cid));
    }
    if let Some(sid) = filter.stage_id {
        q = q.filter(game::stage.eq(sid));
    }
    if let Some(ids) = &filter.game_ids {
        q = q.filter(game::id.eq_any(ids.clone()));
    }
    let placements: Vec<i32> = q
        .select(game_player_stat::placement)
        .load(conn)
        .map_err(|e| anyhow!(e.to_string()))?;

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
    if let Some(cid) = filter.character_id {
        q = q.filter(gamePlayer::character.eq(cid));
    }
    if let Some(sid) = filter.stage_id {
        q = q.filter(game::stage.eq(sid));
    }
    if let Some(ids) = &filter.game_ids {
        q = q.filter(game::id.eq_any(ids.clone()));
    }
    let stocks: Vec<Option<i32>> = q
        .select(game_player_stat::stocks_remaining)
        .load(conn)
        .map_err(|e| anyhow!(e.to_string()))?;

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
    if let Some(cid) = filter.character_id {
        q = q.filter(gamePlayer::character.eq(cid));
    }
    if let Some(sid) = filter.stage_id {
        q = q.filter(game::stage.eq(sid));
    }
    if let Some(ids) = &filter.game_ids {
        q = q.filter(game::id.eq_any(ids.clone()));
    }
    let rows: Vec<(Option<i32>, i32)> = q
        .select((game_player_stat::inputs, game::time))
        .load(conn)
        .map_err(|e| anyhow!(e.to_string()))?;

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
    if let Some(cid) = filter.character_id {
        q = q.filter(gamePlayer::character.eq(cid));
    }
    if let Some(sid) = filter.stage_id {
        q = q.filter(game::stage.eq(sid));
    }
    if let Some(ids) = &filter.game_ids {
        q = q.filter(game::id.eq_any(ids.clone()));
    }
    let rows: Vec<(Option<i32>, Option<i32>)> = q
        .select((
            game_player_stat::l_cancel_attempts,
            game_player_stat::l_cancel_success,
        ))
        .load(conn)
        .map_err(|e| anyhow!(e.to_string()))?;

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

/// Filterable variant of [`avg_stocks_taken_by_code`].
///
/// Filters apply to the *player's own* row when picking which 1v1 games
/// to consider — e.g. `character_id = Falco` means "the player was Falco
/// in this game", not "the opponent was Falco".
pub fn avg_stocks_taken_filtered(
    conn: &mut SqliteConnection,
    player_code: &str,
    filter: &PlayerSummaryFilter,
) -> Result<Option<f64>> {
    use crate::schema::{game, gamePlayer, game_player_stat};

    // Step 1: find every game the player appeared in (with the filter
    // applied to their row, not the opponent's).
    let mut id_q = gamePlayer::table
        .inner_join(game_player_stat::table.on(game_player_stat::game_player_id.eq(gamePlayer::id)))
        .inner_join(game::table.on(game::id.eq(game_player_stat::game_id)))
        .filter(gamePlayer::code.eq(player_code))
        .into_boxed();
    if let Some(cid) = filter.character_id {
        id_q = id_q.filter(gamePlayer::character.eq(cid));
    }
    if let Some(sid) = filter.stage_id {
        id_q = id_q.filter(game::stage.eq(sid));
    }
    if let Some(ids) = &filter.game_ids {
        id_q = id_q.filter(game::id.eq_any(ids.clone()));
    }
    let game_ids: Vec<i32> = id_q
        .select(game_player_stat::game_id)
        .load(conn)
        .map_err(|e| anyhow!(e.to_string()))?;

    if game_ids.is_empty() {
        return Ok(None);
    }

    // Step 2: pull all rows for those games (both sides of the matchup) so we
    // can look at the opponent's stocks.
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
        .map_err(|e| anyhow!(e.to_string()))?;

    let mut by_game: HashMap<i32, Vec<(String, Option<i32>, Option<i32>)>> = HashMap::new();
    for (gid, code, starting, remaining) in rows {
        by_game
            .entry(gid)
            .or_default()
            .push((code, starting, remaining));
    }

    let mut taken_sum: i64 = 0;
    let mut counted: i64 = 0;
    for (_gid, group) in by_game {
        if group.len() != 2 {
            continue; // only 1v1s contribute
        }
        let opp = group.iter().find(|(code, _, _)| code != player_code);
        if let Some((_, starting, remaining)) = opp {
            if let (Some(start), Some(rem)) = (starting, remaining) {
                let taken = (*start - *rem).max(0);
                taken_sum += taken as i64;
                counted += 1;
            }
        }
    }

    if counted == 0 {
        Ok(None)
    } else {
        Ok(Some(taken_sum as f64 / counted as f64))
    }
}

/// Total stocks `player_code` took from opponents and lost to them across the
/// filtered 1v1 games, as `(taken, lost)`.
///
/// Same two-step shape as [`avg_stocks_taken_filtered`] — filters pick the
/// player's own games, then both sides of each 1v1 are pulled so we can read
/// the opponent's stocks (taken) alongside the player's own (lost). Stocks are
/// `starting_stocks - stocks_remaining`, matching the per-game stocks-taken
/// metric. Games missing stock data, or non-1v1s, are skipped on both totals.
pub fn total_stocks_filtered(
    conn: &mut SqliteConnection,
    player_code: &str,
    filter: &PlayerSummaryFilter,
) -> Result<(i64, i64)> {
    use crate::schema::{game, gamePlayer, game_player_stat};

    let mut id_q = gamePlayer::table
        .inner_join(game_player_stat::table.on(game_player_stat::game_player_id.eq(gamePlayer::id)))
        .inner_join(game::table.on(game::id.eq(game_player_stat::game_id)))
        .filter(gamePlayer::code.eq(player_code))
        .into_boxed();
    if let Some(cid) = filter.character_id {
        id_q = id_q.filter(gamePlayer::character.eq(cid));
    }
    if let Some(sid) = filter.stage_id {
        id_q = id_q.filter(game::stage.eq(sid));
    }
    if let Some(ids) = &filter.game_ids {
        id_q = id_q.filter(game::id.eq_any(ids.clone()));
    }
    let game_ids: Vec<i32> = id_q
        .select(game_player_stat::game_id)
        .load(conn)
        .map_err(|e| anyhow!(e.to_string()))?;

    if game_ids.is_empty() {
        return Ok((0, 0));
    }

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
        .map_err(|e| anyhow!(e.to_string()))?;

    let mut by_game: HashMap<i32, Vec<(String, Option<i32>, Option<i32>)>> = HashMap::new();
    for (gid, code, starting, remaining) in rows {
        by_game
            .entry(gid)
            .or_default()
            .push((code, starting, remaining));
    }

    let mut taken_sum: i64 = 0;
    let mut lost_sum: i64 = 0;
    for (_gid, group) in by_game {
        if group.len() != 2 {
            continue; // only 1v1s contribute
        }
        let stocks_dropped = |row: Option<&(String, Option<i32>, Option<i32>)>| -> Option<i64> {
            let (_, starting, remaining) = row?;
            let (start, rem) = (starting.as_ref()?, remaining.as_ref()?);
            Some((*start - *rem).max(0) as i64)
        };
        let opp = group.iter().find(|(code, _, _)| code != player_code);
        let me = group.iter().find(|(code, _, _)| code == player_code);
        if let (Some(taken), Some(lost)) = (stocks_dropped(opp), stocks_dropped(me)) {
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
    if let Some(cid) = filter.character_id {
        q = q.filter(gamePlayer::character.eq(cid));
    }
    if let Some(sid) = filter.stage_id {
        q = q.filter(game::stage.eq(sid));
    }
    if let Some(ids) = &filter.game_ids {
        q = q.filter(game::id.eq_any(ids.clone()));
    }
    let hit_counts: Vec<i32> = q
        .select(punish::hit_count)
        .load(conn)
        .map_err(|e| anyhow!(e.to_string()))?;

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
    if let Some(cid) = filter.character_id {
        q = q.filter(gamePlayer::character.eq(cid));
    }
    if let Some(sid) = filter.stage_id {
        q = q.filter(game::stage.eq(sid));
    }
    if let Some(ids) = &filter.game_ids {
        q = q.filter(game::id.eq_any(ids.clone()));
    }
    let did_kill_flags: Vec<i32> = q
        .select(punish::did_kill)
        .load(conn)
        .map_err(|e| anyhow!(e.to_string()))?;

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
    if let Some(cid) = filter.character_id {
        q = q.filter(gamePlayer::character.eq(cid));
    }
    if let Some(sid) = filter.stage_id {
        q = q.filter(game::stage.eq(sid));
    }
    if let Some(ids) = &filter.game_ids {
        q = q.filter(game::id.eq_any(ids.clone()));
    }
    let rows: Vec<Option<i32>> = q
        .select(punish::kill_move)
        .load(conn)
        .map_err(|e| anyhow!(e.to_string()))?;

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
        .map_err(|e| anyhow!(e.to_string()))
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
    if let Some(cid) = filter.character_id {
        q = q.filter(gamePlayer::character.eq(cid));
    }
    if let Some(sid) = filter.stage_id {
        q = q.filter(game::stage.eq(sid));
    }
    if let Some(ids) = &filter.game_ids {
        q = q.filter(game::id.eq_any(ids.clone()));
    }

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
        .map_err(|e| anyhow!(e.to_string()))?;

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
    if let Some(cid) = filter.character_id {
        q = q.filter(gamePlayer::character.eq(cid));
    }
    if let Some(sid) = filter.stage_id {
        q = q.filter(game::stage.eq(sid));
    }
    if let Some(ids) = &filter.game_ids {
        q = q.filter(game::id.eq_any(ids.clone()));
    }
    // Pull placement + match duration in one pass: placements feed
    // games_played / wins / streaks, and the durations sum to total playtime.
    let rows: Vec<(i32, i32)> = q
        .order(game_player_stat::game_id.asc())
        .select((game_player_stat::placement, game::time))
        .load(conn)
        .map_err(|e| anyhow!(e.to_string()))?;
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
    if let Some(cid) = filter.character_id {
        q = q.filter(gamePlayer::character.eq(cid));
    }
    if let Some(sid) = filter.stage_id {
        q = q.filter(game::stage.eq(sid));
    }
    if let Some(ids) = &filter.game_ids {
        q = q.filter(game::id.eq_any(ids.clone()));
    }
    let placements: Vec<i32> = q
        .order(game_player_stat::game_id.asc())
        .select(game_player_stat::placement)
        .load(conn)
        .map_err(|e| anyhow!(e.to_string()))?;

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

pub fn post_stage(conn: &mut SqliteConnection, id: i32, name: String) -> Result<Stage> {
    use crate::schema::stage;

    let new_stage = NewStage{ id, name };

    diesel::insert_into(stage::table)
        .values(&new_stage)
        .returning(Stage::as_returning())
        .get_result(conn)
        .map_err(|e| anyhow!(e.to_string()))
}

pub fn post_character(conn: &mut SqliteConnection, id: i32, name: String) -> Result<Character> {
    use crate::schema::character;

    let new_character = NewCharacter{ id, name };

    diesel::insert_into(character::table)
        .values(&new_character)
        .returning(Character::as_returning())
        .get_result(conn)
        .map_err(|e| anyhow!(e.to_string()))
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

pub fn is_games_empty(conn: &mut SqliteConnection) -> Result<bool> {

    use crate::schema::game::dsl::*;

    let count: i64 = game.select(dsl::count_star()).first(conn)?;

    Ok(count == 0)
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
        // Order matters only in the sense that each table is independent of
        // the ones listed after it — SQLite doesn't enforce the declared
        // FK constraints by default (requires PRAGMA foreign_keys=ON, which
        // we don't set), but writing deletes leaf-first keeps behavior
        // consistent if someone ever flips that pragma on in the future.
        diesel::delete(punish::table).execute(conn)?;
        diesel::delete(game_player_stat::table).execute(conn)?;
        diesel::delete(gamePlayer::table).execute(conn)?;
        let deleted = diesel::delete(game::table).execute(conn)?;

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

pub fn prompt_user(prompt: &str, newline: bool) -> Result<String> {

    if newline {
        println!("{}", prompt);
    } else {
        print!("{}", prompt);
    }

    io::stdout().flush()?;
    let mut response = String::new();
    io::stdin().read_line(&mut response)?;
    let response = response.trim_end().to_owned();
    Ok(response)
}

pub fn establish_connection() -> Result<SqliteConnection> {
    dotenv().ok();

    let database_url = database_url()?;

    SqliteConnection::establish(&database_url)
        .map_err(|_e| anyhow!("Error connecting to {}", database_url))
}

/// Open the SQLite database at `path` and apply any pending migrations.
///
/// Parent directories are created on demand, so callers (e.g. the GUI on
/// first launch) can point at `~/Library/.../stats_melee.db` before it
/// exists and have the file + schema come up cleanly.
pub fn open_database<P: AsRef<Path>>(path: P) -> Result<SqliteConnection> {
    let path = path.as_ref();

    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)
                .map_err(|e| anyhow!("mkdir {}: {e}", parent.display()))?;
        }
    }

    let url = path
        .to_str()
        .ok_or_else(|| anyhow!("db path is not utf-8: {}", path.display()))?;

    let mut conn = SqliteConnection::establish(url)
        .map_err(|e| anyhow!("opening {}: {e}", url))?;

    // Concurrency hardening. The GUI keeps several connections open against
    // the same file at once — the UI thread plus the ingest and summary
    // workers, which each open their own handle (SqliteConnection is
    // !Send). Under SQLite's default rollback journal a writer takes an
    // exclusive lock that blocks every reader, so an in-flight scan made
    // the Library/Analytics reads fail outright with "database is locked".
    //
    //   - journal_mode = WAL: readers see the last committed snapshot and
    //     no longer block on an active writer (the common case here).
    //   - busy_timeout: when two writers *do* contend (e.g. a delete during
    //     a scan), wait up to 5 s for the lock instead of erroring at once.
    //   - synchronous = NORMAL: the safe pairing with WAL; fewer fsyncs.
    //
    // WAL is a persistent property of the file, so setting it on any open
    // sticks for all connections; busy_timeout is per-connection, hence set
    // on every open.
    conn.batch_execute(
        "PRAGMA busy_timeout = 5000; \
         PRAGMA journal_mode = WAL; \
         PRAGMA synchronous = NORMAL;",
    )
    .map_err(|e| anyhow!("configuring sqlite at {}: {e}", url))?;

    conn.run_pending_migrations(MIGRATIONS)
        .map_err(|e| anyhow!("running migrations at {}: {e}", url))?;

    Ok(conn)
}

pub fn database_url() -> Result<String> {
    env::var("DATABASE_URL").map_err(|_| anyhow!("no database url found"))
}

#[cfg(test)]
mod hash_tests {
    use super::*;
    // `Write` (for `f.write_all`) is already in scope via `super::*` —
    // lib.rs's top-level `use std::io::{self, Write}` re-exports it.

    #[test]
    fn hash_slp_file_is_stable_and_content_addressed() {
        let dir = tempfile::tempdir().expect("tempdir");
        let a = dir.path().join("a.slp");
        let b = dir.path().join("b.slp");
        let c = dir.path().join("c.slp");

        // a == c by content; b differs.
        fs::write(&a, b"hello world").expect("write a");
        fs::write(&b, b"hello world!").expect("write b");
        fs::write(&c, b"hello world").expect("write c");

        let ha = hash_slp_file(&a).expect("hash a");
        let hb = hash_slp_file(&b).expect("hash b");
        let hc = hash_slp_file(&c).expect("hash c");

        assert_eq!(ha.len(), 64, "sha256 hex digest should be 64 chars");
        assert!(
            ha.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
            "expected lowercase hex, got {ha}"
        );

        assert_eq!(ha, hc, "same content → same hash");
        assert_ne!(ha, hb, "different content → different hash");

        // Stability: hashing twice gives the same result.
        let ha2 = hash_slp_file(&a).expect("hash a again");
        assert_eq!(ha, ha2);
    }

    #[test]
    fn hash_slp_file_streams_large_input() {
        // Sanity: hashing a few-MB file shouldn't fail or produce a
        // weirdly-sized digest. Write a buffer larger than any internal
        // copy chunk size to exercise the streaming path.
        let dir = tempfile::tempdir().expect("tempdir");
        let p = dir.path().join("big.slp");
        let mut f = fs::File::create(&p).expect("create big");
        let chunk = [0xABu8; 64 * 1024];
        for _ in 0..200 {
            f.write_all(&chunk).expect("write chunk");
        }
        drop(f);
        let h = hash_slp_file(&p).expect("hash big");
        assert_eq!(h.len(), 64);
    }

    #[test]
    fn hash_slp_file_errors_on_missing_path() {
        let dir = tempfile::tempdir().expect("tempdir");
        let p = dir.path().join("does-not-exist.slp");
        assert!(hash_slp_file(&p).is_err());
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
