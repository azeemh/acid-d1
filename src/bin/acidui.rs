use acid_d1::app::AcidApp;
use eframe::egui;
use std::process::ExitCode;

fn main() -> ExitCode {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1200.0, 700.0])
            .with_min_inner_size([800.0, 500.0])
            .with_title("acid-ui — loop-based music production on Linux"),
        ..Default::default()
    };

    match eframe::run_native(
        "acid-ui",
        options,
        Box::new(|_cc| Ok(Box::new(AcidApp::new()))),
    ) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {}", e);
            ExitCode::from(1)
        }
    }
}
