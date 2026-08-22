//! Interactive terminal recorder, session file list, and clip editor.
//!
//! [`crate::tui::run`] owns terminal input and drawing on the main thread. `App` keeps all
//! interface state. Audio samples arrive through a bounded channel, so drawing
//! cannot block WASAPI or Media Foundation. The default split view shows both
//! live channels and the spectrum.
//!
//! A named clip is a finalized session part with a temporary PCM edit source.
//! The editor displays that complete source, moves frame-accurate boundaries,
//! previews only the selection with Windows MCI, and trims on a helper thread.
//! Preview pauses capture so the recorder does not capture its own playback.

use std::{
    collections::VecDeque,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use anyhow::Result;
use crossbeam_channel::{Receiver, Sender, TrySendError};
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::{
    DefaultTerminal, Frame,
    layout::{Alignment, Constraint, Flex, Layout, Rect},
    style::{Color, Modifier, Style, Stylize},
    symbols::{self, Marker},
    text::{Line as TextLine, Span},
    widgets::{
        Block, BorderType, Borders, Clear, LineGauge, Padding, Paragraph, Sparkline, Wrap,
        canvas::{Canvas, Line as CanvasLine},
    },
};

use crate::{
    audio::{AudioCommand, AudioEvent, RecordingSummary, SavedFile, SavedFileKind},
    clip::{
        ClipSelection, ClipWaveform, PreviewPlayer, SelectionHandle, WaveformBin,
        load_pcm_waveform, trim_clip,
    },
    session::{OutputTarget, clip_stem},
    spectrum::Spectrum,
    video::{VideoEvent, VideoSummary},
    waveform::Waveform,
};

const CYAN: Color = Color::Rgb(65, 214, 224);
const GREEN: Color = Color::Rgb(72, 214, 147);
const RED: Color = Color::Rgb(247, 75, 103);
const AMBER: Color = Color::Rgb(244, 183, 73);
const PURPLE: Color = Color::Rgb(172, 122, 255);
const MUTED: Color = Color::Rgb(126, 139, 158);
const BORDER: Color = Color::Rgb(55, 65, 82);

#[derive(Clone, Copy, Debug, Default)]
/// Selects the live visualization arrangement.
enum ViewMode {
    Waveform,
    Spectrum,
    #[default]
    Split,
}

impl ViewMode {
    /// Returns the next view in the fixed keyboard cycle.
    const fn next(self) -> Self {
        match self {
            Self::Waveform => Self::Spectrum,
            Self::Spectrum => Self::Split,
            Self::Split => Self::Waveform,
        }
    }

    /// Returns the short label for the recorder header.
    const fn label(self) -> &'static str {
        match self {
            Self::Waveform => "WAVE",
            Self::Spectrum => "SPECTRUM",
            Self::Split => "SPLIT",
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
/// Describes the recorder state shown in the header.
enum CaptureState {
    #[default]
    Starting,
    Recording,
    Paused,
    Previewing,
    Finalizing,
}

/// Runs the full-screen interface until the audio worker and any trim job stop.
pub fn run(
    events: &Receiver<AudioEvent>,
    worker: &JoinHandle<Result<RecordingSummary>>,
    stop: Arc<AtomicBool>,
    paused: Arc<AtomicBool>,
    commands: Sender<AudioCommand>,
    target: OutputTarget,
) -> Result<()> {
    let mut app = App::new(stop, paused, commands, target);
    ratatui::run(|terminal| app.run(terminal, events, worker))
}

/// Runs the full-screen screen-capture interface until the video worker stops.
pub fn run_video(
    events: &Receiver<VideoEvent>,
    worker: &JoinHandle<Result<VideoSummary>>,
    stop: Arc<AtomicBool>,
    paused: Arc<AtomicBool>,
    output: PathBuf,
) -> Result<()> {
    let mut app = VideoApp::new(stop, paused, output);
    ratatui::run(|terminal| app.run(terminal, events, worker))
}

#[derive(Debug, Default)]
/// Stores text and validation feedback for the clip-name modal.
struct ClipPrompt {
    /// Contains the file name as the user enters it.
    input: String,
    /// Contains the last validation or channel error.
    error: Option<String>,
}

/// Stores one open selection and its bounded full-clip waveform.
struct ClipReview {
    /// Identifies the file in the session save list.
    save_index: usize,
    /// Contains absolute PCM frame boundaries for the selection.
    selection: ClipSelection,
    /// Contains the bounded display copy of both channels.
    waveform: ClipWaveform,
    /// Contains the last preview or trim error.
    error: Option<String>,
}

/// Tracks a Media Foundation trim that runs away from terminal drawing.
struct TrimJob {
    /// Identifies the file that the worker will replace.
    save_index: usize,
    /// Contains the first PCM frame to keep.
    start_frame: u64,
    /// Contains the exclusive last PCM frame to keep.
    end_frame: u64,
    /// Owns the native encode worker until it stops.
    worker: JoinHandle<Result<u64>>,
}

/// Owns all terminal state for one recording process.
struct App {
    /// Contains the capture state that the header shows.
    state: CaptureState,
    /// Contains the active live visualization layout.
    view: ViewMode,
    /// Contains six seconds of live stereo samples.
    waveform: Waveform,
    /// Contains the lazy FFT state after first use.
    spectrum: Option<Spectrum>,
    /// Requests final capture shutdown.
    stop: Arc<AtomicBool>,
    /// Omits capture packets while true.
    paused: Arc<AtomicBool>,
    /// Sends clip requests to the audio owner thread.
    commands: Sender<AudioCommand>,
    /// Contains the session directory or explicit file path.
    output: PathBuf,
    /// Reports whether this output target can make named clips.
    clips_enabled: bool,
    /// Contains finalized MP3 files in capture order.
    saved: VecDeque<SavedFile>,
    /// Identifies the active row in the save list.
    selected_save: Option<usize>,
    /// Contains the open clip-name modal, if any.
    clip_prompt: Option<ClipPrompt>,
    /// Contains the open clip editor, if any.
    clip_review: Option<ClipReview>,
    /// Owns the active Windows preview device.
    preview: Option<PreviewPlayer>,
    /// Stores the pause state from before preview started.
    preview_was_paused: Option<bool>,
    /// Contains one active background trim operation.
    trim_job: Option<TrimJob>,
    /// Contains short feedback for the saves panel.
    notice: Option<String>,
    /// Contains total encoded frames for elapsed time.
    encoded_frames: u64,
    /// Contains the rate that Media Foundation selected.
    sample_rate: u32,
    /// Contains the bit rate that Media Foundation selected.
    bitrate: u32,
    /// Reports whether the help overlay is open.
    show_help: bool,
    /// Describes the source that the saves selection currently targets.
    source_label: Option<String>,
}

impl App {
    /// Creates the initial state without an FFT plan or large waveform reserve.
    fn new(
        stop: Arc<AtomicBool>,
        paused: Arc<AtomicBool>,
        commands: Sender<AudioCommand>,
        target: OutputTarget,
    ) -> Self {
        Self {
            state: CaptureState::Starting,
            view: ViewMode::default(),
            waveform: Waveform::new(48_000 * 6),
            spectrum: None,
            stop,
            paused,
            commands,
            output: target.root().to_path_buf(),
            clips_enabled: target.is_session(),
            saved: VecDeque::new(),
            selected_save: None,
            clip_prompt: None,
            clip_review: None,
            preview: None,
            preview_was_paused: None,
            trim_job: None,
            notice: None,
            encoded_frames: 0,
            sample_rate: 48_000,
            bitrate: 320_000,
            show_help: false,
            source_label: None,
        }
    }

    /// Draws frames and handles input until all worker activity is complete.
    fn run(
        &mut self,
        terminal: &mut DefaultTerminal,
        events: &Receiver<AudioEvent>,
        worker: &JoinHandle<Result<RecordingSummary>>,
    ) -> Result<()> {
        loop {
            self.drain_audio_events(events);
            self.poll_preview();
            self.poll_trim_job();
            if worker.is_finished() && self.trim_job.is_none() {
                break;
            }
            terminal.draw(|frame| self.render(frame))?;
            if event::poll(Duration::from_millis(33))? {
                self.handle_event(event::read()?);
            }
            if self.stop.load(Ordering::Relaxed) {
                self.state = CaptureState::Finalizing;
            }
        }
        self.drain_audio_events(events);
        terminal.draw(|frame| self.render(frame))?;
        Ok(())
    }

    /// Updates the header source label from the saves selection.
    fn refresh_source_label(&mut self) {
        self.source_label = self
            .selected_save
            .and_then(|index| self.saved.get(index))
            .map(|file| {
                let name = file
                    .path
                    .file_name()
                    .unwrap_or(file.path.as_os_str())
                    .to_string_lossy();
                let kind = match file.kind {
                    SavedFileKind::Recording => "FILE",
                    SavedFileKind::Part => "PART",
                    SavedFileKind::Clip => "CLIP",
                };
                format!("{kind} · {name}")
            });
    }

    /// Applies all pending audio events without waiting for another event.
    fn drain_audio_events(&mut self, events: &Receiver<AudioEvent>) {
        let mut samples_changed = false;
        for event in events.try_iter() {
            match event {
                AudioEvent::Started {
                    sample_rate,
                    bitrate,
                    ..
                } => {
                    self.sample_rate = sample_rate;
                    self.bitrate = bitrate;
                    self.state = CaptureState::Recording;
                }
                AudioEvent::Samples {
                    left,
                    right,
                    encoded_frames,
                } => {
                    self.waveform.push(&left, &right);
                    samples_changed = true;
                    self.encoded_frames = encoded_frames;
                }
                AudioEvent::Saved(file) => {
                    self.notice = Some(format!("Saved {}", file.path.display()));
                    self.saved.push_back(file);
                    self.selected_save = Some(self.saved.len() - 1);
                    self.refresh_source_label();
                }
                AudioEvent::Notice(message) => self.notice = Some(message),
                AudioEvent::Finalizing => self.state = CaptureState::Finalizing,
            }
        }
        if samples_changed && matches!(self.view, ViewMode::Spectrum | ViewMode::Split) {
            let spectrum = self.spectrum.get_or_insert_with(|| Spectrum::new(2_048));
            let (wave_left, wave_right) = self.waveform.channels();
            spectrum.update(
                wave_left
                    .iter()
                    .zip(wave_right)
                    .map(|(left, right)| (left + right) * 0.5),
            );
        }
    }

    /// Routes one terminal event to the active modal or recorder controls.
    fn handle_event(&mut self, event: Event) {
        let Event::Key(key) = event else {
            return;
        };
        if !matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
            return;
        }
        if matches!(key.code, KeyCode::Char('c' | 'C'))
            && key.modifiers.contains(KeyModifiers::CONTROL)
        {
            self.request_stop();
            return;
        }
        if self.clip_prompt.is_some() {
            self.handle_clip_prompt(key);
            return;
        }
        if self.clip_review.is_some() {
            self.handle_clip_review(key);
            return;
        }
        if self.show_help {
            if matches!(key.code, KeyCode::Esc | KeyCode::Char('?')) {
                self.show_help = false;
            }
            return;
        }
        match key {
            KeyEvent {
                code: KeyCode::Char('s' | 'S' | 'q' | 'Q'),
                ..
            }
            | KeyEvent {
                code: KeyCode::Esc, ..
            } => self.request_stop(),
            KeyEvent {
                code: KeyCode::Char(' '),
                ..
            } if !matches!(
                self.state,
                CaptureState::Starting | CaptureState::Finalizing
            ) =>
            {
                let paused = !self.paused.load(Ordering::Relaxed);
                self.paused.store(paused, Ordering::Relaxed);
                self.state = if paused {
                    CaptureState::Paused
                } else {
                    CaptureState::Recording
                };
            }
            KeyEvent {
                code: KeyCode::Char('w' | 'W'),
                ..
            } => {
                self.view = self.view.next();
                if matches!(self.view, ViewMode::Spectrum | ViewMode::Split) {
                    self.spectrum.get_or_insert_with(|| Spectrum::new(2_048));
                }
            }
            KeyEvent {
                code: KeyCode::Char('?'),
                ..
            } => self.show_help = true,
            KeyEvent {
                code: KeyCode::Char('c' | 'C'),
                ..
            } if self.clips_enabled
                && matches!(self.state, CaptureState::Recording | CaptureState::Paused) =>
            {
                self.clip_prompt = Some(ClipPrompt::default());
                self.notice = None;
            }
            KeyEvent {
                code: KeyCode::Up, ..
            } => self.select_previous_save(),
            KeyEvent {
                code: KeyCode::Down,
                ..
            } => self.select_next_save(),
            KeyEvent {
                code: KeyCode::Char('e' | 'E'),
                ..
            } => self.open_clip_review(),
            _ => {}
        }
    }

    /// Stops preview first and then requests MP3 finalization.
    fn request_stop(&mut self) {
        self.stop_preview();
        self.stop.store(true, Ordering::Relaxed);
        self.state = CaptureState::Finalizing;
    }

    /// Edits, validates, or submits the clip-name modal.
    fn handle_clip_prompt(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => self.clip_prompt = None,
            KeyCode::Enter => {
                let requested = self
                    .clip_prompt
                    .as_ref()
                    .map_or("", |prompt| prompt.input.as_str());
                match clip_stem(requested) {
                    Ok(stem) => {
                        match self.commands.try_send(AudioCommand::SaveClip(stem.clone())) {
                            Ok(()) => {
                                self.notice = Some(format!("Saving {stem}.mp3"));
                                self.clip_prompt = None;
                            }
                            Err(TrySendError::Full(_)) => {
                                if let Some(prompt) = &mut self.clip_prompt {
                                    prompt.error =
                                        Some("The audio thread is busy. Try again.".to_owned());
                                }
                            }
                            Err(TrySendError::Disconnected(_)) => {
                                if let Some(prompt) = &mut self.clip_prompt {
                                    prompt.error = Some("Audio capture has stopped.".to_owned());
                                }
                            }
                        }
                    }
                    Err(error) => {
                        if let Some(prompt) = &mut self.clip_prompt {
                            prompt.error = Some(error.to_string());
                        }
                    }
                }
            }
            KeyCode::Backspace => {
                if let Some(prompt) = &mut self.clip_prompt {
                    prompt.input.pop();
                    prompt.error = None;
                }
            }
            KeyCode::Char(character)
                if !character.is_control()
                    && !key
                        .modifiers
                        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                if let Some(prompt) = &mut self.clip_prompt
                    && prompt.input.chars().count() < 96
                {
                    prompt.input.push(character);
                    prompt.error = None;
                }
            }
            _ => {}
        }
    }

    /// Moves the save-list selection toward the first item.
    fn select_previous_save(&mut self) {
        self.selected_save = match self.selected_save {
            Some(index) if index > 0 => Some(index - 1),
            Some(_) | None if !self.saved.is_empty() => Some(0),
            _ => None,
        };
        self.refresh_source_label();
    }

    /// Moves the save-list selection toward the last item.
    fn select_next_save(&mut self) {
        self.selected_save = match self.selected_save {
            Some(index) if index + 1 < self.saved.len() => Some(index + 1),
            Some(index) => Some(index),
            None if !self.saved.is_empty() => Some(0),
            None => None,
        };
        self.refresh_source_label();
    }

    /// Loads the complete PCM waveform for the selected named clip.
    fn open_clip_review(&mut self) {
        let Some(index) = self.selected_save else {
            self.notice = Some("Select a named clip first".to_owned());
            return;
        };
        let Some(file) = self.saved.get(index) else {
            self.notice = Some("The selected file is no longer available".to_owned());
            return;
        };
        let Some(source) = &file.edit_source else {
            self.notice = Some("Only named clips can open in the editor".to_owned());
            return;
        };
        let selection =
            match ClipSelection::new(source.start_frame, source.end_frame, file.sample_rate) {
                Ok(selection) => selection,
                Err(error) => {
                    self.notice = Some(error.to_string());
                    return;
                }
            };
        match load_pcm_waveform(
            &source.pcm_path,
            source.start_frame,
            source.end_frame,
            2_048,
        ) {
            Ok(waveform) => {
                self.clip_review = Some(ClipReview {
                    save_index: index,
                    selection,
                    waveform,
                    error: None,
                });
                self.notice = None;
            }
            Err(error) => self.notice = Some(format!("Could not open clip: {error:#}")),
        }
    }

    /// Applies trim-handle and preview keys to the open editor.
    fn handle_clip_review(&mut self, key: KeyEvent) {
        if self.trim_job.is_some() {
            return;
        }
        match key.code {
            KeyCode::Esc => {
                self.stop_preview();
                self.clip_review = None;
            }
            KeyCode::Tab => {
                if let Some(review) = &mut self.clip_review {
                    review.selection.toggle_handle();
                    review.error = None;
                }
            }
            KeyCode::Left | KeyCode::Right => {
                if let Some(review) = &mut self.clip_review {
                    let rate = i64::from(review.selection.sample_rate());
                    let step = if key.modifiers.contains(KeyModifiers::SHIFT) {
                        rate
                    } else if key.modifiers.contains(KeyModifiers::CONTROL) {
                        (rate / 100).max(1)
                    } else {
                        (rate / 10).max(1)
                    };
                    let direction = if key.code == KeyCode::Left { -1 } else { 1 };
                    review.selection.nudge(step * direction);
                    review.error = None;
                }
            }
            KeyCode::Char('r' | 'R') => {
                if let Some(review) = &mut self.clip_review {
                    review.selection.reset();
                    review.error = None;
                }
            }
            KeyCode::Char('p' | 'P') => self.toggle_preview(),
            KeyCode::Enter => self.start_trim_job(),
            _ => {}
        }
    }

    /// Starts or stops playback of only the selected clip range.
    fn toggle_preview(&mut self) {
        if self.preview.is_some() {
            self.stop_preview();
            return;
        }
        let Some(review) = &self.clip_review else {
            return;
        };
        let Some(file) = self.saved.get(review.save_index) else {
            return;
        };
        let Some(source) = &file.edit_source else {
            return;
        };
        let rate = f64::from(file.sample_rate);
        let start = Duration::from_secs_f64(
            (review.selection.start_frame() - source.start_frame) as f64 / rate,
        );
        let end = Duration::from_secs_f64(
            (review.selection.end_frame() - source.start_frame) as f64 / rate,
        );
        let was_paused = self.paused.swap(true, Ordering::Relaxed);
        self.state = CaptureState::Previewing;
        match PreviewPlayer::start(&file.path, start, end) {
            Ok(player) => {
                self.preview = Some(player);
                self.preview_was_paused = Some(was_paused);
                if let Some(review) = &mut self.clip_review {
                    review.error = None;
                }
            }
            Err(error) => {
                self.paused.store(was_paused, Ordering::Relaxed);
                self.state = if was_paused {
                    CaptureState::Paused
                } else {
                    CaptureState::Recording
                };
                if let Some(review) = &mut self.clip_review {
                    review.error = Some(format!("Preview failed: {error:#}"));
                }
            }
        }
    }

    /// Closes native playback and restores the previous capture pause state.
    fn stop_preview(&mut self) {
        if let Some(mut player) = self.preview.take() {
            player.stop();
        }
        if let Some(was_paused) = self.preview_was_paused.take() {
            self.paused.store(was_paused, Ordering::Relaxed);
            if !matches!(self.state, CaptureState::Finalizing) {
                self.state = if was_paused {
                    CaptureState::Paused
                } else {
                    CaptureState::Recording
                };
            }
        }
    }

    /// Detects the natural end of native preview playback.
    fn poll_preview(&mut self) {
        let Some(player) = &self.preview else {
            return;
        };
        match player.is_playing() {
            Ok(true) => {}
            Ok(false) => self.stop_preview(),
            Err(error) => {
                if let Some(review) = &mut self.clip_review {
                    review.error = Some(format!("Preview failed: {error:#}"));
                }
                self.stop_preview();
            }
        }
    }

    /// Starts a helper thread that rebuilds and replaces the selected MP3.
    fn start_trim_job(&mut self) {
        self.stop_preview();
        let Some(review) = &self.clip_review else {
            return;
        };
        let Some(file) = self.saved.get(review.save_index) else {
            return;
        };
        let Some(source) = file.edit_source.clone() else {
            return;
        };
        let start_frame = review.selection.start_frame();
        let end_frame = review.selection.end_frame();
        if start_frame == source.start_frame && end_frame == source.end_frame {
            self.notice = Some(format!("Kept {} without trimming", file.path.display()));
            self.clip_review = None;
            return;
        }
        let save_index = review.save_index;
        let mp3_path = file.path.clone();
        let sample_rate = file.sample_rate;
        let bitrate = file.bitrate;
        let worker = thread::Builder::new()
            .name("record-clip-trim".to_owned())
            .spawn(move || {
                trim_clip(
                    &source.pcm_path,
                    &mp3_path,
                    sample_rate,
                    bitrate,
                    start_frame,
                    end_frame,
                )
            });
        match worker {
            Ok(worker) => {
                self.trim_job = Some(TrimJob {
                    save_index,
                    start_frame,
                    end_frame,
                    worker,
                });
                if let Some(review) = &mut self.clip_review {
                    review.error = None;
                }
            }
            Err(error) => {
                if let Some(review) = &mut self.clip_review {
                    review.error = Some(format!("Could not start trim: {error}"));
                }
            }
        }
    }

    /// Applies a completed trim result to the saves panel and editor.
    fn poll_trim_job(&mut self) {
        if !self
            .trim_job
            .as_ref()
            .is_some_and(|job| job.worker.is_finished())
        {
            return;
        }
        let job = self.trim_job.take().expect("finished trim job exists");
        let result = job
            .worker
            .join()
            .map_err(|_| anyhow::anyhow!("the clip trim thread panicked"))
            .and_then(|result| result);
        match result {
            Ok(frames) => {
                if let Some(file) = self.saved.get_mut(job.save_index) {
                    file.frames = frames;
                    if let Some(source) = &mut file.edit_source {
                        source.start_frame = job.start_frame;
                        source.end_frame = job.end_frame;
                    }
                    self.notice = Some(format!("Trimmed {}", file.path.display()));
                }
                self.clip_review = None;
            }
            Err(error) => {
                if let Some(review) = &mut self.clip_review {
                    review.error = Some(format!("Trim failed: {error:#}"));
                }
            }
        }
    }

    /// Draws the live view and all active overlays.
    fn render(&mut self, frame: &mut Frame) {
        let area = frame.area();
        let saves_height = if self.clips_enabled { 5 } else { 0 };
        let layout = Layout::vertical([
            Constraint::Length(3),
            Constraint::Min(8),
            Constraint::Length(4),
            Constraint::Length(saves_height),
            Constraint::Length(3),
        ])
        .split(area);
        self.render_header(frame, layout[0]);
        self.render_visualization(frame, layout[1]);
        self.render_meters(frame, layout[2]);
        if self.clips_enabled {
            self.render_saves(frame, layout[3]);
        }
        self.render_footer(frame, layout[4]);
        if self.show_help {
            self.render_help(frame, centered(area, 66, 21));
        }
        if self.clip_prompt.is_some() {
            self.render_clip_prompt(frame, centered(area, 64, 8));
        }
        if self.clip_review.is_some() {
            let width = area.width.saturating_sub(4).min(110);
            let height = area.height.saturating_sub(2).min(26);
            self.render_clip_review(frame, centered(area, width, height));
        }
    }

    /// Draws capture state, elapsed time, format, and view mode.
    fn render_header(&self, frame: &mut Frame, area: Rect) {
        let elapsed = self.elapsed();
        let (status, color) = match self.state {
            CaptureState::Starting => ("● STARTING", AMBER),
            CaptureState::Recording => ("● RECORDING", RED),
            CaptureState::Paused => ("Ⅱ PAUSED", AMBER),
            CaptureState::Previewing => ("▶ PREVIEW", GREEN),
            CaptureState::Finalizing => ("◌ FINALIZING", AMBER),
        };
        let title = TextLine::from(vec![
            Span::styled(" record ", Style::default().fg(RED).bold()),
            Span::styled("SYSTEM AUDIO", Style::default().fg(MUTED)),
        ]);
        let header = Paragraph::new(TextLine::from(vec![
            Span::styled(status, Style::default().fg(color).bold()),
            Span::raw(format!("   {elapsed}   ")),
            Span::styled(
                format!(
                    "{} kbps · {} kHz · stereo",
                    self.bitrate / 1_000,
                    self.sample_rate / 1_000
                ),
                Style::default().fg(MUTED),
            ),
            Span::raw("   "),
            Span::styled(self.view.label(), Style::default().fg(PURPLE).bold()),
        ]))
        .block(
            Block::new()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(BORDER))
                .title(title),
        )
        .alignment(Alignment::Center);
        frame.render_widget(header, area);
    }

    /// Draws the active waveform, spectrum, or split arrangement.
    fn render_visualization(&self, frame: &mut Frame, area: Rect) {
        match self.view {
            ViewMode::Waveform => self.render_waveforms(frame, area),
            ViewMode::Spectrum => self.render_spectrum(frame, area),
            ViewMode::Split => {
                let split =
                    Layout::horizontal([Constraint::Percentage(64), Constraint::Percentage(36)])
                        .split(area);
                self.render_waveforms(frame, split[0]);
                self.render_spectrum(frame, split[1]);
            }
        }
    }

    /// Draws independent left and right live scopes.
    fn render_waveforms(&self, frame: &mut Frame, area: Rect) {
        let panels =
            Layout::vertical([Constraint::Percentage(50), Constraint::Percentage(50)]).split(area);
        let (left, right) = self.waveform.channels();
        render_waveform(frame, panels[0], " LEFT · 6 SECOND SCOPE ", left, CYAN);
        render_waveform(frame, panels[1], " RIGHT · 6 SECOND SCOPE ", right, GREEN);
    }

    /// Draws the lazy logarithmic frequency display.
    fn render_spectrum(&self, frame: &mut Frame, area: Rect) {
        let Some(spectrum) = &self.spectrum else {
            frame.render_widget(
                Paragraph::new("Spectrum initializes on first use")
                    .alignment(Alignment::Center)
                    .block(panel(" SPECTRUM ", PURPLE)),
                area,
            );
            return;
        };
        let width = area.width.saturating_sub(2).max(1) as usize;
        let bins = spectrum.bins();
        let data = (0..width)
            .map(|column| {
                let position = column as f32 / width as f32;
                let index = ((position * position) * (bins.len() - 1) as f32) as usize;
                (bins[index] * 100.0).round() as u64
            })
            .collect::<Vec<_>>();
        let spectrum = Sparkline::default()
            .block(panel(" SPECTRUM · 20 Hz to 24 kHz ", PURPLE))
            .data(&data)
            .max(100)
            .style(Style::default().fg(PURPLE))
            .bar_set(symbols::bar::NINE_LEVELS);
        frame.render_widget(spectrum, area);
    }

    /// Draws current left and right peak meters.
    fn render_meters(&self, frame: &mut Frame, area: Rect) {
        let rows = Layout::vertical([Constraint::Length(2), Constraint::Length(2)]).split(area);
        let (left, right) = self.waveform.peaks();
        frame.render_widget(level_gauge(" L ", left, CYAN), rows[0]);
        frame.render_widget(level_gauge(" R ", right, GREEN), rows[1]);
    }

    /// Draws a window around the selected finalized file.
    fn render_saves(&self, frame: &mut Frame, area: Rect) {
        let visible_rows = usize::from(area.height.saturating_sub(2)).max(1);
        let selected = self.selected_save.unwrap_or(0);
        let maximum_start = self.saved.len().saturating_sub(visible_rows);
        let start = selected
            .saturating_add(1)
            .saturating_sub(visible_rows)
            .min(maximum_start);
        let lines = if self.saved.is_empty() {
            vec![TextLine::from(Span::styled(
                " No files are finalized yet",
                Style::default().fg(MUTED),
            ))]
        } else {
            self.saved
                .iter()
                .enumerate()
                .skip(start)
                .take(visible_rows)
                .map(|(index, file)| {
                    let marker = if Some(index) == self.selected_save {
                        ">"
                    } else {
                        " "
                    };
                    let kind = match file.kind {
                        SavedFileKind::Recording => "FILE",
                        SavedFileKind::Part => "PART",
                        SavedFileKind::Clip => "CLIP",
                    };
                    let name = file
                        .path
                        .file_name()
                        .unwrap_or(file.path.as_os_str())
                        .to_string_lossy();
                    let style = if Some(index) == self.selected_save {
                        Style::default()
                            .fg(Color::Black)
                            .bg(Color::White)
                            .add_modifier(Modifier::BOLD)
                    } else {
                        Style::default().fg(MUTED)
                    };
                    TextLine::from(vec![
                        Span::styled(format!(" {marker} ✓ {kind:<4} "), style),
                        Span::styled(name.into_owned(), style),
                        Span::styled(
                            format!("   {:.1}s", file.duration().as_secs_f64()),
                            Style::default().fg(MUTED),
                        ),
                    ])
                })
                .collect()
        };
        let title = if let Some(notice) = &self.notice {
            format!(" SAVES · {notice} ")
        } else {
            " SAVES · SESSION ".to_owned()
        };
        frame.render_widget(Paragraph::new(lines).block(panel(&title, GREEN)), area);
    }

    /// Draws controls and the compact session path.
    fn render_footer(&self, frame: &mut Frame, area: Rect) {
        let output = compact_path(&self.output, area.width.saturating_sub(4) as usize);
        let compact = area.width < 96;
        let mut controls = vec![
            Span::styled(" Ctrl+C / S ", Style::default().fg(RED).bold()),
            Span::styled(
                if compact { "stop   " } else { "save & stop   " },
                Style::default().fg(MUTED),
            ),
            Span::styled(" Space ", Style::default().fg(AMBER).bold()),
            Span::styled("pause   ", Style::default().fg(MUTED)),
            Span::styled(" W ", Style::default().fg(PURPLE).bold()),
            Span::styled("view   ", Style::default().fg(MUTED)),
        ];
        if self.clips_enabled {
            controls.extend([
                Span::styled(" C ", Style::default().fg(GREEN).bold()),
                Span::styled("clip   ", Style::default().fg(MUTED)),
                Span::styled(" E ", Style::default().fg(PURPLE).bold()),
                Span::styled("edit   ", Style::default().fg(MUTED)),
            ]);
        }
        controls.extend([
            Span::styled(" ? ", Style::default().fg(CYAN).bold()),
            Span::styled("help", Style::default().fg(MUTED)),
        ]);
        let footer = Paragraph::new(vec![
            TextLine::from(controls),
            TextLine::from(vec![
                Span::styled(
                    if self.clips_enabled {
                        " SESSION  "
                    } else {
                        " OUTPUT   "
                    },
                    Style::default().fg(MUTED),
                ),
                Span::styled(output, Style::default().fg(Color::White)),
            ]),
        ])
        .block(
            Block::new()
                .borders(Borders::TOP)
                .border_style(Style::default().fg(BORDER)),
        );
        frame.render_widget(footer, area);
    }

    /// Draws the recorder help overlay.
    fn render_help(&self, frame: &mut Frame, area: Rect) {
        let help = Paragraph::new(vec![
            TextLine::from("record starts audio capture immediately. No menu is necessary."),
            TextLine::from(""),
            help_line("Ctrl+C / S / Q / Esc", "finalize the MP3 and exit"),
            help_line("Space", "pause or resume (paused time is omitted)"),
            help_line("W", "cycle waveform, spectrum, and split views"),
            help_line("C", "name and finalize the current session part"),
            help_line("Up / Down", "select a finalized session file"),
            help_line("E", "review and trim the selected named clip"),
            help_line("? / Esc", "close this help"),
            TextLine::from(""),
            TextLine::from(Span::styled(
                "Captures every ordinary app routed to the default Windows output.",
                Style::default().fg(MUTED),
            )),
        ])
        .wrap(Wrap { trim: true })
        .block(
            Block::new()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(PURPLE))
                .padding(Padding::uniform(1))
                .title(TextLine::from(" QUICK HELP ").fg(PURPLE).bold()),
        );
        frame.render_widget(Clear, area);
        frame.render_widget(help, area);
    }

    /// Draws the clip-name prompt and current validation message.
    fn render_clip_prompt(&self, frame: &mut Frame, area: Rect) {
        let Some(prompt) = &self.clip_prompt else {
            return;
        };
        let error = prompt
            .error
            .as_deref()
            .unwrap_or("Enter saves this part as a clip. Capture continues in a new part.");
        let error_color = if prompt.error.is_some() { RED } else { MUTED };
        let input = if prompt.input.is_empty() {
            Span::styled("clip-name", Style::default().fg(MUTED))
        } else {
            Span::styled(
                prompt.input.clone(),
                Style::default().fg(Color::White).bold(),
            )
        };
        let prompt = Paragraph::new(vec![
            TextLine::from(vec![
                Span::styled(" File name  ", Style::default().fg(CYAN)),
                input,
            ]),
            TextLine::from(""),
            TextLine::from(Span::styled(error, Style::default().fg(error_color))),
            TextLine::from(Span::styled(
                "Enter save   Esc cancel",
                Style::default().fg(MUTED),
            )),
        ])
        .block(
            Block::new()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(GREEN))
                .padding(Padding::horizontal(1))
                .title(TextLine::from(" SAVE CLIP ").fg(GREEN).bold()),
        );
        frame.render_widget(Clear, area);
        frame.render_widget(prompt, area);
    }

    /// Draws complete clip channels, handles, times, and edit controls.
    fn render_clip_review(&self, frame: &mut Frame, area: Rect) {
        let Some(review) = &self.clip_review else {
            return;
        };
        let Some(file) = self.saved.get(review.save_index) else {
            return;
        };
        let name = file
            .path
            .file_name()
            .unwrap_or(file.path.as_os_str())
            .to_string_lossy();
        let outer = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(PURPLE))
            .title(
                TextLine::from(format!(" CLIP REVIEW · {name} "))
                    .fg(PURPLE)
                    .bold(),
            );
        let inner = outer.inner(area);
        frame.render_widget(Clear, area);
        frame.render_widget(outer, area);

        let rows = Layout::vertical([
            Constraint::Length(3),
            Constraint::Min(5),
            Constraint::Min(5),
            Constraint::Length(2),
            Constraint::Length(2),
        ])
        .split(inner);
        let active = match review.selection.active_handle() {
            SelectionHandle::Start => "START",
            SelectionHandle::End => "END",
        };
        let status = if self.trim_job.is_some() {
            Span::styled("  ◌ WRITING TRIMMED MP3", Style::default().fg(AMBER).bold())
        } else if self.preview.is_some() {
            Span::styled("  ▶ PLAYING SELECTION", Style::default().fg(GREEN).bold())
        } else {
            Span::raw("")
        };
        frame.render_widget(
            Paragraph::new(vec![
                TextLine::from(vec![
                    Span::styled(" START ", Style::default().fg(CYAN).bold()),
                    Span::raw(format_clip_time(
                        review.selection.start_frame() - review.selection.source_start_frame(),
                        review.selection.sample_rate(),
                    )),
                    Span::styled("   END ", Style::default().fg(GREEN).bold()),
                    Span::raw(format_clip_time(
                        review.selection.end_frame() - review.selection.source_start_frame(),
                        review.selection.sample_rate(),
                    )),
                    Span::styled("   LENGTH ", Style::default().fg(PURPLE).bold()),
                    Span::raw(format!("{:.2}s", review.selection.duration().as_secs_f64())),
                    status,
                ]),
                TextLine::from(vec![
                    Span::styled(" ACTIVE HANDLE  ", Style::default().fg(MUTED)),
                    Span::styled(active, Style::default().fg(AMBER).bold()),
                ]),
            ]),
            rows[0],
        );
        render_clip_waveform(
            frame,
            rows[1],
            " LEFT · CLIP ",
            &review.waveform.left,
            &review.selection,
            CYAN,
        );
        render_clip_waveform(
            frame,
            rows[2],
            " RIGHT · CLIP ",
            &review.waveform.right,
            &review.selection,
            GREEN,
        );
        let message = review
            .error
            .as_deref()
            .unwrap_or("Tab handle   ←/→ 0.1s   Shift+←/→ 1s   Ctrl+←/→ 0.01s");
        frame.render_widget(
            Paragraph::new(message).style(Style::default().fg(if review.error.is_some() {
                RED
            } else {
                MUTED
            })),
            rows[3],
        );
        frame.render_widget(
            Paragraph::new(TextLine::from(vec![
                Span::styled(" P ", Style::default().fg(GREEN).bold()),
                Span::styled("preview   ", Style::default().fg(MUTED)),
                Span::styled(" R ", Style::default().fg(CYAN).bold()),
                Span::styled("reset   ", Style::default().fg(MUTED)),
                Span::styled(" Enter ", Style::default().fg(PURPLE).bold()),
                Span::styled("save trim   ", Style::default().fg(MUTED)),
                Span::styled(" Esc ", Style::default().fg(RED).bold()),
                Span::styled("keep original", Style::default().fg(MUTED)),
            ])),
            rows[4],
        );
    }

    /// Formats total encoded frames as an hours, minutes, and seconds clock.
    fn elapsed(&self) -> String {
        let duration = if self.sample_rate == 0 {
            Duration::ZERO
        } else {
            Duration::from_secs_f64(self.encoded_frames as f64 / f64::from(self.sample_rate))
        };
        format_clock(duration)
    }
}

/// Owns all terminal state for one screen-capture process.
struct VideoApp {
    /// Contains the capture state that the header shows.
    state: CaptureState,
    /// Requests final capture shutdown.
    stop: Arc<AtomicBool>,
    /// Omits capture packets while true.
    paused: Arc<AtomicBool>,
    /// Contains the destination MP4 path.
    output: PathBuf,
    /// Contains short backend feedback for the status panel.
    notice: Option<String>,
    /// Reports whether the help overlay is open.
    show_help: bool,
    /// Contains the encoded frame width once capture starts.
    width: u32,
    /// Contains the encoded frame height once capture starts.
    height: u32,
    /// Contains the encoded frame rate once capture starts.
    fps: u32,
    /// Contains the AAC sample rate once capture starts.
    sample_rate: u32,
    /// Contains milliseconds from start to capture readiness.
    capture_ready_ms: Option<f64>,
    /// Contains recorded media time, excluding pauses.
    recorded: Duration,
    /// Contains the previous loop instant for elapsed accumulation.
    tick: Instant,
}

impl VideoApp {
    /// Creates the initial state before the first backend event arrives.
    fn new(stop: Arc<AtomicBool>, paused: Arc<AtomicBool>, output: PathBuf) -> Self {
        Self {
            state: CaptureState::Starting,
            stop,
            paused,
            output,
            notice: None,
            show_help: false,
            width: 0,
            height: 0,
            fps: 0,
            sample_rate: 48_000,
            capture_ready_ms: None,
            recorded: Duration::ZERO,
            tick: Instant::now(),
        }
    }

    /// Draws frames and handles input until the video worker stops.
    fn run(
        &mut self,
        terminal: &mut DefaultTerminal,
        events: &Receiver<VideoEvent>,
        worker: &JoinHandle<Result<VideoSummary>>,
    ) -> Result<()> {
        loop {
            self.advance_recorded_time();
            self.drain_video_events(events);
            if worker.is_finished() {
                break;
            }
            terminal.draw(|frame| self.render(frame))?;
            if event::poll(Duration::from_millis(33))? {
                self.handle_event(event::read()?);
            }
            if self.stop.load(Ordering::Relaxed) {
                self.state = CaptureState::Finalizing;
            }
        }
        self.drain_video_events(events);
        terminal.draw(|frame| self.render(frame))?;
        Ok(())
    }

    /// Adds wall time to the clock only while actively recording.
    fn advance_recorded_time(&mut self) {
        let now = Instant::now();
        let delta = now - self.tick;
        self.tick = now;
        if matches!(self.state, CaptureState::Recording | CaptureState::Previewing)
            && !self.paused.load(Ordering::Relaxed)
        {
            self.recorded += delta;
        }
    }

    /// Applies all pending backend events without waiting for another event.
    fn drain_video_events(&mut self, events: &Receiver<VideoEvent>) {
        for event in events.try_iter() {
            match event {
                VideoEvent::Started {
                    width,
                    height,
                    fps,
                    sample_rate,
                    capture_ready_ms,
                } => {
                    self.width = width;
                    self.height = height;
                    self.fps = fps;
                    self.sample_rate = sample_rate;
                    self.capture_ready_ms = Some(capture_ready_ms);
                    self.state = CaptureState::Recording;
                }
                VideoEvent::Notice(message) => self.notice = Some(message),
                VideoEvent::Finalizing => self.state = CaptureState::Finalizing,
                VideoEvent::Saved(_) => {}
            }
        }
    }

    /// Routes one terminal event to recorder controls or overlays.
    fn handle_event(&mut self, event: Event) {
        let Event::Key(key) = event else {
            return;
        };
        if !matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
            return;
        }
        if matches!(key.code, KeyCode::Char('c' | 'C'))
            && key.modifiers.contains(KeyModifiers::CONTROL)
        {
            self.request_stop();
            return;
        }
        if self.show_help {
            if matches!(key.code, KeyCode::Esc | KeyCode::Char('?')) {
                self.show_help = false;
            }
            return;
        }
        match key.code {
            KeyCode::Char('s' | 'S' | 'q' | 'Q') | KeyCode::Esc => self.request_stop(),
            KeyCode::Char(' ')
                if matches!(self.state, CaptureState::Recording | CaptureState::Paused) =>
            {
                let paused = !self.paused.load(Ordering::Relaxed);
                self.paused.store(paused, Ordering::Relaxed);
                self.state = if paused {
                    CaptureState::Paused
                } else {
                    CaptureState::Recording
                };
            }
            KeyCode::Char('?') => self.show_help = true,
            _ => {}
        }
    }

    /// Requests MP4 finalization and exits the live view.
    fn request_stop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        self.state = CaptureState::Finalizing;
    }

    /// Draws the header, live status panel, footer, and any overlay.
    fn render(&mut self, frame: &mut Frame) {
        let area = frame.area();
        let layout = Layout::vertical([
            Constraint::Length(3),
            Constraint::Min(6),
            Constraint::Length(3),
        ])
        .split(area);
        self.render_header(frame, layout[0]);
        self.render_status(frame, layout[1]);
        self.render_footer(frame, layout[2]);
        if self.show_help {
            self.render_help(frame, centered(area, 62, 12));
        }
    }

    /// Draws capture state, elapsed time, and the encoded format.
    fn render_header(&self, frame: &mut Frame, area: Rect) {
        let (status, color) = match self.state {
            CaptureState::Starting => ("● STARTING", AMBER),
            CaptureState::Recording => ("● RECORDING", RED),
            CaptureState::Paused => ("Ⅱ PAUSED", AMBER),
            CaptureState::Previewing => ("▶ RECORDING", RED),
            CaptureState::Finalizing => ("◌ FINALIZING", AMBER),
        };
        let title = TextLine::from(vec![
            Span::styled(" record ", Style::default().fg(RED).bold()),
            Span::styled("SCREEN CAPTURE", Style::default().fg(MUTED)),
        ]);
        let geometry = if self.width > 0 {
            format!("{}x{} @ {} fps", self.width, self.height, self.fps)
        } else {
            "preparing encoder".to_owned()
        };
        let header = Paragraph::new(TextLine::from(vec![
            Span::styled(status, Style::default().fg(color).bold()),
            Span::raw(format!("   {}   ", format_clock(self.recorded))),
            Span::styled(
                format!("{geometry} · H.264 + AAC"),
                Style::default().fg(MUTED),
            ),
        ]))
        .block(
            Block::new()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(BORDER))
                .title(title),
        )
        .alignment(Alignment::Center);
        frame.render_widget(header, area);
    }

    /// Draws the centered live panel with timing and backend feedback.
    fn render_status(&self, frame: &mut Frame, area: Rect) {
        let mut lines = vec![
            TextLine::from(Span::styled(
                format_clock(self.recorded),
                Style::default().fg(Color::White).bold(),
            )),
            TextLine::from(""),
            TextLine::from(vec![
                Span::styled(" OUTPUT   ", Style::default().fg(MUTED)),
                Span::styled(
                    compact_path(&self.output, usize::from(area.width.saturating_sub(12)).max(8)),
                    Style::default().fg(Color::White),
                ),
            ]),
            TextLine::from(vec![
                Span::styled(" AUDIO    ", Style::default().fg(MUTED)),
                Span::raw(format!(
                    "{} kHz stereo AAC",
                    self.sample_rate / 1_000
                )),
            ]),
        ];
        if let Some(ready) = self.capture_ready_ms {
            lines.push(TextLine::from(vec![
                Span::styled(" READY IN ", Style::default().fg(MUTED)),
                Span::raw(format!("{ready:.0} ms")),
            ]));
        }
        if let Some(notice) = &self.notice {
            lines.push(TextLine::from(""));
            lines.push(TextLine::from(Span::styled(
                notice.clone(),
                Style::default().fg(AMBER),
            )));
        }
        let panel_block = panel(" LIVE ", PURPLE);
        frame.render_widget(
            Paragraph::new(lines)
                .alignment(Alignment::Center)
                .block(panel_block),
            area,
        );
    }

    /// Draws controls and the compact output path.
    fn render_footer(&self, frame: &mut Frame, area: Rect) {
        let compact = area.width < 96;
        let footer = Paragraph::new(vec![TextLine::from(vec![
            Span::styled(" Ctrl+C / S ", Style::default().fg(RED).bold()),
            Span::styled(
                if compact { "stop   " } else { "save & stop   " },
                Style::default().fg(MUTED),
            ),
            Span::styled(" Space ", Style::default().fg(AMBER).bold()),
            Span::styled("pause   ", Style::default().fg(MUTED)),
            Span::styled(" ? ", Style::default().fg(CYAN).bold()),
            Span::styled("help", Style::default().fg(MUTED)),
        ])])
        .block(
            Block::new()
                .borders(Borders::TOP)
                .border_style(Style::default().fg(BORDER)),
        );
        frame.render_widget(footer, area);
    }

    /// Draws the screen-capture help overlay.
    fn render_help(&self, frame: &mut Frame, area: Rect) {
        let help = Paragraph::new(vec![
            TextLine::from("Screen capture records the primary monitor and system audio."),
            TextLine::from(""),
            help_line("Ctrl+C / S / Q / Esc", "finalize the MP4 and exit"),
            help_line("Space", "pause or resume (paused time is omitted)"),
            help_line("? / Esc", "close this help"),
            TextLine::from(""),
            TextLine::from(Span::styled(
                "The MP4 finalizes on exit; keep the terminal open until it saves.",
                Style::default().fg(MUTED),
            )),
        ])
        .wrap(Wrap { trim: true })
        .block(
            Block::new()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(PURPLE))
                .padding(Padding::uniform(1))
                .title(TextLine::from(" SCREEN HELP ").fg(PURPLE).bold()),
        );
        frame.render_widget(Clear, area);
        frame.render_widget(help, area);
    }
}

/// Draws one live channel as a braille line scope.
fn render_waveform(
    frame: &mut Frame,
    area: Rect,
    title: &str,
    samples: &VecDeque<f32>,
    color: Color,
) {
    let maximum_points = usize::from(area.width.max(2)) * 3;
    let points = waveform_points(samples, maximum_points);
    let x_max = points.last().map_or(1.0, |point| point.0.max(1.0));
    let canvas = Canvas::default()
        .block(panel(title, color))
        .marker(Marker::Braille)
        .x_bounds([0.0, x_max])
        .y_bounds([-1.0, 1.0])
        .paint(|context| {
            context.draw(&CanvasLine::new(0.0, 0.0, x_max, 0.0, BORDER));
            for window in points.windows(2) {
                context.draw(&CanvasLine::new(
                    window[0].0,
                    window[0].1,
                    window[1].0,
                    window[1].1,
                    color,
                ));
            }
        });
    frame.render_widget(canvas, area);
}

/// Draws one full clip channel with its start and end handles.
fn render_clip_waveform(
    frame: &mut Frame,
    area: Rect,
    title: &str,
    bins: &[WaveformBin],
    selection: &ClipSelection,
    color: Color,
) {
    if bins.is_empty() {
        frame.render_widget(
            Paragraph::new("No waveform data").block(panel(title, color)),
            area,
        );
        return;
    }
    let x_max = (bins.len().saturating_sub(1) as f64).max(1.0);
    let selection_start = selection.normalized_start() * x_max;
    let selection_end = selection.normalized_end() * x_max;
    let start_color = if selection.active_handle() == SelectionHandle::Start {
        AMBER
    } else {
        BORDER
    };
    let end_color = if selection.active_handle() == SelectionHandle::End {
        AMBER
    } else {
        BORDER
    };
    let canvas = Canvas::default()
        .block(panel(title, color))
        .marker(Marker::Braille)
        .x_bounds([0.0, x_max])
        .y_bounds([-1.0, 1.0])
        .paint(|context| {
            context.draw(&CanvasLine::new(0.0, 0.0, x_max, 0.0, BORDER));
            for (index, bin) in bins.iter().enumerate() {
                let x = if bins.len() == 1 {
                    0.0
                } else {
                    index as f64 / (bins.len() - 1) as f64 * x_max
                };
                let bin_color = if x >= selection_start && x <= selection_end {
                    color
                } else {
                    MUTED
                };
                context.draw(&CanvasLine::new(
                    x,
                    f64::from(bin.minimum.clamp(-1.0, 1.0)),
                    x,
                    f64::from(bin.maximum.clamp(-1.0, 1.0)),
                    bin_color,
                ));
            }
            context.draw(&CanvasLine::new(
                selection_start,
                -1.0,
                selection_start,
                1.0,
                start_color,
            ));
            context.draw(&CanvasLine::new(
                selection_end,
                -1.0,
                selection_end,
                1.0,
                end_color,
            ));
        });
    frame.render_widget(canvas, area);
}

/// Formats an absolute frame count with millisecond resolution.
fn format_clip_time(frames: u64, sample_rate: u32) -> String {
    if sample_rate == 0 {
        return "00:00.000".to_owned();
    }
    let milliseconds =
        u64::try_from(u128::from(frames).saturating_mul(1_000) / u128::from(sample_rate))
            .unwrap_or(u64::MAX);
    let hours = milliseconds / 3_600_000;
    let minutes = milliseconds / 60_000 % 60;
    let seconds = milliseconds / 1_000 % 60;
    let remainder = milliseconds % 1_000;
    if hours > 0 {
        format!("{hours:02}:{minutes:02}:{seconds:02}.{remainder:03}")
    } else {
        format!("{minutes:02}:{seconds:02}.{remainder:03}")
    }
}

/// Reduces a rolling sample history to a bounded canvas point list.
fn waveform_points(samples: &VecDeque<f32>, maximum: usize) -> Vec<(f64, f64)> {
    if samples.is_empty() {
        return vec![(0.0, 0.0), (1.0, 0.0)];
    }
    let step = samples.len().div_ceil(maximum.max(2));
    samples
        .iter()
        .step_by(step)
        .enumerate()
        .map(|(index, sample)| (index as f64, f64::from(*sample)))
        .collect()
}

/// Formats a duration as an hours, minutes, and seconds clock.
fn format_clock(duration: Duration) -> String {
    let total = duration.as_secs();
    format!(
        "{:02}:{:02}:{:02}",
        total / 3_600,
        total / 60 % 60,
        total % 60
    )
}

/// Creates the common rounded panel style.
fn panel(title: &str, color: Color) -> Block<'_> {
    Block::new()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(BORDER))
        .title(TextLine::from(title).fg(color).bold())
}

/// Creates a peak gauge with a floor of minus 60 dB.
fn level_gauge(label: &'static str, peak: f32, color: Color) -> LineGauge<'static> {
    let db = if peak < 0.001 {
        -60.0
    } else {
        (20.0 * peak.log10()).max(-60.0)
    };
    LineGauge::default()
        .block(
            Block::new()
                .borders(Borders::LEFT | Borders::RIGHT)
                .border_style(BORDER),
        )
        .label(format!("{label}{db:>4.0} dB"))
        .ratio(f64::from(peak.clamp(0.0, 1.0)))
        .filled_style(Style::default().fg(color).add_modifier(Modifier::BOLD))
        .unfilled_style(Style::default().fg(BORDER))
        .filled_symbol(symbols::line::THICK_HORIZONTAL)
        .unfilled_symbol(symbols::line::HORIZONTAL)
}

/// Creates one aligned key row for the help overlay.
fn help_line(key: &'static str, description: &'static str) -> TextLine<'static> {
    TextLine::from(vec![
        Span::styled(format!("{key:<24}"), Style::default().fg(CYAN).bold()),
        Span::raw(description),
    ])
}

/// Centers a rectangle and clamps it to the terminal area.
fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let horizontal = Layout::horizontal([Constraint::Length(width.min(area.width))])
        .flex(Flex::Center)
        .split(area)[0];
    Layout::vertical([Constraint::Length(height.min(area.height))])
        .flex(Flex::Center)
        .split(horizontal)[0]
}

/// Keeps the end of a path within a display-width character limit.
fn compact_path(path: &Path, maximum: usize) -> String {
    if maximum == 0 {
        return String::new();
    }
    let text = path.display().to_string();
    if text.chars().count() <= maximum {
        return text;
    }
    if maximum <= 3 {
        return ".".repeat(maximum);
    }
    let tail = text
        .chars()
        .rev()
        .take(maximum.saturating_sub(3))
        .collect::<String>();
    format!("...{}", tail.chars().rev().collect::<String>())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossbeam_channel::bounded;
    use ratatui::{Terminal, backend::TestBackend};

    fn session_app() -> (App, Receiver<AudioCommand>) {
        let (commands, receiver) = bounded(8);
        let app = App::new(
            Arc::new(AtomicBool::new(false)),
            Arc::new(AtomicBool::new(false)),
            commands,
            OutputTarget::Session {
                directory: PathBuf::from("recording-session"),
                segment_duration: Duration::from_secs(600),
            },
        );
        (app, receiver)
    }

    fn press(app: &mut App, code: KeyCode, modifiers: KeyModifiers) {
        app.handle_event(Event::Key(KeyEvent::new(code, modifiers)));
    }

    fn saved_file(name: &str, kind: SavedFileKind) -> SavedFile {
        SavedFile {
            path: PathBuf::from(name),
            kind,
            sample_rate: 48_000,
            bitrate: 320_000,
            frames: 48_000,
            edit_source: None,
        }
    }

    #[test]
    fn split_is_the_default_view() {
        let (app, _commands) = session_app();
        assert!(matches!(app.view, ViewMode::Split));
        assert!(app.spectrum.is_none());
    }

    #[test]
    fn view_key_cycles_all_three_modes() {
        let (mut app, _commands) = session_app();
        press(&mut app, KeyCode::Char('w'), KeyModifiers::NONE);
        assert!(matches!(app.view, ViewMode::Waveform));
        press(&mut app, KeyCode::Char('w'), KeyModifiers::NONE);
        assert!(matches!(app.view, ViewMode::Spectrum));
        assert!(app.spectrum.is_some());
        press(&mut app, KeyCode::Char('w'), KeyModifiers::NONE);
        assert!(matches!(app.view, ViewMode::Split));
    }

    #[test]
    fn clip_prompt_validates_and_sends_a_trimmed_name() {
        let (mut app, commands) = session_app();
        app.state = CaptureState::Recording;
        press(&mut app, KeyCode::Char('c'), KeyModifiers::NONE);
        for character in "  demo clip.mp3  ".chars() {
            press(&mut app, KeyCode::Char(character), KeyModifiers::NONE);
        }
        press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(
            commands.try_recv().unwrap(),
            AudioCommand::SaveClip("demo clip".to_owned())
        );
        assert!(app.clip_prompt.is_none());
    }

    #[test]
    fn invalid_clip_name_keeps_the_prompt_open() {
        let (mut app, commands) = session_app();
        app.state = CaptureState::Recording;
        press(&mut app, KeyCode::Char('c'), KeyModifiers::NONE);
        for character in "NUL".chars() {
            press(&mut app, KeyCode::Char(character), KeyModifiers::NONE);
        }
        press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        assert!(commands.try_recv().is_err());
        assert!(
            app.clip_prompt
                .as_ref()
                .and_then(|prompt| prompt.error.as_ref())
                .is_some()
        );
    }

    #[test]
    fn explicit_output_disables_the_clip_prompt() {
        let (commands, _receiver) = bounded(1);
        let mut app = App::new(
            Arc::new(AtomicBool::new(false)),
            Arc::new(AtomicBool::new(false)),
            commands,
            OutputTarget::SingleFile {
                path: PathBuf::from("take.mp3"),
                replace: false,
            },
        );
        app.state = CaptureState::Recording;
        press(&mut app, KeyCode::Char('c'), KeyModifiers::NONE);
        assert!(app.clip_prompt.is_none());
    }

    #[test]
    fn control_shift_c_stops_capture_from_any_modal() {
        let (mut app, _commands) = session_app();
        app.state = CaptureState::Recording;
        app.clip_prompt = Some(ClipPrompt::default());
        press(
            &mut app,
            KeyCode::Char('C'),
            KeyModifiers::CONTROL | KeyModifiers::SHIFT,
        );
        assert!(app.stop.load(Ordering::Relaxed));
        assert!(matches!(app.state, CaptureState::Finalizing));
    }

    #[test]
    fn audio_events_update_state_samples_and_saved_selection() {
        let (mut app, _commands) = session_app();
        let (sender, receiver) = bounded(8);
        sender
            .send(AudioEvent::Started {
                sample_rate: 44_100,
                bitrate: 192_000,
                channels: 2,
            })
            .unwrap();
        sender
            .send(AudioEvent::Samples {
                left: vec![0.5],
                right: vec![-0.25],
                encoded_frames: 44_100,
            })
            .unwrap();
        sender
            .send(AudioEvent::Saved(saved_file(
                "part-001.mp3",
                SavedFileKind::Part,
            )))
            .unwrap();
        sender
            .send(AudioEvent::Notice("backend notice".to_owned()))
            .unwrap();
        app.drain_audio_events(&receiver);
        assert!(matches!(app.state, CaptureState::Recording));
        assert_eq!(app.sample_rate, 44_100);
        assert_eq!(app.bitrate, 192_000);
        assert_eq!(app.encoded_frames, 44_100);
        assert_eq!(app.saved.len(), 1);
        assert_eq!(app.selected_save, Some(0));
        assert_eq!(app.notice.as_deref(), Some("backend notice"));
        assert!(app.spectrum.is_some());
    }

    #[test]
    fn session_view_renders_in_a_standard_terminal() {
        let (mut app, _commands) = session_app();
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| app.render(frame)).unwrap();
        let content = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(content.contains("SPLIT"));
        assert!(content.contains("SAVES"));
        assert!(content.contains("Ctrl+C"));
    }

    #[test]
    fn compacts_from_the_front_to_keep_the_filename() {
        let result = compact_path(Path::new("C:/very/long/path/recording.mp3"), 18);
        assert!(result.starts_with("..."));
        assert!(result.ends_with("recording.mp3"));
    }

    #[test]
    fn compact_path_never_exceeds_tiny_limits() {
        for maximum in 0..=3 {
            assert!(compact_path(Path::new("recording.mp3"), maximum).len() <= maximum);
        }
    }

    #[test]
    fn clip_time_formats_minutes_hours_and_zero_rate() {
        assert_eq!(format_clip_time(0, 0), "00:00.000");
        assert_eq!(format_clip_time(90_061, 1_000), "01:30.061");
        assert_eq!(format_clip_time(3_661_001, 1_000), "01:01:01.001");
    }

    #[test]
    fn waveform_downsamples_to_a_bounded_size() {
        let samples = (0..10_000)
            .map(|index| index as f32)
            .collect::<VecDeque<_>>();
        assert!(waveform_points(&samples, 100).len() <= 100);
    }

    #[test]
    fn empty_waveform_has_a_visible_zero_line() {
        assert_eq!(
            waveform_points(&VecDeque::new(), 0),
            vec![(0.0, 0.0), (1.0, 0.0)]
        );
    }

    fn video_app() -> VideoApp {
        VideoApp::new(
            Arc::new(AtomicBool::new(false)),
            Arc::new(AtomicBool::new(false)),
            PathBuf::from("recording-20260821-005218.mp4"),
        )
    }

    fn press_video(app: &mut VideoApp, code: KeyCode, modifiers: KeyModifiers) {
        app.handle_event(Event::Key(KeyEvent::new(code, modifiers)));
    }

    fn start_recording(app: &mut VideoApp) {
        let (sender, receiver) = bounded(8);
        sender
            .send(VideoEvent::Started {
                width: 2560,
                height: 1440,
                fps: 60,
                sample_rate: 48_000,
                capture_ready_ms: 148.0,
            })
            .unwrap();
        app.drain_video_events(&receiver);
    }

    #[test]
    fn video_started_event_activates_recording_state() {
        let mut app = video_app();
        assert_eq!(app.width, 0);
        start_recording(&mut app);
        assert!(matches!(app.state, CaptureState::Recording));
        assert_eq!(app.width, 2560);
        assert_eq!(app.height, 1440);
        assert_eq!(app.fps, 60);
        assert_eq!(app.sample_rate, 48_000);
        assert_eq!(app.capture_ready_ms, Some(148.0));
    }

    #[test]
    fn video_notice_and_finalizing_events_update_state() {
        let mut app = video_app();
        let (sender, receiver) = bounded(8);
        sender.send(VideoEvent::Notice("duplication reset".to_owned())).unwrap();
        sender.send(VideoEvent::Finalizing).unwrap();
        app.drain_video_events(&receiver);
        assert_eq!(app.notice.as_deref(), Some("duplication reset"));
        assert!(matches!(app.state, CaptureState::Finalizing));
    }

    #[test]
    fn video_space_toggles_pause() {
        let mut app = video_app();
        start_recording(&mut app);
        press_video(&mut app, KeyCode::Char(' '), KeyModifiers::NONE);
        assert!(app.paused.load(Ordering::Relaxed));
        assert!(matches!(app.state, CaptureState::Paused));
        press_video(&mut app, KeyCode::Char(' '), KeyModifiers::NONE);
        assert!(!app.paused.load(Ordering::Relaxed));
        assert!(matches!(app.state, CaptureState::Recording));
    }

    #[test]
    fn video_clock_freezes_while_paused() {
        let mut app = video_app();
        start_recording(&mut app);
        app.tick -= Duration::from_secs(5);
        app.advance_recorded_time();
        assert!(app.recorded >= Duration::from_secs(4));
        let frozen = app.recorded;
        press_video(&mut app, KeyCode::Char(' '), KeyModifiers::NONE);
        app.tick -= Duration::from_secs(10);
        app.advance_recorded_time();
        assert_eq!(app.recorded, frozen);
    }

    #[test]
    fn video_stop_keys_finalize_capture() {
        let mut app = video_app();
        start_recording(&mut app);
        press_video(&mut app, KeyCode::Char('s'), KeyModifiers::NONE);
        assert!(app.stop.load(Ordering::Relaxed));
        assert!(matches!(app.state, CaptureState::Finalizing));

        let mut app = video_app();
        press_video(
            &mut app,
            KeyCode::Char('C'),
            KeyModifiers::CONTROL | KeyModifiers::SHIFT,
        );
        assert!(app.stop.load(Ordering::Relaxed));

        let mut app = video_app();
        press_video(&mut app, KeyCode::Esc, KeyModifiers::NONE);
        assert!(app.stop.load(Ordering::Relaxed));
    }

    #[test]
    fn video_help_overlay_opens_and_closes() {
        let mut app = video_app();
        press_video(&mut app, KeyCode::Char('?'), KeyModifiers::NONE);
        assert!(app.show_help);
        press_video(&mut app, KeyCode::Esc, KeyModifiers::NONE);
        assert!(!app.show_help);
    }

    #[test]
    fn video_view_renders_the_screen_capture_ui() {
        let mut app = video_app();
        start_recording(&mut app);
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| app.render(frame)).unwrap();
        let content = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(content.contains("SCREEN CAPTURE"));
        assert!(content.contains("RECORDING"));
        assert!(content.contains("2560x1440 @ 60 fps"));
        assert!(content.contains("Space"));
        assert!(content.contains(".mp4"));
    }

    #[test]
    fn video_view_before_start_shows_the_preparing_state() {
        let mut app = video_app();
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| app.render(frame)).unwrap();
        let content = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(content.contains("STARTING"));
        assert!(content.contains("preparing encoder"));
    }

    #[test]
    fn clock_formats_hours_minutes_and_seconds() {
        assert_eq!(format_clock(Duration::ZERO), "00:00:00");
        assert_eq!(format_clock(Duration::from_secs(61)), "00:01:01");
        assert_eq!(format_clock(Duration::from_secs(3_661)), "01:01:01");
    }
}
