//! Frame-accurate MP4 trimming for the integrated video editor.
//!
//! Media Foundation decodes the selected H.264/AAC range to NV12 and PCM,
//! then the recorder's existing sink writer rebuilds the MP4. The replacement
//! is written beside the source and swapped in only after finalization succeeds.

mod reader;

use std::{
    fs,
    ops::Range,
    os::windows::ffi::OsStrExt,
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::{Context, Result, anyhow, bail};
use windows::{
    Win32::Storage::FileSystem::{REPLACEFILE_WRITE_THROUGH, ReplaceFileW},
    core::PCWSTR,
};

use super::{
    Mp4Writer, Nv12Frame, VideoSummary,
    edit::{
        FadeEnvelope, ValidatedVideoEditSettings, VideoEditSettings, crop_nv12, fade_nv12,
        fade_pcm16,
    },
};
use crate::audio::windows::{ComGuard, MediaFoundationGuard};
use reader::ClipReader;

/// Describes the current stage of rebuilding and replacing a trimmed video.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum TrimProgress {
    /// The source reader, destination writer, and temporary output are being prepared.
    Preparing,
    /// Decoded video frames are being written to the trimmed output.
    Processing {
        /// Number of selected video frames written so far.
        completed_frames: u64,
        /// Total number of selected video frames to write.
        total_frames: u64,
    },
    /// The new MP4 is being finalized into a complete playable file.
    Finishing,
    /// The finalized temporary MP4 is replacing the original recording.
    Saving,
}

/// Re-encodes `start_frame..end_frame` and safely replaces the source MP4.
pub fn trim_video(
    path: &Path,
    summary: &VideoSummary,
    start_frame: u64,
    end_frame: u64,
) -> Result<VideoSummary> {
    trim_video_with_progress(path, summary, start_frame, end_frame, |_| {})
}

/// Re-encodes one frame range with optional crop and independent audio/video fades.
pub fn trim_video_with_settings(
    path: &Path,
    summary: &VideoSummary,
    start_frame: u64,
    end_frame: u64,
    settings: &VideoEditSettings,
) -> Result<VideoSummary> {
    trim_video_with_settings_and_progress(path, summary, start_frame, end_frame, settings, |_| {})
}

/// Re-encodes one frame range while reporting each stage of the replacement.
pub(crate) fn trim_video_with_progress(
    path: &Path,
    summary: &VideoSummary,
    start_frame: u64,
    end_frame: u64,
    progress: impl FnMut(TrimProgress),
) -> Result<VideoSummary> {
    trim_video_with_settings_and_progress(
        path,
        summary,
        start_frame,
        end_frame,
        &VideoEditSettings::default(),
        progress,
    )
}

/// Re-encodes one edited frame range while reporting each replacement stage.
pub(crate) fn trim_video_with_settings_and_progress(
    path: &Path,
    summary: &VideoSummary,
    start_frame: u64,
    end_frame: u64,
    settings: &VideoEditSettings,
    mut progress: impl FnMut(TrimProgress),
) -> Result<VideoSummary> {
    if start_frame >= end_frame || end_frame > summary.frames {
        bail!("the video selection is outside the recorded frame range");
    }
    if start_frame == 0 && end_frame == summary.frames && settings.is_identity() {
        return Ok(summary.clone());
    }
    let validated = settings.validate(summary.width, summary.height)?;

    progress(TrimProgress::Preparing);
    let temporary = temporary_output(path)?;
    let result = trim_video_inner(
        &temporary,
        path,
        summary,
        start_frame..end_frame,
        settings,
        validated,
        &mut progress,
    );
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    let mut trimmed = result?;
    progress(TrimProgress::Saving);
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
    frame_range: Range<u64>,
    settings: &VideoEditSettings,
    validated: ValidatedVideoEditSettings,
    progress: &mut impl FnMut(TrimProgress),
) -> Result<VideoSummary> {
    let start_frame = frame_range.start;
    let end_frame = frame_range.end;
    let _com = ComGuard::new()?;
    let _media_foundation = MediaFoundationGuard::new()?;
    let mut reader = unsafe { ClipReader::open(source, summary)? };
    let mut writer = unsafe {
        Mp4Writer::open(
            temporary,
            validated.output_width,
            validated.output_height,
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
    let video_fade = FadeEnvelope::new(
        target_video_frames,
        summary.fps,
        settings.video_fade_in,
        settings.video_fade_out,
    );
    let audio_fade = FadeEnvelope::new(
        target_audio_frames,
        summary.sample_rate,
        settings.audio_fade_in,
        settings.audio_fade_out,
    );
    let mut written_video = 0_u64;
    let mut written_audio = 0_u64;
    let mut video_ended = false;
    let mut audio_ended = false;
    progress(TrimProgress::Processing {
        completed_frames: 0,
        total_frames: target_video_frames,
    });

    while (!video_ended && written_video < target_video_frames)
        || (!audio_ended && written_audio < target_audio_frames)
    {
        if !video_ended && written_video < target_video_frames {
            match unsafe { reader.read_video()? } {
                Some(sample) => {
                    if sample.timestamp >= hundred_nanos(start) {
                        let mut data = pack_nv12(
                            &sample.bytes,
                            summary.width,
                            summary.height,
                            reader.video_stride,
                        )?;
                        if validated.output_width != summary.width
                            || validated.output_height != summary.height
                        {
                            data = crop_nv12(
                                data,
                                summary.width,
                                summary.height,
                                settings,
                                validated,
                            )?;
                        }
                        fade_nv12(
                            &mut data,
                            validated.output_width,
                            validated.output_height,
                            written_video,
                            &video_fade,
                        )?;
                        let frame = Nv12Frame { data };
                        let timestamp = frame_time(written_video, summary.fps);
                        unsafe { writer.write_video_frame(&frame, timestamp)? };
                        written_video += 1;
                        progress(TrimProgress::Processing {
                            completed_frames: written_video,
                            total_frames: target_video_frames,
                        });
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
                            let mut pcm = pcm_slice(&sample.bytes, offset, frames)?;
                            fade_pcm16(&mut pcm, written_audio, &audio_fade);
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
    progress(TrimProgress::Finishing);
    unsafe { writer.close() }
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

    #[test]
    fn full_range_identity_settings_skip_the_rebuild() {
        let summary = VideoSummary {
            output: PathBuf::from("does-not-need-to-exist.mp4"),
            width: 64,
            height: 48,
            fps: 10,
            frames: 10,
            sample_rate: 48_000,
            video_bitrate: 2_000_000,
            audio_bitrate: 192_000,
            recording_ready_ms: None,
            finalize_ms: 0.0,
        };
        let mut progress = Vec::new();
        let unchanged = trim_video_with_settings_and_progress(
            &summary.output,
            &summary,
            0,
            summary.frames,
            &VideoEditSettings::default(),
            |update| progress.push(update),
        )
        .unwrap();

        assert_eq!(unchanged.output, summary.output);
        assert_eq!(unchanged.frames, summary.frames);
        assert!(progress.is_empty());
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
        let mut progress = Vec::new();
        let trimmed = trim_video_with_progress(&path, &original, 2, 7, |update| {
            progress.push(update);
        })
        .unwrap();
        assert_eq!(trimmed.frames, 5);
        assert_eq!(trimmed.duration(), Duration::from_millis(500));
        assert_eq!(progress.first(), Some(&TrimProgress::Preparing));
        assert_eq!(
            progress.get(1),
            Some(&TrimProgress::Processing {
                completed_frames: 0,
                total_frames: 5,
            })
        );
        assert_eq!(
            progress
                .iter()
                .rev()
                .find(|update| matches!(update, TrimProgress::Processing { .. })),
            Some(&TrimProgress::Processing {
                completed_frames: 5,
                total_frames: 5,
            })
        );
        assert_eq!(
            progress.get(progress.len().saturating_sub(2)),
            Some(&TrimProgress::Finishing)
        );
        assert_eq!(progress.last(), Some(&TrimProgress::Saving));
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

    #[cfg(windows)]
    #[test]
    fn edits_a_full_native_mp4_when_settings_are_not_identity() {
        if super::super::check_video_support().is_err() {
            return;
        }
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("edit-source.mp4");
        let original = {
            let _com = ComGuard::new().unwrap();
            let _media_foundation = MediaFoundationGuard::new().unwrap();
            let mut writer = unsafe {
                Mp4Writer::open(
                    &path, 320, 240, 10, 2_000_000, 48_000, 192_000, true, None, 0.0,
                )
            }
            .unwrap();
            for frame in 0..10 {
                unsafe {
                    writer
                        .write_video_frame(
                            &Nv12Frame::black(320, 240),
                            Duration::from_millis(frame * 100),
                        )
                        .unwrap();
                }
            }
            unsafe { writer.write_audio(&vec![1_000_i16; 48_000 * 2]).unwrap() };
            unsafe { writer.close().unwrap() }
        };
        let settings = VideoEditSettings {
            audio_fade_in: Duration::from_millis(200),
            audio_fade_out: Duration::from_millis(300),
            video_fade_in: Duration::from_millis(100),
            video_fade_out: Duration::from_millis(200),
            crop_left: 2,
            crop_right: 2,
            crop_top: 2,
            crop_bottom: 2,
        };
        let mut progress = Vec::new();
        let edited = trim_video_with_settings_and_progress(
            &path,
            &original,
            0,
            original.frames,
            &settings,
            |update| progress.push(update),
        )
        .unwrap();

        assert_eq!(edited.width, 316);
        assert_eq!(edited.height, 236);
        assert_eq!(edited.frames, 10);
        assert_eq!(edited.duration(), Duration::from_secs(1));
        assert_eq!(edited.output, path);
        assert_eq!(progress.first(), Some(&TrimProgress::Preparing));
        assert_eq!(progress.last(), Some(&TrimProgress::Saving));
        assert!(progress.contains(&TrimProgress::Processing {
            completed_frames: 10,
            total_frames: 10,
        }));
    }
}
