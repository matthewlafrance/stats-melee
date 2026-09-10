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
use stats_melee::analytics::{WinAnalytics, WinProportion};
use stats_melee::{PlayerSummary, PlayerSummaryFilter};

mod format;
mod pages;
mod theme;
mod widgets;
mod workers;

use self::format::*;
use self::theme::*;
use self::widgets::*;

use crate::config::AppConfig;
use crate::replay_list::{self, ReplayRow, SortDirection, SortKey};
use crate::slippi;
use crate::viewer::{self, ViewerState};


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
    /// as `summary_rx`.
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

    /// Result of the most recent Settings "Re-extract icons from Slippi"
    /// click — `(characters, stages)` on success, else an error message.
    last_icon_extract: Option<Result<crate::slippi_icons::ExtractReport, String>>,

    /// Persistent file-backed cache for [`ReplayAnalysis`] keyed on
    /// each .slp's content hash. Constructed once at app startup; the
    /// viewer's load path consults it before re-parsing peppi, which
    /// turns the second view of any replay from a ~1s blocking parse
    /// into an instant DB-style read. See [`AnalysisCache`].
    analysis_cache: AnalysisCache,

    /// Lazily-populated GPU-texture cache for character + stage icons,
    /// with a drawn-badge fallback when no PNG asset is present. See
    /// [`crate::icons`].
    icons: crate::icons::IconCache,
}

/// One message off the summary-worker channel. We flatten the Result into a
/// plain enum so the sender side doesn't have to deal with `Send` bounds on
/// anyhow errors — a `String` round-trips cleanly.
// The Ok variant is ~1.8 KB against Err's 24, so every message carries the
// larger footprint. These are sent once per recompute — a filter drag, not a
// hot loop — so boxing would trade a real allocation for a saving nobody can
// measure.
#[allow(clippy::large_enum_variant)]
pub(crate) enum SummaryMsg {
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
pub(crate) enum IngestMsg {
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
pub(crate) struct SummaryKey {
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
        // First launch: rip character / stage icons from a local Slippi install
        // into the writable assets dir so the library/viewer show real art
        // instead of badges. One-shot, best-effort, off the render path.
        ensure_slippi_icons(config.slippi_launcher_path.as_deref());
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
            last_icon_extract: None,
            analysis_cache,
            icons: crate::icons::IconCache::default(),
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
















    fn page_replay_viewer(&mut self, ui: &mut egui::Ui) {
        // Defer navigation / reload / launch actions until after the
        // borrow of self.viewer_state ends — egui's closure pattern
        // means we'd otherwise double-borrow self.
        let mut back_clicked = false;
        let mut reload_clicked = false;
        let mut launch_clicked = false;

        // Whether the currently-loaded viewer has a usable replay_path —
        // used to enable/disable the "Open in Slippi" button below.
        let can_launch = matches!(
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
        }
        if reload_clicked {
            self.reload_viewer();
        }
        if launch_clicked {
            self.launch_in_slippi();
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

/// First-launch icon population: if the writable assets dir has no character
/// art yet, rip it from a local Slippi Launcher install. Best-effort and
/// one-shot — no Slippi, no write permission, or a bundle-layout change all
/// just leave the drawn-badge fallback in place. Re-runs (cheaply, hitting the
/// no-Slippi path fast) only while the dir stays empty, so installing Slippi
/// later still gets picked up on a subsequent launch.
fn ensure_slippi_icons(launcher_override: Option<&Path>) {
    let Ok(dest) = AppConfig::default_assets_dir() else {
        return;
    };
    let chars_dir = dest.join("characters");
    let already = std::fs::read_dir(&chars_dir)
        .map(|d| d.flatten().any(|e| e.path().extension().is_some_and(|x| x == "png")))
        .unwrap_or(false);
    if already {
        return;
    }
    match crate::slippi_icons::extract_to(&dest, launcher_override) {
        Ok(r) => eprintln!(
            "stats-melee: extracted {} character + {} stage icons from Slippi into {} \
             (asset modules: {}, char refs: {}, stage refs: {})",
            r.characters, r.stages, dest.display(), r.asset_modules, r.char_refs, r.stage_refs
        ),
        Err(e) => eprintln!("stats-melee: Slippi icon extraction skipped ({e})"),
    }
}




// --- Date helpers (dependency-free) -------------------------------------------
//
// We store played dates as ISO-8601 strings and only need two things the
// standard library can't give us without a date crate: a monotonic day
// number for the range slider, and its inverse to turn a slider position
// back into a `YYYY-MM-DD` string. Howard Hinnant's `days_from_civil` /
// `civil_from_days` (public-domain) do exactly that, branch-free.
