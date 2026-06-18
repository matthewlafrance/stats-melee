//! Sidecar cache for [`crate::combat::ReplayAnalysis`].
//!
//! Wraps the generic [`crate::file_cache::FileCache`] with the
//! analysis-specific bookkeeping: bincode serialization, version /
//! config-hash checks, and the lookup-or-recompute glue the viewer
//! load path needs.
//!
//! # Policy
//!
//! Analysis blobs are tens of KB — small enough that a generous byte
//! budget (`AnalysisCacheConfig::DEFAULT_MAX_BYTES` = 500 MB) holds
//! roughly a thousand games' worth of cached results. `prune_on_drop`
//! is `false` by default, which is the whole point: the cache survives
//! app restarts so the second viewer-open of any replay is instant.
//!
//! See [`crate::file_cache`]'s top-level docs for the contrast against
//! the video cache, which has the opposite policy on every axis.

use std::path::PathBuf;

use anyhow::{anyhow, Result};

use crate::combat::{CachedAnalysis, CombatV2Config, ReplayAnalysis};
use crate::file_cache::{FileCache, FileCacheConfig};

/// Tunable knobs for the analysis cache. `Default` matches the
/// production policy described in the module docs; tests usually
/// build a tighter version with `prune_on_drop = true` so the
/// per-test directory doesn't outlive the test.
#[derive(Debug, Clone, Copy)]
pub struct AnalysisCacheConfig {
    /// Total bytes ceiling. LRU eviction (oldest mtime first) kicks in
    /// when finalize would push over.
    pub max_bytes: u64,
    /// Wipe the cache dir when the [`AnalysisCache`] is dropped. The
    /// default (`false`) is what makes the second viewer-open fast
    /// across restarts; tests flip it to keep tempdirs clean.
    pub prune_on_drop: bool,
}

impl AnalysisCacheConfig {
    /// 500 MB — at ~50 KB per analysis blob this is roughly 10k games,
    /// which is more than any realistic single-user corpus. The byte
    /// budget is the only ceiling; we deliberately don't cap entry
    /// count.
    pub const DEFAULT_MAX_BYTES: u64 = 500 * 1024 * 1024;
}

impl Default for AnalysisCacheConfig {
    fn default() -> Self {
        Self {
            max_bytes: Self::DEFAULT_MAX_BYTES,
            prune_on_drop: false,
        }
    }
}

/// Persistent file-backed cache for [`ReplayAnalysis`] keyed on the
/// .slp file's content hash. See module docs for policy + use-case.
pub struct AnalysisCache {
    file: FileCache,
    classifier_config: CombatV2Config,
}

impl AnalysisCache {
    /// Open (or create) an analysis cache rooted at `root`. The active
    /// `classifier_config` is the *current* `CombatV2Config` — it's
    /// what we'll write into new entries and what we'll compare
    /// against on lookup to decide if a cached entry is still fresh.
    pub fn open(
        root: PathBuf,
        config: AnalysisCacheConfig,
        classifier_config: CombatV2Config,
    ) -> Result<Self> {
        let file = FileCache::open(
            root,
            FileCacheConfig {
                max_bytes: config.max_bytes,
                max_entries: None,
                prune_on_drop: config.prune_on_drop,
            },
        )?;
        Ok(Self {
            file,
            classifier_config,
        })
    }

    /// Look up the analysis for `key`. Returns `None` if missing OR
    /// stale (version drift, classifier-config drift). Stale entries
    /// are NOT auto-evicted here — they get overwritten on the next
    /// `put` call, which is the natural recovery path.
    pub fn get(&self, key: &str) -> Option<ReplayAnalysis> {
        let path = self.file.lookup(key)?;
        let bytes = std::fs::read(&path).ok()?;
        let cached: CachedAnalysis = bincode::deserialize(&bytes).ok()?;
        if cached.is_fresh(&self.classifier_config) {
            Some(cached.analysis)
        } else {
            None
        }
    }

    /// Persist `analysis` under `key`. Writes through a `.tmp`
    /// staging file + atomic rename so a crash mid-write can't leave
    /// a half-baked entry the next `get` would deserialize into
    /// nonsense. Triggers eviction on success.
    pub fn put(&mut self, key: &str, analysis: &ReplayAnalysis) -> Result<()> {
        let cached = CachedAnalysis::new(analysis.clone(), &self.classifier_config);
        let bytes = bincode::serialize(&cached)
            .map_err(|e| anyhow!("serialize CachedAnalysis: {e}"))?;

        let final_path = self.file.path_for_write(key);
        // Sibling `.tmp` so the rename below is atomic on POSIX
        // (same dir, rename(2) syscall). On Windows rename across the
        // same dir is also atomic if the target doesn't exist; for
        // overwrites we fall back to a delete-then-rename which is
        // still safe under the single-writer contract.
        let tmp_path = final_path.with_extension("tmp");
        std::fs::write(&tmp_path, &bytes)
            .map_err(|e| anyhow!("write {}: {e}", tmp_path.display()))?;
        std::fs::rename(&tmp_path, &final_path).or_else(|_| {
            // Overwrite path: if the rename failed because the target
            // exists on a platform that doesn't allow atomic
            // overwrites, remove + rename.
            let _ = std::fs::remove_file(&final_path);
            std::fs::rename(&tmp_path, &final_path)
        })
        .map_err(|e| {
            anyhow!(
                "rename {} -> {}: {e}",
                tmp_path.display(),
                final_path.display()
            )
        })?;

        self.file.finalize(key)?;
        Ok(())
    }

    /// Convenience: try the cache, and on a miss run `compute`,
    /// persist its result, and return it. The thread of "best
    /// effort: don't let cache failures block the viewer" lives in
    /// the caller — both the cache lookup and the put-after-compute
    /// can be ignored without affecting correctness, since the
    /// computed analysis is the source of truth.
    pub fn get_or_insert_with<F>(
        &mut self,
        key: &str,
        compute: F,
    ) -> Result<ReplayAnalysis>
    where
        F: FnOnce() -> Result<ReplayAnalysis>,
    {
        if let Some(hit) = self.get(key) {
            return Ok(hit);
        }
        let fresh = compute()?;
        // Best-effort persist — a write failure (full disk, perms)
        // shouldn't bubble up and break the viewer. Caller still
        // gets the correct analysis; the next view will retry the
        // write.
        if let Err(e) = self.put(key, &fresh) {
            eprintln!("analysis-cache: failed to write {key}: {e}");
        }
        Ok(fresh)
    }

    /// How many bytes the cache currently holds. Useful for a
    /// future "Settings → cache usage" widget.
    pub fn total_bytes(&self) -> Result<u64> {
        self.file.total_bytes()
    }

    /// Wipe every cache entry. Exposed so the app's "Clear cache"
    /// affordance has a one-line hook.
    pub fn clear(&mut self) -> Result<usize> {
        self.file.clear()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::combat::CombatState;

    fn fresh(prune: bool) -> (tempfile::TempDir, AnalysisCache) {
        let dir = tempfile::tempdir().expect("tempdir");
        let cache = AnalysisCache::open(
            dir.path().to_path_buf(),
            AnalysisCacheConfig {
                max_bytes: 10_000_000,
                prune_on_drop: prune,
            },
            CombatV2Config::default(),
        )
        .expect("open");
        (dir, cache)
    }

    fn sample_analysis() -> ReplayAnalysis {
        ReplayAnalysis {
            combat: vec![
                CombatState::Neutral,
                CombatState::AdvP1,
                CombatState::Trade,
                CombatState::AdvP2,
                CombatState::Neutral,
            ],
            p1_port_idx: 0,
            p2_port_idx: 2,
        }
    }

    #[test]
    fn put_then_get_round_trips() {
        let (_dir, mut cache) = fresh(true);
        let analysis = sample_analysis();
        cache.put("hash-a", &analysis).expect("put");
        let got = cache.get("hash-a").expect("hit");
        assert_eq!(got.combat, analysis.combat);
        assert_eq!(got.p1_port_idx, analysis.p1_port_idx);
        assert_eq!(got.p2_port_idx, analysis.p2_port_idx);
    }

    #[test]
    fn get_returns_none_for_missing_key() {
        let (_dir, cache) = fresh(true);
        assert!(cache.get("absent").is_none());
    }

    #[test]
    fn classifier_config_drift_invalidates_entries() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().to_path_buf();
        let analysis = sample_analysis();

        // Write with default config.
        {
            let mut cache = AnalysisCache::open(
                root.clone(),
                AnalysisCacheConfig {
                    max_bytes: 10_000_000,
                    prune_on_drop: false,
                },
                CombatV2Config::default(),
            )
            .expect("open writer");
            cache.put("the-key", &analysis).expect("put");
            assert!(cache.get("the-key").is_some(), "self-read should hit");
        }

        // Reopen with a *different* classifier config — entry must
        // read as missing despite still being on disk.
        let alt_config = CombatV2Config {
            hitstun_tail_frames: CombatV2Config::default().hitstun_tail_frames + 7,
        };
        let cache = AnalysisCache::open(
            root,
            AnalysisCacheConfig {
                max_bytes: 10_000_000,
                prune_on_drop: false,
            },
            alt_config,
        )
        .expect("open reader");
        assert!(
            cache.get("the-key").is_none(),
            "stale entry should miss under different classifier config"
        );
    }

    #[test]
    fn get_or_insert_with_runs_compute_only_on_miss() {
        let (_dir, mut cache) = fresh(true);
        let analysis = sample_analysis();

        let mut compute_calls = 0;
        let first = cache
            .get_or_insert_with("k", || {
                compute_calls += 1;
                Ok(analysis.clone())
            })
            .expect("first call");
        assert_eq!(compute_calls, 1);
        assert_eq!(first.combat, analysis.combat);

        let second = cache
            .get_or_insert_with("k", || {
                compute_calls += 1;
                panic!("compute should NOT run on cache hit");
            })
            .expect("second call");
        assert_eq!(compute_calls, 1);
        assert_eq!(second.combat, analysis.combat);
    }

    #[test]
    fn put_overwrites_existing_entry() {
        let (_dir, mut cache) = fresh(true);
        let mut analysis = sample_analysis();
        cache.put("k", &analysis).expect("put 1");

        analysis.combat.push(CombatState::AdvP1);
        cache.put("k", &analysis).expect("put 2 (overwrite)");

        let got = cache.get("k").expect("hit");
        assert_eq!(got.combat.len(), analysis.combat.len());
        assert_eq!(got.combat, analysis.combat);
    }
}
