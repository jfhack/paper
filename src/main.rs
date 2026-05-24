#![cfg_attr(all(not(debug_assertions), target_os = "windows"), windows_subsystem = "windows")]

mod app;
mod file_open;
mod pdf;

use app::PaperApp;

fn app_icon() -> eframe::egui::IconData {
    let png = include_bytes!("../assets/icon_256.png");
    match image::load_from_memory(png) {
        Ok(img) => {
            let rgba = img.into_rgba8();
            let (width, height) = rgba.dimensions();
            eframe::egui::IconData { rgba: rgba.into_raw(), width, height }
        }
        Err(_) => eframe::egui::IconData::default(),
    }
}

fn main() -> eframe::Result<()> {
    file_open::install_handler();
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title("Paper PDF editor")
            .with_app_id("paper")
            .with_icon(app_icon())
            .with_inner_size([1360.0, 900.0])
            .with_min_inner_size([800.0, 560.0]),
        persist_window: false,
        ..Default::default()
    };
    eframe::run_native(
        "Paper",
        options,
        Box::new(|cc| Ok(Box::new(PaperApp::new(cc)))),
    )
}
