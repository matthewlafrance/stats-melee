//! The replay library page: the filter panel, the controls above the table,
//! and the table itself, plus the predicates that decide which rows survive
//! the current filter.

use crate::app::*;

impl StatsMeleeApp {
    /// Whether `r` survives the current Replay Library filters. Used to build
    /// the library's visible-row set and the "showing N of M" count. Now
    /// identical to the structured filter (free-text search was removed); kept
    /// as a named seam in case a library-only filter returns later.
    pub(crate) fn library_row_visible(&self, r: &ReplayRow) -> bool {
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
    pub(crate) fn library_row_matches_structured(&self, r: &ReplayRow) -> bool {
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
    pub(crate) fn library_date_in_range(&self, r: &ReplayRow) -> bool {
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
    pub(crate) fn library_added_date_in_range(&self, r: &ReplayRow) -> bool {
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
    pub(crate) fn library_filter_active(&self) -> bool {
        self.structured_filter_active()
    }

    /// True when any *structured* panel filter (everything except the
    /// free-text search box) is narrowing the set. This is the filter
    /// Analytics shares — when false, the Analytics summary is whole-career.
    pub(crate) fn structured_filter_active(&self) -> bool {
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
    pub(crate) fn library_filtered_game_ids(&self) -> Vec<i32> {
        self.rows
            .iter()
            .filter(|r| self.library_row_matches_structured(r))
            .map(|r| r.game_id)
            .collect()
    }

    /// A short human description of the active structured filter, for the
    /// Analytics header (e.g. "Fox · vs Falco · Battlefield · Wins"). Returns
    /// "Your whole history" when no structured filter is set.
    pub(crate) fn filter_description(&self) -> String {
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
    pub(crate) fn library_played_date_ordinals(&self) -> Vec<i64> {
        self.rows
            .iter()
            .filter_map(|r| r.played_date().and_then(date_to_ordinal))
            .collect()
    }

    /// Ingested ("date added") date of every loaded row, as day ordinals.
    /// The sibling of [`Self::library_played_date_ordinals`] — effectively
    /// one entry per row (every row carries an ingested_at).
    pub(crate) fn library_ingested_date_ordinals(&self) -> Vec<i64> {
        self.rows
            .iter()
            .filter_map(|r| r.ingested_date().and_then(date_to_ordinal))
            .collect()
    }

    /// Sorted, distinct opponent connect codes across loaded rows — the
    /// autocomplete pool for the opponent-tag filter. Excludes the user's
    /// own code when one is configured.
    pub(crate) fn library_opponent_codes(&self) -> Vec<String> {
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
    pub(crate) fn set_sort(&mut self, key: SortKey) {
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
    /// is synchronous — a `.slp` parse is typically sub-second, so we
    /// block the click rather than juggle another worker channel.
    pub(crate) fn open_viewer(&mut self, game_id: i32) {
        self.viewing_game_id = Some(game_id);
        self.viewer_state = None; // invalidate before reload
        // A stale "launched Slippi" status from a previous replay would
        // be confusing next to a freshly-opened viewer.
        self.last_slippi_launch = None;
        self.page = Page::ReplayViewer;
        self.reload_viewer();
    }

    pub(crate) fn page_replay_library(&mut self, ui: &mut egui::Ui) {
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
    pub(crate) fn render_filter_panel(&mut self, ctx: &egui::Context) {
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
    pub(crate) fn render_opponent_tag_filter(&mut self, ui: &mut egui::Ui, codes: &[String]) {
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
    pub(crate) fn render_library_controls(&mut self, ui: &mut egui::Ui) {
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

    pub(crate) fn render_replay_table(&mut self, ui: &mut egui::Ui) {
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
}
