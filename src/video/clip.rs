//! Frame-accurate MP4 trimming for the integrated video editor.
//!
//! Media Foundation decodes the selected H.264/AAC range to NV12 and PCM,
//! then the recorder's existing sink writer rebuilds the MP4. The replacement
//! is written beside the source and swapped in only after finalization succeeds.

use std::{
    fs,
    os::windows::ffi::OsStrExt,
    path::{Path, PathBuf},
    ptr,
    time::Duration,
};

use anyhow::{Context, Result, anyhow, bail};
use windows::{
    Win32::{
        Media::MediaFoundation::{
            IMFSourceReader, MF_MT_AUDIO_AVG_BYTES_PER_SECOND, MF_MT_AUDIO_BITS_PER_SAMPLE,
            MF_MT_AUDIO_BLOCK_ALIGNMENT, MF_MT_AUDIO_NUM_CHANNELS, MF_MT_AUDIO_SAMPLES_PER_SECOND,
            MF_MT_DEFAULT_STRIDE, MF_MT_FRAME_RATE, MF_MT_FRAME_SIZE, MF_MT_MAJOR_TYPE,
            MF_MT_SUBTYPE, MF_SOURCE_READER_ALL_STREAMS,
            MF_SOURCE_READER_ENABLE_ADVANCED_VIDEO_PROCESSING, MF_SOURCE_READER_FIRST_AUDIO_STREAM,
            MF_SOURCE_READER_FIRST_VIDEO_STREAM, MF_SOURCE_READERF_ENDOFSTREAM, MFAudioFormat_PCM,
            MFCreateAttributes, MFCreateMediaType, MFCreateSourceReaderFromURL, MFMediaType_Audio,
            MFMediaType_Video, MFVideoFormat_NV12,
        },
        Storage::FileSystem::{REPLACEFILE_WRITE_THROUGH, ReplaceFileW},
        System::{Com::StructuredStorage::PROPVARIANT, Variant::VT_I8},
    },
    core::{GUID, PCWSTR},
};

use super::{Mp4Writer, Nv12Frame, VideoSummary};
use crate::audio::windows::{ComGuard, MediaFoundationGuard};

/// Re-encodes `start_frame..end_frame` and safely replaces the source MP4.
pub fn trim_video(
    path: &Path,
    summary: &VideoSummary,
    start_frame: u64,
    end_frame: u64,
) -> Result<VideoSummary> {
    if start_frame >= end_frame || end_frame > summary.frames {
        bail!("the video selection is outside the recorded frame range");
    }
    if start_frame == 0 && end_frame == summary.frames {
        return Ok(summary.clone());
    }

    let temporary = temporary_output(path)?;
    let result = trim_video_inner(&temporary, path, summary, start_frame, end_frame);
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    let mut trimmed = result?;
    if let Err(error) = replace_source(path, &temporary) {
        let _ = fs::remove_file(&temporary);
        return Err(error);
    }
    trimmed.output = path.to_path_buf();
    Ok(trimmed)
}

/// Runs every COM-bound decode and encode operation on the caller's worker thread.
fn trim_video_inner(
    temporary: &Path,
    source: &Path,
    summary: &VideoSummary,
    start_frame: u64,
    end_frame: u64,
) -> Result<VideoSummary> {
    let _com = ComGuard::new()?;
    let _media_foundation = MediaFoundationGuard::new()?;
    let mut reader = unsafe { ClipReader::open(source, summary)? };
    let mut writer = unsafe {
        Mp4Writer::open(
            temporary,
            summary.width,
            summary.height,
            summary.fps,
            summary.video_bitrate,
            summary.sample_rate,
            summary.audio_bitrate,
            true,
            None,
            0.0,
        )?
    };

    let start = frame_time(start_frame, summary.fps);
    unsafe { reader.seek(start)? };
    let target_video_frames = end_frame - start_frame;
    let source_audio_start = scale_frames(start_frame, summary.sample_rate, summary.fps);
    let target_audio_frames = scale_frames(target_video_frames, summary.sample_rate, summary.fps);
    let mut written_video = 0_u64;
    let mut written_audio = 0_u64;
    let mut video_ended = false;
    let mut audio_ended = false;

    while (!video_ended && written_video < target_video_frames)
        || (!audio_ended && written_audio < target_audio_frames)
    {
        if !video_ended && written_video < target_video_frames {
            match unsafe { reader.read_video()? } {
                Some(sample) => {
                    if sample.timestamp >= hundred_nanos(start) {
                        let frame = Nv12Frame {
                            data: pack_nv12(
                                &sample.bytes,
                                summary.width,
                                summary.height,
                                reader.video_stride,
                            )?,
                        };
                        let timestamp = frame_time(written_video, summary.fps);
                        unsafe { writer.write_video_frame(&frame, timestamp)? };
                        written_video += 1;
                    }
                }
                None => video_ended = true,
            }
        }

        if !audio_ended && written_audio < target_audio_frames {
            match unsafe { reader.read_audio()? } {
                Some(sample) => {
                    let sample_start =
                        hundred_nanos_to_frames(sample.timestamp, summary.sample_rate);
                    let sample_frames = (sample.bytes.len() / 4) as u64;
                    let wanted_source_frame = source_audio_start + written_audio;
                    let sample_end = sample_start.saturating_add(sample_frames);
                    if sample_end > wanted_source_frame {
                        if sample_start > wanted_source_frame {
                            let gap = (sample_start - wanted_source_frame)
                                .min(target_audio_frames - written_audio);
                            write_silence(&mut writer, gap)?;
                            written_audio += gap;
                        }
                        let wanted_source_frame = source_audio_start + written_audio;
                        if sample_end > wanted_source_frame && written_audio < target_audio_frames {
                            let offset = wanted_source_frame.saturating_sub(sample_start);
                            let available = sample_frames.saturating_sub(offset);
                            let frames = available.min(target_audio_frames - written_audio);
                            let pcm = pcm_slice(&sample.bytes, offset, frames)?;
                            unsafe { writer.write_audio(&pcm)? };
                            written_audio += frames;
                        }
                    }
                }
                None => audio_ended = true,
            }
        }
    }

    if written_video == 0 {
        bail!("the selected range did not contain a decodable video frame");
    }
    if written_audio < target_audio_frames {
        write_silence(&mut writer, target_audio_frames - written_audio)?;
    }
    unsafe { writer.close() }
}

/// One decoded Media Foundation sample and its original presentation time.
struct DecodedSample {
    timestamp: i64,
    bytes: Vec<u8>,
}

/// Owns the two decoded streams selected from a finished recording.
struct ClipReader {
    reader: IMFSourceReader,
    video_stride: usize,
}

impl ClipReader {
    /// Opens H.264 as NV12 and AAC as stereo PCM16.
    ///
    /// # Safety
    ///
    /// COM and Media Foundation must be initialized on the current thread.
    unsafe fn open(path: &Path, summary: &VideoSummary) -> Result<Self> {
        let absolute = std::path::absolute(path)
            .with_context(|| format!("could not resolve {}", path.display()))?;
        let wide = wide_path(&absolute);
        let mut attributes_slot = None;
        unsafe { MFCreateAttributes(&mut attributes_slot, 4)? };
        let attributes = attributes_slot.context("could not create Media Foundation attributes")?;
        unsafe {
            attributes.SetUINT32(&MF_SOURCE_READER_ENABLE_ADVANCED_VIDEO_PROCESSING, 1)?;
        }
        let reader = unsafe { MFCreateSourceReaderFromURL(PCWSTR(wide.as_ptr()), &attributes) }
            .with_context(|| format!("could not open {} for trimming", path.display()))?;
        unsafe {
            reader.SetStreamSelection(MF_SOURCE_READER_ALL_STREAMS.0 as u32, false)?;
            reader.SetStreamSelection(MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32, true)?;
            reader.SetStreamSelection(MF_SOURCE_READER_FIRST_AUDIO_STREAM.0 as u32, true)?;
        }

        let video = unsafe { MFCreateMediaType()? };
        unsafe {
            video.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
            video.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_NV12)?;
            video.SetUINT64(
                &MF_MT_FRAME_SIZE,
                (u64::from(summary.width) << 32) | u64::from(summary.height),
            )?;
            video.SetUINT64(&MF_MT_FRAME_RATE, u64::from(summary.fps.max(1)) << 32 | 1)?;
            reader.SetCurrentMediaType(
                MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32,
                None,
                &video,
            )?;
        }
        let current_video =
            unsafe { reader.GetCurrentMediaType(MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32)? };
        let video_stride = unsafe { current_video.GetUINT32(&MF_MT_DEFAULT_STRIDE) }
            .map_or(summary.width as usize, |stride| stride as usize);

        let audio = unsafe { MFCreateMediaType()? };
        unsafe {
            audio.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio)?;
            audio.SetGUID(&MF_MT_SUBTYPE, &MFAudioFormat_PCM)?;
            audio.SetUINT32(&MF_MT_AUDIO_NUM_CHANNELS, 2)?;
            audio.SetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND, summary.sample_rate)?;
            audio.SetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE, 16)?;
            audio.SetUINT32(&MF_MT_AUDIO_BLOCK_ALIGNMENT, 4)?;
            audio.SetUINT32(&MF_MT_AUDIO_AVG_BYTES_PER_SECOND, summary.sample_rate * 4)?;
            reader.SetCurrentMediaType(
                MF_SOURCE_READER_FIRST_AUDIO_STREAM.0 as u32,
                None,
                &audio,
            )?;
        }
        Ok(Self {
            reader,
            video_stride,
        })
    }

    /// Seeks both selected streams to the requested media time.
    unsafe fn seek(&mut self, time: Duration) -> Result<()> {
        let mut position = PROPVARIANT::default();
        unsafe {
            (*position.Anonymous.Anonymous).vt = VT_I8;
            (*position.Anonymous.Anonymous).Anonymous.hVal = hundred_nanos(time);
            self.reader.SetCurrentPosition(&GUID::zeroed(), &position)?;
        }
        Ok(())
    }

    /// Reads the next decoded NV12 sample.
    unsafe fn read_video(&mut self) -> Result<Option<DecodedSample>> {
        unsafe { self.read(MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32) }
    }

    /// Reads the next decoded PCM sample.
    unsafe fn read_audio(&mut self) -> Result<Option<DecodedSample>> {
        unsafe { self.read(MF_SOURCE_READER_FIRST_AUDIO_STREAM.0 as u32) }
    }

    /// Reads through stream ticks until a sample or end-of-stream flag arrives.
    unsafe fn read(&mut self, stream: u32) -> Result<Option<DecodedSample>> {
        loop {
            let mut flags = 0_u32;
            let mut timestamp = 0_i64;
            let mut sample = None;
            unsafe {
                self.reader.ReadSample(
                    stream,
                    0,
                    None,
                    Some(&mut flags),
                    Some(&mut timestamp),
                    Some(&mut sample),
                )?;
            }
            if let Some(sample) = sample {
                let buffer = unsafe { sample.ConvertToContiguousBuffer()? };
                let mut data = ptr::null_mut();
                let mut length = 0_u32;
                unsafe { buffer.Lock(&mut data, None, Some(&mut length))? };
                let bytes = unsafe { std::slice::from_raw_parts(data, length as usize) }.to_vec();
                unsafe { buffer.Unlock()? };
                return Ok(Some(DecodedSample { timestamp, bytes }));
            }
            if flags & MF_SOURCE_READERF_ENDOFSTREAM.0 as u32 != 0 {
                return Ok(None);
            }
        }
    }
}

/// Packs potentially padded NV12 rows for the recorder's sink writer.
fn pack_nv12(bytes: &[u8], width: u32, height: u32, stride: usize) -> Result<Vec<u8>> {
    let width = width as usize;
    let height = height as usize;
    let stride = stride.max(width);
    let needed = stride
        .checked_mul(height + height / 2)
        .context("the decoded NV12 dimensions overflowed")?;
    if bytes.len() < needed {
        bail!(
            "a decoded NV12 frame had {} bytes; expected at least {needed}",
            bytes.len()
        );
    }
    if stride == width {
        return Ok(bytes[..width * (height + height / 2)].to_vec());
    }
    let mut packed = Vec::with_capacity(width * (height + height / 2));
    for row in 0..height + height / 2 {
        let start = row * stride;
        packed.extend_from_slice(&bytes[start..start + width]);
    }
    Ok(packed)
}

/// Converts the selected stereo PCM byte range to the sink writer's samples.
fn pcm_slice(bytes: &[u8], offset_frames: u64, frames: u64) -> Result<Vec<i16>> {
    let start = usize::try_from(
        offset_frames
            .checked_mul(4)
            .context("PCM offset overflowed")?,
    )?;
    let length = usize::try_from(frames.checked_mul(4).context("PCM length overflowed")?)?;
    let selected = bytes
        .get(start..start + length)
        .context("a decoded PCM sample ended before the selected range")?;
    Ok(selected
        .as_chunks::<2>()
        .0
        .iter()
        .map(|sample| i16::from_le_bytes(*sample))
        .collect())
}

/// Submits bounded stereo silence when timestamps expose an audio gap.
fn write_silence(writer: &mut Mp4Writer, frames: u64) -> Result<()> {
    let mut remaining = frames;
    let silence = vec![0_i16; 8_192 * 2];
    while remaining > 0 {
        let block = remaining.min(8_192) as usize;
        unsafe { writer.write_audio(&silence[..block * 2])? };
        remaining -= block as u64;
    }
    Ok(())
}

/// Scales video frames into another fixed-rate timeline without floating point.
fn scale_frames(frames: u64, target_rate: u32, source_rate: u32) -> u64 {
    if source_rate == 0 {
        return 0;
    }
    u64::try_from(
        (u128::from(frames) * u128::from(target_rate) + u128::from(source_rate / 2))
            / u128::from(source_rate),
    )
    .unwrap_or(u64::MAX)
}

/// Returns an exact presentation time for a constant-rate video frame.
fn frame_time(frame: u64, fps: u32) -> Duration {
    if fps == 0 {
        Duration::ZERO
    } else {
        Duration::from_secs_f64(frame as f64 / f64::from(fps))
    }
}

/// Converts a media timestamp to the floor of its PCM frame position.
fn hundred_nanos_to_frames(timestamp: i64, sample_rate: u32) -> u64 {
    u64::try_from(u128::from(timestamp.max(0) as u64) * u128::from(sample_rate) / 10_000_000)
        .unwrap_or(u64::MAX)
}

/// Converts a duration into Media Foundation's 100-nanosecond units.
fn hundred_nanos(time: Duration) -> i64 {
    i64::try_from(time.as_nanos() / 100).unwrap_or(i64::MAX)
}

/// Chooses a unique temporary MP4 beside the recording for an atomic swap.
fn temporary_output(path: &Path) -> Result<PathBuf> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let stem = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("recording");
    for index in 0..1_000_u32 {
        let candidate = parent.join(format!(
            ".{stem}.record-trim-{}-{index}.mp4",
            std::process::id()
        ));
        if !candidate.exists() {
            return Ok(candidate);
        }
    }
    Err(anyhow!("could not choose a temporary video trim path"))
}

/// Replaces the original only after the temporary MP4 is fully finalized.
fn replace_source(source: &Path, replacement: &Path) -> Result<()> {
    let source = wide_path(&std::path::absolute(source)?);
    let replacement = wide_path(&std::path::absolute(replacement)?);
    unsafe {
        ReplaceFileW(
            PCWSTR(source.as_ptr()),
            PCWSTR(replacement.as_ptr()),
            PCWSTR::null(),
            REPLACEFILE_WRITE_THROUGH,
            None,
            None,
        )
    }
    .context("Windows could not replace the recording with the trimmed MP4")
}

/// Encodes a Windows path as a nul-terminated UTF-16 string.
fn wide_path(path: &Path) -> Vec<u16> {
    path.as_os_str().encode_wide().chain(Some(0)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_scaling_rounds_to_the_nearest_target_frame() {
        assert_eq!(scale_frames(60, 48_000, 60), 48_000);
        assert_eq!(scale_frames(1, 48_000, 60), 800);
        assert_eq!(scale_frames(1, 44_100, 60), 735);
    }

    #[test]
    fn pcm_slicing_keeps_stereo_frame_boundaries() {
        let samples = [1_i16, 2, 3, 4, 5, 6];
        let bytes = samples
            .iter()
            .flat_map(|sample| sample.to_le_bytes())
            .collect::<Vec<_>>();
        assert_eq!(pcm_slice(&bytes, 1, 2).unwrap(), vec![3, 4, 5, 6]);
    }

    #[test]
    fn nv12_packing_removes_row_padding() {
        let bytes = [1, 2, 3, 4, 9, 9, 5, 6, 7, 8, 9, 9, 10, 11, 12, 13, 9, 9];
        assert_eq!(
            pack_nv12(&bytes, 4, 2, 6).unwrap(),
            vec![1, 2, 3, 4, 5, 6, 7, 8, 10, 11, 12, 13]
        );
    }

    #[cfg(windows)]
    #[test]
    fn trims_a_native_mp4_and_replaces_it() {
        if super::super::check_video_support().is_err() {
            return;
        }
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("trim-source.mp4");
        let original = {
            let _com = ComGuard::new().unwrap();
            let _media_foundation = MediaFoundationGuard::new().unwrap();
            let mut writer = unsafe {
                Mp4Writer::open(
                    &path, 64, 48, 10, 2_000_000, 48_000, 192_000, true, None, 0.0,
                )
            }
            .unwrap();
            for frame in 0..10 {
                unsafe {
                    writer
                        .write_video_frame(
                            &Nv12Frame::black(64, 48),
                            Duration::from_millis(frame * 100),
                        )
                        .unwrap();
                }
            }
            unsafe { writer.write_audio(&vec![0_i16; 48_000 * 2]).unwrap() };
            unsafe { writer.close().unwrap() }
        };
        let (commands, events, decoder) =
            super::super::preview::spawn_preview_worker(path.clone()).unwrap();
        loop {
            match events.recv_timeout(Duration::from_secs(10)).unwrap() {
                super::super::preview::PreviewEvent::Ready(_) => break,
                super::super::preview::PreviewEvent::Failed(message) => {
                    panic!("decoder failed: {message}")
                }
                _ => {}
            }
        }
        commands
            .send(super::super::preview::PreviewCommand::Seek(Duration::ZERO))
            .unwrap();
        loop {
            match events.recv_timeout(Duration::from_secs(10)).unwrap() {
                super::super::preview::PreviewEvent::Frame(_) => break,
                super::super::preview::PreviewEvent::Failed(message) => {
                    panic!("decoder failed: {message}")
                }
                _ => {}
            }
        }
        commands
            .send(super::super::preview::PreviewCommand::Close)
            .unwrap();
        decoder.join().unwrap();
        let trimmed = trim_video(&path, &original, 2, 7).unwrap();
        assert_eq!(trimmed.frames, 5);
        assert_eq!(trimmed.duration(), Duration::from_millis(500));
        assert!(path.exists());
        assert!(
            !directory
                .path()
                .join(format!(
                    ".trim-source.record-trim-{}-0.mp4",
                    std::process::id()
                ))
                .exists()
        );
    }
}
