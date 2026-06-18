//! Persistent app settings.
//!
//! Stored as TOML at the OS-appropriate config location (resolved via the
//! `directories` crate's `ProjectDirs`):
//!
//! - Linux:   `$XDG_CONFIG_HOME/stats-melee/config.toml`
//!            (falls back to `$HOME/.config/stats-melee/config.toml`)
//! - macOS:   `$HOME/Library/Application Support/dev.slippi.stats-melee/config.toml`
//! - Windows: `%APPDATA%\slippi\stats-melee\config\config.toml`
//!
//! The DB file mirrors the same layout but lives under the *data* dir rather
//! than the config dir, so config survives a DB nuke and vice versa.

use std::path::PathBuf;

use anyhow::{anyhow, Result};
use directories::ProjectDirs;
use serde::{Deserialize, Serialize};

const QUALIFIER: &str = "dev";
const ORGANIZATION: &str = "slippi";
const APPLICATION: &str = "stats-melee";

/// User-tweakable app settings.
///
/// Every field is optional / has a sensible default, so a fresh user with
/// no config file on disk still gets a usable `AppConfig::default()` value.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AppConfig {
    /// Root directory containing session subfolders of `.slp` replays.
    /// When `None`, the app will prompt the user to pick one on next launch.
    #[serde(default)]
    pub replay_dir: Option<PathBuf>,

    /// The current user's Slippi connect code (e.g. "MATT#123"). Used to
    /// filter "your games" views. Empty string = not set.
    #[serde(default)]
    pub user_player_code: String,

    /// Override location for the SQLite database. When `None`, the app
    /// uses [`AppConfig::default_db_path`].
    #[serde(default)]
    pub db_path: Option<PathBuf>,

    /// Path to the user's Slippi Dolphin install. Can be either:
    /// - the `.app` bundle (macOS file pickers return this) — the
    ///   launcher resolves it to `Contents/MacOS/<Name>` automatically
    /// - the inner binary path directly
    ///
    /// When `None` / empty, the app falls back to the per-platform
    /// default (`/Applications/Slippi Dolphin.app/...` on macOS, error
    /// on Linux/Windows — see [`crate::slippi`]).
    #[serde(default)]
    pub slippi_playback_command: Option<String>,

    /// Path to the Melee 1.02 NTSC ISO used by the headless render
    /// pipeline (Track 10). Same ISO the user already has configured
    /// inside Slippi Launcher for normal replay viewing — we just
    /// need it spelled out separately here because the render worker
    /// passes it to Dolphin as an explicit `--exec=<path>` arg rather
    /// than relying on Dolphin's recent-files state.
    ///
    /// When `None` / empty, the "Render video" button on the viewer
    /// page is disabled with a "set the Melee ISO path in Settings"
    /// tooltip.
    #[serde(default)]
    pub melee_iso_path: Option<PathBuf>,

    /// Override path for the `ffmpeg` binary used to mux Dolphin's
    /// frame + audio dumps into the cached MP4. `None` / empty means
    /// "whatever's on PATH" — the typical macOS install via
    /// `brew install ffmpeg` lands at `/opt/homebrew/bin/ffmpeg` and
    /// is on PATH automatically.
    ///
    /// Stored as a `String` rather than `PathBuf` so `Some("ffmpeg")`
    /// (a bare name to be PATH-resolved) round-trips cleanly through
    /// TOML. `effective_ffmpeg_command` resolves this to the actual
    /// `Command::new(...)` argument.
    #[serde(default)]
    pub ffmpeg_command: Option<String>,
}

impl AppConfig {
    /// Best-effort load. Silently falls back to `AppConfig::default()` on
    /// any error (missing file, unreadable, malformed) so the first launch
    /// path just works.
    pub fn load() -> Self {
        Self::try_load().unwrap_or_default()
    }

    /// Strict load — used by tests + callers that want to surface "couldn't
    /// read config" errors in the UI instead of silently resetting.
    pub fn try_load() -> Result<Self> {
        let path = Self::config_path()?;
        let raw = std::fs::read_to_string(&path)
            .map_err(|e| anyhow!("reading {}: {e}", path.display()))?;
        Self::from_toml_str(&raw)
    }

    /// Persist to the config path, creating the parent directory if needed.
    pub fn save(&self) -> Result<()> {
        let path = Self::config_path()?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| anyhow!("mkdir {}: {e}", parent.display()))?;
        }
        let raw = toml::to_string_pretty(self)
            .map_err(|e| anyhow!("serialize config: {e}"))?;
        std::fs::write(&path, raw)
            .map_err(|e| anyhow!("writing {}: {e}", path.display()))
    }

    /// Resolve the config file path. Factored out so tests can reason about
    /// it without actually touching the filesystem.
    pub fn config_path() -> Result<PathBuf> {
        Ok(project_dirs()?.config_dir().join("config.toml"))
    }

    /// Default DB location when `db_path` is unset.
    pub fn default_db_path() -> Result<PathBuf> {
        Ok(project_dirs()?.data_dir().join("stats_melee.db"))
    }

    /// The DB path the app should actually open — user override if set,
    /// otherwise the OS-default data location.
    pub fn effective_db_path(&self) -> Result<PathBuf> {
        match &self.db_path {
            Some(p) => Ok(p.clone()),
            None => Self::default_db_path(),
        }
    }

    /// The ffmpeg binary the render worker should invoke. Returns the
    /// trimmed user override if set, otherwise the bare name `"ffmpeg"`
    /// — `Command::new("ffmpeg")` resolves through `PATH`, which
    /// matches the brew / package-manager-installed defaults.
    pub fn effective_ffmpeg_command(&self) -> PathBuf {
        match self
            .ffmpeg_command
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            Some(s) => PathBuf::from(s),
            None => PathBuf::from("ffmpeg"),
        }
    }

    // --- TOML-only helpers (pure; unit-testable) -----------------------------

    /// Parse TOML text into an `AppConfig`.
    pub fn from_toml_str(raw: &str) -> Result<Self> {
        toml::from_str(raw).map_err(|e| anyhow!("parse config toml: {e}"))
    }

    /// Render an `AppConfig` as TOML text. Round-trips with
    /// [`Self::from_toml_str`].
    pub fn to_toml_string(&self) -> Result<String> {
        toml::to_string_pretty(self).map_err(|e| anyhow!("serialize config: {e}"))
    }

    /// True when the user-facing "first launch wizard" should still run
    /// (i.e. we don't have a replay dir yet).
    pub fn needs_onboarding(&self) -> bool {
        self.replay_dir
            .as_deref()
            .map_or(true, |p| p.as_os_str().is_empty())
    }
}

fn project_dirs() -> Result<ProjectDirs> {
    ProjectDirs::from(QUALIFIER, ORGANIZATION, APPLICATION)
        .ok_or_else(|| anyhow!("could not resolve ProjectDirs for stats-melee"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_has_no_replay_dir() {
        let cfg = AppConfig::default();
        assert!(cfg.replay_dir.is_none());
        assert!(cfg.user_player_code.is_empty());
        assert!(cfg.db_path.is_none());
        assert!(cfg.slippi_playback_command.is_none());
        assert!(cfg.melee_iso_path.is_none());
        assert!(cfg.ffmpeg_command.is_none());
    }

    #[test]
    fn effective_ffmpeg_falls_back_to_path_resolution() {
        // Unset → bare name, lets Command::new("ffmpeg") resolve via PATH.
        let cfg = AppConfig::default();
        assert_eq!(cfg.effective_ffmpeg_command(), PathBuf::from("ffmpeg"));

        // Whitespace-only override is treated as unset — same fallback.
        let cfg = AppConfig {
            ffmpeg_command: Some("   ".to_string()),
            ..Default::default()
        };
        assert_eq!(cfg.effective_ffmpeg_command(), PathBuf::from("ffmpeg"));

        // Real override wins.
        let cfg = AppConfig {
            ffmpeg_command: Some("/opt/homebrew/bin/ffmpeg".to_string()),
            ..Default::default()
        };
        assert_eq!(
            cfg.effective_ffmpeg_command(),
            PathBuf::from("/opt/homebrew/bin/ffmpeg")
        );
    }

    #[test]
    fn toml_roundtrip_preserves_all_fields() {
        let cfg = AppConfig {
            replay_dir: Some(PathBuf::from("/home/user/slippi")),
            user_player_code: "MATT#123".to_string(),
            db_path: Some(PathBuf::from("/tmp/stats_melee.db")),
            slippi_playback_command: Some(
                "/Applications/Slippi Dolphin.app/Contents/MacOS/Slippi Dolphin".to_string(),
            ),
            melee_iso_path: Some(PathBuf::from("/home/user/melee.iso")),
            ffmpeg_command: Some("/opt/homebrew/bin/ffmpeg".to_string()),
        };

        let toml_text = cfg.to_toml_string().expect("serialize");
        let parsed = AppConfig::from_toml_str(&toml_text).expect("parse");

        assert_eq!(parsed.replay_dir, cfg.replay_dir);
        assert_eq!(parsed.user_player_code, cfg.user_player_code);
        assert_eq!(parsed.db_path, cfg.db_path);
        assert_eq!(parsed.slippi_playback_command, cfg.slippi_playback_command);
        assert_eq!(parsed.melee_iso_path, cfg.melee_iso_path);
        assert_eq!(parsed.ffmpeg_command, cfg.ffmpeg_command);
    }

    #[test]
    fn empty_toml_yields_default() {
        let parsed = AppConfig::from_toml_str("").expect("parse empty");
        assert!(parsed.replay_dir.is_none());
        assert!(parsed.user_player_code.is_empty());
        assert!(parsed.db_path.is_none());
        assert!(parsed.slippi_playback_command.is_none());
        assert!(parsed.melee_iso_path.is_none());
        assert!(parsed.ffmpeg_command.is_none());
    }

    #[test]
    fn partial_toml_fills_missing_fields_with_defaults() {
        // Only user_player_code specified — other fields should default.
        let parsed =
            AppConfig::from_toml_str("user_player_code = \"FOX#1\"").expect("parse");
        assert!(parsed.replay_dir.is_none());
        assert_eq!(parsed.user_player_code, "FOX#1");
        assert!(parsed.db_path.is_none());
        assert!(parsed.slippi_playback_command.is_none());
        assert!(parsed.melee_iso_path.is_none());
        assert!(parsed.ffmpeg_command.is_none());
    }

    #[test]
    fn malformed_toml_errors_cleanly() {
        let err = AppConfig::from_toml_str("not = = valid toml").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("parse config toml"), "got: {msg}");
    }
}
