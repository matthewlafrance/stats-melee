//! Reusable pieces of the interface: custom-painted controls, cards, and
//! the win-rate sections shared between the Analytics and Career pages.
//!
//! These are free functions rather than methods because none of them need
//! the app state — they take exactly the data they draw, which keeps them
//! usable from any page and testable in isolation.

use super::*;

/// A double-thumb range slider over the inclusive integer domain
/// `[min, max]`. Edits `lo`/`hi` in place (kept ordered and clamped) and
/// returns `true` when either thumb moved this frame. The active thumb is
/// whichever is nearer the pointer, so dragging from anywhere on the track
/// grabs the closest handle.
pub(super) fn range_slider(ui: &mut egui::Ui, lo: &mut i64, hi: &mut i64, min: i64, max: i64) -> bool {
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


/// A small density histogram of game counts across the date domain
/// `[min, max]`, drawn directly above a [`range_slider`] and sharing its
/// horizontal inset so the bars line up with the slider track. Each game's
/// day ordinal falls into one of up to 48 equal-width bins; bar heights are
/// normalized to the busiest bin. Purely decorative (no interaction).
pub(super) fn render_date_histogram(ui: &mut egui::Ui, ordinals: &[i64], min: i64, max: i64) {
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
pub(super) fn render_date_range_filter(
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


/// A `YYYY-MM-DD` text box that auto-inserts the dashes as the user types
/// (see [`autoformat_ymd`]) so dates are quick to enter. `field` ("from"/"to")
/// disambiguates the two boxes within one filter and `label` disambiguates the
/// two filters — together they make a stable widget id so the caret fix
/// targets the right box.
pub(super) fn date_text_edit(ui: &mut egui::Ui, label: &str, field: &str, value: &mut String, hint: &str) {
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
pub(super) fn sortable_header(
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
pub(super) fn render_slot_cell(
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
pub(super) fn view_pill(ui: &mut egui::Ui, current: Page, target: Page, label: &str) -> bool {
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
pub(super) fn argmax_winproportion(arr: &[WinProportion]) -> Option<(usize, &WinProportion)> {
    arr.iter()
        .enumerate()
        .filter(|(_, wp)| wp.total > 0)
        .max_by_key(|(_, wp)| wp.total)
}

/// A "favorite" card for the Career page: a small label over an icon + name,
/// with a "{games} games" subtitle. The icon is drawn by `draw_icon` so
/// character / stage / icon-less (opponent) variants share one renderer.
pub(super) fn favorite_card(
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
pub(super) fn render_top_kill_moves(ui: &mut egui::Ui, s: &PlayerSummary, character_filter: Option<i32>) {
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
pub(super) fn summary_metrics_block(ui: &mut egui::Ui, s: &PlayerSummary) {
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
pub(super) fn icon_select_row(
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
pub(super) fn filter_field_label(ui: &mut egui::Ui, text: &str) {
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
pub(super) fn character_filter_combo(
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
pub(super) fn stage_filter_combo(
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
pub(super) fn metric_card(ui: &mut egui::Ui, label: &str, value: &str) {
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
pub(super) fn primary_button(ui: &mut egui::Ui, label: &str) -> egui::Response {
    ui.add(egui::Button::new(egui::RichText::new(label).color(ON_ACCENT).strong()).fill(ACCENT))
}

/// A destructive-action button — flame fill, white text. Use for delete
/// confirmations and other irreversible actions.
pub(super) fn danger_button(ui: &mut egui::Ui, label: &str) -> egui::Response {
    ui.add(
        egui::Button::new(egui::RichText::new(label).color(egui::Color32::WHITE).strong())
            .fill(FLAME),
    )
}

/// Lerp two RGB triples in sRGB space.
pub(super) fn lerp_rgb(a: (u8, u8, u8), b: (u8, u8, u8), t: f32) -> egui::Color32 {
    let t = t.clamp(0.0, 1.0);
    let l = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    egui::Color32::from_rgb(l(a.0, b.0), l(a.1, b.1), l(a.2, b.2))
}

/// Win-rate color ramp: red (low) → amber (even) → green (high).
pub(super) fn win_color(p: f32) -> egui::Color32 {
    if p < 0.5 {
        lerp_rgb((212, 80, 80), (216, 176, 72), p / 0.5)
    } else {
        lerp_rgb((216, 176, 72), (90, 190, 110), (p - 0.5) / 0.5)
    }
}

/// Horizontal win-rate bar: a dark track with a colored fill proportional
/// to `proportion` (0..=1).
pub(super) fn draw_win_bar(ui: &mut egui::Ui, proportion: f32, width: f32, height: f32) {
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
pub(super) fn winrate_row(
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
pub(super) fn char_winrate_section(
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
pub(super) fn stage_winrate_section(
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
pub(super) fn opponent_winrate_section(
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

