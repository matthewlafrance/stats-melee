//! eframe entry point for the stats-melee desktop app.
//!
//! This is the minimum viable skeleton — a window with a sidebar and a
//! placeholder main panel. Actual database + replay list wiring lands in
//! later tasks; this commit's only job is to prove the workspace split,
//! the eframe dep, and the stats-melee library dep all compile together.
//!
//! See stats-melee/TODO.txt (Track 3) for the roadmap.

use eframe::egui;

mod app;
mod config;
mod icons;
mod render_worker;
mod replay_list;
mod slippi;
mod viewer;

use app::StatsMeleeApp;

fn main() -> eframe::Result<()> {
    // Native window options. Defaults are fine for now — we may want to
    // restore last-known size/position from config later (Track 3 settings
    // work).
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("stats-melee")
            .with_icon(app_icon())
            // Wide enough for the full replay-library row (9 columns +
            // the sidebar) to fit without horizontal scrolling.
            .with_inner_size([1480.0, 900.0])
            .with_min_inner_size([900.0, 560.0]),
        ..Default::default()
    };

    eframe::run_native(
        "stats-melee",
        options,
        // eframe 0.29 changed the AppCreator signature — the closure now
        // returns `Result<Box<dyn App>, Box<dyn Error + Send + Sync>>`
        // so startup errors can be surfaced without panicking.
        //
        // The explicit `as Box<dyn eframe::App>` cast is load-bearing: the
        // unsized coercion from `Box<StatsMeleeApp>` to `Box<dyn App>`
        // doesn't fire inside an `Ok(_)` constructor, so we spell it out.
        Box::new(|cc| Ok(Box::new(StatsMeleeApp::new(cc)) as Box<dyn eframe::App>)),
    )
}

/// Build the window / taskbar icon procedurally — a Melee-gold diamond on the
/// deep-indigo app background, rounded like an app tile. Generated at startup
/// so we don't carry a committed binary asset, and so it stays in sync with
/// the in-app palette.
fn app_icon() -> egui::IconData {
    const S: usize = 256;
    const BG: [u8; 3] = [0x16, 0x13, 0x20]; // app indigo
    const GOLD: [u8; 3] = [0xE7, 0xB1, 0x3B]; // Melee gold accent
    let radius = 40.0_f32; // rounded-corner radius
    let center = S as f32 / 2.0;
    let diamond = 82.0_f32; // half-width of the centered diamond

    let mut rgba = Vec::with_capacity(S * S * 4);
    for y in 0..S {
        for x in 0..S {
            let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);
            // Rounded-rect mask: clamp to the inset rect, anything farther than
            // `radius` from it (i.e. outside a rounded corner) is transparent.
            let cx = fx.clamp(radius, S as f32 - radius);
            let cy = fy.clamp(radius, S as f32 - radius);
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
    egui::IconData {
        rgba,
        width: S as u32,
        height: S as u32,
    }
}
