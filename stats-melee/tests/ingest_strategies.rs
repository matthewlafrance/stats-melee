//! The three ingest work-distribution strategies: same rows, different clocks.
//!
//! Two tests live here and they answer different questions.
//!
//! * [`strategies_produce_identical_rows`] is the regression test — it runs
//!   all three strategies over the bundled fixture corpus and asserts they
//!   write byte-identical rows. Chunked partitioning that drops an index, or
//!   a dynamic cursor that hands the same index to two workers, shows up
//!   here. It runs on every `cargo test` (skipping when the corpus, which is
//!   gitignored, is absent).
//!
//! * [`bench_ingest_strategies`] is the measurement. It is opt-in: point
//!   `STATS_MELEE_BENCH_REPLAYS` at a replay directory and run with
//!   `--nocapture` to see the table. Without that variable it prints a note
//!   and returns, so CI stays fast.
//!
//! ```sh
//! STATS_MELEE_BENCH_REPLAYS=~/Slippi \
//!   cargo test --release -p stats-melee --test ingest_strategies -- --nocapture
//! ```
//!
//! Build in `--release`. A debug-build parse is dominated by unoptimized
//! peppi work, which flattens the differences between the strategies.
//!
//! Extra knobs:
//!
//! * `STATS_MELEE_BENCH_LIMIT` — cap the corpus at N files (default: all).
//! * `STATS_MELEE_BENCH_REPS`  — repetitions per strategy (default: 3).

use std::collections::BTreeMap;
use std::path::Path;
use std::time::{Duration, Instant};

use diesel::prelude::*;
use stats_melee::schema::game::dsl as game_dsl;
use stats_melee::testing::{corpus_from_env, fixtures_dir, slps_recursive, stage_corpus, TestDb};
use stats_melee::{parse_new_replays_timed_at, IngestStrategy, IngestTimings};

/// Read every staged file once so the timed runs all start from a warm page
/// cache. Without this the first strategy measured pays for every cold read
/// and looks slow for reasons that have nothing to do with scheduling.
fn warm_page_cache(root: &Path) {
    for slp in slps_recursive(root) {
        let _ = std::fs::read(&slp);
    }
}

// ---------------------------------------------------------------------------
// Row-level comparison
// ---------------------------------------------------------------------------

/// Ingest `root` into a fresh database with `strategy`, returning the row
/// count the ingester reported and how long the whole call took.
fn run_strategy(root: &Path, strategy: IngestStrategy) -> (usize, Duration, IngestTimings, TestDb) {
    let mut db = TestDb::new().expect("tempdir db");
    let db_path = db.path.clone();

    // `elapsed` is end-to-end and includes the directory scan + dedup that
    // every strategy shares; `timings.total` covers only the parse + insert
    // phase the strategies actually differ in.
    let started = Instant::now();
    let (ingested, timings) = parse_new_replays_timed_at(&mut db.conn, root, &db_path, strategy)
        .expect("ingest should succeed");
    let elapsed = started.elapsed();

    (ingested, elapsed, timings, db)
}

/// The ingested corpus reduced to something comparable across runs:
/// `replay_path -> content_hash`.
///
/// Deliberately excludes `game.id` and `ingested_at`. Ids are assigned in
/// parse-*completion* order, so they legitimately differ between strategies
/// — that is documented behaviour, not a bug, and asserting on them would
/// make this test fail for the wrong reason.
fn row_fingerprints(db: &mut TestDb) -> BTreeMap<String, Option<String>> {
    game_dsl::game
        .select((game_dsl::replay_path, game_dsl::content_hash))
        .load::<(Option<String>, Option<String>)>(&mut db.conn)
        .expect("load replay_path + content_hash")
        .into_iter()
        .map(|(path, hash)| (path.expect("ingested rows carry a replay_path"), hash))
        .collect()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// All three strategies must ingest the same corpus into the same rows.
///
/// This is the test that earns the extra code: `Chunked` and `Dynamic` hand
/// out indices by completely different mechanisms, and either could silently
/// skip or duplicate a file. Comparing `replay_path -> content_hash` maps
/// catches both.
#[test]
fn strategies_produce_identical_rows() {
    let fixtures = fixtures_dir();
    if slps_recursive(&fixtures).is_empty() {
        // Corpus is local-only (gitignored) — skip rather than fail on a
        // clean checkout or in CI.
        return;
    }

    // Enough files that the work actually spreads over several workers.
    let (root, staged) = stage_corpus(&fixtures, Some(24));
    assert!(staged > 0, "staged an empty corpus from {}", fixtures.display());

    let mut baseline: Option<(usize, BTreeMap<String, Option<String>>)> = None;

    for strategy in IngestStrategy::ALL {
        let (ingested, _elapsed, _timings, mut db) = run_strategy(root.path(), strategy);
        let fingerprints = row_fingerprints(&mut db);

        assert_eq!(
            ingested,
            fingerprints.len(),
            "{}: reported {ingested} ingested but wrote {} rows",
            strategy.name(),
            fingerprints.len()
        );

        // A corpus the parser rejects wholesale would otherwise satisfy
        // every assertion below trivially: 0 == 0 for all three strategies.
        assert!(
            ingested > 0,
            "{} ingested 0 of {staged} staged replays — the strategies agree \
             only because none of them did any work",
            strategy.name()
        );

        match &baseline {
            None => baseline = Some((ingested, fingerprints)),
            Some((base_count, base_rows)) => {
                assert_eq!(
                    ingested,
                    *base_count,
                    "{} ingested {ingested} files, serial ingested {base_count}",
                    strategy.name()
                );
                assert_eq!(
                    &fingerprints,
                    base_rows,
                    "{} wrote different rows than serial",
                    strategy.name()
                );
            }
        }
    }
}

/// Time all three strategies over one corpus and print the comparison.
///
/// Opt in with `STATS_MELEE_BENCH_REPLAYS`; see the module docs. Timings
/// cover the whole `parse_new_replays_with` call, including the directory
/// scan and dedup phase that every strategy shares, so the speedups printed
/// are end-to-end rather than parse-phase-only.
#[test]
fn bench_ingest_strategies() {
    let Some(corpus) = corpus_from_env("STATS_MELEE_BENCH_REPLAYS") else {
        eprintln!(
            "bench_ingest_strategies: skipped — set STATS_MELEE_BENCH_REPLAYS \
             to a replay directory to run it (see the module docs)."
        );
        return;
    };
    assert!(
        corpus.is_dir(),
        "STATS_MELEE_BENCH_REPLAYS is not a directory: {}",
        corpus.display()
    );

    let limit = env_usize("STATS_MELEE_BENCH_LIMIT");
    let reps = env_usize("STATS_MELEE_BENCH_REPS").unwrap_or(3).max(1);

    let (root, staged) = stage_corpus(&corpus, limit);
    assert!(
        staged > 0,
        "no .slp files found under {} — expected replays at any depth",
        corpus.display()
    );
    warm_page_cache(root.path());

    let workers = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(0);

    eprintln!();
    eprintln!("ingest strategy benchmark");
    eprintln!("  corpus      {}", corpus.display());
    eprintln!("  staged      {staged} replays");
    eprintln!("  reps        {reps} per strategy (best of)");
    eprintln!("  parallelism {workers} logical cores");
    eprintln!(
        "  profile     {}",
        if cfg!(debug_assertions) {
            "debug — rebuild with --release for meaningful numbers"
        } else {
            "release"
        }
    );
    eprintln!();

    // One rep of every strategy, then the next rep — not all reps of one
    // strategy back to back. Sustained all-core work heats a machine and
    // slows it down, so grouping by strategy hands the whole penalty to
    // whichever one runs last. Interleaving spreads it evenly.
    let mut best: Vec<Duration> = vec![Duration::MAX; IngestStrategy::ALL.len()];
    let mut total: Vec<Duration> = vec![Duration::ZERO; IngestStrategy::ALL.len()];
    let mut counts: Vec<usize> = vec![0; IngestStrategy::ALL.len()];
    // Keep the breakdown from each strategy's *fastest* rep — the later
    // reps run on a hot machine and their absolute numbers drift.
    let mut breakdown: Vec<IngestTimings> = vec![IngestTimings::default(); IngestStrategy::ALL.len()];

    for rep in 0..reps {
        for (i, strategy) in IngestStrategy::ALL.into_iter().enumerate() {
            let (ingested, elapsed, timings, _db) = run_strategy(root.path(), strategy);
            if rep == 0 {
                counts[i] = ingested;
            } else {
                assert_eq!(
                    ingested, counts[i],
                    "{} ingested {ingested} on rep {rep} but {} on rep 0",
                    strategy.name(),
                    counts[i]
                );
            }
            if elapsed < best[i] {
                breakdown[i] = timings;
            }
            best[i] = best[i].min(elapsed);
            total[i] += elapsed;
            eprintln!(
                "  rep {}/{reps} {:<8} {:>7.2}s",
                rep + 1,
                strategy.name(),
                elapsed.as_secs_f64()
            );
        }
    }

    let results: Vec<(IngestStrategy, usize, Duration, Duration)> = IngestStrategy::ALL
        .into_iter()
        .enumerate()
        .map(|(i, s)| (s, counts[i], best[i], total[i] / reps as u32))
        .collect();
    eprintln!();

    // Every strategy must have ingested the same number of files, or the
    // timings are measuring different amounts of work.
    let expected = results[0].1;
    // ...and it must not be zero. A corpus the parser rejects wholesale
    // still succeeds at every layer below this — the workers distribute the
    // failures just as happily as they would real work — so the table would
    // report a speedup for an ingest that wrote no rows at all.
    assert!(
        expected > 0,
        "every strategy ingested 0 replays from {} — the corpus was staged \
         ({staged} files) but nothing parsed, so the timings measure only \
         the failure path. Check the parse errors logged above.",
        corpus.display()
    );
    for (strategy, ingested, _, _) in &results {
        assert_eq!(
            *ingested,
            expected,
            "{} ingested {ingested} files, {} ingested {expected} — \
             timings are not comparable",
            strategy.name(),
            results[0].0.name()
        );
    }

    let serial_best = results
        .iter()
        .find(|(s, ..)| *s == IngestStrategy::Serial)
        .map(|(_, _, best, _)| *best)
        .expect("serial is always measured");

    eprintln!(
        "  {:<9} {:>10} {:>10} {:>12} {:>11} {:>9}",
        "strategy", "best", "mean", "per replay", "replays/s", "speedup"
    );
    eprintln!("  {}", "-".repeat(65));
    for (strategy, ingested, best, mean) in &results {
        let per_file = best.as_secs_f64() / (*ingested).max(1) as f64;
        let rate = if best.as_secs_f64() > 0.0 {
            *ingested as f64 / best.as_secs_f64()
        } else {
            f64::INFINITY
        };
        let speedup = serial_best.as_secs_f64() / best.as_secs_f64();
        eprintln!(
            "  {:<9} {:>9.2}s {:>9.2}s {:>10.1}ms {:>11.1} {:>8.2}x",
            strategy.name(),
            best.as_secs_f64(),
            mean.as_secs_f64(),
            per_file * 1000.0,
            rate,
            speedup
        );
    }
    // --- where the time actually went -------------------------------------
    //
    // The speedup table above says how much parallelism bought. This one
    // says why it stopped there. `insert busy` is the serialized section:
    // by Amdahl's law it caps the achievable speedup at 1/its share of wall
    // time, no matter how many cores parse. `send blocked` is workers
    // waiting on a full channel (the writer is the constraint); `insert
    // blocked` is the inserter waiting on an empty one (parsing is).
    eprintln!("  time breakdown, from each strategy's fastest rep");
    eprintln!(
        "  {:<9} {:>9} {:>11} {:>11} {:>13} {:>10} {:>9}",
        "strategy", "phase", "parse busy", "insert busy", "insert blocked", "send blkd", "par."
    );
    eprintln!("  {}", "-".repeat(78));
    for (i, strategy) in IngestStrategy::ALL.into_iter().enumerate() {
        let t = &breakdown[i];
        eprintln!(
            "  {:<9} {:>8.2}s {:>10.2}s {:>10.2}s {:>12.2}s {:>9.2}s {:>8.2}x",
            strategy.name(),
            t.total.as_secs_f64(),
            t.parse_busy.as_secs_f64(),
            t.insert_busy.as_secs_f64(),
            t.insert_blocked.as_secs_f64(),
            t.send_blocked.as_secs_f64(),
            t.effective_parallelism(),
        );
    }
    eprintln!();
    eprintln!("  worker load balance (busiest vs idlest thread, fastest rep)");
    eprintln!(
        "  {:<9} {:>12} {:>12} {:>11}",
        "strategy", "idlest", "busiest", "imbalance"
    );
    eprintln!("  {}", "-".repeat(48));
    for (i, strategy) in IngestStrategy::ALL.into_iter().enumerate().skip(1) {
        let t = &breakdown[i];
        eprintln!(
            "  {:<9} {:>11.3}s {:>11.3}s {:>10.1}%",
            strategy.name(),
            t.worker_busy_min.as_secs_f64(),
            t.worker_busy_max.as_secs_f64(),
            t.imbalance() * 100.0,
        );
    }
    eprintln!();

    // Interpretation. The Amdahl ceiling is a property of the *serial*
    // split — how much of a one-threaded run is unparallelizable insert
    // work. Computing it from a parallel run instead is circular: once the
    // writer saturates, insert-busy is ~100% of wall by construction, which
    // would report a ceiling of 1.00x no matter how fast things got.
    let serial_t = &breakdown[0];
    let ceiling = if serial_t.serial_fraction() > 0.0 {
        1.0 / serial_t.serial_fraction()
    } else {
        f64::INFINITY
    };
    eprintln!(
        "  serial split: {:.1}% parse / {:.1}% insert  ->  Amdahl ceiling {:.2}x",
        100.0 - serial_t.serial_fraction() * 100.0,
        serial_t.serial_fraction() * 100.0,
        ceiling
    );
    for (i, strategy) in IngestStrategy::ALL.into_iter().enumerate().skip(1) {
        let t = &breakdown[i];
        let achieved = serial_best.as_secs_f64() / best[i].as_secs_f64();
        eprintln!(
            "  {:<9} achieved {:.2}x ({:.0}% of ceiling); inserter {:.1}% busy, \
             starved {:.2}s, workers blocked on a full channel {:.1}s",
            strategy.name(),
            achieved,
            achieved / ceiling * 100.0,
            t.serial_fraction() * 100.0,
            t.insert_blocked.as_secs_f64(),
            t.send_blocked.as_secs_f64(),
        );
    }
    eprintln!();
    eprintln!("  {expected} replays ingested per run.");
    eprintln!();
}

// ---------------------------------------------------------------------------
// Small helpers
// ---------------------------------------------------------------------------

fn env_usize(key: &str) -> Option<usize> {
    std::env::var(key).ok()?.trim().parse().ok()
}

