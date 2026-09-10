//! Reading a `.slp` file: parse, hash, analyze.
//!
//! Nothing here touches the database. These are the pure file-to-data entry
//! points that ingestion, the replay viewer, and the tests all share.

use anyhow::{anyhow, Result};
use peppi::io::slippi;
use std::path::Path;
use std::{fs, io};

use crate::combat;
use crate::gamedata::GameData;

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
        let _ = write!(hex, "{b:02x}");
    }
    Ok(hex)
}

/// Parse a .slp file and produce a full [`combat::ReplayAnalysis`] —
/// combat states + per-frame character positions + port indices — in
/// one read of the file.
///
/// This is the entry point the embedded 2D analysis view uses. The app
/// crate takes no direct dependency on peppi, so exposing a
/// `path -> ReplayAnalysis` helper here keeps that boundary clean.
pub fn parse_replay_analysis<P: AsRef<Path>>(
    path: P,
) -> Result<combat::ReplayAnalysis> {
    let mut r = io::BufReader::new(fs::File::open(path.as_ref())?);
    let game = slippi::read(&mut r, None)?;
    combat::compute_analysis_1v1(&game)
}

#[cfg(test)]
mod hash_tests {
    use super::*;
    use std::io::Write as _;
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
