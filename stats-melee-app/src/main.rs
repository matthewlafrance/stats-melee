//! eframe entry point for the stats-melee desktop app.

// On Windows, build against the "windows" subsystem in release so
// double-clicking the .exe launches the GUI without a console window behind
// it. Debug builds keep the console so `eprintln!` diagnostics stay visible.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use eframe::egui;

mod app;
mod app_icon;
mod config;
mod icons;
mod replay_list;
mod slippi;
mod slippi_icons;
mod viewer;

use app::StatsMeleeApp;

fn main() -> eframe::Result<()> {
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
        // The explicit `as Box<dyn eframe::App>` cast is load-bearing: the
        // unsized coercion from `Box<StatsMeleeApp>` to `Box<dyn App>`
        // doesn't fire inside an `Ok(_)` constructor, so we spell it out.
        Box::new(|cc| Ok(Box::new(StatsMeleeApp::new(cc)) as Box<dyn eframe::App>)),
    )
}

/// Build the window / taskbar icon from the shared procedural source
/// ([`app_icon::diamond_rgba`]) — a Melee-gold diamond on the deep-indigo app
/// background, rounded like an app tile. The same source is baked into the
/// Windows `.exe` at build time (see `build.rs`), so the window icon and the
/// file/shortcut icon always match.
fn app_icon() -> egui::IconData {
    const S: u32 = 256;
    egui::IconData {
        rgba: app_icon::diamond_rgba(S as usize),
        width: S,
        height: S,
    }
}
