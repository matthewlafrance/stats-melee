//! Top-level [`eframe::App`] implementation for stats-melee.
//!
//! Owns the app-wide state — config, DB connection, cached replay rows —
//! and delegates rendering of each page to an inline method.

use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::thread;

use diesel::SqliteConnection;
use eframe::egui;
use egui_extras::{Column, TableBuilder};

use stats_melee::analysis_cache::{AnalysisCache, AnalysisCacheConfig};
use stats_melee::combat::CombatV2Config;
use stats_melee::gamedata::{CHARACTERS, STAGES};
use stats_melee::video_cache::{VideoCache, VideoCacheConfig};
use stats_melee::{PlayerSummary, PlayerSummaryFilter};

use crate::config::AppConfig;
use crate::render_worker::{self, RenderMsg, RenderRequest};
use crate::replay_list::{self, ReplayRow, SortDirection, SortKey};
use crate::slippi;
use crate::viewer::{self, ViewerState};

/// Toggle for the in-house render-video pipeline (Tracks 10 & 12).
///
/// Set to `false`: the in-house render pipeline (Track 12's
/// `.slp → DTM → vanilla Dolphin → ffmpeg → MP4`) is parked while we
/// ship a production version built around the Slippi launcher alone
/// (the "▶ Open in Slippi" button, which plays replays directly in the
/// user's Slippi Dolphin install). The DTM/boot-nav work still has open
/// bugs — see TODO.txt and the Track 12 docs — so it's gated off rather
/// than removed. With the gate off:
/// - The "Render video" / "Open video" buttons disappear from the
///   viewer page nav bar.
/// - The Melee ISO + ffmpeg override rows disappear from Settings.
/// - The render-state fields (`render_rx`, `render_status`, etc.)
///   stay on the app struct (they're cheap and removing them would
///   churn the constructor + tests).
/// - The render worker module compiles unchanged; flipping this back
///   to `true` re-enables the entire pipeline without further code
///   changes.
const RENDER_VIDEO_FEATURE_ENABLED: bool = false;

/// Accent color used for the active nav toggle, the settings gear when
/// open, and selection highlights. Kept in sync with [`apply_theme`].
const ACCENT: egui::Color32 = egui::Color32::from_rgb(0x4C, 0x8B, 0xF5);

/// Max width of the centered page content column. Sized to fit the full
/// replay-library row (~1060 px of columns + spacing) so the table sits
/// centered rather than hugging the left edge on wide windows.
const CONTENT_MAX_WIDTH: f32 = 1080.0;

/// Which page is currently displayed in the main panel.
///
/// `ReplayLibrary` and `Analytics` are the two primary views, reached
/// from the floating toggle at the bottom of the window. `Settings` is
/// reached from the gear in the top bar. `ReplayViewer` is a drill-down
/// from the library ("View" on a row) and has no nav entry of its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Page {
    ReplayLibrary,
    Analytics,
    Settings,
    ReplayViewer,
}

pub struct StatsMeleeApp {
    page: Page,
    config: AppConfig,
    /// Last failed config save, if any.
    last_config_error: Option<String>,

    /// Lazily-opened SQLite connection. `None` until the first time we
    /// attempt to use the DB after a valid replay_dir has been configured.
    db_conn: Option<SqliteConnection>,
    /// The db path the current `db_conn` was opened against. We compare
    /// this to `config.effective_db_path()` each frame so a user switching
    /// DB paths in Settings re-triggers open.
    db_opened_path: Option<PathBuf>,
    /// Most recent error from opening / using the DB.
    db_error: Option<String>,

    /// Cached list of replays. Refreshed manually via the UI — we don't
    /// auto-reload every frame because each reload is two DB queries.
    rows: Vec<ReplayRow>,
    /// Most recent error from loading rows.
    rows_error: Option<String>,

    /// Status line after the most recent ingestion run.
    last_ingest_summary: Option<String>,
    /// Receiver for the in-flight ingestion worker, if one is
    /// running. `Some` exactly when a scan thread has been spawned
    /// and we haven't yet drained its `Done` message. Same pattern
    /// as `summary_rx` / `render_rx`.
    ingest_rx: Option<mpsc::Receiver<IngestMsg>>,
    /// True while the ingest worker is in flight. Mirrors
    /// `ingest_rx.is_some()` but makes the UI's button-disabled +
    /// spinner-shown checks self-documenting.
    ingest_loading: bool,
    /// Latch flipped on the first successful auto-scan attempt so
    /// `update()` doesn't kick off a scan every frame. Reset to
    /// `false` whenever the user picks a new replay folder so the
    /// next update fires a fresh scan against the new location.
    auto_scan_attempted: bool,

    /// Cached PlayerSummary for the Analytics page. Rebuilt on demand —
    /// it's a handful of DB queries, not frame-cheap. Keyed by `summary_for`
    /// so a Settings code edit *or* a selector change invalidates it.
    summary: Option<PlayerSummary>,
    /// Most recent error from `player_summary_filtered`.
    summary_error: Option<String>,
    /// The (code, character_id, stage_id) tuple the cached `summary` was
    /// built for. We compare this to the current config + selectors to
    /// know when to rebuild — flipping the character selector regenerates
    /// the summary the same way changing the user code does.
    summary_for: Option<SummaryKey>,
    /// Receiver side of the background summary worker. When `Some`, a
    /// worker thread is computing a summary and we should be polling it
    /// each frame via [`poll_summary_worker`]. `None` means idle.
    summary_rx: Option<mpsc::Receiver<SummaryMsg>>,
    /// True while a worker is in flight. Mirrors `summary_rx.is_some()` but
    /// makes the "show a spinner" check in the UI loop self-documenting.
    summary_loading: bool,
    /// Character filter selected in the Analytics page's "Character" combo,
    /// or `None` for "Any". Stored as the raw `gamePlayer.character` id —
    /// indexes [`CHARACTERS`] for the display label.
    analytics_character_id: Option<i32>,
    /// Stage filter selected in the Analytics page's "Stage" combo, or
    /// `None` for "Any". Raw `game.stage` id — indexes [`STAGES`].
    analytics_stage_id: Option<i32>,
    /// Clone of the egui Context captured on first `update()` call. Lets
    /// the worker thread call `request_repaint()` so we don't sit idle
    /// waiting for a mouse move when the summary is ready.
    egui_ctx: Option<egui::Context>,

    /// True once the user clicks "Delete all replays" on the Settings
    /// page — flips the button into a two-step confirm state so a
    /// misclick can't wipe the DB. Reset on navigation or explicit
    /// cancel.
    nuke_confirm_pending: bool,
    /// Status line for the most recent nuke attempt — success count or
    /// error message. Populated on click of the red Confirm button.
    last_nuke_summary: Option<String>,

    /// Active sort column for the Replay Library table. Defaults to
    /// "most recently ingested first". Persists across reloads so a
    /// new ingest re-surfaces the user's chosen ordering.
    sort_key: SortKey,
    sort_direction: SortDirection,

    /// Free-text search filter for the Replay Library. Matched
    /// substring-style against player codes, character names, stage
    /// names, and ingested timestamps. Session-state only — not
    /// persisted to config because "what I was searching for" isn't
    /// useful across launches and would surprise the user on next
    /// startup.
    replay_search: String,

    /// Game id whose row's delete button is currently showing
    /// "Confirm?" instead of the trash glyph. `None` when no row is
    /// in confirm state; `Some(id)` while waiting for the user to
    /// either click again to delete or click another action to
    /// reset. Mirrors `nuke_confirm_pending` for the all-replays
    /// version, just per-row.
    delete_confirm_game_id: Option<i32>,
    /// Status line for the most recent per-row delete attempt —
    /// success or error message. Cleared when the user navigates or
    /// initiates another delete.
    last_delete_summary: Option<Result<i32, String>>,

    /// Currently-viewed replay id. `Some` exactly while the user is on
    /// the [`Page::ReplayViewer`] page — navigating away drops it so a
    /// stale viewer state doesn't flash back in on re-entry.
    viewing_game_id: Option<i32>,
    /// Cached viewer state for `viewing_game_id`. `Ok` when load_viewer
    /// succeeded (the scrub bar may still show an error internally via
    /// `ViewerState.combat`), `Err` when we couldn't even load the DB
    /// rows. `None` before the first load for this game.
    viewer_state: Option<Result<ViewerState, String>>,
    /// Status line from the most recent "Open in Slippi" click, shown
    /// below the button on the viewer page. Cleared when the user
    /// navigates to a different replay.
    last_slippi_launch: Option<Result<(), String>>,

    /// Persistent file-backed cache for [`ReplayAnalysis`] keyed on
    /// each .slp's content hash. Constructed once at app startup; the
    /// viewer's load path consults it before re-parsing peppi, which
    /// turns the second view of any replay from a ~1s blocking parse
    /// into an instant DB-style read. See [`AnalysisCache`] +
    /// `Track 11` in TODO.txt for the full rationale.
    analysis_cache: AnalysisCache,

    /// Aggressive file-backed cache for rendered MP4s, keyed on the
    /// .slp content hash. Constructed once at startup; wiped on Drop
    /// so the multi-GB videos don't accumulate across sessions.
    /// `Track 10` in TODO.txt for the full policy contrast against
    /// the analysis cache.
    video_cache: VideoCache,
    /// Receiver for the in-flight render worker, if one is running.
    /// `Some` exactly when a render thread has been spawned and we
    /// haven't yet drained its `Done` message. Mirrors
    /// `summary_rx`/`summary_loading` for the analytics worker.
    render_rx: Option<mpsc::Receiver<RenderMsg>>,
    /// `Some(hash)` while a render is in flight — the .slp content
    /// hash the worker is producing an MP4 for. We need this on the
    /// app side so `poll_render_worker` can call
    /// `video_cache.finalize(hash)` once the worker is done. Cleared
    /// alongside `render_rx`.
    render_in_flight_hash: Option<String>,
    /// Most recent progress message from the render worker, displayed
    /// next to a spinner on the viewer page. `None` outside of an
    /// active render.
    render_status: Option<String>,
    /// Status of the most recently completed render — `Ok(path)` to
    /// the cached MP4 on success, `Err(message)` on failure. Cleared
    /// when the user navigates to a different replay.
    last_render_summary: Option<Result<PathBuf, String>>,

    /// Lazily-populated GPU-texture cache for character + stage icons,
    /// with a drawn-badge fallback when no PNG asset is present. See
    /// [`crate::icons`].
    icons: crate::icons::IconCache,
}

/// One message off the summary-worker channel. We flatten the Result into a
/// plain enum so the sender side doesn't have to deal with `Send` bounds on
/// anyhow errors — a `String` round-trips cleanly.
enum SummaryMsg {
    Ok(PlayerSummary),
    Err(String),
}

/// One message off the ingestion-worker channel. Same shape as
/// [`SummaryMsg`]: success carries the count of newly-ingested
/// games, failure carries a stringified error.
enum IngestMsg {
    Ok(usize),
    Err(String),
}

/// Cache key for the Analytics page's PlayerSummary — the player code plus
/// the active character/stage selectors. A change to any field invalidates
/// the cached summary and re-kicks the worker.
#[derive(Debug, Clone, PartialEq, Eq)]
struct SummaryKey {
    code: String,
    character_id: Option<i32>,
    stage_id: Option<i32>,
}

impl SummaryKey {
    fn filter(&self) -> PlayerSummaryFilter {
        PlayerSummaryFilter {
            character_id: self.character_id,
            stage_id: self.stage_id,
        }
    }
}

impl StatsMeleeApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        apply_theme(&cc.egui_ctx);
        let config = AppConfig::load();
        // If onboarding is needed, default to Settings so the first thing
        // the user sees is the "pick replay folder" widget.
        let page = if config.needs_onboarding() {
            Page::Settings
        } else {
            Page::ReplayLibrary
        };

        // Stand up the analysis sidecar cache early — it's cheap (just
        // creates a directory) and lives for the whole app session.
        // If the cache fails to open (e.g. permissions on the cache
        // dir), fall back to a tempdir-rooted cache for this session
        // so the viewer never crashes — the user just doesn't get
        // persistence across restarts.
        let analysis_cache = open_analysis_cache().unwrap_or_else(|e| {
            eprintln!("stats-melee: analysis cache disabled: {e}");
            // Tempdir fallback. `prune_on_drop = true` means we don't
            // leak files when the OS cleans up the tempdir; the cache
            // is effectively in-process for this session only.
            let dir = std::env::temp_dir().join("stats-melee-analysis-fallback");
            AnalysisCache::open(
                dir,
                AnalysisCacheConfig {
                    max_bytes: 50 * 1024 * 1024,
                    prune_on_drop: true,
                },
                CombatV2Config::default(),
            )
            .expect("tempdir-rooted fallback cache should always open")
        });

        // Same shape for the video cache — but with the aggressive
        // "wipe on close" policy; see `VideoCacheConfig::default`.
        // Tempdir fallback for the same reason.
        let video_cache = open_video_cache().unwrap_or_else(|e| {
            eprintln!("stats-melee: video cache disabled: {e}");
            let dir = std::env::temp_dir().join("stats-melee-video-fallback");
            VideoCache::open(dir, VideoCacheConfig::default())
                .expect("tempdir-rooted fallback video cache should always open")
        });

        Self {
            page,
            config,
            last_config_error: None,
            db_conn: None,
            db_opened_path: None,
            db_error: None,
            rows: Vec::new(),
            rows_error: None,
            last_ingest_summary: None,
            ingest_rx: None,
            ingest_loading: false,
            auto_scan_attempted: false,
            summary: None,
            summary_error: None,
            summary_for: None,
            summary_rx: None,
            summary_loading: false,
            analytics_character_id: None,
            analytics_stage_id: None,
            egui_ctx: None,
            nuke_confirm_pending: false,
            last_nuke_summary: None,
            sort_key: SortKey::IngestedAt,
            sort_direction: SortDirection::Desc,
            replay_search: String::new(),
            delete_confirm_game_id: None,
            last_delete_summary: None,
            viewing_game_id: None,
            viewer_state: None,
            last_slippi_launch: None,
            analysis_cache,
            video_cache,
            render_rx: None,
            render_in_flight_hash: None,
            render_status: None,
            last_render_summary: None,
            icons: crate::icons::IconCache::default(),
        }
    }

    /// Drop a single replay (game + game_player_stat + punish rows) by
    /// `game_id`, then invalidate the cached row list + analytics
    /// summary + analysis cache entry for that hash. Mirrors
    /// [`Self::nuke_replays`] at row-scope. Called from the per-row
    /// "Confirm?" button on the library table.
    fn delete_replay(&mut self, game_id: i32) {
        // Pull the content_hash off the row before we delete it, so
        // we can clean up the matching analysis-cache sidecar after.
        // Best-effort: a missing hash just means we skip the cache
        // wipe — the cached entry will get evicted naturally when
        // the LRU budget needs the space.
        let content_hash = self.fetch_content_hash(game_id);

        self.ensure_db();
        let Some(conn) = self.db_conn.as_mut() else {
            self.last_delete_summary = Some(Err(self
                .db_error
                .clone()
                .unwrap_or_else(|| "db not open".to_string())));
            return;
        };

        match stats_melee::nuke_replay(conn, game_id) {
            Ok(0) => {
                self.last_delete_summary = Some(Err(format!(
                    "Game #{game_id} not found (already deleted?)"
                )));
            }
            Ok(_n) => {
                self.last_delete_summary = Some(Ok(game_id));
                // Drop the row from our in-memory list — re-render
                // is instant, no need to round-trip through the DB.
                self.rows.retain(|r| r.game_id != game_id);
                // Analytics summary now reflects a different
                // population; force a recompute on next view.
                self.summary = None;
                self.summary_for = None;
                self.summary_rx = None;
                self.summary_loading = false;
                // Best-effort: drop the analysis sidecar for this
                // replay's content hash. Failure is logged but not
                // surfaced.
                if let Some(hash) = content_hash {
                    // AnalysisCache doesn't expose per-key delete
                    // yet — orphaned entries fall out via LRU
                    // eviction when the budget rolls over.
                    let _ = hash;
                }
            }
            Err(e) => {
                self.last_delete_summary = Some(Err(format!("Delete failed: {e}")));
            }
        }
        self.delete_confirm_game_id = None;
    }

    /// Drop every replay-scoped row from the DB, then invalidate all of
    /// our cached state (rows + summary + in-flight worker) so the UI
    /// reflects the now-empty state on the next frame. Called from the
    /// red confirm button on the Settings page.
    fn nuke_replays(&mut self) {
        // Make sure we have a connection before we try anything — the
        // button shouldn't render otherwise, but guard against races.
        self.ensure_db();
        let Some(conn) = self.db_conn.as_mut() else {
            self.last_nuke_summary = Some(
                self.db_error
                    .clone()
                    .unwrap_or_else(|| "db not open".to_string()),
            );
            return;
        };

        match stats_melee::nuke_replays(conn) {
            Ok(n) => {
                self.last_nuke_summary = Some(format!("Deleted {n} replay(s)."));
                // Clear all view caches so the empty DB is reflected.
                self.rows.clear();
                self.rows_error = None;
                self.summary = None;
                self.summary_error = None;
                self.summary_for = None;
                self.summary_rx = None;
                self.summary_loading = false;
                self.last_ingest_summary = None;
                // Wipe the analysis sidecar cache too — the entries
                // are keyed on .slp content hashes, none of which map
                // to a row anymore. Best-effort: a clear failure
                // shouldn't block the nuke message.
                if let Err(e) = self.analysis_cache.clear() {
                    eprintln!("nuke: analysis cache clear failed: {e}");
                }
            }
            Err(e) => {
                self.last_nuke_summary = Some(format!("Nuke failed: {e}"));
            }
        }
        self.nuke_confirm_pending = false;
    }

    /// Open a native folder picker; on pick, update config and persist.
    /// Resets the auto-scan latch so the next `update()` tick fires a
    /// fresh scan against the new dir — matches the user's mental
    /// model of "I just told the app where my replays are; ingest
    /// them."
    fn pick_replay_dir(&mut self) {
        if let Some(path) = rfd::FileDialog::new()
            .set_title("Pick your Slippi replay folder")
            .pick_folder()
        {
            self.config.replay_dir = Some(path);
            self.save_config();
            self.auto_scan_attempted = false;
        }
    }

    /// Open a native file picker for the Slippi Dolphin executable.
    /// Stores the absolute path in `slippi_playback_command` and persists.
    /// Cancelling the dialog leaves the current value untouched.
    fn pick_slippi_binary(&mut self) {
        if let Some(path) = rfd::FileDialog::new()
            .set_title("Pick your Slippi Dolphin executable")
            .pick_file()
        {
            self.config.slippi_playback_command = Some(path.display().to_string());
            self.save_config();
        }
    }

    /// Open a native file picker for the Melee ISO. Filters to common
    /// disc-image extensions but the user can override — Slippi
    /// Dolphin accepts `.iso`, `.ciso`, `.gcm`, etc.
    fn pick_melee_iso(&mut self) {
        if let Some(path) = rfd::FileDialog::new()
            .set_title("Pick your Super Smash Bros. Melee 1.02 ISO")
            .add_filter("Disc image", &["iso", "ciso", "gcm", "gcz"])
            .pick_file()
        {
            self.config.melee_iso_path = Some(path);
            self.save_config();
        }
    }

    fn save_config(&mut self) {
        match self.config.save() {
            Ok(()) => self.last_config_error = None,
            Err(e) => self.last_config_error = Some(e.to_string()),
        }
    }

    /// Ensure `self.db_conn` is open against the config's current effective
    /// DB path. Re-opens if the user pointed the config at a new location.
    /// Leaves `self.db_error` populated on failure and clears `db_conn`.
    fn ensure_db(&mut self) {
        let target = match self.config.effective_db_path() {
            Ok(p) => p,
            Err(e) => {
                self.db_error = Some(e.to_string());
                self.db_conn = None;
                return;
            }
        };

        // Already open against the same path — nothing to do.
        if self.db_opened_path.as_deref() == Some(target.as_path()) && self.db_conn.is_some() {
            return;
        }

        match stats_melee::open_database(&target) {
            Ok(conn) => {
                self.db_conn = Some(conn);
                self.db_opened_path = Some(target);
                self.db_error = None;
            }
            Err(e) => {
                self.db_conn = None;
                self.db_error = Some(e.to_string());
            }
        }
    }

    /// Reload the cached row list from the open DB. No-op if the DB isn't
    /// open yet — caller should invoke `ensure_db` first.
    fn reload_rows(&mut self) {
        let Some(conn) = self.db_conn.as_mut() else {
            return;
        };

        let code_filter = {
            let code = self.config.user_player_code.trim();
            if code.is_empty() {
                None
            } else {
                Some(code.to_string())
            }
        };

        match replay_list::load_rows(conn, code_filter.as_deref()) {
            Ok(mut rows) => {
                // Apply the user's current column sort. The DB query
                // already returns rows by `id desc` (newest first) which
                // matches the default IngestedAt desc ordering, but we
                // re-sort unconditionally so an alternate sort_key
                // survives a refresh.
                replay_list::sort_rows(&mut rows, self.sort_key, self.sort_direction);
                self.rows = rows;
                self.rows_error = None;
            }
            Err(e) => {
                self.rows_error = Some(e.to_string());
            }
        }
    }

    /// Set the table sort column. Clicking the same column flips
    /// direction; clicking a different column resets to that column's
    /// default direction (see [`SortKey::default_direction`]).
    fn set_sort(&mut self, key: SortKey) {
        if self.sort_key == key {
            self.sort_direction = match self.sort_direction {
                SortDirection::Asc => SortDirection::Desc,
                SortDirection::Desc => SortDirection::Asc,
            };
        } else {
            self.sort_key = key;
            self.sort_direction = key.default_direction();
        }
        // Re-sort the already-loaded rows in place — no DB round-trip
        // needed.
        replay_list::sort_rows(&mut self.rows, self.sort_key, self.sort_direction);
    }

    /// Navigate to the replay viewer for `game_id`, loading (or re-
    /// loading) its `ViewerState` from the DB + replay file. The load
    /// is synchronous — a `.slp` parse is typically sub-second, and
    /// we'd rather block the click than juggle another worker channel
    /// for now. If this gets painful for long replays we'll mirror the
    /// summary-worker pattern.
    fn open_viewer(&mut self, game_id: i32) {
        self.viewing_game_id = Some(game_id);
        self.viewer_state = None; // invalidate before reload
        // Stale "launched Slippi" / "rendered video" status from a
        // previous replay would be confusing next to a freshly-opened
        // viewer.
        self.last_slippi_launch = None;
        self.last_render_summary = None;
        self.page = Page::ReplayViewer;
        self.reload_viewer();
    }

    /// Shell out to Slippi Dolphin for the currently-viewed replay.
    /// Stores the outcome in `self.last_slippi_launch` for the viewer
    /// page to display.
    fn launch_in_slippi(&mut self) {
        // Grab the replay path out of the currently-cached viewer state.
        // If we're on the viewer page the state should always be
        // Some(Ok(_)) — but guard anyway.
        let replay_path = match &self.viewer_state {
            Some(Ok(s)) => s.replay_path.clone(),
            _ => {
                self.last_slippi_launch =
                    Some(Err("no replay loaded to launch".to_string()));
                return;
            }
        };
        let Some(path) = replay_path else {
            self.last_slippi_launch = Some(Err(slippi::SlippiLaunchError::NoReplayPath.to_string()));
            return;
        };

        let override_cmd = self
            .config
            .slippi_playback_command
            .as_deref()
            .filter(|s| !s.trim().is_empty());

        match slippi::launch_replay(&path, override_cmd) {
            Ok(()) => {
                self.last_slippi_launch = Some(Ok(()));
            }
            Err(e) => {
                self.last_slippi_launch = Some(Err(e.to_string()));
            }
        }
    }

    /// Populate `self.viewer_state` for the currently-viewed game.
    /// No-op if `viewing_game_id` is `None` or if the DB isn't open.
    fn reload_viewer(&mut self) {
        let Some(gid) = self.viewing_game_id else {
            return;
        };
        self.ensure_db();
        let Some(conn) = self.db_conn.as_mut() else {
            self.viewer_state = Some(Err(self
                .db_error
                .clone()
                .unwrap_or_else(|| "db not open".to_string())));
            return;
        };
        // Pass the user's player code through so `load_viewer` can flip
        // the scrub-bar palette to "you / opponent" when one of the
        // game's slots matches.
        let user_code = self.config.user_player_code.trim().to_string();
        let user_code = if user_code.is_empty() {
            None
        } else {
            Some(user_code)
        };
        match viewer::load_viewer(
            conn,
            gid,
            &mut self.analysis_cache,
            user_code.as_deref(),
        ) {
            Ok(s) => self.viewer_state = Some(Ok(s)),
            Err(e) => self.viewer_state = Some(Err(e.to_string())),
        }
    }

    /// Spawn a background scan of the configured replay folder. Returns
    /// immediately — the worker thread does the file walk + peppi parses
    /// off the UI thread, then sends back the count via [`IngestMsg`].
    /// [`Self::poll_ingest_worker`] drains the channel each frame.
    ///
    /// `diesel::SqliteConnection` is `!Send`, same constraint as the
    /// summary worker — the worker opens its own connection against
    /// the DB path. SQLite is happy with a second concurrent handle.
    fn ingest_replays(&mut self) {
        // Already in flight — don't double-spawn.
        if self.ingest_rx.is_some() {
            return;
        }

        let Some(replay_dir) = self.config.replay_dir.clone() else {
            self.last_ingest_summary = Some("No replay folder configured.".to_string());
            return;
        };

        let db_path = match self.config.effective_db_path() {
            Ok(p) => p,
            Err(e) => {
                self.last_ingest_summary = Some(format!("db path: {e}"));
                return;
            }
        };

        let (tx, rx) = mpsc::channel::<IngestMsg>();
        let ctx_for_thread = self.egui_ctx.clone();

        thread::spawn(move || {
            let msg = match stats_melee::open_database(&db_path) {
                Ok(mut conn) => match stats_melee::parse_new_replays(
                    &mut conn,
                    &replay_dir,
                    &db_path,
                ) {
                    Ok(n) => IngestMsg::Ok(n),
                    Err(e) => IngestMsg::Err(e.to_string()),
                },
                Err(e) => IngestMsg::Err(e.to_string()),
            };
            // Best-effort send + nudge eframe to repaint so the
            // status flips immediately rather than after a mouse
            // move.
            let _ = tx.send(msg);
            if let Some(ctx) = ctx_for_thread {
                ctx.request_repaint();
            }
        });

        self.ingest_rx = Some(rx);
        self.ingest_loading = true;
        self.last_ingest_summary = Some("Scanning replays…".to_string());
    }

    /// Drain any pending message from the ingest worker. Called at
    /// the top of every `update()` so the result lands on the frame
    /// it arrives. Same shape as [`Self::poll_summary_worker`] /
    /// [`Self::poll_render_worker`].
    fn poll_ingest_worker(&mut self) {
        let Some(rx) = self.ingest_rx.as_ref() else {
            return;
        };
        match rx.try_recv() {
            Ok(IngestMsg::Ok(n)) => {
                self.last_ingest_summary = Some(format!("Ingested {n} new replay(s)."));
                self.ingest_rx = None;
                self.ingest_loading = false;
                // Refresh the visible rows now that the DB is up
                // to date. Cheap — just re-runs the existing
                // load_rows query.
                self.reload_rows();
            }
            Ok(IngestMsg::Err(e)) => {
                self.last_ingest_summary = Some(format!("Ingest failed: {e}"));
                self.ingest_rx = None;
                self.ingest_loading = false;
            }
            Err(mpsc::TryRecvError::Empty) => {
                // Still scanning — leave state untouched.
            }
            Err(mpsc::TryRecvError::Disconnected) => {
                self.last_ingest_summary =
                    Some("Ingest worker exited without producing a result.".to_string());
                self.ingest_rx = None;
                self.ingest_loading = false;
            }
        }
    }

    // --- Panels ---------------------------------------------------------------

    /// Switch the active page, resetting any per-page confirm/transient
    /// state that shouldn't survive navigation.
    fn navigate_to(&mut self, page: Page) {
        if self.page == page {
            return;
        }
        // A stale "are you sure?" (Settings nuke) or per-row delete
        // confirm shouldn't linger when the user comes back later.
        self.nuke_confirm_pending = false;
        self.delete_confirm_game_id = None;
        self.last_delete_summary = None;
        self.page = page;
    }

    /// Top bar: wordmark + replay count on the left; search (library
    /// page only) and the settings gear on the right. Replaces the old
    /// left sidebar.
    fn render_top_bar(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::top("topbar").show(ctx, |ui| {
            ui.add_space(5.0);
            ui.horizontal(|ui| {
                ui.add_space(2.0);
                ui.label(egui::RichText::new("stats-melee").size(18.0).strong());
                if !self.rows.is_empty() {
                    ui.label(
                        egui::RichText::new(format!("· {} replays", self.rows.len()))
                            .color(egui::Color32::from_gray(130)),
                    );
                }

                // Right-aligned cluster: gear first (right_to_left lays
                // out from the right edge inward), then the search box.
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let on_settings = self.page == Page::Settings;
                    let gear = egui::Button::new(egui::RichText::new("⚙").size(17.0))
                        .min_size(egui::vec2(32.0, 28.0))
                        .fill(if on_settings {
                            ACCENT
                        } else {
                            egui::Color32::TRANSPARENT
                        });
                    if ui.add(gear).on_hover_text("Settings").clicked() {
                        // Gear toggles into Settings, or back out to the
                        // library if we're already there.
                        let target = if on_settings {
                            Page::ReplayLibrary
                        } else {
                            Page::Settings
                        };
                        self.navigate_to(target);
                    }

                    if self.page == Page::ReplayLibrary {
                        ui.add(
                            egui::TextEdit::singleline(&mut self.replay_search)
                                .hint_text("Search code / character / stage / date…")
                                .desired_width(240.0),
                        );
                    }
                });
            });
            ui.add_space(5.0);
        });
    }

    /// Floating Library / Analytics toggle anchored to the bottom-center
    /// of the window. Hidden on the drill-down viewer page, which has its
    /// own "Back to library" nav.
    fn render_view_toggle(&mut self, ctx: &egui::Context) {
        if self.page == Page::ReplayViewer {
            return;
        }
        egui::Area::new(egui::Id::new("view_toggle"))
            .anchor(egui::Align2::CENTER_BOTTOM, egui::vec2(0.0, -18.0))
            .show(ctx, |ui| {
                egui::Frame::none()
                    .fill(egui::Color32::from_rgb(0x14, 0x16, 0x1A))
                    .stroke(egui::Stroke::new(0.5, egui::Color32::from_gray(64)))
                    .rounding(egui::Rounding::same(999.0))
                    .inner_margin(egui::Margin::same(6.0))
                    .show(ui, |ui| {
                        let mut target = None;
                        ui.horizontal(|ui| {
                            if view_pill(ui, self.page, Page::ReplayLibrary, "Library") {
                                target = Some(Page::ReplayLibrary);
                            }
                            if view_pill(ui, self.page, Page::Analytics, "Analytics") {
                                target = Some(Page::Analytics);
                            }
                        });
                        if let Some(p) = target {
                            self.navigate_to(p);
                        }
                    });
            });
    }

    fn main_panel(&mut self, ui: &mut egui::Ui) {
        match self.page {
            Page::ReplayLibrary => self.page_replay_library(ui),
            Page::Analytics => self.page_analytics(ui),
            Page::Settings => self.page_settings(ui),
            Page::ReplayViewer => self.page_replay_viewer(ui),
        }
    }

    // --- Pages ----------------------------------------------------------------

    fn page_replay_library(&mut self, ui: &mut egui::Ui) {
        if self.config.replay_dir.is_none() {
            ui.label(
                "No replay folder configured. Pick one to start ingesting \
                 your replays.",
            );
            ui.add_space(4.0);
            if ui.button("Pick replay folder…").clicked() {
                self.pick_replay_dir();
            }
            return;
        }

        // Make sure the DB is open before we try to render rows.
        self.ensure_db();

        // Action bar. Scan button is disabled while a worker run is
        // in flight so we don't double-spawn (also enforced inside
        // ingest_replays as belt-and-suspenders).
        ui.horizontal(|ui| {
            if ui.button("Refresh list").clicked() {
                self.reload_rows();
            }
            let scan_btn = egui::Button::new("Scan for new replays");
            let resp = ui.add_enabled(!self.ingest_loading, scan_btn);
            let resp = if self.ingest_loading {
                resp.on_disabled_hover_text("A scan is already running")
            } else {
                resp.on_hover_text(
                    "Walk the configured replay folder and ingest any \
                     .slp files we haven't seen before (off the UI thread).",
                )
            };
            if resp.clicked() {
                self.ingest_replays();
            }
            // Live progress indicator. The spinner is the visible
            // signal; the status text is also surfaced separately
            // below the button row via `last_ingest_summary`.
            if self.ingest_loading {
                ui.spinner();
            }
            if let Some(dir) = &self.config.replay_dir {
                ui.add_space(12.0);
                ui.label(
                    egui::RichText::new(format!("from {}", dir.display()))
                        .small()
                        .color(egui::Color32::GRAY),
                );
            }
        });

        if let Some(summary) = &self.last_ingest_summary {
            ui.label(summary);
        }
        if let Some(err) = &self.db_error {
            ui.colored_label(egui::Color32::RED, format!("DB error: {err}"));
        }
        if let Some(err) = &self.rows_error {
            ui.colored_label(egui::Color32::RED, format!("Load error: {err}"));
        }

        ui.add_space(8.0);

        // Auto-load once on first entry to this page.
        if self.rows.is_empty() && self.rows_error.is_none() && self.db_conn.is_some() {
            self.reload_rows();
        }

        // The live search box lives in the top bar (bound to the same
        // `self.replay_search`); `render_replay_table` reads it to filter
        // rows. When a search is active, show a small "(N of M)" count
        // here above the table so the user knows the filter is on.
        let q = self.replay_search.trim();
        if !q.is_empty() {
            let total = self.rows.len();
            let shown = self.rows.iter().filter(|r| r.matches_search(q)).count();
            ui.label(
                egui::RichText::new(format!("Showing {shown} of {total} (filtered)"))
                    .small()
                    .color(egui::Color32::GRAY),
            );
            ui.add_space(4.0);
        }

        // Inline result line for the most recent per-row delete.
        // Sits between the search row and the table so it's visible
        // alongside the row that just got removed.
        if let Some(result) = &self.last_delete_summary {
            match result {
                Ok(gid) => {
                    ui.colored_label(
                        egui::Color32::from_rgb(90, 180, 100),
                        format!("✓ Deleted game #{gid}."),
                    );
                }
                Err(e) => {
                    ui.colored_label(egui::Color32::from_rgb(220, 80, 80), format!("⚠ {e}"));
                }
            }
            ui.add_space(4.0);
        }

        self.render_replay_table(ui);

        // Clearance so the last row isn't hidden behind the floating
        // bottom nav toggle.
        ui.add_space(64.0);
    }

    fn render_replay_table(&mut self, ui: &mut egui::Ui) {
        if self.rows.is_empty() {
            ui.label(
                egui::RichText::new("No replays ingested yet. Click \"Scan for new replays\".")
                    .italics()
                    .color(egui::Color32::GRAY),
            );
            return;
        }

        // Build the visible-rows index up front. The table body
        // closure needs random-access by row.index() into the
        // filtered set, so a dense Vec<usize> mapping table-position
        // → underlying-row index is the right shape. Empty-search
        // hot path skips the filter and uses a 0..N range so we
        // don't pay the per-row predicate cost.
        let q = self.replay_search.trim();
        let visible: Vec<usize> = if q.is_empty() {
            (0..self.rows.len()).collect()
        } else {
            self.rows
                .iter()
                .enumerate()
                .filter(|(_, r)| r.matches_search(q))
                .map(|(i, _)| i)
                .collect()
        };

        if visible.is_empty() {
            ui.label(
                egui::RichText::new(format!(
                    "No replays match \"{}\". Clear the search to see all rows.",
                    q
                ))
                .italics()
                .color(egui::Color32::GRAY),
            );
            return;
        }

        let user_code = self.config.user_player_code.trim().to_string();

        // Split disjoint field borrows up front: the table body closure
        // reads `rows` immutably while mutating the `icons` texture cache
        // (lazy-loads on first sight of each id). Borrowing through two
        // named locals — rather than `self.rows` / `self.icons` inside the
        // closure — keeps the borrow checker happy and lets the
        // post-table `self.set_sort(...)` / `self.open_viewer(...)` calls
        // reborrow `self` once these end.
        let rows = &self.rows;
        let icons = &mut self.icons;

        // Collect header clicks from inside the closure via a local —
        // egui header closures can't capture &mut self directly, and
        // calling self.set_sort immediately would double-borrow
        // TableBuilder's internal UI state.
        let mut clicked: Option<SortKey> = None;
        let current_key = self.sort_key;
        let current_dir = self.sort_direction;

        // Defer opening the viewer until after TableBuilder returns —
        // same reason as sort clicks above: can't borrow &mut self
        // inside egui's row closures.
        let mut view_clicked: Option<i32> = None;
        // Per-row delete state collected the same way:
        //   - `delete_arm_clicked`: row's "🗑" button was clicked from
        //     the disarmed state → flip into confirm mode.
        //   - `delete_confirm_clicked`: row's "Confirm?" button was
        //     clicked → execute the delete.
        //   - `delete_cancel_clicked`: dismiss the confirm without
        //     deleting.
        let mut delete_arm_clicked: Option<i32> = None;
        let mut delete_confirm_clicked: Option<i32> = None;
        let mut delete_cancel_clicked = false;
        let pending_delete = self.delete_confirm_game_id;

        // The outer `ScrollArea::both()` wrapping the central panel handles
        // both-axis overflow for us — no need for an inner horizontal
        // scroll area here. Columns use `initial()` (fixed natural width)
        // so the table has a defined size that can overflow into the
        // parent scroll area; `remainder()` would try to expand to fill
        // the parent, which inside an auto-shrink=false ScrollArea::both
        // is effectively infinite.
        TableBuilder::new(ui)
            .striped(true)
            .resizable(true)
            .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
            .column(Column::initial(56.0).at_least(48.0)) // Game id
            .column(Column::initial(48.0).at_least(40.0)) // Outcome
            .column(Column::initial(200.0).at_least(140.0)) // P1 (winner)
            .column(Column::initial(200.0).at_least(140.0)) // P2
            .column(Column::initial(140.0).at_least(120.0)) // Stage
            .column(Column::initial(72.0).at_least(60.0)) // Duration
            .column(Column::initial(140.0).at_least(110.0)) // Ingested
            .column(Column::initial(64.0).at_least(56.0)) // View button
            .column(Column::initial(72.0).at_least(56.0)) // Delete (icon-only; confirm uses two narrow buttons)
            .header(22.0, |mut header| {
                header.col(|ui| {
                    if sortable_header(ui, "ID", SortKey::GameId, current_key, current_dir) {
                        clicked = Some(SortKey::GameId);
                    }
                });
                header.col(|ui| {
                    if sortable_header(ui, "W/L", SortKey::Outcome, current_key, current_dir) {
                        clicked = Some(SortKey::Outcome);
                    }
                });
                header.col(|ui| {
                    // Winner/Loser columns aren't sortable — they're
                    // derived from per-slot data, and alphabetizing
                    // opponent codes isn't a particularly useful view.
                    ui.strong("Winner");
                });
                header.col(|ui| {
                    ui.strong("Loser");
                });
                header.col(|ui| {
                    if sortable_header(ui, "Stage", SortKey::Stage, current_key, current_dir) {
                        clicked = Some(SortKey::Stage);
                    }
                });
                header.col(|ui| {
                    if sortable_header(
                        ui,
                        "Duration",
                        SortKey::Duration,
                        current_key,
                        current_dir,
                    ) {
                        clicked = Some(SortKey::Duration);
                    }
                });
                header.col(|ui| {
                    if sortable_header(
                        ui,
                        "Ingested",
                        SortKey::IngestedAt,
                        current_key,
                        current_dir,
                    ) {
                        clicked = Some(SortKey::IngestedAt);
                    }
                });
                header.col(|ui| {
                    // No header label — the per-row "View" buttons are
                    // self-describing, and a column header here would
                    // read like just another sortable field.
                    ui.label("");
                });
                header.col(|ui| {
                    // Same rationale as the View column — the trash
                    // glyph is its own affordance.
                    ui.label("");
                });
            })
            .body(|body| {
                body.rows(22.0, visible.len(), |mut row| {
                    // `row.index()` is the *visible-table* row number
                    // (0..visible.len()); `visible[i]` maps back to
                    // the underlying row in `self.rows`.
                    let idx = visible[row.index()];
                    let r = &rows[idx];

                    row.col(|ui| {
                        ui.label(r.game_id.to_string());
                    });

                    row.col(|ui| match r.user_won {
                        Some(true) => {
                            ui.colored_label(egui::Color32::from_rgb(60, 180, 75), "W");
                        }
                        Some(false) => {
                            ui.colored_label(egui::Color32::from_rgb(220, 80, 80), "L");
                        }
                        None => {
                            ui.label("–");
                        }
                    });

                    row.col(|ui| render_slot_cell(ui, icons, r.slots[0].as_ref(), &user_code));
                    row.col(|ui| {
                        // "Loser" cell picks the next populated non-winner
                        // slot — for 1v1 that's always slot 1, for FFA it's
                        // best-effort (we just show 2nd place).
                        render_slot_cell(ui, icons, r.slots[1].as_ref(), &user_code);
                    });

                    row.col(|ui| {
                        ui.horizontal(|ui| {
                            crate::icons::stage_icon(ui, icons, r.stage_id, 18.0);
                            ui.add_space(5.0);
                            ui.label(r.stage_name());
                        });
                    });

                    row.col(|ui| {
                        ui.label(r.duration_display());
                    });

                    row.col(|ui| {
                        // Show "YYYY-MM-DD" — full timestamp is overkill for
                        // the table and burns horizontal space. Hover tip
                        // exposes the full UTC timestamp for the curious.
                        let short: &str = r
                            .ingested_at
                            .split_whitespace()
                            .next()
                            .unwrap_or(r.ingested_at.as_str());
                        ui.label(short).on_hover_text(&r.ingested_at);
                    });

                    row.col(|ui| {
                        if ui
                            .small_button("View")
                            .on_hover_text("Open this replay in the viewer")
                            .clicked()
                        {
                            view_clicked = Some(r.game_id);
                        }
                    });

                    row.col(|ui| {
                        // Disarmed state: small trash glyph. Armed
                        // state (when this row is `pending_delete`):
                        // red "Confirm?" + "Cancel" pair. Pattern
                        // mirrors the all-replays nuke button in
                        // Settings.
                        if pending_delete == Some(r.game_id) {
                            let confirm = egui::Button::new(
                                egui::RichText::new("Delete?")
                                    .color(egui::Color32::WHITE)
                                    .small(),
                            )
                            .fill(egui::Color32::from_rgb(180, 40, 40));
                            if ui
                                .add(confirm)
                                .on_hover_text("Click to permanently delete")
                                .clicked()
                            {
                                delete_confirm_clicked = Some(r.game_id);
                            }
                            if ui.small_button("✕").on_hover_text("Cancel").clicked() {
                                delete_cancel_clicked = true;
                            }
                        } else if ui
                            .small_button("🗑")
                            .on_hover_text(
                                "Delete this replay's DB rows. The .slp file on \
                                 disk is not touched.",
                            )
                            .clicked()
                        {
                            delete_arm_clicked = Some(r.game_id);
                        }
                    });
                });
            });

        if let Some(key) = clicked {
            self.set_sort(key);
        }
        if let Some(gid) = view_clicked {
            self.open_viewer(gid);
        }
        if delete_cancel_clicked {
            self.delete_confirm_game_id = None;
        }
        if let Some(gid) = delete_arm_clicked {
            // Arming this row also clears any stale confirm on
            // another row — only one row can be armed at a time.
            self.delete_confirm_game_id = Some(gid);
            // Stale "deleted X" status from a previous click would
            // be confusing now that we're aiming at a different row.
            self.last_delete_summary = None;
        }
        if let Some(gid) = delete_confirm_clicked {
            self.delete_replay(gid);
        }
    }

    fn page_analytics(&mut self, ui: &mut egui::Ui) {
        let code = self.config.user_player_code.trim().to_string();
        if code.is_empty() {
            ui.label(
                egui::RichText::new(
                    "Set your player code in Settings to see a per-code \
                     statistical summary.",
                )
                .italics()
                .color(egui::Color32::GRAY),
            );
            if ui.button("Go to Settings").clicked() {
                self.page = Page::Settings;
            }
            return;
        }

        // Make sure the DB is open before we try to compute the summary.
        self.ensure_db();

        // Action bar — Refresh + the active code, like before.
        ui.horizontal(|ui| {
            if ui.button("Refresh summary").clicked() {
                // Force a reload regardless of cache key — useful after a
                // background ingest dropped new rows in the same session.
                self.summary_for = None;
                self.reload_summary();
            }
            ui.add_space(12.0);
            ui.label(
                egui::RichText::new(format!("for {code}"))
                    .small()
                    .color(egui::Color32::GRAY),
            );
        });

        // Filter row — Character / Stage selectors. Each ComboBox surfaces an
        // "Any" entry plus the legal-pool stages / playable characters; users
        // who only ever play tournament-legal stages don't have to hunt past
        // unplayable entries. Selectors are render-only here — the auto-load
        // check below picks up any change because `summary_for` no longer
        // matches the new key.
        ui.horizontal(|ui| {
            ui.label("Character:");
            egui::ComboBox::from_id_salt("analytics_character_combo")
                .selected_text(character_label(self.analytics_character_id))
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.analytics_character_id, None, "Any");
                    // Slots 0..=26 are the playable cast (Mario through Roy);
                    // 27+ are CPU/boss entries that won't show up in real
                    // matches anyway.
                    for cid in 0..=26 {
                        let label = CHARACTERS[cid as usize];
                        ui.selectable_value(
                            &mut self.analytics_character_id,
                            Some(cid),
                            label,
                        );
                    }
                });

            ui.add_space(12.0);
            ui.label("Stage:");
            egui::ComboBox::from_id_salt("analytics_stage_combo")
                .selected_text(stage_label(self.analytics_stage_id))
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.analytics_stage_id, None, "Any");
                    // Tournament-legal pool — the only stages a typical user
                    // will have multiple games on. Casual stages can join via
                    // a future "All stages" toggle.
                    for sid in [2, 3, 8, 28, 31, 32] {
                        let label = STAGES[sid as usize];
                        ui.selectable_value(&mut self.analytics_stage_id, Some(sid), label);
                    }
                });
        });

        if let Some(err) = &self.db_error {
            ui.colored_label(egui::Color32::RED, format!("DB error: {err}"));
        }
        if let Some(err) = &self.summary_error {
            ui.colored_label(egui::Color32::RED, format!("Summary error: {err}"));
        }

        ui.add_space(8.0);

        // Auto-load on first entry to this page, after a user-code change,
        // or after the user flips a selector. The cache key bundles all
        // three so any of them invalidates correctly.
        let target_key = SummaryKey {
            code: code.clone(),
            character_id: self.analytics_character_id,
            stage_id: self.analytics_stage_id,
        };
        let needs_load = self.summary_error.is_none()
            && self.db_conn.is_some()
            && !self.summary_loading
            && self.summary_for.as_ref() != Some(&target_key);
        if needs_load {
            self.reload_summary();
        }

        if self.summary_loading {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(
                    egui::RichText::new("Computing summary…")
                        .italics()
                        .color(egui::Color32::GRAY),
                );
            });
        } else if let Some(summary) = self.summary.clone() {
            // Pass the active character filter through so the kill-moves
            // section can character-gate itself — see the long comment in
            // `render_player_summary` for why the cross-character table
            // would be misleading.
            self.render_player_summary(ui, &summary, self.analytics_character_id);
        } else if self.summary_error.is_none() {
            ui.label(
                egui::RichText::new("Loading summary…")
                    .italics()
                    .color(egui::Color32::GRAY),
            );
        }

        // Clearance for the floating bottom nav toggle.
        ui.add_space(64.0);
    }

    /// Kick off a background recompute of the summary for the current
    /// `(user_player_code, character_id, stage_id)`. Cheap — the actual
    /// work happens on a worker thread so eframe's UI loop keeps running.
    /// Results land back via [`poll_summary_worker`].
    ///
    /// `diesel::SqliteConnection` is `!Send`, so we can't hand the one
    /// owned by `self` to the worker. We give the worker its own path
    /// and let it open a second connection — SQLite is perfectly happy
    /// with a second read-only-ish handle.
    fn reload_summary(&mut self) {
        let code = self.config.user_player_code.trim().to_string();
        if code.is_empty() {
            self.summary = None;
            self.summary_error = None;
            self.summary_for = None;
            self.summary_rx = None;
            self.summary_loading = false;
            return;
        }

        let key = SummaryKey {
            code: code.clone(),
            character_id: self.analytics_character_id,
            stage_id: self.analytics_stage_id,
        };

        let db_path = match self.config.effective_db_path() {
            Ok(p) => p,
            Err(e) => {
                self.summary = None;
                self.summary_error = Some(e.to_string());
                self.summary_for = Some(key);
                return;
            }
        };

        let (tx, rx) = mpsc::channel::<SummaryMsg>();
        let ctx_for_thread = self.egui_ctx.clone();
        let code_for_thread = code.clone();
        let filter_for_thread = key.filter();

        thread::spawn(move || {
            let msg = match stats_melee::open_database(&db_path) {
                Ok(mut conn) => match stats_melee::player_summary_filtered(
                    &mut conn,
                    &code_for_thread,
                    &filter_for_thread,
                ) {
                    Ok(s) => SummaryMsg::Ok(s),
                    Err(e) => SummaryMsg::Err(e.to_string()),
                },
                Err(e) => SummaryMsg::Err(e.to_string()),
            };
            // Best-effort send; if the receiver is gone the user already
            // navigated away / triggered another reload, and we just drop.
            let _ = tx.send(msg);
            // Nudge eframe to repaint so the result appears without the
            // user having to wiggle the mouse.
            if let Some(ctx) = ctx_for_thread {
                ctx.request_repaint();
            }
        });

        self.summary_rx = Some(rx);
        self.summary_loading = true;
        self.summary_for = Some(key);
        self.summary_error = None;
    }

    /// Drain any pending message from the render worker. Mirrors
    /// [`Self::poll_summary_worker`] — called at the top of every
    /// `update()` so progress + done messages land on the frame they
    /// arrive.
    fn poll_render_worker(&mut self) {
        if self.render_rx.is_none() {
            return;
        }
        // Drain every available message in one pass — Progress events
        // pile up faster than 60fps when Dolphin is spamming them.
        loop {
            // Re-borrow each iteration so the `Done` branch can take
            // `&mut self` for the cache finalize call.
            let msg = match self.render_rx.as_ref().map(|rx| rx.try_recv()) {
                Some(Ok(m)) => m,
                Some(Err(mpsc::TryRecvError::Empty)) => break,
                Some(Err(mpsc::TryRecvError::Disconnected)) => {
                    self.render_rx = None;
                    self.render_in_flight_hash = None;
                    self.render_status = None;
                    self.last_render_summary = Some(Err(
                        "render worker exited without producing a video".to_string(),
                    ));
                    break;
                }
                None => break,
            };
            match msg {
                RenderMsg::Progress(s) => {
                    self.render_status = Some(s);
                }
                RenderMsg::Done(result) => {
                    let in_flight = self.render_in_flight_hash.take();
                    self.render_rx = None;
                    self.render_status = None;
                    if result.is_ok() {
                        // Tell the video cache the .mp4 just landed
                        // so its eviction sweep can run. The hash is
                        // exactly what we handed the worker at
                        // start_render time.
                        if let Some(hash) = in_flight {
                            if let Err(e) = self.video_cache.finalize(&hash) {
                                eprintln!("video cache finalize failed: {e}");
                            }
                        }
                    }
                    self.last_render_summary = Some(result);
                    break;
                }
            }
        }
    }

    /// Start a fresh render for the currently-viewed replay. Looks up
    /// the prerequisites (Melee ISO, Dolphin binary, content hash)
    /// and surfaces a clear error message via `last_render_summary`
    /// if anything is missing.
    fn start_render(&mut self) {
        // Already in flight — don't double-spawn.
        if self.render_rx.is_some() {
            return;
        }
        // Need a viewer state with a usable replay path.
        let state = match self.viewer_state.as_ref() {
            Some(Ok(s)) => s,
            _ => {
                self.last_render_summary =
                    Some(Err("no replay loaded to render".to_string()));
                return;
            }
        };
        let Some(replay_path) = state.replay_path.clone() else {
            self.last_render_summary = Some(Err(
                "this replay has no path on disk; re-ingest to enable rendering".to_string(),
            ));
            return;
        };
        let game_id = state.game_id;

        // Pull the .slp's content_hash off the row. Without it the
        // video cache has no stable key to write under, so we'd have
        // to render every time — better to surface "re-ingest" early.
        let content_hash = match self.fetch_content_hash(game_id) {
            Some(h) => h,
            None => {
                self.last_render_summary = Some(Err(
                    "this replay was ingested before content_hash existed; \
                     re-ingest to enable rendering"
                        .to_string(),
                ));
                return;
            }
        };

        // Resolve the prerequisite binaries / iso path.
        let Some(iso) = self.config.melee_iso_path.clone() else {
            self.last_render_summary = Some(Err(
                "set the Melee ISO path in Settings to enable rendering".to_string(),
            ));
            return;
        };
        if !iso.is_file() {
            self.last_render_summary = Some(Err(format!(
                "Melee ISO not found at {}",
                iso.display()
            )));
            return;
        }
        let Some(dolphin_binary) = resolve_vanilla_dolphin_binary(&self.config) else {
            self.last_render_summary = Some(Err(
                "couldn't find vanilla Dolphin — set the Dolphin binary path in Settings"
                    .to_string(),
            ));
            return;
        };

        let req = RenderRequest {
            slp_path: PathBuf::from(replay_path),
            slp_hash: content_hash.clone(),
            melee_iso: iso,
            dolphin_binary,
            ffmpeg_binary: self.config.effective_ffmpeg_command(),
            mp4_out: self.video_cache.path_for_write(&content_hash),
        };

        self.render_in_flight_hash = Some(content_hash);
        self.render_rx = Some(render_worker::spawn_render(req, self.egui_ctx.clone()));
        self.render_status = Some("Starting render".to_string());
        self.last_render_summary = None;
    }

    /// Path to the cached MP4 for the currently-viewed replay, if
    /// the video cache has it. Looks up by content_hash off the
    /// current viewer state's game id. Returns `None` when:
    /// - no replay is being viewed,
    /// - the row has no content_hash (legacy, pre-Track-11d), or
    /// - the cache doesn't have a matching `.mp4` on disk.
    fn cached_video_path(&mut self) -> Option<PathBuf> {
        let game_id = match self.viewer_state.as_ref() {
            Some(Ok(s)) => s.game_id,
            _ => return None,
        };
        let hash = self.fetch_content_hash(game_id)?;
        self.video_cache.lookup(&hash)
    }

    /// Pull the .slp content_hash for a game id off the DB. Helper
    /// for [`Self::start_render`] — separated so the borrow checker
    /// is happy about us holding `self.db_conn` mutably here while the
    /// rest of `start_render` is reading other fields.
    fn fetch_content_hash(&mut self, game_id: i32) -> Option<String> {
        use diesel::prelude::*;
        use stats_melee::schema::game::dsl as g;

        self.ensure_db();
        let conn = self.db_conn.as_mut()?;
        g::game
            .filter(g::id.eq(game_id))
            .select(g::content_hash)
            .first::<Option<String>>(conn)
            .ok()
            .flatten()
    }

    /// Open the cached MP4 for the currently-viewed replay in the OS
    /// default video player. Caller is responsible for having
    /// confirmed cache.lookup() returned `Some` first.
    fn open_video(&mut self, video_path: &Path) {
        if let Err(e) = open_in_os_player(video_path) {
            self.last_render_summary = Some(Err(e.to_string()));
        }
    }

    /// Drain any pending message from the summary worker. Called at the
    /// top of every `update()` so results show up the frame they arrive.
    fn poll_summary_worker(&mut self) {
        let Some(rx) = self.summary_rx.as_ref() else {
            return;
        };
        match rx.try_recv() {
            Ok(SummaryMsg::Ok(s)) => {
                self.summary = Some(s);
                self.summary_error = None;
                self.summary_loading = false;
                self.summary_rx = None;
            }
            Ok(SummaryMsg::Err(e)) => {
                self.summary = None;
                self.summary_error = Some(e);
                self.summary_loading = false;
                self.summary_rx = None;
            }
            Err(mpsc::TryRecvError::Empty) => {
                // Still computing — leave state untouched.
            }
            Err(mpsc::TryRecvError::Disconnected) => {
                // Worker crashed without sending. Surface something rather
                // than spinning forever.
                self.summary_error =
                    Some("summary worker exited without producing a result".to_string());
                self.summary_loading = false;
                self.summary_rx = None;
            }
        }
    }

    fn render_player_summary(
        &mut self,
        ui: &mut egui::Ui,
        s: &PlayerSummary,
        character_filter: Option<i32>,
    ) {
        if s.games_played == 0 {
            ui.label(
                egui::RichText::new(format!(
                    "No games recorded yet for {}. Ingest some replays first.",
                    s.code
                ))
                .italics()
                .color(egui::Color32::GRAY),
            );
            return;
        }

        // Header: the filtered character's icon (when active) next to the
        // player code + game count.
        ui.horizontal(|ui| {
            if let Some(cid) = character_filter {
                crate::icons::character_icon(ui, &mut self.icons, cid, 36.0);
                ui.add_space(10.0);
            }
            ui.vertical(|ui| {
                ui.label(egui::RichText::new(&s.code).size(22.0).strong());
                ui.label(
                    egui::RichText::new(format!("{} games played", s.games_played))
                        .color(egui::Color32::from_gray(140)),
                );
            });
        });
        ui.add_space(14.0);

        // Headline metric cards.
        ui.horizontal_wrapped(|ui| {
            metric_card(ui, "Avg placement", &fmt_opt_f64(s.avg_placement, 2));
            metric_card(ui, "APM", &fmt_opt_f64(s.avg_apm, 0));
            metric_card(ui, "L-cancel", &fmt_opt_percent(s.l_cancel_rate));
            metric_card(ui, "Stocks left", &fmt_opt_f64(s.avg_stocks_remaining, 1));
        });
        ui.add_space(10.0);

        // Streak banner, tinted green for a win run / red for a loss run.
        ui.horizontal(|ui| {
            let (label, color) = match s.streaks.current {
                c if c > 0 => (
                    format!("{c}-game win streak"),
                    egui::Color32::from_rgb(90, 190, 110),
                ),
                c if c < 0 => (
                    format!("{}-game loss streak", -c),
                    egui::Color32::from_rgb(220, 95, 95),
                ),
                _ => (
                    "No active streak".to_string(),
                    egui::Color32::from_gray(150),
                ),
            };
            egui::Frame::none()
                .fill(color.linear_multiply(0.18))
                .stroke(egui::Stroke::new(1.0, color.linear_multiply(0.7)))
                .rounding(egui::Rounding::same(8.0))
                .inner_margin(egui::Margin::symmetric(14.0, 8.0))
                .show(ui, |ui| {
                    ui.label(egui::RichText::new(label).color(color).strong());
                });
            ui.add_space(8.0);
            ui.label(
                egui::RichText::new(format!(
                    "Best: {} W · {} L",
                    s.streaks.longest_win, s.streaks.longest_loss
                ))
                .color(egui::Color32::from_gray(140)),
            );
        });
        ui.add_space(12.0);

        // L-cancel rate as a progress bar for an at-a-glance read.
        if let Some(rate) = s.l_cancel_rate {
            ui.label(
                egui::RichText::new("L-cancel success")
                    .size(12.0)
                    .color(egui::Color32::from_gray(150)),
            );
            ui.add_space(2.0);
            ui.add(
                egui::ProgressBar::new(rate as f32)
                    .desired_width(320.0)
                    .text(format!("{:.0}%", rate * 100.0)),
            );
            ui.add_space(12.0);
        }

        // Secondary metrics.
        ui.horizontal_wrapped(|ui| {
            metric_card(ui, "Stocks taken (1v1)", &fmt_opt_f64(s.avg_stocks_taken, 2));
            metric_card(ui, "Punish length", &fmt_opt_f64(s.avg_punish_length, 2));
            metric_card(ui, "Openings / kill", &fmt_opt_f64(s.openings_per_kill, 2));
        });

        ui.add_space(16.0);

        // Kill-moves section is character-gated. Without a character filter
        // the rolled-up distribution mixes attack ids that mean different
        // moves for different characters (id 23 is Falcon Punch for Falcon
        // and Marth's Counter for Marth — putting them in one row would be
        // worse than no signal). With a character filter active, the
        // attack-id table is character-consistent and we can resolve the
        // universal-id band (jab / aerials / smashes / throws) through
        // [`stats_melee::gamedata::attack_display_name`]. The character-
        // specific band (23..49) renders as "attack #N" until Track 8d's
        // follow-up wires character-specific names.
        match character_filter {
            // No placeholder when there's no character filter — the section
            // simply doesn't render. Cross-character attack ids would mean
            // different moves for different characters, so the rolled-up
            // view would be misleading; staying silent is the right call.
            None => {}
            Some(_) => {
                ui.strong("Top kill moves");
                ui.add_space(2.0);
                if s.top_kill_moves.is_empty() {
                    ui.label(
                        egui::RichText::new("(no kill moves recorded yet)")
                            .italics()
                            .color(egui::Color32::GRAY),
                    );
                } else {
                    // (No explicit id_source/id_salt — egui_extras 0.29 derives one
                    // from widget position, and the two tables in this app never
                    // render on the same frame.)
                    TableBuilder::new(ui)
                        .striped(true)
                        .column(Column::auto().at_least(140.0))
                        .column(Column::auto().at_least(60.0))
                        .header(20.0, |mut h| {
                            h.col(|ui| {
                                ui.strong("Move");
                            });
                            h.col(|ui| {
                                ui.strong("Count");
                            });
                        })
                        .body(|body| {
                            body.rows(20.0, s.top_kill_moves.len(), |mut row| {
                                let i = row.index();
                                let (attack_id, count) = s.top_kill_moves[i];
                                row.col(|ui| {
                                    ui.label(
                                        stats_melee::gamedata::attack_display_name(attack_id),
                                    );
                                });
                                row.col(|ui| {
                                    ui.label(count.to_string());
                                });
                            });
                        });
                }
            }
        }
    }

    fn page_replay_viewer(&mut self, ui: &mut egui::Ui) {
        // Defer navigation / reload / launch actions until after the
        // borrow of self.viewer_state ends — egui's closure pattern
        // means we'd otherwise double-borrow self.
        let mut back_clicked = false;
        let mut reload_clicked = false;
        let mut launch_clicked = false;
        let mut render_clicked = false;
        let mut open_video_clicked: Option<PathBuf> = None;

        // Whether the currently-loaded viewer has a usable replay_path —
        // used to enable/disable the "Open in Slippi" button below.
        let can_launch = matches!(
            &self.viewer_state,
            Some(Ok(s)) if s.replay_path.as_deref().map(|p| !p.is_empty()).unwrap_or(false)
        );

        // Render-button gating: we need a viewer state, an ISO path,
        // and a non-empty content_hash on the current viewer's
        // game row. The hash check is what the actual `start_render`
        // path enforces — pre-checking here so the button is greyed
        // out instead of silently failing on click.
        let render_in_flight = self.render_rx.is_some();
        let iso_set = self.config.melee_iso_path.is_some();
        let cached_video = self.cached_video_path();
        let can_render = !render_in_flight
            && iso_set
            && matches!(
                &self.viewer_state,
                Some(Ok(s)) if s.replay_path.as_deref().map(|p| !p.is_empty()).unwrap_or(false)
            );

        // Nav bar.
        ui.horizontal(|ui| {
            if ui.button("← Back to library").clicked() {
                back_clicked = true;
            }
            if ui.button("Reload").clicked() {
                reload_clicked = true;
            }
            // "Open in Slippi" sits in the nav bar so it's always in the
            // same spot regardless of scroll position. Disabled-state
            // hover text spells out why when the row has no path.
            let open_btn = egui::Button::new("▶ Open in Slippi");
            let resp = ui.add_enabled(can_launch, open_btn);
            let resp = if can_launch {
                resp.on_hover_text("Launches the .slp in your local Slippi Dolphin install")
            } else {
                resp.on_disabled_hover_text(
                    "No replay file path on this game row — re-ingest to enable",
                )
            };
            if resp.clicked() {
                launch_clicked = true;
            }

            // "Render video" + "Open video" — gated behind the
            // `RENDER_VIDEO_FEATURE_ENABLED` flag while Track 10 is
            // parked (see the const's docstring + TODO.txt). When
            // off, none of these branches render anything and the
            // worker-state fields just sit idle on `self`.
            if RENDER_VIDEO_FEATURE_ENABLED {
                if let Some(path) = cached_video.clone() {
                    let open_btn = egui::Button::new("🎞 Open video");
                    if ui
                        .add(open_btn)
                        .on_hover_text("Opens the cached MP4 in your default video player")
                        .clicked()
                    {
                        open_video_clicked = Some(path);
                    }
                    let rerender_btn = egui::Button::new("↻ Re-render");
                    let resp = ui.add_enabled(can_render, rerender_btn);
                    let resp = if !iso_set {
                        resp.on_disabled_hover_text(
                            "Set the Melee ISO path in Settings to enable rendering",
                        )
                    } else if render_in_flight {
                        resp.on_disabled_hover_text("Render already in progress")
                    } else {
                        resp.on_hover_text("Re-render this replay (replaces the cached MP4)")
                    };
                    if resp.clicked() {
                        render_clicked = true;
                    }
                } else {
                    let render_btn = egui::Button::new("🎞 Render video");
                    let resp = ui.add_enabled(can_render, render_btn);
                    let resp = if !iso_set {
                        resp.on_disabled_hover_text(
                            "Set the Melee ISO path in Settings to enable rendering",
                        )
                    } else if render_in_flight {
                        resp.on_disabled_hover_text("Render already in progress")
                    } else if !can_launch {
                        resp.on_disabled_hover_text(
                            "No replay file path on this game row — re-ingest to enable",
                        )
                    } else {
                        resp.on_hover_text(
                            "Renders this replay headlessly via Slippi Dolphin + ffmpeg \
                             (a few seconds for a 5-minute replay)",
                        )
                    };
                    if resp.clicked() {
                        render_clicked = true;
                    }
                }
            }

            if let Some(gid) = self.viewing_game_id {
                ui.add_space(12.0);
                ui.label(
                    egui::RichText::new(format!("Game #{gid}"))
                        .small()
                        .color(egui::Color32::GRAY),
                );
            }
        });

        // Slippi launch status line, right under the nav bar so it sits
        // next to the button that triggered it.
        if let Some(result) = &self.last_slippi_launch {
            ui.add_space(4.0);
            match result {
                Ok(()) => {
                    ui.colored_label(
                        egui::Color32::from_rgb(90, 180, 100),
                        "✓ Launched in Slippi — check your dock.",
                    );
                }
                Err(e) => {
                    ui.colored_label(egui::Color32::from_rgb(220, 80, 80), format!("⚠ {e}"));
                }
            }
        }

        // Render progress / completion status — gated alongside the
        // render buttons. `render_status` and `last_render_summary`
        // can only ever be `Some` when the feature is enabled (the
        // worker is the only thing that sets them), but we still
        // gate the UI so a stale `Some` from a previous build doesn't
        // surface unexpectedly.
        if RENDER_VIDEO_FEATURE_ENABLED {
            if let Some(status) = &self.render_status {
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(
                        egui::RichText::new(status.as_str())
                            .italics()
                            .color(egui::Color32::GRAY),
                    );
                });
            } else if let Some(result) = &self.last_render_summary {
                ui.add_space(4.0);
                match result {
                    Ok(path) => {
                        ui.colored_label(
                            egui::Color32::from_rgb(90, 180, 100),
                            format!("✓ Rendered: {}", path.display()),
                        );
                    }
                    Err(e) => {
                        ui.colored_label(
                            egui::Color32::from_rgb(220, 80, 80),
                            format!("⚠ {e}"),
                        );
                    }
                }
            }
        }

        ui.add_space(8.0);

        if let Some(err) = &self.db_error {
            ui.colored_label(egui::Color32::RED, format!("DB error: {err}"));
        }

        match &self.viewer_state {
            Some(Ok(state)) => {
                viewer::render_viewer(ui, state);
            }
            Some(Err(e)) => {
                ui.colored_label(
                    egui::Color32::from_rgb(220, 80, 80),
                    format!("Couldn't load replay: {e}"),
                );
            }
            None => {
                ui.label(
                    egui::RichText::new("No replay selected.")
                        .italics()
                        .color(egui::Color32::GRAY),
                );
            }
        }

        if back_clicked {
            self.page = Page::ReplayLibrary;
            self.viewing_game_id = None;
            self.viewer_state = None;
            self.last_slippi_launch = None;
            self.last_render_summary = None;
        }
        if render_clicked {
            self.start_render();
        }
        if let Some(p) = open_video_clicked {
            self.open_video(&p);
        }
        if reload_clicked {
            self.reload_viewer();
        }
        if launch_clicked {
            self.launch_in_slippi();
        }
    }

    fn page_settings(&mut self, ui: &mut egui::Ui) {
        // Deferred actions from inside the Grid closure — we can't call
        // &mut self methods (save_config, pick_slippi_binary) directly
        // while the grid has a &mut borrow of the UI.
        let mut slippi_binary_save_pending = false;
        let mut slippi_binary_pick_clicked = false;
        let mut melee_iso_save_pending = false;
        let mut melee_iso_pick_clicked = false;
        let mut ffmpeg_save_pending = false;

        egui::Grid::new("settings_grid")
            .num_columns(2)
            .spacing([12.0, 8.0])
            .show(ui, |ui| {
                ui.label("Replay folder");
                ui.horizontal(|ui| {
                    let display = match &self.config.replay_dir {
                        Some(p) => p.display().to_string(),
                        None => "(not set)".to_string(),
                    };
                    ui.label(display);
                    if ui.button("Change…").clicked() {
                        self.pick_replay_dir();
                    }
                });
                ui.end_row();

                ui.label("Your player code");
                let resp = ui.text_edit_singleline(&mut self.config.user_player_code);
                if resp.lost_focus() {
                    self.save_config();
                    // New code filter invalidates the cached row list
                    // and the cached PlayerSummary. Also drop any in-flight
                    // summary worker — its result would be for the old code
                    // and `summary_loading` would keep the spinner stuck if
                    // we didn't reset it.
                    self.rows.clear();
                    self.summary = None;
                    self.summary_error = None;
                    self.summary_for = None;
                    self.summary_rx = None;
                    self.summary_loading = false;
                    // The character/stage selectors are stored as raw ids,
                    // so they survive a code change — but if the new code
                    // never played the previously-picked character, the
                    // summary will just come back empty for that filter,
                    // which is honest behavior. Reset to "Any" on code
                    // change anyway so the user sees data immediately.
                    self.analytics_character_id = None;
                    self.analytics_stage_id = None;
                }
                ui.end_row();

                ui.label("Database path");
                ui.label(
                    self.config
                        .effective_db_path()
                        .map(|p| p.display().to_string())
                        .unwrap_or_else(|e| format!("(unresolved: {e})")),
                );
                ui.end_row();

                // Slippi playback binary — optional override for the
                // viewer's "Open in Slippi" button. Empty / blank means
                // fall back to the platform default (see crate::slippi).
                ui.label("Slippi playback binary").on_hover_text(
                    "Point this at your Slippi Dolphin install — the `.app` \
                     bundle itself works on macOS; the launcher resolves to \
                     the inner binary automatically. Leave empty to use the \
                     platform default (macOS: /Applications/Slippi Dolphin.app; \
                     Linux/Windows: must be set).",
                );
                ui.horizontal(|ui| {
                    // Bind the text edit to a working copy of the Option<String>
                    // so typing an empty string collapses cleanly to `None`.
                    let mut buf = self
                        .config
                        .slippi_playback_command
                        .clone()
                        .unwrap_or_default();
                    let resp = ui.add(
                        egui::TextEdit::singleline(&mut buf)
                            .hint_text("(platform default)")
                            .desired_width(320.0),
                    );
                    if resp.changed() {
                        let trimmed = buf.trim();
                        self.config.slippi_playback_command = if trimmed.is_empty() {
                            None
                        } else {
                            Some(buf.clone())
                        };
                    }
                    if resp.lost_focus() {
                        slippi_binary_save_pending = true;
                    }
                    if ui.button("Browse…").clicked() {
                        slippi_binary_pick_clicked = true;
                    }
                    if !buf.is_empty() && ui.button("Clear").clicked() {
                        self.config.slippi_playback_command = None;
                        slippi_binary_save_pending = true;
                    }
                });
                ui.end_row();

                // Melee ISO + ffmpeg rows — only relevant for the
                // in-house render-video pipeline, which is parked
                // (RENDER_VIDEO_FEATURE_ENABLED = false) while we ship
                // the Slippi-launcher production build. The config
                // fields stay so a user who already set them keeps
                // their values for when the feature comes back.
                if RENDER_VIDEO_FEATURE_ENABLED {
                    ui.label("Melee ISO").on_hover_text(
                        "Path to your Super Smash Bros. Melee 1.02 NTSC ISO. \
                         Required for the headless render pipeline (the \
                         'Render video' button on the viewer page). If you \
                         don't render videos, this can stay unset.",
                    );
                    ui.horizontal(|ui| {
                        let display = match &self.config.melee_iso_path {
                            Some(p) => p.display().to_string(),
                            None => "(not set)".to_string(),
                        };
                        ui.label(display);
                        if ui.button("Browse…").clicked() {
                            melee_iso_pick_clicked = true;
                        }
                        if self.config.melee_iso_path.is_some()
                            && ui.button("Clear").clicked()
                        {
                            self.config.melee_iso_path = None;
                            melee_iso_save_pending = true;
                        }
                    });
                    ui.end_row();

                    ui.label("ffmpeg binary").on_hover_text(
                        "Override path to ffmpeg. Leave empty to use \
                         whatever's on PATH (the default `brew install \
                         ffmpeg` setup works without an override).",
                    );
                    ui.horizontal(|ui| {
                        let mut buf = self
                            .config
                            .ffmpeg_command
                            .clone()
                            .unwrap_or_default();
                        let resp = ui.add(
                            egui::TextEdit::singleline(&mut buf)
                                .hint_text("(use PATH)")
                                .desired_width(320.0),
                        );
                        if resp.changed() {
                            let trimmed = buf.trim();
                            self.config.ffmpeg_command = if trimmed.is_empty() {
                                None
                            } else {
                                Some(buf.clone())
                            };
                        }
                        if resp.lost_focus() {
                            ffmpeg_save_pending = true;
                        }
                        if !buf.is_empty() && ui.button("Clear").clicked() {
                            self.config.ffmpeg_command = None;
                            ffmpeg_save_pending = true;
                        }
                    });
                    ui.end_row();
                }
            });

        if slippi_binary_pick_clicked {
            self.pick_slippi_binary();
        }
        if slippi_binary_save_pending {
            self.save_config();
        }
        if melee_iso_pick_clicked {
            self.pick_melee_iso();
        }
        if melee_iso_save_pending || ffmpeg_save_pending {
            self.save_config();
        }

        ui.add_space(16.0);
        if let Some(err) = &self.last_config_error {
            ui.colored_label(egui::Color32::RED, format!("Config save failed: {err}"));
        }

        // "Delete all replays" sits inline at the bottom of Settings.
        // Red styling marks it as destructive without needing a whole
        // "Danger zone" subheading — the red + two-step confirm pattern
        // carries that meaning on its own. .slp files on disk are never
        // touched; this just wipes DB rows.
        ui.add_space(16.0);

        if self.nuke_confirm_pending {
            // Two-step confirm — Confirm fires the delete, Cancel bails.
            ui.horizontal(|ui| {
                let confirm = egui::Button::new(
                    egui::RichText::new("Confirm delete").color(egui::Color32::WHITE),
                )
                .fill(egui::Color32::from_rgb(180, 40, 40));
                if ui.add(confirm).clicked() {
                    self.nuke_replays();
                }
                if ui.button("Cancel").clicked() {
                    self.nuke_confirm_pending = false;
                }
                ui.label(
                    egui::RichText::new("Wipes all ingested replays + stats. .slp files are safe.")
                        .small()
                        .color(egui::Color32::from_rgb(220, 80, 80)),
                );
            });
        } else {
            // Default-state: red-tinted "Delete all replays…" button,
            // carrying a tooltip with the destructive specifics so the
            // user doesn't misread a benign-looking button for a nuke.
            let delete_btn = egui::Button::new(
                egui::RichText::new("Delete all replays…").color(egui::Color32::WHITE),
            )
            .fill(egui::Color32::from_rgb(180, 40, 40));
            let resp = ui
                .add(delete_btn)
                .on_hover_text(
                    "Wipes every ingested replay + all derived stats from the \
                     database. Character/stage/player lookup tables stay. The \
                     .slp files on disk are not touched.",
                );
            if resp.clicked() {
                self.nuke_confirm_pending = true;
                // Stale status from a previous nuke shouldn't linger next
                // to a fresh confirm prompt.
                self.last_nuke_summary = None;
            }
        }

        if let Some(msg) = &self.last_nuke_summary {
            ui.add_space(6.0);
            ui.label(msg);
        }
    }
}

impl eframe::App for StatsMeleeApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Capture the Context once so background workers can kick repaints
        // when they finish. Cheap to clone — it's just an Arc under the hood.
        if self.egui_ctx.is_none() {
            self.egui_ctx = Some(ctx.clone());
        }

        // Drain any pending summary-worker result before we repaint.
        self.poll_summary_worker();
        // Same for the render worker — both background threads' state
        // gets reflected on this frame.
        self.poll_render_worker();
        // Same for the ingestion worker — drain a Done message so
        // the rows + status flip on the same frame the scan finishes.
        self.poll_ingest_worker();
        // Auto-scan trigger. Conditions for a one-shot scan-on-this-
        // frame: (1) we haven't scanned this session yet (or the user
        // just picked a new folder, which resets the latch), (2) a
        // replay folder is configured, (3) no scan is already in
        // flight. The DB doesn't need to be open here — `ingest_replays`
        // opens its own connection on the worker thread.
        if !self.auto_scan_attempted
            && self.config.replay_dir.is_some()
            && self.ingest_rx.is_none()
        {
            self.auto_scan_attempted = true;
            self.ingest_replays();
        }

        self.render_top_bar(ctx);

        egui::CentralPanel::default().show(ctx, |ui| {
            // Center a fixed-max-width content column in the (now
            // sidebar-less) window so the table doesn't hug the left
            // edge on wide displays. `panel_w` is read before the scroll
            // area expands its content, so it's the true bounded panel
            // width — the basis for the symmetric side margin.
            let panel_w = ui.available_width();
            let content_w = CONTENT_MAX_WIDTH.min(panel_w);
            let side = ((panel_w - content_w) * 0.5).max(0.0);

            // Wrap the whole page in a both-axis scroll area so content
            // below/right of the viewport stays reachable when the user
            // shrinks the window. Without this, egui just clips whatever
            // overflows and there's no affordance to scroll it back.
            egui::ScrollArea::both()
                .id_salt("main_panel_scroll")
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    ui.horizontal_top(|ui| {
                        ui.add_space(side);
                        ui.allocate_ui(
                            egui::vec2(content_w, ui.available_height().max(1.0)),
                            |ui| {
                                ui.set_width(content_w);
                                self.main_panel(ui);
                            },
                        );
                    });
                });
        });

        // Floating nav toggle is drawn last so it layers over the
        // central panel's content near the bottom edge.
        self.render_view_toggle(ctx);
    }
}

/// Render `Option<f64>` with `decimals` precision, falling back to "—".
fn fmt_opt_f64(v: Option<f64>, decimals: usize) -> String {
    match v {
        // Named-arg precision (`.prec$`) plays nicely with Rust's captured
        // format args — `.*` would require a positional `decimals` arg and
        // conflict with the captured `x`.
        Some(x) => format!("{x:.prec$}", prec = decimals),
        None => "—".to_string(),
    }
}

/// Render `Option<f64>` in `[0.0, 1.0]` as `NN.N%`, or "—" when `None`.
fn fmt_opt_percent(v: Option<f64>) -> String {
    match v {
        Some(x) => format!("{:.1}%", x * 100.0),
        None => "—".to_string(),
    }
}

/// Open the production analysis sidecar cache rooted under the
/// platform cache dir (`~/Library/Caches/...` on macOS,
/// `~/.cache/...` on Linux, `%LOCALAPPDATA%\...\Cache` on Windows).
///
/// Returns `Err` only if `ProjectDirs` can't pick a cache dir at all
/// (extremely rare — happens on systems without a recognized HOME).
/// Real callers fall back to a tempdir-rooted cache; this helper just
/// expresses the happy path.
fn open_analysis_cache() -> anyhow::Result<AnalysisCache> {
    let dirs = directories::ProjectDirs::from("", "", "stats-melee")
        .ok_or_else(|| anyhow::anyhow!("could not resolve a platform cache directory"))?;
    let root = dirs.cache_dir().join("analysis");
    AnalysisCache::open(
        root,
        AnalysisCacheConfig::default(),
        CombatV2Config::default(),
    )
}

/// Open the production video cache, sibling of the analysis cache
/// under `<ProjectDirs::cache_dir>/video/`. Same fallback contract as
/// [`open_analysis_cache`] — production callers wrap this in an
/// `unwrap_or_else` that points at a tempdir.
fn open_video_cache() -> anyhow::Result<VideoCache> {
    let dirs = directories::ProjectDirs::from("", "", "stats-melee")
        .ok_or_else(|| anyhow::anyhow!("could not resolve a platform cache directory"))?;
    let root = dirs.cache_dir().join("video");
    VideoCache::open(root, VideoCacheConfig::default())
}

/// Resolve the actual user dir Slippi's playback Dolphin reads on
/// startup. The render worker writes its dump config into this dir
/// (with backup/restore around the render). See [`render_worker`]'s
/// module docs for the empirical history of why we don't try to
/// override this via `-u <our-dir>`.
///
/// **Path subtlety:** Dolphin reads from `Application Support/<bundle-
/// identifier>/playback/User/`, where the bundle identifier is
/// `com.project-slippi.dolphin`. NOT `Slippi Launcher/playback/User/`
/// — the "Slippi Launcher" subdir holds the *Launcher app*'s config
/// (and a copy of the Dolphin .app bundle), not Dolphin's runtime
/// state. Confirmed empirically against a Slippi Launcher 2.x
/// install on macOS 14+: the populated GFX.ini lives at
/// `com.project-slippi.dolphin/playback/User/Config/GFX.ini`.
///
/// Currently macOS-only — Linux / Windows Slippi installs would have
/// their own equivalent paths and would surface here, but we don't
/// have user reports yet. Returns `Err` on unsupported platforms or
/// when `HOME` isn't set.
/// Resolve the vanilla Dolphin binary path for the render worker.
///
/// The render pipeline requires our patched vanilla Dolphin master build
/// (interpreter mode, --user isolation), NOT Slippi Playback Dolphin.
/// Resolution order:
///   1. `slippi_playback_command` config field (user override — they can
///      point this at any binary).
///   2. The known dev-build path from the Track 12 spike.
fn resolve_vanilla_dolphin_binary(config: &AppConfig) -> Option<PathBuf> {
    use crate::slippi::predict_app_inner_binary;

    // User override wins.
    if let Some(s) = config
        .slippi_playback_command
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        let raw = PathBuf::from(s);
        return Some(predict_app_inner_binary(&raw).unwrap_or(raw));
    }

    // Fall back to the known dev-build path.
    let dev_build = PathBuf::from(
        "/Users/matthewlafrance/Dev/dolphin/Build/Binaries/Dolphin.app/Contents/MacOS/Dolphin",
    );
    if dev_build.is_file() {
        return Some(dev_build);
    }

    None
}

/// Cross-platform "open this file in the OS default handler". Shells
/// out so we don't pull in another GUI dep just for this one button.
///
/// Each platform gets a separate cfg-gated implementation to keep the
/// argv list local to its branch — Windows in particular needs the
/// `cmd /C start "" <path>` dance because `start` is a cmd builtin
/// rather than a real binary.
#[cfg(target_os = "macos")]
fn open_in_os_player(path: &Path) -> anyhow::Result<()> {
    std::process::Command::new("open")
        .arg(path)
        .spawn()
        .map(|_| ())
        .map_err(|e| anyhow::anyhow!("opening {}: {e}", path.display()))
}

#[cfg(target_os = "linux")]
fn open_in_os_player(path: &Path) -> anyhow::Result<()> {
    std::process::Command::new("xdg-open")
        .arg(path)
        .spawn()
        .map(|_| ())
        .map_err(|e| anyhow::anyhow!("opening {}: {e}", path.display()))
}

#[cfg(target_os = "windows")]
fn open_in_os_player(path: &Path) -> anyhow::Result<()> {
    // The empty `""` after `start` is the window-title arg — without
    // it, `start` interprets a quoted path as the title.
    std::process::Command::new("cmd")
        .args(["/C", "start", "", path.to_string_lossy().as_ref()])
        .spawn()
        .map(|_| ())
        .map_err(|e| anyhow::anyhow!("opening {}: {e}", path.display()))
}

#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
fn open_in_os_player(_path: &Path) -> anyhow::Result<()> {
    Err(anyhow::anyhow!(
        "open-in-default-player is not implemented on this platform"
    ))
}

/// Display label for the Analytics character selector. `None` → "Any";
/// otherwise looks up the id in [`CHARACTERS`] and falls back to a literal
/// "char #N" so an out-of-range id never panics the UI.
fn character_label(id: Option<i32>) -> String {
    match id {
        None => "Any".to_string(),
        Some(c) => CHARACTERS
            .get(c as usize)
            .map(|s| (*s).to_string())
            .unwrap_or_else(|| format!("char #{c}")),
    }
}

/// Display label for the Analytics stage selector. Mirrors
/// [`character_label`] for the stage table.
fn stage_label(id: Option<i32>) -> String {
    match id {
        None => "Any".to_string(),
        Some(s) => STAGES
            .get(s as usize)
            .map(|name| (*name).to_string())
            .unwrap_or_else(|| format!("stage #{s}")),
    }
}

/// Render a clickable header label with a sort-indicator arrow when this
/// column is the active sort. Returns `true` if the header was clicked
/// this frame.
///
/// We render the label as an egui `Button` with `.frame(false)` so it
/// looks like the existing plain text headers (no button chrome) but
/// still participates in hit-testing — egui's `Label` + `.sense(CLICK)`
/// is close but loses the subtle hover highlight that makes it obvious
/// the header is interactive.
fn sortable_header(
    ui: &mut egui::Ui,
    label: &str,
    key: SortKey,
    current_key: SortKey,
    current_dir: SortDirection,
) -> bool {
    let active = key == current_key;
    // Small arrow disambiguates direction without stealing real estate
    // from the label itself. Inactive columns get nothing — clutter-free.
    let arrow = if active {
        match current_dir {
            SortDirection::Asc => " \u{25B2}",
            SortDirection::Desc => " \u{25BC}",
        }
    } else {
        ""
    };
    let text = egui::RichText::new(format!("{label}{arrow}")).strong();
    ui.add(egui::Button::new(text).frame(false)).clicked()
}

/// Render one player-slot cell. Highlights the user's own code in accent
/// color so it pops in long lists.
fn render_slot_cell(
    ui: &mut egui::Ui,
    icons: &mut crate::icons::IconCache,
    slot: Option<&crate::replay_list::PlayerSlot>,
    user_code: &str,
) {
    match slot {
        None => {
            ui.label("–");
        }
        Some(s) => {
            ui.horizontal(|ui| {
                crate::icons::character_icon(ui, icons, s.character_id, 18.0);
                ui.add_space(5.0);
                let is_me = !user_code.is_empty() && s.code == user_code;
                let text = format!("{} ({})", s.code, s.character_name());
                if is_me {
                    ui.colored_label(egui::Color32::from_rgb(100, 170, 255), text);
                } else {
                    ui.label(text);
                }
            });
        }
    }
}

/// One segment of the floating bottom nav toggle. Returns `true` when
/// clicked. Active segment is filled with [`ACCENT`]; inactive is a
/// transparent pill that lights up on hover.
fn view_pill(ui: &mut egui::Ui, current: Page, target: Page, label: &str) -> bool {
    let selected = current == target;
    let text_color = if selected {
        egui::Color32::from_rgb(10, 20, 40)
    } else {
        egui::Color32::from_gray(205)
    };
    let btn = egui::Button::new(egui::RichText::new(label).size(13.5).color(text_color))
        .min_size(egui::vec2(104.0, 30.0))
        .rounding(egui::Rounding::same(999.0))
        .fill(if selected {
            ACCENT
        } else {
            egui::Color32::TRANSPARENT
        });
    ui.add(btn).clicked()
}

/// A compact metric card: a small muted label over a large value, on a
/// raised surface. The building block of the Analytics summary.
fn metric_card(ui: &mut egui::Ui, label: &str, value: &str) {
    egui::Frame::none()
        .fill(egui::Color32::from_rgb(0x26, 0x29, 0x31))
        .rounding(egui::Rounding::same(8.0))
        .inner_margin(egui::Margin::symmetric(16.0, 11.0))
        .show(ui, |ui| {
            ui.vertical(|ui| {
                ui.set_min_width(116.0);
                ui.label(
                    egui::RichText::new(label)
                        .size(12.0)
                        .color(egui::Color32::from_gray(145)),
                );
                ui.add_space(3.0);
                ui.label(egui::RichText::new(value).size(23.0).strong());
            });
        });
}

/// Apply the app-wide visual theme: a roomier dark style with a cool
/// accent, a clear type scale, and consistently rounded widgets. Called
/// once at startup from [`StatsMeleeApp::new`].
fn apply_theme(ctx: &egui::Context) {
    use egui::{Color32, FontFamily, FontId, Rounding, Stroke, TextStyle};

    let mut style = (*ctx.style()).clone();

    // Roomier than egui's defaults — the dense table benefits from a bit
    // more breathing room around controls.
    style.spacing.item_spacing = egui::vec2(8.0, 8.0);
    style.spacing.button_padding = egui::vec2(10.0, 5.0);
    style.spacing.menu_margin = egui::Margin::same(8.0);
    style.spacing.interact_size.y = 26.0;

    // Type scale with a clear heading hierarchy.
    style.text_styles = [
        (TextStyle::Heading, FontId::new(22.0, FontFamily::Proportional)),
        (TextStyle::Body, FontId::new(14.0, FontFamily::Proportional)),
        (TextStyle::Monospace, FontId::new(13.0, FontFamily::Monospace)),
        (TextStyle::Button, FontId::new(14.0, FontFamily::Proportional)),
        (TextStyle::Small, FontId::new(11.0, FontFamily::Proportional)),
    ]
    .into();

    // Dark visuals with a cool blue accent (matches ACCENT).
    let mut v = egui::Visuals::dark();
    let accent = ACCENT;
    v.panel_fill = Color32::from_rgb(0x1B, 0x1D, 0x23);
    v.window_fill = Color32::from_rgb(0x22, 0x25, 0x2C);
    v.extreme_bg_color = Color32::from_rgb(0x14, 0x16, 0x1A);
    v.faint_bg_color = Color32::from_rgb(0x26, 0x29, 0x31); // striped table rows
    v.hyperlink_color = accent;
    v.selection.bg_fill = accent.linear_multiply(0.45);
    v.selection.stroke = Stroke::new(1.0, accent);

    let rounding = Rounding::same(5.0);
    for w in [
        &mut v.widgets.noninteractive,
        &mut v.widgets.inactive,
        &mut v.widgets.hovered,
        &mut v.widgets.active,
        &mut v.widgets.open,
    ] {
        w.rounding = rounding;
    }
    v.widgets.hovered.bg_stroke = Stroke::new(1.0, accent.linear_multiply(0.6));
    v.window_rounding = Rounding::same(8.0);

    style.visuals = v;
    ctx.set_style(style);
}
