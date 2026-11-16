//! The boundary between the dashboard and the capture engines.
//!
//! The dashboard draws state and sends requests. It owns no capture state.
//! These plain types carry the requests and the updates, so the dashboard runs
//! in tests with stand-in engines and no window.

use std::{
    path::{Path, PathBuf},
    sync::{Arc, atomic::AtomicBool},
};

use crossbeam_channel::Receiver;

/// One of the three things the dashboard records.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Mode {
    /// System audio to MP3.
    Sound,
    /// Screen and system audio to MP4.
    Video,
    /// Still images of the screen.
    Screenshot,
}

impl Mode {
    /// Every mode in the order of the switcher.
    pub const ALL: [Self; 3] = [Self::Sound, Self::Video, Self::Screenshot];

    /// Returns the name shown in the switcher.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Sound => "Sound",
            Self::Video => "Video",
            Self::Screenshot => "Screenshot",
        }
    }

    /// Returns the name used on the command line and in the settings file.
    #[must_use]
    pub const fn id(self) -> &'static str {
        match self {
            Self::Sound => "sound",
            Self::Video => "video",
            Self::Screenshot => "screenshot",
        }
    }

    /// Finds a mode by its identifier, without regard to case.
    #[must_use]
    pub fn from_id(id: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|mode| mode.id().eq_ignore_ascii_case(id.trim()))
    }
}

/// One attached monitor for the video source list.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Monitor {
    /// Index that the video engine accepts.
    pub index: u32,
    /// Windows display name.
    pub name: String,
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// Reports whether this is the primary monitor.
    pub primary: bool,
}

/// One report from a running sound recording.
#[derive(Clone, Debug, PartialEq)]
pub enum SoundUpdate {
    /// Capture started with these actual values.
    Started {
        /// Sample rate in frames per second.
        sample_rate: u32,
        /// MP3 bit rate in bits per second.
        bitrate: u32,
    },
    /// Peak levels of the newest audio block.
    Levels {
        /// Left peak from 0 to 1.
        left: f32,
        /// Right peak from 0 to 1.
        right: f32,
        /// Encoded time so far in seconds.
        seconds: f64,
    },
    /// Capture stopped and the file is being closed.
    Finalizing,
    /// A short message from the engine.
    Notice(String),
    /// The recording ended with a file or with an error.
    Stopped(Result<PathBuf, String>),
}

/// One report from a running video recording.
#[derive(Clone, Debug, PartialEq)]
pub enum VideoUpdate {
    /// Capture started with these actual values.
    Started {
        /// Encoded width in pixels.
        width: u32,
        /// Encoded height in pixels.
        height: u32,
        /// Encoded frames per second.
        fps: u32,
    },
    /// A small live picture of the desktop.
    Preview {
        /// Picture width in pixels.
        width: u32,
        /// Picture height in pixels.
        height: u32,
        /// Packed RGBA bytes in top-down rows.
        rgba: Vec<u8>,
    },
    /// Capture stopped and the file is being closed.
    Finalizing,
    /// A short message from the engine.
    Notice(String),
    /// The recording ended with a file or with an error.
    Stopped(Result<PathBuf, String>),
}

/// The controls and the update stream of one running recording.
#[derive(Debug)]
pub struct Session<U> {
    /// Reports from the engine thread.
    pub updates: Receiver<U>,
    /// Set to true to stop the recording.
    pub stop: Arc<AtomicBool>,
    /// True while the recording omits new material.
    pub paused: Arc<AtomicBool>,
}

/// What the dashboard asks of the sound and video engines and of the system.
pub trait Engines {
    /// Starts a sound recording to an MP3 file.
    fn start_sound(&self, output: PathBuf, bitrate: u32) -> Session<SoundUpdate>;

    /// Starts a video recording of one monitor to an MP4 file.
    fn start_video(&self, output: PathBuf, monitor: u32, fps: u32) -> Session<VideoUpdate>;

    /// Lists the attached monitors.
    fn monitors(&self) -> Vec<Monitor>;

    /// Returns a local timestamp for file names.
    fn timestamp(&self) -> String;

    /// Returns the folder that receives sound or video files.
    fn folder(&self, mode: Mode) -> PathBuf;

    /// Shows a file or a folder in the system file manager.
    fn reveal(&self, path: &Path);

    /// Places a saved image on the clipboard.
    fn copy_image(&self, path: &Path) -> Result<(), String>;
}
