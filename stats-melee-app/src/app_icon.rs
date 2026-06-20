//! Procedural app icon — a Melee-gold diamond on the deep-indigo app
//! background, rounded like an app tile.
//!
//! One source of truth for two consumers: the runtime window / taskbar icon
//! (`main.rs`) and the Windows `.exe` icon baked in at build time (`build.rs`
//! `include!`s this file). Generated rather than committed so we don't carry a
//! binary asset, and so the icon stays in sync with the in-app palette.

/// RGBA8 pixels for the app icon at `size`×`size`. The geometry scales with
/// `size`, so callers can render every resolution an `.ico` wants (16…256)
/// from the same code.
pub fn diamond_rgba(size: usize) -> Vec<u8> {
    const BG: [u8; 3] = [0x16, 0x13, 0x20]; // app indigo
    const GOLD: [u8; 3] = [0xE7, 0xB1, 0x3B]; // Melee gold accent
    let s = size as f32;
    let radius = s * (40.0 / 256.0); // rounded-corner radius (proportional)
    let center = s / 2.0;
    let diamond = s * (82.0 / 256.0); // half-width of the centered diamond

    let mut rgba = Vec::with_capacity(size * size * 4);
    for y in 0..size {
        for x in 0..size {
            let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);
            // Rounded-rect mask: clamp to the inset rect; anything farther than
            // `radius` from it (i.e. outside a rounded corner) is transparent.
            let cx = fx.clamp(radius, s - radius);
            let cy = fy.clamp(radius, s - radius);
            let (dx, dy) = (fx - cx, fy - cy);
            if dx * dx + dy * dy > radius * radius {
                rgba.extend_from_slice(&[0, 0, 0, 0]);
                continue;
            }
            let in_diamond = (fx - center).abs() + (fy - center).abs() <= diamond;
            let c = if in_diamond { GOLD } else { BG };
            rgba.extend_from_slice(&[c[0], c[1], c[2], 255]);
        }
    }
    rgba
}
