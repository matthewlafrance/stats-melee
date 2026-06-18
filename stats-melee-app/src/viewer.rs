//! Replay-viewer page: per-game detail view with a colored combat-state
//! scrub bar.
//!
//! The embedded 2D stage view was removed — we're pivoting to an
//! in-house video pipeline (see Track 10) that will render an actual
//! Melee replay. The scrub bar survives the rip-out because it's going
//! to overlay the video once playback lands; for now it renders as a
//! non-interactive timeline visualization of combat state over time.
//!
//! Lifecycle:
//! 1. User clicks "View" on a row in the library table.
//! 2. `load_viewer` reads the `game` + `gamePlayer` rows from SQLite,
//!    opens the `.slp` off disk, and runs the 1v1 combat-state analysis.
//! 3. The resulting [`ViewerState`] gets cached on the app and rendered
//!    via [`render_viewer`] on every frame.
//!
//! Errors are partial — metadata always comes through from the DB, and
//! only the scrub bar falls back to a text message when the replay
//! file is missing / unparseable / not 1v1. A user on a 2v2 replay
//! still sees who played and where.

use std::path::Path;

use anyhow::{anyhow, Result};
use diesel::prelude::*;
use eframe::egui;

use stats_melee::analysis_cache::AnalysisCache;
use stats_melee::combat::{CombatState, ReplayAnalysis};
use stats_melee::gamedata::{CHARACTERS, STAGES};
use stats_melee::models::{Game, GamePlayer};

/// One player in the viewer's metadata header. Ordered by placement in
/// [`ViewerState::players`] (slot 0 = 1st place, 1 = 2nd, ...).
#[derive(Debug, Clone)]
pub struct ViewerPlayer {
    /// 0-indexed placement slot.
    pub placement: usize,
    pub code: String,
    pub character_id: i32,
    /// Peppi port index (0..=3).
    pub port: i32,
}

impl ViewerPlayer {
    pub fn character_name(&self) -> &'static str {
        usize::try_from(self.character_id)
            .ok()
            .and_then(|i| CHARACTERS.get(i).copied())
            .unwrap_or("Unknown")
    }
}

/// Where the configured user sits relative to the combat-state
/// vector's lower-port (P1) / higher-port (P2) split. Driven by
/// `config.user_player_code` matching one of the loaded players'
/// `code`. When set, the scrub bar's color palette becomes
/// "you / opponent" instead of "lower-port / higher-port".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UserPerspective {
    /// User is the lower-port player. `CombatState::AdvP1` reads as
    /// "you have advantage".
    P1,
    /// User is the higher-port player. `CombatState::AdvP2` reads as
    /// "you have advantage".
    P2,
}

/// What a key-moment marker represents on the scrub bar. Drives both
/// the icon shape and the tooltip prefix.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyMomentKind {
    /// A punish that took a stock. Highest-priority marker — these
    /// are the "anchor moments" of the match the user wants to find
    /// instantly. Rendered as a filled triangle.
    Kill,
    /// A non-killing combo with `hit_count >= LONG_COMBO_THRESHOLD`.
    /// Rendered as a smaller filled circle. Shorter combos aren't
    /// worth the visual noise — every neutral exchange in Melee has
    /// 2-3-hit jab strings that don't need their own marker.
    LongCombo,
}

/// Hit-count threshold above which a non-killing punish gets a
/// scrub-bar marker. Below this, the combo doesn't add enough signal
/// over what the combat-state coloring already shows.
pub const LONG_COMBO_THRESHOLD: i32 = 4;

/// One scrub-bar marker. Pulled from the punish table at
/// `load_viewer` time and laid above the combat-state bar by
/// `render_scrub_bar`'s key-moments overlay.
#[derive(Debug, Clone)]
pub struct KeyMoment {
    /// Frame index into the combat-state vector. Matches
    /// `Punish::end_frame` for kills (the moment the stock dropped)
    /// and `start_frame` for long combos (the start of the
    /// momentum). The renderer treats both as "x position on the
    /// timeline" and clamps to the bar's width.
    pub frame: i32,
    pub kind: KeyMomentKind,
    /// `true` when the attacker was the lower-port (P1) player —
    /// drives marker color via the same palette the bar itself uses,
    /// so a "you killed them" marker tints the same as a "you have
    /// advantage" frame.
    pub by_p1: bool,
    /// Pre-formatted tooltip text. Built at load_viewer time so the
    /// renderer doesn't need access to the attack-name table or the
    /// player roster — keeps the render path purely visual.
    pub label: String,
}

/// Everything the viewer page needs to render a single game.
///
/// `analysis` carries either the combat-state vector + port indices or
/// a human-readable reason we couldn't build it — missing
/// `replay_path`, file not on disk, peppi parse failure, or "not a
/// 1v1". The DB-sourced metadata is always populated regardless.
#[derive(Debug, Clone)]
pub struct ViewerState {
    pub game_id: i32,
    pub replay_path: Option<String>,
    pub stage_id: i32,
    pub duration_seconds: i32,
    pub ingested_at: String,
    /// In placement order; may be shorter than 4 for 1v1 / 3-player games.
    pub players: Vec<ViewerPlayer>,
    /// Per-frame analysis. `Err(reason)` when the replay file is missing
    /// or not 1v1; the metadata header still renders in that case.
    pub analysis: Result<ReplayAnalysis, String>,
    /// `Some(P1|P2)` when the configured user_player_code matches one
    /// of this game's slots. Drives the scrub-bar palette flip from
    /// "lower-port / higher-port" to "you / opponent". `None` when
    /// the user code is unset, doesn't match anyone in this game, or
    /// the analysis didn't resolve port indices.
    pub user_perspective: Option<UserPerspective>,
    /// Key-moment markers laid above the scrub bar. Empty when the
    /// game has no punishes (e.g. analysis failed, or the punish
    /// extractor hasn't run for legacy ingested rows). Sorted by
    /// `frame` ascending, same order `get_punishes_for_game` returns
    /// rows.
    pub key_moments: Vec<KeyMoment>,
}

impl ViewerState {
    pub fn stage_name(&self) -> &'static str {
        usize::try_from(self.stage_id)
            .ok()
            .and_then(|i| STAGES.get(i).copied())
            .unwrap_or("Unknown")
    }

    pub fn duration_display(&self) -> String {
        let total = self.duration_seconds.max(0);
        let minutes = total / 60;
        let seconds = total % 60;
        format!("{minutes}:{seconds:02}")
    }
}

/// Build a [`ViewerState`] for `game_id`. Errors only on genuine DB-side
/// problems (game not found, query failed). Replay-file / analysis
/// failures are packaged into `state.analysis = Err(...)` so the UI can
/// still show the metadata header.
///
/// `cache` is consulted before re-parsing the .slp from disk. The
/// game's `content_hash` column (Track 11d) is the cache key — rows
/// ingested before that column existed always miss the cache and fall
/// through to the slow path.
///
/// `user_player_code`, when `Some`, is matched against the loaded
/// players to set [`ViewerState::user_perspective`]. The scrub bar
/// uses this to flip its palette to "you / opponent". Pass `None`
/// (or an empty string) to keep the port-relative palette.
pub fn load_viewer(
    conn: &mut SqliteConnection,
    game_id: i32,
    cache: &mut AnalysisCache,
    user_player_code: Option<&str>,
) -> Result<ViewerState> {
    use stats_melee::schema::{game, gamePlayer};

    let g: Game = game::table
        .filter(game::id.eq(game_id))
        .select(Game::as_select())
        .first(conn)
        .map_err(|e| anyhow!("loading game {game_id}: {e}"))?;

    // Each slot_id is an Option<i32>; lookup the gamePlayer for each non-
    // null slot. We issue one query per slot instead of a single IN-list
    // load because we need to preserve placement order in the output and
    // there are at most 4 lookups per call — not worth the complexity.
    let slot_ids: [Option<i32>; 4] = [g.first, g.second, g.third, g.fourth];
    let mut players: Vec<ViewerPlayer> = Vec::new();
    // Side-table the same loads into a `gp_id -> port` map so the
    // key-moments builder below can resolve a punish's `attacker_id`
    // (which is a gp_id) into a peppi port index without a second
    // round of queries.
    let mut gp_id_to_port: std::collections::HashMap<i32, i32> =
        std::collections::HashMap::new();
    for (slot, maybe_id) in slot_ids.iter().enumerate() {
        if let Some(gp_id) = maybe_id {
            let gp: GamePlayer = gamePlayer::table
                .filter(gamePlayer::id.eq(*gp_id))
                .select(GamePlayer::as_select())
                .first(conn)
                .map_err(|e| anyhow!("loading gamePlayer {gp_id}: {e}"))?;
            gp_id_to_port.insert(*gp_id, gp.port);
            players.push(ViewerPlayer {
                placement: slot,
                code: gp.code,
                character_id: gp.character,
                port: gp.port,
            });
        }
    }

    // Now attempt the per-frame analysis. Any failure here collapses
    // into a user-friendly string on `analysis` — caller's UI shows a
    // greyed-out scrub bar + the reason but still renders the metadata
    // pulled from the DB above.
    //
    // Lookup order:
    //   1. If the game has a content_hash, try the analysis cache.
    //   2. On miss (or missing hash), re-parse the .slp from disk.
    //   3. On a successful re-parse, write back to the cache so the
    //      next viewer-open of this replay is instant.
    let analysis = match g.replay_path.as_deref() {
        None => Err("this game was ingested before replay-path tracking — \
                     re-ingest to enable the scrub bar"
            .to_string()),
        Some(path) => derive_analysis(path, g.content_hash.as_deref(), cache)
            .map_err(|e| e.to_string()),
    };

    // Figure out which side of the combat-state vector the user
    // sits on, if any. We need both a non-empty user code AND a
    // successful analysis (port indices come from there) — without
    // both there's no perspective to flip to.
    let user_perspective = compute_user_perspective(
        user_player_code,
        &players,
        analysis.as_ref().ok(),
    );

    // Build the key-moment list from the punish table. Skipped when
    // analysis failed (we'd have nothing to overlay them on anyway).
    // Cheap: one DB query per viewer load + an in-memory map.
    let key_moments = match analysis.as_ref() {
        Ok(a) => match stats_melee::get_punishes_for_game(conn, game_id) {
            Ok(punishes) => key_moments_from_punishes(&punishes, &gp_id_to_port, a),
            Err(e) => {
                // Don't fail the whole viewer load over a punish-query
                // hiccup; key-moment markers degrade to "absent", the
                // scrub bar still renders.
                eprintln!("loading punishes for game {game_id}: {e}");
                Vec::new()
            }
        },
        Err(_) => Vec::new(),
    };

    Ok(ViewerState {
        game_id: g.id,
        replay_path: g.replay_path,
        stage_id: g.stage,
        duration_seconds: g.time,
        ingested_at: g.ingested_at,
        players,
        analysis,
        user_perspective,
        key_moments,
    })
}

/// Build [`KeyMoment`]s from the punish rows for one game. Pure —
/// `gp_id_to_port` is the map `load_viewer` builds while loading
/// player slots, and `analysis.p1_port_idx` is what defines "P1" for
/// the marker color. Punishes whose attacker port doesn't resolve are
/// silently dropped (shouldn't happen in practice for 1v1 ingestion,
/// but belt-and-suspenders).
fn key_moments_from_punishes(
    punishes: &[stats_melee::models::Punish],
    gp_id_to_port: &std::collections::HashMap<i32, i32>,
    analysis: &ReplayAnalysis,
) -> Vec<KeyMoment> {
    let mut out = Vec::new();
    for p in punishes {
        // Filter: only kills + long combos earn a marker. Short
        // combos (jab strings) would be visual noise.
        let kind = if p.did_kill_bool() {
            KeyMomentKind::Kill
        } else if p.hit_count >= LONG_COMBO_THRESHOLD {
            KeyMomentKind::LongCombo
        } else {
            continue;
        };

        let attacker_port = match gp_id_to_port.get(&p.attacker_id) {
            Some(port) => *port,
            None => continue,
        };
        let by_p1 = attacker_port == analysis.p1_port_idx as i32;

        // Anchor frame: kill marker sits at end_frame (moment of
        // impact); combo marker sits at start_frame (where the
        // momentum began).
        let frame = match kind {
            KeyMomentKind::Kill => p.end_frame,
            KeyMomentKind::LongCombo => p.start_frame,
        };

        let label = match kind {
            KeyMomentKind::Kill => match p.kill_move {
                Some(id) => format!(
                    "Kill: {} ({} hits)",
                    stats_melee::gamedata::attack_display_name(id),
                    p.hit_count
                ),
                None => format!("Kill ({} hits)", p.hit_count),
            },
            KeyMomentKind::LongCombo => format!("{}-hit combo", p.hit_count),
        };

        out.push(KeyMoment {
            frame,
            kind,
            by_p1,
            label,
        });
    }
    out
}

/// Match `user_code` (when set) against the loaded `players` and the
/// resolved `analysis` port indices to decide which side of the combat-
/// state vector the user occupies. Pure helper — no IO, easy to unit
/// test.
fn compute_user_perspective(
    user_code: Option<&str>,
    players: &[ViewerPlayer],
    analysis: Option<&ReplayAnalysis>,
) -> Option<UserPerspective> {
    let code = user_code.map(str::trim).filter(|s| !s.is_empty())?;
    let analysis = analysis?;
    // Find the user's player slot, then their peppi port index.
    let user_port = players
        .iter()
        .find(|p| p.code == code)
        .map(|p| p.port)?;
    if user_port == analysis.p1_port_idx as i32 {
        Some(UserPerspective::P1)
    } else if user_port == analysis.p2_port_idx as i32 {
        Some(UserPerspective::P2)
    } else {
        // Shouldn't happen for a 1v1 — the analysis only resolves two
        // port indices and the user has to be in one of them. Bail
        // gracefully and fall back to port-relative coloring.
        None
    }
}

/// Open a `.slp` off disk and run the 1v1 per-frame analysis on it,
/// consulting the analysis sidecar cache first when a `content_hash`
/// is available.
///
/// Three flows:
///   - **Hash present + cache hit:** zero-cost, returns instantly.
///   - **Hash present + cache miss:** parse .slp, write the result
///     back to the cache before returning. Next call hits.
///   - **No hash (legacy row, ingested before Track 11d):** parse
///     from disk every time, no cache write — there's no stable key
///     to write under.
fn derive_analysis(
    replay_path: &str,
    content_hash: Option<&str>,
    cache: &mut AnalysisCache,
) -> Result<ReplayAnalysis> {
    let p = Path::new(replay_path);
    if !p.exists() {
        return Err(anyhow!(
            "replay file not found on disk: {replay_path}\n\
             (was the folder moved or renamed?)"
        ));
    }

    let parse = || {
        stats_melee::parse_replay_analysis(p)
            .map_err(|e| anyhow!("parsing {replay_path}: {e}"))
    };

    // Without a stable hash key the cache can't help — fall straight
    // through to a fresh parse every time.
    let Some(key) = content_hash else {
        return parse();
    };

    // Cache-aware path. `get_or_insert_with` swallows write failures
    // internally (they shouldn't break the viewer) and only surfaces
    // errors from the parse itself.
    cache.get_or_insert_with(key, parse)
}

// --- Rendering ---------------------------------------------------------------

/// Scrub-bar palette. Chosen for decent contrast on both dark and light
/// egui themes — these are relatively saturated but not neon. The
/// "green vs red" mapping to advantage / disadvantage is decided
/// dynamically by [`user_palette`] based on the current user
/// perspective, so the constants are named for color rather than for
/// player.
const COLOR_NEUTRAL: egui::Color32 = egui::Color32::from_rgb(70, 80, 95); // slate
const COLOR_GREEN: egui::Color32 = egui::Color32::from_rgb(60, 170, 90);
const COLOR_RED: egui::Color32 = egui::Color32::from_rgb(220, 80, 80);
const COLOR_TRADE: egui::Color32 = egui::Color32::from_rgb(230, 170, 40); // amber

/// Vertical strip allocated above the scrub bar for the kill /
/// long-combo markers. Tall enough for a 6-7px-radius triangle to
/// land cleanly without overlapping the bar's outline.
const MARKERS_ROW_HEIGHT: f32 = 14.0;

/// Resolve the colors `CombatState::AdvP1` and `CombatState::AdvP2`
/// should paint as, given the current user perspective.
///
/// - **Perspective set** → green is always "you on advantage", red is
///   always "you on disadvantage". Flip the P1/P2 mapping when the
///   user is the higher-port player.
/// - **No perspective** → green = lower-port (P1), red = higher-port
///   (P2). Arbitrary but stable; the legend still calls them out by
///   port so the user isn't misled.
///
/// Returns `(color_for_AdvP1, color_for_AdvP2)`.
fn user_palette(perspective: Option<UserPerspective>) -> (egui::Color32, egui::Color32) {
    match perspective {
        None | Some(UserPerspective::P1) => (COLOR_GREEN, COLOR_RED),
        Some(UserPerspective::P2) => (COLOR_RED, COLOR_GREEN),
    }
}

/// Render the full viewer page. Caller is responsible for the heading +
/// nav controls; this draws the body: metadata → combat-state scrub bar.
///
/// Playback + embedded video land in Track 10; today this is a static
/// overview with the scrub bar as a timeline visualization. When video
/// arrives the scrub bar becomes click-to-seek and gains a playhead.
pub fn render_viewer(ui: &mut egui::Ui, state: &ViewerState) {
    render_metadata(ui, state);
    ui.add_space(16.0);
    ui.separator();
    ui.add_space(12.0);

    match &state.analysis {
        Ok(analysis) => {
            ui.strong("Combat state");
            ui.label(
                egui::RichText::new(intro_blurb(state.user_perspective))
                    .small()
                    .color(egui::Color32::GRAY),
            );
            ui.add_space(6.0);
            render_key_moments(
                ui,
                &state.key_moments,
                analysis.combat.len(),
                state.user_perspective,
                MARKERS_ROW_HEIGHT,
            );
            // No vertical gap between the markers row and the scrub
            // bar — they're meant to read as one composite widget.
            render_scrub_bar(ui, &analysis.combat, 24.0, state.user_perspective);
            ui.add_space(4.0);
            render_legend(ui, state.user_perspective);
        }
        Err(msg) => {
            ui.strong("Combat state");
            ui.add_space(6.0);
            render_scrub_bar_placeholder(ui, 24.0);
            ui.add_space(6.0);
            ui.colored_label(egui::Color32::from_rgb(200, 140, 60), format!("⚠ {msg}"));
        }
    }
}

/// Intro line under the "Combat state" heading. Swaps wording when a
/// user perspective is active so the colors read as "you" / "opponent"
/// instead of "lower-port" / "higher-port".
fn intro_blurb(perspective: Option<UserPerspective>) -> &'static str {
    match perspective {
        Some(_) => {
            "Colored per-frame: green = you have advantage, red = you're \
             on disadvantage, amber = trade, slate = neutral."
        }
        None => {
            "Colored per-frame: green = lower-port player has advantage, \
             red = higher-port player, amber = trade, slate = neutral. \
             Set your player code in Settings to flip the palette to \
             your perspective."
        }
    }
}

fn render_metadata(ui: &mut egui::Ui, state: &ViewerState) {
    egui::Grid::new("viewer_metadata_grid")
        .num_columns(2)
        .spacing([16.0, 4.0])
        .striped(true)
        .show(ui, |ui| {
            kv(ui, "Game id", state.game_id.to_string());
            kv(ui, "Stage", state.stage_name().to_string());
            kv(ui, "Duration", state.duration_display());
            kv(ui, "Ingested", state.ingested_at.clone());
            if let Some(path) = &state.replay_path {
                kv(ui, "Replay path", path.clone());
            } else {
                kv(ui, "Replay path", "(not recorded)".to_string());
            }
        });

    ui.add_space(12.0);
    ui.strong("Players");
    ui.add_space(4.0);
    if state.players.is_empty() {
        ui.label(
            egui::RichText::new("(no player slots populated)")
                .italics()
                .color(egui::Color32::GRAY),
        );
        return;
    }
    egui::Grid::new("viewer_players_grid")
        .num_columns(4)
        .spacing([16.0, 4.0])
        .striped(true)
        .show(ui, |ui| {
            ui.strong("Placement");
            ui.strong("Code");
            ui.strong("Character");
            ui.strong("Port");
            ui.end_row();
            for p in &state.players {
                ui.label(ordinal(p.placement));
                ui.label(&p.code);
                ui.label(p.character_name());
                ui.label(format!("P{}", p.port + 1));
                ui.end_row();
            }
        });
}

fn kv(ui: &mut egui::Ui, key: &str, value: String) {
    ui.label(key);
    ui.label(value);
    ui.end_row();
}

/// `0 → "1st", 1 → "2nd", ...`. Covers the 0..=3 range we actually hit.
fn ordinal(placement: usize) -> &'static str {
    match placement {
        0 => "1st",
        1 => "2nd",
        2 => "3rd",
        3 => "4th",
        _ => "?",
    }
}

/// Draw the colored combat-state bar across the full available width.
///
/// Today this is a pure visualization — no click-to-seek, no playhead.
/// Both will return when the embedded video widget lands (Track 10f)
/// and the bar has a playback time to sync against.
///
/// Each horizontal pixel corresponds to a range of frames in `states`
/// (same pixel count per frame when the game is longer than the bar,
/// which is the common case at ~8 frames/px for a 3-minute 1v1 on a
/// 1200px window). Per-pixel color uses a priority rule so brief
/// advantage windows still show up: `Trade > Adv* > Neutral`, with
/// "both p1 and p2 show advantage in the same pixel window" collapsing
/// to `Trade` since that's the semantically honest answer.
///
/// Adjacent same-color pixels are coalesced into a single rect to keep
/// the paint-call count bounded.
pub fn render_scrub_bar(
    ui: &mut egui::Ui,
    states: &[CombatState],
    height: f32,
    perspective: Option<UserPerspective>,
) {
    let palette = user_palette(perspective);
    let avail_w = ui.available_width().max(1.0);
    let desired = egui::vec2(avail_w, height);
    let (rect, _resp) = ui.allocate_exact_size(desired, egui::Sense::hover());
    let painter = ui.painter_at(rect);

    // Background fill so partially-empty bars still look like a rail.
    painter.rect_filled(rect, 2.0, COLOR_NEUTRAL);

    let n = states.len();
    let pixels = rect.width().floor().max(1.0) as usize;

    if n > 0 && pixels > 0 {
        // Walk pixels left-to-right, bucketing frames into each.
        // Coalesce runs of the same color.
        let mut run_color: Option<egui::Color32> = None;
        let mut run_start_px: usize = 0;

        for px in 0..pixels {
            let start_frame = px * n / pixels;
            let end_frame = ((px + 1) * n / pixels).min(n).max(start_frame + 1);

            let color = bucket_color(&states[start_frame..end_frame], palette);

            match run_color {
                Some(c) if c == color => { /* extend current run */ }
                Some(c) => {
                    // Flush previous run.
                    let x0 = rect.left() + run_start_px as f32;
                    let x1 = rect.left() + px as f32;
                    painter.rect_filled(
                        egui::Rect::from_min_max(
                            egui::pos2(x0, rect.top()),
                            egui::pos2(x1, rect.bottom()),
                        ),
                        0.0,
                        c,
                    );
                    run_color = Some(color);
                    run_start_px = px;
                }
                None => {
                    run_color = Some(color);
                    run_start_px = px;
                }
            }
        }

        // Flush final run.
        if let Some(c) = run_color {
            let x0 = rect.left() + run_start_px as f32;
            let x1 = rect.left() + pixels as f32;
            painter.rect_filled(
                egui::Rect::from_min_max(
                    egui::pos2(x0, rect.top()),
                    egui::pos2(x1, rect.bottom()),
                ),
                0.0,
                c,
            );
        }
    }

    // Outline — keeps the bar visually bounded when it runs up against
    // sibling widgets with similar backgrounds.
    painter.rect_stroke(
        rect,
        2.0,
        egui::Stroke::new(1.0, egui::Color32::from_rgb(40, 40, 50)),
    );
}

/// Grey placeholder rail for the error state. Same dimensions as the
/// real bar so the page doesn't reflow when switching between games.
/// Render the key-moment markers row above the scrub bar.
///
/// One marker per element of `moments`, mapped to an x position via
/// `frame / total_frames * width`. Markers tint per-attacker using
/// the same palette as the scrub bar below, so visually a "you got
/// the kill" marker reads green when perspective is set to your
/// side. Hovering surfaces the pre-formatted `label` (move name +
/// hit count) as a tooltip — the rect's `Sense::hover` is what
/// powers it.
///
/// When there are no moments to show we still allocate the strip so
/// the page doesn't reflow between games-with-markers and
/// games-without; saves a layout shift when the user clicks across
/// replays.
fn render_key_moments(
    ui: &mut egui::Ui,
    moments: &[KeyMoment],
    total_frames: usize,
    perspective: Option<UserPerspective>,
    height: f32,
) {
    let avail_w = ui.available_width().max(1.0);
    let (rect, resp) =
        ui.allocate_exact_size(egui::vec2(avail_w, height), egui::Sense::hover());
    if total_frames == 0 || moments.is_empty() {
        return;
    }
    let painter = ui.painter_at(rect);
    let palette = user_palette(perspective);
    let pointer = resp.hover_pos();
    // Track which marker (if any) the pointer is over. We pick the
    // *closest* marker by horizontal distance to break ties
    // deterministically when multiple markers overlap.
    let mut hit: Option<(&KeyMoment, f32)> = None;

    for m in moments {
        let t = (m.frame.max(0) as f32) / (total_frames as f32);
        let t = t.clamp(0.0, 1.0);
        let x = rect.left() + t * rect.width();
        let center_y = rect.center().y;
        let color = if m.by_p1 { palette.0 } else { palette.1 };

        let radius = match m.kind {
            KeyMomentKind::Kill => {
                // Filled triangle pointing down at the bar — the
                // "anchor" shape the user looks for at a glance.
                let half = height * 0.45;
                painter.add(egui::Shape::convex_polygon(
                    vec![
                        egui::pos2(x - half, rect.top() + 1.0),
                        egui::pos2(x + half, rect.top() + 1.0),
                        egui::pos2(x, rect.bottom() - 1.0),
                    ],
                    color,
                    egui::Stroke::new(0.5, egui::Color32::from_rgb(20, 20, 25)),
                ));
                half + 2.0
            }
            KeyMomentKind::LongCombo => {
                // Smaller filled circle — secondary signal, doesn't
                // compete with the kill triangles for attention.
                let r = height * 0.28;
                painter.circle_filled(egui::pos2(x, center_y), r, color);
                r + 2.0
            }
        };

        if let Some(p) = pointer {
            let dx = (p.x - x).abs();
            if dx <= radius && rect.contains(p) {
                let better = hit
                    .as_ref()
                    .map(|(_, prev_dx)| dx < *prev_dx)
                    .unwrap_or(true);
                if better {
                    hit = Some((m, dx));
                }
            }
        }
    }

    if let Some((m, _)) = hit {
        // `show_tooltip_at_pointer` is the reliable path here — egui's
        // `Response::on_hover_text` checks the *whole rect's* hover
        // state, so it won't fire for "pointer over a specific marker
        // inside a wider hoverable rect." Showing the tooltip only
        // when our per-marker hit-test matched gives crisp behavior.
        let label = m.label.clone();
        egui::show_tooltip_at_pointer(
            ui.ctx(),
            resp.layer_id,
            egui::Id::new("stats_melee_key_moment_tooltip"),
            |ui| {
                ui.label(label);
            },
        );
    }
}

fn render_scrub_bar_placeholder(ui: &mut egui::Ui, height: f32) {
    let avail_w = ui.available_width().max(1.0);
    let (rect, _) = ui.allocate_exact_size(egui::vec2(avail_w, height), egui::Sense::hover());
    let painter = ui.painter_at(rect);
    // Muted grey — distinct from the neutral color in the real bar.
    painter.rect_filled(rect, 2.0, egui::Color32::from_rgb(50, 50, 60));
    painter.rect_stroke(
        rect,
        2.0,
        egui::Stroke::new(1.0, egui::Color32::from_rgb(40, 40, 50)),
    );
}

fn render_legend(ui: &mut egui::Ui, perspective: Option<UserPerspective>) {
    let (p1_color, p2_color) = user_palette(perspective);
    let (p1_label, p2_label) = match perspective {
        Some(UserPerspective::P1) => ("You", "Opponent"),
        Some(UserPerspective::P2) => ("Opponent", "You"),
        None => ("P1 advantage", "P2 advantage"),
    };
    ui.horizontal(|ui| {
        swatch(ui, p1_color, p1_label);
        ui.add_space(12.0);
        swatch(ui, p2_color, p2_label);
        ui.add_space(12.0);
        swatch(ui, COLOR_TRADE, "Trade");
        ui.add_space(12.0);
        swatch(ui, COLOR_NEUTRAL, "Neutral");

        // Visual gap before the marker shape legend so the eye reads
        // them as a separate group ("here's what the shapes above
        // the bar mean"). Render in `p1_color` so the user sees the
        // shape associated with one of the palette colors they've
        // already learned — they'll generalize the tinting from there.
        ui.add_space(20.0);
        triangle_swatch(ui, p1_color, "Kill");
        ui.add_space(12.0);
        circle_swatch(ui, p1_color, "Long combo");
    });
}

fn swatch(ui: &mut egui::Ui, color: egui::Color32, label: &str) {
    let size = egui::vec2(14.0, 14.0);
    let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
    ui.painter().rect_filled(rect, 2.0, color);
    ui.label(label);
}

/// Filled-triangle swatch matching the kill-marker shape. Same
/// signature shape as `swatch` so the legend's render code reads
/// uniformly.
fn triangle_swatch(ui: &mut egui::Ui, color: egui::Color32, label: &str) {
    let size = egui::vec2(14.0, 14.0);
    let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
    let painter = ui.painter();
    // Match the proportions of the rendered marker — apex at the
    // bottom, base across the top.
    let half = rect.width() * 0.45;
    let cx = rect.center().x;
    painter.add(egui::Shape::convex_polygon(
        vec![
            egui::pos2(cx - half, rect.top() + 1.0),
            egui::pos2(cx + half, rect.top() + 1.0),
            egui::pos2(cx, rect.bottom() - 1.0),
        ],
        color,
        egui::Stroke::new(0.5, egui::Color32::from_rgb(20, 20, 25)),
    ));
    ui.label(label);
}

/// Filled-circle swatch matching the long-combo marker shape.
fn circle_swatch(ui: &mut egui::Ui, color: egui::Color32, label: &str) {
    let size = egui::vec2(14.0, 14.0);
    let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
    let r = rect.width() * 0.32;
    ui.painter().circle_filled(rect.center(), r, color);
    ui.label(label);
}

/// Pick the dominant color for a slice of frames using the
/// `Trade > Adv > Neutral` priority described above.
///
/// `palette` is `(color_for_AdvP1, color_for_AdvP2)` from
/// [`user_palette`]; passing it in rather than referencing the
/// constants directly is what lets the same `bucket_color` serve
/// both the perspective-flipped and port-relative palettes.
fn bucket_color(slice: &[CombatState], palette: (egui::Color32, egui::Color32)) -> egui::Color32 {
    let (color_p1, color_p2) = palette;
    let mut has_trade = false;
    let mut has_p1 = false;
    let mut has_p2 = false;
    for s in slice {
        match s {
            CombatState::Trade => has_trade = true,
            CombatState::AdvP1 => has_p1 = true,
            CombatState::AdvP2 => has_p2 = true,
            CombatState::Neutral => {}
        }
    }
    if has_trade || (has_p1 && has_p2) {
        COLOR_TRADE
    } else if has_p1 {
        color_p1
    } else if has_p2 {
        color_p2
    } else {
        COLOR_NEUTRAL
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ordinal_covers_4_slots() {
        assert_eq!(ordinal(0), "1st");
        assert_eq!(ordinal(1), "2nd");
        assert_eq!(ordinal(2), "3rd");
        assert_eq!(ordinal(3), "4th");
        assert_eq!(ordinal(99), "?");
    }

    #[test]
    fn stage_name_falls_back_to_unknown() {
        let s = ViewerState {
            game_id: 1,
            replay_path: None,
            stage_id: 999,
            duration_seconds: 0,
            ingested_at: String::new(),
            players: Vec::new(),
            analysis: Err("no data".to_string()),
            user_perspective: None,
            key_moments: Vec::new(),
        };
        assert_eq!(s.stage_name(), "Unknown");
    }

    #[test]
    fn duration_display_matches_mmss() {
        let mk = |secs| ViewerState {
            game_id: 0,
            replay_path: None,
            stage_id: 0,
            duration_seconds: secs,
            ingested_at: String::new(),
            players: Vec::new(),
            analysis: Err("".to_string()),
            user_perspective: None,
            key_moments: Vec::new(),
        };
        assert_eq!(mk(0).duration_display(), "0:00");
        assert_eq!(mk(7).duration_display(), "0:07");
        assert_eq!(mk(125).duration_display(), "2:05");
        assert_eq!(mk(-5).duration_display(), "0:00");
    }

    #[test]
    fn viewer_player_character_name_resolves() {
        let p = ViewerPlayer {
            placement: 0,
            code: "X".into(),
            character_id: 1, // Fox
            port: 0,
        };
        assert_eq!(p.character_name(), "Fox");

        let p = ViewerPlayer {
            placement: 1,
            code: "Y".into(),
            character_id: 999,
            port: 1,
        };
        assert_eq!(p.character_name(), "Unknown");
    }

    /// Default port-relative palette — what the bar uses when no
    /// user perspective is set.
    fn default_palette() -> (egui::Color32, egui::Color32) {
        user_palette(None)
    }

    #[test]
    fn bucket_color_priority_trade_over_adv() {
        let slice = [
            CombatState::Neutral,
            CombatState::AdvP1,
            CombatState::Trade,
        ];
        assert_eq!(bucket_color(&slice, default_palette()), COLOR_TRADE);
    }

    #[test]
    fn bucket_color_both_sides_advantage_renders_as_trade() {
        // Even without an explicit Trade frame, seeing both players
        // advantaged within the same pixel window means "something
        // exchange-y happened" — color it as a trade.
        let slice = [CombatState::AdvP1, CombatState::AdvP2];
        assert_eq!(bucket_color(&slice, default_palette()), COLOR_TRADE);
    }

    #[test]
    fn bucket_color_single_side_advantage() {
        let palette = default_palette();
        assert_eq!(
            bucket_color(&[CombatState::AdvP1, CombatState::Neutral], palette),
            COLOR_GREEN
        );
        assert_eq!(
            bucket_color(&[CombatState::Neutral, CombatState::AdvP2], palette),
            COLOR_RED
        );
    }

    #[test]
    fn bucket_color_all_neutral() {
        let slice = [CombatState::Neutral; 4];
        assert_eq!(bucket_color(&slice, default_palette()), COLOR_NEUTRAL);
    }

    #[test]
    fn bucket_color_empty_is_neutral() {
        assert_eq!(bucket_color(&[], default_palette()), COLOR_NEUTRAL);
    }

    // --- user-perspective palette --------------------------------------

    #[test]
    fn user_palette_no_perspective_keeps_port_relative_colors() {
        // No perspective set → green = AdvP1, red = AdvP2 (the
        // arbitrary-but-stable port-relative default).
        assert_eq!(user_palette(None), (COLOR_GREEN, COLOR_RED));
    }

    #[test]
    fn user_palette_p1_is_green_for_user() {
        // User is the lower-port player → AdvP1 (= "user has
        // advantage") is green, AdvP2 is red. Same colors as the
        // port-relative default by coincidence — but the semantic
        // meaning has changed.
        assert_eq!(user_palette(Some(UserPerspective::P1)), (COLOR_GREEN, COLOR_RED));
    }

    #[test]
    fn user_palette_p2_flips_palette() {
        // User is the higher-port player → AdvP2 (= "user has
        // advantage") is green, AdvP1 is red. The palette flips.
        assert_eq!(user_palette(Some(UserPerspective::P2)), (COLOR_RED, COLOR_GREEN));
    }

    #[test]
    fn bucket_color_uses_passed_palette_for_perspective_flip() {
        // P2-perspective palette should color a P2-advantage frame
        // green (= "you on advantage") and a P1-advantage frame red.
        let palette = user_palette(Some(UserPerspective::P2));
        assert_eq!(
            bucket_color(&[CombatState::AdvP2], palette),
            COLOR_GREEN,
            "P2 advantage with P2 perspective should be green"
        );
        assert_eq!(
            bucket_color(&[CombatState::AdvP1], palette),
            COLOR_RED,
            "P1 advantage with P2 perspective should be red"
        );
    }

    // --- compute_user_perspective --------------------------------------

    fn analysis_with_ports(p1: usize, p2: usize) -> ReplayAnalysis {
        ReplayAnalysis {
            combat: vec![CombatState::Neutral],
            p1_port_idx: p1,
            p2_port_idx: p2,
        }
    }

    fn player(code: &str, port: i32) -> ViewerPlayer {
        ViewerPlayer {
            placement: 0,
            code: code.to_string(),
            character_id: 0,
            port,
        }
    }

    #[test]
    fn perspective_none_when_user_code_unset() {
        let players = vec![player("ME#1", 0), player("OPP#2", 1)];
        let analysis = analysis_with_ports(0, 1);
        assert_eq!(
            compute_user_perspective(None, &players, Some(&analysis)),
            None
        );
        assert_eq!(
            compute_user_perspective(Some(""), &players, Some(&analysis)),
            None
        );
        assert_eq!(
            compute_user_perspective(Some("   "), &players, Some(&analysis)),
            None
        );
    }

    #[test]
    fn perspective_none_when_no_analysis() {
        let players = vec![player("ME#1", 0), player("OPP#2", 1)];
        assert_eq!(compute_user_perspective(Some("ME#1"), &players, None), None);
    }

    #[test]
    fn perspective_p1_when_user_is_lower_port() {
        let players = vec![player("ME#1", 0), player("OPP#2", 1)];
        let analysis = analysis_with_ports(0, 1);
        assert_eq!(
            compute_user_perspective(Some("ME#1"), &players, Some(&analysis)),
            Some(UserPerspective::P1)
        );
    }

    #[test]
    fn perspective_p2_when_user_is_higher_port() {
        let players = vec![player("ME#1", 2), player("OPP#2", 0)];
        let analysis = analysis_with_ports(0, 2);
        assert_eq!(
            compute_user_perspective(Some("ME#1"), &players, Some(&analysis)),
            Some(UserPerspective::P2)
        );
    }

    #[test]
    fn perspective_none_when_user_code_no_match() {
        let players = vec![player("OTHER#1", 0), player("OPP#2", 1)];
        let analysis = analysis_with_ports(0, 1);
        assert_eq!(
            compute_user_perspective(Some("ME#1"), &players, Some(&analysis)),
            None
        );
    }

    // --- key-moment derivation -----------------------------------------

    fn punish(
        attacker_id: i32,
        start_frame: i32,
        end_frame: i32,
        hit_count: i32,
        did_kill: bool,
        kill_move: Option<i32>,
    ) -> stats_melee::models::Punish {
        stats_melee::models::Punish {
            id: 0,
            game_id: 0,
            attacker_id,
            victim_id: 0,
            start_frame,
            end_frame,
            hit_count,
            did_kill: if did_kill { 1 } else { 0 },
            kill_move,
        }
    }

    #[test]
    fn key_moments_skips_short_combos_that_dont_kill() {
        let mut gp_to_port = std::collections::HashMap::new();
        gp_to_port.insert(1, 0);
        let analysis = analysis_with_ports(0, 1);

        // A 2-hit non-kill — below LONG_COMBO_THRESHOLD, no marker.
        let punishes = vec![punish(1, 100, 110, 2, false, None)];
        let out = key_moments_from_punishes(&punishes, &gp_to_port, &analysis);
        assert!(out.is_empty(), "short combo should not earn a marker: {out:?}");
    }

    #[test]
    fn key_moments_keeps_kills_regardless_of_hit_count() {
        // Even a 1-hit kill (a sweetspotted kill move) is the most
        // important kind of marker — must always render.
        let mut gp_to_port = std::collections::HashMap::new();
        gp_to_port.insert(1, 0);
        let analysis = analysis_with_ports(0, 1);

        let punishes = vec![punish(1, 100, 105, 1, true, Some(11))];
        let out = key_moments_from_punishes(&punishes, &gp_to_port, &analysis);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].kind, KeyMomentKind::Kill);
        // Kill markers anchor at end_frame (moment of impact).
        assert_eq!(out[0].frame, 105);
        // P1 attacked (gp 1 → port 0 == p1_port_idx).
        assert!(out[0].by_p1);
        // Tooltip resolves the attack id through the gamedata table.
        assert!(
            out[0].label.contains("forward smash"),
            "kill label should resolve attack #11 to forward smash, got {:?}",
            out[0].label
        );
        assert!(out[0].label.contains("1 hits"), "got: {:?}", out[0].label);
    }

    #[test]
    fn key_moments_long_combo_anchors_at_start_frame() {
        let mut gp_to_port = std::collections::HashMap::new();
        gp_to_port.insert(1, 0);
        let analysis = analysis_with_ports(0, 1);

        // 5-hit combo that didn't kill — over the threshold, anchors
        // at start_frame so the marker sits at the start of the
        // momentum, not the end.
        let punishes = vec![punish(1, 200, 280, 5, false, None)];
        let out = key_moments_from_punishes(&punishes, &gp_to_port, &analysis);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].kind, KeyMomentKind::LongCombo);
        assert_eq!(out[0].frame, 200);
        assert_eq!(out[0].label, "5-hit combo");
    }

    #[test]
    fn key_moments_attribute_to_higher_port_attacker() {
        let mut gp_to_port = std::collections::HashMap::new();
        gp_to_port.insert(7, 2); // attacker on port 2
        let analysis = analysis_with_ports(0, 2);

        let punishes = vec![punish(7, 50, 60, 4, false, None)];
        let out = key_moments_from_punishes(&punishes, &gp_to_port, &analysis);
        assert_eq!(out.len(), 1);
        assert!(
            !out[0].by_p1,
            "attacker on p2_port_idx should set by_p1 = false"
        );
    }

    #[test]
    fn key_moments_drops_punishes_with_unresolvable_attacker() {
        // Attacker gp_id isn't in the port map — this shouldn't happen
        // for properly-ingested 1v1 games, but we silently drop rather
        // than panic.
        let gp_to_port = std::collections::HashMap::new();
        let analysis = analysis_with_ports(0, 1);
        let punishes = vec![punish(99, 0, 5, 5, true, Some(11))];
        let out = key_moments_from_punishes(&punishes, &gp_to_port, &analysis);
        assert!(out.is_empty());
    }

    #[test]
    fn key_moments_unknown_attack_id_falls_back_in_label() {
        // attack id 23 isn't in the universal name table — we should
        // still emit a marker with the "attack #N" placeholder.
        let mut gp_to_port = std::collections::HashMap::new();
        gp_to_port.insert(1, 0);
        let analysis = analysis_with_ports(0, 1);
        let punishes = vec![punish(1, 0, 10, 3, true, Some(23))];
        let out = key_moments_from_punishes(&punishes, &gp_to_port, &analysis);
        assert_eq!(out.len(), 1);
        assert!(
            out[0].label.contains("attack #23"),
            "got: {:?}",
            out[0].label
        );
    }
}
