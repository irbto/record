//! Plain types shared by the capture thread and the terminal hub.
//!
//! They have no Windows dependency, so the hub drawing code can run in tests
//! with a stand-in for the capture thread.

use std::{
    path::PathBuf,
    sync::{
        Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use crossbeam_channel::Receiver;

use super::{
    CaptureMode, MonitorChoice, ScreenshotSummary, default_directory,
    format::ImageFormat,
    saved::SavedSettings,
    upload::{Uploader, default_destination, resolve_destination},
};

/// The capture defaults that the settings menu can change while the hub runs.
#[derive(Clone, Debug, PartialEq)]
pub struct CaptureOptions {
    /// Folder that receives the image files.
    pub directory: PathBuf,
    /// File format of saved screenshots.
    pub format: ImageFormat,
    /// Monitor for the region and monitor modes.
    pub monitor: MonitorChoice,
    /// Overrides the SDR white level that Windows reports, in nits.
    pub sdr_white_nits: Option<f32>,
    /// Time between auto captures.
    pub auto_interval: Duration,
    /// Wait of the delayed captures.
    pub delay: Duration,
}

/// Settings shared by the hub thread and the terminal hub.
#[derive(Debug)]
pub struct HubSettings {
    /// Capture defaults. The settings menu changes them.
    pub options: Mutex<CaptureOptions>,
    /// Places each image on the clipboard.
    pub copy: AtomicBool,
    /// Writes each image to a PNG file.
    pub save: AtomicBool,
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
    /// Builds the hub settings from the stored defaults.
    ///
    /// The stored upload destination becomes active when it still resolves.
    #[must_use]
    pub fn from_saved(saved: &SavedSettings) -> Self {
        let uploader =
            default_destination().and_then(|destination| resolve_destination(&destination).ok());
        Self {
            options: Mutex::new(CaptureOptions {
                directory: saved.folder.clone().unwrap_or_else(default_directory),
                format: saved.format,
                monitor: if saved.primary_monitor {
                    MonitorChoice::Primary
                } else {
                    MonitorChoice::Pointer
                },
                sdr_white_nits: saved.sdr_white_nits,
                auto_interval: Duration::from_secs_f32(saved.interval_seconds),
                delay: Duration::from_secs_f32(saved.delay_seconds),
            }),
            copy: AtomicBool::new(saved.copy || !saved.save),
            save: AtomicBool::new(saved.save),
            upload: AtomicBool::new(uploader.is_some() && saved.upload),
            uploader: Mutex::new(uploader),
        }
    }

    /// Stores the current choices for later runs.
    pub fn store(&self) -> Result<(), String> {
        let options = self.options();
        SavedSettings {
            copy: self.copy.load(Ordering::Relaxed),
            save: self.save.load(Ordering::Relaxed),
            upload: self.upload.load(Ordering::Relaxed),
            folder: Some(options.directory),
            format: options.format,
            delay_seconds: options.delay.as_secs_f32(),
            interval_seconds: options.auto_interval.as_secs_f32(),
            primary_monitor: options.monitor == MonitorChoice::Primary,
            sdr_white_nits: options.sdr_white_nits,
        }
        .store()
    }

    /// Makes a destination active and turns uploads on.
    ///
    /// Returns the display name of the destination.
    pub fn select_destination(&self, destination: &str) -> Result<String, String> {
        let uploader = resolve_destination(destination)?;
        let name = uploader.name.clone();
        if let Ok(mut active) = self.uploader.lock() {
            *active = Some(uploader);
        }
        self.upload.store(true, Ordering::Relaxed);
        Ok(name)
    }

    /// Returns a copy of the current capture defaults.
    #[must_use]
    pub fn options(&self) -> CaptureOptions {
        match self.options.lock() {
            Ok(options) => options.clone(),
            Err(poisoned) => poisoned.into_inner().clone(),
        }
    }

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
