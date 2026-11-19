mod app;
mod install_ui;
mod theme;

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Skald — Skyrim SE Mod Manager")
            .with_app_id("skald")
            .with_inner_size([1280.0, 800.0])
            .with_min_inner_size([900.0, 560.0])
            .with_drag_and_drop(true),
        ..Default::default()
    };
    eframe::run_native("Skald", options, Box::new(|cc| Ok(Box::new(app::App::new(cc)))))
}
