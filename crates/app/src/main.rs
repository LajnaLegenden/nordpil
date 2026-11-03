//! nordpil: a lightweight, native MongoDB GUI.

mod app;
mod connections;
mod icons;
mod logging;
mod lookup;
#[cfg(target_os = "macos")]
mod menu;
mod query_language;
mod shell;
mod shortcuts;
mod tabs;
mod theme;
mod ui;
mod update;
mod widgets;

use eframe::egui;
use nordpil_backend::Backend;
use nordpil_backend::secrets::Keyring;
use nordpil_backend::store::ConnectionStore;

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
        Box::new(|cc| {
            let store_path = ConnectionStore::default_path()
                .ok_or_else(|| std::io::Error::other("Could not find a home directory."))?;
            let backend = Backend::new(ConnectionStore::new(store_path), Keyring);
            let mut app = app::NordpilApp::new(&cc.egui_ctx, backend)?;
            #[cfg(target_os = "macos")]
            app.install_menu(&cc.egui_ctx);
            // Development builds aren't replaced by releases.
            if !cfg!(debug_assertions) {
                app.enable_updates(&cc.egui_ctx);
            }
            Ok(Box::new(app))
        }),
    )
}
