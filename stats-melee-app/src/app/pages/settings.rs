//! The Settings page and the actions it fires: directory pickers, config
//! saves, icon re-extraction, replay deletion, and launching a replay in
//! Slippi Dolphin.

use crate::app::*;

impl StatsMeleeApp {
    /// Drop a single replay (game + game_player_stat + punish rows) by
    /// `game_id`, then invalidate the cached row list + analytics
    /// summary + analysis cache entry for that hash. Mirrors
    /// [`Self::nuke_replays`] at row-scope. Called from the per-row
    /// "Confirm?" button on the library table.
    pub(crate) fn delete_replay(&mut self, game_id: i32) {
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
                // The replay's analysis-cache sidecar is now orphaned;
                // it falls out via LRU eviction when the budget rolls over.
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
    pub(crate) fn nuke_replays(&mut self) {
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
    pub(crate) fn pick_replay_dir(&mut self) {
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
    pub(crate) fn pick_slippi_binary(&mut self) {
        if let Some(path) = rfd::FileDialog::new()
            .set_title("Pick your Slippi Dolphin executable")
            .pick_file()
        {
            self.config.slippi_playback_command = Some(path.display().to_string());
            self.save_config();
        }
    }

    /// Open a native file/folder picker for the Slippi Launcher install,
    /// used as the manual source for icon ripping when auto-discovery misses.
    /// We pick a folder (the install dir or the `.app` bundle); the resolver
    /// finds the `app.asar` inside. Cancelling leaves the current value.
    pub(crate) fn pick_slippi_launcher(&mut self) {
        if let Some(path) = rfd::FileDialog::new()
            .set_title("Pick your Slippi Launcher install folder")
            .pick_folder()
        {
            self.config.slippi_launcher_path = Some(path);
            self.save_config();
        }
    }

    /// Open a native file picker for the Melee ISO. Filters to common
    /// disc-image extensions but the user can override — Slippi
    /// Dolphin accepts `.iso`, `.ciso`, `.gcm`, etc.
    pub(crate) fn pick_melee_iso(&mut self) {
        if let Some(path) = rfd::FileDialog::new()
            .set_title("Pick your Super Smash Bros. Melee 1.02 ISO")
            .add_filter("Disc image", &["iso", "ciso", "gcm", "gcz"])
            .pick_file()
        {
            self.config.melee_iso_path = Some(path);
            self.save_config();
        }
    }

    pub(crate) fn save_config(&mut self) {
        match self.config.save() {
            Ok(()) => self.last_config_error = None,
            Err(e) => self.last_config_error = Some(e.to_string()),
        }
    }

    /// Shell out to Slippi Dolphin for the currently-viewed replay.
    /// Stores the outcome in `self.last_slippi_launch` for the viewer
    /// page to display.
    pub(crate) fn launch_in_slippi(&mut self) {
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

    /// Settings action: re-rip character / stage icons from the local Slippi
    /// install into the writable assets dir, then clear the icon cache so the
    /// new art shows immediately. Records the outcome in `last_icon_extract`.
    pub(crate) fn reextract_icons(&mut self) {
        let dest = match AppConfig::default_assets_dir() {
            Ok(d) => d,
            Err(e) => {
                self.last_icon_extract = Some(Err(e.to_string()));
                return;
            }
        };
        let launcher_override = self.config.slippi_launcher_path.as_deref();
        self.last_icon_extract = Some(match crate::slippi_icons::extract_to(&dest, launcher_override)
        {
            Ok(report) => {
                self.icons.clear();
                Ok(report)
            }
            Err(e) => Err(e.to_string()),
        });
    }

    pub(crate) fn page_settings(&mut self, ui: &mut egui::Ui) {
        // Deferred actions from inside the Grid closure — we can't call
        // &mut self methods (save_config, pick_slippi_binary) directly
        // while the grid has a &mut borrow of the UI.
        let mut slippi_binary_save_pending = false;
        let mut slippi_binary_pick_clicked = false;
        let mut melee_iso_save_pending = false;
        let mut melee_iso_pick_clicked = false;
        let mut icon_extract_clicked = false;
        let mut slippi_launcher_pick_clicked = false;
        let mut slippi_launcher_clear_clicked = false;

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

                // Character / stage icons — re-rip from the local Slippi
                // install (e.g. after a Slippi update, or once Slippi is
                // installed if it wasn't at first launch).
                ui.label("Character / stage icons").on_hover_text(
                    "Copies the stock icons + stage art out of your local \
                     Slippi Launcher install. Done automatically on first \
                     launch; use this to refresh after a Slippi update.",
                );
                ui.horizontal(|ui| {
                    if ui.button("Re-extract from Slippi").clicked() {
                        icon_extract_clicked = true;
                    }
                });
                ui.end_row();

                // Slippi Launcher path — manual override for the icon source.
                // The app auto-finds the Launcher's bundle on first launch;
                // this is the escape hatch when that fails (non-standard
                // install dir), so icon ripping never hard-fails.
                ui.label("Slippi Launcher path").on_hover_text(
                    "Only needed if icon extraction can't find Slippi \
                     automatically. Point this at your Slippi Launcher install \
                     folder (or the app.asar inside it); leave unset to \
                     auto-detect.",
                );
                ui.horizontal(|ui| {
                    let display = match &self.config.slippi_launcher_path {
                        Some(p) => p.display().to_string(),
                        None => "(auto-detect)".to_string(),
                    };
                    ui.label(display);
                    if ui.button("Browse…").clicked() {
                        slippi_launcher_pick_clicked = true;
                    }
                    if self.config.slippi_launcher_path.is_some() && ui.button("Clear").clicked() {
                        slippi_launcher_clear_clicked = true;
                    }
                });
                ui.end_row();

                // Melee ISO — optional. Slippi Dolphin already has a default
                // ISO from the Launcher, so playback works without this; we
                // only pass `-e <iso>` when it's set (override / no-default
                // Dolphin). See crate::slippi.
                ui.label("Melee ISO").on_hover_text(
                    "Optional. Path to your Super Smash Bros. Melee 1.02 NTSC \
                     ISO. Usually unnecessary — a Dolphin installed via the \
                     Slippi Launcher already has a default ISO, so replays play \
                     without it. Set it only as an override.",
                );
                ui.horizontal(|ui| {
                    let display = match &self.config.melee_iso_path {
                        Some(p) => p.display().to_string(),
                        None => "(optional — using Slippi's default)".to_string(),
                    };
                    ui.label(display);
                    if ui.button("Browse…").clicked() {
                        melee_iso_pick_clicked = true;
                    }
                    if self.config.melee_iso_path.is_some() && ui.button("Clear").clicked() {
                        self.config.melee_iso_path = None;
                        melee_iso_save_pending = true;
                    }
                });
                ui.end_row();
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
        if melee_iso_save_pending {
            self.save_config();
        }
        if icon_extract_clicked {
            self.reextract_icons();
        }
        if slippi_launcher_pick_clicked {
            self.pick_slippi_launcher();
        }
        if slippi_launcher_clear_clicked {
            self.config.slippi_launcher_path = None;
            self.save_config();
        }

        ui.add_space(16.0);
        if let Some(err) = &self.last_config_error {
            ui.colored_label(egui::Color32::RED, format!("Config save failed: {err}"));
        }
        if let Some(result) = &self.last_icon_extract {
            match result {
                // Found Slippi and parsed its bundle, but resolved no icons —
                // almost always a Slippi version whose asset layout differs.
                // Show the parse breakdown (it's the only diagnostic the
                // console-less release build can surface) and note the badge
                // fallback so the green count isn't mistaken for success.
                Ok(r) if r.characters == 0 && r.stages == 0 => ui.colored_label(
                    egui::Color32::from_rgb(210, 160, 60),
                    format!(
                        "⚠ Found Slippi but matched 0 icons (asset modules: {}, \
                         character refs: {}, stage refs: {}). Your Slippi version's \
                         icon layout may differ — using badge fallbacks. Please \
                         report these numbers.",
                        r.asset_modules, r.char_refs, r.stage_refs
                    ),
                ),
                Ok(r) => ui.colored_label(
                    egui::Color32::from_rgb(90, 180, 100),
                    format!(
                        "✓ Extracted {} character + {} stage icons from Slippi.",
                        r.characters, r.stages
                    ),
                ),
                Err(e) => ui.colored_label(
                    egui::Color32::from_rgb(220, 80, 80),
                    format!("⚠ Icon extraction failed: {e}"),
                ),
            };
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
