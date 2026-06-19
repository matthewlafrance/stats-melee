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
use stats_melee::gamedata::{spaced_name, CHARACTERS, STAGES};
use stats_melee::video_cache::{VideoCache, VideoCacheConfig};
use stats_melee::analytics::{WinAnalytics, WinProportion};
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

// === Melee palette ===========================================================
// A single fixed dark theme inspired by the Melee title screen: a deep
// indigo/purple base under a warm "Melee gold" accent, with flame-orange for
// destructive actions. No light mode — every surface reads against this base.
// `Color32::from_rgb` is `const`, so these compose into the style at startup
// and are referenced directly by the custom-painted widgets.

/// App background — the deep indigo behind every panel.
const BG_APP: egui::Color32 = egui::Color32::from_rgb(0x16, 0x13, 0x20);
/// Raised window / menu / popup fill, one step up from [`BG_APP`].
const BG_WINDOW: egui::Color32 = egui::Color32::from_rgb(0x1F, 0x1B, 0x2E);
/// "Card" / info-bubble surface — metric cards, favorites, head-to-head.
const BG_CARD: egui::Color32 = egui::Color32::from_rgb(0x29, 0x23, 0x3A);
/// Deepest sink — text-edit interiors, the floating nav capsule.
const BG_EXTREME: egui::Color32 = egui::Color32::from_rgb(0x0F, 0x0D, 0x17);
/// Striped table rows / faint backgrounds.
const BG_STRIPE: egui::Color32 = egui::Color32::from_rgb(0x21, 0x1C, 0x30);
/// Neutral raised fill — default button bodies and bar tracks.
const BG_TRACK: egui::Color32 = egui::Color32::from_rgb(0x33, 0x2C, 0x47);
/// Same, one shade lighter — button hover.
const BG_TRACK_HI: egui::Color32 = egui::Color32::from_rgb(0x40, 0x37, 0x59);

/// Melee gold — primary accent: active nav, primary buttons, selection,
/// the user's own connect code, the settings gear when open.
const ACCENT: egui::Color32 = egui::Color32::from_rgb(0xE7, 0xB1, 0x3B);
/// Brighter gold for hover strokes / pressed primary buttons.
const ACCENT_HI: egui::Color32 = egui::Color32::from_rgb(0xF4, 0xC9, 0x5A);
/// Dark text/iconography that sits *on top of* the gold accent.
const ON_ACCENT: egui::Color32 = egui::Color32::from_rgb(0x1A, 0x12, 0x06);
/// Flame orange-red — destructive actions and the loss marker.
const FLAME: egui::Color32 = egui::Color32::from_rgb(0xDD, 0x52, 0x33);
/// Victory green — the win marker and the top of the win-rate ramp.
const WIN_GREEN: egui::Color32 = egui::Color32::from_rgb(0x5F, 0xC1, 0x6E);

/// Primary text.
const TEXT_HI: egui::Color32 = egui::Color32::from_rgb(0xED, 0xE9, 0xF4);
/// Muted secondary text — captions, sublabels, inactive controls.
const TEXT_MUTED: egui::Color32 = egui::Color32::from_rgb(0x9A, 0x92, 0xAD);

/// Max width of the centered page content column. Sized to fit the full
/// replay-library row (~1095 px of columns + ~70 px of inter-column spacing
/// across the ID / outcome / two player / stage / duration / played / added /
/// view / delete columns) so the table sits centered rather than hugging the
/// left edge on wide windows.
const CONTENT_MAX_WIDTH: f32 = 1180.0;

/// Which page is currently displayed in the main panel.
///
/// `ReplayLibrary`, `Analytics`, and `Career` are the three primary views,
/// reached from the floating toggle at the bottom of the window. Library and
/// Analytics share the left filter menu (Analytics reflects the same filtered
/// game set as the library); Career is filter-independent (whole-history
/// favorites + win-rate breakdowns). `Settings` is reached from the gear in
/// the top bar. `ReplayViewer` is a drill-down from the library ("View" on a
/// row) and has no nav entry of its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Page {
    ReplayLibrary,
    Analytics,
    Career,
    Settings,
    ReplayViewer,
}

/// Outcome filter for the Replay Library. `All` shows everything;
/// `Wins`/`Losses` keep only rows where the configured user code placed
/// first / didn't (rows with no known outcome — user absent or no code set
/// — are hidden by both non-`All` options).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OutcomeFilter {
    All,
    Wins,
    Losses,
}

impl OutcomeFilter {
    fn label(self) -> &'static str {
        match self {
            OutcomeFilter::All => "All",
            OutcomeFilter::Wins => "Wins",
            OutcomeFilter::Losses => "Losses",
        }
    }
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

    /// Cached PlayerSummary for the Analytics page, narrowed to the same
    /// game set the library filter is currently showing (the shared filter
    /// menu drives both pages). With no filter active this is the player's
    /// whole-career summary; any structured filter scopes every metric
    /// (including the win rate) to that subset. `games_played == 0` means the
    /// filter matched no replays. Rebuilt on demand and keyed by
    /// `summary_for` so a Settings code edit *or* a filter change kicks the
    /// worker.
    filtered_summary: Option<PlayerSummary>,
    /// Whole-career (filter-independent) PlayerSummary, shown on the Career
    /// page alongside the favorites + win-rate breakdowns. Computed in the
    /// same worker pass as `filtered_summary`.
    career_summary: Option<PlayerSummary>,
    /// Career win-rate breakdowns (by played character, opponent-character
    /// matchup, stage, and opponent code) for the current code. Computed in
    /// the same worker as `filtered_summary` and rendered on the Career page.
    /// Filter-independent — always the full career view.
    win_analytics: Option<WinAnalytics>,
    /// Most recent error from `player_summary_filtered`.
    summary_error: Option<String>,
    /// The (code + library filter signature) the cached summaries were built
    /// for. Compared to the current config + filter each frame to know when
    /// to rebuild — a filter tweak regenerates the summary the same way
    /// changing the user code does.
    summary_for: Option<SummaryKey>,
    /// Receiver side of the background summary worker. When `Some`, a
    /// worker thread is computing a summary and we should be polling it
    /// each frame via [`poll_summary_worker`]. `None` means idle.
    summary_rx: Option<mpsc::Receiver<SummaryMsg>>,
    /// True while a worker is in flight. Mirrors `summary_rx.is_some()` but
    /// makes the "show a spinner" check in the UI loop self-documenting.
    summary_loading: bool,
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

    /// Structured Replay Library filters, shown in the left filter menu and
    /// ANDed together. Character/opp-character are the user's vs the
    /// opponent's pick (fall back to "any slot" when no user code is set);
    /// stage matches `game.stage`; outcome is relative to the user code; date
    /// is an inclusive `YYYY-MM-DD` range on the played date; opponent tag is
    /// a case-insensitive substring on non-user slot codes. Session-state only.
    library_character_filter: Option<i32>,
    library_opp_character_filter: Option<i32>,
    library_stage_filter: Option<i32>,
    library_outcome_filter: OutcomeFilter,
    library_date_from: String,
    library_date_to: String,
    /// Inclusive `YYYY-MM-DD` range on the *ingested* ("date added")
    /// timestamp — the sibling of `library_date_from`/`to` for the second
    /// date filter in the menu. Always populated (every row has an
    /// ingested_at), unlike the played-date range.
    library_added_from: String,
    library_added_to: String,
    library_opponent_tag: String,
    /// Whether the left filter menu is shown on the Replay Library page.
    show_filter_panel: bool,

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
    /// `(filtered summary, optional career bundle)`. The filtered summary
    /// reflects the shared library filter (the Analytics page) and is always
    /// recomputed. The career bundle `(career summary, win breakdowns)` is
    /// filter-independent, so the worker only recomputes it when the code or
    /// underlying data changed — a filter-only change carries `None` and the
    /// app keeps its existing career data, keeping the Analytics refresh snappy
    /// while dragging filters.
    Ok(PlayerSummary, Option<(PlayerSummary, WinAnalytics)>),
    Err(String),
}

/// One message off the ingestion-worker channel. Same shape as
/// [`SummaryMsg`]: success carries the count of newly-ingested
/// games, failure carries a stringified error.
enum IngestMsg {
    Ok(usize),
    Err(String),
}

/// Cache key for the summary worker: the player code plus a signature of the
/// shared library filter. Library + Analytics share this filter, so any
/// change to a structured filter field (or the code) invalidates the cached
/// summaries and re-kicks the worker. The actual `game_ids` restriction is derived from the
/// loaded rows at worker-spawn time, not stored here (the signature alone
/// determines whether that set changed, since the rows are stable between
/// explicit cache resets).
#[derive(Debug, Clone, PartialEq, Eq)]
struct SummaryKey {
    code: String,
    character: Option<i32>,
    opp_character: Option<i32>,
    stage: Option<i32>,
    outcome: OutcomeFilter,
    date_from: String,
    date_to: String,
    added_from: String,
    added_to: String,
    opponent_tag: String,
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
            filtered_summary: None,
            career_summary: None,
            win_analytics: None,
            summary_error: None,
            summary_for: None,
            summary_rx: None,
            summary_loading: false,
            egui_ctx: None,
            nuke_confirm_pending: false,
            last_nuke_summary: None,
            sort_key: SortKey::IngestedAt,
            sort_direction: SortDirection::Desc,
            library_character_filter: None,
            library_opp_character_filter: None,
            library_stage_filter: None,
            library_outcome_filter: OutcomeFilter::All,
            library_date_from: String::new(),
            library_date_to: String::new(),
            library_added_from: String::new(),
            library_added_to: String::new(),
            library_opponent_tag: String::new(),
            show_filter_panel: true,
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
                self.filtered_summary = None;
                self.career_summary = None;
                self.win_analytics = None;
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
                self.filtered_summary = None;
                self.career_summary = None;
                self.win_analytics = None;
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

    /// Load the cached rows once if they're empty and the DB is open. No-op
    /// otherwise. Shared by the Library page and the `update()` pre-pass so
    /// the Analytics game-id set + filter-panel histograms have data even
    /// when the user lands on Analytics without visiting the Library first.
    fn ensure_rows_loaded(&mut self) {
        if self.rows.is_empty() && self.rows_error.is_none() && self.db_conn.is_some() {
            self.reload_rows();
        }
    }

    /// Whether `r` survives the current Replay Library filters. Used to build
    /// the library's visible-row set and the "showing N of M" count. Now
    /// identical to the structured filter (free-text search was removed); kept
    /// as a named seam in case a library-only filter returns later.
    fn library_row_visible(&self, r: &ReplayRow) -> bool {
        self.library_row_matches_structured(r)
    }

    /// Whether `r` survives the structured filter panel (my-character AND
    /// opposing-character AND stage AND outcome AND opponent-tag AND
    /// played-date range AND added-date range). This is the shared
    /// Library/Analytics filter — Analytics builds its game set from exactly
    /// these rows.
    ///
    /// "My" vs "opposing" slots are split by the configured user code; with
    /// no code set, both character filters fall back to matching any slot.
    fn library_row_matches_structured(&self, r: &ReplayRow) -> bool {
        let user_code = self.config.user_player_code.trim();
        let has_user = !user_code.is_empty();

        // My character: a slot I played (or any slot if no code is set).
        let my_ok = self.library_character_filter.is_none_or(|c| {
            r.slots
                .iter()
                .flatten()
                .any(|s| s.character_id == c && (!has_user || s.code == user_code))
        });
        // Opposing character: a slot someone *other* than me played.
        let opp_ok = self.library_opp_character_filter.is_none_or(|c| {
            r.slots
                .iter()
                .flatten()
                .any(|s| s.character_id == c && (!has_user || s.code != user_code))
        });
        let stage_ok = self.library_stage_filter.is_none_or(|st| r.stage_id == st);
        let outcome_ok = match self.library_outcome_filter {
            OutcomeFilter::All => true,
            OutcomeFilter::Wins => r.user_won == Some(true),
            OutcomeFilter::Losses => r.user_won == Some(false),
        };

        // Opponent tag: case-insensitive substring against opponent codes.
        let tag = self.library_opponent_tag.trim().to_lowercase();
        let tag_ok = tag.is_empty()
            || r.slots.iter().flatten().any(|s| {
                (!has_user || s.code != user_code) && s.code.to_lowercase().contains(&tag)
            });

        let date_ok = self.library_date_in_range(r);
        let added_ok = self.library_added_date_in_range(r);

        my_ok && opp_ok && stage_ok && outcome_ok && tag_ok && date_ok && added_ok
    }

    /// Inclusive `YYYY-MM-DD` played-date range test. Empty bounds don't
    /// constrain. When a bound is set, a row with no recorded play date is
    /// excluded (we can't confirm it falls in range — re-ingest to populate).
    fn library_date_in_range(&self, r: &ReplayRow) -> bool {
        let from = self.library_date_from.trim();
        let to = self.library_date_to.trim();
        if from.is_empty() && to.is_empty() {
            return true;
        }
        let Some(played) = r.played_date() else {
            return false;
        };
        // ISO-8601 dates compare correctly lexicographically.
        if !from.is_empty() && played < from {
            return false;
        }
        if !to.is_empty() && played > to {
            return false;
        }
        true
    }

    /// Inclusive `YYYY-MM-DD` ingested ("date added") range test. Same
    /// contract as [`Self::library_date_in_range`] but against the row's
    /// ingested timestamp rather than its played date.
    fn library_added_date_in_range(&self, r: &ReplayRow) -> bool {
        let from = self.library_added_from.trim();
        let to = self.library_added_to.trim();
        if from.is_empty() && to.is_empty() {
            return true;
        }
        let Some(added) = r.ingested_date() else {
            return false;
        };
        if !from.is_empty() && added < from {
            return false;
        }
        if !to.is_empty() && added > to {
            return false;
        }
        true
    }

    /// True when any Replay Library filter is narrowing the list.
    fn library_filter_active(&self) -> bool {
        self.structured_filter_active()
    }

    /// True when any *structured* panel filter (everything except the
    /// free-text search box) is narrowing the set. This is the filter
    /// Analytics shares — when false, the Analytics summary is whole-career.
    fn structured_filter_active(&self) -> bool {
        self.library_character_filter.is_some()
            || self.library_opp_character_filter.is_some()
            || self.library_stage_filter.is_some()
            || self.library_outcome_filter != OutcomeFilter::All
            || !self.library_date_from.trim().is_empty()
            || !self.library_date_to.trim().is_empty()
            || !self.library_added_from.trim().is_empty()
            || !self.library_added_to.trim().is_empty()
            || !self.library_opponent_tag.trim().is_empty()
    }

    /// The set of `game.id`s matching the structured filter — the population
    /// the Analytics page aggregates over. Threaded into the summary worker
    /// as [`stats_melee::PlayerSummaryFilter::game_ids`] so every metric
    /// reflects exactly the filtered library view.
    fn library_filtered_game_ids(&self) -> Vec<i32> {
        self.rows
            .iter()
            .filter(|r| self.library_row_matches_structured(r))
            .map(|r| r.game_id)
            .collect()
    }

    /// Cache key / worker signature for the shared-filter summaries: the
    /// player code plus every structured filter field (search excluded).
    fn summary_key(&self, code: &str) -> SummaryKey {
        SummaryKey {
            code: code.to_string(),
            character: self.library_character_filter,
            opp_character: self.library_opp_character_filter,
            stage: self.library_stage_filter,
            outcome: self.library_outcome_filter,
            date_from: self.library_date_from.trim().to_string(),
            date_to: self.library_date_to.trim().to_string(),
            added_from: self.library_added_from.trim().to_string(),
            added_to: self.library_added_to.trim().to_string(),
            opponent_tag: self.library_opponent_tag.trim().to_string(),
        }
    }

    /// A short human description of the active structured filter, for the
    /// Analytics header (e.g. "Fox · vs Falco · Battlefield · Wins"). Returns
    /// "Your whole history" when no structured filter is set.
    fn filter_description(&self) -> String {
        if !self.structured_filter_active() {
            return "Your whole history".to_string();
        }
        let mut parts: Vec<String> = Vec::new();
        if let Some(c) = self.library_character_filter {
            parts.push(character_label(Some(c)));
        }
        if let Some(c) = self.library_opp_character_filter {
            parts.push(format!("vs {}", character_label(Some(c))));
        }
        if let Some(s) = self.library_stage_filter {
            parts.push(stage_label(Some(s)));
        }
        match self.library_outcome_filter {
            OutcomeFilter::All => {}
            OutcomeFilter::Wins => parts.push("Wins".to_string()),
            OutcomeFilter::Losses => parts.push("Losses".to_string()),
        }
        let tag = self.library_opponent_tag.trim();
        if !tag.is_empty() {
            parts.push(format!("vs {tag}"));
        }
        let date_span = |from: &str, to: &str, label: &str| -> Option<String> {
            let (from, to) = (from.trim(), to.trim());
            if from.is_empty() && to.is_empty() {
                None
            } else {
                let a = if from.is_empty() { "…" } else { from };
                let b = if to.is_empty() { "…" } else { to };
                Some(format!("{label} {a}–{b}"))
            }
        };
        if let Some(s) = date_span(&self.library_date_from, &self.library_date_to, "played") {
            parts.push(s);
        }
        if let Some(s) = date_span(&self.library_added_from, &self.library_added_to, "added") {
            parts.push(s);
        }
        parts.join(" · ")
    }

    /// Played date of every loaded row that has one, as day ordinals. Feeds
    /// both the date slider's domain (min/max) and the density histogram
    /// above it (one entry per game). Unsorted; empty when no row has a play
    /// date.
    fn library_played_date_ordinals(&self) -> Vec<i64> {
        self.rows
            .iter()
            .filter_map(|r| r.played_date().and_then(date_to_ordinal))
            .collect()
    }

    /// Ingested ("date added") date of every loaded row, as day ordinals.
    /// The sibling of [`Self::library_played_date_ordinals`] — effectively
    /// one entry per row (every row carries an ingested_at).
    fn library_ingested_date_ordinals(&self) -> Vec<i64> {
        self.rows
            .iter()
            .filter_map(|r| r.ingested_date().and_then(date_to_ordinal))
            .collect()
    }

    /// Sorted, distinct opponent connect codes across loaded rows — the
    /// autocomplete pool for the opponent-tag filter. Excludes the user's
    /// own code when one is configured.
    fn library_opponent_codes(&self) -> Vec<String> {
        let user_code = self.config.user_player_code.trim();
        let has_user = !user_code.is_empty();
        let mut set = std::collections::BTreeSet::new();
        for r in &self.rows {
            for s in r.slots.iter().flatten() {
                if !s.code.is_empty() && (!has_user || s.code != user_code) {
                    set.insert(s.code.clone());
                }
            }
        }
        set.into_iter().collect()
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

        // Pass the configured Melee ISO so Dolphin actually boots the game the
        // replay's inputs run against — without it, playback Dolphin opens but
        // never starts the replay.
        let iso_path = self
            .config
            .melee_iso_path
            .as_deref()
            .map(|p| p.to_string_lossy().into_owned());

        match slippi::launch_replay(&path, override_cmd, iso_path.as_deref()) {
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

    /// Top bar: wordmark + replay count on the left; the settings gear on the
    /// right. Replaces the old left sidebar. (Free-text search was removed —
    /// the structured filter panel covers code / character / stage / date.)
    fn render_top_bar(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::top("topbar").show(ctx, |ui| {
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.add_space(2.0);
                ui.label(
                    egui::RichText::new("stats-melee")
                        .size(18.0)
                        .strong()
                        .color(ACCENT),
                );
                if !self.rows.is_empty() {
                    ui.label(
                        egui::RichText::new(format!("· {} replays", self.rows.len()))
                            .color(TEXT_MUTED),
                    );
                }

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let on_settings = self.page == Page::Settings;
                    let gear = egui::Button::new(
                        egui::RichText::new("⚙")
                            .size(17.0)
                            .color(if on_settings { ON_ACCENT } else { TEXT_HI }),
                    )
                    .min_size(egui::vec2(34.0, 28.0))
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
                });
            });
            ui.add_space(6.0);
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
                // A deep capsule with a faint purple rim that floats above
                // the page content.
                egui::Frame::none()
                    .fill(BG_EXTREME)
                    .stroke(egui::Stroke::new(1.0, egui::Color32::from_rgb(0x39, 0x31, 0x4E)))
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
                            if view_pill(ui, self.page, Page::Career, "Career") {
                                target = Some(Page::Career);
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
            Page::Career => self.page_career(ui),
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
            let scan_btn =
                egui::Button::new(egui::RichText::new("Scan for new replays").color(ON_ACCENT).strong())
                    .fill(ACCENT);
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
        self.ensure_rows_loaded();

        // Structured filter + sort controls.
        self.render_library_controls(ui);

        // Show a "(N of M)" count whenever a structured filter is narrowing
        // the list.
        if self.library_filter_active() {
            let total = self.rows.len();
            let shown = self
                .rows
                .iter()
                .filter(|r| self.library_row_visible(r))
                .count();
            ui.label(
                egui::RichText::new(format!("Showing {shown} of {total}"))
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

    /// Left-hand filter menu for the Replay Library: my-character,
    /// opposing-character, stage, outcome, played-date range, and opponent
    /// tag. Rendered as a `SidePanel` at the context level (so it carves
    /// space off the window's left edge), toggled by `show_filter_panel`.
    fn render_filter_panel(&mut self, ctx: &egui::Context) {
        // Read-only data computed before any &mut borrows of `self`. The
        // ordinal lists feed both the slider domain (their min/max) and the
        // density histogram drawn above each slider (one entry per game).
        let played_ordinals = self.library_played_date_ordinals();
        let added_ordinals = self.library_ingested_date_ordinals();
        let played_domain = ordinal_domain(&played_ordinals);
        let added_domain = ordinal_domain(&added_ordinals);
        let opponent_codes = self.library_opponent_codes();

        egui::SidePanel::left("library_filter_panel")
            .resizable(false)
            .exact_width(248.0)
            .show(ctx, |ui| {
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    ui.heading("Filters");
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("✕").on_hover_text("Hide filters").clicked() {
                            self.show_filter_panel = false;
                        }
                    });
                });
                ui.separator();

                egui::ScrollArea::vertical().show(ui, |ui| {
                    // Character / stage / outcome combos (disjoint borrows
                    // so the icon cache + each filter field can be mutated
                    // inside the nested combo closures).
                    {
                        let Self {
                            icons,
                            library_character_filter,
                            library_opp_character_filter,
                            library_stage_filter,
                            library_outcome_filter,
                            ..
                        } = &mut *self;

                        filter_field_label(ui, "My character");
                        character_filter_combo(ui, icons, "filter_my_char", library_character_filter);

                        filter_field_label(ui, "Opposing character");
                        character_filter_combo(
                            ui,
                            icons,
                            "filter_opp_char",
                            library_opp_character_filter,
                        );

                        filter_field_label(ui, "Stage");
                        stage_filter_combo(ui, icons, "filter_stage", library_stage_filter);

                        filter_field_label(ui, "Outcome");
                        egui::ComboBox::from_id_salt("filter_outcome")
                            .width(200.0)
                            .selected_text((*library_outcome_filter).label())
                            .show_ui(ui, |ui| {
                                ui.selectable_value(
                                    library_outcome_filter,
                                    OutcomeFilter::All,
                                    "All",
                                );
                                ui.selectable_value(
                                    library_outcome_filter,
                                    OutcomeFilter::Wins,
                                    "Wins",
                                );
                                ui.selectable_value(
                                    library_outcome_filter,
                                    OutcomeFilter::Losses,
                                    "Losses",
                                );
                            });
                    }

                    // Two date filters share one renderer — borrow each
                    // pair of fields disjointly (distinct struct fields, so
                    // no overlap) rather than going through `self`.
                    render_date_range_filter(
                        ui,
                        "Date played",
                        &mut self.library_date_from,
                        &mut self.library_date_to,
                        played_domain,
                        &played_ordinals,
                    );
                    render_date_range_filter(
                        ui,
                        "Date added",
                        &mut self.library_added_from,
                        &mut self.library_added_to,
                        added_domain,
                        &added_ordinals,
                    );
                    self.render_opponent_tag_filter(ui, &opponent_codes);

                    ui.add_space(14.0);
                    if ui.button("Clear all filters").clicked() {
                        self.library_character_filter = None;
                        self.library_opp_character_filter = None;
                        self.library_stage_filter = None;
                        self.library_outcome_filter = OutcomeFilter::All;
                        self.library_date_from.clear();
                        self.library_date_to.clear();
                        self.library_added_from.clear();
                        self.library_added_to.clear();
                        self.library_opponent_tag.clear();
                    }
                    ui.add_space(8.0);
                });
            });
    }

    /// "Opponent tag" filter: a text box plus an autocomplete list of
    /// matching opponent codes drawn from the loaded rows. Clicking a
    /// suggestion fills the box.
    fn render_opponent_tag_filter(&mut self, ui: &mut egui::Ui, codes: &[String]) {
        filter_field_label(ui, "Opponent tag");
        ui.add(
            egui::TextEdit::singleline(&mut self.library_opponent_tag)
                .hint_text("e.g. ABC#123")
                .desired_width(200.0),
        );

        // Suggestions whenever there's a non-exact partial match. Not gated
        // on focus — that avoids the click-defocus race, and showing the
        // matches is useful on its own.
        let q = self.library_opponent_tag.trim().to_lowercase();
        if !q.is_empty() {
            let matches: Vec<&String> = codes
                .iter()
                .filter(|c| {
                    let lc = c.to_lowercase();
                    lc.contains(&q) && lc != q
                })
                .take(6)
                .collect();
            if !matches.is_empty() {
                let mut pick: Option<String> = None;
                egui::Frame::group(ui.style())
                    .inner_margin(egui::Margin::symmetric(6.0, 4.0))
                    .show(ui, |ui| {
                        for m in matches {
                            if ui
                                .add(egui::Label::new(m).sense(egui::Sense::click()))
                                .on_hover_text("Use this opponent")
                                .clicked()
                            {
                                pick = Some(m.clone());
                            }
                        }
                    });
                if let Some(code) = pick {
                    self.library_opponent_tag = code;
                }
            }
        }
    }

    /// Filter + sort controls row shown above the table: a Filters-menu
    /// toggle on the left and the sort controls ("Sort by:" key dropdown
    /// sharing `sort_key`/`sort_direction` with the column headers, plus a
    /// direction toggle).
    fn render_library_controls(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            // Toggle for the left filter menu.
            let toggle = if self.show_filter_panel {
                "◀ Hide filters"
            } else {
                "☰ Filters"
            };
            if ui.button(toggle).clicked() {
                self.show_filter_panel = !self.show_filter_panel;
            }

            ui.add_space(16.0);
            ui.label("Sort by:");
            let mut chosen = self.sort_key;
            egui::ComboBox::from_id_salt("library_sort_combo")
                .selected_text(sort_key_label(self.sort_key))
                .show_ui(ui, |ui| {
                    for key in [
                        SortKey::IngestedAt,
                        SortKey::PlayedAt,
                        SortKey::GameId,
                        SortKey::Stage,
                        SortKey::Duration,
                        SortKey::Outcome,
                    ] {
                        ui.selectable_value(&mut chosen, key, sort_key_label(key));
                    }
                });
            if chosen != self.sort_key {
                self.sort_key = chosen;
                self.sort_direction = chosen.default_direction();
                replay_list::sort_rows(&mut self.rows, self.sort_key, self.sort_direction);
            }
            let arrow = match self.sort_direction {
                SortDirection::Asc => "▲ Asc",
                SortDirection::Desc => "▼ Desc",
            };
            if ui
                .button(arrow)
                .on_hover_text("Toggle sort direction")
                .clicked()
            {
                self.sort_direction = match self.sort_direction {
                    SortDirection::Asc => SortDirection::Desc,
                    SortDirection::Desc => SortDirection::Asc,
                };
                replay_list::sort_rows(&mut self.rows, self.sort_key, self.sort_direction);
            }
        });
        ui.add_space(6.0);
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

        // Build the visible-rows index up front. The table body closure
        // needs random-access by row.index() into the filtered set, so a
        // dense Vec<usize> mapping table-position → underlying-row index is
        // the right shape. When no filter is active we skip the per-row
        // predicate and use a 0..N range as a hot path.
        let visible: Vec<usize> = if !self.library_filter_active() {
            (0..self.rows.len()).collect()
        } else {
            self.rows
                .iter()
                .enumerate()
                .filter(|(_, r)| self.library_row_visible(r))
                .map(|(i, _)| i)
                .collect()
        };

        if visible.is_empty() {
            ui.label(
                egui::RichText::new(
                    "No replays match the current filters. Clear them to see all rows.",
                )
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
            .resizable(false)
            .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
            // Fixed widths (the table is non-resizable). Sized so the widest
            // realistic content — a long connect code + spaced character name
            // ("FALCO#123 (Captain Falcon)"), or "Mushroom Kingdom II" — fits
            // with a little breathing room before the next column rather than
            // clipping mid-glyph. Labels also truncate as a backstop.
            // Widths sum (+ inter-column spacing) to stay under
            // `CONTENT_MAX_WIDTH` so the centered table never triggers a
            // horizontal scrollbar on a wide window.
            .column(Column::initial(56.0)) // Game id
            .column(Column::initial(48.0)) // Outcome
            .column(Column::initial(210.0)) // P1 (winner)
            .column(Column::initial(210.0)) // P2
            .column(Column::initial(158.0)) // Stage
            .column(Column::initial(72.0)) // Duration
            .column(Column::initial(88.0)) // Date played
            .column(Column::initial(88.0)) // Date added (ingested)
            .column(Column::initial(86.0)) // View button (accent CTA)
            .column(Column::initial(72.0)) // Delete (icon-only; confirm uses two narrow buttons)
            .header(24.0, |mut header| {
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
                        "Played",
                        SortKey::PlayedAt,
                        current_key,
                        current_dir,
                    ) {
                        clicked = Some(SortKey::PlayedAt);
                    }
                });
                header.col(|ui| {
                    if sortable_header(
                        ui,
                        "Added",
                        SortKey::IngestedAt,
                        current_key,
                        current_dir,
                    ) {
                        clicked = Some(SortKey::IngestedAt);
                    }
                });
                header.col(|ui| {
                    ui.strong("Watch");
                });
                header.col(|ui| {
                    // No header — the trash glyph is its own affordance.
                    ui.label("");
                });
            })
            .body(|body| {
                body.rows(30.0, visible.len(), |mut row| {
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
                            ui.label(egui::RichText::new("W").strong().color(WIN_GREEN));
                        }
                        Some(false) => {
                            ui.label(egui::RichText::new("L").strong().color(FLAME));
                        }
                        None => {
                            ui.label(egui::RichText::new("–").color(TEXT_MUTED));
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
                            ui.add(
                                egui::Label::new(spaced_name(r.stage_name())).truncate(),
                            );
                        });
                    });

                    row.col(|ui| {
                        ui.label(r.duration_display());
                    });

                    row.col(|ui| match r.played_date() {
                        Some(date) => {
                            // Hover shows the full timestamp when present.
                            ui.label(date).on_hover_text(
                                r.played_at.as_deref().unwrap_or(date),
                            );
                        }
                        None => {
                            ui.label("—")
                                .on_hover_text("No play date — re-scan to add it");
                        }
                    });

                    row.col(|ui| match r.ingested_date() {
                        Some(date) => {
                            // Hover shows the full ingest timestamp.
                            ui.label(date).on_hover_text(r.ingested_at.as_str());
                        }
                        None => {
                            ui.label("—");
                        }
                    });

                    row.col(|ui| {
                        if primary_button(ui, "▶ View")
                            .on_hover_text("Open this replay in the viewer")
                            .clicked()
                        {
                            view_clicked = Some(r.game_id);
                        }
                    });

                    row.col(|ui| {
                        // Disarmed state: small trash glyph. Armed
                        // state (when this row is `pending_delete`):
                        // flame "Delete?" + "Cancel" pair. Pattern
                        // mirrors the all-replays nuke button in
                        // Settings.
                        if pending_delete == Some(r.game_id) {
                            if danger_button(ui, "Delete?")
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

    /// Shared "you haven't set a player code" prompt for the Analytics and
    /// Career pages, both of which are per-code.
    fn render_set_code_prompt(&mut self, ui: &mut egui::Ui) {
        ui.label(
            egui::RichText::new("Set your player code in Settings to see your stats.")
                .italics()
                .color(egui::Color32::GRAY),
        );
        if ui.button("Go to Settings").clicked() {
            self.page = Page::Settings;
        }
    }

    /// Kick the summary worker if the cached summaries are stale for the
    /// current code + shared filter. Shared by the Analytics and Career
    /// pages — both read from the same `(filtered_summary, career_summary,
    /// win_analytics)` triple computed in one worker pass.
    fn ensure_summary_loaded(&mut self, code: &str) {
        let target_key = self.summary_key(code);
        let needs_load = self.summary_error.is_none()
            && self.db_conn.is_some()
            && !self.summary_loading
            && self.summary_for.as_ref() != Some(&target_key);
        if needs_load {
            self.reload_summary();
        }
    }

    /// A compact "Filters · toggle · refresh · for CODE" action bar shared by
    /// the Analytics and Career pages.
    fn render_stats_action_bar(&mut self, ui: &mut egui::Ui, code: &str, show_filter_toggle: bool) {
        ui.horizontal(|ui| {
            if show_filter_toggle {
                let toggle = if self.show_filter_panel {
                    "◀ Hide filters"
                } else {
                    "☰ Filters"
                };
                if ui.button(toggle).clicked() {
                    self.show_filter_panel = !self.show_filter_panel;
                }
                ui.add_space(8.0);
            }
            if ui.button("Refresh").clicked() {
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
        if let Some(err) = &self.db_error {
            ui.colored_label(egui::Color32::RED, format!("DB error: {err}"));
        }
        if let Some(err) = &self.summary_error {
            ui.colored_label(egui::Color32::RED, format!("Summary error: {err}"));
        }
    }

    fn page_analytics(&mut self, ui: &mut egui::Ui) {
        let code = self.config.user_player_code.trim().to_string();
        if code.is_empty() {
            self.render_set_code_prompt(ui);
            return;
        }
        self.ensure_db();
        self.render_stats_action_bar(ui, &code, true);
        ui.add_space(8.0);
        self.ensure_summary_loaded(&code);

        // Header: the shared library filter described in words, so it's clear
        // these numbers track the library view.
        ui.label(egui::RichText::new("Analytics").size(18.0).strong());
        ui.add_space(2.0);
        ui.label(
            egui::RichText::new(self.filter_description())
                .color(egui::Color32::from_gray(150)),
        );
        ui.add_space(10.0);

        if let Some(filtered) = self.filtered_summary.clone() {
            let character_filter = self.library_character_filter;
            self.render_filtered_section(ui, &filtered, character_filter);
        } else if self.summary_loading {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(
                    egui::RichText::new("Computing…")
                        .italics()
                        .color(egui::Color32::GRAY),
                );
            });
        } else if self.summary_error.is_none() {
            ui.label(
                egui::RichText::new("Loading…")
                    .italics()
                    .color(egui::Color32::GRAY),
            );
        }

        // Clearance for the floating bottom nav toggle.
        ui.add_space(64.0);
    }

    /// The Career page: whole-history identity. Headline totals + favorites
    /// (favorite character / stage / matchup / opponent) and the career
    /// win-rate breakdowns. Filter-independent — it always shows the full
    /// history regardless of the shared library filter.
    fn page_career(&mut self, ui: &mut egui::Ui) {
        let code = self.config.user_player_code.trim().to_string();
        if code.is_empty() {
            self.render_set_code_prompt(ui);
            return;
        }
        self.ensure_db();
        // The trend graphs read the in-memory rows (date + outcome), so make
        // sure they're loaded even on a direct landing on the Career page.
        self.ensure_rows_loaded();
        self.render_stats_action_bar(ui, &code, false);
        ui.add_space(8.0);
        self.ensure_summary_loaded(&code);

        ui.label(egui::RichText::new("Career").size(18.0).strong());
        ui.add_space(2.0);
        ui.label(
            egui::RichText::new("Your whole history at a glance.")
                .small()
                .color(egui::Color32::from_gray(140)),
        );
        ui.add_space(10.0);

        if self.career_summary.is_some() && self.win_analytics.is_some() {
            self.render_career_overview(ui);
        } else if self.summary_loading {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(
                    egui::RichText::new("Computing…")
                        .italics()
                        .color(egui::Color32::GRAY),
                );
            });
        } else if self.summary_error.is_none() {
            ui.label(
                egui::RichText::new("Loading…")
                    .italics()
                    .color(egui::Color32::GRAY),
            );
        }

        // Career win-rate breakdowns (own separator + heading; early-returns
        // when `win_analytics` is None).
        ui.add_space(12.0);
        ui.separator();
        ui.add_space(8.0);
        self.render_win_breakdowns(ui);

        // Clearance for the floating bottom nav toggle.
        ui.add_space(64.0);
    }

    /// Headline career totals + "favorites" cards, drawn from the unfiltered
    /// `career_summary` and the career `win_analytics`.
    fn render_career_overview(&mut self, ui: &mut egui::Ui) {
        let Self {
            career_summary,
            win_analytics,
            icons,
            ..
        } = self;
        let (Some(cs), Some(wa)) = (career_summary.as_ref(), win_analytics.as_ref()) else {
            return;
        };

        if cs.games_played == 0 {
            ui.label(
                egui::RichText::new("No games recorded yet. Ingest some replays first.")
                    .italics()
                    .color(egui::Color32::GRAY),
            );
            return;
        }

        // Headline totals.
        ui.horizontal_wrapped(|ui| {
            metric_card(ui, "Total matches", &cs.games_played.to_string());
            metric_card(ui, "Win rate", &fmt_opt_percent(cs.win_rate()));
            let losses = (cs.games_played - cs.wins).max(0);
            metric_card(ui, "Record", &format!("{}\u{2013}{}", cs.wins, losses));
            metric_card(ui, "Total playtime", &fmt_playtime(cs.total_seconds));
            metric_card(ui, "Stocks taken", &cs.total_stocks_taken.to_string());
            metric_card(ui, "Stocks lost", &cs.total_stocks_lost.to_string());
        });
        ui.add_space(14.0);

        ui.label(egui::RichText::new("Favorites").size(15.0).strong());
        ui.add_space(6.0);
        ui.horizontal_wrapped(|ui| {
            if let Some((cid, wp)) = argmax_winproportion(&wa.played_characters) {
                let name = spaced_name(CHARACTERS.get(cid).copied().unwrap_or("Unknown"));
                favorite_card(ui, "Favorite character", &name, wp.total, |ui| {
                    crate::icons::character_icon(ui, icons, cid as i32, 22.0)
                });
            }
            if let Some((sid, wp)) = argmax_winproportion(&wa.stages) {
                let name = spaced_name(STAGES.get(sid).copied().unwrap_or("Unknown"));
                favorite_card(ui, "Favorite stage", &name, wp.total, |ui| {
                    crate::icons::stage_icon(ui, icons, sid as i32, 22.0)
                });
            }
            if let Some((cid, wp)) = argmax_winproportion(&wa.opp_characters) {
                let name = spaced_name(CHARACTERS.get(cid).copied().unwrap_or("Unknown"));
                favorite_card(ui, "Most-faced character", &name, wp.total, |ui| {
                    crate::icons::character_icon(ui, icons, cid as i32, 22.0)
                });
            }
            // Top opponent — keyed by connect code, so no icon.
            if let Some((opp, wp)) = wa
                .opponents
                .iter()
                .filter(|(_, wp)| wp.total > 0)
                .max_by_key(|(_, wp)| wp.total)
            {
                favorite_card(ui, "Top opponent", opp, wp.total, |_ui| {});
            }
        });
    }

    /// Kick off a background recompute of the summaries for the current code
    /// + shared library filter. One worker pass computes three things that
    /// land on the same frame: the filtered summary (Analytics — restricted
    /// to the games the library filter is showing), the whole-career summary,
    /// and the career win-rate breakdowns (both for the Career page). Results
    /// land back via [`poll_summary_worker`].
    ///
    /// `diesel::SqliteConnection` is `!Send`, so we can't hand the one owned
    /// by `self` to the worker. We give the worker its own path and let it
    /// open a second connection — SQLite is happy with a second handle. The
    /// full multi-dimensional library filter is threaded down as an explicit
    /// game-id set (computed here from the in-memory rows), so the filtered
    /// summary reflects exactly the filtered library view.
    fn reload_summary(&mut self) {
        let code = self.config.user_player_code.trim().to_string();
        if code.is_empty() {
            self.filtered_summary = None;
            self.career_summary = None;
            self.win_analytics = None;
            self.summary_error = None;
            self.summary_for = None;
            self.summary_rx = None;
            self.summary_loading = false;
            return;
        }

        let key = self.summary_key(&code);

        let db_path = match self.config.effective_db_path() {
            Ok(p) => p,
            Err(e) => {
                self.filtered_summary = None;
                self.career_summary = None;
                self.win_analytics = None;
                self.summary_error = Some(e.to_string());
                self.summary_for = Some(key);
                return;
            }
        };

        // Build the game-id restriction from the in-memory rows. Only pass it
        // when a structured filter is active — the whole-career case stays a
        // plain unfiltered aggregate (and avoids a needlessly huge IN-list).
        let game_ids = if self.structured_filter_active() {
            Some(self.library_filtered_game_ids())
        } else {
            None
        };
        let filter = PlayerSummaryFilter {
            character_id: None,
            stage_id: None,
            game_ids,
        };

        // The career bundle (filter-independent) only needs recomputing when
        // the code or underlying data changed — not on a filter-only tweak.
        // `summary_for` holds the *previous* key; a differing code (or any of
        // the explicit cache resets, which null these out) forces a refresh.
        let recompute_career = self.career_summary.is_none()
            || self.win_analytics.is_none()
            || self.summary_for.as_ref().map(|k| k.code.as_str()) != Some(code.as_str());

        let (tx, rx) = mpsc::channel::<SummaryMsg>();
        let ctx_for_thread = self.egui_ctx.clone();
        let code_for_thread = code.clone();

        thread::spawn(move || {
            let msg = match stats_melee::open_database(&db_path) {
                Ok(mut conn) => {
                    let filtered = stats_melee::player_summary_filtered(
                        &mut conn,
                        &code_for_thread,
                        &filter,
                    );
                    let career = if recompute_career {
                        let c = stats_melee::player_summary_filtered(
                            &mut conn,
                            &code_for_thread,
                            &PlayerSummaryFilter::NONE,
                        );
                        let a = stats_melee::win_analytics(&mut conn, &code_for_thread);
                        match (c, a) {
                            (Ok(c), Ok(a)) => Ok(Some((c, a))),
                            (Err(e), _) | (_, Err(e)) => Err(e),
                        }
                    } else {
                        Ok(None)
                    };
                    match (filtered, career) {
                        (Ok(f), Ok(bundle)) => SummaryMsg::Ok(f, bundle),
                        (Err(e), _) | (_, Err(e)) => SummaryMsg::Err(e.to_string()),
                    }
                }
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
            Ok(SummaryMsg::Ok(filtered, career_bundle)) => {
                self.filtered_summary = Some(filtered);
                // A filter-only change carries `None` — keep the existing
                // career data rather than clearing it.
                if let Some((career, a)) = career_bundle {
                    self.career_summary = Some(career);
                    self.win_analytics = Some(a);
                }
                self.summary_error = None;
                self.summary_loading = false;
                self.summary_rx = None;
            }
            Ok(SummaryMsg::Err(e)) => {
                self.filtered_summary = None;
                self.career_summary = None;
                self.win_analytics = None;
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

    /// The Analytics body: the win rate + metric cards for the filtered game
    /// set and the character-gated top-kill-moves table. The surrounding
    /// header (which describes the active filter) is owned by the caller.
    /// `character_filter` is the shared library "my character" filter — it
    /// gates the kill-moves table (attack ids are only meaningful per
    /// character). Renders an empty-state line when the filter matched no
    /// games.
    fn render_filtered_section(
        &self,
        ui: &mut egui::Ui,
        s: &PlayerSummary,
        character_filter: Option<i32>,
    ) {
        if s.games_played == 0 {
            let msg = if !self.structured_filter_active() {
                format!(
                    "No games recorded yet for {}. Ingest some replays first.",
                    s.code
                )
            } else {
                "No games match the current filter.".to_string()
            };
            ui.label(egui::RichText::new(msg).italics().color(egui::Color32::GRAY));
            return;
        }

        ui.label(
            egui::RichText::new(format!("{} games", s.games_played))
                .color(egui::Color32::from_gray(140)),
        );
        ui.add_space(10.0);

        // Metrics on the left, top kill moves on the right. The kill-moves
        // table is narrow, so stacking it below the metric cards left a tall
        // empty gutter; sitting it alongside the cards keeps the section
        // compact vertically.
        ui.horizontal_top(|ui| {
            ui.vertical(|ui| {
                ui.set_width(600.0);
                summary_metrics_block(ui, s);
            });
            ui.add_space(24.0);
            ui.vertical(|ui| {
                render_top_kill_moves(ui, s, character_filter);
            });
        });

        // Advanced aggregate metrics (from the per-game `advanced` counters).
        ui.add_space(16.0);
        ui.label(egui::RichText::new("Advanced").size(15.0).strong());
        ui.add_space(6.0);
        let adv = &s.advanced;
        ui.horizontal_wrapped(|ui| {
            metric_card(ui, "Damage / opening", &fmt_opt_f64(adv.avg_damage_per_opening, 1));
            metric_card(ui, "Edge-guard %", &fmt_opt_percent(adv.edgeguard_success));
            metric_card(ui, "First-blood win %", &fmt_opt_percent(adv.first_blood_win_rate));
            metric_card(ui, "Comeback rate", &fmt_opt_percent(adv.comeback_rate));
            metric_card(ui, "Avg death", &fmt_opt_death_percent(adv.avg_death_percent));
        });
    }

    /// Career win-rate breakdowns under the summary cards: by the
    /// character the player picked, by opponent-character matchup, by
    /// stage, and by opponent code. Each is a sorted top-N list of
    /// icon + name + win-rate bar + record. Filter-independent.
    fn render_win_breakdowns(&mut self, ui: &mut egui::Ui) {
        // Disjoint borrows: read the analytics while mutating the icon
        // texture cache.
        let Self {
            win_analytics,
            icons,
            ..
        } = self;
        let Some(wa) = win_analytics.as_ref() else {
            return;
        };

        ui.separator();
        ui.add_space(8.0);
        ui.label(
            egui::RichText::new("Win-rate breakdowns")
                .size(16.0)
                .strong(),
        );
        ui.add_space(8.0);

        const TOP_N: usize = 8;

        // Two side-by-side columns of sections so the page stays compact.
        ui.columns(2, |cols| {
            char_winrate_section(
                &mut cols[0],
                icons,
                "Your characters",
                &wa.played_characters,
                TOP_N,
            );
            char_winrate_section(
                &mut cols[1],
                icons,
                "Matchups (vs)",
                &wa.opp_characters,
                TOP_N,
            );
        });
        ui.add_space(12.0);
        ui.columns(2, |cols| {
            stage_winrate_section(&mut cols[0], icons, "Stages", &wa.stages, TOP_N);
            opponent_winrate_section(&mut cols[1], "Top opponents", &wa.opponents, TOP_N);
        });
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
            let open_btn = egui::Button::new(
                egui::RichText::new("▶ Open in Slippi").color(ON_ACCENT).strong(),
            )
            .fill(ACCENT);
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
                        "✓ Launched in Slippi.",
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

        // Disjoint borrow: the viewer renders character/stage icons (mutating
        // the icon cache) while reading the viewer state.
        let Self {
            viewer_state,
            icons,
            ..
        } = &mut *self;
        match viewer_state {
            Some(Ok(state)) => {
                viewer::render_viewer(ui, icons, state);
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
                    self.filtered_summary = None;
                    self.career_summary = None;
                    self.win_analytics = None;
                    self.summary_error = None;
                    self.summary_for = None;
                    self.summary_rx = None;
                    self.summary_loading = false;
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

        // Left filter menu — shared by the Library and Analytics pages (both
        // react to the same structured filter), and only when shown. Added
        // before the CentralPanel so it carves space off the left edge and
        // the centered content flows in the remaining width. Ensure the rows
        // are loaded first so the panel's histograms / autocomplete and the
        // Analytics game-id set have data even on a direct landing.
        if matches!(self.page, Page::ReplayLibrary | Page::Analytics) {
            self.ensure_db();
            self.ensure_rows_loaded();
            if self.show_filter_panel {
                self.render_filter_panel(ctx);
            }
        }

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
                        // Constrain only the *width* and let height flow
                        // naturally — the replay table virtualizes its
                        // rows against the available height, so pinning a
                        // fixed height here collapses it to zero rows.
                        ui.vertical(|ui| {
                            ui.set_width(content_w);
                            self.main_panel(ui);
                        });
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

/// Render an `Option<f64>` that's already a melee damage percent (0..200+),
/// e.g. an average death percent, as `NN%` — *not* scaled by 100 like a
/// `[0,1]` ratio.
fn fmt_opt_death_percent(v: Option<f64>) -> String {
    match v {
        Some(x) => format!("{x:.0}%"),
        None => "—".to_string(),
    }
}

/// Format a total duration in seconds as a compact `Hh Mm` read-out
/// ("12h 34m"), dropping the hours below an hour ("45m") and the minutes
/// when exactly zero ("3h"). Sub-minute totals round up to "<1m" so a
/// non-empty history never reads "0m".
fn fmt_playtime(total_seconds: i64) -> String {
    let total = total_seconds.max(0);
    let hours = total / 3600;
    let minutes = (total % 3600) / 60;
    if hours == 0 && minutes == 0 {
        return if total > 0 { "<1m".to_string() } else { "0m".to_string() };
    }
    match (hours, minutes) {
        (0, m) => format!("{m}m"),
        (h, 0) => format!("{h}h"),
        (h, m) => format!("{h}h {m}m"),
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
/// The render pipeline requires a patched vanilla Dolphin master build
/// (interpreter mode, --user isolation), NOT Slippi Playback Dolphin. It's
/// resolved solely from the `slippi_playback_command` config override — the
/// render feature is parked ([`RENDER_VIDEO_FEATURE_ENABLED`]), so there's no
/// platform auto-discovery here. `None` when no override is set.
fn resolve_vanilla_dolphin_binary(config: &AppConfig) -> Option<PathBuf> {
    use crate::slippi::predict_app_inner_binary;

    config
        .slippi_playback_command
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| {
            let raw = PathBuf::from(s);
            predict_app_inner_binary(&raw).unwrap_or(raw)
        })
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
            .map(|s| spaced_name(s))
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
            .map(|name| spaced_name(name))
            .unwrap_or_else(|| format!("stage #{s}")),
    }
}

/// Display label for a [`SortKey`] in the Replay Library sort dropdown.
/// "Newest" reads better than "Ingested at" for the default chronological
/// ordering.
fn sort_key_label(key: SortKey) -> &'static str {
    match key {
        SortKey::IngestedAt => "Newest (added)",
        SortKey::PlayedAt => "Date played",
        SortKey::GameId => "Game ID",
        SortKey::Stage => "Stage",
        SortKey::Duration => "Duration",
        SortKey::Outcome => "Outcome",
    }
}

// --- Date helpers (dependency-free) -------------------------------------------
//
// We store played dates as ISO-8601 strings and only need two things the
// standard library can't give us without a date crate: a monotonic day
// number for the range slider, and its inverse to turn a slider position
// back into a `YYYY-MM-DD` string. Howard Hinnant's `days_from_civil` /
// `civil_from_days` (public-domain) do exactly that, branch-free.

/// Parse the leading `YYYY-MM-DD` of an ISO date/time string into `(y, m, d)`.
fn parse_iso_ymd(s: &str) -> Option<(i64, i64, i64)> {
    let b = s.as_bytes();
    if b.len() < 10 || b[4] != b'-' || b[7] != b'-' {
        return None;
    }
    let y: i64 = s.get(0..4)?.parse().ok()?;
    let m: i64 = s.get(5..7)?.parse().ok()?;
    let d: i64 = s.get(8..10)?.parse().ok()?;
    if (1..=12).contains(&m) && (1..=31).contains(&d) {
        Some((y, m, d))
    } else {
        None
    }
}

/// Days since 1970-01-01 for a proleptic-Gregorian `(y, m, d)`.
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400; // [0, 399]
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1; // [0, 365]
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy; // [0, 146096]
    era * 146097 + doe - 719468
}

/// Inverse of [`days_from_civil`]: day number → `(y, m, d)`.
fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = doy - (153 * mp + 2) / 5 + 1; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// `YYYY-MM-DD` prefix → day ordinal, or `None` if it doesn't parse.
fn date_to_ordinal(s: &str) -> Option<i64> {
    let (y, m, d) = parse_iso_ymd(s)?;
    Some(days_from_civil(y, m, d))
}

/// Day ordinal → `YYYY-MM-DD`.
fn ordinal_to_date(z: i64) -> String {
    let (y, m, d) = civil_from_days(z);
    format!("{y:04}-{m:02}-{d:02}")
}

/// A double-thumb range slider over the inclusive integer domain
/// `[min, max]`. Edits `lo`/`hi` in place (kept ordered and clamped) and
/// returns `true` when either thumb moved this frame. The active thumb is
/// whichever is nearer the pointer, so dragging from anywhere on the track
/// grabs the closest handle.
fn range_slider(ui: &mut egui::Ui, lo: &mut i64, hi: &mut i64, min: i64, max: i64) -> bool {
    // Compact: a low-profile track so the menu's two date filters don't
    // each eat a tall band. Inset horizontally by the thumb radius so the
    // end thumbs sit fully inside the allocated rect (their centers reach
    // the track ends rather than overhanging).
    let height = 14.0;
    let thumb_r = 5.0;
    let width = ui.available_width().max(60.0);
    let (outer, resp) =
        ui.allocate_exact_size(egui::vec2(width, height), egui::Sense::click_and_drag());
    let rect = outer.shrink2(egui::vec2(thumb_r, 0.0));

    let span = (max - min).max(1) as f32;
    let x_of = |v: i64| {
        rect.left() + ((v - min) as f32 / span) * rect.width()
    };
    let v_of_x = |x: f32| {
        let t = ((x - rect.left()) / rect.width()).clamp(0.0, 1.0);
        min + (t * span).round() as i64
    };

    let track_y = rect.center().y;
    let painter = ui.painter();
    // Track.
    painter.line_segment(
        [
            egui::pos2(rect.left(), track_y),
            egui::pos2(rect.right(), track_y),
        ],
        egui::Stroke::new(3.0, egui::Color32::from_gray(80)),
    );
    let lo_x = x_of(*lo);
    let hi_x = x_of(*hi);
    // Selected span.
    painter.line_segment(
        [egui::pos2(lo_x, track_y), egui::pos2(hi_x, track_y)],
        egui::Stroke::new(3.0, ACCENT),
    );
    // Thumbs.
    painter.circle_filled(egui::pos2(lo_x, track_y), thumb_r, ACCENT);
    painter.circle_filled(egui::pos2(hi_x, track_y), thumb_r, ACCENT);

    let mut changed = false;
    if resp.dragged() || resp.clicked() {
        if let Some(p) = resp.interact_pointer_pos() {
            let v = v_of_x(p.x);
            // Grab the nearer thumb; clamp so the two can't cross.
            if (p.x - lo_x).abs() <= (p.x - hi_x).abs() {
                let nv = v.clamp(min, *hi);
                if nv != *lo {
                    *lo = nv;
                    changed = true;
                }
            } else {
                let nv = v.clamp(*lo, max);
                if nv != *hi {
                    *hi = nv;
                    changed = true;
                }
            }
        }
    }
    changed
}

/// Inclusive min/max of a list of day ordinals, or `None` when empty. Used to
/// derive a date filter's slider domain from its per-game ordinal list.
fn ordinal_domain(ordinals: &[i64]) -> Option<(i64, i64)> {
    let mut it = ordinals.iter().copied();
    let first = it.next()?;
    Some(it.fold((first, first), |(lo, hi), v| (lo.min(v), hi.max(v))))
}

/// A small density histogram of game counts across the date domain
/// `[min, max]`, drawn directly above a [`range_slider`] and sharing its
/// horizontal inset so the bars line up with the slider track. Each game's
/// day ordinal falls into one of up to 48 equal-width bins; bar heights are
/// normalized to the busiest bin. Purely decorative (no interaction).
fn render_date_histogram(ui: &mut egui::Ui, ordinals: &[i64], min: i64, max: i64) {
    let height = 26.0;
    let thumb_r = 5.0; // match range_slider's horizontal inset
    let width = ui.available_width().max(60.0);
    let (outer, _resp) =
        ui.allocate_exact_size(egui::vec2(width, height), egui::Sense::hover());
    let rect = outer.shrink2(egui::vec2(thumb_r, 0.0));
    if !ui.is_rect_visible(rect) {
        return;
    }

    let span = (max - min).max(1);
    let bins = ((span + 1).clamp(1, 48)) as usize;
    let mut counts = vec![0u32; bins];
    for &o in ordinals {
        if o < min || o > max {
            continue;
        }
        let t = (o - min) as f64 / span as f64; // 0..=1
        let b = ((t * bins as f64).floor() as usize).min(bins - 1);
        counts[b] += 1;
    }
    let max_count = counts.iter().copied().max().unwrap_or(0);
    let painter = ui.painter();
    // Faint baseline so an all-empty domain still reads as "a chart".
    painter.line_segment(
        [
            egui::pos2(rect.left(), rect.bottom()),
            egui::pos2(rect.right(), rect.bottom()),
        ],
        egui::Stroke::new(1.0, ui.visuals().weak_text_color().linear_multiply(0.5)),
    );
    if max_count == 0 {
        return;
    }
    let bin_w = rect.width() / bins as f32;
    let color = ACCENT.linear_multiply(0.6);
    for (i, &c) in counts.iter().enumerate() {
        if c == 0 {
            continue;
        }
        // Reserve a 1px floor so a single-game bin is still visible.
        let h = (c as f32 / max_count as f32) * (rect.height() - 1.0) + 1.0;
        let x0 = rect.left() + i as f32 * bin_w;
        let bar = egui::Rect::from_min_max(
            egui::pos2(x0 + 0.5, rect.bottom() - h),
            egui::pos2(x0 + bin_w - 0.5, rect.bottom()),
        );
        painter.rect_filled(bar, egui::Rounding::same(1.0), color);
    }
}

/// A labeled date-range filter: a small game-density histogram over two
/// auto-formatting `YYYY-MM-DD` text boxes (see [`date_text_edit`]) and a
/// compact double-thumb [`range_slider`], with the track's extreme dates
/// labeled at each end. The text boxes are the source of truth for filtering;
/// the slider is a convenience that writes them. `domain` is the min/max
/// present date as day ordinals — a single-day domain is padded so the slider
/// still renders; `None` (no dated rows) disables it. `ordinals` is one day-
/// ordinal per game, bucketed into the histogram. Shared by the "Date played"
/// and "Date added" filters.
fn render_date_range_filter(
    ui: &mut egui::Ui,
    label: &str,
    from: &mut String,
    to: &mut String,
    domain: Option<(i64, i64)>,
    ordinals: &[i64],
) {
    filter_field_label(ui, label);
    ui.horizontal(|ui| {
        date_text_edit(ui, label, "from", from, "From");
        ui.label("–");
        date_text_edit(ui, label, "to", to, "To");
    });
    ui.add_space(4.0);

    match domain {
        Some((data_min, data_max)) => {
            // Pad a single-day domain (e.g. every row ingested in one scan)
            // so the slider is still drawn and draggable, rather than
            // collapsing both thumbs onto one point and effectively vanishing.
            let (slider_min, slider_max) = if data_max > data_min {
                (data_min, data_max)
            } else {
                (data_min - 7, data_max + 7)
            };
            // Density histogram, sharing the slider's domain so its bars line
            // up with the track below it.
            render_date_histogram(ui, ordinals, slider_min, slider_max);
            // Seed the thumbs from the text boxes, falling back to the full
            // (padded) domain when a box is empty / mid-typing.
            let mut lo = date_to_ordinal(from.trim())
                .unwrap_or(slider_min)
                .clamp(slider_min, slider_max);
            let mut hi = date_to_ordinal(to.trim())
                .unwrap_or(slider_max)
                .clamp(slider_min, slider_max);
            if lo > hi {
                std::mem::swap(&mut lo, &mut hi);
            }
            if range_slider(ui, &mut lo, &mut hi, slider_min, slider_max) {
                *from = ordinal_to_date(lo);
                *to = ordinal_to_date(hi);
            }
            // Track extremes anchored under the ends so the user can see the
            // available range at a glance: earliest on the left, latest
            // flushed to the right.
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new(ordinal_to_date(slider_min))
                        .small()
                        .color(egui::Color32::from_gray(130)),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(
                        egui::RichText::new(ordinal_to_date(slider_max))
                            .small()
                            .color(egui::Color32::from_gray(130)),
                    );
                });
            });
        }
        None => {
            ui.label(
                egui::RichText::new("No dates yet — re-scan to enable the slider.")
                    .small()
                    .italics()
                    .color(egui::Color32::from_gray(130)),
            );
        }
    }
}

/// Reformat free-form date input to a `YYYY-MM-DD` mask: keep up to 8 leading
/// digits and group them year(4)-month(2)-day(2). Forgiving of any separator
/// style or partial entry — `"20250401"`, `"2025/4/1"`, or mid-typing — so
/// the user never has to type the dashes themselves.
fn autoformat_ymd(s: &str) -> String {
    let digits: String = s.chars().filter(|c| c.is_ascii_digit()).take(8).collect();
    let mut out = String::with_capacity(10);
    for (i, c) in digits.chars().enumerate() {
        if i == 4 || i == 6 {
            out.push('-');
        }
        out.push(c);
    }
    out
}

/// A `YYYY-MM-DD` text box that auto-inserts the dashes as the user types
/// (see [`autoformat_ymd`]) so dates are quick to enter. `field` ("from"/"to")
/// disambiguates the two boxes within one filter and `label` disambiguates the
/// two filters — together they make a stable widget id so the caret fix
/// targets the right box.
fn date_text_edit(ui: &mut egui::Ui, label: &str, field: &str, value: &mut String, hint: &str) {
    let id = ui.make_persistent_id((label, field));
    let resp = ui.add(
        egui::TextEdit::singleline(value)
            .id(id)
            .hint_text(hint)
            .desired_width(88.0),
    );
    if resp.changed() {
        let formatted = autoformat_ymd(value);
        if formatted != *value {
            *value = formatted;
            // Inserting dashes shifts character positions; snap the caret to
            // the end so the next keystroke appends instead of landing before
            // an auto-inserted dash.
            if let Some(mut state) = egui::TextEdit::load_state(ui.ctx(), id) {
                let end = egui::text::CCursor::new(value.chars().count());
                state
                    .cursor
                    .set_char_range(Some(egui::text::CCursorRange::one(end)));
                egui::TextEdit::store_state(ui.ctx(), id, state);
            }
        }
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
                let text = format!("{} ({})", s.code, spaced_name(s.character_name()));
                let mut rich = egui::RichText::new(text);
                if is_me {
                    rich = rich.color(ACCENT).strong();
                }
                // Truncate (ellipsis) rather than clip mid-glyph if an
                // unusually long code+name exceeds the fixed column width.
                ui.add(egui::Label::new(rich).truncate());
            });
        }
    }
}

/// One segment of the floating bottom nav toggle. Returns `true` when
/// clicked. Active segment is filled with [`ACCENT`]; inactive is a
/// transparent pill that lights up on hover.
fn view_pill(ui: &mut egui::Ui, current: Page, target: Page, label: &str) -> bool {
    let selected = current == target;
    // Selected: dark text on the gold accent. Inactive: muted text on a
    // transparent pill that lights up on hover.
    let text_color = if selected { ON_ACCENT } else { TEXT_MUTED };
    let btn = egui::Button::new(
        egui::RichText::new(label)
            .size(13.5)
            .strong()
            .color(text_color),
    )
    .min_size(egui::vec2(104.0, 30.0))
    .rounding(egui::Rounding::same(999.0))
    .fill(if selected {
        ACCENT
    } else {
        egui::Color32::TRANSPARENT
    });
    ui.add(btn).clicked()
}

/// Index + win-proportion of the most-played entry in a per-id win-rate
/// array (the "favorite" — highest `total`), or `None` when every entry is
/// empty. Used for the Career page's favorite character / stage / matchup.
fn argmax_winproportion(arr: &[WinProportion]) -> Option<(usize, &WinProportion)> {
    arr.iter()
        .enumerate()
        .filter(|(_, wp)| wp.total > 0)
        .max_by_key(|(_, wp)| wp.total)
}

/// A "favorite" card for the Career page: a small label over an icon + name,
/// with a "{games} games" subtitle. The icon is drawn by `draw_icon` so
/// character / stage / icon-less (opponent) variants share one renderer.
fn favorite_card(
    ui: &mut egui::Ui,
    label: &str,
    name: &str,
    games: i32,
    draw_icon: impl FnOnce(&mut egui::Ui),
) {
    let fill = surface_fill(ui.visuals());
    egui::Frame::none()
        .fill(fill)
        .rounding(egui::Rounding::same(8.0))
        .inner_margin(egui::Margin::symmetric(14.0, 11.0))
        .show(ui, |ui| {
            ui.vertical(|ui| {
                ui.set_min_width(150.0);
                ui.label(
                    egui::RichText::new(label)
                        .size(12.0)
                        .color(egui::Color32::from_gray(145)),
                );
                ui.add_space(5.0);
                ui.horizontal(|ui| {
                    draw_icon(ui);
                    ui.add_space(6.0);
                    ui.add(
                        egui::Label::new(egui::RichText::new(name).size(16.0).strong())
                            .truncate(),
                    );
                });
                ui.add_space(2.0);
                ui.label(
                    egui::RichText::new(format!("{games} games"))
                        .small()
                        .color(egui::Color32::from_gray(135)),
                );
            });
        });
}

/// The character-gated "Top kill moves" panel, rendered to the right of the
/// metric cards in the per character/stage section. Without a character
/// filter the rolled-up move distribution mixes attack ids that mean
/// different moves for different characters (id 23 is Falcon Punch for
/// Falcon but Marth's Counter for Marth), so we gate on a character pick and
/// prompt for one otherwise. With a character active the ids are
/// character-consistent and resolve through
/// [`stats_melee::gamedata::attack_display_name`].
fn render_top_kill_moves(ui: &mut egui::Ui, s: &PlayerSummary, character_filter: Option<i32>) {
    ui.strong("Top kill moves");
    ui.add_space(2.0);
    match character_filter {
        None => {
            ui.label(
                egui::RichText::new("Pick a character to see your most common kill moves.")
                    .italics()
                    .color(egui::Color32::GRAY),
            );
        }
        Some(_) => {
            if s.top_kill_moves.is_empty() {
                ui.label(
                    egui::RichText::new("No kill moves recorded yet.")
                        .italics()
                        .color(egui::Color32::GRAY),
                );
            } else {
                // (No explicit id_source/id_salt — egui_extras 0.29 derives
                // one from widget position, and the two tables in this app
                // never render on the same frame.)
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
                                ui.label(stats_melee::gamedata::attack_display_name(attack_id));
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

/// Shared metric block for the Analytics per-character/stage section: the
/// headline win-rate bar, the metric cards, the win/loss streak banner, the
/// L-cancel progress bar, and the secondary cards. Takes a plain
/// `&PlayerSummary` — the caller owns the surrounding header.
fn summary_metrics_block(ui: &mut egui::Ui, s: &PlayerSummary) {
    // Win rate — the headline number for the current filter. A colored bar
    // (same ramp as the breakdowns) plus the percentage and W–L record.
    if let Some(rate) = s.win_rate() {
        let losses = (s.games_played - s.wins).max(0);
        ui.horizontal(|ui| {
            draw_win_bar(ui, rate as f32, 200.0, 16.0);
            ui.add_space(10.0);
            ui.label(
                egui::RichText::new(format!("{:.0}% win rate", rate * 100.0))
                    .size(16.0)
                    .strong(),
            );
            ui.add_space(8.0);
            ui.label(
                egui::RichText::new(format!("{}–{}", s.wins, losses))
                    .color(egui::Color32::from_gray(150)),
            );
        });
        ui.add_space(12.0);
    }

    // Headline metric cards.
    ui.horizontal_wrapped(|ui| {
        // `avg_placement` is stored 0-indexed (0 = 1st); display it 1-indexed
        // (1 = 1st … 4 = 4th) so it reads like an actual finishing position.
        metric_card(
            ui,
            "Avg placement",
            &fmt_opt_f64(s.avg_placement.map(|p| p + 1.0), 2),
        );
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
        // Tint the panel background toward the streak color rather than
        // dimming the color toward black — `linear_multiply` looked fine on
        // dark but produced a near-black bubble on a light background.
        let bg = ui.visuals().panel_fill;
        egui::Frame::none()
            .fill(mix_color(bg, color, 0.20))
            .stroke(egui::Stroke::new(1.0, mix_color(bg, color, 0.60)))
            .rounding(egui::Rounding::same(8.0))
            .inner_margin(egui::Margin::symmetric(14.0, 8.0))
            .show(ui, |ui| {
                ui.label(egui::RichText::new(label).color(color).strong());
            });
        ui.add_space(8.0);
        ui.label(
            egui::RichText::new(format!(
                "Longest: {} W · {} L",
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
}

/// One full-width clickable row in the Analytics character/stage
/// dropdowns: a leading icon + label spanning the whole combo width, so a
/// click anywhere on the row selects it — not just the text. Returns `true`
/// when clicked. Paints the standard selectable hover/selected highlight so
/// it still reads like a normal menu entry.
fn icon_select_row(
    ui: &mut egui::Ui,
    selected: bool,
    label: &str,
    draw_icon: impl FnOnce(&mut egui::Ui),
) -> bool {
    let width = ui.available_width();
    let height = ui.spacing().interact_size.y.max(20.0);
    let (rect, resp) =
        ui.allocate_exact_size(egui::vec2(width, height), egui::Sense::click());
    if ui.is_rect_visible(rect) {
        let visuals = ui.style().interact_selectable(&resp, selected);
        if selected || resp.hovered() {
            ui.painter()
                .rect_filled(rect, visuals.rounding, visuals.weak_bg_fill);
        }
        // Draw the icon + label into the row rect, vertically centered.
        let mut content = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(rect.shrink2(egui::vec2(6.0, 0.0)))
                .layout(egui::Layout::left_to_right(egui::Align::Center)),
        );
        draw_icon(&mut content);
        content.add_space(6.0);
        content.label(egui::RichText::new(label).color(visuals.text_color()));
    }
    resp.clicked()
}

/// Small muted field label used above each filter widget in the library
/// filter menu.
fn filter_field_label(ui: &mut egui::Ui, text: &str) {
    ui.add_space(6.0);
    ui.label(
        egui::RichText::new(text)
            .size(12.0)
            .color(egui::Color32::from_gray(150)),
    );
    ui.add_space(2.0);
}

/// A character filter combo: the selected icon (when set) + a dropdown of
/// "Any" plus the playable cast, each row icon + spaced name and fully
/// clickable. Mutates `sel` in place.
fn character_filter_combo(
    ui: &mut egui::Ui,
    icons: &mut crate::icons::IconCache,
    id_salt: &str,
    sel: &mut Option<i32>,
) {
    ui.horizontal(|ui| {
        if let Some(cid) = *sel {
            crate::icons::character_icon(ui, icons, cid, 18.0);
            ui.add_space(2.0);
        }
        egui::ComboBox::from_id_salt(id_salt)
            .width(if sel.is_some() { 176.0 } else { 200.0 })
            .height(440.0)
            .selected_text(character_label(*sel))
            .show_ui(ui, |ui| {
                if icon_select_row(ui, sel.is_none(), "Any", |cui| cui.add_space(18.0)) {
                    *sel = None;
                    ui.close_menu();
                }
                for cid in 0..=26 {
                    let name = spaced_name(CHARACTERS[cid as usize]);
                    if icon_select_row(ui, *sel == Some(cid), &name, |cui| {
                        crate::icons::character_icon(cui, icons, cid, 18.0)
                    }) {
                        *sel = Some(cid);
                        ui.close_menu();
                    }
                }
            });
    });
}

/// Stage filter combo — the stage analogue of [`character_filter_combo`],
/// over the tournament-legal pool.
fn stage_filter_combo(
    ui: &mut egui::Ui,
    icons: &mut crate::icons::IconCache,
    id_salt: &str,
    sel: &mut Option<i32>,
) {
    ui.horizontal(|ui| {
        if let Some(sid) = *sel {
            crate::icons::stage_icon(ui, icons, sid, 18.0);
            ui.add_space(2.0);
        }
        egui::ComboBox::from_id_salt(id_salt)
            .width(if sel.is_some() { 176.0 } else { 200.0 })
            .height(440.0)
            .selected_text(stage_label(*sel))
            .show_ui(ui, |ui| {
                if icon_select_row(ui, sel.is_none(), "Any", |cui| cui.add_space(18.0)) {
                    *sel = None;
                    ui.close_menu();
                }
                for sid in [2, 3, 8, 28, 31, 32] {
                    let name = spaced_name(STAGES[sid as usize]);
                    if icon_select_row(ui, *sel == Some(sid), &name, |cui| {
                        crate::icons::stage_icon(cui, icons, sid, 18.0)
                    }) {
                        *sel = Some(sid);
                        ui.close_menu();
                    }
                }
            });
    });
}

/// A compact metric card: a small muted label over a large value, on a
/// raised surface. The building block of the Analytics summary.
fn metric_card(ui: &mut egui::Ui, label: &str, value: &str) {
    let fill = surface_fill(ui.visuals());
    egui::Frame::none()
        .fill(fill)
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

/// A prominent primary-action button — Melee-gold fill with dark text. Use for
/// the single clear call-to-action in a cluster (View a replay, Scan, Open in
/// Slippi). Returns the [`egui::Response`] so callers test `.clicked()`.
fn primary_button(ui: &mut egui::Ui, label: &str) -> egui::Response {
    ui.add(egui::Button::new(egui::RichText::new(label).color(ON_ACCENT).strong()).fill(ACCENT))
}

/// A destructive-action button — flame fill, white text. Use for delete
/// confirmations and other irreversible actions.
fn danger_button(ui: &mut egui::Ui, label: &str) -> egui::Response {
    ui.add(
        egui::Button::new(egui::RichText::new(label).color(egui::Color32::WHITE).strong())
            .fill(FLAME),
    )
}

/// Lerp two RGB triples in sRGB space.
fn lerp_rgb(a: (u8, u8, u8), b: (u8, u8, u8), t: f32) -> egui::Color32 {
    let t = t.clamp(0.0, 1.0);
    let l = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    egui::Color32::from_rgb(l(a.0, b.0), l(a.1, b.1), l(a.2, b.2))
}

/// Win-rate color ramp: red (low) → amber (even) → green (high).
fn win_color(p: f32) -> egui::Color32 {
    if p < 0.5 {
        lerp_rgb((212, 80, 80), (216, 176, 72), p / 0.5)
    } else {
        lerp_rgb((216, 176, 72), (90, 190, 110), (p - 0.5) / 0.5)
    }
}

/// Horizontal win-rate bar: a dark track with a colored fill proportional
/// to `proportion` (0..=1).
fn draw_win_bar(ui: &mut egui::Ui, proportion: f32, width: f32, height: f32) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(width, height), egui::Sense::hover());
    if !ui.is_rect_visible(rect) {
        return;
    }
    let p = proportion.clamp(0.0, 1.0);
    let track = track_fill(ui.visuals());
    let painter = ui.painter();
    painter.rect_filled(rect, egui::Rounding::same(3.0), track);
    let fill_w = (width * p).round();
    if fill_w >= 1.0 {
        let fill = egui::Rect::from_min_size(rect.min, egui::vec2(fill_w, height));
        painter.rect_filled(fill, egui::Rounding::same(3.0), win_color(p));
    }
}

/// One win-rate row: an optional leading icon, a fixed-width name, the
/// bar, and a "NN%  W-L" record. `draw_icon` lets character/stage rows
/// show an icon while opponent rows pass a no-op.
fn winrate_row(
    ui: &mut egui::Ui,
    draw_icon: impl FnOnce(&mut egui::Ui),
    name: &str,
    wp: &WinProportion,
) {
    ui.horizontal(|ui| {
        draw_icon(ui);
        ui.add_sized(
            [104.0, 18.0],
            egui::Label::new(egui::RichText::new(name).size(13.0)).truncate(),
        );
        draw_win_bar(ui, wp.proportion, 84.0, 11.0);
        ui.add_space(6.0);
        let losses = wp.total - wp.wins;
        ui.label(
            egui::RichText::new(format!("{:.0}%  {}-{}", wp.proportion * 100.0, wp.wins, losses))
                .size(12.0)
                .color(egui::Color32::from_gray(165)),
        );
    });
}

/// Header for a win-rate section, followed by the top-`top_n` rows (by
/// games played) of a per-character [`WinProportion`] array, each led by
/// the character icon. `arr` is indexed by internal character id.
fn char_winrate_section(
    ui: &mut egui::Ui,
    icons: &mut crate::icons::IconCache,
    title: &str,
    arr: &[WinProportion],
    top_n: usize,
) {
    ui.label(egui::RichText::new(title).strong());
    ui.add_space(4.0);
    let mut rows: Vec<(usize, &WinProportion)> = arr
        .iter()
        .enumerate()
        .filter(|(_, wp)| wp.total > 0)
        .collect();
    rows.sort_by(|a, b| b.1.total.cmp(&a.1.total));
    if rows.is_empty() {
        ui.label(
            egui::RichText::new("(no data)")
                .italics()
                .color(egui::Color32::from_gray(120)),
        );
        return;
    }
    for (id, wp) in rows.into_iter().take(top_n) {
        let name = CHARACTERS.get(id).map(|s| spaced_name(s)).unwrap_or_else(|| "Unknown".to_string());
        winrate_row(
            ui,
            |ui| {
                crate::icons::character_icon(ui, icons, id as i32, 18.0);
                ui.add_space(5.0);
            },
            &name,
            wp,
        );
    }
}

/// Same as [`char_winrate_section`] but for the per-stage array, led by
/// the stage icon.
fn stage_winrate_section(
    ui: &mut egui::Ui,
    icons: &mut crate::icons::IconCache,
    title: &str,
    arr: &[WinProportion],
    top_n: usize,
) {
    ui.label(egui::RichText::new(title).strong());
    ui.add_space(4.0);
    let mut rows: Vec<(usize, &WinProportion)> = arr
        .iter()
        .enumerate()
        .filter(|(_, wp)| wp.total > 0)
        .collect();
    rows.sort_by(|a, b| b.1.total.cmp(&a.1.total));
    if rows.is_empty() {
        ui.label(
            egui::RichText::new("(no data)")
                .italics()
                .color(egui::Color32::from_gray(120)),
        );
        return;
    }
    for (id, wp) in rows.into_iter().take(top_n) {
        let name = STAGES.get(id).map(|s| spaced_name(s)).unwrap_or_else(|| "Unknown".to_string());
        winrate_row(
            ui,
            |ui| {
                crate::icons::stage_icon(ui, icons, id as i32, 18.0);
                ui.add_space(5.0);
            },
            &name,
            wp,
        );
    }
}

/// Win-rate-by-opponent-code section. No icons — opponents are keyed by
/// connect code.
fn opponent_winrate_section(
    ui: &mut egui::Ui,
    title: &str,
    map: &std::collections::HashMap<String, WinProportion>,
    top_n: usize,
) {
    ui.label(egui::RichText::new(title).strong());
    ui.add_space(4.0);
    let mut rows: Vec<(&String, &WinProportion)> =
        map.iter().filter(|(_, wp)| wp.total > 0).collect();
    rows.sort_by(|a, b| b.1.total.cmp(&a.1.total));
    if rows.is_empty() {
        ui.label(
            egui::RichText::new("(no data)")
                .italics()
                .color(egui::Color32::from_gray(120)),
        );
        return;
    }
    for (code, wp) in rows.into_iter().take(top_n) {
        winrate_row(ui, |_ui| {}, code, wp);
    }
}

/// Install the app-wide visual theme — the single fixed Melee palette
/// ([`BG_APP`] … [`ACCENT`]) with a roomier layout, a clear type scale, and
/// consistently styled buttons. Called once at startup from
/// [`StatsMeleeApp::new`].
///
/// The app has no light mode: we pin [`egui::ThemePreference::Dark`] so the OS
/// appearance can't switch us, and register the same Melee style for *both*
/// theme slots as a belt-and-suspenders so anything that resolves a style by
/// `egui::Theme` still gets our palette. With this in place `dark_mode` is
/// always `true`.
fn apply_theme(ctx: &egui::Context) {
    ctx.options_mut(|o| o.theme_preference = egui::ThemePreference::Dark);
    let style = build_style();
    ctx.set_style_of(egui::Theme::Dark, style.clone());
    ctx.set_style_of(egui::Theme::Light, style);
}

/// Build the one [`egui::Style`]: shared spacing + type scale, the Melee
/// surface palette, and uniform button styling so every `ui.button()` reads as
/// the same raised gold-on-hover control. Custom-painted widgets (cards, win
/// bars, nav capsule) pull their fills from the palette constants directly.
fn build_style() -> egui::Style {
    use egui::{FontFamily, FontId, Rounding, Stroke, TextStyle};

    let mut style = egui::Style::default();

    // Roomier than egui's defaults — the dense table benefits from a bit
    // more breathing room around controls.
    style.spacing.item_spacing = egui::vec2(8.0, 8.0);
    style.spacing.button_padding = egui::vec2(12.0, 6.0);
    style.spacing.menu_margin = egui::Margin::same(8.0);
    style.spacing.interact_size.y = 28.0;

    // Type scale with a clear heading hierarchy.
    style.text_styles = [
        (TextStyle::Heading, FontId::new(22.0, FontFamily::Proportional)),
        (TextStyle::Body, FontId::new(14.0, FontFamily::Proportional)),
        (TextStyle::Monospace, FontId::new(13.0, FontFamily::Monospace)),
        (TextStyle::Button, FontId::new(14.0, FontFamily::Proportional)),
        (TextStyle::Small, FontId::new(11.0, FontFamily::Proportional)),
    ]
    .into();

    // Start from stock dark visuals (correct `dark_mode` + base text), then
    // lay the Melee palette on top.
    let mut v = egui::Visuals::dark();
    v.panel_fill = BG_APP;
    v.window_fill = BG_WINDOW;
    v.window_stroke = Stroke::new(1.0, egui::Color32::from_rgb(0x39, 0x31, 0x4E));
    v.extreme_bg_color = BG_EXTREME;
    v.faint_bg_color = BG_STRIPE; // striped table rows
    v.hyperlink_color = ACCENT;
    v.selection.bg_fill = ACCENT.linear_multiply(0.35);
    v.selection.stroke = Stroke::new(1.0, ACCENT);
    v.override_text_color = Some(TEXT_HI);

    let rounding = Rounding::same(6.0);

    // Non-interactive chrome (labels, separators, panel frames).
    v.widgets.noninteractive.rounding = rounding;
    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, egui::Color32::from_rgb(0x2E, 0x27, 0x40));
    v.widgets.noninteractive.fg_stroke = Stroke::new(1.0, TEXT_HI);

    // Buttons at rest: a flat raised purple body, no border, light text.
    v.widgets.inactive.rounding = rounding;
    v.widgets.inactive.weak_bg_fill = BG_TRACK;
    v.widgets.inactive.bg_fill = BG_TRACK;
    v.widgets.inactive.bg_stroke = Stroke::NONE;
    v.widgets.inactive.fg_stroke = Stroke::new(1.0, TEXT_HI);
    v.widgets.inactive.expansion = 0.0;

    // Hover: lighter body + a gold hairline so the control "lifts".
    v.widgets.hovered.rounding = rounding;
    v.widgets.hovered.weak_bg_fill = BG_TRACK_HI;
    v.widgets.hovered.bg_fill = BG_TRACK_HI;
    v.widgets.hovered.bg_stroke = Stroke::new(1.0, ACCENT_HI);
    v.widgets.hovered.fg_stroke = Stroke::new(1.0, TEXT_HI);
    v.widgets.hovered.expansion = 1.0;

    // Pressed/active: gold wash to confirm the click.
    v.widgets.active.rounding = rounding;
    v.widgets.active.weak_bg_fill = ACCENT.linear_multiply(0.55);
    v.widgets.active.bg_fill = ACCENT.linear_multiply(0.55);
    v.widgets.active.bg_stroke = Stroke::new(1.0, ACCENT);
    v.widgets.active.fg_stroke = Stroke::new(1.0, TEXT_HI);
    v.widgets.active.expansion = 1.0;

    // Open (combo-box / menu popped): match hover.
    v.widgets.open.rounding = rounding;
    v.widgets.open.weak_bg_fill = BG_TRACK_HI;
    v.widgets.open.bg_fill = BG_TRACK_HI;
    v.widgets.open.bg_stroke = Stroke::new(1.0, egui::Color32::from_rgb(0x39, 0x31, 0x4E));

    v.window_rounding = Rounding::same(10.0);

    style.visuals = v;
    style
}

/// Slightly-raised "card" / info-bubble surface fill — used by [`metric_card`],
/// favorites, and the floating nav toggle.
fn surface_fill(_visuals: &egui::Visuals) -> egui::Color32 {
    BG_CARD
}

/// The neutral track behind a colored win-rate bar fill.
fn track_fill(_visuals: &egui::Visuals) -> egui::Color32 {
    BG_TRACK
}

/// Linearly blend two opaque colors in sRGB space (alpha ignored), `t` from
/// `a`→`b`. Used to tint a status banner's background toward its accent
/// without the `linear_multiply` darkening that breaks in light mode.
fn mix_color(a: egui::Color32, b: egui::Color32, t: f32) -> egui::Color32 {
    let t = t.clamp(0.0, 1.0);
    let f = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    egui::Color32::from_rgb(f(a.r(), b.r()), f(a.g(), b.g()), f(a.b(), b.b()))
}

#[cfg(test)]
mod date_tests {
    use super::*;

    #[test]
    fn parse_iso_ymd_accepts_date_and_datetime() {
        assert_eq!(parse_iso_ymd("2025-04-01"), Some((2025, 4, 1)));
        assert_eq!(parse_iso_ymd("2025-04-01T14:39:10Z"), Some((2025, 4, 1)));
        assert_eq!(parse_iso_ymd("2025-12-31 23:59:59"), Some((2025, 12, 31)));
        // Bad shapes / out-of-range.
        assert_eq!(parse_iso_ymd("2025/04/01"), None);
        assert_eq!(parse_iso_ymd("2025-13-01"), None);
        assert_eq!(parse_iso_ymd("2025-04-00"), None);
        assert_eq!(parse_iso_ymd("nope"), None);
    }

    #[test]
    fn ordinal_round_trips_through_date_string() {
        for s in ["1970-01-01", "2000-02-29", "2024-02-29", "2025-04-01", "2099-12-31"] {
            let ord = date_to_ordinal(s).expect("parse");
            assert_eq!(ordinal_to_date(ord), s, "round-trip failed for {s}");
        }
    }

    #[test]
    fn ordinal_is_monotonic_and_day_steps_are_one() {
        let a = date_to_ordinal("2025-04-01").unwrap();
        let b = date_to_ordinal("2025-04-02").unwrap();
        let c = date_to_ordinal("2025-05-01").unwrap();
        assert_eq!(b - a, 1, "consecutive days differ by 1");
        assert_eq!(c - a, 30, "April has 30 days");
        assert!(a < c, "later dates have larger ordinals");
    }

    #[test]
    fn epoch_is_zero() {
        assert_eq!(date_to_ordinal("1970-01-01"), Some(0));
    }

    #[test]
    fn fmt_playtime_reads_compactly() {
        assert_eq!(fmt_playtime(0), "0m");
        assert_eq!(fmt_playtime(30), "<1m"); // non-empty but sub-minute
        assert_eq!(fmt_playtime(60), "1m");
        assert_eq!(fmt_playtime(45 * 60), "45m");
        assert_eq!(fmt_playtime(3600), "1h");
        assert_eq!(fmt_playtime(3 * 3600), "3h"); // exact hours drop minutes
        assert_eq!(fmt_playtime(12 * 3600 + 34 * 60), "12h 34m");
        assert_eq!(fmt_playtime(-5), "0m"); // negative clamps
    }

    #[test]
    fn autoformat_ymd_masks_progressively_and_forgives_separators() {
        // Progressive masking as the user types digits.
        assert_eq!(autoformat_ymd(""), "");
        assert_eq!(autoformat_ymd("2025"), "2025");
        assert_eq!(autoformat_ymd("20250"), "2025-0");
        assert_eq!(autoformat_ymd("202504"), "2025-04");
        assert_eq!(autoformat_ymd("2025040"), "2025-04-0");
        assert_eq!(autoformat_ymd("20250401"), "2025-04-01");
        // Any separator style collapses to the canonical mask (the mask
        // packs digits positionally, so months/days must be zero-padded).
        assert_eq!(autoformat_ymd("2025/04/01"), "2025-04-01");
        assert_eq!(autoformat_ymd("2025.04.01"), "2025-04-01");
        assert_eq!(autoformat_ymd("2025-04-01"), "2025-04-01");
        // Excess digits past the day are dropped, junk is ignored.
        assert_eq!(autoformat_ymd("2025040199"), "2025-04-01");
        assert_eq!(autoformat_ymd("abc2025"), "2025");
        // The masked output is idempotent (re-running doesn't drift).
        let once = autoformat_ymd("2025-04-01");
        assert_eq!(autoformat_ymd(&once), once);
    }

    #[test]
    fn ordinal_domain_spans_min_to_max() {
        assert_eq!(ordinal_domain(&[]), None);
        assert_eq!(ordinal_domain(&[5]), Some((5, 5)));
        assert_eq!(ordinal_domain(&[5, 1, 9, 3]), Some((1, 9)));
        assert_eq!(ordinal_domain(&[-3, -10, 0]), Some((-10, 0)));
    }
}
