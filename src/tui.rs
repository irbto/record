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

mod audio;
mod utils;
mod video;

use std::{
    path::PathBuf,
    sync::{Arc, atomic::AtomicBool},
    thread::JoinHandle,
};

use anyhow::Result;
use crossbeam_channel::{Receiver, Sender};
use ratatui::style::Color;

use crate::{
    audio::{AudioCommand, AudioEvent, RecordingSummary},
    session::OutputTarget,
    video::{VideoEvent, VideoSummary, preview::PreviewFrame},
};

pub(crate) const CYAN: Color = Color::Rgb(65, 214, 224);
pub(crate) const GREEN: Color = Color::Rgb(72, 214, 147);
pub(crate) const RED: Color = Color::Rgb(247, 75, 103);
pub(crate) const AMBER: Color = Color::Rgb(244, 183, 73);
pub(crate) const PURPLE: Color = Color::Rgb(172, 122, 255);
pub(crate) const MUTED: Color = Color::Rgb(126, 139, 158);
pub(crate) const BORDER: Color = Color::Rgb(55, 65, 82);

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
    audio::run(events, worker, stop, paused, commands, target)
}

/// Runs the full-screen screen-capture interface until the video worker stops.
pub fn run_video(
    events: &Receiver<VideoEvent>,
    previews: &Receiver<PreviewFrame>,
    worker: &JoinHandle<Result<VideoSummary>>,
    stop: Arc<AtomicBool>,
    paused: Arc<AtomicBool>,
    output: PathBuf,
) -> Result<Option<VideoSummary>> {
    video::run(events, previews, worker, stop, paused, output)
}
