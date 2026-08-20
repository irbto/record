#[cfg(windows)]
mod windows;

use std::{path::PathBuf, sync::Arc, sync::atomic::AtomicBool, time::Duration};

use crossbeam_channel::Sender;

#[cfg(windows)]
pub use windows::{check_support, record};

#[derive(Clone, Debug)]
pub struct RecordConfig {
    pub output: PathBuf,
    pub bitrate: u32,
    pub duration: Option<Duration>,
    pub force: bool,
    pub stop: Arc<AtomicBool>,
    pub paused: Arc<AtomicBool>,
}

#[derive(Clone, Debug)]
pub enum AudioEvent {
    Started {
        sample_rate: u32,
        bitrate: u32,
        channels: u16,
    },
    Samples {
        left: Vec<f32>,
        right: Vec<f32>,
        encoded_frames: u64,
    },
    Finalizing,
}

#[derive(Clone, Debug)]
pub struct RecordingSummary {
    pub output: PathBuf,
    pub sample_rate: u32,
    pub bitrate: u32,
    pub frames: u64,
}

impl RecordingSummary {
    #[must_use]
    pub fn duration(&self) -> Duration {
        Duration::from_secs_f64(self.frames as f64 / f64::from(self.sample_rate))
    }
}

pub type EventSender = Sender<AudioEvent>;

#[cfg(not(windows))]
pub fn check_support() -> anyhow::Result<()> {
    anyhow::bail!(
        "record currently captures system audio on Windows 10/11 only; Linux and macOS backends are planned"
    )
}

#[cfg(not(windows))]
pub fn record(_config: RecordConfig, _events: &EventSender) -> anyhow::Result<RecordingSummary> {
    check_support()?;
    unreachable!()
}
