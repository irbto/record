//! Audio capture contracts shared by the platform backend and the interface.
//!
//! The Windows backend owns WASAPI and Media Foundation objects. It sends
//! bounded display events to the main thread. The main thread sends rare
//! control commands in the other direction. This split keeps rendering work
//! away from the real-time capture loop.

#[cfg(windows)]
pub(crate) mod windows;

use std::{path::PathBuf, sync::Arc, sync::atomic::AtomicBool, time::Duration};

use crossbeam_channel::{Receiver, Sender};

use crate::session::OutputTarget;

#[cfg(windows)]
pub(crate) use windows::trim_pcm_clip;
#[cfg(windows)]
pub use windows::{check_support, record};

/// Contains all settings and shared controls for one recording process.
#[derive(Clone, Debug)]
pub struct RecordConfig {
    /// Selects one file or a rotating session directory.
    pub target: OutputTarget,
    /// Requests an MP3 bit rate in bits per second.
    pub bitrate: u32,
    /// Stops capture after this encoded duration when set.
    pub duration: Option<Duration>,
    /// Lets the interface ask the audio thread to finalize a named clip.
    pub commands: Receiver<AudioCommand>,
    /// Becomes true when capture must stop.
    pub stop: Arc<AtomicBool>,
    /// Becomes true while capture packets must be omitted.
    pub paused: Arc<AtomicBool>,
}

/// A low-frequency command from the interface to the audio thread.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AudioCommand {
    /// Finalize the current session part under the provided validated stem.
    SaveClip(String),
}

/// Identifies why the recorder finalized an MP3 file.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SavedFileKind {
    /// The user requested one explicit output file.
    Recording,
    /// The automatic session boundary finalized this numbered part.
    Part,
    /// The user named and finalized the current session part.
    Clip,
}

#[derive(Debug, Eq, PartialEq)]
/// Owns one temporary directory until all edit sources release it.
pub(crate) struct ClipCacheLease {
    /// Contains the directory that `Drop` removes.
    pub(crate) path: PathBuf,
}

impl Drop for ClipCacheLease {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// Points to temporary PCM data that can rebuild a named clip.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClipEditSource {
    /// Temporary stereo 16-bit PCM file in little-endian order.
    pub pcm_path: std::path::PathBuf,
    /// First source frame in the current clip.
    pub start_frame: u64,
    /// Exclusive last source frame in the current clip.
    pub end_frame: u64,
    pub(crate) cache: Arc<ClipCacheLease>,
}

/// Describes one complete MP3 file from the active process.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SavedFile {
    /// Final path of the MP3 file.
    pub path: std::path::PathBuf,
    /// Reason that the backend finalized the file.
    pub kind: SavedFileKind,
    /// Encoded sample rate in frames per second.
    pub sample_rate: u32,
    /// Encoded MP3 bit rate in bits per second.
    pub bitrate: u32,
    /// Number of stereo frames in this file.
    pub frames: u64,
    /// Temporary edit source for a named clip.
    pub edit_source: Option<ClipEditSource>,
}

impl SavedFile {
    /// Returns the encoded duration of this file.
    #[must_use]
    pub fn duration(&self) -> Duration {
        if self.sample_rate == 0 {
            Duration::ZERO
        } else {
            Duration::from_secs_f64(self.frames as f64 / f64::from(self.sample_rate))
        }
    }
}

/// Reports capture state and bounded visualization samples to the interface.
#[derive(Clone, Debug)]
pub enum AudioEvent {
    /// Reports that WASAPI capture and the first MP3 writer have started.
    Started {
        /// Actual MP3 sample rate in frames per second.
        sample_rate: u32,
        /// Actual MP3 bit rate in bits per second.
        bitrate: u32,
        /// Number of output channels.
        channels: u16,
    },
    /// Provides a small stereo block for the terminal display.
    Samples {
        /// Normalized left-channel samples.
        left: Vec<f32>,
        /// Normalized right-channel samples.
        right: Vec<f32>,
        /// Total frames encoded in the process so far.
        encoded_frames: u64,
    },
    /// Reports one MP3 after all file handles are closed.
    Saved(SavedFile),
    /// Provides a short backend message for the current interface.
    Notice(String),
    /// Reports that capture has stopped and final encoding is in progress.
    Finalizing,
}

/// Summarizes all files that one recorder process created.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecordingSummary {
    /// File or session directory shown as the main output.
    pub output: std::path::PathBuf,
    /// Actual MP3 sample rate in frames per second.
    pub sample_rate: u32,
    /// Actual MP3 bit rate in bits per second.
    pub bitrate: u32,
    /// Total number of encoded stereo frames.
    pub frames: u64,
    /// Finalized MP3 files in capture order.
    pub files: Vec<SavedFile>,
}

impl RecordingSummary {
    /// Returns the total encoded duration across all files.
    #[must_use]
    pub fn duration(&self) -> Duration {
        if self.sample_rate == 0 {
            Duration::ZERO
        } else {
            Duration::from_secs_f64(self.frames as f64 / f64::from(self.sample_rate))
        }
    }
}

/// Sends capture events without giving the backend access to the receiver.
pub type EventSender = Sender<AudioEvent>;

/// Returns an error on platforms that do not have a capture backend yet.
#[cfg(not(windows))]
pub fn check_support() -> anyhow::Result<()> {
    anyhow::bail!(
        "record currently captures system audio on Windows 10/11 only; Linux and macOS backends are planned"
    )
}

/// Returns the platform support error without creating an output file.
#[cfg(not(windows))]
pub fn record(_config: RecordConfig, _events: &EventSender) -> anyhow::Result<RecordingSummary> {
    check_support()?;
    unreachable!()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn saved_file_duration_uses_its_sample_rate() {
        let file = SavedFile {
            path: PathBuf::from("part-001.mp3"),
            kind: SavedFileKind::Part,
            sample_rate: 48_000,
            bitrate: 320_000,
            frames: 120_000,
            edit_source: None,
        };
        assert_eq!(file.duration(), Duration::from_millis(2_500));
    }

    #[test]
    fn zero_sample_rate_has_zero_duration() {
        let file = SavedFile {
            path: PathBuf::from("empty.mp3"),
            kind: SavedFileKind::Recording,
            sample_rate: 0,
            bitrate: 320_000,
            frames: 100,
            edit_source: None,
        };
        assert_eq!(file.duration(), Duration::ZERO);
    }

    #[test]
    fn summary_duration_uses_total_process_frames() {
        let summary = RecordingSummary {
            output: PathBuf::from("session"),
            sample_rate: 44_100,
            bitrate: 320_000,
            frames: 88_200,
            files: Vec::new(),
        };
        assert_eq!(summary.duration(), Duration::from_secs(2));
    }

    #[test]
    fn clip_cache_lease_removes_its_directory_on_last_drop() {
        let root = tempfile::tempdir().unwrap();
        let cache = root.path().join("cache");
        std::fs::create_dir(&cache).unwrap();
        std::fs::write(cache.join("part.pcm"), [1, 2, 3]).unwrap();
        let first = Arc::new(ClipCacheLease {
            path: cache.clone(),
        });
        let second = Arc::clone(&first);
        drop(first);
        assert!(cache.exists());
        drop(second);
        assert!(!cache.exists());
    }
}
