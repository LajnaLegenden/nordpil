//! nordpil: a lightweight, native MongoDB GUI.

mod app;
mod logging;
mod spike_language;
mod widgets;

use eframe::egui;

fn main() -> eframe::Result {
    let _log_guard = logging::init();
    tracing::info!(version = env!("CARGO_PKG_VERSION"), "starting nordpil");

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("nordpil")
            .with_app_id("nordpil")
            .with_inner_size([1200.0, 800.0])
            .with_min_inner_size([640.0, 400.0]),
        ..Default::default()
    };
    eframe::run_native(
        "nordpil",
        options,
        Box::new(|cc| Ok(Box::new(app::NordpilApp::new(cc)?))),
    )
}
