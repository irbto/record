//! The commands that the interface calls and the events it receives.
//!
//! Every command runs away from the window thread, so a slow disk or a slow
//! registry query never freezes the window.

use std::{
    fs,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, MutexGuard,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    thread,
    time::{Duration, Instant, SystemTime},
};

use record_core::{
    glance::glance,
    screenshot::{
        CaptureMode, MonitorChoice, copy_file_to_clipboard, edit_saved_image,
        events::{CaptureHub, HubEvent, HubRequest, HubSettings},
        format::{ImageFormat, is_image_file, jpeg, viewable_bytes},
        history::list_history,
        hotkeys::{Chord, HotkeyId, conflict, hotkey_states},
        hub::Hub,
        saved::{
            SavedSettings, autostart_enabled, parse_countdown, parse_sdr_white, parse_seconds,
            set_autostart_line,
        },
        upload::{
            PRESETS, Preset, import_sxcu, list_destinations, read_setting, set_default_destination,
            write_setting,
        },
    },
    shell,
    video::enumerate_monitors,
};
use tauri::{AppHandle, Emitter, Manager, State, WebviewWindow, ipc::Response};

use crate::{
    editing::EditState,
    engines::default_folder,
    files::{FILE_LIMIT, file_entry, recent_files, session_within, within},
    payload::{Destination, FileEntry, HotkeyInfo, Mode, Monitor, ShotUpdate, Snapshot, file_name},
    recording::{self, Recordings},
    tray::Activity,
};

/// Event that carries the updates of an audio recording.
pub(crate) const SOUND_EVENT: &str = "sound";
/// Event that carries the updates of a video recording.
pub(crate) const VIDEO_EVENT: &str = "video";
/// Event that carries the reports of the screenshot engine.
const SHOTS_EVENT: &str = "shots";
/// Event that carries the state of every global hotkey.
const HOTKEYS_EVENT: &str = "hotkeys";
/// Event that tells the interface that the window waits for a file.
const CLOSING_EVENT: &str = "closing";
/// Label of the only window.
const MAIN_WINDOW: &str = "main";
/// Time that Windows needs to move a minimized window off the screen.
pub(crate) const MINIMIZE_TIME: Duration = Duration::from_millis(300);
/// Longest wait for the capture thread to register changed hotkeys.
const HOTKEY_WAIT: Duration = Duration::from_millis(500);
/// Settings file that holds the JSON text of the interface.
const UI_SETTING: &str = "desktop-ui.json";
/// Largest size of the JSON text of the interface in bytes.
const UI_SETTING_LIMIT: usize = 64 * 1_024;
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
    pub(crate) settings: Arc<HubSettings>,
    /// The capture thread, or the reason it did not start.
    hub: Result<Hub, String>,
    /// The recordings that run now.
    pub(crate) running: Mutex<Recordings>,
    /// The program exits when every running recording has saved its file.
    pub(crate) closing: AtomicBool,
    /// The region selector is open.
    pub(crate) selecting: AtomicBool,
    /// The close button hides the window and the program stays in the tray.
    close_to_tray: AtomicBool,
    /// Reports whether the tray starts at sign-in.
    autostart: AtomicBool,
    /// Registration result of each global hotkey.
    hotkeys: Mutex<[bool; 5]>,
    /// Number of hotkey reports from the capture thread so far.
    hotkey_reports: AtomicU64,
    /// Reports whether auto capture runs now.
    auto_active: AtomicBool,
    /// The window moved out of the way of a capture and must come back.
    restore: AtomicBool,
}

/// Locks a mutex and keeps its value after a panic on another thread.
pub(crate) fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

impl AppState {
    /// Loads the stored choices and starts the screenshot engine.
    #[must_use]
    pub fn start() -> Self {
        let settings = Arc::new(HubSettings::from_saved(&SavedSettings::load()));
        let hub =
            Hub::start_with_video(Arc::clone(&settings)).map_err(|error| format!("{error:#}"));
        Self {
            settings,
            hub,
            running: Mutex::new(Recordings::default()),
            closing: AtomicBool::new(false),
            selecting: AtomicBool::new(false),
            close_to_tray: AtomicBool::new(
                read_setting(TRAY_SETTING).is_none_or(|text| text.trim() != "false"),
            ),
            autostart: AtomicBool::new(autostart_enabled()),
            hotkeys: Mutex::new([false; 5]),
            hotkey_reports: AtomicU64::new(0),
            auto_active: AtomicBool::new(false),
            restore: AtomicBool::new(false),
        }
    }

    /// Returns the folders whose files the interface may open. The flag
    /// marks the folder whose files may lie one folder deeper.
    pub(crate) fn folders(&self) -> [(PathBuf, bool); 3] {
        [
            (self.settings.options().directory, false),
            (default_folder(Mode::Audio), true),
            (default_folder(Mode::Video), false),
        ]
    }

    /// Returns the pause switch of the running audio recording.
    ///
    /// The clip preview holds the audio recording with it, because the
    /// recording would capture the clip again.
    #[must_use]
    pub fn pause_switch(&self) -> Option<Arc<AtomicBool>> {
        lock(&self.running).audio_pause()
    }

    /// Reports which modes record now.
    #[must_use]
    pub fn activity(&self) -> Activity {
        lock(&self.running).activity()
    }

    /// Collects the state of every global hotkey for the interface.
    fn hotkey_infos(&self) -> Vec<HotkeyInfo> {
        hotkey_states(&self.settings.hotkeys(), *lock(&self.hotkeys))
            .into_iter()
            .map(|state| HotkeyInfo {
                id: state.id.name().to_owned(),
                label: state.id.label().to_owned(),
                keys: state.keys,
                enabled: state.enabled,
                registered: state.registered,
            })
            .collect()
    }

    /// Stores the hotkeys and asks the capture thread to register them.
    ///
    /// The call waits a moment for the result, so the returned state already
    /// tells which hotkeys Windows gave. A `hotkeys` event follows in any
    /// case.
    fn apply_hotkeys(&self) -> Result<Snapshot, String> {
        self.settings.store()?;
        if let Ok(hub) = &self.hub {
            let before = self.hotkey_reports.load(Ordering::Relaxed);
            hub.request(HubRequest::ReloadHotkeys);
            let started = Instant::now();
            while self.hotkey_reports.load(Ordering::Relaxed) == before
                && started.elapsed() < HOTKEY_WAIT
            {
                thread::sleep(Duration::from_millis(10));
            }
        }
        Ok(self.snapshot())
    }

    /// Returns the folder that holds the files of one mode.
    fn folder(&self, mode: Mode) -> PathBuf {
        match mode {
            Mode::Screenshot => self.settings.options().directory,
            Mode::Audio | Mode::Video => default_folder(mode),
        }
    }

    /// Checks a path from the interface against the listed folders.
    pub fn listed(&self, path: &str) -> Result<PathBuf, String> {
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
            hotkeys: self.hotkey_infos(),
            countdown_seconds: self.settings.countdown_seconds.load(Ordering::Relaxed),
            running: lock(&self.running).modes(),
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

    /// Asks every running recording to save its file before the program
    /// exits.
    ///
    /// Returns `false` when nothing records, so the program can exit at once.
    /// Otherwise the program exits when the last recording has ended.
    pub fn hold_window(&self) -> bool {
        let running = lock(&self.running);
        let held = running.stop_all();
        if held {
            self.closing.store(true, Ordering::Relaxed);
        }
        held
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
                    // The report of every hotkey follows this one.
                    HubEvent::Ready(_) => continue,
                    HubEvent::Hotkeys(states) => {
                        let mut registered = [false; 5];
                        for hotkey in &states {
                            registered[hotkey.id.index()] = hotkey.registered;
                        }
                        *lock(&state.hotkeys) = registered;
                        state.hotkey_reports.fetch_add(1, Ordering::Relaxed);
                        let _ = app.emit(HOTKEYS_EVENT, state.hotkey_infos());
                        continue;
                    }
                    HubEvent::VideoHotkey(id) => {
                        recording::video_hotkey(&app, id);
                        continue;
                    }
                    HubEvent::Capturing => ShotUpdate::Capturing,
                    HubEvent::Finished(summary) => ShotUpdate::Finished {
                        file: summary.output.as_deref().map(file_name),
                        path: summary
                            .output
                            .as_deref()
                            .map(|path| path.display().to_string()),
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
        "countdownSeconds" => settings
            .countdown_seconds
            .store(parse_countdown(value)?, Ordering::Relaxed),
        "format" => {
            lock(&settings.options).format = ImageFormat::parse(value)
                .ok_or_else(|| format!("{value} is not an image format"))?;
        }
        _ => return Err(format!("{name} is not a setting")),
    }
    state.stored()
}

/// Finds a global hotkey by the name that the interface uses.
fn hotkey_id(id: &str) -> Result<HotkeyId, String> {
    HotkeyId::parse(id).ok_or_else(|| format!("{id} is not a hotkey"))
}

/// Refuses a chord that another enabled hotkey already uses.
fn check_conflict(state: &AppState, id: HotkeyId, chord: Chord) -> Result<(), String> {
    match conflict(&state.settings.hotkeys(), id, chord) {
        Some(other) => Err(format!(
            "{} already uses {}",
            other.label(),
            chord.to_text()
        )),
        None => Ok(()),
    }
}

/// Changes the keys of one global hotkey from text such as `Ctrl+Shift+F8`.
#[tauri::command(async)]
pub fn set_hotkey(state: State<'_, AppState>, id: &str, keys: &str) -> Result<Snapshot, String> {
    let id = hotkey_id(id)?;
    let chord = Chord::parse(keys)?;
    if state.settings.hotkeys()[id.index()].enabled {
        check_conflict(&state, id, chord)?;
    }
    lock(&state.settings.hotkeys)[id.index()].chord = chord;
    state.apply_hotkeys()
}

/// Turns one global hotkey on or off.
#[tauri::command(async)]
pub fn set_hotkey_enabled(
    state: State<'_, AppState>,
    id: &str,
    enabled: bool,
) -> Result<Snapshot, String> {
    let id = hotkey_id(id)?;
    if enabled {
        check_conflict(&state, id, state.settings.hotkeys()[id.index()].chord)?;
    }
    lock(&state.settings.hotkeys)[id.index()].enabled = enabled;
    state.apply_hotkeys()
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

/// Opens one saved screenshot in the annotation overlay.
///
/// The window minimizes first and returns when the overlay closes. The
/// result arrives as a `shots` event, like a capture.
#[tauri::command(async)]
pub fn edit_screenshot(
    app: AppHandle,
    window: WebviewWindow,
    state: State<'_, AppState>,
    path: &str,
) -> Result<(), String> {
    let file = state.listed(path)?;
    if !is_image_file(&file) {
        return Err(format!("{} is not a screenshot", file_name(&file)));
    }
    let monitor = state.settings.options().monitor;
    let copy = state.settings.copy.load(Ordering::Relaxed);
    let minimized = window.minimize().is_ok();
    // The overlay owns a window, so it needs a thread of its own.
    thread::Builder::new()
        .name("screenshot-edit".to_owned())
        .spawn(move || {
            let _ = app.emit(SHOTS_EVENT, ShotUpdate::Capturing);
            if minimized {
                thread::sleep(MINIMIZE_TIME);
            }
            let update = match edit_saved_image(&file, monitor, copy) {
                Ok(Some(summary)) => ShotUpdate::Finished {
                    file: summary.output.as_deref().map(file_name),
                    path: summary
                        .output
                        .as_deref()
                        .map(|path| path.display().to_string()),
                    width: summary.width,
                    height: summary.height,
                    url: None,
                    copied: summary.copied,
                    hdr_nits: None,
                    error: summary
                        .clipboard_error
                        .map(|error| format!("Clipboard failed: {error}")),
                },
                Ok(None) => ShotUpdate::Cancelled,
                Err(error) => ShotUpdate::Failed {
                    error: format!("{error:#}"),
                },
            };
            if minimized && let Some(window) = app.get_webview_window(MAIN_WINDOW) {
                let _ = window.unminimize();
                let _ = window.set_focus();
            }
            let _ = app.emit(SHOTS_EVENT, update);
        })
        .map(drop)
        .map_err(|error| format!("could not start the editor: {error}"))
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

/// Lists the saved files of one mode, newest first.
#[tauri::command(async)]
pub fn files(state: State<'_, AppState>, mode: Mode) -> Vec<FileEntry> {
    let now = SystemTime::now();
    if mode != Mode::Screenshot {
        // The parts and clips of an audio session lie in their own folder.
        return recent_files(
            &state.folder(mode),
            mode.extension(),
            mode == Mode::Audio,
            now,
        );
    }
    // Screenshots come in several formats.
    list_history(&state.folder(mode))
        .into_iter()
        .take(FILE_LIMIT)
        .map(|entry| {
            let size = fs::metadata(&entry.path).map_or(0, |metadata| metadata.len());
            file_entry(entry.name, &entry.path, entry.modified, size, now)
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

/// Shows one saved file in the file explorer.
///
/// The path may also be the folder of an audio session.
#[tauri::command(async)]
pub fn reveal(state: State<'_, AppState>, path: &str) -> Result<(), String> {
    let target = state.listed(path).or_else(|refusal| {
        session_within(&state.folders(), Path::new(path)).map_err(|_| refusal)
    })?;
    shell::reveal(&target).map_err(|error| format!("{error:#}"))
}

/// Opens one saved file with the program that Windows uses for it.
#[tauri::command(async)]
pub fn open_file(state: State<'_, AppState>, path: &str) -> Result<(), String> {
    let file = state.listed(path)?;
    shell::open(&file).map_err(|error| format!("{error:#}"))
}

/// Opens the folder that holds the files of one mode.
#[tauri::command(async)]
pub fn open_folder(state: State<'_, AppState>, mode: Mode) -> Result<(), String> {
    let folder = state.folder(mode);
    fs::create_dir_all(&folder)
        .map_err(|error| format!("could not create {}: {error}", folder.display()))?;
    shell::open(&folder).map_err(|error| format!("{error:#}"))
}

/// Moves one saved file to the Recycle Bin.
#[tauri::command(async)]
pub fn delete_file(
    state: State<'_, AppState>,
    edits: State<'_, EditState>,
    path: &str,
) -> Result<(), String> {
    let file = state.listed(path)?;
    shell::recycle(&file).map_err(|error| format!("{error:#}"))?;
    edits.forget_video(&file);
    Ok(())
}

/// Returns a small PNG picture of one saved file, at most `edge` pixels in
/// each direction.
///
/// Windows gives the picture that the file explorer shows for the file.
#[tauri::command(async)]
pub fn thumbnail(state: State<'_, AppState>, path: &str, edge: u32) -> Result<Response, String> {
    let file = state.listed(path)?;
    shell::thumbnail_png(&file, edge)
        .map(Response::new)
        .map_err(|error| format!("{error:#}"))
}

/// Returns the JSON text that the interface stored, or `{}`.
#[tauri::command(async)]
pub fn ui_settings() -> String {
    read_setting(UI_SETTING)
        .filter(|text| !text.trim().is_empty())
        .unwrap_or_else(|| "{}".to_owned())
}

/// Stores the JSON text of the interface. The program does not read it.
#[tauri::command(async)]
pub fn set_ui_settings(json: &str) -> Result<(), String> {
    if json.len() > UI_SETTING_LIMIT {
        return Err(format!(
            "The interface settings are larger than {} KB",
            UI_SETTING_LIMIT / 1_024
        ));
    }
    write_setting(UI_SETTING, json)
}

/// Places one saved screenshot on the clipboard.
#[tauri::command(async)]
pub fn copy_picture(state: State<'_, AppState>, path: &str) -> Result<(), String> {
    let file = state.listed(path)?;
    copy_file_to_clipboard(&file).map_err(|error| format!("{error:#}"))
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
