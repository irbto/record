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
/// The screenshot hub.
pub mod screenshot;
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
use record_core::theme;

use crate::{
    audio::{AudioCommand, AudioEvent, RecordingSummary},
    session::OutputTarget,
    video::{VideoEvent, VideoSummary, preview::PreviewFrame},
};

/// Converts a shared theme color to the terminal color type.
const fn color(rgb: theme::Rgb) -> Color {
    Color::Rgb(rgb.0, rgb.1, rgb.2)
}

pub(crate) const CYAN: Color = color(theme::CYAN);
pub(crate) const GREEN: Color = color(theme::GREEN);
pub(crate) const RED: Color = color(theme::RED);
pub(crate) const AMBER: Color = color(theme::AMBER);
pub(crate) const PURPLE: Color = color(theme::PURPLE);
pub(crate) const MUTED: Color = color(theme::MUTED);
pub(crate) const BORDER: Color = color(theme::BORDER);
pub(crate) const TEXT: Color = color(theme::TEXT);
pub(crate) const SELECTED: Color = color(theme::SELECTED);

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
