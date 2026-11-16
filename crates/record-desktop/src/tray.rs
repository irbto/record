//! The tray picture and its text for each recording state.
//!
//! The tray icon shows at a glance what the program records. The base
//! picture is the dot pulse of the program. An audio recording turns the
//! whole pulse red. A video recording adds a red disc in the lower right
//! corner. Nothing here depends on Windows or on the window shell.

/// What the program records now.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Activity {
    /// An audio recording runs or waits for its countdown.
    pub audio: bool,
    /// A video recording runs or waits for its countdown.
    pub video: bool,
}

/// The recording color of the palette as red, green, and blue.
const RED: [u8; 3] = [247, 75, 103];
/// The color of the ring that separates the disc from the pulse.
const RING: [u8; 3] = [12, 12, 12];
/// Smallest difference between two channels of a colored dot.
const COLORED: u8 = 60;

/// Returns the tooltip of the tray icon.
#[must_use]
pub const fn tooltip(activity: Activity) -> &'static str {
    match (activity.audio, activity.video) {
        (false, false) => "record",
        (true, false) => "record: recording audio",
        (false, true) => "record: recording video",
        (true, true) => "record: recording audio and video",
    }
}

/// Returns the label of the tray menu entry that starts or stops audio.
#[must_use]
pub const fn audio_label(activity: Activity) -> &'static str {
    if activity.audio {
        "Stop audio recording"
    } else {
        "Start audio recording"
    }
}

/// Draws the tray picture of one state.
///
/// `base` holds the idle picture as packed RGBA bytes of a square with the
/// given edge. A buffer of another size comes back unchanged.
#[must_use]
pub fn picture(base: &[u8], edge: u32, activity: Activity) -> Vec<u8> {
    let mut pixels = base.to_vec();
    if edge == 0 || base.len() != edge as usize * edge as usize * 4 {
        return pixels;
    }
    let (rows, _) = pixels.as_chunks_mut::<4>();
    if activity.audio {
        for pixel in rows.iter_mut() {
            let [red, green, blue, alpha] = *pixel;
            let spread = red.max(green).max(blue) - red.min(green).min(blue);
            if alpha > 0 && spread >= COLORED {
                *pixel = [RED[0], RED[1], RED[2], alpha];
            }
        }
    }
    if activity.video {
        // The disc covers the lower right quarter of the picture.
        let size = edge as f32;
        let (center, radius) = (size * 0.72, size * 0.25);
        let ring = radius + (size / 16.0).max(1.0);
        for y in 0..edge {
            for x in 0..edge {
                let (dx, dy) = (x as f32 + 0.5 - center, y as f32 + 0.5 - center);
                let distance = (dx * dx + dy * dy).sqrt();
                let pixel = &mut rows[(y * edge + x) as usize];
                if distance <= radius {
                    *pixel = [RED[0], RED[1], RED[2], 255];
                } else if distance <= ring {
                    *pixel = [RING[0], RING[1], RING[2], 255];
                }
            }
        }
    }
    pixels
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A dark picture with one cyan dot, one green dot, and a clear corner.
    fn base() -> Vec<u8> {
        let mut pixels = [12_u8, 12, 12, 255].repeat(32 * 32);
        pixels[..4].copy_from_slice(&[0, 0, 0, 0]);
        pixels[(4 * 32 + 4) * 4..][..4].copy_from_slice(&[65, 214, 224, 255]);
        pixels[(4 * 32 + 8) * 4..][..4].copy_from_slice(&[72, 214, 147, 255]);
        pixels
    }

    /// Reads one pixel of a picture.
    fn pixel(pixels: &[u8], x: usize, y: usize) -> [u8; 4] {
        let start = (y * 32 + x) * 4;
        [
            pixels[start],
            pixels[start + 1],
            pixels[start + 2],
            pixels[start + 3],
        ]
    }

    #[test]
    fn the_idle_picture_is_the_base_picture() {
        assert_eq!(picture(&base(), 32, Activity::default()), base());
        // A buffer of the wrong size comes back unchanged.
        let both = Activity {
            audio: true,
            video: true,
        };
        assert_eq!(picture(&[1, 2, 3], 32, both), [1, 2, 3]);
        assert_eq!(picture(&[], 0, both), Vec::<u8>::new());
    }

    #[test]
    fn audio_turns_the_colored_dots_red() {
        let audio = Activity {
            audio: true,
            video: false,
        };
        let drawn = picture(&base(), 32, audio);
        assert_eq!(pixel(&drawn, 4, 4), [247, 75, 103, 255]);
        assert_eq!(pixel(&drawn, 8, 4), [247, 75, 103, 255]);
        assert_eq!(pixel(&drawn, 20, 20), [12, 12, 12, 255]);
        assert_eq!(pixel(&drawn, 0, 0), [0, 0, 0, 0]);
    }

    #[test]
    fn video_adds_a_red_disc_with_a_ring_in_the_corner() {
        let video = Activity {
            audio: false,
            video: true,
        };
        let drawn = picture(&base(), 32, video);
        assert_eq!(pixel(&drawn, 23, 23), [247, 75, 103, 255]);
        assert_eq!(pixel(&drawn, 23, 14), [12, 12, 12, 255]);
        // The pulse outside the disc keeps its colors.
        assert_eq!(pixel(&drawn, 4, 4), [65, 214, 224, 255]);
        let both = picture(
            &base(),
            32,
            Activity {
                audio: true,
                video: true,
            },
        );
        assert_eq!(pixel(&both, 4, 4), [247, 75, 103, 255]);
        assert_eq!(pixel(&both, 23, 23), [247, 75, 103, 255]);
    }

    #[test]
    fn the_texts_name_the_state() {
        let state = |audio, video| Activity { audio, video };
        assert_eq!(tooltip(state(false, false)), "record");
        assert_eq!(tooltip(state(true, false)), "record: recording audio");
        assert_eq!(tooltip(state(false, true)), "record: recording video");
        assert_eq!(
            tooltip(state(true, true)),
            "record: recording audio and video"
        );
        assert_eq!(audio_label(state(false, true)), "Start audio recording");
        assert_eq!(audio_label(state(true, false)), "Stop audio recording");
    }
}
