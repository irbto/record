//! The plain values that cross between the engines and the interface.
//!
//! The interface is a web page. It receives these values as JSON events and
//! command results. Nothing here depends on Windows or on the window shell.

use std::path::Path;

use serde::{Deserialize, Serialize};

/// One of the three things the program records.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    /// System audio to MP3.
    Audio,
    /// Screen and system audio to MP4.
    Video,
    /// Still images of the screen.
    Screenshot,
}

impl Mode {
    /// Returns the file extension of this mode's recordings.
    #[must_use]
    pub const fn extension(self) -> &'static str {
        match self {
            Self::Audio => "mp3",
            Self::Video => "mp4",
            Self::Screenshot => "png",
        }
    }
}

/// One report from a running audio recording.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum SoundUpdate {
    /// Capture started with these actual values.
    #[serde(rename_all = "camelCase")]
    Started {
        /// Sample rate in frames per second.
        sample_rate: u32,
        /// MP3 bit rate in bits per second.
        bitrate: u32,
    },
    /// The shape of the newest audio block, about 47 times each second.
    #[serde(rename_all = "camelCase")]
    Block {
        /// Lowest and highest left sample from -1 to 1.
        left: [f32; 2],
        /// Lowest and highest right sample from -1 to 1.
        right: [f32; 2],
        /// Encoded time so far in seconds.
        seconds: f64,
        /// Loudness from 0 to 1 in bands from low to high frequency.
        bands: Vec<f32>,
    },
    /// Capture stopped and the file is being closed.
    Finalizing,
    /// One file of the session is complete: a part or a named clip.
    Saved {
        /// File name without its folder.
        name: String,
        /// Full path, for the commands that open or edit the file.
        path: String,
        /// Reports whether the user named this file as a clip.
        clip: bool,
        /// Length in seconds.
        seconds: f64,
        /// Reports whether the clip can still be trimmed.
        editable: bool,
    },
    /// A short message from the engine.
    Notice {
        /// The message text.
        text: String,
    },
    /// The recording ended.
    Stopped {
        /// Name of the saved file, when the recording succeeded.
        file: Option<String>,
        /// The reason for a failure.
        error: Option<String>,
    },
}

/// The shape of one audio clip for the trim view.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ClipShape {
    /// Lowest and highest left sample of each column.
    pub left: Vec<[f32; 2]>,
    /// Lowest and highest right sample of each column.
    pub right: Vec<[f32; 2]>,
    /// Length of the clip in seconds.
    pub seconds: f64,
}

/// One video that is open in the editor.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct VideoInfo {
    /// File name without its folder.
    pub name: String,
    /// Full path of the file.
    pub path: String,
    /// Picture width in pixels.
    pub width: u32,
    /// Picture height in pixels.
    pub height: u32,
    /// Frames per second.
    pub fps: u32,
    /// Number of frames.
    pub frames: u64,
}

/// The edit that the interface asks for. Times are in seconds.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct VideoEdit {
    /// First frame to keep.
    pub start_frame: u64,
    /// Frame after the last frame to keep.
    pub end_frame: u64,
    /// Length of the audio fade at the start.
    pub audio_fade_in: f64,
    /// Length of the audio fade at the end.
    pub audio_fade_out: f64,
    /// Length of the picture fade at the start.
    pub video_fade_in: f64,
    /// Length of the picture fade at the end.
    pub video_fade_out: f64,
    /// Pixels to remove at the left edge.
    pub crop_left: u32,
    /// Pixels to remove at the right edge.
    pub crop_right: u32,
    /// Pixels to remove at the top edge.
    pub crop_top: u32,
    /// Pixels to remove at the bottom edge.
    pub crop_bottom: u32,
}

/// One report from a running video export.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum EditUpdate {
    /// The export entered a stage without a count.
    Stage {
        /// Name of the stage.
        name: &'static str,
    },
    /// Frames written so far.
    Progress {
        /// Number of frames written.
        done: u64,
        /// Number of frames to write.
        total: u64,
    },
    /// The export ended. The editor shows the file as it is now.
    Done {
        /// The file after the export, when it opens.
        info: Option<VideoInfo>,
        /// The reason for a failure.
        error: Option<String>,
    },
}

/// One report from a running video recording.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
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
    /// Capture stopped and the file is being closed.
    Finalizing,
    /// A short message from the engine.
    Notice {
        /// The message text.
        text: String,
    },
    /// The recording ended.
    Stopped {
        /// Name of the saved file, when the recording succeeded.
        file: Option<String>,
        /// The reason for a failure.
        error: Option<String>,
    },
}

/// One report from the screenshot engine.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum ShotUpdate {
    /// The engine runs. Each flag reports one registered hotkey: region,
    /// fullscreen, and active window.
    Ready {
        /// Registration result of each global hotkey.
        hotkeys: [bool; 3],
    },
    /// A capture started.
    Capturing,
    /// A capture finished.
    #[serde(rename_all = "camelCase")]
    Finished {
        /// Name of the saved file, when one was written.
        file: Option<String>,
        /// Image width in pixels.
        width: u32,
        /// Image height in pixels.
        height: u32,
        /// Link of the uploaded image.
        url: Option<String>,
        /// Reports whether the clipboard holds the image or the link.
        copied: bool,
        /// SDR white level used for an HDR monitor, in nits.
        hdr_nits: Option<f32>,
        /// The reason an upload or a clipboard copy failed.
        error: Option<String>,
    },
    /// The user closed the overlay without saving.
    Cancelled,
    /// A capture failed.
    Failed {
        /// The reason.
        error: String,
    },
    /// Auto capture changed state.
    Auto {
        /// Reports whether auto capture runs now.
        active: bool,
    },
}

/// One attached monitor for the video source list.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct Monitor {
    /// Index that the video engine accepts.
    pub index: u32,
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// Reports whether this is the primary monitor.
    pub primary: bool,
    /// Left edge on the desktop in pixels.
    pub left: i32,
    /// Top edge on the desktop in pixels.
    pub top: i32,
}

/// One saved recording or screenshot.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileEntry {
    /// File name without its folder.
    pub name: String,
    /// Full path, for the commands that open or copy the file.
    pub path: String,
    /// Seconds since the file changed.
    pub age_seconds: u64,
}

/// One upload destination in the destination list.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Destination {
    /// Identifier that selects the destination.
    pub id: String,
    /// Name shown in the list.
    pub name: String,
    /// One short fact, such as `ready` or `needs an API key`.
    pub note: String,
    /// What the destination calls its key, when it still needs one.
    pub key_label: Option<String>,
    /// Where the user gets the key.
    pub key_source: String,
}

/// Every screenshot choice and setting, sent after each change.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    /// Places each image on the clipboard.
    pub copy: bool,
    /// Writes each image to a PNG file.
    pub save: bool,
    /// Uploads each image and copies the link.
    pub upload: bool,
    /// Name of the active upload destination.
    pub destination: Option<String>,
    /// Every destination the user can select.
    pub destinations: Vec<Destination>,
    /// Folder that receives screenshots.
    pub folder: String,
    /// Name of the file format of saved screenshots.
    pub format: String,
    /// Every file format that the user can select.
    pub formats: Vec<String>,
    /// The close button hides the window and the program stays in the tray.
    pub close_to_tray: bool,
    /// Folder that receives audio recordings.
    pub audio_folder: String,
    /// Folder that receives video recordings.
    pub video_folder: String,
    /// Seconds that a delayed capture waits.
    pub delay_seconds: f32,
    /// Seconds between two auto captures.
    pub interval_seconds: f32,
    /// Captures the primary monitor in place of the monitor under the pointer.
    pub primary_monitor: bool,
    /// Fixed SDR white level in nits. `None` uses the Windows value.
    pub sdr_white_nits: Option<f32>,
    /// Reports whether the tray starts at sign-in.
    pub autostart: bool,
    /// Registration result of each global hotkey: region, fullscreen, and
    /// active window.
    pub hotkeys: [bool; 3],
    /// Reports whether auto capture runs now.
    pub auto_active: bool,
    /// The reason the screenshot engine did not start.
    pub engine_error: Option<String>,
    /// Program version.
    pub version: String,
}

/// Returns the file name of a path for messages.
#[must_use]
pub fn file_name(path: &Path) -> String {
    path.file_name().map_or_else(
        || path.display().to_string(),
        |name| name.to_string_lossy().into_owned(),
    )
}

/// Returns the lowest and the highest sample of a block.
///
/// An empty block gives silence.
#[must_use]
pub fn extent(samples: &[f32]) -> [f32; 2] {
    samples.iter().fold([0.0, 0.0], |[low, high], sample| {
        [low.min(*sample), high.max(*sample)]
    })
}

/// Lowest frequency of the first spectrum band in hertz.
const BAND_FLOOR_HZ: f32 = 40.0;
/// Highest frequency of the last spectrum band in hertz.
const BAND_CEILING_HZ: f32 = 20_000.0;

/// Groups linear spectrum bins into bands of equal musical width.
///
/// `bins` run from zero hertz to half the sample rate. Each band takes the
/// loudest bin in its range, so a single tone stays visible.
#[must_use]
pub fn spectrum_bands(bins: &[f32], sample_rate: u32, count: usize) -> Vec<f32> {
    if bins.is_empty() || count == 0 || sample_rate == 0 {
        return vec![0.0; count];
    }
    let nyquist = sample_rate as f32 / 2.0;
    let ceiling = BAND_CEILING_HZ.min(nyquist);
    let ratio = ceiling / BAND_FLOOR_HZ;
    let bin_of = |hertz: f32| ((hertz / nyquist) * bins.len() as f32) as usize;
    (0..count)
        .map(|band| {
            let low = BAND_FLOOR_HZ * ratio.powf(band as f32 / count as f32);
            let high = BAND_FLOOR_HZ * ratio.powf((band + 1) as f32 / count as f32);
            let first = bin_of(low).min(bins.len() - 1);
            let last = bin_of(high).clamp(first + 1, bins.len());
            bins[first..last]
                .iter()
                .fold(0.0_f32, |peak, bin| peak.max(*bin))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_extent_of_a_block_spans_its_samples() {
        assert_eq!(extent(&[0.25, -0.5, 0.125]), [-0.5, 0.25]);
        assert_eq!(extent(&[0.5, 0.75]), [0.0, 0.75]);
        assert_eq!(extent(&[]), [0.0, 0.0]);
    }

    #[test]
    fn bands_place_a_tone_by_its_frequency() {
        // 1024 bins at 48 kHz are 23.4 Hz wide. Bin 43 is about 1 kHz.
        let mut bins = vec![0.0_f32; 1_024];
        bins[43] = 0.8;
        let bands = spectrum_bands(&bins, 48_000, 24);
        let loudest = bands
            .iter()
            .enumerate()
            .max_by(|first, second| first.1.total_cmp(second.1))
            .map(|(index, _)| index);
        assert_eq!(bands.len(), 24);
        assert_eq!(loudest, Some(12));
        assert_eq!(bands.iter().filter(|band| **band > 0.0).count(), 1);
    }

    #[test]
    fn bands_cover_every_bin_range_without_a_panic() {
        for (length, rate, count) in [(1, 48_000, 24), (8, 8_000, 24), (1_024, 44_100, 3)] {
            let bins = vec![0.5_f32; length];
            let bands = spectrum_bands(&bins, rate, count);
            assert_eq!(bands.len(), count);
            assert!(bands.iter().all(|band| *band == 0.5));
        }
        assert_eq!(spectrum_bands(&[], 48_000, 4), [0.0; 4]);
    }

    #[test]
    fn updates_use_the_field_names_the_interface_reads() {
        let block = SoundUpdate::Block {
            left: [-0.5, 0.25],
            right: [0.0, 0.0],
            seconds: 1.5,
            bands: vec![0.0],
        };
        let json = serde_json::to_string(&block).unwrap();
        assert_eq!(
            json,
            r#"{"kind":"block","left":[-0.5,0.25],"right":[0.0,0.0],"seconds":1.5,"bands":[0.0]}"#
        );
        let started = serde_json::to_string(&SoundUpdate::Started {
            sample_rate: 48_000,
            bitrate: 320_000,
        })
        .unwrap();
        assert_eq!(
            started,
            r#"{"kind":"started","sampleRate":48000,"bitrate":320000}"#
        );
        let finished = serde_json::to_string(&ShotUpdate::Finished {
            file: None,
            width: 8,
            height: 6,
            url: None,
            copied: true,
            hdr_nits: Some(240.0),
            error: None,
        })
        .unwrap();
        assert!(finished.contains(r#""hdrNits":240.0"#), "{finished}");
        assert_eq!(
            serde_json::to_string(&Mode::Screenshot).unwrap(),
            r#""screenshot""#
        );
    }
}
