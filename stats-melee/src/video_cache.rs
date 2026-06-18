//! Aggressive file-backed cache for rendered MP4s.
//!
//! Wraps the generic [`crate::file_cache::FileCache`] with the
//! video-specific policy: tiny entry cap, multi-GB byte budget, and
//! `prune_on_drop = true` so the app's shutdown handler wipes the dir.
//! The contrast with [`crate::analysis_cache`] is deliberate — see
//! that module's docs for the persistent-cache policy on the other end
//! of the spectrum.
//!
//! # Why aggressive?
//!
//! MP4s are big — a 5-minute replay at 25 Mbps lands around 950 MB. They
//! also rarely get re-watched in the same session: the user picks a
//! replay, watches it once, moves on. Persisting them across restarts
//! would burn gigabytes of disk for nothing. The defaults below keep
//! the most recent handful around for back-and-forth navigation
//! within a session and let the OS reclaim the space cleanly on app
//! exit.
//!
//! # Layout
//!
//! Entries live as `<root>/<slp-hash>.mp4`. The `.mp4` suffix is added
//! by this wrapper — callers pass the raw content hash as the key.
//! `path_for_write` returns the `.mp4` path the render worker should
//! aim its ffmpeg invocation at.

use std::path::PathBuf;

use anyhow::Result;

use crate::file_cache::{FileCache, FileCacheConfig};

/// Tunable knobs for the video cache. `Default` matches the production
/// policy described in the module docs.
#[derive(Debug, Clone, Copy)]
pub struct VideoCacheConfig {
    /// Hard ceiling on total bytes. Set well above any realistic single
    /// MP4 so the [`Self::max_entries`] cap is what's *usually*
    /// triggering eviction; `max_bytes` is the safety net for the
    /// "tournament-set replay rendered at high bitrate" outlier.
    pub max_bytes: u64,
    /// How many MP4s to retain at most. The whole point of the video
    /// cache is "the last few you rendered are still here for
    /// back-and-forth"; the value is intentionally small so disk
    /// pressure from this cache stays bounded.
    pub max_entries: usize,
    /// Wipe the cache dir when the [`VideoCache`] is dropped. The
    /// production app sets this to `true` (the eframe shutdown handler
    /// triggers it); tests usually set it to `false` so test fixtures
    /// can be inspected.
    pub prune_on_drop: bool,
}

impl VideoCacheConfig {
    /// 2 GB. Big enough that a handful of MP4s fits comfortably even
    /// at high bitrates; small enough that even a leak would cap out
    /// at a few percent of a typical SSD.
    pub const DEFAULT_MAX_BYTES: u64 = 2 * 1024 * 1024 * 1024;
    /// 5 entries — enough for the user to go A → B → A within a
    /// session without re-rendering, but not enough for the cache to
    /// become a long-term storage solution.
    pub const DEFAULT_MAX_ENTRIES: usize = 5;
}

impl Default for VideoCacheConfig {
    fn default() -> Self {
        Self {
            max_bytes: Self::DEFAULT_MAX_BYTES,
            max_entries: Self::DEFAULT_MAX_ENTRIES,
            prune_on_drop: true,
        }
    }
}

/// File-suffix appended to keys so the on-disk layout is
/// `<root>/<hash>.mp4` (a regular MP4 the OS player can open
/// directly with the default association).
const MP4_EXT: &str = ".mp4";

/// Aggressive file-backed cache for rendered replay MP4s. See module
/// docs for the policy contrast against the analysis cache.
pub struct VideoCache {
    file: FileCache,
}

impl VideoCache {
    /// Open (or create) a video cache rooted at `root`.
    pub fn open(root: PathBuf, config: VideoCacheConfig) -> Result<Self> {
        let file = FileCache::open(
            root,
            FileCacheConfig {
                max_bytes: config.max_bytes,
                max_entries: Some(config.max_entries),
                prune_on_drop: config.prune_on_drop,
            },
        )?;
        Ok(Self { file })
    }

    /// Look up the MP4 path for `slp_hash`. Returns `None` if not
    /// rendered (or if the entry was evicted in a previous session
    /// and the same hash hasn't been re-rendered since).
    pub fn lookup(&self, slp_hash: &str) -> Option<PathBuf> {
        self.file.lookup(&with_mp4(slp_hash))
    }

    /// Reserve the on-disk path the render worker should target. The
    /// returned path has the `.mp4` suffix already attached. After
    /// the worker finishes writing (atomically — see the staging
    /// pattern in [`crate::file_cache`]), it should call
    /// [`Self::finalize`] to trigger eviction.
    pub fn path_for_write(&self, slp_hash: &str) -> PathBuf {
        self.file.path_for_write(&with_mp4(slp_hash))
    }

    /// Mark `slp_hash`'s MP4 as freshly-rendered and trigger eviction
    /// if the cache is now over budget.
    pub fn finalize(&mut self, slp_hash: &str) -> Result<()> {
        self.file.finalize(&with_mp4(slp_hash))
    }

    /// Wipe every cache entry. Used by the app's "Clear video cache"
    /// affordance when we add one, and by `Drop` indirectly via
    /// `prune_on_drop`.
    pub fn clear(&mut self) -> Result<usize> {
        self.file.clear()
    }

    /// Total bytes currently held by the video cache. Useful for a
    /// future "Settings → cache usage" widget that surfaces both
    /// caches side by side.
    pub fn total_bytes(&self) -> Result<u64> {
        self.file.total_bytes()
    }
}

/// Append the `.mp4` suffix the on-disk layout expects. Centralized so
/// every method agrees on the same naming scheme — if we ever switch
/// to `.webm` or per-key subdirs the change happens here.
#[inline]
fn with_mp4(key: &str) -> String {
    format!("{key}{MP4_EXT}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::io::Write as _;
    use std::thread;
    use std::time::Duration;

    fn fresh(prune_on_drop: bool) -> (tempfile::TempDir, VideoCache) {
        let dir = tempfile::tempdir().expect("tempdir");
        let cache = VideoCache::open(
            dir.path().to_path_buf(),
            VideoCacheConfig {
                max_bytes: 1_000_000,
                max_entries: 3,
                prune_on_drop,
            },
        )
        .expect("open");
        (dir, cache)
    }

    /// Helper: stand in for the render worker — write an MP4-shaped
    /// blob of `size` bytes under `slp_hash`, then finalize.
    fn write_video(cache: &mut VideoCache, slp_hash: &str, size: usize) -> PathBuf {
        let p = cache.path_for_write(slp_hash);
        let mut f = fs::File::create(&p).expect("create video entry");
        f.write_all(&vec![0xFAu8; size]).expect("write");
        drop(f);
        cache.finalize(slp_hash).expect("finalize");
        p
    }

    #[test]
    fn entries_have_mp4_suffix_on_disk() {
        let (dir, mut cache) = fresh(false);
        let p = write_video(&mut cache, "abc123", 64);
        assert!(
            p.extension().and_then(|e| e.to_str()) == Some("mp4"),
            "video entries must land with the .mp4 extension on disk"
        );
        // Sanity: the file actually lives inside the cache root.
        assert!(p.starts_with(dir.path()));
    }

    #[test]
    fn lookup_keys_on_raw_hash_not_filename() {
        let (_dir, mut cache) = fresh(false);
        write_video(&mut cache, "abc123", 64);
        // Caller hands us the bare hash — wrapper handles the suffix.
        assert!(cache.lookup("abc123").is_some());
        // The full filename is not the lookup key.
        assert!(cache.lookup("abc123.mp4").is_none());
    }

    #[test]
    fn entry_cap_evicts_oldest() {
        let (_dir, mut cache) = fresh(false);
        write_video(&mut cache, "a", 64);
        thread::sleep(Duration::from_millis(1100));
        write_video(&mut cache, "b", 64);
        thread::sleep(Duration::from_millis(1100));
        write_video(&mut cache, "c", 64);
        thread::sleep(Duration::from_millis(1100));
        // Now over the 3-entry cap.
        write_video(&mut cache, "d", 64);

        assert!(cache.lookup("a").is_none(), "oldest must be evicted");
        assert!(cache.lookup("b").is_some());
        assert!(cache.lookup("c").is_some());
        assert!(cache.lookup("d").is_some());
    }

    #[test]
    fn prune_on_drop_wipes_dir() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().to_path_buf();
        {
            let mut cache = VideoCache::open(
                root.clone(),
                VideoCacheConfig {
                    max_bytes: 1_000_000,
                    max_entries: 5,
                    prune_on_drop: true,
                },
            )
            .expect("open");
            write_video(&mut cache, "ephemeral", 32);
            assert!(cache.lookup("ephemeral").is_some());
            // Drop happens at end of this block.
        }
        assert!(
            !root.join("ephemeral.mp4").exists(),
            "prune_on_drop should remove .mp4 entries on close"
        );
        assert!(root.is_dir(), "root dir should survive");
    }

    #[test]
    fn default_config_matches_documented_policy() {
        // Guard the public defaults — these are referenced in TODO.txt
        // and the module docs, so a silent change should fail this
        // test.
        let cfg = VideoCacheConfig::default();
        assert_eq!(cfg.max_bytes, 2 * 1024 * 1024 * 1024);
        assert_eq!(cfg.max_entries, 5);
        assert!(cfg.prune_on_drop);
    }
}
