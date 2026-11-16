//! Plain types shared by the capture thread and the terminal hub.
//!
//! They have no Windows dependency, so the hub drawing code can run in tests
//! with a stand-in for the capture thread.

use std::{
    path::PathBuf,
    sync::{Mutex, atomic::AtomicBool},
    time::Duration,
};

use crossbeam_channel::Receiver;

use super::{CaptureMode, MonitorChoice, ScreenshotSummary, upload::Uploader};

/// Settings shared by the hub thread and the terminal hub.
#[derive(Debug)]
pub struct HubSettings {
    /// Folder that receives the PNG files.
    pub directory: PathBuf,
    /// Monitor for the region and monitor modes.
    pub monitor: MonitorChoice,
    /// Overrides the SDR white level that Windows reports, in nits.
    pub sdr_white_nits: Option<f32>,
    /// Places each image on the clipboard.
    pub copy: AtomicBool,
    /// Writes each image to a PNG file.
    pub save: AtomicBool,
    /// Time between auto captures.
    pub auto_interval: Duration,
    /// Destination for uploads, when one is configured. The hub can change it.
    pub uploader: Mutex<Option<Uploader>>,
    /// Uploads each image to the destination and copies the link.
    pub upload: AtomicBool,
}

/// One request from the terminal hub to the capture thread.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HubRequest {
    /// Capture now.
    Capture(CaptureMode),
    /// Capture after three seconds, so the user can arrange the desktop.
    Delayed(CaptureMode),
    /// Start or stop auto capture.
    ToggleAuto,
}

/// One report from the capture thread.
#[derive(Clone, Debug)]
pub enum HubEvent {
    /// The thread is running. Each flag reports one registered hotkey:
    /// region, fullscreen, and active window.
    Ready([bool; 3]),
    /// A capture started.
    Capturing,
    /// A capture finished.
    Finished(ScreenshotSummary),
    /// The user closed the overlay without saving.
    Cancelled,
    /// A capture failed.
    Failed(String),
    /// Auto capture changed state.
    Auto(bool),
    /// The tray menu asked the hub to stop.
    QuitRequested,
}

impl HubSettings {
    /// Returns a copy of the configured upload destination.
    #[must_use]
    pub fn active_uploader(&self) -> Option<Uploader> {
        self.uploader
            .lock()
            .ok()
            .and_then(|uploader| uploader.clone())
    }
}

/// The capture side of the hub: it accepts requests and reports results.
pub trait CaptureHub {
    /// Returns the channel that carries reports from the capture thread.
    fn events(&self) -> &Receiver<HubEvent>;

    /// Sends one request. A busy capture thread drops it.
    fn request(&self, request: HubRequest);
}
