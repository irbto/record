//! Clip selection, waveform loading, preview, and safe trimming.
//!
//! Named clips keep a temporary PCM source while the recorder runs. The TUI
//! uses that source to draw the complete clip and to rebuild a selected range.
//! Windows preview uses the system multimedia service, so no player process or
//! external codec package is required.

#[cfg(windows)]
mod windows;

use std::{
    error::Error,
    fmt,
    fs::File,
    io::{BufReader, Read, Seek, SeekFrom},
    path::Path,
    time::Duration,
};

use anyhow::{Context, Result, bail};

#[cfg(windows)]
pub use windows::PreviewPlayer;

/// Identifies the clip boundary that arrow keys move.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SelectionHandle {
    /// Move the first included frame.
    Start,
    /// Move the exclusive last included frame.
    End,
}

/// Stores a valid editable range inside one temporary PCM source.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClipSelection {
    source_start: u64,
    source_end: u64,
    start: u64,
    end: u64,
    minimum_length: u64,
    sample_rate: u32,
    active: SelectionHandle,
}

/// Describes invalid clip-selection bounds.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SelectionError;

impl fmt::Display for SelectionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("the clip selection must contain audio")
    }
}

impl Error for SelectionError {}

impl ClipSelection {
    /// Creates a full-range selection from absolute source frame positions.
    pub fn new(
        source_start: u64,
        source_end: u64,
        sample_rate: u32,
    ) -> Result<Self, SelectionError> {
        if source_start >= source_end || sample_rate == 0 {
            return Err(SelectionError);
        }
        let length = source_end - source_start;
        let minimum_length = u64::from((sample_rate / 20).max(1)).min(length);
        Ok(Self {
            source_start,
            source_end,
            start: source_start,
            end: source_end,
            minimum_length,
            sample_rate,
            active: SelectionHandle::Start,
        })
    }

    /// Returns the first selected source frame.
    #[must_use]
    pub const fn start_frame(&self) -> u64 {
        self.start
    }

    /// Returns the exclusive last selected source frame.
    #[must_use]
    pub const fn end_frame(&self) -> u64 {
        self.end
    }

    /// Returns the first frame that this editing pass can select.
    #[must_use]
    pub const fn source_start_frame(&self) -> u64 {
        self.source_start
    }

    /// Returns the last frame that this editing pass can select.
    #[must_use]
    pub const fn source_end_frame(&self) -> u64 {
        self.source_end
    }

    /// Returns the encoded sample rate.
    #[must_use]
    pub const fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// Returns the boundary that arrow keys move.
    #[must_use]
    pub const fn active_handle(&self) -> SelectionHandle {
        self.active
    }

    /// Selects the other boundary.
    pub const fn toggle_handle(&mut self) {
        self.active = match self.active {
            SelectionHandle::Start => SelectionHandle::End,
            SelectionHandle::End => SelectionHandle::Start,
        };
    }

    /// Moves the active boundary by a signed frame count.
    pub fn nudge(&mut self, frames: i64) {
        match self.active {
            SelectionHandle::Start => {
                let maximum = self.end.saturating_sub(self.minimum_length);
                self.start = add_signed(self.start, frames).clamp(self.source_start, maximum);
            }
            SelectionHandle::End => {
                let minimum = self.start.saturating_add(self.minimum_length);
                self.end = add_signed(self.end, frames).clamp(minimum, self.source_end);
            }
        }
    }

    /// Restores the full range and selects the start handle.
    pub fn reset(&mut self) {
        self.start = self.source_start;
        self.end = self.source_end;
        self.active = SelectionHandle::Start;
    }

    /// Returns the selected frame count.
    #[must_use]
    pub const fn frames(&self) -> u64 {
        self.end - self.start
    }

    /// Returns the selected duration.
    #[must_use]
    pub fn duration(&self) -> Duration {
        Duration::from_secs_f64(self.frames() as f64 / f64::from(self.sample_rate))
    }

    /// Returns the selection start as a fraction of the source range.
    #[must_use]
    pub fn normalized_start(&self) -> f64 {
        (self.start - self.source_start) as f64 / (self.source_end - self.source_start) as f64
    }

    /// Returns the selection end as a fraction of the source range.
    #[must_use]
    pub fn normalized_end(&self) -> f64 {
        (self.end - self.source_start) as f64 / (self.source_end - self.source_start) as f64
    }
}

fn add_signed(value: u64, change: i64) -> u64 {
    if change.is_negative() {
        value.saturating_sub(change.unsigned_abs())
    } else {
        value.saturating_add(change as u64)
    }
}

/// Stores the minimum and maximum sample values in one display bin.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct WaveformBin {
    /// Lowest normalized sample in this bin.
    pub minimum: f32,
    /// Highest normalized sample in this bin.
    pub maximum: f32,
}

/// Contains downsampled left and right channels for the clip editor.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ClipWaveform {
    /// Left-channel display bins.
    pub left: Vec<WaveformBin>,
    /// Right-channel display bins.
    pub right: Vec<WaveformBin>,
}

/// Reads stereo 16-bit little-endian PCM and builds a bounded waveform.
pub fn load_pcm_waveform(
    path: &Path,
    start_frame: u64,
    end_frame: u64,
    maximum_bins: usize,
) -> Result<ClipWaveform> {
    if start_frame >= end_frame {
        bail!("the clip waveform range is empty");
    }
    if maximum_bins == 0 {
        bail!("the clip waveform needs at least one display bin");
    }

    let file = File::open(path)
        .with_context(|| format!("could not open clip source {}", path.display()))?;
    let available_frames = file.metadata()?.len() / 4;
    if end_frame > available_frames {
        bail!(
            "the clip source has {available_frames} frames, but the selection ends at {end_frame}"
        );
    }
    let frame_count = end_frame - start_frame;
    let bin_count = maximum_bins.min(usize::try_from(frame_count).unwrap_or(usize::MAX));
    let mut waveform = ClipWaveform {
        left: vec![
            WaveformBin {
                minimum: 1.0,
                maximum: -1.0,
            };
            bin_count
        ],
        right: vec![
            WaveformBin {
                minimum: 1.0,
                maximum: -1.0,
            };
            bin_count
        ],
    };

    let mut reader = BufReader::new(file);
    reader.seek(SeekFrom::Start(start_frame.saturating_mul(4)))?;
    let mut processed = 0_u64;
    let mut bytes = vec![0_u8; 64 * 1_024];
    while processed < frame_count {
        let frames = usize::try_from(frame_count - processed)
            .unwrap_or(usize::MAX)
            .min(bytes.len() / 4);
        let byte_count = frames * 4;
        reader
            .read_exact(&mut bytes[..byte_count])
            .context("the temporary clip source ended early")?;
        for (index, frame) in bytes[..byte_count].as_chunks::<4>().0.iter().enumerate() {
            let relative = processed + index as u64;
            let bin =
                usize::try_from(u128::from(relative) * bin_count as u128 / u128::from(frame_count))
                    .unwrap_or(bin_count - 1)
                    .min(bin_count - 1);
            let left = f32::from(i16::from_le_bytes([frame[0], frame[1]])) / f32::from(i16::MAX);
            let right = f32::from(i16::from_le_bytes([frame[2], frame[3]])) / f32::from(i16::MAX);
            update_bin(&mut waveform.left[bin], left);
            update_bin(&mut waveform.right[bin], right);
        }
        processed += frames as u64;
    }

    for bin in waveform.left.iter_mut().chain(&mut waveform.right) {
        if bin.minimum > bin.maximum {
            *bin = WaveformBin::default();
        }
    }
    Ok(waveform)
}

fn update_bin(bin: &mut WaveformBin, value: f32) {
    bin.minimum = bin.minimum.min(value);
    bin.maximum = bin.maximum.max(value);
}

/// Rebuilds an MP3 from an absolute frame range in its temporary PCM source.
#[cfg(windows)]
pub fn trim_clip(
    pcm_path: &Path,
    mp3_path: &Path,
    sample_rate: u32,
    bitrate: u32,
    start_frame: u64,
    end_frame: u64,
) -> Result<u64> {
    crate::audio::trim_pcm_clip(
        pcm_path,
        mp3_path,
        sample_rate,
        bitrate,
        start_frame,
        end_frame,
    )
}

/// Reports that native clip editing is not available on this platform.
#[cfg(not(windows))]
pub fn trim_clip(
    _pcm_path: &Path,
    _mp3_path: &Path,
    _sample_rate: u32,
    _bitrate: u32,
    _start_frame: u64,
    _end_frame: u64,
) -> Result<u64> {
    bail!("native clip editing is available on Windows only")
}

#[cfg(not(windows))]
/// Stub preview player for unsupported platforms.
pub struct PreviewPlayer;

#[cfg(not(windows))]
impl PreviewPlayer {
    /// Returns the platform support error.
    pub fn start(_path: &Path, _start: Duration, _end: Duration) -> Result<Self> {
        bail!("native clip preview is available on Windows only")
    }

    /// Reports that the unsupported player is not active.
    pub const fn is_playing(&self) -> Result<bool> {
        Ok(false)
    }

    /// Does nothing on an unsupported platform.
    pub const fn stop(&mut self) {}
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    #[test]
    fn selection_never_crosses_its_minimum_length() {
        let mut selection = ClipSelection::new(0, 48_000, 48_000).unwrap();
        selection.nudge(i64::MAX);
        assert_eq!(selection.start_frame(), 45_600);
        selection.toggle_handle();
        selection.nudge(i64::MIN);
        assert_eq!(selection.end_frame(), 48_000);
    }

    #[test]
    fn selection_respects_nonzero_source_offsets() {
        let mut selection = ClipSelection::new(10_000, 20_000, 1_000).unwrap();
        selection.nudge(-50_000);
        assert_eq!(selection.start_frame(), 10_000);
        selection.toggle_handle();
        selection.nudge(50_000);
        assert_eq!(selection.end_frame(), 20_000);
    }

    #[test]
    fn toggle_and_reset_restore_the_full_range() {
        let mut selection = ClipSelection::new(0, 10_000, 1_000).unwrap();
        selection.nudge(2_000);
        selection.toggle_handle();
        selection.nudge(-2_000);
        selection.reset();
        assert_eq!(selection.start_frame(), 0);
        assert_eq!(selection.end_frame(), 10_000);
        assert_eq!(selection.active_handle(), SelectionHandle::Start);
    }

    #[test]
    fn selection_reports_normalized_bounds() {
        let mut selection = ClipSelection::new(1_000, 2_000, 1_000).unwrap();
        selection.nudge(250);
        selection.toggle_handle();
        selection.nudge(-250);
        assert!((selection.normalized_start() - 0.25).abs() < f64::EPSILON);
        assert!((selection.normalized_end() - 0.75).abs() < f64::EPSILON);
        assert_eq!(selection.duration(), Duration::from_millis(500));
    }

    #[test]
    fn rejects_empty_selections_and_zero_rates() {
        assert!(ClipSelection::new(1, 1, 48_000).is_err());
        assert!(ClipSelection::new(0, 1, 0).is_err());
    }

    #[test]
    fn waveform_keeps_channel_extremes() {
        let mut source = NamedTempFile::new().unwrap();
        for (left, right) in [
            (i16::MIN, i16::MAX),
            (0, 0),
            (i16::MAX, i16::MIN),
            (1_000, -2_000),
        ] {
            source.write_all(&left.to_le_bytes()).unwrap();
            source.write_all(&right.to_le_bytes()).unwrap();
        }
        source.flush().unwrap();
        let waveform = load_pcm_waveform(source.path(), 0, 4, 2).unwrap();
        assert_eq!(waveform.left.len(), 2);
        assert!(waveform.left[0].minimum <= -1.0);
        assert!(waveform.left[1].maximum >= 0.99);
        assert!(waveform.right[0].maximum >= 0.99);
        assert!(waveform.right[1].minimum <= -1.0);
    }

    #[test]
    fn waveform_rejects_an_out_of_range_request() {
        let source = NamedTempFile::new().unwrap();
        assert!(load_pcm_waveform(source.path(), 0, 1, 10).is_err());
        assert!(load_pcm_waveform(source.path(), 0, 1, 0).is_err());
    }

    #[test]
    fn one_waveform_bin_keeps_extremes_from_the_whole_range() {
        let mut source = NamedTempFile::new().unwrap();
        for sample in [-10_000_i16, 20_000, -30_000, 5_000] {
            source.write_all(&sample.to_le_bytes()).unwrap();
            source.write_all(&(-sample).to_le_bytes()).unwrap();
        }
        source.flush().unwrap();
        let waveform = load_pcm_waveform(source.path(), 0, 4, 1).unwrap();
        assert_eq!(waveform.left.len(), 1);
        assert!(waveform.left[0].minimum < -0.9);
        assert!(waveform.left[0].maximum > 0.6);
        assert!(waveform.right[0].minimum < -0.6);
        assert!(waveform.right[0].maximum > 0.9);
    }

    #[test]
    fn waveform_reads_only_the_requested_frame_range() {
        let mut source = NamedTempFile::new().unwrap();
        for sample in [1_000_i16, 2_000, 3_000] {
            source.write_all(&sample.to_le_bytes()).unwrap();
            source.write_all(&sample.to_le_bytes()).unwrap();
        }
        source.flush().unwrap();
        let waveform = load_pcm_waveform(source.path(), 1, 2, 10).unwrap();
        let expected = 2_000.0 / f32::from(i16::MAX);
        assert_eq!(
            waveform.left,
            vec![WaveformBin {
                minimum: expected,
                maximum: expected
            }]
        );
        assert_eq!(waveform.right, waveform.left);
    }

    #[test]
    fn minimum_selection_length_is_fifty_milliseconds() {
        let mut selection = ClipSelection::new(0, 48_000, 48_000).unwrap();
        selection.nudge(i64::MAX);
        assert_eq!(selection.frames(), 2_400);
        assert_eq!(selection.duration(), Duration::from_millis(50));
    }
}
