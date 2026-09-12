//! Background loading state for the MP4 editor's audio waveform.

use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
};

use anyhow::{Context, Result, anyhow};
use crossbeam_channel::{Receiver, bounded};

use crate::video::waveform::{WaveformLoad, load_mp4_waveform};

/// High-resolution source bins retained for terminal-width resampling.
const SOURCE_BINS: usize = 4_096;

/// Owns the optional background decoder while it summarizes MP4 audio.
pub(super) enum EditorWaveform {
    Loading(WaveformWorker),
    Ready(Vec<f32>),
    Unavailable(String),
}

/// Keeps the source reader cancellable before trim needs to replace the MP4.
pub(super) struct WaveformWorker {
    result: Receiver<Result<WaveformLoad, String>>,
    cancelled: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl EditorWaveform {
    /// Starts bounded waveform extraction without delaying the first editor draw.
    pub(super) fn start(path: PathBuf) -> Self {
        let (sender, result) = bounded(1);
        let cancelled = Arc::new(AtomicBool::new(false));
        let worker_cancelled = Arc::clone(&cancelled);
        let worker = thread::Builder::new()
            .name("record-video-waveform".to_owned())
            .spawn(move || {
                let outcome = load_mp4_waveform(&path, SOURCE_BINS, &worker_cancelled)
                    .map_err(|error| format!("{error:#}"));
                let _ = sender.send(outcome);
            });
        match worker {
            Ok(worker) => Self::Loading(WaveformWorker {
                result,
                cancelled,
                worker: Some(worker),
            }),
            Err(error) => Self::Unavailable(format!("Could not load audio: {error}")),
        }
    }

    /// Applies a completed waveform result without blocking terminal drawing.
    pub(super) fn poll(&mut self) {
        let result = match self {
            Self::Loading(job) => job.result.try_recv().ok(),
            Self::Ready(_) | Self::Unavailable(_) => None,
        };
        let Some(result) = result else {
            return;
        };
        let replacement = match result {
            Ok(WaveformLoad::Ready(bins)) => Self::Ready(
                bins.into_iter()
                    .map(|bin| bin.minimum.abs().max(bin.maximum.abs()).clamp(0.0, 1.0))
                    .collect(),
            ),
            Ok(WaveformLoad::Unavailable) => {
                Self::Unavailable("This video does not contain an audio track.".to_owned())
            }
            Ok(WaveformLoad::Cancelled) => {
                Self::Unavailable("Audio waveform loading was cancelled.".to_owned())
            }
            Err(error) => Self::Unavailable(format!("Could not load audio: {error}")),
        };
        let previous = std::mem::replace(self, replacement);
        if let Self::Loading(mut job) = previous
            && let Some(worker) = job.worker.take()
        {
            let _ = worker.join();
        }
    }

    /// Returns decoded peak magnitudes when the worker has completed.
    pub(super) fn peaks(&self) -> Option<&[f32]> {
        match self {
            Self::Ready(peaks) => Some(peaks),
            Self::Loading(_) | Self::Unavailable(_) => None,
        }
    }

    /// Returns a concise panel-state suffix while audio is unavailable.
    pub(super) fn status(&self) -> Option<&str> {
        match self {
            Self::Loading(_) => Some("LOADING AUDIO"),
            Self::Ready(_) => None,
            Self::Unavailable(message) => Some(message),
        }
    }

    /// Requests cancellation without waiting on terminal input handling.
    pub(super) fn cancel(&self) {
        if let Self::Loading(job) = self {
            job.cancelled.store(true, Ordering::Relaxed);
        }
    }

    /// Releases a waveform source reader before atomic video replacement.
    pub(super) fn cancel_and_join(self) -> Result<()> {
        let Self::Loading(mut job) = self else {
            return Ok(());
        };
        job.cancelled.store(true, Ordering::Relaxed);
        if let Some(worker) = job.worker.take() {
            worker
                .join()
                .map_err(|_| anyhow!("the audio waveform thread panicked"))
                .context("could not stop audio waveform loading")?;
        }
        Ok(())
    }
}
