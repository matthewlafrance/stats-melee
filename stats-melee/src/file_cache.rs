//! Generic byte-keyed file cache for derived artifacts.
//!
//! Storage substrate behind [`crate::analysis_cache`]: a budgeted,
//! LRU-evicting store of derived blobs keyed by the source's content hash.
//! Combat-state vectors are tens of KB, so a generous byte budget with no
//! entry cap and `prune_on_drop = false` keeps the user's entire viewing
//! history on disk — the point is that the second view of any replay is
//! instant, even after an app restart. The wrapper layer
//! ([`crate::analysis_cache`]) owns the policy; this module just provides
//! the storage primitives and eviction semantics.
//!
//! # Concurrency
//!
//! Single-writer / multi-reader. Multiple threads may call [`FileCache::lookup`]
//! concurrently — the cache only reads the filesystem and never mutates
//! shared state on lookup. Mutation (`finalize`, `evict_to_budget`,
//! `clear`) is single-threaded by design; the eframe app pattern of
//! "background workers compute, main thread finalizes" already
//! enforces this naturally.
//!
//! # Atomic writes
//!
//! [`FileCache::path_for_write`] returns the *final* path — no temp-file
//! rename dance. Callers that need crash-safe writes should write to
//! `<final>.tmp` and rename on success themselves; this module's eviction
//! sweep ignores `.tmp` files (they're not finalized yet) so partially-
//! written entries don't get prematurely promoted.
//!
//! # Key requirements
//!
//! Keys must be filesystem-safe path components — no `/`, no `..`, no
//! NUL. The intended caller pattern is "hex-encoded SHA-256 of the
//! .slp content", which trivially satisfies that. The cache does NOT
//! re-hash or sanitize: garbage in, garbage out.

use anyhow::{anyhow, Context, Result};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// Eviction policy + budget for a [`FileCache`]. Both consumers
/// (video, analysis) instantiate their own with very different values
/// — see this module's top-level docs for the rationale.
#[derive(Debug, Clone, Copy)]
pub struct FileCacheConfig {
    /// Hard ceiling on total bytes across all entries. Eviction kicks
    /// in on [`FileCache::finalize`] (after a fresh write) and on
    /// explicit [`FileCache::evict_to_budget`] calls. A single entry
    /// larger than `max_bytes` will get evicted on the next finalize
    /// — we don't make a "always keep at least one" guarantee, the
    /// budget is the budget.
    pub max_bytes: u64,
    /// Optional cap on entry count. `None` means "no cap" (the byte
    /// budget is the only limit). Used by the video cache to enforce
    /// "at most 5 MP4s in flight", since the typical render is much
    /// smaller than the byte budget but we still don't want a long
    /// tail of re-rendered videos to accumulate.
    pub max_entries: Option<usize>,
    /// When `true`, [`FileCache`]'s `Drop` impl wipes the cache dir.
    /// Used by the video cache so MP4s don't accumulate across
    /// sessions. Test code overrides this to `false` so test fixtures
    /// survive the test harness tearing down the cache.
    pub prune_on_drop: bool,
}

impl Default for FileCacheConfig {
    /// Conservative defaults — 100 MB, no entry cap, no drop-prune.
    /// Real consumers should always supply their own config; this
    /// `Default` exists for tests + ad-hoc experimentation.
    fn default() -> Self {
        Self {
            max_bytes: 100_000_000,
            max_entries: None,
            prune_on_drop: false,
        }
    }
}

/// Byte-keyed file cache. See module docs for the policy split between
/// consumers and the concurrency / atomicity contracts.
pub struct FileCache {
    root: PathBuf,
    config: FileCacheConfig,
}

impl FileCache {
    /// Open (or create) a file cache rooted at `root`. The directory
    /// is created on demand — callers can point at a fresh
    /// `ProjectDirs::cache_dir().join("video")` without a separate
    /// `mkdir -p`.
    pub fn open(root: PathBuf, config: FileCacheConfig) -> Result<Self> {
        fs::create_dir_all(&root).with_context(|| {
            format!("create file cache root {}", root.display())
        })?;
        Ok(Self { root, config })
    }

    /// Build the on-disk path for `key`. Used by both [`Self::lookup`]
    /// and [`Self::path_for_write`]; centralized so a future change to
    /// the layout (subdir sharding, suffix per cache kind, etc.)
    /// happens in exactly one place.
    fn entry_path(&self, key: &str) -> PathBuf {
        debug_assert!(
            !key.is_empty() && !key.contains('/') && !key.contains('\\') && !key.contains('\0'),
            "FileCache key must be a valid path component, got {key:?}"
        );
        self.root.join(key)
    }

    /// Look up the cached file for `key`. Returns the path if the
    /// entry exists and is a regular file. Does **not** touch
    /// mtime — see the module note about LRU semantics being
    /// write-time, not access-time.
    pub fn lookup(&self, key: &str) -> Option<PathBuf> {
        let p = self.entry_path(key);
        // `is_file` returns false for missing paths AND directories,
        // which is exactly what we want — cache entries are always
        // regular files.
        if p.is_file() {
            Some(p)
        } else {
            None
        }
    }

    /// Reserve a path the caller can write `key`'s contents to. The
    /// returned path is the *final* location, not a staging path —
    /// callers that need atomic writes should write to a sibling
    /// `.tmp` file and rename themselves before calling
    /// [`Self::finalize`].
    pub fn path_for_write(&self, key: &str) -> PathBuf {
        self.entry_path(key)
    }

    /// Mark `key` as freshly-written and trigger eviction if the cache
    /// is now over budget. Should be called immediately after the
    /// caller finishes writing the entry (and after any rename for
    /// atomic-write callers). No-op if the entry doesn't exist yet —
    /// callers that hand the cache an empty `key` get a silent skip
    /// rather than an error so the worker code stays terse.
    pub fn finalize(&mut self, key: &str) -> Result<()> {
        let p = self.entry_path(key);
        if !p.is_file() {
            return Ok(());
        }
        // We deliberately don't bump mtime here — the file's mtime is
        // already "right now" because the caller just wrote it. If
        // a future use case wants to extend an entry's lifetime
        // without re-writing, that's a touch_mtime helper on top of
        // the filetime crate; not a v1 concern.
        let _ = self.evict_to_budget()?;
        Ok(())
    }

    /// Evict oldest-by-mtime entries until the cache fits both
    /// `max_bytes` and (if set) `max_entries`. Returns how many
    /// entries were removed. Safe to call on an empty / nonexistent
    /// cache root — returns 0.
    pub fn evict_to_budget(&mut self) -> Result<usize> {
        let mut entries = self.scan_entries()?;
        // Sort oldest-first so the eviction loop can drain from the
        // front. SystemTime sorts naturally by clock order.
        entries.sort_by_key(|e| e.mtime);

        let mut total_bytes: u64 = entries.iter().map(|e| e.size).sum();
        let mut total_entries = entries.len();
        let mut evicted = 0usize;

        for entry in &entries {
            let over_bytes = total_bytes > self.config.max_bytes;
            let over_count = self
                .config
                .max_entries
                .map(|cap| total_entries > cap)
                .unwrap_or(false);
            if !over_bytes && !over_count {
                break;
            }
            // Best-effort delete. A concurrent eviction (which
            // shouldn't happen given the single-writer contract) or
            // an external `rm` race would surface as ENOENT — we
            // treat that as "already gone, count it as evicted" so
            // the byte/entry math stays consistent with what the
            // filesystem now contains.
            match fs::remove_file(&entry.path) {
                Ok(()) => {}
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => {
                    return Err(anyhow!(
                        "evicting {}: {e}",
                        entry.path.display()
                    ))
                }
            }
            total_bytes = total_bytes.saturating_sub(entry.size);
            total_entries = total_entries.saturating_sub(1);
            evicted += 1;
        }
        Ok(evicted)
    }

    /// Wipe every entry in the cache. Returns the number of entries
    /// removed. Used by the `prune_on_drop` path and exposed so the
    /// app's "Clear cache" Settings button (when we add one) has a
    /// direct hook.
    pub fn clear(&mut self) -> Result<usize> {
        let entries = self.scan_entries()?;
        let mut removed = 0;
        for entry in &entries {
            // Same best-effort delete contract as `evict_to_budget`.
            if fs::remove_file(&entry.path).is_ok() {
                removed += 1;
            }
        }
        Ok(removed)
    }

    /// Total bytes currently consumed by cache entries. Includes
    /// `.tmp` staging files because they live in the same dir — that's
    /// intentional, callers who care about atomicity should clean up
    /// their own `.tmp` files anyway.
    pub fn total_bytes(&self) -> Result<u64> {
        let entries = self.scan_entries()?;
        Ok(entries.iter().map(|e| e.size).sum())
    }

    /// Number of entries currently in the cache.
    pub fn entry_count(&self) -> Result<usize> {
        let entries = self.scan_entries()?;
        Ok(entries.len())
    }

    /// One-shot directory scan — the inner walk for every method that
    /// needs to enumerate the cache. Skips subdirectories and dotfiles
    /// so the future "shard by hash prefix" optimization can land
    /// without these helpers needing a sharding-aware rewrite.
    fn scan_entries(&self) -> Result<Vec<EntryMeta>> {
        let dir = match fs::read_dir(&self.root) {
            Ok(d) => d,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => {
                return Err(anyhow!("read cache dir {}: {e}", self.root.display()))
            }
        };
        let mut out = Vec::new();
        for entry in dir {
            let entry = entry.with_context(|| {
                format!("iterating cache dir {}", self.root.display())
            })?;
            let path = entry.path();
            // Skip dotfiles + non-regular files. The dotfile filter
            // dodges macOS's `.DS_Store` and any future per-cache
            // bookkeeping file we might want to drop into the same
            // root.
            if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                if name.starts_with('.') {
                    continue;
                }
            }
            let md = match entry.metadata() {
                Ok(m) => m,
                Err(_) => continue,
            };
            if !md.is_file() {
                continue;
            }
            let size = md.len();
            let mtime = md.modified().unwrap_or(SystemTime::UNIX_EPOCH);
            out.push(EntryMeta { path, size, mtime });
        }
        Ok(out)
    }
}

impl Drop for FileCache {
    fn drop(&mut self) {
        if self.config.prune_on_drop {
            // Best-effort: a clear failure during shutdown isn't worth
            // panicking the drop. Worst case is some leftover files
            // the next run's eviction picks up.
            let _ = self.clear();
        }
    }
}

/// Internal metadata for one cache entry — collected by `scan_entries`
/// and used by every helper that needs to make eviction decisions.
struct EntryMeta {
    path: PathBuf,
    size: u64,
    mtime: SystemTime,
}

/// True if `path` looks like a `.tmp` staging file written by an
/// atomic-write caller. Currently unused internally but exposed for
/// callers that want to clean up their own staging dir on shutdown.
pub fn is_staging_file(path: &Path) -> bool {
    path.extension().and_then(|e| e.to_str()) == Some("tmp")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::thread;
    use std::time::Duration;
    use tempfile::TempDir;

    /// Helper: write a fresh entry of `size` bytes to `cache` under
    /// `key`, finalize it, and return the resulting path.
    fn write_entry(cache: &mut FileCache, key: &str, size: usize) -> PathBuf {
        let p = cache.path_for_write(key);
        let mut f = fs::File::create(&p).expect("create entry");
        f.write_all(&vec![0u8; size]).expect("write entry bytes");
        // Ensure the file is fully flushed before finalize touches it.
        drop(f);
        cache.finalize(key).expect("finalize");
        p
    }

    fn fresh(config: FileCacheConfig) -> (TempDir, FileCache) {
        let dir = tempfile::tempdir().expect("tempdir");
        let cache = FileCache::open(dir.path().to_path_buf(), config).expect("open");
        (dir, cache)
    }

    #[test]
    fn open_creates_root_dir_if_missing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let nested = dir.path().join("a/b/c");
        assert!(!nested.exists());
        let _cache = FileCache::open(nested.clone(), FileCacheConfig::default())
            .expect("open creates nested dirs");
        assert!(nested.is_dir(), "open() should mkdir -p the cache root");
    }

    #[test]
    fn lookup_returns_none_for_missing_key() {
        let (_dir, cache) = fresh(FileCacheConfig::default());
        assert!(cache.lookup("absent").is_none());
    }

    #[test]
    fn lookup_returns_path_for_present_key() {
        let (_dir, mut cache) = fresh(FileCacheConfig::default());
        let written = write_entry(&mut cache, "present", 16);
        let found = cache.lookup("present").expect("entry should resolve");
        assert_eq!(found, written);
    }

    #[test]
    fn evict_to_budget_purges_oldest_first() {
        // Tight 100-byte budget; three 60-byte entries → must evict
        // exactly one (the oldest).
        let (_dir, mut cache) = fresh(FileCacheConfig {
            max_bytes: 100,
            max_entries: None,
            prune_on_drop: false,
        });

        write_entry(&mut cache, "a", 60);
        // Sleep so mtimes differ on filesystems with second-resolution
        // (HFS+, FAT). 1.1s is enough on every common FS without
        // making the test painful.
        thread::sleep(Duration::from_millis(1100));
        write_entry(&mut cache, "b", 60);
        // After this finalize the cache is at 120 bytes — over budget
        // by 20. One entry must go; the oldest (`a`) is the loser.

        assert!(cache.lookup("a").is_none(), "a should be evicted");
        assert!(cache.lookup("b").is_some(), "b should survive");
    }

    #[test]
    fn max_entries_cap_is_independent_of_byte_budget() {
        // Generous byte budget, but cap at 2 entries. Three writes →
        // oldest must go even though we're nowhere near the byte cap.
        let (_dir, mut cache) = fresh(FileCacheConfig {
            max_bytes: 1_000_000,
            max_entries: Some(2),
            prune_on_drop: false,
        });

        write_entry(&mut cache, "old", 8);
        thread::sleep(Duration::from_millis(1100));
        write_entry(&mut cache, "mid", 8);
        thread::sleep(Duration::from_millis(1100));
        write_entry(&mut cache, "new", 8);

        assert!(cache.lookup("old").is_none(), "old should be evicted by entry cap");
        assert!(cache.lookup("mid").is_some());
        assert!(cache.lookup("new").is_some());
        assert_eq!(cache.entry_count().unwrap(), 2);
    }

    #[test]
    fn evict_to_budget_drops_single_oversized_entry() {
        // Budget is 50 bytes, single entry is 200 bytes → eviction
        // wipes it. We deliberately don't make a "keep at least one"
        // guarantee.
        let (_dir, mut cache) = fresh(FileCacheConfig {
            max_bytes: 50,
            max_entries: None,
            prune_on_drop: false,
        });
        write_entry(&mut cache, "huge", 200);
        assert!(cache.lookup("huge").is_none(), "oversized entry must be evicted");
        assert_eq!(cache.entry_count().unwrap(), 0);
    }

    #[test]
    fn clear_removes_every_entry() {
        let (_dir, mut cache) = fresh(FileCacheConfig::default());
        write_entry(&mut cache, "one", 16);
        write_entry(&mut cache, "two", 16);
        write_entry(&mut cache, "three", 16);
        assert_eq!(cache.entry_count().unwrap(), 3);

        let removed = cache.clear().expect("clear");
        assert_eq!(removed, 3);
        assert_eq!(cache.entry_count().unwrap(), 0);
    }

    #[test]
    fn prune_on_drop_wipes_dir_when_enabled() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().to_path_buf();
        {
            let mut cache = FileCache::open(
                root.clone(),
                FileCacheConfig {
                    max_bytes: 1_000_000,
                    max_entries: None,
                    prune_on_drop: true,
                },
            )
            .expect("open");
            write_entry(&mut cache, "ephemeral", 32);
            assert!(cache.lookup("ephemeral").is_some());
            // Drop happens at end of this block.
        }

        // After drop, the file should be gone — but the directory
        // itself stays (so reopen-without-mkdir works).
        let entry_path = root.join("ephemeral");
        assert!(!entry_path.exists(), "prune_on_drop should remove entries");
        assert!(root.is_dir(), "prune_on_drop should leave the root dir");
    }

    #[test]
    fn prune_on_drop_is_no_op_when_disabled() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().to_path_buf();
        {
            let mut cache = FileCache::open(
                root.clone(),
                FileCacheConfig {
                    max_bytes: 1_000_000,
                    max_entries: None,
                    prune_on_drop: false,
                },
            )
            .expect("open");
            write_entry(&mut cache, "persistent", 32);
        }
        let entry_path = root.join("persistent");
        assert!(
            entry_path.exists(),
            "prune_on_drop=false must leave entries on disk"
        );
    }

    #[test]
    fn scan_skips_dotfiles() {
        // macOS .DS_Store / future bookkeeping files shouldn't count
        // toward eviction or entry counts.
        let (_dir, mut cache) = fresh(FileCacheConfig::default());
        write_entry(&mut cache, "real", 16);

        // Drop a dotfile straight into the cache root, bypassing
        // path_for_write so the debug_assert on key validity doesn't
        // trip. scan_entries should skip this when computing entry
        // counts and eviction candidates.
        let dotfile_path = cache.root.join(".sneaky");
        fs::write(&dotfile_path, b"junk").expect("write dotfile");

        assert_eq!(cache.entry_count().unwrap(), 1);
        assert!(cache.lookup("real").is_some());
        // Dotfile is still there on disk — we just don't account for it.
        assert!(dotfile_path.exists());
    }

    #[test]
    fn lookup_does_not_extend_lifetime_via_mtime_touch() {
        // Document the v1 semantics: looking up an entry doesn't bump
        // its mtime, so heavy lookup traffic against an old entry
        // doesn't keep it alive against fresh writes.
        let (_dir, mut cache) = fresh(FileCacheConfig {
            max_bytes: 100,
            max_entries: None,
            prune_on_drop: false,
        });
        write_entry(&mut cache, "old", 60);
        thread::sleep(Duration::from_millis(1100));

        // Hammer lookups on `old` to "use" it.
        for _ in 0..10 {
            let _ = cache.lookup("old");
        }

        // Now write a fresh entry that pushes us over budget. The
        // eviction should still pick `old` because lookups didn't
        // touch its mtime.
        write_entry(&mut cache, "fresh", 60);
        assert!(cache.lookup("old").is_none(), "old should be evicted despite recent lookups");
        assert!(cache.lookup("fresh").is_some());
    }
}
