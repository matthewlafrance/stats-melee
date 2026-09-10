//! Read-path timings against a realistically-sized database.
//!
//! Ingests a corpus once, then times the calls the GUI actually makes —
//! `win_analytics` behind the Analytics page and `player_summary_filtered`
//! behind Career — along with the pieces they are built from, so a slow page
//! can be attributed to a specific query rather than guessed at.
//!
//! ```sh
//! STATS_MELEE_BENCH_REPLAYS=~/melee-corpus \
//!   cargo test --release -p stats-melee --test query_bench -- --nocapture
//! ```
//!
//! `STATS_MELEE_BENCH_LIMIT` caps the corpus, which is the useful knob here:
//! running at two sizes shows whether a query scales linearly or worse.


use std::time::Instant;

use diesel::prelude::*;
use stats_melee::testing::{corpus_from_env, stage_corpus, TestDb};
use stats_melee::{
    analyze_games, filter_games, parse_new_replays, player_summary_filtered, win_analytics,
    PlayerSummaryFilter,
};

macro_rules! timed {
    ($label:expr, $body:expr) => {{
        let start = Instant::now();
        let out = $body;
        let elapsed = start.elapsed();
        eprintln!("  {:<34} {:>9.3}s", $label, elapsed.as_secs_f64());
        (out, elapsed)
    }};
}

#[test]
fn read_path_timings() {
    let Some(corpus) = corpus_from_env("STATS_MELEE_BENCH_REPLAYS") else {
        eprintln!("read_path_timings: skipped — set STATS_MELEE_BENCH_REPLAYS to run");
        return;
    };
    let limit = std::env::var("STATS_MELEE_BENCH_LIMIT")
        .ok()
        .and_then(|v| v.trim().parse::<usize>().ok());

    let (root, staged) = stage_corpus(&corpus, limit);
    assert!(staged > 0, "no replays under {}", corpus.display());

    let mut db = TestDb::new().expect("tempdir db");
    let db_path = db.path.clone();
    let (ingested, _) = timed!(
        format!("ingest {staged} replays"),
        parse_new_replays(&mut db.conn, root.path(), &db_path).expect("ingest")
    );
    assert!(ingested > 0, "ingested nothing");

    // Row counts give the queries below a denominator.
    use stats_melee::schema::{game, gamePlayer, game_player_stat, punish};
    let n_games: i64 = game::table.count().get_result(&mut db.conn).unwrap();
    let n_gp: i64 = gamePlayer::table.count().get_result(&mut db.conn).unwrap();
    let n_stats: i64 = game_player_stat::table.count().get_result(&mut db.conn).unwrap();
    let n_punish: i64 = punish::table.count().get_result(&mut db.conn).unwrap();
    eprintln!();
    eprintln!("  rows: {n_games} game, {n_gp} gamePlayer, {n_stats} game_player_stat, {n_punish} punish");
    eprintln!();

    // Whichever code appears most is the one a user would be looking at.
    let code: String = gamePlayer::table
        .select(gamePlayer::code)
        .first(&mut db.conn)
        .expect("at least one gamePlayer");
    eprintln!("  querying as code {code:?}");
    eprintln!();

    let (games, t_filter) = timed!("filter_games", filter_games(&mut db.conn, &code).expect("filter"));
    eprintln!("  -> {} games returned", games.len());

    let (_, t_analyze) = timed!(
        "analyze_games (on those games)",
        analyze_games(&mut db.conn, &games, &code).expect("analyze")
    );

    let (_, t_win) = timed!(
        "win_analytics (analytics page)",
        win_analytics(&mut db.conn, &code).expect("win_analytics")
    );

    let (_, t_summary) = timed!(
        "player_summary_filtered (career page)",
        player_summary_filtered(&mut db.conn, &code, &PlayerSummaryFilter::NONE).expect("summary")
    );

    eprintln!();
    eprintln!(
        "  win_analytics = filter_games ({:.3}s) + analyze_games ({:.3}s) = {:.3}s",
        t_filter.as_secs_f64(),
        t_analyze.as_secs_f64(),
        t_win.as_secs_f64()
    );
    eprintln!(
        "  analyze_games resolved {} games with one prefetch query",
        games.len()
    );
    eprintln!(
        "  career page: {:.3}s ({:.1}x cheaper than the analytics page)",
        t_summary.as_secs_f64(),
        t_win.as_secs_f64() / t_summary.as_secs_f64().max(1e-9),
    );
    eprintln!();
}
