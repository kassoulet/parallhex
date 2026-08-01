#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod color;
mod entropy;
mod panes;

use eframe::egui;

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("EntropyMap")
            .with_inner_size([1600.0, 900.0])
            .with_min_inner_size([1000.0, 600.0]),
        ..Default::default()
    };
    eframe::run_native(
        "EntropyMap",
        options,
        Box::new(|cc| Ok(Box::new(app::EntropyMapApp::new(cc)))),
    )
}
