//! System-audio terminal interface, session file list, and MP3 clip editor.

mod render;

use std::{
    collections::VecDeque,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::Duration,
};

use anyhow::Result;
use crossbeam_channel::{Receiver, Sender, TrySendError};
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::DefaultTerminal;

use crate::{
    audio::{AudioCommand, AudioEvent, RecordingSummary, SavedFile, SavedFileKind},
    clip::{ClipSelection, ClipWaveform, PreviewPlayer, load_pcm_waveform, trim_clip},
    session::{OutputTarget, clip_stem},
    spectrum::Spectrum,
    waveform::Waveform,
};

use super::CaptureState;

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

/// Runs the full-screen interface until the audio worker and any trim job stop.
pub(super) fn run(
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
}

#[cfg(test)]
mod tests;
