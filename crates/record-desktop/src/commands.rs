//! The commands that the interface calls and the events it receives.
//!
//! Every command runs away from the window thread, so a slow disk or a slow
//! registry query never freezes the window.

use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    sync::{
        Arc, Mutex, MutexGuard,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, SystemTime},
};

use record_core::{
    glance::glance,
    screenshot::{
        CaptureMode, MonitorChoice, copy_file_to_clipboard,
        events::{CaptureHub, HubEvent, HubRequest, HubSettings},
        format::{ImageFormat, is_image_file, jpeg, viewable_bytes},
        history::list_history,
        hub::Hub,
        saved::{
            SavedSettings, autostart_enabled, parse_sdr_white, parse_seconds, set_autostart_line,
        },
        upload::{
            PRESETS, Preset, import_sxcu, list_destinations, read_setting, set_default_destination,
            write_setting,
        },
    },
    video::{enumerate_monitors, video_timestamp},
};
use tauri::{AppHandle, Emitter, Manager, State, WebviewWindow, ipc::Response};

use crate::{
    engines::{Control, Report, default_folder, start_sound, start_video, stopping},
    files::{FILE_LIMIT, recent_files, within},
    payload::{
        Destination, FileEntry, Mode, Monitor, ShotUpdate, Snapshot, SoundUpdate, VideoUpdate,
        file_name,
    },
};

/// Event that carries the updates of an audio recording.
const SOUND_EVENT: &str = "sound";
/// Event that carries the updates of a video recording.
const VIDEO_EVENT: &str = "video";
/// Event that carries the reports of the screenshot engine.
const SHOTS_EVENT: &str = "shots";
/// Event that tells the interface that the window waits for a file.
const CLOSING_EVENT: &str = "closing";
/// Frames per second of a video recording without an explicit choice.
const DEFAULT_FPS: u32 = 60;
/// Label of the only window.
const MAIN_WINDOW: &str = "main";
/// Time that Windows needs to move a minimized window off the screen.
const MINIMIZE_TIME: Duration = Duration::from_millis(300);
/// Longest edge of the live monitor preview in pixels.
const PREVIEW_EDGE: u32 = 1_280;
/// JPEG quality of the live monitor preview.
const PREVIEW_QUALITY: u8 = 80;
/// Settings file that holds the close-to-tray switch.
const TRAY_SETTING: &str = "desktop-close-to-tray.txt";
/// Argument that starts the program without showing its window.
pub const HIDDEN_ARGUMENT: &str = "--hidden";
/// Argument that asks the running program to save its work and exit.
pub const QUIT_ARGUMENT: &str = "--quit";

/// Everything the commands share.
pub struct AppState {
    /// Screenshot choices, shared with the capture thread.
    settings: Arc<HubSettings>,
    /// The capture thread, or the reason it did not start.
    hub: Result<Hub, String>,
    /// The recording that runs now.
    running: Mutex<Option<(Mode, Control)>>,
    /// The program exits when the running recording has saved its file.
    closing: AtomicBool,
    /// The close button hides the window and the program stays in the tray.
    close_to_tray: AtomicBool,
    /// Reports whether the tray starts at sign-in.
    autostart: AtomicBool,
    /// Registration result of each global hotkey.
    hotkeys: Mutex<[bool; 3]>,
    /// Reports whether auto capture runs now.
    auto_active: AtomicBool,
    /// The window moved out of the way of a capture and must come back.
    restore: AtomicBool,
}

/// Locks a mutex and keeps its value after a panic on another thread.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

impl AppState {
    /// Loads the stored choices and starts the screenshot engine.
    #[must_use]
    pub fn start() -> Self {
        let settings = Arc::new(HubSettings::from_saved(&SavedSettings::load()));
        let hub = Hub::start(Arc::clone(&settings)).map_err(|error| format!("{error:#}"));
        Self {
            settings,
            hub,
            running: Mutex::new(None),
            closing: AtomicBool::new(false),
            close_to_tray: AtomicBool::new(
                read_setting(TRAY_SETTING).is_none_or(|text| text.trim() != "false"),
            ),
            autostart: AtomicBool::new(autostart_enabled()),
            hotkeys: Mutex::new([false; 3]),
            auto_active: AtomicBool::new(false),
            restore: AtomicBool::new(false),
        }
    }

    /// Returns the folders whose files the interface may open.
    fn folders(&self) -> [PathBuf; 3] {
        [
            self.settings.options().directory,
            default_folder(Mode::Audio),
            default_folder(Mode::Video),
        ]
    }

    /// Returns the folder that holds the files of one mode.
    fn folder(&self, mode: Mode) -> PathBuf {
        match mode {
            Mode::Screenshot => self.settings.options().directory,
            Mode::Audio | Mode::Video => default_folder(mode),
        }
    }

    /// Checks a path from the interface against the listed folders.
    fn listed(&self, path: &str) -> Result<PathBuf, String> {
        within(&self.folders(), Path::new(path))
    }

    /// Stores the screenshot choices and returns the new state.
    fn stored(&self) -> Result<Snapshot, String> {
        self.settings.store()?;
        Ok(self.snapshot())
    }

    /// Collects every screenshot choice for the interface.
    fn snapshot(&self) -> Snapshot {
        let options = self.settings.options();
        let mut destinations: Vec<Destination> = PRESETS
            .iter()
            .map(|preset| {
                let ready = preset.ready();
                Destination {
                    id: preset.id.to_owned(),
                    name: preset.name.to_owned(),
                    note: if ready && preset.key_label.is_some() {
                        "ready".to_owned()
                    } else {
                        preset.note.to_owned()
                    },
                    key_label: preset.key_label.filter(|_| !ready).map(str::to_owned),
                    key_source: preset.key_source.to_owned(),
                }
            })
            .collect();
        destinations.extend(list_destinations().into_iter().map(|name| Destination {
            id: name.clone(),
            name,
            note: "custom uploader".to_owned(),
            key_label: None,
            key_source: String::new(),
        }));
        Snapshot {
            copy: self.settings.copy.load(Ordering::Relaxed),
            save: self.settings.save.load(Ordering::Relaxed),
            upload: self.settings.upload.load(Ordering::Relaxed),
            destination: self
                .settings
                .active_uploader()
                .map(|uploader| uploader.name),
            destinations,
            folder: options.directory.display().to_string(),
            format: options.format.name().to_owned(),
            formats: ImageFormat::ALL
                .iter()
                .map(|format| format.name().to_owned())
                .collect(),
            close_to_tray: self.close_to_tray(),
            audio_folder: default_folder(Mode::Audio).display().to_string(),
            video_folder: default_folder(Mode::Video).display().to_string(),
            delay_seconds: options.delay.as_secs_f32(),
            interval_seconds: options.auto_interval.as_secs_f32(),
            primary_monitor: options.monitor == MonitorChoice::Primary,
            sdr_white_nits: options.sdr_white_nits,
            autostart: self.autostart.load(Ordering::Relaxed),
            hotkeys: *lock(&self.hotkeys),
            auto_active: self.auto_active.load(Ordering::Relaxed),
            engine_error: self.hub.as_ref().err().cloned(),
            version: env!("CARGO_PKG_VERSION").to_owned(),
        }
    }

    /// Reports whether the close button hides the window to the tray.
    #[must_use]
    pub fn close_to_tray(&self) -> bool {
        self.close_to_tray.load(Ordering::Relaxed)
    }

    /// Asks the running recording to save its file before the program exits.
    ///
    /// Returns `false` when nothing records, so the program can exit at once.
    pub fn hold_window(&self) -> bool {
        match &*lock(&self.running) {
            Some((_, control)) => {
                self.closing.store(true, Ordering::Relaxed);
                control.stop.store(true, Ordering::Relaxed);
                true
            }
            None => false,
        }
    }
}

/// Forwards the reports of the screenshot engine to the interface.
pub fn forward_captures(app: &AppHandle) {
    let state = app.state::<AppState>();
    let Ok(hub) = &state.hub else {
        return;
    };
    let events = hub.events().clone();
    let app = app.clone();
    let spawned = thread::Builder::new()
        .name("captures".to_owned())
        .spawn(move || {
            for event in events {
                let state = app.state::<AppState>();
                let update = match event {
                    HubEvent::Ready(hotkeys) => {
                        *lock(&state.hotkeys) = hotkeys;
                        ShotUpdate::Ready { hotkeys }
                    }
                    HubEvent::Capturing => ShotUpdate::Capturing,
                    HubEvent::Finished(summary) => ShotUpdate::Finished {
                        file: summary.output.as_deref().map(file_name),
                        width: summary.width,
                        height: summary.height,
                        url: summary.uploaded.map(|uploaded| uploaded.url),
                        copied: summary.copied,
                        hdr_nits: summary.hdr.then_some(summary.sdr_white_nits),
                        error: summary
                            .upload_error
                            .map(|error| format!("Upload failed: {error}"))
                            .or(summary
                                .clipboard_error
                                .map(|error| format!("Clipboard failed: {error}"))),
                    },
                    HubEvent::Cancelled => ShotUpdate::Cancelled,
                    HubEvent::Failed(error) => ShotUpdate::Failed { error },
                    HubEvent::Auto(active) => {
                        state.auto_active.store(active, Ordering::Relaxed);
                        ShotUpdate::Auto { active }
                    }
                    // Only the tray menu sends this, and this program has none.
                    HubEvent::QuitRequested => continue,
                };
                let ended = matches!(
                    update,
                    ShotUpdate::Finished { .. } | ShotUpdate::Cancelled | ShotUpdate::Failed { .. }
                );
                if ended
                    && state.restore.swap(false, Ordering::Relaxed)
                    && let Some(window) = app.get_webview_window(MAIN_WINDOW)
                {
                    let _ = window.unminimize();
                    let _ = window.set_focus();
                }
                let _ = app.emit(SHOTS_EVENT, update);
            }
        });
    // Without this thread the captures still work. Only their reports are lost.
    drop(spawned);
}

/// Clears the running recording and closes the window that waited for it.
fn recording_ended(app: &AppHandle) {
    let state = app.state::<AppState>();
    *lock(&state.running) = None;
    if state.closing.load(Ordering::Relaxed) {
        // Give the last event time to reach the interface.
        thread::sleep(Duration::from_millis(50));
        app.exit(0);
    }
}

/// Returns every screenshot choice and setting.
#[tauri::command(async)]
pub fn snapshot(state: State<'_, AppState>) -> Snapshot {
    state.snapshot()
}

/// Turns one switch on or off.
#[tauri::command(async)]
pub fn set_flag(state: State<'_, AppState>, name: &str, value: bool) -> Result<Snapshot, String> {
    let settings = &state.settings;
    match name {
        "copy" | "save" => {
            let (changed, other) = if name == "copy" {
                (&settings.copy, &settings.save)
            } else {
                (&settings.save, &settings.copy)
            };
            changed.store(value, Ordering::Relaxed);
            // An image always goes somewhere.
            if !value {
                other.store(true, Ordering::Relaxed);
            }
        }
        "upload" => {
            if value && settings.active_uploader().is_none() {
                return Err("Choose an upload destination first".to_owned());
            }
            settings.upload.store(value, Ordering::Relaxed);
        }
        "primaryMonitor" => {
            lock(&settings.options).monitor = if value {
                MonitorChoice::Primary
            } else {
                MonitorChoice::Pointer
            };
        }
        "autostart" => {
            // The sign-in entry starts this program in the tray.
            let line = if value {
                let program = std::env::current_exe()
                    .map_err(|error| format!("could not find this program: {error}"))?;
                Some(format!("\"{}\" {HIDDEN_ARGUMENT}", program.display()))
            } else {
                None
            };
            set_autostart_line(line.as_deref())?;
            state.autostart.store(value, Ordering::Relaxed);
        }
        "closeToTray" => {
            write_setting(TRAY_SETTING, if value { "true" } else { "false" })?;
            state.close_to_tray.store(value, Ordering::Relaxed);
        }
        _ => return Err(format!("{name} is not a switch")),
    }
    state.stored()
}

/// Changes one setting from its text form.
#[tauri::command(async)]
pub fn set_setting(
    state: State<'_, AppState>,
    name: &str,
    value: &str,
) -> Result<Snapshot, String> {
    let value = value.trim();
    let settings = &state.settings;
    match name {
        "folder" => {
            if value.is_empty() {
                return Err("Enter a folder path".to_owned());
            }
            fs::create_dir_all(value).map_err(|error| format!("could not use {value}: {error}"))?;
            lock(&settings.options).directory = PathBuf::from(value);
        }
        "delaySeconds" => {
            lock(&settings.options).delay = Duration::from_secs_f32(parse_seconds(value)?);
        }
        "intervalSeconds" => {
            lock(&settings.options).auto_interval = Duration::from_secs_f32(parse_seconds(value)?);
        }
        "sdrWhiteNits" => lock(&settings.options).sdr_white_nits = parse_sdr_white(value)?,
        "format" => {
            lock(&settings.options).format = ImageFormat::parse(value)
                .ok_or_else(|| format!("{value} is not an image format"))?;
        }
        _ => return Err(format!("{name} is not a setting")),
    }
    state.stored()
}

/// Makes a destination active and stores it. No destination turns uploads off.
#[tauri::command(async)]
pub fn select_destination(
    state: State<'_, AppState>,
    id: Option<&str>,
) -> Result<Snapshot, String> {
    match id {
        Some(id) => {
            state.settings.select_destination(id)?;
            set_default_destination(Some(id))?;
        }
        None => state.settings.upload.store(false, Ordering::Relaxed),
    }
    state.stored()
}

/// Stores the key of a built-in destination and makes it active.
#[tauri::command(async)]
pub fn save_key(state: State<'_, AppState>, id: &str, key: &str) -> Result<Snapshot, String> {
    let preset = Preset::find(id).ok_or_else(|| format!("{id} is not a built-in destination"))?;
    preset.save_key(key)?;
    select_destination(state, Some(preset.id))
}

/// Copies a `.sxcu` file into the uploaders folder and makes it active.
#[tauri::command(async)]
pub fn import_uploader(state: State<'_, AppState>, path: &str) -> Result<Snapshot, String> {
    let path = path.trim().trim_matches('"');
    if path.is_empty() {
        return Err("Enter the path of a .sxcu file".to_owned());
    }
    let name = import_sxcu(Path::new(path))?;
    select_destination(state, Some(&name))
}

/// Starts one screenshot. A delayed one waits for the stored delay first.
///
/// The window minimizes first, so it is not in its own picture and the
/// window below it is the active one. It returns when the capture ends.
#[tauri::command(async)]
pub fn capture(
    window: WebviewWindow,
    state: State<'_, AppState>,
    mode: &str,
    delayed: bool,
) -> Result<(), String> {
    let hub = state.hub.as_ref().map_err(Clone::clone)?;
    let mode = match mode {
        "region" => CaptureMode::Region,
        "fullscreen" => CaptureMode::Fullscreen,
        "window" => CaptureMode::Window,
        "monitor" => CaptureMode::Monitor,
        "last" => CaptureMode::LastRegion,
        _ => return Err(format!("{mode} is not a capture mode")),
    };
    if window.minimize().is_ok() {
        state.restore.store(true, Ordering::Relaxed);
        thread::sleep(MINIMIZE_TIME);
    }
    hub.request(if delayed {
        HubRequest::Delayed(mode)
    } else {
        HubRequest::Capture(mode)
    });
    Ok(())
}

/// Starts or stops auto capture.
#[tauri::command(async)]
pub fn toggle_auto(state: State<'_, AppState>) -> Result<(), String> {
    state
        .hub
        .as_ref()
        .map_err(Clone::clone)?
        .request(HubRequest::ToggleAuto);
    Ok(())
}

/// Lists the attached monitors for the video source list.
#[tauri::command(async)]
pub fn monitors() -> Result<Vec<Monitor>, String> {
    let monitors = enumerate_monitors().map_err(|error| format!("{error:#}"))?;
    Ok(monitors
        .into_iter()
        .map(|monitor| Monitor {
            index: monitor.index,
            width: monitor.width,
            height: monitor.height,
            primary: monitor.primary,
            left: monitor.left,
            top: monitor.top,
        })
        .collect())
}

/// Returns a small JPEG picture of one area of the desktop.
///
/// The interface asks for it a few times each second while the screen
/// capture screen is open. It shows the monitor before, during, and after a
/// recording, also when nothing on the monitor moves.
#[tauri::command(async)]
pub fn monitor_preview(left: i32, top: i32, width: u32, height: u32) -> Result<Response, String> {
    let picture =
        glance(left, top, width, height, PREVIEW_EDGE).map_err(|error| format!("{error:#}"))?;
    jpeg(&picture.rgb, picture.width, picture.height, PREVIEW_QUALITY)
        .map(Response::new)
        .map_err(|error| format!("{error:#}"))
}

/// Starts an audio or a video recording and returns the path of its file.
///
/// One recording runs at a time, because both modes capture the same audio
/// device.
#[tauri::command(async)]
pub fn start_recording(
    app: AppHandle,
    state: State<'_, AppState>,
    mode: Mode,
    monitor: Option<u32>,
    fps: Option<u32>,
) -> Result<String, String> {
    let mut running = lock(&state.running);
    if running.is_some() {
        return Err("A recording runs already. Save it first.".to_owned());
    }
    if mode == Mode::Screenshot {
        return Err("A screenshot is not a recording".to_owned());
    }
    let output = default_folder(mode).join(format!(
        "recording-{}.{}",
        video_timestamp(),
        mode.extension()
    ));
    let control = Control::default();
    *running = Some((mode, control.clone()));
    drop(running);
    let path = output.display().to_string();
    if mode == Mode::Audio {
        let report: Report<SoundUpdate> = Arc::new(move |update| {
            let ended = matches!(update, SoundUpdate::Stopped { .. });
            if ended {
                *lock(&app.state::<AppState>().running) = None;
            }
            let _ = app.emit(SOUND_EVENT, update);
            if ended {
                recording_ended(&app);
            }
        });
        start_sound(output, &control, report);
    } else {
        let report: Report<VideoUpdate> = Arc::new(move |update| {
            let ended = matches!(update, VideoUpdate::Stopped { .. });
            if ended {
                *lock(&app.state::<AppState>().running) = None;
            }
            let _ = app.emit(VIDEO_EVENT, update);
            if ended {
                recording_ended(&app);
            }
        });
        start_video(
            output,
            monitor.unwrap_or(0),
            fps.unwrap_or(DEFAULT_FPS).clamp(1, 240),
            &control,
            report,
        );
    }
    Ok(path)
}

/// Asks the running recording to save its file.
#[tauri::command(async)]
pub fn stop_recording(state: State<'_, AppState>) {
    if let Some((_, control)) = &*lock(&state.running) {
        control.stop.store(true, Ordering::Relaxed);
    }
}

/// Pauses or resumes the running recording. Returns the new pause state.
#[tauri::command(async)]
pub fn set_paused(state: State<'_, AppState>, paused: bool) -> bool {
    match &*lock(&state.running) {
        Some((_, control)) if !stopping(control) => {
            control.paused.store(paused, Ordering::Relaxed);
            paused
        }
        _ => false,
    }
}

/// Lists the saved files of one mode, newest first.
#[tauri::command(async)]
pub fn files(state: State<'_, AppState>, mode: Mode) -> Vec<FileEntry> {
    let now = SystemTime::now();
    if mode != Mode::Screenshot {
        return recent_files(&state.folder(mode), mode.extension(), now);
    }
    // Screenshots come in several formats.
    list_history(&state.folder(mode))
        .into_iter()
        .take(FILE_LIMIT)
        .map(|entry| FileEntry {
            name: entry.name,
            path: entry.path.display().to_string(),
            age_seconds: now
                .duration_since(entry.modified)
                .map_or(0, |age| age.as_secs()),
        })
        .collect()
}

/// Returns the bytes of one saved screenshot in a form the window can show.
#[tauri::command(async)]
pub fn picture(state: State<'_, AppState>, path: &str) -> Result<Response, String> {
    let file = state.listed(path)?;
    if !is_image_file(&file) {
        return Err(format!("{} is not a screenshot", file_name(&file)));
    }
    viewable_bytes(&file)
        .map(Response::new)
        .map_err(|error| format!("{error:#}"))
}

/// Starts a helper program and formats a failure.
fn launch(command: &mut Command) -> Result<(), String> {
    command
        .spawn()
        .map(drop)
        .map_err(|error| format!("could not start the file explorer: {error}"))
}

/// Shows one saved file in the file explorer.
#[tauri::command(async)]
pub fn reveal(state: State<'_, AppState>, path: &str) -> Result<(), String> {
    let file = state.listed(path)?;
    // The explorer does not accept the long path form of a canonical path.
    launch(
        Command::new("explorer")
            .arg("/select,")
            .arg(plain_path(&file)),
    )
}

/// Opens one saved file with the program that Windows uses for it.
#[tauri::command(async)]
pub fn open_file(state: State<'_, AppState>, path: &str) -> Result<(), String> {
    let file = state.listed(path)?;
    launch(Command::new("explorer").arg(plain_path(&file)))
}

/// Opens the folder that holds the files of one mode.
#[tauri::command(async)]
pub fn open_folder(state: State<'_, AppState>, mode: Mode) -> Result<(), String> {
    let folder = state.folder(mode);
    fs::create_dir_all(&folder)
        .map_err(|error| format!("could not create {}: {error}", folder.display()))?;
    launch(Command::new("explorer").arg(folder))
}

/// Places one saved screenshot on the clipboard.
#[tauri::command(async)]
pub fn copy_picture(state: State<'_, AppState>, path: &str) -> Result<(), String> {
    let file = state.listed(path)?;
    copy_file_to_clipboard(&file).map_err(|error| format!("{error:#}"))
}

/// Removes the `\\?\` prefix that a canonical Windows path carries.
fn plain_path(path: &Path) -> PathBuf {
    let text = path.display().to_string();
    match text.strip_prefix(r"\\?\") {
        Some(rest) if !rest.starts_with("UNC") => PathBuf::from(rest),
        _ => path.to_path_buf(),
    }
}

/// Shows the window and brings it to the front.
pub fn show_window(app: &AppHandle) {
    if let Some(window) = app.get_webview_window(MAIN_WINDOW) {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}

/// Ends the program. A running recording saves its file first.
pub fn request_quit(app: &AppHandle) {
    if app.state::<AppState>().hold_window() {
        let _ = app.emit(CLOSING_EVENT, ());
    } else {
        app.exit(0);
    }
}

/// Starts one screenshot from the tray menu. The window stays as it is.
pub fn capture_from_tray(app: &AppHandle, mode: CaptureMode) {
    if let Ok(hub) = &app.state::<AppState>().hub {
        hub.request(HubRequest::Capture(mode));
    }
}

/// Ends the program. The File menu calls this.
#[tauri::command(async)]
pub fn quit(app: AppHandle) {
    request_quit(&app);
}

/// Closes the window as the close button of a title bar does.
///
/// The window event of the program decides what a close means: it hides the
/// window to the tray, or it ends the program.
#[tauri::command(async)]
pub fn close_window(window: WebviewWindow) -> Result<(), String> {
    window
        .close()
        .map_err(|error| format!("could not close the window: {error}"))
}

/// Minimizes the window. The title bar of the interface calls this.
#[tauri::command(async)]
pub fn minimize(window: WebviewWindow) -> Result<(), String> {
    window
        .minimize()
        .map_err(|error| format!("could not minimize the window: {error}"))
}

/// Maximizes the window, or restores it. Returns the new state.
#[tauri::command(async)]
pub fn toggle_maximize(window: WebviewWindow) -> Result<bool, String> {
    let maximized = window.is_maximized().unwrap_or(false);
    let changed = if maximized {
        window.unmaximize()
    } else {
        window.maximize()
    };
    changed
        .map(|()| !maximized)
        .map_err(|error| format!("could not resize the window: {error}"))
}

/// Reports whether the window fills the screen.
#[tauri::command(async)]
pub fn is_maximized(window: WebviewWindow) -> bool {
    window.is_maximized().unwrap_or(false)
}
