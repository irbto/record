//! Starting, stopping, and discarding the recordings.
//!
//! Audio and video are independent. Each mode has one slot, so one audio
//! recording and one video recording can run at the same time. The interface
//! starts and stops recordings with commands. The global hotkeys and the
//! tray menu do the same without the interface, so every change goes to the
//! interface as an event.
//!
//! A start can wait for a countdown. The wait runs on its own thread with
//! the overlay of `record_core::countdown`. A stop during the wait cancels
//! the start.

use std::{
    fs, io,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::Duration,
};

use record_core::{
    countdown,
    region::{CaptureTarget, MonitorArea, plan_capture, pointer_position},
    screenshot::{
        DesktopRect, MonitorChoice, hotkeys::HotkeyId, saved::COUNTDOWN_LIMIT,
        select_region as select_on_screen,
    },
    session::create_session_directory,
    video::{CropRect, enumerate_monitors, video_timestamp},
};
use tauri::{AppHandle, Emitter, Manager, State, WebviewWindow};

use crate::{
    commands::{AppState, MINIMIZE_TIME, SOUND_EVENT, VIDEO_EVENT, lock},
    editing::{EditState, audio_saved},
    engines::{Control, Report, default_folder, start_sound, start_video, stopping},
    files::own_output,
    payload::{Mode, Region, SoundUpdate, VideoUpdate},
    tray::Activity,
};

/// Frames per second of a video recording without an explicit choice.
const DEFAULT_FPS: u32 = 60;
/// Time that the desktop needs to remove the region selector from the screen.
const PICKER_CLOSE_TIME: Duration = Duration::from_millis(80);
/// Time that a tray menu needs to close.
const MENU_CLOSE_TIME: Duration = Duration::from_millis(150);
/// Number of attempts to delete the output of a discarded recording.
const DELETE_ATTEMPTS: u32 = 5;
/// Wait between two attempts to delete an output.
const DELETE_WAIT: Duration = Duration::from_millis(120);

/// One recording that runs or waits for its countdown.
pub struct Active {
    /// The switches that stop and pause the recording.
    control: Control,
    /// Deletes the output when the recording ends.
    discard: Arc<AtomicBool>,
    /// The file of a video or the session folder of an audio recording.
    output: PathBuf,
}

/// The recording of each mode.
#[derive(Default)]
pub struct Recordings {
    /// The audio recording.
    audio: Option<Active>,
    /// The video recording.
    video: Option<Active>,
}

impl Recordings {
    /// Returns the slot of one mode. A screenshot has none.
    const fn slot(&mut self, mode: Mode) -> Option<&mut Option<Active>> {
        match mode {
            Mode::Audio => Some(&mut self.audio),
            Mode::Video => Some(&mut self.video),
            Mode::Screenshot => None,
        }
    }

    /// Returns the recording of one mode.
    const fn get(&self, mode: Mode) -> Option<&Active> {
        match mode {
            Mode::Audio => self.audio.as_ref(),
            Mode::Video => self.video.as_ref(),
            Mode::Screenshot => None,
        }
    }

    /// Reports which modes record now.
    #[must_use]
    pub const fn activity(&self) -> Activity {
        Activity {
            audio: self.audio.is_some(),
            video: self.video.is_some(),
        }
    }

    /// Lists the modes that record now.
    #[must_use]
    pub fn modes(&self) -> Vec<Mode> {
        [Mode::Audio, Mode::Video]
            .into_iter()
            .filter(|mode| self.get(*mode).is_some())
            .collect()
    }

    /// Returns the pause switch of the audio recording.
    #[must_use]
    pub fn audio_pause(&self) -> Option<Arc<AtomicBool>> {
        self.audio
            .as_ref()
            .map(|active| Arc::clone(&active.control.paused))
    }

    /// Asks every recording to save its file. Returns `false` when nothing
    /// records.
    pub fn stop_all(&self) -> bool {
        let mut any = false;
        for active in [&self.audio, &self.video].into_iter().flatten() {
            active.control.stop.store(true, Ordering::Relaxed);
            any = true;
        }
        any
    }
}

/// What one start captures, after the monitors were read.
#[derive(Clone, Copy, Debug)]
struct Shot {
    /// Index of the recorded monitor.
    monitor: u32,
    /// The recorded part in pixels of the monitor. The default is all of it.
    crop: CropRect,
    /// The recorded part in desktop pixels, for the interface.
    region: Option<Region>,
    /// The monitor that shows the countdown, when it is known.
    area: Option<DesktopRect>,
}

/// Reads the place of every attached monitor.
fn monitor_areas() -> Result<Vec<MonitorArea>, String> {
    let monitors = enumerate_monitors().map_err(|error| format!("{error:#}"))?;
    Ok(monitors
        .into_iter()
        .map(|monitor| MonitorArea {
            index: monitor.index,
            left: monitor.left,
            top: monitor.top,
            width: monitor.width,
            height: monitor.height,
        })
        .collect())
}

/// Resolves the target of a video recording.
///
/// A whole monitor by its index needs no look at the monitors, unless the
/// countdown must know where the monitor is.
fn video_shot(target: CaptureTarget, countdown: bool) -> Result<Shot, String> {
    let unknown = |monitor| Shot {
        monitor,
        crop: CropRect::default(),
        region: None,
        area: None,
    };
    let monitors = match (target, countdown) {
        (CaptureTarget::Monitor(index), false) => return Ok(unknown(index)),
        (CaptureTarget::Monitor(index), true) => match monitor_areas() {
            Ok(monitors) => monitors,
            // The recorder reports a missing monitor itself.
            Err(_) => return Ok(unknown(index)),
        },
        _ => monitor_areas()?,
    };
    let plan = plan_capture(&monitors, target)?;
    Ok(Shot {
        monitor: plan.monitor.index,
        crop: plan
            .region
            .map_or_else(CropRect::default, |region| CropRect {
                left: region.left,
                top: region.top,
                width: region.width,
                height: region.height,
            }),
        region: plan.region.map(|region| Region {
            left: region.desktop.left,
            top: region.desktop.top,
            width: region.desktop.width as i32,
            height: region.desktop.height as i32,
        }),
        area: Some(plan.monitor.rect()),
    })
}

/// Picks the monitor that shows the countdown of an audio recording: the
/// one under the pointer.
fn audio_shot(countdown: bool) -> Shot {
    let (x, y) = pointer_position();
    let area = countdown
        .then(monitor_areas)
        .and_then(Result::ok)
        .and_then(|monitors| plan_capture(&monitors, CaptureTarget::Point(x, y)).ok())
        .map(|plan| plan.monitor.rect());
    Shot {
        monitor: 0,
        crop: CropRect::default(),
        region: None,
        area,
    }
}

/// Formats the refusal of a second start of one mode.
fn busy(mode: Mode) -> String {
    let name = if mode == Mode::Audio {
        "An audio"
    } else {
        "A video"
    };
    format!("{name} recording runs already. Save it first.")
}

/// Sends the end of a recording to the interface.
fn emit_stopped(
    app: &AppHandle,
    mode: Mode,
    file: Option<String>,
    path: Option<String>,
    error: Option<String>,
    discarded: bool,
) {
    let _ = if mode == Mode::Audio {
        app.emit(
            SOUND_EVENT,
            SoundUpdate::Stopped {
                file,
                path,
                error,
                discarded,
            },
        )
    } else {
        app.emit(
            VIDEO_EVENT,
            VideoUpdate::Stopped {
                file,
                path,
                error,
                discarded,
            },
        )
    };
}

/// Deletes what a discarded recording wrote: the file of a video, or the
/// whole session folder of an audio recording.
fn remove_output(mode: Mode, output: &Path) -> Result<(), String> {
    if !own_output(&default_folder(mode), output) {
        return Err(format!(
            "{} is not a recording of this program",
            output.display()
        ));
    }
    let mut attempt = 1;
    loop {
        let removed = if mode == Mode::Audio {
            fs::remove_dir_all(output)
        } else {
            fs::remove_file(output)
        };
        match removed {
            Ok(()) => return Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            // A virus scanner can hold a new file for a moment.
            Err(_) if attempt < DELETE_ATTEMPTS => {
                attempt += 1;
                thread::sleep(DELETE_WAIT);
            }
            Err(error) => {
                return Err(format!("could not delete {}: {error}", output.display()));
            }
        }
    }
}

/// How one recording ended.
enum Ending {
    /// The engine ran and reported this result.
    Recorded {
        /// Name of the output, when the recording succeeded.
        file: Option<String>,
        /// Full path of the output, when the recording succeeded.
        path: Option<String>,
        /// The reason for a failure.
        error: Option<String>,
    },
    /// The start was cancelled before the engine ran.
    Cancelled,
}

/// Ends one recording: frees its slot, deletes a discarded output, tells
/// the interface, and exits the program when it waited for this.
fn finish(app: &AppHandle, mode: Mode, ending: Ending) {
    let state = app.state::<AppState>();
    let edits = app.state::<EditState>();
    let ended = lock(&state.running).slot(mode).and_then(Option::take);
    match (ending, ended) {
        (Ending::Cancelled, ended) => {
            // Only the empty session folder exists. The files of an earlier
            // session stay as they are.
            if let Some(active) = ended.filter(|_| mode == Mode::Audio)
                && own_output(&default_folder(mode), &active.output)
            {
                let _ = fs::remove_dir(&active.output);
            }
            emit_stopped(app, mode, None, None, None, true);
        }
        (Ending::Recorded { file, path, error }, ended) => {
            if mode == Mode::Audio {
                edits.end_audio();
            }
            let discard = ended.filter(|active| active.discard.load(Ordering::Relaxed));
            if let Some(active) = discard {
                if mode == Mode::Audio {
                    // The clips of the session hold their sources open.
                    edits.forget_audio();
                } else {
                    edits.forget_video(&active.output);
                }
                match remove_output(mode, &active.output) {
                    Ok(()) => emit_stopped(app, mode, None, None, None, true),
                    Err(error) => emit_stopped(app, mode, file, path, Some(error), false),
                }
            } else {
                emit_stopped(app, mode, file, path, error, false);
            }
        }
    }
    crate::refresh_tray(app);
    if state.closing.load(Ordering::Relaxed) && lock(&state.running).modes().is_empty() {
        // Give the last event time to reach the interface.
        thread::sleep(Duration::from_millis(50));
        app.exit(0);
    }
}

/// Everything the start thread of one recording needs.
struct Launch {
    /// The program.
    app: AppHandle,
    /// What to record.
    mode: Mode,
    /// The switches of the recording.
    control: Control,
    /// The file or the session folder.
    output: PathBuf,
    /// The monitor and the part of it.
    shot: Shot,
    /// Frames per second of a video.
    fps: u32,
    /// Seconds to wait before capture.
    countdown: u32,
}

impl Launch {
    /// Waits for the countdown and starts the engine.
    fn run(self) {
        let waited = if self.countdown > 0 {
            // Without a known monitor the wait runs with no overlay.
            let area = self.shot.area.unwrap_or(DesktopRect {
                left: 0,
                top: 0,
                width: 0,
                height: 0,
            });
            let (app, mode) = (self.app.clone(), self.mode);
            countdown::run(area, self.countdown, &self.control.stop, &mut |seconds| {
                let _ = if mode == Mode::Audio {
                    app.emit(SOUND_EVENT, SoundUpdate::Countdown { seconds })
                } else {
                    app.emit(VIDEO_EVENT, VideoUpdate::Countdown { seconds })
                };
            })
        } else {
            !stopping(&self.control)
        };
        if !waited {
            // A stop during the wait cancels the start.
            finish(&self.app, self.mode, Ending::Cancelled);
        } else if self.mode == Mode::Audio {
            self.sound();
        } else {
            self.video();
        }
    }

    /// Starts the audio engine.
    fn sound(self) {
        let app = self.app;
        let (clips, clip_requests) = crossbeam_channel::bounded(8);
        app.state::<EditState>().begin_audio(clips);
        let saved = {
            let app = app.clone();
            Arc::new(move |file| audio_saved(&app, file))
        };
        let report: Report<SoundUpdate> = Arc::new(move |update| match update {
            SoundUpdate::Stopped {
                file, path, error, ..
            } => finish(&app, Mode::Audio, Ending::Recorded { file, path, error }),
            update => {
                let _ = app.emit(SOUND_EVENT, update);
            }
        });
        start_sound(self.output, &self.control, clip_requests, saved, report);
    }

    /// Starts the video engine.
    fn video(self) {
        let app = self.app;
        let saved = {
            let app = app.clone();
            Arc::new(move |summary: &_| app.state::<EditState>().remember_video(summary))
        };
        let report: Report<VideoUpdate> = Arc::new(move |update| match update {
            VideoUpdate::Stopped {
                file, path, error, ..
            } => finish(&app, Mode::Video, Ending::Recorded { file, path, error }),
            update => {
                let _ = app.emit(VIDEO_EVENT, update);
            }
        });
        start_video(
            self.output,
            self.shot.monitor,
            self.shot.crop,
            self.fps,
            &self.control,
            saved,
            report,
        );
    }
}

/// Starts a recording and returns the path of its file or session folder.
///
/// The call returns before capture starts. The `starting` event comes
/// first, then the countdown when one is set, then `started`. A second start
/// of a mode that records fails.
pub fn begin(
    app: &AppHandle,
    mode: Mode,
    target: CaptureTarget,
    fps: Option<u32>,
) -> Result<String, String> {
    let state = app.state::<AppState>();
    if mode == Mode::Screenshot {
        return Err("A screenshot is not a recording".to_owned());
    }
    if lock(&state.running).get(mode).is_some() {
        return Err(busy(mode));
    }
    let countdown = state
        .settings
        .countdown_seconds
        .load(Ordering::Relaxed)
        .min(COUNTDOWN_LIMIT);
    let shot = if mode == Mode::Video {
        video_shot(target, countdown > 0)?
    } else {
        audio_shot(countdown > 0)
    };
    let folder = default_folder(mode);
    let stamp = video_timestamp();
    let control = Control::default();
    let output = {
        let mut running = lock(&state.running);
        let slot = running.slot(mode).ok_or_else(|| busy(mode))?;
        if slot.is_some() {
            return Err(busy(mode));
        }
        let output = if mode == Mode::Audio {
            // An audio recording is a folder of parts and named clips.
            create_session_directory(&folder, &stamp)
                .map_err(|error| format!("could not create the session folder: {error}"))?
        } else {
            folder.join(format!("recording-{stamp}.{}", mode.extension()))
        };
        *slot = Some(Active {
            control: control.clone(),
            discard: Arc::new(AtomicBool::new(false)),
            output: output.clone(),
        });
        output
    };
    let path = output.display().to_string();
    let _ = if mode == Mode::Audio {
        app.emit(
            SOUND_EVENT,
            SoundUpdate::Starting {
                path: path.clone(),
                countdown,
            },
        )
    } else {
        app.emit(
            VIDEO_EVENT,
            VideoUpdate::Starting {
                path: path.clone(),
                countdown,
                monitor: shot.monitor,
                region: shot.region,
            },
        )
    };
    crate::refresh_tray(app);
    let launch = Launch {
        app: app.clone(),
        mode,
        control,
        output,
        shot,
        fps: fps.unwrap_or(DEFAULT_FPS).clamp(1, 240),
        countdown,
    };
    let spawned = thread::Builder::new()
        .name("recording-start".to_owned())
        .spawn(move || launch.run());
    if let Err(error) = spawned {
        finish(app, mode, Ending::Cancelled);
        return Err(format!("could not start the recording thread: {error}"));
    }
    Ok(path)
}

/// Asks the recording of one mode to save its file. During a countdown this
/// cancels the start.
pub fn stop(app: &AppHandle, mode: Mode) {
    if let Some(active) = lock(&app.state::<AppState>().running).get(mode) {
        active.control.stop.store(true, Ordering::Relaxed);
    }
}

/// Stops the recording of one mode and deletes what it wrote.
pub fn discard(app: &AppHandle, mode: Mode) {
    if let Some(active) = lock(&app.state::<AppState>().running).get(mode) {
        active.discard.store(true, Ordering::Relaxed);
        active.control.stop.store(true, Ordering::Relaxed);
    }
}

/// Reports whether one mode records now.
fn records(app: &AppHandle, mode: Mode) -> bool {
    lock(&app.state::<AppState>().running).get(mode).is_some()
}

/// Shows the region selector on the monitor under the pointer.
///
/// Returns `None` when the user cancelled. One selector is open at a time.
fn pick_region(state: &AppState) -> Result<Option<DesktopRect>, String> {
    if state.selecting.swap(true, Ordering::Relaxed) {
        return Err("The region selector is open already".to_owned());
    }
    let picked = select_on_screen(MonitorChoice::Pointer).map_err(|error| format!("{error:#}"));
    state.selecting.store(false, Ordering::Relaxed);
    // The selector must be off the screen before a capture starts.
    thread::sleep(PICKER_CLOSE_TIME);
    picked
}

/// Lets the user select a region and records it.
fn record_region(app: &AppHandle) -> Result<(), String> {
    let state = app.state::<AppState>();
    if state.selecting.load(Ordering::Relaxed) {
        // A second press while the selector is open does nothing.
        return Ok(());
    }
    match pick_region(&state)? {
        Some(region) => begin(app, Mode::Video, CaptureTarget::Region(region), None).map(drop),
        None => Ok(()),
    }
}

/// Records the monitor under the pointer.
fn record_pointer_monitor(app: &AppHandle) -> Result<(), String> {
    let (x, y) = pointer_position();
    begin(app, Mode::Video, CaptureTarget::Point(x, y), None).map(drop)
}

/// Runs one action of a hotkey or of the tray menu on its own thread.
///
/// The interface did not ask for the action, so a failure reaches it as a
/// `stopped` event with the reason.
fn run_action(
    app: &AppHandle,
    mode: Mode,
    action: impl FnOnce(&AppHandle) -> Result<(), String> + Send + 'static,
) {
    let app = app.clone();
    let spawned = thread::Builder::new()
        .name("recording-action".to_owned())
        .spawn(move || {
            if let Err(error) = action(&app) {
                emit_stopped(&app, mode, None, None, Some(error), false);
            }
        });
    // Without the thread the key press does nothing.
    drop(spawned);
}

/// Acts on the hotkey of a video recording.
///
/// A video that records is saved. Otherwise the hotkey starts a recording
/// of a region or of the monitor under the pointer.
pub fn video_hotkey(app: &AppHandle, id: HotkeyId) {
    if records(app, Mode::Video) {
        stop(app, Mode::Video);
        return;
    }
    match id {
        HotkeyId::VideoRegion => run_action(app, Mode::Video, record_region),
        HotkeyId::VideoFullscreen => run_action(app, Mode::Video, record_pointer_monitor),
        HotkeyId::Region | HotkeyId::Fullscreen | HotkeyId::Window => {}
    }
}

/// Acts on one recording entry of the tray menu.
pub fn tray_action(app: &AppHandle, entry: &str) {
    match entry {
        "audio" if records(app, Mode::Audio) => stop(app, Mode::Audio),
        "audio" => run_action(app, Mode::Audio, |app| {
            begin(app, Mode::Audio, CaptureTarget::Monitor(0), None).map(drop)
        }),
        "videoRegion" => run_action(app, Mode::Video, |app| {
            // The menu must close before the selector freezes the screen.
            thread::sleep(MENU_CLOSE_TIME);
            record_region(app)
        }),
        "videoFullscreen" => run_action(app, Mode::Video, |app| {
            thread::sleep(MENU_CLOSE_TIME);
            record_pointer_monitor(app)
        }),
        "stopVideo" => stop(app, Mode::Video),
        _ => {}
    }
}

/// Converts a region of the interface to a desktop rectangle.
fn desktop_rect(region: Region) -> Result<DesktopRect, String> {
    match (u32::try_from(region.width), u32::try_from(region.height)) {
        (Ok(width), Ok(height)) if width > 0 && height > 0 => Ok(DesktopRect {
            left: region.left,
            top: region.top,
            width,
            height,
        }),
        _ => Err("The region has no size".to_owned()),
    }
}

/// Starts an audio or a video recording and returns the path of its file or
/// session folder.
///
/// `region` records one part of the desktop and wins over `monitor`. The
/// command returns before capture starts. The events tell the rest.
#[tauri::command(async)]
pub fn start_recording(
    app: AppHandle,
    mode: Mode,
    monitor: Option<u32>,
    fps: Option<u32>,
    region: Option<Region>,
) -> Result<String, String> {
    let target = match region.filter(|_| mode == Mode::Video) {
        Some(region) => CaptureTarget::Region(desktop_rect(region)?),
        None => CaptureTarget::Monitor(monitor.unwrap_or(0)),
    };
    begin(&app, mode, target, fps)
}

/// Asks the recording of one mode to save its file.
#[tauri::command(async)]
pub fn stop_recording(app: AppHandle, mode: Mode) {
    stop(&app, mode);
}

/// Stops the recording of one mode and deletes everything it wrote.
#[tauri::command(async)]
pub fn discard_recording(app: AppHandle, mode: Mode) {
    discard(&app, mode);
}

/// Pauses or resumes the recording of one mode. Returns the new pause state.
#[tauri::command(async)]
pub fn set_paused(state: State<'_, AppState>, mode: Mode, paused: bool) -> bool {
    match lock(&state.running).get(mode) {
        Some(active) if !stopping(&active.control) => {
            active.control.paused.store(paused, Ordering::Relaxed);
            paused
        }
        _ => false,
    }
}

/// Lets the user select a region of the desktop.
///
/// The window minimizes first, so it is not in the way, and returns when the
/// selector closes. The result is `None` when the user cancelled.
#[tauri::command(async)]
pub fn select_region(
    window: WebviewWindow,
    state: State<'_, AppState>,
) -> Result<Option<Region>, String> {
    let minimized = window.minimize().is_ok();
    if minimized {
        thread::sleep(MINIMIZE_TIME);
    }
    let picked = pick_region(&state);
    if minimized {
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
    Ok(picked?.map(|region| Region {
        left: region.left,
        top: region.top,
        width: region.width as i32,
        height: region.height as i32,
    }))
}
