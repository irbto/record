//! Synchronizes Media Foundation audio-only playback with decoded TUI frames.

use std::{path::PathBuf, thread::JoinHandle, time::Duration};

use anyhow::{Result, anyhow};
use crossbeam_channel::{Receiver, Sender};

use crate::video::audio_preview::{
    AudioPreviewCommand, AudioPreviewEvent, spawn_audio_preview_worker,
};

use super::{VideoApp, VideoEditor, frame_time, selection_end_time, selection_start_time};

/// Owns one MP4 audio worker and its nonblocking terminal-side state.
pub(super) struct EditorAudioPreview {
    commands: Sender<AudioPreviewCommand>,
    events: Receiver<AudioPreviewEvent>,
    worker: Option<JoinHandle<()>>,
    pub(super) ready: bool,
    pub(super) playing: bool,
}

impl EditorAudioPreview {
    /// Starts loading the finalized MP4 without delaying the first editor draw.
    pub(super) fn start(path: PathBuf) -> Result<Self> {
        let (commands, events, worker) = spawn_audio_preview_worker(path)?;
        Ok(Self {
            commands,
            events,
            worker: Some(worker),
            ready: false,
            playing: false,
        })
    }

    /// Sends a low-frequency control without blocking terminal input.
    pub(super) fn try_send(&self, command: AudioPreviewCommand) {
        let _ = self.commands.try_send(command);
    }

    /// Requests prompt shutdown when the editor keeps the original MP4.
    pub(super) fn close(&self) {
        self.try_send(AudioPreviewCommand::Close);
    }

    /// Releases the Media Engine before trim atomically replaces its source.
    pub(super) fn close_and_join(mut self) -> Result<()> {
        let _ = self.commands.send(AudioPreviewCommand::Close);
        if let Some(worker) = self.worker.take() {
            worker
                .join()
                .map_err(|_| anyhow!("the video audio-preview thread panicked"))?;
        }
        Ok(())
    }
}

impl VideoApp {
    /// Applies audio clock, pause, end, and failure events without waiting.
    pub(super) fn drain_audio_preview_events(&mut self) {
        let events = self
            .editor
            .as_ref()
            .and_then(|editor| editor.audio_preview.as_ref())
            .map(|preview| preview.events.try_iter().collect::<Vec<_>>())
            .unwrap_or_default();
        for event in events {
            let Some(editor) = &mut self.editor else {
                return;
            };
            match event {
                AudioPreviewEvent::Ready { .. } => {
                    if let Some(preview) = &mut editor.audio_preview {
                        preview.ready = true;
                    }
                    editor.audio_preview_error = None;
                }
                AudioPreviewEvent::Position { position, playing } => {
                    editor.position = position.min(editor.summary.duration());
                    if let Some(preview) = &mut editor.audio_preview {
                        preview.playing = playing;
                    }
                    if playing {
                        editor.playing = true;
                        set_audio_preview_volume(editor);
                    }
                }
                AudioPreviewEvent::Paused { position } => {
                    editor.position = position.min(editor.summary.duration());
                    if let Some(preview) = &mut editor.audio_preview {
                        preview.playing = false;
                    }
                    set_audio_preview_volume(editor);
                }
                AudioPreviewEvent::End { position } => {
                    editor.position = position.min(editor.summary.duration());
                    if let Some(preview) = &mut editor.audio_preview {
                        preview.playing = false;
                    }
                    editor.playing = false;
                    editor.resume_pending = false;
                    set_audio_preview_volume(editor);
                }
                AudioPreviewEvent::Failed(message) => {
                    editor.audio_preview_error = Some(format!(
                        "Audio preview is unavailable; video preview will continue silently: {message}"
                    ));
                    if let Some(preview) = &mut editor.audio_preview {
                        preview.ready = false;
                        preview.playing = false;
                    }
                }
            }
        }
    }
}

/// Stops native audio immediately before a scrub, edit, or explicit restart.
pub(super) fn pause_audio_preview(editor: &mut VideoEditor) {
    if let Some(preview) = &editor.audio_preview {
        preview.try_send(AudioPreviewCommand::PauseForEdit);
    }
}

/// Starts, pauses, or resumes the audio side of current video playback.
pub(super) fn control_audio_playback(editor: &mut VideoEditor, restart: bool, was_playing: bool) {
    let Some(preview) = &editor.audio_preview else {
        return;
    };
    if was_playing {
        preview.try_send(AudioPreviewCommand::Pause);
        return;
    }
    preview.try_send(AudioPreviewCommand::SetVolume(audio_preview_gain(editor)));
    if restart {
        preview.try_send(AudioPreviewCommand::PlayRange {
            start: selection_start_time(editor),
            end: selection_end_time(editor),
        });
    } else {
        preview.try_send(AudioPreviewCommand::Resume);
    }
}

/// Keeps paused audio position and gain aligned with timeline scrubbing.
pub(super) fn seek_audio_preview(editor: &VideoEditor, position: Duration) {
    let Some(preview) = &editor.audio_preview else {
        return;
    };
    preview.try_send(AudioPreviewCommand::Seek(position));
    preview.try_send(AudioPreviewCommand::SetVolume(audio_preview_gain(editor)));
}

/// Reports whether Media Engine is the playback clock for this editor.
pub(super) fn audio_clock_active(editor: &VideoEditor) -> bool {
    editor
        .audio_preview
        .as_ref()
        .is_some_and(|preview| preview.ready && preview.playing)
}

/// Returns the same independent linear envelope used during audio export.
fn audio_preview_gain(editor: &VideoEditor) -> f64 {
    let start = selection_start_time(editor);
    let end = selection_end_time(editor);
    let fade_in = duration_gain(
        editor.position.saturating_sub(start),
        editor.edit_settings.audio_fade_in,
    );
    let fade_out = duration_gain(
        end.saturating_sub(editor.position),
        editor.edit_settings.audio_fade_out,
    );
    fade_in.min(fade_out)
}

fn set_audio_preview_volume(editor: &VideoEditor) {
    if let Some(preview) = &editor.audio_preview {
        preview.try_send(AudioPreviewCommand::SetVolume(audio_preview_gain(editor)));
    }
}

fn duration_gain(elapsed: Duration, fade: Duration) -> f64 {
    if fade.is_zero() {
        1.0
    } else {
        (elapsed.as_secs_f64() / fade.as_secs_f64()).clamp(0.0, 1.0)
    }
}

/// Returns the largest tolerable sequential-decoder lag before an exact seek.
pub(super) fn maximum_picture_lag(editor: &VideoEditor) -> Duration {
    frame_time(12, editor.summary.fps).max(Duration::from_millis(200))
}

#[cfg(test)]
impl EditorAudioPreview {
    /// Creates controllable channels without starting Media Foundation.
    pub(super) fn test_channels() -> (
        Self,
        Receiver<AudioPreviewCommand>,
        Sender<AudioPreviewEvent>,
    ) {
        let (commands, received_commands) = crossbeam_channel::bounded(16);
        let (sent_events, events) = crossbeam_channel::bounded(16);
        (
            Self {
                commands,
                events,
                worker: None,
                ready: true,
                playing: true,
            },
            received_commands,
            sent_events,
        )
    }
}
