//! The desktop program of `record`: audio, video, and screenshots in one
//! window.
//!
//! The window has no system frame. The interface draws the title bar with
//! the menus and the window buttons. The window shows a web interface from
//! `ui/dist`. This crate connects that
//! interface to the engines of `record-core`: [`commands`] holds what the
//! interface can ask for, [`engines`] runs the recordings, [`editing`] cuts
//! clips and edits finished videos, and [`payload`]
//! defines every value that crosses the boundary.
//!
//! One copy of the program runs at a time. It keeps an icon in the tray, so
//! the global hotkeys work while the window is hidden. A second start shows
//! the window of the first copy, and `--quit` asks the first copy to exit.

pub mod commands;
pub mod editing;
pub mod engines;
pub mod files;
pub mod payload;

use record_core::screenshot::CaptureMode;
use tauri::{
    App, AppHandle, Manager, WindowEvent,
    image::Image,
    menu::{MenuBuilder, MenuItemBuilder},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
};

use crate::{
    commands::{AppState, HIDDEN_ARGUMENT, QUIT_ARGUMENT},
    editing::EditState,
};

/// Edge of the tray picture in pixels.
const TRAY_EDGE: u32 = 32;
/// The tray picture as packed RGBA bytes.
const TRAY_PICTURE: &[u8] = include_bytes!("../icons/tray-32.rgba");

/// Reports whether an argument list holds one argument.
fn has_argument(arguments: &[String], wanted: &str) -> bool {
    arguments.iter().skip(1).any(|argument| argument == wanted)
}

/// Adds the tray icon and its menu.
fn add_tray(app: &App) -> tauri::Result<()> {
    let item = |id: &str, label: &str| MenuItemBuilder::with_id(id, label).build(app);
    let menu = MenuBuilder::new(app)
        .item(&item("open", "Open record")?)
        .separator()
        .item(&item("region", "Capture region")?)
        .item(&item("fullscreen", "Capture fullscreen")?)
        .item(&item("window", "Capture active window")?)
        .separator()
        .item(&item("quit", "Quit")?)
        .build()?;
    TrayIconBuilder::with_id("record")
        .icon(Image::new(TRAY_PICTURE, TRAY_EDGE, TRAY_EDGE))
        .tooltip("record")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app: &AppHandle, event| match event.id().as_ref() {
            "open" => commands::show_window(app),
            "region" => commands::capture_from_tray(app, CaptureMode::Region),
            "fullscreen" => commands::capture_from_tray(app, CaptureMode::Fullscreen),
            "window" => commands::capture_from_tray(app, CaptureMode::Window),
            "quit" => commands::request_quit(app),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                commands::show_window(tray.app_handle());
            }
        })
        .build(app)?;
    Ok(())
}

/// Opens the window and runs until the user quits.
///
/// # Panics
///
/// Panics when the window system cannot start, because nothing can run then.
pub fn run() {
    let arguments: Vec<String> = std::env::args().collect();
    tauri::Builder::default()
        // This runs in the first copy when a second copy starts.
        .plugin(tauri_plugin_single_instance::init(|app, arguments, _| {
            if has_argument(&arguments, QUIT_ARGUMENT) {
                commands::request_quit(app);
            } else {
                commands::show_window(app);
            }
        }))
        .setup(move |app| {
            // Nothing runs, so there is nothing to quit.
            if has_argument(&arguments, QUIT_ARGUMENT) {
                app.handle().exit(0);
                return Ok(());
            }
            app.manage(AppState::start());
            app.manage(EditState::default());
            commands::forward_captures(app.handle());
            add_tray(app)?;
            if !has_argument(&arguments, HIDDEN_ARGUMENT) {
                commands::show_window(app.handle());
            }
            Ok(())
        })
        .on_window_event(|window, event| {
            let WindowEvent::CloseRequested { api, .. } = event else {
                return;
            };
            let Some(state) = window.try_state::<AppState>() else {
                return;
            };
            if state.close_to_tray() {
                // The program stays in the tray and keeps its hotkeys.
                api.prevent_close();
                let _ = window.hide();
            } else if state.hold_window() {
                // A recording that runs must close its file first. The
                // program exits when the file is saved.
                api.prevent_close();
                commands::request_quit(window.app_handle());
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
            commands::edit_screenshot,
            commands::monitors,
            commands::monitor_preview,
            commands::start_recording,
            commands::stop_recording,
            commands::set_paused,
            commands::files,
            commands::picture,
            commands::reveal,
            commands::open_file,
            commands::open_folder,
            commands::copy_picture,
            commands::quit,
            commands::close_window,
            commands::minimize,
            commands::toggle_maximize,
            commands::is_maximized,
            editing::save_clip,
            editing::clip_waveform,
            editing::play_clip,
            editing::stop_clip,
            editing::trim_audio_clip,
            editing::open_video_editor,
            editing::close_video_editor,
            editing::editor_frame,
            editing::editor_waveform,
            editing::export_video,
        ])
        .run(tauri::generate_context!())
        .expect("the desktop program could not start");
}
