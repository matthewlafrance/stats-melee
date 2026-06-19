//! Launching replays in the user's local Slippi Dolphin install.
//!
//! We don't link against or embed Slippi — the app shells out to the
//! user's Slippi Dolphin binary using the same protocol the official
//! Slippi Launcher uses: a small JSON "comm file" is written to the OS
//! temp directory, and Slippi is invoked with `-i <comm.json> -b -e <iso>`.
//! The `"mode":"normal"` entry tells Slippi to play back the `.slp`
//! at `replay`; `-b` tells Dolphin to quit when playback ends so we
//! don't leak emulator windows; `-e <iso>` boots the Melee disc image
//! the replay's inputs run against. Without `-e`, a standalone-launched
//! playback Dolphin has no game to boot (it opens but never starts the
//! replay), because — unlike when launched from the Slippi Launcher — its
//! config has no default ISO set.
//!
//! ## Why this matters
//!
//! An earlier revision of this module shelled out via
//! `open -a "Slippi Dolphin" <path>`. That works at the process level,
//! but macOS delivers the `.slp` to Slippi as a generic "open document"
//! Apple Event — and Slippi's open-document handler treats unknown
//! files as *disc images*, popping up a "This does not seem to be a
//! copy of Super Smash Bros. Melee" warning instead of playing the
//! replay. The comm-file path is the only way to reach playback mode
//! from the command line.
//!
//! ## `.app` bundle resolution
//!
//! If `slippi_playback_command` points at a `.app` directory (which is
//! what macOS file pickers return), [`plan_launch`] resolves it to the
//! inner `Contents/MacOS/<Name>` binary. `exec` rejects directories
//! with `EACCES`, so we have to dereference the bundle ourselves.
//!
//! ## Strategy split
//!
//! 1. [`plan_launch`] / [`plan_for_platform`] — pure functions that
//!    take a replay path, an override, and a pre-computed comm-file
//!    path, and return a [`LaunchPlan`] with the exact program + argv.
//!    Unit-testable without spawning or writing to disk.
//! 2. [`launch`] / [`launch_replay`] — thin wrappers over
//!    `Command::spawn` that also write the comm file.
//!
//! ## Platform defaults (no override set)
//!
//! Each platform knows where the Slippi Launcher keeps its bundled `playback`
//! / `netplay` Dolphin builds; the Settings override is the guaranteed
//! fallback for non-standard installs.
//!
//! - macOS: the Launcher's `~/Library/Application Support/Slippi Launcher/`
//!   builds, then `/Applications/Slippi Dolphin.app`, then a Spotlight
//!   (`mdfind`) search.
//! - Windows: `%APPDATA%\Slippi Launcher\{playback,netplay}\Slippi Dolphin.exe`.
//! - Linux: the first `*.AppImage` under
//!   `${XDG_CONFIG_HOME:-~/.config}/Slippi Launcher/{playback,netplay}`.
//!
//! When discovery finds nothing, [`plan_with_default`] returns
//! [`SlippiLaunchError::NoDefaultForPlatform`], prompting the user to set the
//! binary path in Settings.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

/// Errors from trying to launch Slippi. Designed to give the user a
/// specific, actionable message — the UI prints whichever variant we
/// return verbatim.
#[derive(Debug)]
pub enum SlippiLaunchError {
    /// Game row has no `replay_path` (ingested before that column
    /// existed). User should re-ingest.
    NoReplayPath,
    /// The `.slp` file was recorded in the DB but is no longer at the
    /// expected path on disk.
    ReplayFileMissing(PathBuf),
    /// No default Slippi binary for this platform and the user hasn't
    /// set `slippi_playback_command` in Settings.
    NoDefaultForPlatform,
    /// Couldn't write the JSON comm file that tells Slippi which
    /// replay to play. Typically `/tmp` full or read-only.
    CommFileFailed(String),
    /// Spawning the child process failed — usually "file not found"
    /// or a permission error on the configured binary path.
    SpawnFailed(String),
}

impl std::fmt::Display for SlippiLaunchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoReplayPath => write!(
                f,
                "this game was ingested before replay-path tracking — \
                 re-ingest to enable Slippi playback"
            ),
            Self::ReplayFileMissing(p) => {
                write!(f, "replay file not found on disk: {}", p.display())
            }
            Self::NoDefaultForPlatform => write!(
                f,
                "couldn't find Slippi Dolphin automatically — \
                 set the playback binary path in Settings"
            ),
            Self::CommFileFailed(msg) => {
                write!(f, "failed to write Slippi comm file: {msg}")
            }
            Self::SpawnFailed(msg) => write!(f, "failed to launch Slippi: {msg}"),
        }
    }
}

impl std::error::Error for SlippiLaunchError {}

/// A fully-resolved "program + args" pair ready to be handed to
/// `Command::new(...).args(...)`. Exposed so unit tests can assert on
/// the exact argv without actually spawning.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchPlan {
    pub program: String,
    pub args: Vec<String>,
}

/// Build a [`LaunchPlan`] for the current platform. Runs IO discovery
/// (known install dirs + Spotlight) to find Slippi when no override is
/// set. The caller is responsible for having already written the
/// comm-file JSON at `comm_file_path` — see [`launch_replay`].
pub fn plan_launch(
    replay_path: &str,
    comm_file_path: &Path,
    override_cmd: Option<&str>,
    iso_path: Option<&str>,
) -> Result<LaunchPlan, SlippiLaunchError> {
    plan_with_default(
        replay_path,
        comm_file_path,
        override_cmd,
        iso_path,
        find_default_binary_for_current_platform(),
    )
}

/// Which platform-default behavior to use. Split out from the real
/// `cfg!` checks so tests can exercise every branch regardless of
/// where they run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    MacOs,
    Linux,
    Windows,
    Unknown,
}

fn current_platform() -> Platform {
    if cfg!(target_os = "macos") {
        Platform::MacOs
    } else if cfg!(target_os = "linux") {
        Platform::Linux
    } else if cfg!(target_os = "windows") {
        Platform::Windows
    } else {
        Platform::Unknown
    }
}

/// Test-friendly variant of [`plan_launch`] — uses the static
/// [`default_binary_for_platform`] fallback (no IO, no Spotlight) so
/// tests pin down deterministic behavior across platforms. Prefer
/// [`plan_launch`] in production code.
pub fn plan_for_platform(
    replay_path: &str,
    comm_file_path: &Path,
    override_cmd: Option<&str>,
    iso_path: Option<&str>,
    platform: Platform,
) -> Result<LaunchPlan, SlippiLaunchError> {
    plan_with_default(
        replay_path,
        comm_file_path,
        override_cmd,
        iso_path,
        default_binary_for_platform(platform).ok(),
    )
}

/// Pure core of launch planning. `resolved_default` is the
/// already-discovered platform default binary (if any) — the caller
/// decides whether to get it via IO lookup ([`plan_launch`]) or via
/// the static fallback ([`plan_for_platform`]).
fn plan_with_default(
    replay_path: &str,
    comm_file_path: &Path,
    override_cmd: Option<&str>,
    iso_path: Option<&str>,
    resolved_default: Option<PathBuf>,
) -> Result<LaunchPlan, SlippiLaunchError> {
    if replay_path.is_empty() {
        return Err(SlippiLaunchError::NoReplayPath);
    }

    let program = match override_cmd.map(str::trim).filter(|s| !s.is_empty()) {
        Some(cmd) => resolve_override_binary(cmd),
        None => resolved_default.ok_or(SlippiLaunchError::NoDefaultForPlatform)?,
    };

    // `-i <comm>` tells Slippi which replay to play; `-b` quits Dolphin when
    // playback ends. `-e <iso>` boots the Melee disc image — without it the
    // playback build comes up with no game to run (it opens, but the replay
    // never starts), since a standalone-launched Dolphin has no default ISO
    // configured. We only append it when an ISO path is set in Settings;
    // otherwise we fall back to whatever default the user's Dolphin has.
    let mut args = vec![
        "-i".to_string(),
        comm_file_path.display().to_string(),
        "-b".to_string(),
    ];
    if let Some(iso) = iso_path.map(str::trim).filter(|s| !s.is_empty()) {
        args.push("-e".to_string());
        args.push(iso.to_string());
    }

    Ok(LaunchPlan {
        program: program.display().to_string(),
        args,
    })
}

/// If `cmd` ends in `.app`, resolve to its inner
/// `Contents/MacOS/<Name>` binary. Otherwise treat as a direct
/// executable path.
fn resolve_override_binary(cmd: &str) -> PathBuf {
    let path = PathBuf::from(cmd);
    predict_app_inner_binary(&path).unwrap_or(path)
}

/// Given a `.app` bundle path, return the predicted inner-binary
/// location using the `CFBundleExecutable` convention:
/// `Contents/MacOS/<Name>` where `<Name>` is the app's basename with
/// the `.app` suffix stripped. Returns `None` for paths that don't
/// look like `.app` bundles.
///
/// This is a prediction — we don't verify the file exists. `spawn`
/// will surface a missing-file error if the convention doesn't hold
/// for some hypothetical app bundle (Slippi Dolphin does follow it).
pub fn predict_app_inner_binary(path: &Path) -> Option<PathBuf> {
    let name = path.file_name()?.to_str()?;
    let app_name = name.strip_suffix(".app")?;
    Some(path.join("Contents").join("MacOS").join(app_name))
}

/// Static per-platform fallback. Used by [`plan_for_platform`] for
/// unit-test determinism; production uses
/// [`find_default_binary_for_current_platform`] which also stats real
/// install dirs and can call out to Spotlight.
fn default_binary_for_platform(platform: Platform) -> Result<PathBuf, SlippiLaunchError> {
    match platform {
        Platform::MacOs => Ok(PathBuf::from(
            "/Applications/Slippi Dolphin.app/Contents/MacOS/Slippi Dolphin",
        )),
        Platform::Linux | Platform::Windows | Platform::Unknown => {
            Err(SlippiLaunchError::NoDefaultForPlatform)
        }
    }
}

// --- IO-based discovery -----------------------------------------------------

/// Cached result of the default-binary lookup. We only run the disk
/// probes (and, on macOS, the `mdfind` call) once per app lifetime — a
/// new Slippi install won't be picked up without a restart, but that's a
/// fine tradeoff for not shelling out / stat-ing on every click.
static DEFAULT_BINARY: OnceLock<Option<PathBuf>> = OnceLock::new();

/// Runtime default-binary resolution for the current OS. Returns
/// `Some(binary)` if we found a Slippi Dolphin install, `None` otherwise —
/// in which case [`plan_with_default`] surfaces
/// [`SlippiLaunchError::NoDefaultForPlatform`] so the user gets a
/// "set the binary path in Settings" prompt. Each platform knows where the
/// Slippi Launcher keeps its bundled Dolphin builds; the manual override in
/// Settings is always the guaranteed fallback for non-standard installs.
fn find_default_binary_for_current_platform() -> Option<PathBuf> {
    DEFAULT_BINARY
        .get_or_init(|| match current_platform() {
            Platform::MacOs => find_macos_default_binary_uncached(),
            Platform::Windows => find_windows_default_binary_uncached(),
            Platform::Linux => find_linux_default_binary_uncached(),
            Platform::Unknown => None,
        })
        .clone()
}

/// Ordered Windows candidates. The Slippi Launcher installs its Dolphin
/// builds under `%APPDATA%\Slippi Launcher\{playback,netplay}\`, each a
/// `Slippi Dolphin.exe`. Pure — exposed for unit tests.
fn windows_default_candidates() -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Ok(appdata) = std::env::var("APPDATA") {
        let base = PathBuf::from(&appdata).join("Slippi Launcher");
        // `playback` first — the build designed for `-i <comm> -b`. `netplay`
        // is the same Dolphin fork and works as a fallback.
        out.push(base.join("playback").join("Slippi Dolphin.exe"));
        out.push(base.join("netplay").join("Slippi Dolphin.exe"));
    }
    out
}

fn find_windows_default_binary_uncached() -> Option<PathBuf> {
    windows_default_candidates().into_iter().find(|p| p.is_file())
}

/// The Slippi Launcher's Dolphin directories on Linux:
/// `${XDG_CONFIG_HOME:-~/.config}/Slippi Launcher/{playback,netplay}`. Pure —
/// exposed for unit tests.
fn linux_dolphin_dirs() -> Vec<PathBuf> {
    let config = if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME").map(|s| s.trim().to_string()) {
        if xdg.is_empty() {
            None
        } else {
            Some(PathBuf::from(xdg))
        }
    } else {
        None
    }
    .or_else(|| std::env::var("HOME").ok().map(|h| PathBuf::from(h).join(".config")));

    match config {
        Some(c) => {
            let root = c.join("Slippi Launcher");
            vec![root.join("playback"), root.join("netplay")]
        }
        None => Vec::new(),
    }
}

fn find_linux_default_binary_uncached() -> Option<PathBuf> {
    // The Launcher ships Dolphin as an AppImage whose exact filename changes
    // per release, so scan each build dir for the first `*.AppImage`.
    linux_dolphin_dirs()
        .iter()
        .find_map(|dir| first_appimage_in(dir))
}

/// First `*.AppImage` (case-insensitive extension) in `dir`, chosen
/// deterministically (sorted) so repeat launches pick the same file. `None`
/// if the directory is unreadable or holds no AppImage.
fn first_appimage_in(dir: &Path) -> Option<PathBuf> {
    let mut hits: Vec<PathBuf> = std::fs::read_dir(dir)
        .ok()?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            p.extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| e.eq_ignore_ascii_case("AppImage"))
        })
        .collect();
    hits.sort();
    hits.into_iter().next()
}

fn find_macos_default_binary_uncached() -> Option<PathBuf> {
    // 1. Stat the usual install locations in priority order.
    for cand in macos_default_candidates() {
        if let Some(inner) = resolve_existing_app(&cand) {
            return Some(inner);
        }
    }

    // 2. Fall back to Spotlight. `mdfind` lives on every macOS box
    // and returns in <100ms cold; cheap enough to call as a one-time
    // last resort. If the user has no Spotlight index (e.g. on
    // mdutil-disabled volumes), this silently returns None and we'd
    // fall through to NoDefaultForPlatform.
    let output = Command::new("mdfind")
        .arg("kMDItemFSName == 'Slippi Dolphin.app'")
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let app_path = parse_mdfind_output(&stdout)?;
    resolve_existing_app(&app_path)
}

/// Ordered list of `.app` bundles to try on macOS. Pure — exposed so
/// a unit test can assert the priority order without touching disk.
/// `HOME`-derived paths come first because Slippi Launcher's bundled
/// Dolphin install lives there and is overwhelmingly the modern
/// setup; `/Applications/Slippi Dolphin.app` covers the legacy
/// standalone distribution.
fn macos_default_candidates() -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Ok(home) = std::env::var("HOME") {
        let base = PathBuf::from(&home).join("Library/Application Support/Slippi Launcher");
        // `playback` first — it's the build actually designed for the
        // `-i <comm.json> -b` protocol. `netplay` works too (same
        // Dolphin fork) and is the fallback when the launcher hasn't
        // downloaded the playback build yet.
        out.push(base.join("playback/Slippi Dolphin.app"));
        out.push(base.join("netplay/Slippi Dolphin.app"));
    }
    out.push(PathBuf::from("/Applications/Slippi Dolphin.app"));
    out
}

/// Pure: pick the best `.app` path from `mdfind` stdout. Preference:
///
/// 1. Paths containing `/playback/` (Slippi Launcher's replay build).
/// 2. Paths containing `/netplay/` (same launcher, online-play build).
/// 3. Everything else.
///
/// Skips paths under `/Volumes/` — those are almost always DMG
/// installers the user forgot to eject. Dolphin doesn't cope well
/// with being launched from a removable volume (its per-user config
/// lookup breaks), and the binary disappears as soon as the user
/// ejects.
fn parse_mdfind_output(stdout: &str) -> Option<PathBuf> {
    let mut playback: Option<PathBuf> = None;
    let mut netplay: Option<PathBuf> = None;
    let mut other: Option<PathBuf> = None;

    for line in stdout.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with("/Volumes/") {
            continue;
        }
        let path = PathBuf::from(line);
        if line.contains("/playback/") {
            playback.get_or_insert(path);
        } else if line.contains("/netplay/") {
            netplay.get_or_insert(path);
        } else {
            other.get_or_insert(path);
        }
    }

    playback.or(netplay).or(other)
}

/// Resolve an `.app` path to its inner binary and check that the
/// binary actually exists on disk. Returns `None` if the path isn't a
/// `.app` bundle, doesn't follow the `Contents/MacOS/<Name>`
/// convention, or the inner binary is missing.
fn resolve_existing_app(app_path: &Path) -> Option<PathBuf> {
    let inner = predict_app_inner_binary(app_path)?;
    inner.exists().then_some(inner)
}

/// Pure: build the JSON body of the Slippi comm file for `"normal"`
/// playback of a single replay. Shape mirrors what Slippi Launcher
/// writes internally.
pub fn build_comm_json(replay_path: &str) -> String {
    format!(
        r#"{{"mode":"normal","replay":{},"isRealTimeMode":false,"outputOverlayFiles":false}}"#,
        json_string_escape(replay_path),
    )
}

/// Minimal JSON string escape. We only feed this filesystem paths, so
/// we keep the surface small — no surrogate-pair handling, no full
/// ECMA-404 compliance. Covers the control chars + `"` and `\`, which
/// are all that realistically show up in macOS/Linux/Windows paths.
fn json_string_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                out.push_str(&format!("\\u{:04x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Write the comm-file JSON to a uniquely-named file in the OS temp
/// directory and return its path. We leave the file on disk after the
/// call — Dolphin reads it lazily at startup, so we can't delete it
/// up front. The OS cleans up temp files on reboot; fine for a desktop
/// app.
fn write_comm_file(replay_path: &str) -> Result<PathBuf, SlippiLaunchError> {
    let dir = std::env::temp_dir();
    // Nanos timestamp is sufficient for uniqueness — we don't spawn
    // multiple playbacks concurrently from one process.
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let path = dir.join(format!("stats-melee-slippi-{ts}.json"));
    let json = build_comm_json(replay_path);
    std::fs::write(&path, json)
        .map_err(|e| SlippiLaunchError::CommFileFailed(format!("{}: {e}", path.display())))?;
    Ok(path)
}

/// Launch Slippi with the given plan. Spawns a detached child process
/// — we don't wait for it or capture its stdout/stderr.
///
/// On macOS, when the program is an `.app`-bundled binary (the normal case),
/// we route through `open -n -a <bundle> --args <flags>` instead of spawning
/// the inner binary directly. A directly-spawned GUI process opens *behind*
/// our window — the user would have to click the dock icon to bring Dolphin
/// up. `open` goes through LaunchServices, which activates Dolphin to the
/// foreground; `-n` forces a fresh instance and `--args` forwards Dolphin's
/// CLI flags to the executable. A bare-binary override (no bundle) or a
/// non-macOS platform falls back to a direct spawn.
pub fn launch(plan: &LaunchPlan) -> Result<(), SlippiLaunchError> {
    let bundle = if cfg!(target_os = "macos") {
        app_bundle_for_inner_binary(Path::new(&plan.program))
    } else {
        None
    };

    let mut command = match bundle {
        Some(bundle) => {
            let mut c = Command::new("open");
            c.arg("-n").arg("-a").arg(bundle).arg("--args").args(&plan.args);
            c
        }
        None => {
            let mut c = Command::new(&plan.program);
            c.args(&plan.args);
            c
        }
    };

    command
        .spawn()
        .map(|_| ())
        .map_err(|e| SlippiLaunchError::SpawnFailed(e.to_string()))
}

/// Given a resolved inner-binary path that follows the macOS
/// `<Name>.app/Contents/MacOS/<Name>` convention, return the enclosing
/// `.app` bundle so it can be handed to `open -a`. Returns `None` for paths
/// that don't match — e.g. a bare Unix binary a power user pointed the
/// override at, which has no bundle and must be spawned directly.
fn app_bundle_for_inner_binary(program: &Path) -> Option<PathBuf> {
    let macos_dir = program.parent()?; // <bundle>.app/Contents/MacOS
    if macos_dir.file_name()? != "MacOS" {
        return None;
    }
    let contents = macos_dir.parent()?; // <bundle>.app/Contents
    if contents.file_name()? != "Contents" {
        return None;
    }
    let bundle = contents.parent()?; // <bundle>.app
    if bundle.extension()? != "app" {
        return None;
    }
    Some(bundle.to_path_buf())
}

/// Convenience: preflight + write comm file + plan + spawn, in one
/// call. Errors cleanly if the replay file is missing on disk — we'd
/// rather short-circuit here than hand a dead path to Slippi and get a
/// confusing "couldn't open replay" dialog from the playback binary.
pub fn launch_replay(
    replay_path: &str,
    override_cmd: Option<&str>,
    iso_path: Option<&str>,
) -> Result<(), SlippiLaunchError> {
    if replay_path.is_empty() {
        return Err(SlippiLaunchError::NoReplayPath);
    }
    let p = Path::new(replay_path);
    if !p.exists() {
        return Err(SlippiLaunchError::ReplayFileMissing(p.to_path_buf()));
    }
    let comm = write_comm_file(replay_path)?;
    let plan = plan_launch(replay_path, &comm, override_cmd, iso_path)?;
    launch(&plan)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fake_comm() -> PathBuf {
        PathBuf::from("/tmp/slippi-comm.json")
    }

    #[test]
    fn override_command_wins_over_defaults() {
        let plan = plan_for_platform(
            "/tmp/game.slp",
            &fake_comm(),
            Some("/usr/local/bin/slippi"),
            None,
            Platform::MacOs,
        )
        .expect("should succeed");
        assert_eq!(plan.program, "/usr/local/bin/slippi");
        assert_eq!(
            plan.args,
            vec![
                "-i".to_string(),
                "/tmp/slippi-comm.json".to_string(),
                "-b".to_string(),
            ]
        );
    }

    #[test]
    fn override_command_trims_whitespace() {
        // Override non-empty after trim should bypass default lookup,
        // so this passes even though we pick Linux (no default).
        let plan = plan_for_platform(
            "/tmp/game.slp",
            &fake_comm(),
            Some("  /usr/local/bin/slippi  "),
            None,
            Platform::Linux,
        )
        .expect("should succeed");
        assert_eq!(plan.program, "/usr/local/bin/slippi");
    }

    #[test]
    fn override_app_bundle_resolves_to_inner_binary() {
        // The bug this fixes: earlier code handed the `.app` path
        // directly to Command::spawn, which failed with EACCES because
        // you can't exec a directory. We resolve to the inner binary.
        let plan = plan_for_platform(
            "/tmp/game.slp",
            &fake_comm(),
            Some("/Applications/Slippi Dolphin.app"),
            None,
            Platform::MacOs,
        )
        .expect("should succeed");
        assert_eq!(
            plan.program,
            "/Applications/Slippi Dolphin.app/Contents/MacOS/Slippi Dolphin"
        );
    }

    #[test]
    fn predict_app_inner_binary_basic() {
        let p = predict_app_inner_binary(Path::new("/Applications/Foo.app")).unwrap();
        assert_eq!(p, PathBuf::from("/Applications/Foo.app/Contents/MacOS/Foo"));
    }

    #[test]
    fn predict_app_inner_binary_handles_spaces() {
        let p = predict_app_inner_binary(Path::new("/Applications/Slippi Dolphin.app")).unwrap();
        assert_eq!(
            p,
            PathBuf::from("/Applications/Slippi Dolphin.app/Contents/MacOS/Slippi Dolphin")
        );
    }

    #[test]
    fn predict_app_inner_binary_returns_none_for_non_app() {
        assert!(predict_app_inner_binary(Path::new("/usr/local/bin/slippi")).is_none());
        assert!(predict_app_inner_binary(Path::new("/Applications/Foo")).is_none());
    }

    #[test]
    fn app_bundle_derived_from_inner_binary() {
        // The inner binary `open -a` needs to foreground-launch the bundle.
        let inner = Path::new("/Applications/Slippi Dolphin.app/Contents/MacOS/Slippi Dolphin");
        assert_eq!(
            app_bundle_for_inner_binary(inner),
            Some(PathBuf::from("/Applications/Slippi Dolphin.app"))
        );
    }

    #[test]
    fn app_bundle_none_for_bare_or_malformed_paths() {
        // Bare binary (power-user override) — no bundle, spawn directly.
        assert!(app_bundle_for_inner_binary(Path::new("/usr/local/bin/slippi")).is_none());
        // Has Contents/MacOS but the grandparent isn't a `.app`.
        assert!(app_bundle_for_inner_binary(Path::new("/opt/Contents/MacOS/foo")).is_none());
    }

    #[test]
    fn empty_or_whitespace_override_falls_back_to_platform_default() {
        // Blank override should act like None — hit macOS default.
        let plan = plan_for_platform("/tmp/g.slp", &fake_comm(), Some("   "), None, Platform::MacOs)
            .expect("should succeed");
        assert_eq!(
            plan.program,
            "/Applications/Slippi Dolphin.app/Contents/MacOS/Slippi Dolphin"
        );
    }

    #[test]
    fn empty_replay_path_rejected() {
        let err = plan_for_platform("", &fake_comm(), None, None, Platform::MacOs).unwrap_err();
        assert!(matches!(err, SlippiLaunchError::NoReplayPath));
    }

    #[test]
    fn macos_default_points_at_app_bundle_inner_binary() {
        let plan = plan_for_platform("/data/game.slp", &fake_comm(), None, None, Platform::MacOs)
            .expect("should succeed");
        assert_eq!(
            plan.program,
            "/Applications/Slippi Dolphin.app/Contents/MacOS/Slippi Dolphin"
        );
        assert_eq!(plan.args[0], "-i");
        assert_eq!(plan.args[1], "/tmp/slippi-comm.json");
        assert_eq!(plan.args[2], "-b");
    }

    #[test]
    fn iso_path_appends_exec_arg() {
        // With a Melee ISO configured, the plan must append `-e <iso>` so
        // Dolphin actually boots the game the replay runs against.
        let plan = plan_for_platform(
            "/data/game.slp",
            &fake_comm(),
            None,
            Some("/games/melee.iso"),
            Platform::MacOs,
        )
        .expect("should succeed");
        assert_eq!(
            plan.args,
            vec![
                "-i".to_string(),
                "/tmp/slippi-comm.json".to_string(),
                "-b".to_string(),
                "-e".to_string(),
                "/games/melee.iso".to_string(),
            ]
        );
    }

    #[test]
    fn blank_iso_path_is_ignored() {
        // A whitespace-only ISO path should be treated as "not set" — no
        // trailing `-e` with an empty argument.
        let plan = plan_for_platform(
            "/data/game.slp",
            &fake_comm(),
            None,
            Some("   "),
            Platform::MacOs,
        )
        .expect("should succeed");
        assert_eq!(plan.args.len(), 3, "got: {:?}", plan.args);
        assert!(!plan.args.iter().any(|a| a == "-e"));
    }

    #[test]
    fn linux_default_errors_without_override() {
        let err = plan_for_platform("/data/game.slp", &fake_comm(), None, None, Platform::Linux)
            .unwrap_err();
        assert!(matches!(err, SlippiLaunchError::NoDefaultForPlatform));
    }

    #[test]
    fn windows_default_errors_without_override() {
        let err = plan_for_platform("/data/game.slp", &fake_comm(), None, None, Platform::Windows)
            .unwrap_err();
        assert!(matches!(err, SlippiLaunchError::NoDefaultForPlatform));
    }

    #[test]
    fn unknown_platform_errors_without_override() {
        let err = plan_for_platform("/data/game.slp", &fake_comm(), None, None, Platform::Unknown)
            .unwrap_err();
        assert!(matches!(err, SlippiLaunchError::NoDefaultForPlatform));
    }

    #[test]
    fn comm_json_has_normal_mode_and_replay_path() {
        let json = build_comm_json("/tmp/game.slp");
        assert!(json.contains(r#""mode":"normal""#), "got: {json}");
        assert!(json.contains(r#""replay":"/tmp/game.slp""#), "got: {json}");
        assert!(json.contains(r#""isRealTimeMode":false"#), "got: {json}");
        assert!(json.contains(r#""outputOverlayFiles":false"#), "got: {json}");
    }

    #[test]
    fn comm_json_escapes_special_chars_in_path() {
        // Backslashes and quotes should both get escaped. Not common
        // in real paths, but a user with a weird folder name shouldn't
        // produce malformed JSON that makes Dolphin bail.
        let json = build_comm_json(r#"/tmp/weird"name\ok.slp"#);
        assert!(json.contains(r#"\""#), "got: {json}");
        assert!(json.contains(r#"\\"#), "got: {json}");
    }

    #[test]
    fn mdfind_output_prefers_playback_over_netplay() {
        // Both builds ship in the Slippi Launcher bundle; only the
        // playback one is the correct target for the `-i`/`-b`
        // protocol, so it wins when both are present.
        let stdout = "\
/Users/foo/Library/Application Support/Slippi Launcher/netplay/Slippi Dolphin.app
/Users/foo/Library/Application Support/Slippi Launcher/playback/Slippi Dolphin.app
";
        let picked = parse_mdfind_output(stdout).unwrap();
        assert_eq!(
            picked,
            PathBuf::from(
                "/Users/foo/Library/Application Support/Slippi Launcher/playback/Slippi Dolphin.app"
            )
        );
    }

    #[test]
    fn mdfind_output_skips_dmg_installer_volumes() {
        // Regression: first `mdfind` hit the user ran returned two
        // `/Volumes/Slippi Dolphin Installer*` paths before the real
        // install. Those are DMG images; launching binaries from them
        // breaks Dolphin's user-config lookup.
        let stdout = "\
/Volumes/Slippi Dolphin Installer/Slippi Dolphin.app
/Volumes/Slippi Dolphin Installer 1/Slippi Dolphin.app
/Users/foo/Library/Application Support/Slippi Launcher/netplay/Slippi Dolphin.app
";
        let picked = parse_mdfind_output(stdout).unwrap();
        assert!(picked.to_string_lossy().contains("/netplay/"));
        assert!(!picked.to_string_lossy().contains("/Volumes/"));
    }

    #[test]
    fn mdfind_output_falls_back_to_other_paths() {
        // No `/playback/` or `/netplay/` in the path — take what's
        // there (probably the legacy `/Applications/Slippi Dolphin.app`).
        let stdout = "/Applications/Slippi Dolphin.app\n";
        let picked = parse_mdfind_output(stdout).unwrap();
        assert_eq!(picked, PathBuf::from("/Applications/Slippi Dolphin.app"));
    }

    #[test]
    fn mdfind_output_empty_returns_none() {
        assert!(parse_mdfind_output("").is_none());
        assert!(parse_mdfind_output("\n\n\n").is_none());
        // Only `/Volumes/*` results = no usable hit.
        assert!(parse_mdfind_output("/Volumes/X/Slippi Dolphin.app\n").is_none());
    }

    #[test]
    fn macos_default_candidates_orders_playback_before_netplay() {
        let candidates = macos_default_candidates();
        // The `/Applications/Slippi Dolphin.app` legacy fallback is
        // always present regardless of HOME.
        assert!(candidates
            .iter()
            .any(|p| p == &PathBuf::from("/Applications/Slippi Dolphin.app")));

        // Relative ordering of the two launcher-bundled builds only
        // gets asserted when HOME is set (always true on a real macOS
        // host; CI containers may not set it).
        if std::env::var("HOME").is_ok() {
            let playback_idx = candidates
                .iter()
                .position(|p| p.to_string_lossy().contains("/playback/"));
            let netplay_idx = candidates
                .iter()
                .position(|p| p.to_string_lossy().contains("/netplay/"));
            assert!(playback_idx.is_some(), "playback candidate missing");
            assert!(netplay_idx.is_some(), "netplay candidate missing");
            assert!(
                playback_idx < netplay_idx,
                "playback must come before netplay"
            );
        }
    }

    #[test]
    fn windows_candidates_order_playback_before_netplay() {
        // Set a known APPDATA so the test is deterministic regardless of host.
        std::env::set_var("APPDATA", r"C:\Users\me\AppData\Roaming");
        let c = windows_default_candidates();
        std::env::remove_var("APPDATA");
        assert_eq!(c.len(), 2);
        assert!(c[0].to_string_lossy().contains("playback"));
        assert!(c[0].to_string_lossy().ends_with("Slippi Dolphin.exe"));
        assert!(c[1].to_string_lossy().contains("netplay"));
    }

    #[test]
    fn linux_dirs_prefer_xdg_then_playback() {
        std::env::set_var("XDG_CONFIG_HOME", "/home/me/.config");
        let dirs = linux_dolphin_dirs();
        std::env::remove_var("XDG_CONFIG_HOME");
        assert_eq!(dirs.len(), 2);
        assert!(dirs[0]
            .to_string_lossy()
            .ends_with("/Slippi Launcher/playback"));
        assert!(dirs[1].to_string_lossy().ends_with("/Slippi Launcher/netplay"));
    }

    #[test]
    fn plan_with_default_uses_provided_default_when_override_missing() {
        let plan = plan_with_default(
            "/tmp/game.slp",
            &fake_comm(),
            None,
            None,
            Some(PathBuf::from("/custom/discovered/Slippi Dolphin")),
        )
        .expect("should succeed");
        assert_eq!(plan.program, "/custom/discovered/Slippi Dolphin");
    }

    #[test]
    fn plan_with_default_override_still_wins_over_discovery() {
        // Even if discovery returned a path, an explicit override
        // trumps it. This keeps the Settings UI authoritative.
        let plan = plan_with_default(
            "/tmp/game.slp",
            &fake_comm(),
            Some("/custom/override/Slippi Dolphin"),
            None,
            Some(PathBuf::from("/discovered/path/Slippi Dolphin")),
        )
        .expect("should succeed");
        assert_eq!(plan.program, "/custom/override/Slippi Dolphin");
    }

    #[test]
    fn plan_with_default_errors_when_both_missing() {
        let err = plan_with_default("/tmp/g.slp", &fake_comm(), None, None, None).unwrap_err();
        assert!(matches!(err, SlippiLaunchError::NoDefaultForPlatform));
    }

    #[test]
    fn error_messages_are_user_readable() {
        // Locked in as a test so a future refactor has to deliberately
        // update the UI strings.
        assert!(SlippiLaunchError::NoReplayPath
            .to_string()
            .contains("re-ingest"));
        assert!(SlippiLaunchError::NoDefaultForPlatform
            .to_string()
            .contains("Settings"));
        let missing = SlippiLaunchError::ReplayFileMissing(PathBuf::from("/tmp/x.slp"));
        assert!(missing.to_string().contains("/tmp/x.slp"));
        let spawn_err = SlippiLaunchError::SpawnFailed("permission denied".to_string());
        assert!(spawn_err.to_string().contains("permission denied"));
        let comm_err = SlippiLaunchError::CommFileFailed("disk full".to_string());
        assert!(comm_err.to_string().contains("disk full"));
        assert!(comm_err.to_string().contains("comm file"));
    }
}
