//! Walking a replay folder and getting its games into the database.
//!
//! Scan for `.slp` files the database has not seen, parse and hash them
//! across a worker pool, and feed the results to a single inserter over a
//! bounded channel. [`IngestStrategy`] selects how the per-file work is
//! spread across threads; [`IngestTimings`] reports where the time went.

use anyhow::Result;
use diesel::prelude::*;
use std::fs;
use std::path::Path;

use crate::gamedata::GameData;
use crate::{hash_slp_file, parse_single_replay, post_game_full};

/// How [`parse_new_replays_with`] spreads the per-file parse + hash work
/// across threads.
///
/// All three produce identical rows; they differ only in how file indices
/// reach the workers, and whether there are workers at all. Production uses
/// [`Dynamic`](IngestStrategy::Dynamic). The other two exist so the choice
/// between them can be measured rather than argued about —
/// `tests/ingest_strategies.rs` times all three over one corpus and checks
/// they agree row for row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IngestStrategy {
    /// One thread: parse, hash, and insert each file in turn. The baseline
    /// every other strategy is measured against.
    Serial,
    /// Static block partitioning — the file list is cut into `n_workers`
    /// contiguous chunks up front, one per thread. No coordination at all
    /// after the split, but a thread that happens to draw several large
    /// replays keeps working while its neighbours sit idle: the wall clock
    /// is set by the slowest chunk, not the average one.
    Chunked,
    /// Dynamic self-scheduling — workers share a single atomic cursor and
    /// claim the next unparsed index whenever they finish one (OpenMP's
    /// `schedule(dynamic, 1)`). One relaxed `fetch_add` per file is
    /// negligible beside parsing that file, and no thread can be stranded
    /// behind a chunk of expensive replays.
    ///
    /// Not work stealing: that (Rayon, Cilk, ForkJoinPool) gives every
    /// worker a private deque and has idle workers steal from the tails of
    /// other workers' deques. This is one shared counter, no per-worker
    /// queues, nothing to steal.
    Dynamic,
}

impl IngestStrategy {
    /// Every strategy, in the order the benchmark reports them.
    pub const ALL: [IngestStrategy; 3] = [
        IngestStrategy::Serial,
        IngestStrategy::Chunked,
        IngestStrategy::Dynamic,
    ];

    /// Short lowercase label for logs and benchmark tables.
    pub fn name(self) -> &'static str {
        match self {
            IngestStrategy::Serial => "serial",
            IngestStrategy::Chunked => "chunked",
            IngestStrategy::Dynamic => "dynamic",
        }
    }
}

/// Where an ingest run's wall time actually went.
///
/// The pipeline is N parse workers feeding one serialized inserter over a
/// bounded channel, so the bottleneck announces itself as *blocking*, and
/// which side blocks says which side is the constraint:
///
/// * `send_blocked` large  → the channel is full; workers are waiting on the
///   inserter, so the writer is the limit.
/// * `insert_blocked` large → the channel is empty; the inserter is starved,
///   so parsing is the limit.
///
/// Worker figures are summed across threads, so `parse_busy` can exceed
/// `total` — divided by `total` it gives effective parallelism.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct IngestTimings {
    /// Wall time for the parse + insert phase (excludes the directory scan).
    pub total: std::time::Duration,
    /// Time the inserter spent inside `post_game_full`. This is the
    /// serialized section — the part no amount of parallelism can shrink.
    pub insert_busy: std::time::Duration,
    /// Time the inserter spent waiting for a parsed game to arrive.
    pub insert_blocked: std::time::Duration,
    /// Parse + hash time, summed across every worker.
    pub parse_busy: std::time::Duration,
    /// Busy time of the *least* and *most* loaded worker.
    ///
    /// This is the load-imbalance readout, and it is the whole difference
    /// between the two threaded strategies. Static chunking fixes each
    /// worker's share up front, so an unlucky draw of large replays — or
    /// simply landing on an efficiency core — leaves one worker still
    /// grinding while the rest idle, and the run ends when the slowest one
    /// does. Dynamic self-scheduling lets a fast worker keep claiming
    /// files, so the spread should collapse.
    pub worker_busy_min: std::time::Duration,
    pub worker_busy_max: std::time::Duration,
    /// Time workers spent blocked on a full channel, summed across workers.
    pub send_blocked: std::time::Duration,
    /// Worker threads used (1 for `Serial`).
    pub workers: usize,
}

impl IngestTimings {
    /// Share of wall time spent in the serialized inserter. By Amdahl's law
    /// this is what caps the achievable speedup at `1.0 / serial_fraction`,
    /// however many cores are thrown at the parse phase.
    pub fn serial_fraction(&self) -> f64 {
        if self.total.is_zero() {
            return 0.0;
        }
        self.insert_busy.as_secs_f64() / self.total.as_secs_f64()
    }

    /// Spread between the busiest and idlest worker, as a fraction of the
    /// busiest. 0.0 is perfect balance; 0.5 means the idlest worker did half
    /// the work of the busiest and spent the difference doing nothing.
    pub fn imbalance(&self) -> f64 {
        let max = self.worker_busy_max.as_secs_f64();
        if max <= 0.0 {
            return 0.0;
        }
        (max - self.worker_busy_min.as_secs_f64()) / max
    }

    /// Aggregate worker busy time over wall time — how many cores' worth of
    /// parsing actually happened, against `workers` requested.
    pub fn effective_parallelism(&self) -> f64 {
        if self.total.is_zero() {
            return 0.0;
        }
        self.parse_busy.as_secs_f64() / self.total.as_secs_f64()
    }
}

/// Which index-handout scheme the threaded path uses. Private mirror of the
/// two threaded [`IngestStrategy`] variants, so the worker loop never has to
/// consider a `Serial` case it can't reach.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ParallelMode {
    Chunked,
    Dynamic,
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
/// `db_path` is ignored. It remains in the signature for API compatibility;
/// callers can pass any path.
pub fn parse_new_replays<P: AsRef<Path>, Q: AsRef<Path>>(
    conn: &mut SqliteConnection,
    root: P,
    db_path: Q,
) -> Result<usize> {
    parse_new_replays_with(conn, root, db_path, IngestStrategy::Dynamic)
}

/// [`parse_new_replays`], with the work-distribution strategy named
/// explicitly instead of defaulting to [`IngestStrategy::Dynamic`].
///
/// Every strategy sees the same candidate list and inserts through the same
/// serialized path, so the returned count — and the rows written — must not
/// depend on which one is chosen. That invariant is what
/// `tests/ingest_strategies.rs` checks before it reports any timings.
pub fn parse_new_replays_with<P: AsRef<Path>, Q: AsRef<Path>>(
    conn: &mut SqliteConnection,
    root: P,
    _db_path: Q,
    strategy: IngestStrategy,
) -> Result<usize> {
    let new_paths = collect_new_replay_paths(conn, root)?;
    if new_paths.is_empty() {
        return Ok(0);
    }

    Ok(parse_new_replays_timed(conn, new_paths, strategy).0)
}

/// [`parse_new_replays_with`], additionally returning where the time went.
///
/// Used by the ingest benchmark to attribute wall time to the parse phase
/// versus the serialized insert phase. Production calls the untimed form;
/// the instrumentation is four `Instant::now()` calls per file against
/// milliseconds of real work, so it is on in both paths rather than behind
/// a feature flag that would let it rot.
pub fn parse_new_replays_timed_at<P: AsRef<Path>, Q: AsRef<Path>>(
    conn: &mut SqliteConnection,
    root: P,
    _db_path: Q,
    strategy: IngestStrategy,
) -> Result<(usize, IngestTimings)> {
    let new_paths = collect_new_replay_paths(conn, root)?;
    if new_paths.is_empty() {
        return Ok((0, IngestTimings::default()));
    }
    Ok(parse_new_replays_timed(conn, new_paths, strategy))
}

/// Dispatch to the chosen strategy over an already-collected path list.
fn parse_new_replays_timed(
    conn: &mut SqliteConnection,
    new_paths: Vec<(std::path::PathBuf, String)>,
    strategy: IngestStrategy,
) -> (usize, IngestTimings) {
    match strategy {
        IngestStrategy::Serial => ingest_serial(conn, &new_paths),
        IngestStrategy::Chunked => ingest_parallel(conn, &new_paths, ParallelMode::Chunked),
        IngestStrategy::Dynamic => ingest_parallel(conn, &new_paths, ParallelMode::Dynamic),
    }
}

/// Collect `root/<subdir>/*.slp` and drop anything already in the database.
///
/// Returns `(path_on_disk, canonical_path_string)` pairs — the canonical
/// string is what lands in `game.replay_path`, and what dedup compares.
fn collect_new_replay_paths<P: AsRef<Path>>(
    conn: &mut SqliteConnection,
    root: P,
) -> Result<Vec<(std::path::PathBuf, String)>> {
    use crate::schema::game::dsl as game_dsl;
    use std::collections::HashSet;
    use std::path::PathBuf;

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
        ?
        .into_iter()
        .flatten()
        .collect();

    // 3. Canonicalize + drop already-ingested files. Canonicalizing collapses
    //    two spellings of the same path (matches the UNIQUE index); fall back
    //    to the original path if it fails (network FS quirks, permissions).
    Ok(candidates
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
        .collect())
}

/// The whole per-file CPU cost: parse the replay, then hash its bytes.
///
/// Returns `None` when the file is unusable — every strategy skips such a
/// file identically, which is what lets their row counts be compared.
fn parse_and_hash(path: &Path) -> Option<(GameData, Option<String>)> {
    use std::panic::{catch_unwind, AssertUnwindSafe};

    // Real-world .slp files occasionally tickle panics in peppi (truncated
    // headers, unexpected events). catch_unwind keeps one bad file from
    // taking the whole scan (and the app process) down; log and move on.
    let parsed = catch_unwind(AssertUnwindSafe(|| parse_single_replay(path)));
    let gamedata = match parsed {
        Ok(Ok(g)) => g,
        Ok(Err(e)) => {
            eprintln!("stats-melee: skipping {}: parse error: {e}", path.display());
            return None;
        }
        Err(_panic) => {
            eprintln!("stats-melee: skipping {}: parser panicked", path.display());
            return None;
        }
    };

    // SHA-256 for the analysis sidecar cache key. A failure here is
    // non-fatal — we store None and the viewer falls back to a re-parse for
    // that row.
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

    Some((gamedata, content_hash))
}

/// Insert one parsed game. Returns whether the row actually landed.
///
/// Errors here can be UNIQUE-constraint violations (a concurrent
/// double-scan) or other schema issues — log + skip rather than abort the
/// whole scan.
fn insert_parsed(
    conn: &mut SqliteConnection,
    gamedata: &GameData,
    canonical: &str,
    content_hash: Option<&str>,
) -> bool {
    if let Err(e) = post_game_full(conn, gamedata, Some(canonical), content_hash) {
        eprintln!("stats-melee: skipping {canonical}: insert error: {e}");
        return false;
    }
    true
}

/// How many worker threads to spin up for `total` files.
fn worker_count(total: usize) -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
        .min(total)
        .max(1)
}

/// The contiguous slice of `0..total` assigned to `worker` under static
/// block partitioning.
///
/// The first `total % n_workers` workers take one extra item, so chunk sizes
/// differ by at most one and every index is covered exactly once.
fn chunk_range(worker: usize, n_workers: usize, total: usize) -> std::ops::Range<usize> {
    debug_assert!(worker < n_workers);
    let base = total / n_workers;
    let rem = total % n_workers;
    let start = worker * base + worker.min(rem);
    let len = base + usize::from(worker < rem);
    start..start + len
}

/// [`IngestStrategy::Serial`]: no threads, no channel — parse, hash, and
/// insert each file before moving to the next.
fn ingest_serial(
    conn: &mut SqliteConnection,
    new_paths: &[(std::path::PathBuf, String)],
) -> (usize, IngestTimings) {
    use std::time::Instant;

    let started = Instant::now();
    let mut t = IngestTimings {
        workers: 1,
        ..Default::default()
    };
    let mut count = 0usize;

    for (path, canonical) in new_paths {
        // With one thread the two phases simply alternate, so there is no
        // blocking to attribute — every nanosecond is either parse or
        // insert. That makes this run the baseline the parallel splits are
        // compared against.
        let parse_start = Instant::now();
        let parsed = parse_and_hash(path);
        t.parse_busy += parse_start.elapsed();

        let Some((gamedata, content_hash)) = parsed else {
            continue;
        };

        let insert_start = Instant::now();
        let ok = insert_parsed(conn, &gamedata, canonical, content_hash.as_deref());
        t.insert_busy += insert_start.elapsed();
        if ok {
            count += 1;
        }
    }

    t.total = started.elapsed();
    t.worker_busy_min = t.parse_busy;
    t.worker_busy_max = t.parse_busy;
    (count, t)
}

/// The threaded path shared by [`IngestStrategy::Chunked`] and
/// [`IngestStrategy::Dynamic`].
///
/// Both spawn the same number of workers, do the same per-file work, and
/// funnel results to the same single inserter over the same bounded channel.
/// The *only* difference is where a worker's next index comes from, which is
/// the point: any timing gap between the two is attributable to scheduling
/// and nothing else.
fn ingest_parallel(
    conn: &mut SqliteConnection,
    new_paths: &[(std::path::PathBuf, String)],
    mode: ParallelMode,
) -> (usize, IngestTimings) {
    use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
    use std::sync::mpsc::sync_channel;
    use std::time::{Duration, Instant};

    let started = Instant::now();
    // Workers accumulate into shared counters rather than reporting at the
    // end, so a worker that exits early (dropped receiver) still contributes
    // everything it did before stopping.
    let send_blocked_ns = AtomicU64::new(0);

    let total = new_paths.len();
    let n_workers = worker_count(total);
    // Per-worker rather than one shared counter, so the spread between the
    // busiest and idlest thread is visible — that spread is exactly what
    // separates static chunking from self-scheduling.
    let parse_ns: Vec<AtomicU64> = (0..n_workers).map(|_| AtomicU64::new(0)).collect();

    // Shared cursor the dynamic workers claim files from. Untouched under
    // `Chunked`, where each worker's range is fixed before it starts.
    let next = AtomicUsize::new(0);
    // Bounded so parsing can't outrun the inserter into unbounded memory.
    let (tx, rx) =
        sync_channel::<(GameData, String, Option<String>)>(n_workers.saturating_mul(4).max(8));

    let scoped = std::thread::scope(|scope| {
        for w in 0..n_workers {
            let tx = tx.clone();
            let next = &next;
            let parse_ns = &parse_ns[w];
            let send_blocked_ns = &send_blocked_ns;
            scope.spawn(move || {
                let mut chunk = chunk_range(w, n_workers, total);
                loop {
                    let i = match mode {
                        ParallelMode::Dynamic => {
                            let i = next.fetch_add(1, Ordering::Relaxed);
                            if i >= total {
                                break;
                            }
                            i
                        }
                        ParallelMode::Chunked => match chunk.next() {
                            Some(i) => i,
                            None => break,
                        },
                    };
                    let (path, canonical) = &new_paths[i];

                    let parse_start = Instant::now();
                    let parsed = parse_and_hash(path);
                    parse_ns.fetch_add(
                        parse_start.elapsed().as_nanos() as u64,
                        Ordering::Relaxed,
                    );

                    let Some((gamedata, content_hash)) = parsed else {
                        continue;
                    };

                    // `send` only blocks once the channel is full, i.e. once
                    // the inserter has fallen behind — so time spent here is
                    // a direct measure of backpressure from the writer.
                    let send_start = Instant::now();
                    let sent = tx.send((gamedata, canonical.clone(), content_hash));
                    send_blocked_ns.fetch_add(
                        send_start.elapsed().as_nanos() as u64,
                        Ordering::Relaxed,
                    );

                    // If the inserter has gone away the receiver is dropped; stop.
                    if sent.is_err() {
                        break;
                    }
                }
            });
        }
        // Drop our own sender so `rx` ends once every worker's clone is gone.
        drop(tx);

        // Insert as results stream in, splitting the wait from the work:
        // time in `recv` means the inserter is starved (parsing is the
        // limit), time in `insert_parsed` is the serialized section itself.
        let mut count: usize = 0;
        let mut insert_busy = Duration::ZERO;
        let mut insert_blocked = Duration::ZERO;
        loop {
            let recv_start = Instant::now();
            let next = rx.recv();
            insert_blocked += recv_start.elapsed();

            let Ok((gamedata, canonical, content_hash)) = next else {
                break; // every sender dropped: the run is done
            };

            let insert_start = Instant::now();
            let ok = insert_parsed(conn, &gamedata, &canonical, content_hash.as_deref());
            insert_busy += insert_start.elapsed();
            if ok {
                count += 1;
            }
        }
        (count, insert_busy, insert_blocked)
    });

    let (count, insert_busy, insert_blocked) = scoped;
    let per_worker: Vec<u64> = parse_ns.iter().map(|a| a.load(Ordering::Relaxed)).collect();
    (
        count,
        IngestTimings {
            total: started.elapsed(),
            insert_busy,
            insert_blocked,
            parse_busy: Duration::from_nanos(per_worker.iter().sum()),
            worker_busy_min: Duration::from_nanos(per_worker.iter().copied().min().unwrap_or(0)),
            worker_busy_max: Duration::from_nanos(per_worker.iter().copied().max().unwrap_or(0)),
            send_blocked: Duration::from_nanos(send_blocked_ns.load(Ordering::Relaxed)),
            workers: n_workers,
        },
    )
}


#[cfg(test)]
mod partition_tests {
    use super::chunk_range;

    /// Static partitioning must cover `0..total` exactly once, with chunk
    /// sizes differing by at most one. If this breaks, `Chunked` silently
    /// drops or double-parses replays and the benchmark compares nothing.
    #[test]
    fn chunk_ranges_tile_the_index_space() {
        for total in 0..40usize {
            for n_workers in 1..=8usize {
                let ranges: Vec<_> = (0..n_workers)
                    .map(|w| chunk_range(w, n_workers, total))
                    .collect();

                let covered: Vec<usize> = ranges.iter().cloned().flatten().collect();
                let expected: Vec<usize> = (0..total).collect();
                assert_eq!(
                    covered, expected,
                    "total={total} n_workers={n_workers} ranges={ranges:?}"
                );

                let lens: Vec<usize> = ranges.iter().map(|r| r.len()).collect();
                let (min, max) = (
                    lens.iter().copied().min().unwrap(),
                    lens.iter().copied().max().unwrap(),
                );
                assert!(
                    max - min <= 1,
                    "unbalanced chunks for total={total} n_workers={n_workers}: {lens:?}"
                );
            }
        }
    }
}
