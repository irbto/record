//! Reusable edit settings and in-memory transforms for MP4 export.

use std::time::Duration;

use anyhow::{Context, Result, bail};

/// Describes optional effects applied while a selected video range is rebuilt.
///
/// All durations are independent. Crop margins are measured inward from the
/// corresponding edge of the decoded source frame. Because the export path is
/// NV12 (4:2:0), non-zero crop margins must be even pixel counts. Fades longer
/// than the selected range are clamped to it; overlapping fades use the lower
/// of their two linear gains.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct VideoEditSettings {
    /// Linear audio fade-in duration from silence to the source level.
    pub audio_fade_in: Duration,
    /// Linear audio fade-out duration from the source level to silence.
    pub audio_fade_out: Duration,
    /// Linear video fade-in duration from black to the source picture.
    pub video_fade_in: Duration,
    /// Linear video fade-out duration from the source picture to black.
    pub video_fade_out: Duration,
    /// Pixels removed from the source frame's left edge.
    pub crop_left: u32,
    /// Pixels removed from the source frame's right edge.
    pub crop_right: u32,
    /// Pixels removed from the source frame's top edge.
    pub crop_top: u32,
    /// Pixels removed from the source frame's bottom edge.
    pub crop_bottom: u32,
}

impl VideoEditSettings {
    /// Reports whether export would leave the selected samples unchanged.
    #[must_use]
    pub fn is_identity(&self) -> bool {
        self.audio_fade_in.is_zero()
            && self.audio_fade_out.is_zero()
            && self.video_fade_in.is_zero()
            && self.video_fade_out.is_zero()
            && !self.has_crop()
    }

    /// Validates NV12 crop alignment and resolves the encoded dimensions.
    pub(super) fn validate(
        &self,
        source_width: u32,
        source_height: u32,
    ) -> Result<ValidatedVideoEditSettings> {
        if !self.has_crop() {
            return Ok(ValidatedVideoEditSettings {
                output_width: source_width,
                output_height: source_height,
            });
        }
        if source_width < 2 || source_height < 2 {
            bail!("video cropping requires a source image at least 2 by 2 pixels");
        }
        if !source_width.is_multiple_of(2) || !source_height.is_multiple_of(2) {
            bail!("video cropping requires an even source width and height for NV12 encoding");
        }
        if [
            self.crop_left,
            self.crop_right,
            self.crop_top,
            self.crop_bottom,
        ]
        .iter()
        .any(|margin| !margin.is_multiple_of(2))
        {
            bail!("video crop margins must be even pixel counts for NV12 encoding");
        }

        let horizontal = self
            .crop_left
            .checked_add(self.crop_right)
            .context("the horizontal video crop margins overflowed")?;
        let vertical = self
            .crop_top
            .checked_add(self.crop_bottom)
            .context("the vertical video crop margins overflowed")?;
        if horizontal > source_width.saturating_sub(2) || vertical > source_height.saturating_sub(2)
        {
            bail!("video crop margins must leave an image at least 2 by 2 pixels");
        }

        Ok(ValidatedVideoEditSettings {
            output_width: source_width - horizontal,
            output_height: source_height - vertical,
        })
    }

    fn has_crop(&self) -> bool {
        self.crop_left != 0 || self.crop_right != 0 || self.crop_top != 0 || self.crop_bottom != 0
    }
}

/// Contains dimensions resolved after edit settings have been validated.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct ValidatedVideoEditSettings {
    pub(super) output_width: u32,
    pub(super) output_height: u32,
}

/// Precomputes one independent pair of linear fades on a fixed-rate timeline.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct FadeEnvelope {
    total_frames: u64,
    fade_in_frames: u64,
    fade_out_frames: u64,
}

impl FadeEnvelope {
    pub(super) fn new(total_frames: u64, rate: u32, fade_in: Duration, fade_out: Duration) -> Self {
        Self {
            total_frames,
            fade_in_frames: duration_frames(fade_in, rate).min(total_frames),
            fade_out_frames: duration_frames(fade_out, rate).min(total_frames),
        }
    }

    /// Returns the lower of the independent fade-in and fade-out gains.
    fn gain(&self, frame: u64) -> f64 {
        if frame >= self.total_frames {
            return 0.0;
        }
        let fade_in = if self.fade_in_frames == 0 {
            1.0
        } else {
            frame.min(self.fade_in_frames) as f64 / self.fade_in_frames as f64
        };
        let remaining = self.total_frames.saturating_sub(frame + 1);
        let fade_out = if self.fade_out_frames == 0 {
            1.0
        } else {
            remaining.min(self.fade_out_frames) as f64 / self.fade_out_frames as f64
        };
        fade_in.min(fade_out)
    }

    fn is_identity(&self) -> bool {
        self.fade_in_frames == 0 && self.fade_out_frames == 0
    }
}

/// Crops one tightly packed NV12 frame without resampling its chroma plane.
pub(super) fn crop_nv12(
    data: Vec<u8>,
    source_width: u32,
    source_height: u32,
    settings: &VideoEditSettings,
    validated: ValidatedVideoEditSettings,
) -> Result<Vec<u8>> {
    let source_size = nv12_byte_len(source_width, source_height)?;
    if data.len() != source_size {
        bail!(
            "a packed NV12 frame had {} bytes; expected {source_size}",
            data.len()
        );
    }
    if validated.output_width == source_width && validated.output_height == source_height {
        return Ok(data);
    }

    let source_width = source_width as usize;
    let source_height = source_height as usize;
    let output_width = validated.output_width as usize;
    let output_height = validated.output_height as usize;
    let crop_left = settings.crop_left as usize;
    let crop_top = settings.crop_top as usize;
    let mut cropped = Vec::with_capacity(nv12_byte_len(
        validated.output_width,
        validated.output_height,
    )?);

    for row in crop_top..crop_top + output_height {
        let start = row * source_width + crop_left;
        cropped.extend_from_slice(&data[start..start + output_width]);
    }
    let source_chroma = source_width * source_height;
    let crop_chroma_top = crop_top / 2;
    for row in crop_chroma_top..crop_chroma_top + output_height / 2 {
        let start = source_chroma + row * source_width + crop_left;
        cropped.extend_from_slice(&data[start..start + output_width]);
    }
    Ok(cropped)
}

/// Applies an envelope to one tightly packed NV12 frame, fading toward black.
pub(super) fn fade_nv12(
    data: &mut [u8],
    width: u32,
    height: u32,
    frame: u64,
    envelope: &FadeEnvelope,
) -> Result<()> {
    if envelope.is_identity() {
        return Ok(());
    }
    let expected = nv12_byte_len(width, height)?;
    if data.len() != expected {
        bail!(
            "a packed NV12 frame had {} bytes; expected {expected}",
            data.len()
        );
    }
    let gain = envelope.gain(frame);
    if gain >= 1.0 {
        return Ok(());
    }
    let luma_size = width as usize * height as usize;
    if gain <= 0.0 {
        data[..luma_size].fill(0);
        data[luma_size..].fill(128);
        return Ok(());
    }
    for value in &mut data[..luma_size] {
        *value = (f64::from(*value) * gain).round() as u8;
    }
    for value in &mut data[luma_size..] {
        let distance_from_neutral = f64::from(*value) - 128.0;
        *value = (128.0 + distance_from_neutral * gain)
            .round()
            .clamp(0.0, 255.0) as u8;
    }
    Ok(())
}

/// Applies one envelope to interleaved stereo PCM16 frames in place.
pub(super) fn fade_pcm16(samples: &mut [i16], first_frame: u64, envelope: &FadeEnvelope) {
    if envelope.is_identity() {
        return;
    }
    let (stereo_frames, _) = samples.as_chunks_mut::<2>();
    for (offset, stereo_frame) in stereo_frames.iter_mut().enumerate() {
        let gain = envelope.gain(first_frame.saturating_add(offset as u64));
        if gain >= 1.0 {
            continue;
        }
        for sample in stereo_frame {
            *sample = (f64::from(*sample) * gain)
                .round()
                .clamp(f64::from(i16::MIN), f64::from(i16::MAX)) as i16;
        }
    }
}

/// Converts a duration to the smallest whole fixed-rate frame count that covers it.
fn duration_frames(duration: Duration, rate: u32) -> u64 {
    if duration.is_zero() || rate == 0 {
        return 0;
    }
    const NANOS_PER_SECOND: u128 = 1_000_000_000;
    let frames = (duration.as_nanos() * u128::from(rate)).div_ceil(NANOS_PER_SECOND);
    u64::try_from(frames).unwrap_or(u64::MAX)
}

fn nv12_byte_len(width: u32, height: u32) -> Result<usize> {
    let pixels = (width as usize)
        .checked_mul(height as usize)
        .context("the NV12 frame dimensions overflowed")?;
    pixels
        .checked_add(pixels / 2)
        .context("the NV12 frame length overflowed")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_settings_are_identity_edits() {
        let settings = VideoEditSettings::default();
        assert!(settings.is_identity());
        assert_eq!(
            settings.validate(1920, 1080).unwrap(),
            ValidatedVideoEditSettings {
                output_width: 1920,
                output_height: 1080,
            }
        );
    }

    #[test]
    fn crop_validation_requires_even_margins_and_a_visible_frame() {
        let odd = VideoEditSettings {
            crop_left: 1,
            ..VideoEditSettings::default()
        };
        assert!(
            odd.validate(64, 48)
                .unwrap_err()
                .to_string()
                .contains("even")
        );

        let empty = VideoEditSettings {
            crop_left: 32,
            crop_right: 32,
            ..VideoEditSettings::default()
        };
        assert!(
            empty
                .validate(64, 48)
                .unwrap_err()
                .to_string()
                .contains("at least 2 by 2")
        );
    }

    #[test]
    fn nv12_crop_copies_aligned_luma_and_chroma_regions() {
        let settings = VideoEditSettings {
            crop_left: 2,
            crop_top: 2,
            ..VideoEditSettings::default()
        };
        let validated = settings.validate(6, 4).unwrap();
        let source = (0_u8..36).collect::<Vec<_>>();
        assert_eq!(
            crop_nv12(source, 6, 4, &settings, validated).unwrap(),
            vec![14, 15, 16, 17, 20, 21, 22, 23, 32, 33, 34, 35]
        );
    }

    #[test]
    fn video_envelope_fades_luma_and_chroma_toward_black() {
        let envelope = FadeEnvelope::new(3, 10, Duration::from_millis(200), Duration::ZERO);
        let mut first = vec![100, 200, 50, 250, 200, 100];
        fade_nv12(&mut first, 2, 2, 0, &envelope).unwrap();
        assert_eq!(first, vec![0, 0, 0, 0, 128, 128]);

        let mut middle = vec![100, 200, 50, 250, 200, 100];
        fade_nv12(&mut middle, 2, 2, 1, &envelope).unwrap();
        assert_eq!(middle, vec![50, 100, 25, 125, 164, 114]);
    }

    #[test]
    fn audio_envelope_scales_each_stereo_frame_linearly() {
        let envelope = FadeEnvelope::new(
            5,
            10,
            Duration::from_millis(200),
            Duration::from_millis(200),
        );
        let mut samples = vec![1_000_i16; 10];
        fade_pcm16(&mut samples, 0, &envelope);
        assert_eq!(samples, vec![0, 0, 500, 500, 1_000, 1_000, 500, 500, 0, 0]);
    }

    #[test]
    fn nonzero_subframe_durations_cover_one_frame() {
        assert_eq!(duration_frames(Duration::from_nanos(1), 60), 1);
        assert_eq!(duration_frames(Duration::from_millis(100), 10), 1);
        assert_eq!(duration_frames(Duration::from_millis(101), 10), 2);
    }
}
