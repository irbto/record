//! HDR to SDR conversion for desktop frames.
//!
//! An HDR desktop reaches the capture path as linear scRGB half floats. In
//! that space 1.0 is 80 nits, and Windows places SDR content at the SDR white
//! level of the monitor, which is usually much brighter than 80 nits. Reading
//! those values as plain 8-bit sRGB is what makes HDR screenshots look white
//! and washed out. [`ToneMap`] divides by the SDR white level first, so SDR
//! content returns to its original 8-bit values.

/// Number of entries in the linear to sRGB lookup table.
const SRGB_TABLE_SIZE: usize = 1 << 14;

/// Converts one IEEE 754 half float to `f32`.
#[must_use]
pub(crate) fn half_to_f32(bits: u16) -> f32 {
    let sign = u32::from(bits >> 15) << 31;
    let exponent = u32::from(bits >> 10) & 0x1f;
    let mantissa = u32::from(bits) & 0x3ff;
    let value = match (exponent, mantissa) {
        (0, 0) => 0,
        (0, _) => {
            // Subnormal half: shift the mantissa until its leading bit is set.
            let shift = mantissa.leading_zeros() - 21;
            let mantissa = (mantissa << shift) & 0x3ff;
            ((113 - shift) << 23) | (mantissa << 13)
        }
        (0x1f, _) => 0x7f80_0000 | (mantissa << 13),
        _ => ((exponent + 112) << 23) | (mantissa << 13),
    };
    f32::from_bits(sign | value)
}

/// Maps linear scRGB pixels to 8-bit sRGB pixels.
pub(crate) struct ToneMap {
    scale: f32,
    table: Vec<u8>,
}

impl ToneMap {
    /// Creates a map for a monitor with the given SDR white level in nits.
    #[must_use]
    pub(crate) fn new(sdr_white_nits: f32) -> Self {
        let white = if sdr_white_nits.is_finite() && sdr_white_nits >= 80.0 {
            sdr_white_nits
        } else {
            80.0
        };
        let table = (0..=SRGB_TABLE_SIZE)
            .map(|index| srgb_encode(index as f32 / SRGB_TABLE_SIZE as f32))
            .collect();
        Self {
            scale: 80.0 / white,
            table,
        }
    }

    /// Converts one linear scRGB pixel to a `0x00RRGGBB` value.
    ///
    /// Values at or below SDR white keep their exact color. Brighter HDR
    /// highlights scale down to the display range without a hue change.
    /// Colors outside the sRGB gamut clamp to its edge.
    #[must_use]
    pub(crate) fn map(&self, red: f32, green: f32, blue: f32) -> u32 {
        let mut red = sanitize(red) * self.scale;
        let mut green = sanitize(green) * self.scale;
        let mut blue = sanitize(blue) * self.scale;
        let peak = red.max(green).max(blue);
        if peak > 1.0 {
            red /= peak;
            green /= peak;
            blue /= peak;
        }
        (u32::from(self.encode(red)) << 16)
            | (u32::from(self.encode(green)) << 8)
            | u32::from(self.encode(blue))
    }

    /// Looks up the sRGB code for one linear value in `0.0..=1.0`.
    fn encode(&self, linear: f32) -> u8 {
        let index = (linear * SRGB_TABLE_SIZE as f32 + 0.5) as usize;
        self.table[index.min(SRGB_TABLE_SIZE)]
    }
}

/// Replaces negative and non-finite channel values with zero.
fn sanitize(value: f32) -> f32 {
    if value.is_finite() && value > 0.0 {
        value
    } else {
        0.0
    }
}

/// Applies the piecewise sRGB transfer function to one linear value.
fn srgb_encode(linear: f32) -> u8 {
    let encoded = if linear <= 0.003_130_8 {
        linear * 12.92
    } else {
        1.055 * linear.powf(1.0 / 2.4) - 0.055
    };
    (encoded.clamp(0.0, 1.0) * 255.0 + 0.5) as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Applies the inverse sRGB transfer function to one 8-bit code.
    fn srgb_decode(code: u8) -> f32 {
        let value = f32::from(code) / 255.0;
        if value <= 0.040_45 {
            value / 12.92
        } else {
            ((value + 0.055) / 1.055).powf(2.4)
        }
    }

    #[test]
    fn half_floats_decode_to_known_values() {
        assert_eq!(half_to_f32(0x0000), 0.0);
        assert_eq!(half_to_f32(0x3c00), 1.0);
        assert_eq!(half_to_f32(0xc000), -2.0);
        assert_eq!(half_to_f32(0x3555), 0.333_251_95);
        assert_eq!(half_to_f32(0x0001), 5.960_464_5e-8);
        assert_eq!(half_to_f32(0x7c00), f32::INFINITY);
        assert!(half_to_f32(0x7e00).is_nan());
    }

    #[test]
    fn sdr_content_on_an_hdr_desktop_returns_to_its_original_codes() {
        // Windows shows SDR white at 240 nits here, so SDR white is 3.0 in scRGB.
        let map = ToneMap::new(240.0);
        for code in 0..=255_u8 {
            let linear = srgb_decode(code) * 3.0;
            let pixel = map.map(linear, linear, linear);
            let expected = u32::from(code) * 0x01_01_01;
            assert_eq!(pixel, expected, "code {code}");
        }
    }

    #[test]
    fn an_unmapped_read_would_clip_most_of_the_range_to_white() {
        // This is the washed-out result that the SDR white division prevents.
        let naive = ToneMap::new(80.0);
        let linear = srgb_decode(160) * 3.0;
        assert_eq!(naive.map(linear, linear, linear), 0x00ff_ffff);
    }

    #[test]
    fn hdr_highlights_keep_their_hue() {
        let map = ToneMap::new(80.0);
        assert_eq!(map.map(4.0, 2.0, 0.0), map.map(1.0, 0.5, 0.0));
        assert_eq!(map.map(8.0, 8.0, 8.0), 0x00ff_ffff);
    }

    #[test]
    fn invalid_channel_values_become_black() {
        let map = ToneMap::new(200.0);
        assert_eq!(map.map(-0.5, f32::NAN, f32::NEG_INFINITY), 0);
    }
}
