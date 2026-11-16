//! The desktop program of `record`: audio, video, and screenshots in one
//! window.
//!
//! The window shows a web interface from `ui/dist`. This crate connects that
//! interface to the engines of `record-core`: [`commands`] holds what the
//! interface can ask for, [`engines`] runs the recordings, and [`payload`]
//! defines every value that crosses the boundary.

pub mod commands;
pub mod engines;
pub mod files;
pub mod payload;

use tauri::{Manager, WindowEvent};

use crate::commands::AppState;

/// Opens the window and runs until the user closes it.
///
/// # Panics
///
/// Panics when the window system cannot start, because nothing can run then.
pub fn run() {
    tauri::Builder::default()
        .manage(AppState::start())
        .setup(|app| {
            commands::forward_captures(app.handle());
            Ok(())
        })
        .on_window_event(|window, event| {
            // A recording that runs must close its file first. The window
            // closes when the file is saved.
            if let WindowEvent::CloseRequested { api, .. } = event
                && window.state::<AppState>().hold_window()
            {
                api.prevent_close();
                commands::announce_closing(window.app_handle());
            }
        })
        .invoke_handler(tauri::generate_handler![
            commands::snapshot,
            commands::set_flag,
            commands::set_setting,
            commands::select_destination,
            commands::save_key,
            commands::import_uploader,
            commands::capture,
            commands::toggle_auto,
            commands::monitors,
            commands::start_recording,
            commands::stop_recording,
            commands::set_paused,
            commands::video_frame,
            commands::files,
            commands::picture,
            commands::reveal,
            commands::open_file,
            commands::open_folder,
            commands::copy_picture,
            commands::quit,
        ])
        .run(tauri::generate_context!())
        .expect("the desktop program could not start");
}
