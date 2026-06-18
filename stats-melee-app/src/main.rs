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
            .with_inner_size([1200.0, 800.0])
            .with_min_inner_size([640.0, 480.0]),
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
