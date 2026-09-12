//! Lifecycle helpers for the editor's native preview and analysis workers.

use std::thread::JoinHandle;

use anyhow::{Result, anyhow};
use crossbeam_channel::Sender;

use crate::video::preview::{PreviewCommand, spawn_preview_worker};

use super::{VideoEditor, audio_preview::EditorAudioPreview, waveform::EditorWaveform};

/// Worker handles transferred to the off-screen trim coordinator.
pub(super) struct EditorWorkerSet {
    commands: Sender<PreviewCommand>,
    decoder: Option<JoinHandle<()>>,
    waveform: Option<EditorWaveform>,
    audio_preview: Option<EditorAudioPreview>,
}

impl EditorWorkerSet {
    /// Removes file-reading workers only after the coordinator thread exists.
    pub(super) fn take(editor: &mut VideoEditor) -> Self {
        Self {
            commands: editor.commands.clone(),
            decoder: editor.decoder.take(),
            waveform: editor.waveform.take(),
            audio_preview: editor.audio_preview.take(),
        }
    }

    /// Restores untouched handles if the coordinator cannot accept ownership.
    pub(super) fn restore(self, editor: &mut VideoEditor) {
        editor.decoder = self.decoder;
        editor.waveform = self.waveform;
        editor.audio_preview = self.audio_preview;
    }

    /// Releases every source reader before Windows atomically replaces the MP4.
    pub(super) fn close_and_join(self) -> Result<()> {
        let Self {
            commands,
            decoder,
            waveform,
            audio_preview,
        } = self;
        // Blocking is intentional on this coordinator thread. It guarantees a
        // full bounded preview queue cannot strand the decoder during saving.
        let _ = commands.send(PreviewCommand::Close);
        if let Some(decoder) = decoder {
            decoder
                .join()
                .map_err(|_| anyhow!("the video decoder thread panicked"))?;
        }
        if let Some(waveform) = waveform {
            waveform.cancel_and_join()?;
        }
        if let Some(audio_preview) = audio_preview {
            audio_preview.close_and_join()?;
        }
        Ok(())
    }
}

/// Opens a fresh frame decoder after a failed rebuild released the old one.
pub(super) fn restart_editor_decoder(editor: &mut VideoEditor) -> Result<()> {
    let (commands, events, decoder) = spawn_preview_worker(editor.summary.output.clone())?;
    let position = editor.position.min(editor.summary.duration());
    if let Err(error) = commands.try_send(PreviewCommand::Seek(position)) {
        let _ = commands.try_send(PreviewCommand::Close);
        let _ = decoder.join();
        return Err(anyhow!(
            "could not queue the restored preview frame: {error}"
        ));
    }
    editor.commands = commands;
    editor.events = events;
    editor.decoder = Some(decoder);
    editor.format = None;
    editor.awaiting_picture = true;
    editor.pending_seek = None;
    editor.next_picture_due = std::time::Instant::now();
    Ok(())
}

/// Restarts nonessential audio helpers after an unsuccessful trim attempt.
pub(super) fn restart_editor_audio_workers(editor: &mut VideoEditor) {
    editor.waveform = Some(EditorWaveform::start(editor.summary.output.clone()));
    match EditorAudioPreview::start(editor.summary.output.clone()) {
        Ok(preview) => {
            editor.audio_preview = Some(preview);
            editor.audio_preview_error = None;
        }
        Err(error) => {
            editor.audio_preview = None;
            editor.audio_preview_error = Some(format!(
                "Audio preview is unavailable; video preview will continue silently: {error:#}"
            ));
        }
    }
}
