//! Character + stage icons for the replay library and viewer.
//!
//! Two-tier strategy so the UI looks good with or without art assets:
//!
//! 1. **Real PNG icons** — if a matching file exists under the assets
//!    directory (`assets/characters/<Name>.png`,
//!    `assets/stages/<Name>.png`), we decode it once, upload it as a GPU
//!    texture, and cache the [`egui::TextureHandle`]. Drop in a Slippi /
//!    community stock-icon pack named by character/stage and they light
//!    up with no code change.
//! 2. **Drawn badges** — when no PNG is present we paint a rounded badge
//!    tinted by the character's series color (or a per-stage color) with
//!    a short abbreviation. Zero external assets, fully themeable.
//!
//! The cache stores `Option<TextureHandle>`, so a missing file is probed
//! exactly once and thereafter falls straight through to the badge path —
//! no per-frame disk stats.
//!
//! ## Asset directory resolution
//!
//! Looked up once and memoized. In priority order:
//! 1. `<exe_dir>/assets` — the future bundled / packaged layout.
//! 2. `<exe_dir>/../Resources/assets` — inside a macOS `.app` bundle.
//! 3. `$CARGO_MANIFEST_DIR/assets` — the source tree, for `cargo run`.

use std::collections::HashMap;
use std::path::PathBuf;

use eframe::egui;
use stats_melee::gamedata::{CHARACTERS, STAGES};

/// Per-session texture cache for character + stage icons. Lives on the
/// app struct so uploaded textures persist across frames.
#[derive(Default)]
pub struct IconCache {
    /// Resolved assets base dir, computed lazily on first use.
    /// `Some(None)` would be ambiguous, so we use a separate "resolved"
    /// flag via the outer `Option`: `None` = not yet probed.
    base: Option<Option<PathBuf>>,
    chars: HashMap<i32, Option<egui::TextureHandle>>,
    stages: HashMap<i32, Option<egui::TextureHandle>>,
}

impl IconCache {
    /// Texture handle for a character icon, or `None` if there's no PNG
    /// for that id (caller should draw a badge instead).
    pub fn character(&mut self, ctx: &egui::Context, id: i32) -> Option<egui::TextureHandle> {
        if let Some(hit) = self.chars.get(&id) {
            return hit.clone();
        }
        let base = self.resolve_base().clone();
        let loaded = base
            .as_ref()
            .and_then(|b| name_at(b, "characters", &CHARACTERS, id))
            .and_then(|p| load_texture(ctx, &p));
        self.chars.insert(id, loaded.clone());
        loaded
    }

    /// Texture handle for a stage icon, or `None` if there's no PNG.
    pub fn stage(&mut self, ctx: &egui::Context, id: i32) -> Option<egui::TextureHandle> {
        if let Some(hit) = self.stages.get(&id) {
            return hit.clone();
        }
        let base = self.resolve_base().clone();
        let loaded = base
            .as_ref()
            .and_then(|b| name_at(b, "stages", &STAGES, id))
            .and_then(|p| load_texture(ctx, &p));
        self.stages.insert(id, loaded.clone());
        loaded
    }

    fn resolve_base(&mut self) -> &Option<PathBuf> {
        self.base.get_or_insert_with(assets_base)
    }
}

/// Build `<base>/<sub>/<Name>.png` for a valid id whose table entry
/// exists. Returns `None` for out-of-range ids.
fn name_at(base: &std::path::Path, sub: &str, table: &[&str], id: i32) -> Option<PathBuf> {
    let name = usize::try_from(id).ok().and_then(|i| table.get(i).copied())?;
    Some(base.join(sub).join(format!("{name}.png")))
}

/// Decode a PNG off disk and upload it as a texture. `None` if the file
/// is absent or undecodable — both fall through to the badge path.
fn load_texture(ctx: &egui::Context, path: &std::path::Path) -> Option<egui::TextureHandle> {
    let bytes = std::fs::read(path).ok()?;
    let img = image::load_from_memory(&bytes).ok()?;
    let rgba = img.to_rgba8();
    let size = [rgba.width() as usize, rgba.height() as usize];
    let pixels = rgba.into_raw();
    let color_image = egui::ColorImage::from_rgba_unmultiplied(size, &pixels);
    let name = path.file_stem().and_then(|s| s.to_str()).unwrap_or("icon");
    Some(ctx.load_texture(name, color_image, egui::TextureOptions::LINEAR))
}

fn assets_base() -> Option<PathBuf> {
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let next_to_exe = dir.join("assets");
            if next_to_exe.is_dir() {
                return Some(next_to_exe);
            }
            let bundled = dir.join("../Resources/assets");
            if bundled.is_dir() {
                return Some(bundled);
            }
        }
    }
    let dev = PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/assets"));
    dev.is_dir().then_some(dev)
}

// --- Rendering helpers ------------------------------------------------------

/// Render a character icon at `size`×`size` points: real PNG if present,
/// otherwise a tinted badge with the character's abbreviation.
pub fn character_icon(ui: &mut egui::Ui, icons: &mut IconCache, id: i32, size: f32) {
    let sz = egui::vec2(size, size);
    if let Some(tex) = icons.character(ui.ctx(), id) {
        let src = egui::load::SizedTexture::new(tex.id(), sz);
        ui.add(egui::Image::from_texture(src).rounding(egui::Rounding::same(3.0)));
    } else {
        draw_badge(ui, sz, character_color(id), &character_abbrev(id));
    }
}

/// Render a stage icon at `size`×`size` points: real PNG if present,
/// otherwise a tinted badge with the stage's abbreviation.
pub fn stage_icon(ui: &mut egui::Ui, icons: &mut IconCache, id: i32, size: f32) {
    let sz = egui::vec2(size, size);
    if let Some(tex) = icons.stage(ui.ctx(), id) {
        let src = egui::load::SizedTexture::new(tex.id(), sz);
        ui.add(egui::Image::from_texture(src).rounding(egui::Rounding::same(3.0)));
    } else {
        draw_badge(ui, sz, stage_color(id), &stage_abbrev(id));
    }
}

fn draw_badge(ui: &mut egui::Ui, size: egui::Vec2, color: egui::Color32, text: &str) {
    let (rect, _resp) = ui.allocate_exact_size(size, egui::Sense::hover());
    if !ui.is_rect_visible(rect) {
        return;
    }
    let painter = ui.painter();
    let radius = size.x.min(size.y) * 0.5;
    // Circular disc — reads as an icon rather than a flat colored tag.
    painter.circle_filled(rect.center(), radius, color);
    painter.text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        text,
        egui::FontId::proportional(size.y * 0.4),
        readable_on(color),
    );
}

/// Black or white text depending on the badge background luminance, so
/// the abbreviation stays legible on light *and* dark tints.
fn readable_on(bg: egui::Color32) -> egui::Color32 {
    let l = 0.299 * bg.r() as f32 + 0.587 * bg.g() as f32 + 0.114 * bg.b() as f32;
    if l > 150.0 {
        egui::Color32::from_gray(20)
    } else {
        egui::Color32::from_gray(235)
    }
}

/// 2–3 char abbreviation derived from CamelCase capitals (`CaptainFalcon`
/// → `CF`, `DonkeyKong` → `DK`); single-word names take their first
/// three letters (`Fox` → `FOX`).
fn abbrev(name: &str) -> String {
    let caps: String = name.chars().filter(|c| c.is_ascii_uppercase()).collect();
    if caps.len() >= 2 {
        caps.chars().take(3).collect()
    } else {
        name.chars().take(3).collect::<String>().to_uppercase()
    }
}

fn character_abbrev(id: i32) -> String {
    usize::try_from(id)
        .ok()
        .and_then(|i| CHARACTERS.get(i).copied())
        .map(abbrev)
        .unwrap_or_else(|| "?".to_string())
}

/// Community-standard shorthand for the tournament-legal stages; anything
/// else falls back to the generic [`abbrev`].
fn stage_abbrev(id: i32) -> String {
    match id {
        2 => "FoD".to_string(),
        3 => "PS".to_string(),
        8 => "YS".to_string(),
        28 => "DL".to_string(),
        31 => "BF".to_string(),
        32 => "FD".to_string(),
        _ => usize::try_from(id)
            .ok()
            .and_then(|i| STAGES.get(i).copied())
            .map(abbrev)
            .unwrap_or_else(|| "?".to_string()),
    }
}

fn rgb(r: u8, g: u8, b: u8) -> egui::Color32 {
    egui::Color32::from_rgb(r, g, b)
}

/// Representative series / costume color per character for the badge
/// fallback. Unknown ids get a neutral gray.
fn character_color(id: i32) -> egui::Color32 {
    match id {
        0 => rgb(0xE0, 0x32, 0x2E),  // Mario
        1 => rgb(0xD9, 0x8C, 0x3F),  // Fox
        2 => rgb(0x3E, 0x4E, 0x8A),  // Captain Falcon
        3 => rgb(0x7A, 0x4A, 0x2B),  // Donkey Kong
        4 => rgb(0xE5, 0x8F, 0xB0),  // Kirby
        5 => rgb(0x4E, 0x7A, 0x3A),  // Bowser
        6 => rgb(0x3E, 0x8E, 0x5A),  // Link
        7 => rgb(0x6B, 0x6F, 0x86),  // Sheik
        8 => rgb(0xC2, 0x3B, 0x3B),  // Ness
        9 => rgb(0xE5, 0x8F, 0xB0),  // Peach
        10 => rgb(0x5A, 0x8A, 0xD0), // Popo
        11 => rgb(0xE5, 0x8F, 0xB0), // Nana
        12 => rgb(0xE6, 0xC5, 0x3A), // Pikachu
        13 => rgb(0xD9, 0x77, 0x2F), // Samus
        14 => rgb(0x5B, 0xA8, 0x4E), // Yoshi
        15 => rgb(0xE8, 0x9B, 0xB8), // Jigglypuff
        16 => rgb(0x8A, 0x6F, 0xB0), // Mewtwo
        17 => rgb(0x3F, 0xA3, 0x5A), // Luigi
        18 => rgb(0x46, 0x64, 0xB0), // Marth
        19 => rgb(0xB0, 0x56, 0x8A), // Zelda
        20 => rgb(0x4E, 0x9E, 0x66), // Young Link
        21 => rgb(0xD0, 0x45, 0x45), // Dr. Mario
        22 => rgb(0x3A, 0x6A, 0xB8), // Falco
        23 => rgb(0xE6, 0xCB, 0x54), // Pichu
        24 => rgb(0x2A, 0x2A, 0x2A), // Game & Watch
        25 => rgb(0x6E, 0x5A, 0x3A), // Ganondorf
        26 => rgb(0x8A, 0x3A, 0x52), // Roy
        _ => rgb(0x5A, 0x5A, 0x5A),
    }
}

/// Per-stage badge color. Tournament-legal stages get distinct tints;
/// everything else is neutral gray.
fn stage_color(id: i32) -> egui::Color32 {
    match id {
        2 => rgb(0x3A, 0x8A, 0x8A),  // Fountain of Dreams
        3 => rgb(0x5A, 0x8A, 0x4E),  // Pokemon Stadium
        8 => rgb(0x6A, 0xA8, 0x4E),  // Yoshi's Story
        28 => rgb(0x5A, 0xA0, 0xC8), // Dream Land N64
        31 => rgb(0x5A, 0x4E, 0x8A), // Battlefield
        32 => rgb(0x3A, 0x40, 0x60), // Final Destination
        _ => rgb(0x5A, 0x5A, 0x5A),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn abbrev_camelcase_takes_capitals() {
        assert_eq!(abbrev("CaptainFalcon"), "CF");
        assert_eq!(abbrev("DonkeyKong"), "DK");
        assert_eq!(abbrev("GameAndWatch"), "GAW");
    }

    #[test]
    fn abbrev_single_word_takes_first_three() {
        assert_eq!(abbrev("Fox"), "FOX");
        assert_eq!(abbrev("Mario"), "MAR");
    }

    #[test]
    fn stage_abbrev_uses_community_shorthand() {
        assert_eq!(stage_abbrev(2), "FoD");
        assert_eq!(stage_abbrev(31), "BF");
        assert_eq!(stage_abbrev(32), "FD");
    }

    #[test]
    fn out_of_range_ids_are_safe() {
        assert_eq!(character_abbrev(999), "?");
        assert_eq!(stage_abbrev(999), "?");
        // Color lookups must not panic on junk ids.
        let _ = character_color(-1);
        let _ = stage_color(999);
    }
}
