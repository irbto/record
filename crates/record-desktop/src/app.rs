//! The dashboard: one window with a mode switcher and a panel for each mode.
//!
//! Every frame, the dashboard reads the waiting engine updates, handles the
//! keys, and draws the complete window from its state. This is the same shape
//! as the terminal interface.

use std::{
    collections::VecDeque,
    path::{Path, PathBuf},
    sync::{Arc, atomic::Ordering},
    time::{Duration, Instant, SystemTime},
};

use egui::{
    Align, Align2, Color32, CornerRadius, FontId, Frame, Key, Layout, Margin, Rect, RichText,
    Sense, Stroke, StrokeKind, TextureHandle, TextureOptions, Ui, pos2, vec2,
};
use record_core::screenshot::{
    CaptureMode,
    events::{CaptureHub, HubEvent, HubRequest, HubSettings},
    history::{HistoryEntry, list_history},
    upload::{
        PRESETS, Preset, import_sxcu, list_destinations, set_default_destination, write_setting,
    },
};

use crate::{
    backend::{Engines, Mode, Monitor, Session, SoundUpdate, VideoUpdate},
    theme::{AMBER, BORDER, CYAN, GREEN, MUTED, PANEL, RED, SELECTED, TEXT},
};

/// Name of the settings file that remembers the last mode.
pub const MODE_FILE: &str = "desktop-mode.txt";
/// MP3 bit rates offered by the sound panel, in kilobits per second.
const BITRATES: [u32; 4] = [128, 192, 256, 320];
/// Frame rates offered by the video panel.
const FRAME_RATES: [u32; 2] = [30, 60];
/// Number of level values kept for the scrolling waveform.
const WAVE_POINTS: usize = 360;
/// Longest edge of a preview texture in pixels.
const PREVIEW_EDGE: u32 = 1_600;

/// Measures recording time and leaves out the paused parts.
#[derive(Clone, Copy, Debug, Default)]
struct Stopwatch {
    running_since: Option<Instant>,
    banked: Duration,
}

impl Stopwatch {
    /// Starts or continues the count.
    fn run(&mut self) {
        self.running_since.get_or_insert_with(Instant::now);
    }

    /// Stops the count and keeps the time so far.
    fn hold(&mut self) {
        if let Some(since) = self.running_since.take() {
            self.banked += since.elapsed();
        }
    }

    /// Returns the counted time.
    fn elapsed(&self) -> Duration {
        self.banked
            + self
                .running_since
                .map_or(Duration::ZERO, |since| since.elapsed())
    }
}

/// State of the sound panel.
#[derive(Default)]
struct SoundPanel {
    session: Option<Session<SoundUpdate>>,
    bitrate_index: usize,
    left: f32,
    right: f32,
    seconds: f64,
    wave: VecDeque<f32>,
    finalizing: bool,
    files: Vec<PathBuf>,
}

/// State of the video panel.
#[derive(Default)]
struct VideoPanel {
    session: Option<Session<VideoUpdate>>,
    monitors: Vec<Monitor>,
    monitor: u32,
    fps_index: usize,
    clock: Stopwatch,
    preview: Option<TextureHandle>,
    size: Option<(u32, u32, u32)>,
    finalizing: bool,
    files: Vec<PathBuf>,
}

/// State of the screenshot panel.
#[derive(Default)]
struct ShotsPanel {
    history: Vec<HistoryEntry>,
    selected: usize,
    preview: Option<(PathBuf, TextureHandle)>,
    hotkeys: Option<[bool; 3]>,
    capturing: bool,
    auto: bool,
    /// The destination that waits for its key, with the typed text.
    key_entry: Option<(&'static Preset, String)>,
    /// The typed path of a `.sxcu` file to import, while the field is open.
    import_entry: Option<String>,
}

/// Complete state of the dashboard window.
pub struct Dashboard<E: Engines, H: CaptureHub> {
    engines: E,
    hub: H,
    settings: Arc<HubSettings>,
    mode: Mode,
    sound: SoundPanel,
    video: VideoPanel,
    shots: ShotsPanel,
    status: String,
    failed: bool,
    /// Writes choices to the settings folder. Tests turn this off.
    pub persist: bool,
}

impl<E: Engines, H: CaptureHub> Dashboard<E, H> {
    /// Creates the dashboard on the given mode.
    pub fn new(engines: E, hub: H, settings: Arc<HubSettings>, mode: Mode) -> Self {
        let monitors = engines.monitors();
        let monitor = monitors
            .iter()
            .find(|monitor| monitor.primary)
            .or(monitors.first())
            .map_or(0, |monitor| monitor.index);
        let history = list_history(&settings.options().directory);
        Self {
            engines,
            hub,
            settings,
            mode,
            sound: SoundPanel {
                bitrate_index: BITRATES.len() - 1,
                ..SoundPanel::default()
            },
            video: VideoPanel {
                monitors,
                monitor,
                fps_index: FRAME_RATES.len() - 1,
                ..VideoPanel::default()
            },
            shots: ShotsPanel {
                history,
                ..ShotsPanel::default()
            },
            status: "Ready".to_owned(),
            failed: false,
            persist: true,
        }
    }

    /// Returns the selected mode.
    #[must_use]
    pub const fn mode(&self) -> Mode {
        self.mode
    }

    /// Returns the status line text.
    #[must_use]
    pub fn status(&self) -> &str {
        &self.status
    }

    /// Reports whether a sound or video recording is running.
    #[must_use]
    pub const fn recording(&self) -> Option<Mode> {
        if self.sound.session.is_some() {
            Some(Mode::Sound)
        } else if self.video.session.is_some() {
            Some(Mode::Video)
        } else {
            None
        }
    }

    /// Stores a status line. Errors use the alert color.
    fn report(&mut self, status: impl Into<String>, failed: bool) {
        self.status = status.into();
        self.failed = failed;
    }

    /// Shows another mode and remembers it for the next start.
    pub fn select(&mut self, mode: Mode) {
        if self.mode != mode {
            self.mode = mode;
            if self.persist {
                let _ = write_setting(MODE_FILE, mode.id());
            }
        }
    }

    /// Starts or stops the recording of a mode.
    pub fn toggle_recording(&mut self, mode: Mode) {
        match (mode, self.recording()) {
            (Mode::Screenshot, _) => {}
            (_, Some(active)) if active != mode => {
                let name = active.label().to_lowercase();
                self.report(format!("Stop the {name} recording first"), true);
            }
            (Mode::Sound, Some(_)) => {
                if let Some(session) = &self.sound.session {
                    session.stop.store(true, Ordering::Relaxed);
                    self.report("Stopping", false);
                }
            }
            (Mode::Video, Some(_)) => {
                if let Some(session) = &self.video.session {
                    session.stop.store(true, Ordering::Relaxed);
                    self.video.clock.hold();
                    self.report("Stopping", false);
                }
            }
            (Mode::Sound, None) => {
                let name = format!("recording-{}.mp3", self.engines.timestamp());
                let output = self.engines.folder(Mode::Sound).join(name);
                let bitrate = BITRATES[self.sound.bitrate_index] * 1_000;
                self.sound.session = Some(self.engines.start_sound(output, bitrate));
                self.sound.seconds = 0.0;
                self.sound.wave.clear();
                self.sound.finalizing = false;
                self.report("Recording sound", false);
            }
            (Mode::Video, None) => {
                let name = format!("recording-{}.mp4", self.engines.timestamp());
                let output = self.engines.folder(Mode::Video).join(name);
                let fps = FRAME_RATES[self.video.fps_index];
                self.video.session =
                    Some(self.engines.start_video(output, self.video.monitor, fps));
                self.video.clock = Stopwatch::default();
                self.video.size = None;
                self.video.finalizing = false;
                self.report("Starting the video recording", false);
            }
        }
    }

    /// Pauses or continues the running recording of a mode.
    fn toggle_pause(&mut self, mode: Mode) {
        let paused = match mode {
            Mode::Sound => self.sound.session.as_ref().map(|session| &session.paused),
            Mode::Video => self.video.session.as_ref().map(|session| &session.paused),
            Mode::Screenshot => None,
        };
        let Some(paused) = paused else {
            return;
        };
        let now_paused = !paused.fetch_xor(true, Ordering::Relaxed);
        if mode == Mode::Video {
            if now_paused {
                self.video.clock.hold();
            } else {
                self.video.clock.run();
            }
        }
        self.report(if now_paused { "Paused" } else { "Recording" }, false);
    }

    /// Reports whether the recording of a mode is paused.
    fn is_paused(&self, mode: Mode) -> bool {
        let paused = match mode {
            Mode::Sound => self.sound.session.as_ref().map(|session| &session.paused),
            Mode::Video => self.video.session.as_ref().map(|session| &session.paused),
            Mode::Screenshot => None,
        };
        paused.is_some_and(|paused| paused.load(Ordering::Relaxed))
    }

    /// Applies every waiting update from the engines.
    fn drain(&mut self, context: &egui::Context) {
        let sound: Vec<SoundUpdate> = self
            .sound
            .session
            .as_ref()
            .map(|session| session.updates.try_iter().collect())
            .unwrap_or_default();
        for update in sound {
            match update {
                SoundUpdate::Started { bitrate, .. } => {
                    self.report(
                        format!("Recording sound at {} kbps", bitrate / 1_000),
                        false,
                    );
                }
                SoundUpdate::Levels {
                    left,
                    right,
                    seconds,
                } => {
                    self.sound.left = left;
                    self.sound.right = right;
                    self.sound.seconds = seconds;
                    self.sound.wave.push_back(left.max(right));
                    while self.sound.wave.len() > WAVE_POINTS {
                        self.sound.wave.pop_front();
                    }
                }
                SoundUpdate::Finalizing => self.sound.finalizing = true,
                SoundUpdate::Notice(notice) => self.report(notice, false),
                SoundUpdate::Stopped(result) => {
                    self.sound.session = None;
                    self.sound.finalizing = false;
                    self.sound.left = 0.0;
                    self.sound.right = 0.0;
                    match result {
                        Ok(path) => {
                            self.report(format!("Saved {}", file_name(&path)), false);
                            self.sound.files.insert(0, path);
                        }
                        Err(error) => self.report(format!("Sound recording failed: {error}"), true),
                    }
                }
            }
        }
        let video: Vec<VideoUpdate> = self
            .video
            .session
            .as_ref()
            .map(|session| session.updates.try_iter().collect())
            .unwrap_or_default();
        for update in video {
            match update {
                VideoUpdate::Started { width, height, fps } => {
                    self.video.size = Some((width, height, fps));
                    self.video.clock.run();
                    self.report(format!("Recording {width}x{height} at {fps} fps"), false);
                }
                VideoUpdate::Preview {
                    width,
                    height,
                    rgba,
                } => {
                    let size = [width as usize, height as usize];
                    if rgba.len() == size[0] * size[1] * 4 {
                        let image = egui::ColorImage::from_rgba_unmultiplied(size, &rgba);
                        self.video.preview = Some(context.load_texture(
                            "video-preview",
                            image,
                            TextureOptions::LINEAR,
                        ));
                    }
                }
                VideoUpdate::Finalizing => self.video.finalizing = true,
                VideoUpdate::Notice(notice) => self.report(notice, false),
                VideoUpdate::Stopped(result) => {
                    self.video.session = None;
                    self.video.finalizing = false;
                    self.video.clock.hold();
                    match result {
                        Ok(path) => {
                            self.report(format!("Saved {}", file_name(&path)), false);
                            self.video.files.insert(0, path);
                        }
                        Err(error) => self.report(format!("Video recording failed: {error}"), true),
                    }
                }
            }
        }
        let shots: Vec<HubEvent> = self.hub.events().try_iter().collect();
        for event in shots {
            match event {
                HubEvent::Ready(hotkeys) => self.shots.hotkeys = Some(hotkeys),
                HubEvent::Capturing => {
                    self.shots.capturing = true;
                    self.report("Capturing", false);
                }
                HubEvent::Finished(summary) => {
                    self.shots.capturing = false;
                    let mut status = match (&summary.uploaded, &summary.output) {
                        (Some(uploaded), _) => format!("Uploaded {}", uploaded.url),
                        (None, Some(path)) => format!("Saved {}", file_name(path)),
                        (None, None) => "Copied to the clipboard".to_owned(),
                    };
                    status.push_str(&format!(" · {}x{}", summary.width, summary.height));
                    if summary.hdr {
                        status.push_str(&format!(" · HDR at {:.0} nits", summary.sdr_white_nits));
                    }
                    let failed = summary.upload_error.is_some();
                    if let Some(error) = summary.upload_error {
                        status = format!("Upload failed: {error}");
                    }
                    self.report(status, failed);
                    self.refresh_history();
                }
                HubEvent::Cancelled => {
                    self.shots.capturing = false;
                    self.report("Cancelled. No file was written.", false);
                }
                HubEvent::Failed(error) => {
                    self.shots.capturing = false;
                    self.report(format!("Capture failed: {error}"), true);
                }
                HubEvent::Auto(active) => {
                    self.shots.auto = active;
                    self.report(
                        if active {
                            "Auto capture is on"
                        } else {
                            "Auto capture is off"
                        },
                        false,
                    );
                }
                HubEvent::QuitRequested => {}
            }
        }
    }

    /// Reads the screenshots folder again and selects the newest file.
    fn refresh_history(&mut self) {
        self.shots.history = list_history(&self.settings.options().directory);
        self.shots.selected = 0;
    }

    /// Sends a capture request unless a capture is running.
    fn capture(&mut self, request: HubRequest) {
        if self.shots.capturing && request != HubRequest::ToggleAuto {
            self.report("A capture is already running", false);
        } else {
            self.hub.request(request);
        }
    }

    /// Stores the screenshot choices for later runs.
    fn store(&mut self) {
        if self.persist
            && let Err(error) = self.settings.store()
        {
            self.report(error, true);
        }
    }

    /// Makes an upload destination active.
    fn select_destination(&mut self, destination: &str) {
        match self.settings.select_destination(destination) {
            Ok(name) => {
                if self.persist {
                    let _ = set_default_destination(Some(destination));
                }
                self.store();
                self.report(format!("Uploads go to {name}"), false);
            }
            Err(error) => self.report(error, true),
        }
    }

    /// Handles the keys that work in every mode and the keys of the mode.
    fn keys(&mut self, context: &egui::Context) {
        if context.egui_wants_keyboard_input() {
            return;
        }
        let pressed = |key: Key| context.input(|input| input.key_pressed(key));
        for (key, mode) in [
            (Key::Num1, Mode::Sound),
            (Key::Num2, Mode::Video),
            (Key::Num3, Mode::Screenshot),
        ] {
            if pressed(key) {
                self.select(mode);
            }
        }
        if pressed(Key::Space) {
            self.toggle_recording(self.mode);
        }
        if pressed(Key::P) {
            self.toggle_pause(self.mode);
        }
        if self.mode == Mode::Screenshot {
            for (key, request) in [
                (Key::R, HubRequest::Capture(CaptureMode::Region)),
                (Key::F, HubRequest::Capture(CaptureMode::Fullscreen)),
                (Key::W, HubRequest::Delayed(CaptureMode::Window)),
                (Key::M, HubRequest::Capture(CaptureMode::Monitor)),
                (Key::L, HubRequest::Capture(CaptureMode::LastRegion)),
            ] {
                if pressed(key) {
                    self.capture(request);
                }
            }
        }
    }

    /// Draws one frame of the complete window into the root area.
    pub fn ui(&mut self, root: &mut Ui) {
        let context = &root.ctx().clone();
        self.drain(context);
        self.keys(context);
        if self.recording().is_some() || self.shots.capturing {
            context.request_repaint_after(Duration::from_millis(33));
        } else {
            // Hotkey captures arrive while the window is idle.
            context.request_repaint_after(Duration::from_millis(250));
        }
        let bar = Frame::new()
            .fill(PANEL)
            .inner_margin(Margin::symmetric(16, 12));
        egui::Panel::top("modes").frame(bar).show(root, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new("record").color(CYAN).size(22.0).strong());
                ui.add_space(18.0);
                let recording = self.recording();
                for (index, mode) in Mode::ALL.into_iter().enumerate() {
                    let live = recording == Some(mode)
                        || (mode == Mode::Screenshot && self.shots.capturing);
                    if mode_button(ui, mode, self.mode == mode, live, index + 1) {
                        self.select(mode);
                    }
                }
            });
        });
        let footer = Frame::new()
            .fill(PANEL)
            .inner_margin(Margin::symmetric(16, 8));
        egui::Panel::bottom("status")
            .frame(footer)
            .show(root, |ui| {
                ui.horizontal(|ui| {
                    let color = if self.failed {
                        RED
                    } else if self.recording().is_some() || self.shots.capturing {
                        AMBER
                    } else {
                        GREEN
                    };
                    let (dot, _) = ui.allocate_exact_size(vec2(10.0, 10.0), Sense::hover());
                    ui.painter().circle_filled(dot.center(), 5.0, color);
                    ui.label(&self.status);
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.label(
                            RichText::new("1 Sound · 2 Video · 3 Screenshot · Space start or stop")
                                .color(MUTED),
                        );
                    });
                });
            });
        egui::CentralPanel::default().show(root, |ui| match self.mode {
            Mode::Sound => self.sound_ui(ui),
            Mode::Video => self.video_ui(ui),
            Mode::Screenshot => self.shots_ui(ui),
        });
    }

    /// Draws the record button with its time and the pause button.
    fn transport(&mut self, ui: &mut Ui, mode: Mode, elapsed: Duration, finalizing: bool) {
        let active = self.recording() == Some(mode);
        ui.horizontal(|ui| {
            if record_button(ui, active).clicked() {
                self.toggle_recording(mode);
            }
            ui.add_space(8.0);
            ui.vertical(|ui| {
                ui.add_space(14.0);
                ui.label(
                    RichText::new(clock_text(elapsed))
                        .font(FontId::monospace(44.0))
                        .color(if active { TEXT } else { MUTED }),
                );
                let state = if finalizing {
                    "Saving the file"
                } else if self.is_paused(mode) {
                    "Paused"
                } else if active {
                    "Recording. Press Space to stop."
                } else {
                    "Press Space to start."
                };
                ui.label(RichText::new(state).color(MUTED));
            });
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let label = if self.is_paused(mode) {
                    "Continue  P"
                } else {
                    "Pause  P"
                };
                if ui.add_enabled(active, egui::Button::new(label)).clicked() {
                    self.toggle_pause(mode);
                }
            });
        });
    }

    /// Draws the list of files that this window recorded.
    fn files_ui(&mut self, ui: &mut Ui, mode: Mode) {
        let files = match mode {
            Mode::Sound => self.sound.files.clone(),
            _ => self.video.files.clone(),
        };
        let folder = self.engines.folder(mode);
        card(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new("RECORDINGS").color(MUTED).small().strong());
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui.button("Open folder").clicked() {
                        self.engines.reveal(&folder);
                    }
                    ui.label(RichText::new(folder.display().to_string()).color(MUTED));
                });
            });
            if files.is_empty() {
                ui.label(RichText::new("Nothing recorded in this window yet.").color(MUTED));
            }
            for path in files.iter().take(6) {
                ui.horizontal(|ui| {
                    ui.label(file_name(path));
                    if ui.link("Show").clicked() {
                        self.engines.reveal(path);
                    }
                });
            }
        });
    }

    /// Draws the sound panel.
    fn sound_ui(&mut self, ui: &mut Ui) {
        let elapsed = Duration::from_secs_f64(self.sound.seconds.max(0.0));
        let finalizing = self.sound.finalizing;
        card(ui, |ui| {
            self.transport(ui, Mode::Sound, elapsed, finalizing)
        });
        card(ui, |ui| {
            ui.label(RichText::new("LEVELS").color(MUTED).small().strong());
            meter(ui, "L", self.sound.left);
            meter(ui, "R", self.sound.right);
            waveform(ui, &self.sound.wave);
        });
        card(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new("MP3 QUALITY").color(MUTED).small().strong());
                let idle = self.sound.session.is_none();
                for (index, rate) in BITRATES.into_iter().enumerate() {
                    let chosen = self.sound.bitrate_index == index;
                    let label = format!("{rate} kbps");
                    if ui
                        .add_enabled(idle, egui::Button::selectable(chosen, label))
                        .clicked()
                    {
                        self.sound.bitrate_index = index;
                    }
                }
            });
        });
        self.files_ui(ui, Mode::Sound);
    }

    /// Draws the video panel.
    fn video_ui(&mut self, ui: &mut Ui) {
        let elapsed = self.video.clock.elapsed();
        let finalizing = self.video.finalizing;
        card(ui, |ui| {
            self.transport(ui, Mode::Video, elapsed, finalizing)
        });
        card(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new("SCREEN").color(MUTED).small().strong());
                let idle = self.video.session.is_none();
                for monitor in self.video.monitors.clone() {
                    let chosen = self.video.monitor == monitor.index;
                    let label = format!(
                        "{} · {}x{}{}",
                        monitor.index + 1,
                        monitor.width,
                        monitor.height,
                        if monitor.primary { " · primary" } else { "" }
                    );
                    if ui
                        .add_enabled(idle, egui::Button::selectable(chosen, label))
                        .clicked()
                    {
                        self.video.monitor = monitor.index;
                    }
                }
                ui.add_space(12.0);
                ui.label(RichText::new("FRAME RATE").color(MUTED).small().strong());
                for (index, fps) in FRAME_RATES.into_iter().enumerate() {
                    let chosen = self.video.fps_index == index;
                    let label = format!("{fps} fps");
                    if ui
                        .add_enabled(idle, egui::Button::selectable(chosen, label))
                        .clicked()
                    {
                        self.video.fps_index = index;
                    }
                }
            });
        });
        let room = (ui.available_height() - 150.0).max(120.0);
        card(ui, |ui| {
            let size = vec2(ui.available_width(), room);
            match (&self.video.preview, self.video.session.is_some()) {
                (Some(texture), true) => picture(ui, texture, size, 3.0),
                _ => {
                    let (area, _) = ui.allocate_exact_size(size, Sense::hover());
                    ui.painter().text(
                        area.center(),
                        Align2::CENTER_CENTER,
                        "The live picture appears here while a recording runs.",
                        FontId::proportional(15.0),
                        MUTED,
                    );
                }
            }
        });
        self.files_ui(ui, Mode::Video);
    }

    /// Draws the screenshot panel.
    fn shots_ui(&mut self, ui: &mut Ui) {
        let hotkeys = self.shots.hotkeys.unwrap_or([true; 3]);
        let hint = |index: usize, keys: &str| {
            if hotkeys[index] {
                keys.to_owned()
            } else {
                format!("{keys} in use")
            }
        };
        card(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                for (label, keys, request) in [
                    (
                        "Region",
                        hint(0, "Ctrl+PrtSc"),
                        HubRequest::Capture(CaptureMode::Region),
                    ),
                    (
                        "Fullscreen",
                        hint(1, "PrtSc"),
                        HubRequest::Capture(CaptureMode::Fullscreen),
                    ),
                    (
                        "Active window",
                        hint(2, "Alt+PrtSc"),
                        HubRequest::Delayed(CaptureMode::Window),
                    ),
                    (
                        "Monitor",
                        "M".to_owned(),
                        HubRequest::Capture(CaptureMode::Monitor),
                    ),
                    (
                        "Last region",
                        "L".to_owned(),
                        HubRequest::Capture(CaptureMode::LastRegion),
                    ),
                ] {
                    if capture_button(ui, label, &keys).clicked() {
                        self.capture(request);
                    }
                }
                let auto = if self.shots.auto {
                    "Auto capture: on"
                } else {
                    "Auto capture: off"
                };
                if ui
                    .add(egui::Button::selectable(self.shots.auto, auto))
                    .clicked()
                {
                    self.capture(HubRequest::ToggleAuto);
                }
            });
        });
        card(ui, |ui| self.after_capture_ui(ui));
        let entries = self.shots.history.clone();
        let height = ui.available_height().max(120.0);
        ui.with_layout(Layout::left_to_right(Align::Min), |ui| {
            ui.vertical(|ui| {
                ui.set_width(320.0);
                card(ui, |ui| {
                    ui.set_min_height(height - 30.0);
                    ui.label(
                        RichText::new(format!("HISTORY · {}", entries.len()))
                            .color(MUTED)
                            .small()
                            .strong(),
                    );
                    if entries.is_empty() {
                        ui.label(RichText::new("No screenshots yet.").color(MUTED));
                    }
                    egui::ScrollArea::vertical().show(ui, |ui| {
                        let now = SystemTime::now();
                        for (index, entry) in entries.iter().enumerate() {
                            let chosen = index == self.shots.selected;
                            let text =
                                format!("{}   {}", entry.name, age_text(now, entry.modified));
                            if ui.add(egui::Button::selectable(chosen, text)).clicked() {
                                self.shots.selected = index;
                            }
                        }
                    });
                });
            });
            ui.vertical(|ui| {
                card(ui, |ui| {
                    ui.set_min_height(height - 30.0);
                    let Some(entry) = entries.get(self.shots.selected) else {
                        ui.label(RichText::new("Press R to capture a region.").color(MUTED));
                        return;
                    };
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(&entry.name).strong());
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if ui.button("Show in folder").clicked() {
                                self.engines.reveal(&entry.path);
                            }
                            if ui.button("Copy image").clicked() {
                                match self.engines.copy_image(&entry.path) {
                                    Ok(()) => self.report(format!("Copied {}", entry.name), false),
                                    Err(error) => {
                                        self.report(format!("Copy failed: {error}"), true)
                                    }
                                }
                            }
                        });
                    });
                    let stale = self
                        .shots
                        .preview
                        .as_ref()
                        .is_none_or(|(path, _)| *path != entry.path);
                    if stale {
                        self.shots.preview = load_picture(ui.ctx(), &entry.path)
                            .map(|texture| (entry.path.clone(), texture));
                    }
                    match &self.shots.preview {
                        Some((_, texture)) => picture(ui, texture, ui.available_size(), 1.0),
                        None => {
                            ui.label(RichText::new("This file has no preview.").color(MUTED));
                        }
                    }
                })
            });
        });
    }

    /// Draws the after-capture switches and the upload destination.
    fn after_capture_ui(&mut self, ui: &mut Ui) {
        let active = self
            .settings
            .active_uploader()
            .map(|uploader| uploader.name);
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new("AFTER CAPTURE").color(MUTED).small().strong());
            let mut changed = false;
            for (label, flag) in [
                ("Copy to clipboard", &self.settings.copy),
                ("Save to file", &self.settings.save),
            ] {
                let mut value = flag.load(Ordering::Relaxed);
                if ui.checkbox(&mut value, label).changed() {
                    flag.store(value, Ordering::Relaxed);
                    changed = true;
                }
            }
            // Copy or save always stays on, so no capture is lost.
            if !self.settings.copy.load(Ordering::Relaxed)
                && !self.settings.save.load(Ordering::Relaxed)
            {
                self.settings.save.store(true, Ordering::Relaxed);
            }
            let mut upload = active.is_some() && self.settings.upload.load(Ordering::Relaxed);
            let toggle = ui.add_enabled(
                active.is_some(),
                egui::Checkbox::new(&mut upload, "Upload and copy link"),
            );
            if toggle.changed() {
                self.settings.upload.store(upload, Ordering::Relaxed);
                changed = true;
            }
            if changed {
                self.store();
            }
            ui.add_space(12.0);
            ui.label(RichText::new("DESTINATION").color(MUTED).small().strong());
            let mut chosen: Option<String> = None;
            egui::ComboBox::from_id_salt("destination")
                .selected_text(active.clone().unwrap_or_else(|| "Not set".to_owned()))
                .show_ui(ui, |ui| {
                    for preset in &PRESETS {
                        let note = if preset.key_label.is_some() && preset.ready() {
                            "ready"
                        } else {
                            preset.note
                        };
                        let label = format!("{}  ({note})", preset.name);
                        let current = active.as_deref() == Some(preset.name);
                        if ui.selectable_label(current, label).clicked() {
                            if preset.ready() {
                                chosen = Some(preset.id.to_owned());
                            } else {
                                self.shots.key_entry = Some((preset, String::new()));
                            }
                        }
                    }
                    for name in list_destinations() {
                        let current = active.as_deref() == Some(name.as_str());
                        if ui
                            .selectable_label(current, format!("{name}  (.sxcu)"))
                            .clicked()
                        {
                            chosen = Some(name);
                        }
                    }
                    if ui.selectable_label(false, "Import a .sxcu file").clicked() {
                        self.shots.import_entry = Some(String::new());
                    }
                });
            if let Some(destination) = chosen {
                self.select_destination(&destination);
            }
        });
        if let Some((preset, mut text)) = self.shots.key_entry.take() {
            let label = preset.key_label.unwrap_or("key");
            let mut keep = true;
            ui.horizontal(|ui| {
                ui.label(format!(
                    "{} {label} from {}:",
                    preset.name, preset.key_source
                ));
                ui.text_edit_singleline(&mut text);
                if ui.button("Save").clicked() {
                    match preset.save_key(&text) {
                        Ok(()) => {
                            self.select_destination(preset.id);
                            keep = false;
                        }
                        Err(error) => self.report(error, true),
                    }
                }
                if ui.button("Cancel").clicked() {
                    keep = false;
                }
            });
            if keep {
                self.shots.key_entry = Some((preset, text));
            }
        }
        if let Some(mut path) = self.shots.import_entry.take() {
            let mut keep = true;
            ui.horizontal(|ui| {
                ui.label("Path of the .sxcu file:");
                ui.text_edit_singleline(&mut path);
                if ui.button("Import").clicked() {
                    match import_sxcu(Path::new(path.trim().trim_matches('"'))) {
                        Ok(name) => {
                            self.select_destination(&name);
                            keep = false;
                        }
                        Err(error) => self.report(error, true),
                    }
                }
                if ui.button("Cancel").clicked() {
                    keep = false;
                }
            });
            if keep {
                self.shots.import_entry = Some(path);
            }
        }
    }
}

/// Draws a bordered panel around some content.
fn card(ui: &mut Ui, content: impl FnOnce(&mut Ui)) {
    Frame::new()
        .fill(PANEL)
        .stroke(Stroke::new(1.0, BORDER))
        .corner_radius(CornerRadius::same(10))
        .inner_margin(Margin::same(14))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            content(ui);
        });
}

/// Draws one button of the mode switcher and reports a click.
fn mode_button(ui: &mut Ui, mode: Mode, chosen: bool, live: bool, number: usize) -> bool {
    let (area, response) = ui.allocate_exact_size(vec2(204.0, 46.0), Sense::click());
    let fill = if chosen || response.hovered() {
        SELECTED
    } else {
        PANEL
    };
    let outline = if chosen { CYAN } else { BORDER };
    let painter = ui.painter();
    painter.rect_filled(area, 10.0, fill);
    painter.rect_stroke(area, 10.0, Stroke::new(1.5, outline), StrokeKind::Inside);
    let ink = if chosen { CYAN } else { TEXT };
    let icon = Rect::from_center_size(pos2(area.left() + 28.0, area.center().y), vec2(20.0, 20.0));
    let stroke = Stroke::new(2.0, ink);
    match mode {
        Mode::Sound => {
            for (index, height) in [8.0, 18.0, 12.0, 20.0, 6.0].into_iter().enumerate() {
                let x = icon.left() + index as f32 * 5.0;
                painter.line_segment(
                    [
                        pos2(x, icon.center().y - height / 2.0),
                        pos2(x, icon.center().y + height / 2.0),
                    ],
                    stroke,
                );
            }
        }
        Mode::Video => {
            painter.rect_stroke(
                icon.shrink2(vec2(0.0, 3.0)),
                3.0,
                stroke,
                StrokeKind::Inside,
            );
            let center = icon.center();
            painter.add(egui::Shape::convex_polygon(
                vec![
                    pos2(center.x - 3.0, center.y - 4.0),
                    pos2(center.x + 4.0, center.y),
                    pos2(center.x - 3.0, center.y + 4.0),
                ],
                ink,
                Stroke::NONE,
            ));
        }
        Mode::Screenshot => {
            for (x, y, dx, dy) in [
                (icon.left(), icon.top(), 1.0, 1.0),
                (icon.right(), icon.top(), -1.0, 1.0),
                (icon.left(), icon.bottom(), 1.0, -1.0),
                (icon.right(), icon.bottom(), -1.0, -1.0),
            ] {
                painter.line_segment([pos2(x, y), pos2(x + 6.0 * dx, y)], stroke);
                painter.line_segment([pos2(x, y), pos2(x, y + 6.0 * dy)], stroke);
            }
        }
    }
    painter.text(
        pos2(area.left() + 50.0, area.center().y),
        Align2::LEFT_CENTER,
        mode.label(),
        FontId::proportional(17.0),
        ink,
    );
    painter.text(
        pos2(area.right() - 14.0, area.center().y),
        Align2::RIGHT_CENTER,
        number.to_string(),
        FontId::monospace(13.0),
        MUTED,
    );
    if live {
        painter.circle_filled(pos2(area.right() - 34.0, area.center().y), 5.0, RED);
    }
    response.clicked()
}

/// Draws the round record button. It shows a square while recording.
fn record_button(ui: &mut Ui, active: bool) -> egui::Response {
    let (area, response) = ui.allocate_exact_size(vec2(96.0, 96.0), Sense::click());
    let painter = ui.painter();
    let ring = if response.hovered() { TEXT } else { BORDER };
    painter.circle_stroke(area.center(), 44.0, Stroke::new(3.0, ring));
    if active {
        let stop = Rect::from_center_size(area.center(), vec2(34.0, 34.0));
        painter.rect_filled(stop, 6.0, RED);
    } else {
        painter.circle_filled(area.center(), 32.0, RED);
    }
    response
}

/// Draws a capture button with its shortcut under the name.
fn capture_button(ui: &mut Ui, label: &str, keys: &str) -> egui::Response {
    let (area, response) = ui.allocate_exact_size(vec2(150.0, 54.0), Sense::click());
    let painter = ui.painter();
    let fill = if response.hovered() { SELECTED } else { PANEL };
    let outline = if response.hovered() { CYAN } else { BORDER };
    painter.rect_filled(area, 8.0, fill);
    painter.rect_stroke(area, 8.0, Stroke::new(1.0, outline), StrokeKind::Inside);
    painter.text(
        pos2(area.center().x, area.top() + 19.0),
        Align2::CENTER_CENTER,
        label,
        FontId::proportional(15.0),
        TEXT,
    );
    painter.text(
        pos2(area.center().x, area.bottom() - 14.0),
        Align2::CENTER_CENTER,
        keys,
        FontId::monospace(11.0),
        MUTED,
    );
    response
}

/// Draws one level bar from 0 to 1.
fn meter(ui: &mut Ui, label: &str, level: f32) {
    ui.horizontal(|ui| {
        ui.label(RichText::new(label).monospace().color(MUTED));
        let (area, _) = ui.allocate_exact_size(vec2(ui.available_width(), 14.0), Sense::hover());
        let painter = ui.painter();
        painter.rect_filled(area, 7.0, Color32::from_rgb(13, 17, 23));
        let level = level.clamp(0.0, 1.0);
        let color = if level > 0.95 {
            RED
        } else if level > 0.7 {
            AMBER
        } else {
            GREEN
        };
        let mut filled = area;
        filled.set_width(area.width() * level);
        if filled.width() >= 1.0 {
            painter.rect_filled(filled, 7.0, color);
        }
    });
}

/// Draws the recent levels as a centered waveform.
fn waveform(ui: &mut Ui, levels: &VecDeque<f32>) {
    let (area, _) = ui.allocate_exact_size(vec2(ui.available_width(), 90.0), Sense::hover());
    let painter = ui.painter();
    painter.rect_filled(area, 8.0, Color32::from_rgb(13, 17, 23));
    let middle = area.center().y;
    painter.line_segment(
        [pos2(area.left(), middle), pos2(area.right(), middle)],
        Stroke::new(1.0, BORDER),
    );
    let step = area.width() / WAVE_POINTS as f32;
    let start = WAVE_POINTS - levels.len().min(WAVE_POINTS);
    for (index, level) in levels.iter().enumerate() {
        let x = area.left() + (start + index) as f32 * step;
        let reach = level.clamp(0.0, 1.0) * (area.height() / 2.0 - 4.0);
        painter.line_segment(
            [pos2(x, middle - reach), pos2(x, middle + reach)],
            Stroke::new(step.max(1.0), CYAN),
        );
    }
}

/// Draws a texture centered in the given space, as large as fits.
///
/// `limit` is the largest allowed magnification of the texture.
fn picture(ui: &mut Ui, texture: &TextureHandle, space: egui::Vec2, limit: f32) {
    let size = texture.size_vec2();
    if size.x < 1.0 || size.y < 1.0 || space.x < 1.0 || space.y < 1.0 {
        return;
    }
    let scale = (space.x / size.x).min(space.y / size.y).min(limit);
    let (area, _) = ui.allocate_exact_size(space, Sense::hover());
    let placed = Rect::from_center_size(area.center(), size * scale);
    let whole = Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0));
    ui.painter()
        .image(texture.id(), placed, whole, Color32::WHITE);
}

/// Decodes a saved image into a texture of limited size.
fn load_picture(context: &egui::Context, path: &Path) -> Option<TextureHandle> {
    let decoded = image::open(path).ok()?;
    let small = if decoded.width().max(decoded.height()) > PREVIEW_EDGE {
        decoded.thumbnail(PREVIEW_EDGE, PREVIEW_EDGE)
    } else {
        decoded
    };
    let rgba = small.into_rgba8();
    let size = [rgba.width() as usize, rgba.height() as usize];
    let image = egui::ColorImage::from_rgba_unmultiplied(size, rgba.as_raw());
    Some(context.load_texture(path.display().to_string(), image, TextureOptions::LINEAR))
}

/// Formats a duration as `MM:SS` or `H:MM:SS`.
fn clock_text(elapsed: Duration) -> String {
    let seconds = elapsed.as_secs();
    let (hours, minutes, seconds) = (seconds / 3_600, seconds / 60 % 60, seconds % 60);
    if hours > 0 {
        format!("{hours}:{minutes:02}:{seconds:02}")
    } else {
        format!("{minutes:02}:{seconds:02}")
    }
}

/// Formats how long ago a file changed, such as `5m` or `3d`.
fn age_text(now: SystemTime, modified: SystemTime) -> String {
    let seconds = now.duration_since(modified).map_or(0, |age| age.as_secs());
    match seconds {
        0..60 => "now".to_owned(),
        60..3_600 => format!("{}m", seconds / 60),
        3_600..86_400 => format!("{}h", seconds / 3_600),
        _ => format!("{}d", seconds / 86_400),
    }
}

/// Returns the file name of a path for status lines.
fn file_name(path: &Path) -> String {
    path.file_name().map_or_else(
        || path.display().to_string(),
        |name| name.to_string_lossy().into_owned(),
    )
}

#[cfg(test)]
pub(crate) mod tests {
    use std::sync::{Mutex, atomic::AtomicBool};

    use crossbeam_channel::{Receiver, Sender, unbounded};
    use record_core::screenshot::{MonitorChoice, ScreenshotSummary, events::CaptureOptions};

    use super::*;

    /// Records what the dashboard asks of the engines.
    #[derive(Default)]
    pub(crate) struct FakeEngines {
        pub(crate) folder: PathBuf,
        pub(crate) sound: Mutex<Option<Sender<SoundUpdate>>>,
        pub(crate) video: Mutex<Option<Sender<VideoUpdate>>>,
        pub(crate) started: Mutex<Vec<String>>,
    }

    impl Engines for &FakeEngines {
        fn start_sound(&self, output: PathBuf, bitrate: u32) -> Session<SoundUpdate> {
            let (sender, updates) = unbounded();
            *self.sound.lock().unwrap() = Some(sender);
            let entry = format!("sound {} {bitrate}", file_name(&output));
            self.started.lock().unwrap().push(entry);
            Session {
                updates,
                stop: Arc::default(),
                paused: Arc::default(),
            }
        }

        fn start_video(&self, output: PathBuf, monitor: u32, fps: u32) -> Session<VideoUpdate> {
            let (sender, updates) = unbounded();
            *self.video.lock().unwrap() = Some(sender);
            let entry = format!("video {} {monitor} {fps}", file_name(&output));
            self.started.lock().unwrap().push(entry);
            Session {
                updates,
                stop: Arc::default(),
                paused: Arc::default(),
            }
        }

        fn monitors(&self) -> Vec<Monitor> {
            vec![
                Monitor {
                    index: 0,
                    name: "DISPLAY1".to_owned(),
                    width: 2_560,
                    height: 1_440,
                    primary: false,
                },
                Monitor {
                    index: 1,
                    name: "DISPLAY2".to_owned(),
                    width: 1_920,
                    height: 1_080,
                    primary: true,
                },
            ]
        }

        fn timestamp(&self) -> String {
            "20260101-120000".to_owned()
        }

        fn folder(&self, _mode: Mode) -> PathBuf {
            self.folder.clone()
        }

        fn reveal(&self, _path: &Path) {}

        fn copy_image(&self, _path: &Path) -> Result<(), String> {
            Ok(())
        }
    }

    /// Records capture requests in place of the capture thread.
    pub(crate) struct FakeHub {
        pub(crate) events: Receiver<HubEvent>,
        pub(crate) requests: Mutex<Vec<HubRequest>>,
    }

    impl CaptureHub for &FakeHub {
        fn events(&self) -> &Receiver<HubEvent> {
            &self.events
        }

        fn request(&self, request: HubRequest) {
            self.requests.lock().unwrap().push(request);
        }
    }

    /// Everything a test needs to drive a dashboard without a window.
    pub(crate) struct Bench {
        pub(crate) engines: FakeEngines,
        pub(crate) hub: FakeHub,
        pub(crate) hub_events: Sender<HubEvent>,
        pub(crate) settings: Arc<HubSettings>,
        pub(crate) context: egui::Context,
        pub(crate) _folder: tempfile::TempDir,
    }

    impl Bench {
        pub(crate) fn new() -> Self {
            let folder = tempfile::tempdir().unwrap();
            let (hub_events, events) = unbounded();
            let settings = Arc::new(HubSettings {
                options: Mutex::new(CaptureOptions {
                    directory: folder.path().to_path_buf(),
                    monitor: MonitorChoice::Pointer,
                    sdr_white_nits: None,
                    auto_interval: Duration::from_secs(5),
                    delay: Duration::from_secs(3),
                }),
                copy: AtomicBool::new(true),
                save: AtomicBool::new(true),
                uploader: Mutex::new(None),
                upload: AtomicBool::new(false),
            });
            let context = egui::Context::default();
            crate::theme::apply(&context);
            Self {
                engines: FakeEngines {
                    folder: folder.path().to_path_buf(),
                    ..FakeEngines::default()
                },
                hub: FakeHub {
                    events,
                    requests: Mutex::new(Vec::new()),
                },
                hub_events,
                settings,
                context,
                _folder: folder,
            }
        }

        pub(crate) fn dashboard(&self, mode: Mode) -> Dashboard<&FakeEngines, &FakeHub> {
            let mut dashboard =
                Dashboard::new(&self.engines, &self.hub, Arc::clone(&self.settings), mode);
            dashboard.persist = false;
            dashboard
        }

        /// Draws one frame with the given keys pressed.
        pub(crate) fn frame(
            &self,
            dashboard: &mut Dashboard<&FakeEngines, &FakeHub>,
            keys: &[Key],
        ) {
            // A test has no renderer that could take the texture changes.
            self.frame_output(dashboard, keys).textures_delta.clear();
        }

        /// Draws one frame and returns everything a renderer needs.
        pub(crate) fn frame_output(
            &self,
            dashboard: &mut Dashboard<&FakeEngines, &FakeHub>,
            keys: &[Key],
        ) -> egui::FullOutput {
            let events = keys
                .iter()
                .map(|key| egui::Event::Key {
                    key: *key,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: egui::Modifiers::NONE,
                })
                .collect();
            let input = egui::RawInput {
                screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(1_080.0, 720.0))),
                events,
                ..Default::default()
            };
            self.context.run_ui(input, |ui| dashboard.ui(ui))
        }
    }

    #[test]
    fn number_keys_switch_between_the_three_modes() {
        let bench = Bench::new();
        let mut dashboard = bench.dashboard(Mode::Screenshot);
        for (key, mode) in [
            (Key::Num1, Mode::Sound),
            (Key::Num2, Mode::Video),
            (Key::Num3, Mode::Screenshot),
        ] {
            bench.frame(&mut dashboard, &[key]);
            assert_eq!(dashboard.mode(), mode);
            bench.frame(&mut dashboard, &[]);
        }
    }

    #[test]
    fn space_starts_and_stops_a_sound_recording() {
        let bench = Bench::new();
        let mut dashboard = bench.dashboard(Mode::Sound);
        bench.frame(&mut dashboard, &[Key::Space]);
        assert_eq!(dashboard.recording(), Some(Mode::Sound));
        assert_eq!(
            *bench.engines.started.lock().unwrap(),
            ["sound recording-20260101-120000.mp3 320000"]
        );
        let sound = bench.engines.sound.lock().unwrap().clone().unwrap();
        sound
            .send(SoundUpdate::Levels {
                left: 0.5,
                right: 0.25,
                seconds: 75.0,
            })
            .unwrap();
        bench.frame(&mut dashboard, &[]);
        assert_eq!(dashboard.sound.seconds, 75.0);
        assert_eq!(dashboard.sound.wave.back(), Some(&0.5));
        bench.frame(&mut dashboard, &[Key::Space]);
        let stop = &dashboard.sound.session.as_ref().unwrap().stop;
        assert!(stop.load(Ordering::Relaxed));
        let saved = bench.engines.folder.join("recording-20260101-120000.mp3");
        sound.send(SoundUpdate::Stopped(Ok(saved.clone()))).unwrap();
        bench.frame(&mut dashboard, &[]);
        assert_eq!(dashboard.recording(), None);
        assert_eq!(dashboard.status(), "Saved recording-20260101-120000.mp3");
        assert_eq!(dashboard.sound.files, [saved]);
    }

    #[test]
    fn one_recording_runs_at_a_time_and_modes_stay_reachable() {
        let bench = Bench::new();
        let mut dashboard = bench.dashboard(Mode::Sound);
        bench.frame(&mut dashboard, &[Key::Space]);
        bench.frame(&mut dashboard, &[Key::Num2]);
        assert_eq!(dashboard.mode(), Mode::Video);
        bench.frame(&mut dashboard, &[Key::Space]);
        assert_eq!(dashboard.recording(), Some(Mode::Sound));
        assert_eq!(dashboard.status(), "Stop the sound recording first");
        assert_eq!(bench.engines.started.lock().unwrap().len(), 1);
    }

    #[test]
    fn video_uses_the_primary_monitor_and_reports_a_failure() {
        let bench = Bench::new();
        let mut dashboard = bench.dashboard(Mode::Video);
        bench.frame(&mut dashboard, &[Key::Space]);
        assert_eq!(
            *bench.engines.started.lock().unwrap(),
            ["video recording-20260101-120000.mp4 1 60"]
        );
        let video = bench.engines.video.lock().unwrap().clone().unwrap();
        video
            .send(VideoUpdate::Started {
                width: 1_920,
                height: 1_080,
                fps: 60,
            })
            .unwrap();
        video
            .send(VideoUpdate::Preview {
                width: 2,
                height: 1,
                rgba: vec![255; 8],
            })
            .unwrap();
        bench.frame(&mut dashboard, &[]);
        assert_eq!(dashboard.status(), "Recording 1920x1080 at 60 fps");
        assert!(dashboard.video.preview.is_some());
        bench.frame(&mut dashboard, &[Key::P]);
        assert!(dashboard.is_paused(Mode::Video));
        video
            .send(VideoUpdate::Stopped(Err("the encoder stopped".to_owned())))
            .unwrap();
        bench.frame(&mut dashboard, &[]);
        assert_eq!(dashboard.recording(), None);
        assert_eq!(
            dashboard.status(),
            "Video recording failed: the encoder stopped"
        );
    }

    #[test]
    fn screenshot_keys_send_requests_and_results_reach_the_status_line() {
        let bench = Bench::new();
        let mut dashboard = bench.dashboard(Mode::Screenshot);
        bench.frame(&mut dashboard, &[Key::R]);
        bench.frame(&mut dashboard, &[Key::F]);
        assert_eq!(
            *bench.hub.requests.lock().unwrap(),
            [
                HubRequest::Capture(CaptureMode::Region),
                HubRequest::Capture(CaptureMode::Fullscreen),
            ]
        );
        let output = bench.engines.folder.join("screenshot-1.png");
        image::RgbImage::from_pixel(8, 6, image::Rgb([200, 40, 40]))
            .save(&output)
            .unwrap();
        bench
            .hub_events
            .send(HubEvent::Finished(ScreenshotSummary {
                output: Some(output),
                width: 8,
                height: 6,
                copied: true,
                clipboard_error: None,
                uploaded: None,
                upload_error: None,
                hdr: true,
                sdr_white_nits: 240.0,
            }))
            .unwrap();
        bench.frame(&mut dashboard, &[]);
        assert_eq!(
            dashboard.status(),
            "Saved screenshot-1.png · 8x6 · HDR at 240 nits"
        );
        assert_eq!(dashboard.shots.history.len(), 1);
        assert!(dashboard.shots.preview.is_some());
        // Sound keys do nothing in the screenshot mode.
        bench.frame(&mut dashboard, &[Key::Space]);
        assert_eq!(dashboard.recording(), None);
    }

    #[test]
    fn clock_text_adds_hours_only_when_needed() {
        assert_eq!(clock_text(Duration::from_secs(75)), "01:15");
        assert_eq!(clock_text(Duration::from_secs(3_725)), "1:02:05");
    }

    #[test]
    fn mode_names_round_trip() {
        for mode in Mode::ALL {
            assert_eq!(Mode::from_id(mode.id()), Some(mode));
        }
        assert_eq!(Mode::from_id(" VIDEO "), Some(Mode::Video));
        assert_eq!(Mode::from_id("photos"), None);
    }
}
