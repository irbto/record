//! Screen-capture terminal interface and integrated MP4 clip editor.

mod audio_preview;
mod editing;
mod render;
mod tools;
mod waveform;
mod workers;

use std::{
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use anyhow::Result;
use crossbeam_channel::{Receiver, Sender, TrySendError};
use crossterm::{
    ExecutableCommand,
    event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers},
    terminal::{BeginSynchronizedUpdate, EndSynchronizedUpdate},
};
use ratatui::{DefaultTerminal, Frame};

use crate::{
    clip::ClipSelection,
    raster::RasterRenderer,
    video::{
        VideoEvent, VideoSummary,
        clip::{TrimProgress, trim_video_with_settings_and_progress},
        edit::VideoEditSettings,
        preview::{
            PreviewCommand, PreviewEvent, PreviewFormat, PreviewFrame, spawn_preview_worker,
        },
    },
};

use super::CaptureState;
use audio_preview::{
    EditorAudioPreview, audio_clock_active, control_audio_playback, maximum_picture_lag,
    pause_audio_preview, seek_audio_preview,
};
use tools::EditorTool;
use waveform::EditorWaveform;
use workers::{EditorWorkerSet, restart_editor_audio_workers, restart_editor_decoder};

/// Runs the full-screen screen-capture interface until the video worker stops.
pub(super) fn run(
    events: &Receiver<VideoEvent>,
    previews: &Receiver<PreviewFrame>,
    worker: &JoinHandle<Result<VideoSummary>>,
    stop: Arc<AtomicBool>,
    paused: Arc<AtomicBool>,
    output: PathBuf,
) -> Result<Option<VideoSummary>> {
    let mut app = VideoApp::new(stop, paused, output);
    ratatui::run(|terminal| {
        let raster = RasterRenderer::start()?;
        app.run(terminal, events, previews, worker, raster)
    })
}

/// Tracks one background MP4 replacement and the latest user-facing stage.
struct VideoTrimJob {
    /// Owns the decoder shutdown and native trim worker until both stop.
    worker: JoinHandle<Result<VideoSummary>>,
    /// Shares the latest processing stage without blocking terminal drawing.
    progress: Arc<Mutex<TrimProgress>>,
    /// Drives the loader animation and elapsed-time label.
    started: Instant,
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
    /// Contains the integrated frame-selection editor after MP4 finalization.
    editor: Option<VideoEditor>,
    /// Contains the summary returned to the command after editing completes.
    completed_summary: Option<VideoSummary>,
    /// Reports that the editor should close and return to the shell.
    exit_requested: bool,
}

/// Owns decoded frame selection and optional replacement work for one MP4.
struct VideoEditor {
    /// Contains the file metadata and is updated after a successful trim.
    summary: VideoSummary,
    /// Reuses the audio editor's bounded, frame-accurate handle behavior.
    selection: ClipSelection,
    /// Identifies the range, fade, or crop value currently owned by arrow keys.
    active_tool: EditorTool,
    /// Contains non-destructive fades and crop margins applied during export.
    edit_settings: VideoEditSettings,
    /// Loads and retains a full-duration audio envelope for timeline editing.
    waveform: Option<EditorWaveform>,
    /// Sends scrub and playback requests to the native decoder worker.
    commands: Sender<PreviewCommand>,
    /// Receives decoded pictures without blocking terminal drawing.
    events: Receiver<PreviewEvent>,
    /// Owns the decoder until a trim worker waits for it to release the MP4.
    decoder: Option<JoinHandle<()>>,
    /// Contains the stream description announced by Media Foundation.
    format: Option<PreviewFormat>,
    /// Contains the timestamp represented by the current raster picture.
    position: Duration,
    /// Contains the timestamp represented by the most recently decoded picture.
    picture_position: Duration,
    /// Plays the MP4's AAC track without creating a second video window.
    audio_preview: Option<EditorAudioPreview>,
    /// Contains nonfatal audio-output feedback while visual editing remains usable.
    audio_preview_error: Option<String>,
    /// Advances decoded pictures while true.
    playing: bool,
    /// Reports that Space paused active playback and can resume in place.
    resume_pending: bool,
    /// Reports whether exactly one decode request is outstanding.
    awaiting_picture: bool,
    /// Paces silent fallback playback to the source frame rate.
    next_picture_due: Instant,
    /// Coalesces repeated handle movement to the newest requested picture.
    pending_seek: Option<Duration>,
    /// Contains decoder or trim feedback shown in the editor.
    error: Option<String>,
    /// Rebuilds and replaces the MP4 away from the TUI thread.
    trim_job: Option<VideoTrimJob>,
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
            editor: None,
            completed_summary: None,
            exit_requested: false,
        }
    }

    /// Draws frames and handles input until the video worker stops.
    fn run(
        &mut self,
        terminal: &mut DefaultTerminal,
        events: &Receiver<VideoEvent>,
        previews: &Receiver<PreviewFrame>,
        worker: &JoinHandle<Result<VideoSummary>>,
        mut raster: RasterRenderer,
    ) -> Result<Option<VideoSummary>> {
        loop {
            self.advance_recorded_time();
            self.drain_video_events(events);
            if self.editor.is_none()
                && let Some(preview) = previews.try_iter().last()
            {
                raster.set_frame(preview);
            }
            self.drain_editor_events(&mut raster);
            self.drain_audio_preview_events();
            self.poll_video_waveform();
            self.poll_video_trim();
            self.request_editor_picture();
            if self.exit_requested {
                break;
            }
            if worker.is_finished() && self.completed_summary.is_none() {
                break;
            }
            draw_synchronized(terminal, |frame| {
                self.render_with_raster(frame, Some(&mut raster));
            })?;
            let input_wait = if self.editor.as_ref().is_some_and(|editor| editor.playing) {
                Duration::from_millis(8)
            } else {
                Duration::from_millis(33)
            };
            if event::poll(input_wait)? {
                self.handle_event(event::read()?);
            }
            if self.stop.load(Ordering::Relaxed) {
                self.state = CaptureState::Finalizing;
            }
        }
        self.drain_video_events(events);
        if let Some(editor) = &self.editor {
            let _ = editor.commands.try_send(PreviewCommand::Close);
            if let Some(waveform) = &editor.waveform {
                waveform.cancel();
            }
            if let Some(audio_preview) = &editor.audio_preview {
                audio_preview.close();
            }
        }
        Ok(self.completed_summary.clone())
    }

    /// Adds wall time to the clock only while actively recording.
    fn advance_recorded_time(&mut self) {
        let now = Instant::now();
        let delta = now - self.tick;
        self.tick = now;
        if matches!(
            self.state,
            CaptureState::Recording | CaptureState::Previewing
        ) && !self.paused.load(Ordering::Relaxed)
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
                VideoEvent::Saved(summary) => self.open_video_editor(summary),
            }
        }
    }

    /// Opens the finalized MP4 in the same frame-selection workflow.
    fn open_video_editor(&mut self, summary: VideoSummary) {
        self.completed_summary = Some(summary.clone());
        let selection = match ClipSelection::new(0, summary.frames, summary.fps) {
            Ok(selection) => selection,
            Err(_) => {
                self.notice = Some("The recording is too short to edit.".to_owned());
                self.exit_requested = true;
                return;
            }
        };
        let (commands, events, decoder) = match spawn_preview_worker(summary.output.clone()) {
            Ok(worker) => worker,
            Err(error) => {
                self.notice = Some(format!("Could not open the video editor: {error:#}"));
                self.exit_requested = true;
                return;
            }
        };
        let awaiting_picture = commands
            .try_send(PreviewCommand::Seek(Duration::ZERO))
            .is_ok();
        self.output = summary.output.clone();
        let waveform = EditorWaveform::start(summary.output.clone());
        let (audio_preview, audio_preview_error) = match EditorAudioPreview::start(
            summary.output.clone(),
        ) {
            Ok(preview) => (Some(preview), None),
            Err(error) => (
                None,
                Some(format!(
                    "Audio preview is unavailable; video preview will continue silently: {error:#}"
                )),
            ),
        };
        self.editor = Some(VideoEditor {
            summary,
            selection,
            active_tool: EditorTool::RangeStart,
            edit_settings: VideoEditSettings::default(),
            waveform: Some(waveform),
            commands,
            events,
            decoder: Some(decoder),
            format: None,
            position: Duration::ZERO,
            picture_position: Duration::ZERO,
            audio_preview,
            audio_preview_error,
            playing: false,
            resume_pending: false,
            awaiting_picture,
            next_picture_due: Instant::now(),
            pending_seek: None,
            error: None,
            trim_job: None,
        });
        self.notice = None;
        self.show_help = false;
    }

    /// Applies decoded pictures and failures without waiting on Media Foundation.
    fn drain_editor_events(&mut self, raster: &mut RasterRenderer) {
        let Some(editor) = &mut self.editor else {
            return;
        };
        let events = editor.events.try_iter().collect::<Vec<_>>();
        for event in events {
            match event {
                PreviewEvent::Ready(format) => editor.format = Some(format),
                PreviewEvent::Frame(frame) => {
                    editor.awaiting_picture = false;
                    editor.picture_position = frame.time;
                    if !audio_clock_active(editor) {
                        editor.position = frame.time;
                    }
                    raster.set_frame(*frame);
                    if !audio_clock_active(editor)
                        && editor.picture_position >= selection_end_time(editor)
                    {
                        editor.playing = false;
                        editor.resume_pending = false;
                    }
                }
                PreviewEvent::End => {
                    editor.awaiting_picture = false;
                    if !audio_clock_active(editor) {
                        editor.playing = false;
                        editor.resume_pending = false;
                    }
                }
                PreviewEvent::Failed(message) => {
                    let was_playing = editor.playing;
                    control_audio_playback(editor, false, was_playing);
                    editor.awaiting_picture = false;
                    editor.pending_seek = None;
                    editor.playing = false;
                    editor.resume_pending = false;
                    editor.error = Some(message);
                }
            }
        }
    }

    /// Keeps at most one sequential decode request in flight during playback.
    fn request_editor_picture(&mut self) {
        let Some(editor) = &mut self.editor else {
            return;
        };
        if editor.awaiting_picture || editor.error.is_some() || editor.trim_job.is_some() {
            return;
        }
        if let Some(time) = editor.pending_seek.take() {
            match editor.commands.try_send(PreviewCommand::Seek(time)) {
                Ok(()) => editor.awaiting_picture = true,
                Err(TrySendError::Full(_)) => editor.pending_seek = Some(time),
                Err(TrySendError::Disconnected(_)) => {
                    editor.error = Some("The video frame decoder stopped.".to_owned());
                }
            }
            return;
        }
        if !editor.playing {
            return;
        }
        let fallback_frame_duration = if audio_clock_active(editor) {
            let frame_duration = frame_time(1, editor.summary.fps);
            if editor.picture_position.saturating_add(frame_duration) >= editor.position {
                return;
            }
            let lag = editor.position.saturating_sub(editor.picture_position);
            if lag > maximum_picture_lag(editor) {
                match editor
                    .commands
                    .try_send(PreviewCommand::Seek(editor.position))
                {
                    Ok(()) => editor.awaiting_picture = true,
                    Err(TrySendError::Full(_)) => editor.pending_seek = Some(editor.position),
                    Err(TrySendError::Disconnected(_)) => {
                        editor.error = Some("The video frame decoder stopped.".to_owned());
                    }
                }
                return;
            }
            None
        } else {
            let frame_duration = frame_time(1, editor.summary.fps);
            if Instant::now() < editor.next_picture_due {
                return;
            }
            Some(frame_duration)
        };
        match editor.commands.try_send(PreviewCommand::Step) {
            Ok(()) => {
                editor.awaiting_picture = true;
                if let Some(frame_duration) = fallback_frame_duration {
                    editor.next_picture_due = Instant::now() + frame_duration;
                }
            }
            Err(TrySendError::Full(_)) => {}
            Err(TrySendError::Disconnected(_)) => {
                editor.error = Some("The video frame decoder stopped.".to_owned());
                editor.playing = false;
                editor.resume_pending = false;
            }
        }
    }

    /// Applies a finished audio envelope without blocking the editor loop.
    fn poll_video_waveform(&mut self) {
        if let Some(waveform) = self
            .editor
            .as_mut()
            .and_then(|editor| editor.waveform.as_mut())
        {
            waveform.poll();
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
        if self.editor.is_some() {
            self.handle_video_editor(key);
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

    /// Applies the existing clip-editor controls to video frame boundaries.
    fn handle_video_editor(&mut self, key: KeyEvent) {
        let Some(editor) = &self.editor else {
            return;
        };
        if editor.trim_job.is_some() {
            return;
        }
        if matches!(key.code, KeyCode::Char('c' | 'C'))
            && key.modifiers.contains(KeyModifiers::CONTROL)
        {
            self.close_video_editor();
            return;
        }
        if self.show_help {
            if matches!(key.code, KeyCode::Esc | KeyCode::Char('?')) {
                self.show_help = false;
            }
            return;
        }
        match key.code {
            KeyCode::Esc => self.close_video_editor(),
            KeyCode::Char('?') => self.show_help = true,
            KeyCode::Tab => {
                self.select_adjacent_editor_tool(key.modifiers.contains(KeyModifiers::SHIFT))
            }
            KeyCode::BackTab => self.select_adjacent_editor_tool(true),
            KeyCode::Char('t' | 'T') => self.select_range_tool(),
            KeyCode::Char('v' | 'V') => self.select_video_fade_tool(),
            KeyCode::Char('a' | 'A') => self.select_audio_fade_tool(),
            KeyCode::Char('c' | 'C') => self.select_crop_tool(),
            KeyCode::Left | KeyCode::Right | KeyCode::Up | KeyCode::Down => {
                self.adjust_active_editor_tool(key.code, key.modifiers);
            }
            KeyCode::Char('r' | 'R') | KeyCode::Home => {
                if let Some(editor) = &mut self.editor {
                    pause_audio_preview(editor);
                    editor.selection.reset();
                    editor.active_tool = EditorTool::RangeStart;
                    editor.edit_settings = VideoEditSettings::default();
                    editor.playing = false;
                    editor.resume_pending = false;
                    editor.error = None;
                }
                self.seek_editor(Duration::ZERO);
            }
            KeyCode::Backspace => self.restart_video_playback(),
            KeyCode::Char(' ') | KeyCode::Char('p' | 'P') => self.toggle_video_playback(),
            KeyCode::Enter => self.start_video_trim(),
            _ => {}
        }
    }

    /// Keeps the original MP4 and returns from `record video`.
    fn close_video_editor(&mut self) {
        if let Some(editor) = &mut self.editor {
            editor.playing = false;
            editor.resume_pending = false;
            let _ = editor.commands.try_send(PreviewCommand::Close);
            if let Some(waveform) = &editor.waveform {
                waveform.cancel();
            }
            if let Some(audio_preview) = &editor.audio_preview {
                audio_preview.close();
            }
        }
        self.exit_requested = true;
    }

    /// Requests the decoded picture at one exact media position.
    fn seek_editor(&mut self, time: Duration) {
        let Some(editor) = &mut self.editor else {
            return;
        };
        editor.position = time.min(editor.summary.duration());
        seek_audio_preview(editor, editor.position);
        if editor.awaiting_picture {
            editor.pending_seek = Some(editor.position);
            return;
        }
        match editor
            .commands
            .try_send(PreviewCommand::Seek(editor.position))
        {
            Ok(()) => editor.awaiting_picture = true,
            Err(TrySendError::Full(_)) => editor.pending_seek = Some(editor.position),
            Err(TrySendError::Disconnected(_)) => {
                editor.error = Some("The video frame decoder stopped.".to_owned());
            }
        }
    }

    /// Pauses playback and returns the playhead to the selection's first marker.
    fn restart_video_playback(&mut self) {
        let Some(editor) = &mut self.editor else {
            return;
        };
        pause_audio_preview(editor);
        editor.playing = false;
        editor.resume_pending = false;
        editor.error = None;
        let start = selection_start_time(editor);
        self.seek_editor(start);
    }

    /// Plays decoded pictures only inside the selected range.
    fn toggle_video_playback(&mut self) {
        let Some(editor) = &mut self.editor else {
            return;
        };
        if editor.error.is_some() {
            return;
        }
        if editor.playing {
            control_audio_playback(editor, false, true);
            editor.playing = false;
            editor.resume_pending = true;
            return;
        }
        let start = selection_start_time(editor);
        let end = selection_end_time(editor);
        let restart = !editor.resume_pending || editor.position < start || editor.position >= end;
        editor.resume_pending = false;
        editor.playing = true;
        editor.next_picture_due = Instant::now();
        if restart {
            self.seek_editor(start);
        }
        if let Some(editor) = &mut self.editor {
            control_audio_playback(editor, restart, false);
        }
    }

    /// Starts a worker that rebuilds the selected video and audio range.
    fn start_video_trim(&mut self) {
        let Some(editor) = &mut self.editor else {
            return;
        };
        pause_audio_preview(editor);
        editor.playing = false;
        editor.resume_pending = false;
        if editor.selection.start_frame() == editor.selection.source_start_frame()
            && editor.selection.end_frame() == editor.selection.source_end_frame()
            && editor.edit_settings.is_identity()
        {
            self.close_video_editor();
            return;
        }
        let summary = editor.summary.clone();
        let path = summary.output.clone();
        let start_frame = editor.selection.start_frame();
        let end_frame = editor.selection.end_frame();
        let edit_settings = editor.edit_settings;
        let progress = Arc::new(Mutex::new(TrimProgress::Preparing));
        let worker_progress = Arc::clone(&progress);
        let (worker_sender, worker_receiver) = crossbeam_channel::bounded::<EditorWorkerSet>(1);
        match thread::Builder::new()
            .name("record-video-trim".to_owned())
            .spawn(move || {
                let workers = worker_receiver
                    .recv()
                    .map_err(|_| anyhow::anyhow!("video trim startup was interrupted"))?;
                workers.close_and_join()?;
                trim_video_with_settings_and_progress(
                    &path,
                    &summary,
                    start_frame,
                    end_frame,
                    &edit_settings,
                    |update| {
                        let mut current = worker_progress
                            .lock()
                            .unwrap_or_else(|poisoned| poisoned.into_inner());
                        *current = update;
                    },
                )
            }) {
            Ok(worker) => {
                let workers = EditorWorkerSet::take(editor);
                if let Err(not_sent) = worker_sender.send(workers) {
                    not_sent.0.restore(editor);
                    let _ = worker.join();
                    editor.error =
                        Some("Could not hand the video preview to the save worker.".to_owned());
                    return;
                }
                editor.trim_job = Some(VideoTrimJob {
                    worker,
                    progress,
                    started: Instant::now(),
                });
                editor.error = None;
            }
            Err(error) => {
                editor.error = Some(format!("Could not start video trim: {error}"));
            }
        }
    }

    /// Applies a completed MP4 replacement or leaves the editor open on failure.
    fn poll_video_trim(&mut self) {
        let Some(editor) = &mut self.editor else {
            return;
        };
        if !editor
            .trim_job
            .as_ref()
            .is_some_and(|job| job.worker.is_finished())
        {
            return;
        }
        let job = editor.trim_job.take().expect("finished video trim exists");
        match job
            .worker
            .join()
            .map_err(|_| anyhow::anyhow!("the video trim thread panicked"))
            .and_then(|result| result)
        {
            Ok(summary) => {
                editor.summary = summary.clone();
                self.completed_summary = Some(summary);
                self.exit_requested = true;
            }
            Err(error) => {
                let save_error = format!("Could not save the trimmed video: {error:#}");
                let decoder_error = restart_editor_decoder(editor).err();
                restart_editor_audio_workers(editor);
                editor.error = Some(match decoder_error {
                    Some(decoder_error) => format!(
                        "{save_error} The frame preview could not restart: {decoder_error:#}"
                    ),
                    None => save_error,
                });
            }
        }
    }
}

/// Presents a complete terminal frame atomically when the emulator supports
/// DEC synchronized output. Sixel replaces an image by clearing its cell area
/// before transmitting the next picture; without synchronization that brief
/// empty state is visible as playback flicker.
fn draw_synchronized(terminal: &mut DefaultTerminal, draw: impl FnOnce(&mut Frame)) -> Result<()> {
    terminal.backend_mut().execute(BeginSynchronizedUpdate)?;
    let draw_result = terminal.draw(draw).map(|_| ());
    let end_result = terminal.backend_mut().execute(EndSynchronizedUpdate);

    match (draw_result, end_result) {
        (Ok(_), Ok(_)) => Ok(()),
        (Err(error), _) | (Ok(_), Err(error)) => Err(error.into()),
    }
}

/// Returns the selected start as a media timestamp.
fn selection_start_time(editor: &VideoEditor) -> Duration {
    frame_time(editor.selection.start_frame(), editor.summary.fps)
}

/// Returns the exclusive selected end as a media timestamp.
fn selection_end_time(editor: &VideoEditor) -> Duration {
    frame_time(editor.selection.end_frame(), editor.summary.fps)
}

/// Returns the current decoded frame number inside the source MP4.
fn editor_frame(editor: &VideoEditor) -> u64 {
    (editor.position.as_secs_f64() * f64::from(editor.summary.fps)).round() as u64
}

/// Returns the current picture position across the full source recording.
fn editor_position_ratio(editor: &VideoEditor) -> f64 {
    let duration = editor.summary.duration();
    if duration.is_zero() {
        0.0
    } else {
        (editor.position.as_secs_f64() / duration.as_secs_f64()).clamp(0.0, 1.0)
    }
}

/// Returns the constant-rate timestamp for one video frame.
fn frame_time(frame: u64, fps: u32) -> Duration {
    if fps == 0 {
        Duration::ZERO
    } else {
        Duration::from_secs_f64(frame as f64 / f64::from(fps))
    }
}

#[cfg(test)]
mod tests;
