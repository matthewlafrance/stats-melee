//! The app's fixed color palette and layout constants.
//!
//! Split out so a theme change is a change to one file, and so the page
//! modules can reach for a color without the palette being tangled up in
//! the app state that happens to live beside it.

use eframe::egui;

// === Melee palette ===========================================================
// A single fixed dark theme inspired by the Melee title screen: a deep
// indigo/purple base under a warm "Melee gold" accent, with flame-orange for
// destructive actions. No light mode — every surface reads against this base.
// `Color32::from_rgb` is `const`, so these compose into the style at startup
// and are referenced directly by the custom-painted widgets.

/// App background — the deep indigo behind every panel.
pub(super) const BG_APP: egui::Color32 = egui::Color32::from_rgb(0x16, 0x13, 0x20);
/// Raised window / menu / popup fill, one step up from [`BG_APP`].
pub(super) const BG_WINDOW: egui::Color32 = egui::Color32::from_rgb(0x1F, 0x1B, 0x2E);
/// "Card" / info-bubble surface — metric cards, favorites, head-to-head.
pub(super) const BG_CARD: egui::Color32 = egui::Color32::from_rgb(0x29, 0x23, 0x3A);
/// Deepest sink — text-edit interiors, the floating nav capsule.
pub(super) const BG_EXTREME: egui::Color32 = egui::Color32::from_rgb(0x0F, 0x0D, 0x17);
/// Striped table rows / faint backgrounds.
pub(super) const BG_STRIPE: egui::Color32 = egui::Color32::from_rgb(0x21, 0x1C, 0x30);
/// Neutral raised fill — default button bodies and bar tracks.
pub(super) const BG_TRACK: egui::Color32 = egui::Color32::from_rgb(0x33, 0x2C, 0x47);
/// Same, one shade lighter — button hover.
pub(super) const BG_TRACK_HI: egui::Color32 = egui::Color32::from_rgb(0x40, 0x37, 0x59);

/// Melee gold — primary accent: active nav, primary buttons, selection,
/// the user's own connect code, the settings gear when open.
pub(super) const ACCENT: egui::Color32 = egui::Color32::from_rgb(0xE7, 0xB1, 0x3B);
/// Brighter gold for hover strokes / pressed primary buttons.
pub(super) const ACCENT_HI: egui::Color32 = egui::Color32::from_rgb(0xF4, 0xC9, 0x5A);
/// Dark text/iconography that sits *on top of* the gold accent.
pub(super) const ON_ACCENT: egui::Color32 = egui::Color32::from_rgb(0x1A, 0x12, 0x06);
/// Flame orange-red — destructive actions and the loss marker.
pub(super) const FLAME: egui::Color32 = egui::Color32::from_rgb(0xDD, 0x52, 0x33);
/// Victory green — the win marker and the top of the win-rate ramp.
pub(super) const WIN_GREEN: egui::Color32 = egui::Color32::from_rgb(0x5F, 0xC1, 0x6E);

/// Primary text.
pub(super) const TEXT_HI: egui::Color32 = egui::Color32::from_rgb(0xED, 0xE9, 0xF4);
/// Muted secondary text — captions, sublabels, inactive controls.
pub(super) const TEXT_MUTED: egui::Color32 = egui::Color32::from_rgb(0x9A, 0x92, 0xAD);

/// Max width of the centered page content column. Sized to fit the full
/// replay-library row (~1095 px of columns + ~70 px of inter-column spacing
/// across the ID / outcome / two player / stage / duration / played / added /
/// view / delete columns) so the table sits centered rather than hugging the
/// left edge on wide windows.
pub(super) const CONTENT_MAX_WIDTH: f32 = 1180.0;

// --- applying the palette to egui -------------------------------------------

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
pub(super) fn apply_theme(ctx: &egui::Context) {
    ctx.options_mut(|o| o.theme_preference = egui::ThemePreference::Dark);
    let style = build_style();
    ctx.set_style_of(egui::Theme::Dark, style.clone());
    ctx.set_style_of(egui::Theme::Light, style);
}

/// Build the one [`egui::Style`]: shared spacing + type scale, the Melee
/// surface palette, and uniform button styling so every `ui.button()` reads as
/// the same raised gold-on-hover control. Custom-painted widgets (cards, win
/// bars, nav capsule) pull their fills from the palette constants directly.
pub(super) fn build_style() -> egui::Style {
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
pub(super) fn surface_fill(_visuals: &egui::Visuals) -> egui::Color32 {
    BG_CARD
}

/// The neutral track behind a colored win-rate bar fill.
pub(super) fn track_fill(_visuals: &egui::Visuals) -> egui::Color32 {
    BG_TRACK
}

/// Linearly blend two opaque colors in sRGB space (alpha ignored), `t` from
/// `a`→`b`. Used to tint a status banner's background toward its accent
/// without the `linear_multiply` darkening that breaks in light mode.
pub(super) fn mix_color(a: egui::Color32, b: egui::Color32, t: f32) -> egui::Color32 {
    let t = t.clamp(0.0, 1.0);
    let f = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    egui::Color32::from_rgb(f(a.r(), b.r()), f(a.g(), b.g()), f(a.b(), b.b()))
}

