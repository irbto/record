//! Starts the `record` desktop dashboard.

// A desktop program has no console window.
#![cfg_attr(windows, windows_subsystem = "windows")]

/// Opens the dashboard window and runs it until the user closes it.
#[cfg(windows)]
fn main() -> eframe::Result {
    use std::sync::Arc;

    use record_core::screenshot::{
        events::HubSettings, hub::Hub, saved::SavedSettings, upload::read_setting,
    };
    use record_desktop::{
        app::{Dashboard, MODE_FILE},
        backend::Mode,
        engines::CoreEngines,
        theme,
    };

    /// Adapts the dashboard to the window framework.
    struct Window(Dashboard<CoreEngines, Hub>);

    impl eframe::App for Window {
        fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
            self.0.ui(ui);
        }
    }

    // `record-desktop video` opens a mode directly. Otherwise the last mode returns.
    let mode = std::env::args()
        .nth(1)
        .and_then(|name| Mode::from_id(&name))
        .or_else(|| read_setting(MODE_FILE).and_then(|name| Mode::from_id(&name)))
        .unwrap_or(Mode::Screenshot);
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("record")
            .with_inner_size([1_080.0, 720.0])
            .with_min_inner_size([760.0, 520.0]),
        ..Default::default()
    };
    eframe::run_native(
        "record",
        options,
        Box::new(move |creation| {
            theme::apply(&creation.egui_ctx);
            let settings = Arc::new(HubSettings::from_saved(&SavedSettings::load()));
            let hub = Hub::start(Arc::clone(&settings))?;
            Ok(Box::new(Window(Dashboard::new(
                CoreEngines,
                hub,
                settings,
                mode,
            ))))
        }),
    )
}

/// Reports that the dashboard needs Windows.
#[cfg(not(windows))]
fn main() {
    eprintln!("record-desktop currently runs on Windows 10/11 only");
}
