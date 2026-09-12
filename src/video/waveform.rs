//! Streaming MP4 audio summaries for video-editor timelines.
//!
//! The extractor decodes PCM a sample at a time and retains only a bounded
//! min/max envelope. Presentation timestamps map samples across the complete
//! MP4 duration, so silent gaps and late audio stay aligned with video frames.

use std::{
    os::windows::ffi::OsStrExt,
    path::Path,
    ptr,
    sync::atomic::{AtomicBool, Ordering},
};

use anyhow::{Context, Result, bail};
use windows::{
    Win32::{
        Media::MediaFoundation::{
            IMFSourceReader, MF_E_INVALIDSTREAMNUMBER, MF_MT_AUDIO_AVG_BYTES_PER_SECOND,
            MF_MT_AUDIO_BITS_PER_SAMPLE, MF_MT_AUDIO_BLOCK_ALIGNMENT, MF_MT_AUDIO_NUM_CHANNELS,
            MF_MT_AUDIO_SAMPLES_PER_SECOND, MF_MT_MAJOR_TYPE, MF_MT_SUBTYPE, MF_PD_DURATION,
            MF_SOURCE_READER_ALL_STREAMS, MF_SOURCE_READER_FIRST_AUDIO_STREAM,
            MF_SOURCE_READER_MEDIASOURCE, MF_SOURCE_READERF_ENDOFSTREAM, MFAudioFormat_PCM,
            MFCreateAttributes, MFCreateMediaType, MFCreateSourceReaderFromURL, MFMediaType_Audio,
        },
        System::{
            Com::StructuredStorage::PROPVARIANT,
            Variant::{VT_I8, VT_UI8},
        },
    },
    core::PCWSTR,
};

use crate::{
    audio::windows::{ComGuard, MediaFoundationGuard},
    clip::WaveformBin,
};

/// Number of decoded frames processed between cancellation checks.
const CANCELLATION_CHECK_FRAMES: usize = 4_096;

/// Result of attempting to summarize the audio stream in one MP4.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) enum WaveformLoad {
    /// A normalized full-duration envelope is ready for rendering.
    Ready(Vec<WaveformBin>),
    /// The MP4 has no usable decoded audio samples.
    #[default]
    Unavailable,
    /// The caller requested that extraction stop before completion.
    Cancelled,
}

/// Builds a bounded mono envelope from every audio channel in an MP4.
///
/// The returned bins span the complete media duration. Empty timeline regions
/// remain zeroed, and an MP4 without an audio stream returns
/// [`WaveformLoad::Unavailable`]. Cancellation is checked between decoded
/// samples and periodically within larger PCM blocks.
pub(crate) fn load_mp4_waveform(
    path: &Path,
    maximum_bins: usize,
    cancelled: &AtomicBool,
) -> Result<WaveformLoad> {
    if maximum_bins == 0 {
        bail!("the video waveform needs at least one display bin");
    }
    if cancelled.load(Ordering::Relaxed) {
        return Ok(WaveformLoad::Cancelled);
    }

    let _com = ComGuard::new()?;
    let _media_foundation = MediaFoundationGuard::new()?;
    let Some(mut reader) = (unsafe { AudioReader::open(path)? }) else {
        return Ok(WaveformLoad::Unavailable);
    };
    if reader.total_frames == 0 {
        return Ok(WaveformLoad::Unavailable);
    }
    let mut waveform = WaveformAccumulator::new(reader.total_frames, maximum_bins);

    loop {
        if cancelled.load(Ordering::Relaxed) {
            return Ok(WaveformLoad::Cancelled);
        }
        let Some(sample) = (unsafe { reader.read()? }) else {
            break;
        };
        if !waveform.push_pcm(
            sample.start_frame,
            &sample.bytes,
            reader.channels,
            cancelled,
        )? {
            return Ok(WaveformLoad::Cancelled);
        }
    }

    Ok(waveform
        .finish()
        .map_or(WaveformLoad::Unavailable, WaveformLoad::Ready))
}

/// One bounded decoded PCM block and its first presentation frame.
struct AudioSample {
    start_frame: u64,
    bytes: Vec<u8>,
}

/// Owns the Media Foundation source reader and negotiated PCM format.
struct AudioReader {
    reader: IMFSourceReader,
    sample_rate: u32,
    channels: usize,
    total_frames: u64,
    ended: bool,
}

impl AudioReader {
    /// Opens the first audio stream and requests interleaved signed PCM16.
    ///
    /// # Safety
    ///
    /// COM and Media Foundation must be initialized on the current thread.
    unsafe fn open(path: &Path) -> Result<Option<Self>> {
        let absolute = std::path::absolute(path)
            .with_context(|| format!("could not resolve {}", path.display()))?;
        let wide = absolute
            .as_os_str()
            .encode_wide()
            .chain(Some(0))
            .collect::<Vec<_>>();
        let mut attributes_slot = None;
        unsafe { MFCreateAttributes(&mut attributes_slot, 1)? };
        let attributes = attributes_slot.context("could not create Media Foundation attributes")?;
        let reader = unsafe { MFCreateSourceReaderFromURL(PCWSTR(wide.as_ptr()), &attributes) }
            .with_context(|| format!("could not open {} for waveform decoding", path.display()))?;
        unsafe {
            reader.SetStreamSelection(MF_SOURCE_READER_ALL_STREAMS.0 as u32, false)?;
        }

        let native = match unsafe {
            reader.GetNativeMediaType(MF_SOURCE_READER_FIRST_AUDIO_STREAM.0 as u32, 0)
        } {
            Ok(media_type) => media_type,
            Err(error) if error.code() == MF_E_INVALIDSTREAMNUMBER => return Ok(None),
            Err(error) => {
                return Err(error).context("could not inspect the MP4 audio stream");
            }
        };
        let sample_rate = unsafe { native.GetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND) }
            .context("the MP4 audio stream has no sample rate")?;
        let channels = unsafe { native.GetUINT32(&MF_MT_AUDIO_NUM_CHANNELS) }
            .context("the MP4 audio stream has no channel count")?;
        if sample_rate == 0 || channels == 0 {
            return Ok(None);
        }
        let block_alignment = channels
            .checked_mul(2)
            .context("the MP4 audio channel count is too large")?;
        let average_bytes = sample_rate
            .checked_mul(block_alignment)
            .context("the decoded PCM byte rate overflowed")?;

        let output = unsafe { MFCreateMediaType()? };
        unsafe {
            output.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio)?;
            output.SetGUID(&MF_MT_SUBTYPE, &MFAudioFormat_PCM)?;
            output.SetUINT32(&MF_MT_AUDIO_NUM_CHANNELS, channels)?;
            output.SetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND, sample_rate)?;
            output.SetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE, 16)?;
            output.SetUINT32(&MF_MT_AUDIO_BLOCK_ALIGNMENT, block_alignment)?;
            output.SetUINT32(&MF_MT_AUDIO_AVG_BYTES_PER_SECOND, average_bytes)?;
            reader.SetStreamSelection(MF_SOURCE_READER_FIRST_AUDIO_STREAM.0 as u32, true)?;
            reader.SetCurrentMediaType(
                MF_SOURCE_READER_FIRST_AUDIO_STREAM.0 as u32,
                None,
                &output,
            )?;
        }

        let duration = unsafe {
            reader.GetPresentationAttribute(MF_SOURCE_READER_MEDIASOURCE.0 as u32, &MF_PD_DURATION)
        }
        .context("the MP4 has no duration for waveform decoding")?;
        let duration = propvariant_hundred_nanos(duration)
            .context("the MP4 has an invalid duration for waveform decoding")?;
        let total_frames = hundred_nanos_to_frames_ceil(duration, sample_rate);

        Ok(Some(Self {
            reader,
            sample_rate,
            channels: channels as usize,
            total_frames,
            ended: false,
        }))
    }

    /// Reads the next decoded PCM block, skipping stream ticks.
    ///
    /// # Safety
    ///
    /// COM and Media Foundation must remain initialized while the reader lives.
    unsafe fn read(&mut self) -> Result<Option<AudioSample>> {
        while !self.ended {
            let mut flags = 0_u32;
            let mut timestamp = 0_i64;
            let mut sample = None;
            unsafe {
                self.reader.ReadSample(
                    MF_SOURCE_READER_FIRST_AUDIO_STREAM.0 as u32,
                    0,
                    None,
                    Some(&mut flags),
                    Some(&mut timestamp),
                    Some(&mut sample),
                )?;
            }
            if flags & MF_SOURCE_READERF_ENDOFSTREAM.0 as u32 != 0 {
                self.ended = true;
            }
            let Some(sample) = sample else {
                continue;
            };
            let buffer = unsafe { sample.ConvertToContiguousBuffer()? };
            let mut data = ptr::null_mut::<u8>();
            let mut length = 0_u32;
            unsafe { buffer.Lock(&mut data, None, Some(&mut length))? };
            let bytes = unsafe { std::slice::from_raw_parts(data, length as usize) }.to_vec();
            unsafe { buffer.Unlock()? };
            return Ok(Some(AudioSample {
                start_frame: hundred_nanos_to_frames(timestamp.max(0) as u64, self.sample_rate),
                bytes,
            }));
        }
        Ok(None)
    }
}

/// Incrementally fills a bounded full-duration min/max envelope.
struct WaveformAccumulator {
    bins: Vec<WaveformBin>,
    touched: Vec<bool>,
    total_frames: u64,
    frames_seen: u64,
}

impl WaveformAccumulator {
    fn new(total_frames: u64, bin_count: usize) -> Self {
        Self {
            bins: vec![
                WaveformBin {
                    minimum: 1.0,
                    maximum: -1.0,
                };
                bin_count
            ],
            touched: vec![false; bin_count],
            total_frames,
            frames_seen: 0,
        }
    }

    /// Adds one interleaved PCM16 block, returning false when cancelled.
    fn push_pcm(
        &mut self,
        start_frame: u64,
        bytes: &[u8],
        channels: usize,
        cancelled: &AtomicBool,
    ) -> Result<bool> {
        let block_alignment = channels
            .checked_mul(2)
            .context("the decoded PCM channel count overflowed")?;
        if channels == 0 || !bytes.len().is_multiple_of(block_alignment) {
            bail!("a decoded PCM block did not contain complete audio frames");
        }

        for (index, frame) in bytes.chunks_exact(block_alignment).enumerate() {
            if index.is_multiple_of(CANCELLATION_CHECK_FRAMES) && cancelled.load(Ordering::Relaxed)
            {
                return Ok(false);
            }
            let absolute_frame = start_frame.saturating_add(index as u64);
            let bin_index = waveform_bin(absolute_frame, self.total_frames, self.bins.len());
            let mut minimum = 1.0_f32;
            let mut maximum = -1.0_f32;
            let (samples, _) = frame.as_chunks::<2>();
            for sample in samples {
                let value = normalize_pcm(i16::from_le_bytes(*sample));
                minimum = minimum.min(value);
                maximum = maximum.max(value);
            }
            let bin = &mut self.bins[bin_index];
            bin.minimum = bin.minimum.min(minimum);
            bin.maximum = bin.maximum.max(maximum);
            self.touched[bin_index] = true;
            self.frames_seen += 1;
        }
        Ok(true)
    }

    /// Zeroes untouched gaps and returns None when no samples were decoded.
    fn finish(mut self) -> Option<Vec<WaveformBin>> {
        if self.frames_seen == 0 {
            return None;
        }
        for (bin, touched) in self.bins.iter_mut().zip(self.touched) {
            if !touched {
                *bin = WaveformBin::default();
            }
        }
        Some(self.bins)
    }
}

/// Maps one presentation frame into a bounded full-duration bin index.
fn waveform_bin(frame: u64, total_frames: u64, bin_count: usize) -> usize {
    if total_frames == 0 || bin_count <= 1 {
        return 0;
    }
    usize::try_from(u128::from(frame) * bin_count as u128 / u128::from(total_frames))
        .unwrap_or(bin_count - 1)
        .min(bin_count - 1)
}

/// Converts signed PCM16 into the inclusive normalized range `-1.0..=1.0`.
fn normalize_pcm(sample: i16) -> f32 {
    let divisor = if sample.is_negative() {
        32_768.0
    } else {
        32_767.0
    };
    f32::from(sample) / divisor
}

/// Reads a non-negative Media Foundation duration in 100-nanosecond units.
fn propvariant_hundred_nanos(value: PROPVARIANT) -> Option<u64> {
    unsafe {
        match value.Anonymous.Anonymous.vt {
            VT_I8 => u64::try_from(value.Anonymous.Anonymous.Anonymous.hVal).ok(),
            VT_UI8 => Some(value.Anonymous.Anonymous.Anonymous.uhVal),
            _ => None,
        }
    }
}

/// Converts a duration to the ceiling of its decoded PCM frame count.
fn hundred_nanos_to_frames_ceil(duration: u64, sample_rate: u32) -> u64 {
    u64::try_from((u128::from(duration) * u128::from(sample_rate)).div_ceil(10_000_000))
        .unwrap_or(u64::MAX)
}

/// Converts one Media Foundation timestamp to a floor PCM frame position.
fn hundred_nanos_to_frames(timestamp: u64, sample_rate: u32) -> u64 {
    u64::try_from(u128::from(timestamp) * u128::from(sample_rate) / 10_000_000).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    use crate::video::{Mp4Writer, Nv12Frame, check_video_support};

    fn pcm_bytes(frames: &[[i16; 2]]) -> Vec<u8> {
        frames
            .iter()
            .flat_map(|frame| frame.iter().flat_map(|sample| sample.to_le_bytes()))
            .collect()
    }

    #[test]
    fn accumulator_normalizes_and_combines_every_channel() {
        let cancelled = AtomicBool::new(false);
        let mut waveform = WaveformAccumulator::new(4, 2);
        waveform
            .push_pcm(
                0,
                &pcm_bytes(&[
                    [i16::MIN, 0],
                    [0, i16::MAX],
                    [-8_192, 16_384],
                    [4_096, -16_384],
                ]),
                2,
                &cancelled,
            )
            .unwrap();
        let bins = waveform.finish().unwrap();
        assert_eq!(bins.len(), 2);
        assert_eq!(bins[0].minimum, -1.0);
        assert_eq!(bins[0].maximum, 1.0);
        assert!(bins[1].minimum <= -0.5);
        assert!(bins[1].maximum >= 0.5);
    }

    #[test]
    fn accumulator_keeps_timestamp_gaps_zeroed() {
        let cancelled = AtomicBool::new(false);
        let mut waveform = WaveformAccumulator::new(8, 4);
        waveform
            .push_pcm(4, &pcm_bytes(&[[16_384, 16_384]]), 2, &cancelled)
            .unwrap();
        let bins = waveform.finish().unwrap();
        assert_eq!(bins[0], WaveformBin::default());
        assert_eq!(bins[1], WaveformBin::default());
        assert!(bins[2].maximum >= 0.5);
        assert_eq!(bins[3], WaveformBin::default());
    }

    #[test]
    fn cancellation_stops_before_opening_the_source() {
        let cancelled = AtomicBool::new(true);
        assert_eq!(
            load_mp4_waveform(Path::new("missing.mp4"), 64, &cancelled).unwrap(),
            WaveformLoad::Cancelled
        );
    }

    #[test]
    fn duration_and_bin_math_cover_the_full_timeline() {
        assert_eq!(hundred_nanos_to_frames_ceil(10_000_001, 48_000), 48_001);
        assert_eq!(waveform_bin(0, 100, 4), 0);
        assert_eq!(waveform_bin(50, 100, 4), 2);
        assert_eq!(waveform_bin(100, 100, 4), 3);
    }

    #[test]
    fn media_duration_accepts_signed_and_unsigned_variants() {
        let mut signed = PROPVARIANT::default();
        let mut unsigned = PROPVARIANT::default();
        unsafe {
            (*signed.Anonymous.Anonymous).vt = VT_I8;
            (*signed.Anonymous.Anonymous).Anonymous.hVal = 10_000_000;
            (*unsigned.Anonymous.Anonymous).vt = VT_UI8;
            (*unsigned.Anonymous.Anonymous).Anonymous.uhVal = 20_000_000;
        }
        assert_eq!(propvariant_hundred_nanos(signed), Some(10_000_000));
        assert_eq!(propvariant_hundred_nanos(unsigned), Some(20_000_000));
    }

    #[test]
    fn recorded_mp4_audio_produces_a_bounded_envelope() {
        if check_video_support().is_err() {
            return;
        }
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("waveform-source.mp4");
        write_test_mp4(&path, true);

        let cancelled = AtomicBool::new(false);
        let waveform = load_mp4_waveform(&path, 64, &cancelled).unwrap();
        let WaveformLoad::Ready(bins) = waveform else {
            panic!("recorded audio did not produce a waveform");
        };
        assert_eq!(bins.len(), 64);
        assert!(bins.iter().any(|bin| bin.maximum > 0.2));
        assert!(bins.iter().any(|bin| bin.minimum < -0.2));
    }

    #[test]
    fn mp4_without_audio_samples_is_unavailable() {
        if check_video_support().is_err() {
            return;
        }
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("waveform-silent-source.mp4");
        write_test_mp4(&path, false);

        let cancelled = AtomicBool::new(false);
        assert_eq!(
            load_mp4_waveform(&path, 64, &cancelled).unwrap(),
            WaveformLoad::Unavailable
        );
    }

    /// Writes one short native MP4 with an optional second of stereo audio.
    fn write_test_mp4(path: &Path, with_audio: bool) {
        let _com = ComGuard::new().unwrap();
        let _media_foundation = MediaFoundationGuard::new().unwrap();
        let mut writer = unsafe {
            Mp4Writer::open(
                path, 64, 48, 10, 2_000_000, 48_000, 192_000, true, None, 0.0,
            )
        }
        .unwrap();
        for frame in 0..10_u64 {
            unsafe {
                writer
                    .write_video_frame(
                        &Nv12Frame::black(64, 48),
                        Duration::from_millis(frame * 100),
                    )
                    .unwrap();
            }
        }
        if with_audio {
            let mut pcm = Vec::with_capacity(48_000 * 2);
            for frame in 0..48_000 {
                let sample = if (12_000..36_000).contains(&frame) {
                    16_384_i16
                } else {
                    0
                };
                pcm.extend([sample, -sample]);
            }
            unsafe { writer.write_audio(&pcm).unwrap() };
        }
        unsafe { writer.close().unwrap() };
    }
}
