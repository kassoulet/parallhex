#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod color;
mod entropy;
mod hexview;
mod hilbert;
mod texture;

use eframe::egui;

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("EntropyMap")
            .with_inner_size([1280.0, 800.0])
            .with_min_inner_size([900.0, 600.0]),
        ..Default::default()
    };
    eframe::run_native(
        "EntropyMap",
        options,
        Box::new(|cc| Ok(Box::new(app::EntropyMapApp::new(cc)))),
    )
}
