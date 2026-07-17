//! Some Audiobook Player — an egui audiobook player.

mod app;
mod icons;
mod mpris;
mod toast;

use eframe::egui;

fn main() -> eframe::Result {
    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([900.0, 640.0])
            .with_min_inner_size([560.0, 420.0])
            .with_title("Some Audiobook Player")
            // Matches `some-audiobook-player.desktop`, so the compositor links this window to the
            // installed icon (Wayland app_id / X11 WM_CLASS) instead of a generic one.
            .with_app_id("some-audiobook-player"),
        ..Default::default()
    };
    // Optional: `cargo run -- <file.m4b>` opens a book on startup.
    let initial = std::env::args().nth(1).map(std::path::PathBuf::from);
    eframe::run_native(
        "Some Audiobook Player",
        native_options,
        Box::new(move |cc| Ok(Box::new(app::PlayerApp::new(cc, initial)))),
    )
}
