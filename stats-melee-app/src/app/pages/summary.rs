//! The Analytics and Career pages.
//!
//! Both render a [`PlayerSummary`]; they differ in which one. Analytics
//! shows the summary restricted to the library's current filter, Career the
//! whole-history one plus the win-rate breakdowns. They share the action bar
//! and the "set your code" prompt, which is why they live together.

use crate::app::*;

impl StatsMeleeApp {
    /// Shared "you haven't set a player code" prompt for the Analytics and
    /// Career pages, both of which are per-code.
    pub(crate) fn render_set_code_prompt(&mut self, ui: &mut egui::Ui) {
        ui.label(
            egui::RichText::new("Set your player code in Settings to see your stats.")
                .italics()
                .color(egui::Color32::GRAY),
        );
        if ui.button("Go to Settings").clicked() {
            self.page = Page::Settings;
        }
    }

    /// A compact "Filters · toggle · refresh · for CODE" action bar shared by
    /// the Analytics and Career pages.
    pub(crate) fn render_stats_action_bar(&mut self, ui: &mut egui::Ui, code: &str, show_filter_toggle: bool) {
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

    pub(crate) fn page_analytics(&mut self, ui: &mut egui::Ui) {
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
    pub(crate) fn page_career(&mut self, ui: &mut egui::Ui) {
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
    pub(crate) fn render_career_overview(&mut self, ui: &mut egui::Ui) {
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

    /// The Analytics body: the win rate + metric cards for the filtered game
    /// set and the character-gated top-kill-moves table. The surrounding
    /// header (which describes the active filter) is owned by the caller.
    /// `character_filter` is the shared library "my character" filter — it
    /// gates the kill-moves table (attack ids are only meaningful per
    /// character). Renders an empty-state line when the filter matched no
    /// games.
    pub(crate) fn render_filtered_section(
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
    pub(crate) fn render_win_breakdowns(&mut self, ui: &mut egui::Ui) {
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
}
