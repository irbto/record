//! A quick, small picture of one part of the desktop.
//!
//! The desktop program shows it as the live preview of the monitor that a
//! video recording uses. It does not depend on a running recording, so the
//! preview also works before the recording starts and on a still desktop.

use anyhow::Result;

/// One small picture of the desktop.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Glance {
    /// Packed RGB bytes in top-down rows.
    pub rgb: Vec<u8>,
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
}

/// Largest edge of the desktop area that a glance reads, in pixels.
const SOURCE_LIMIT: u32 = 16_384;

/// Returns the size of a picture that fits `max_edge` and keeps its shape.
///
/// A picture that already fits keeps its size.
#[must_use]
pub fn fitted_size(width: u32, height: u32, max_edge: u32) -> (u32, u32) {
    let longest = width.max(height).max(1);
    let limit = max_edge.max(1);
    if longest <= limit {
        return (width.max(1), height.max(1));
    }
    let scale = f64::from(limit) / f64::from(longest);
    (
        ((f64::from(width) * scale).round() as u32).max(1),
        ((f64::from(height) * scale).round() as u32).max(1),
    )
}

/// Converts packed BGRA pixels to packed RGB pixels.
#[must_use]
pub fn rgb_from_bgra(bgra: &[u8]) -> Vec<u8> {
    bgra.as_chunks::<4>()
        .0
        .iter()
        .flat_map(|pixel| [pixel[2], pixel[1], pixel[0]])
        .collect()
}

/// Reads one area of the desktop and scales it to fit `max_edge`.
///
/// The area is in desktop pixels, as [`crate::video::MonitorInfo`] gives it.
#[cfg(windows)]
pub fn glance(left: i32, top: i32, width: u32, height: u32, max_edge: u32) -> Result<Glance> {
    use std::{mem, ptr, slice};

    use anyhow::{Context, bail};
    use windows::Win32::Graphics::Gdi::{
        BI_RGB, BITMAPINFO, BITMAPINFOHEADER, CAPTUREBLT, CreateCompatibleDC, CreateDIBSection,
        DIB_RGB_COLORS, DeleteDC, DeleteObject, GdiFlush, GetDC, HALFTONE, ReleaseDC, SRCCOPY,
        SelectObject, SetStretchBltMode, StretchBlt,
    };

    if width == 0 || height == 0 || width > SOURCE_LIMIT || height > SOURCE_LIMIT {
        bail!("the preview area {width}x{height} is not a monitor size");
    }
    let (small_width, small_height) = fitted_size(width, height, max_edge);
    let info = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: small_width as i32,
            // A negative height stores the top row first.
            biHeight: -(small_height as i32),
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        ..Default::default()
    };
    // SAFETY: Every handle is checked before use and released before the
    // block ends. `bits` points to `small_width * small_height` pixels that
    // live until the bitmap is deleted, and the copy ends before that.
    unsafe {
        let screen = GetDC(None);
        if screen.is_invalid() {
            bail!("the desktop is not available");
        }
        let memory = CreateCompatibleDC(Some(screen));
        let mut bits = ptr::null_mut();
        let bitmap = CreateDIBSection(Some(memory), &info, DIB_RGB_COLORS, &mut bits, None, 0);
        let result = match bitmap {
            Ok(bitmap) if !bits.is_null() => {
                let previous = SelectObject(memory, bitmap.into());
                SetStretchBltMode(memory, HALFTONE);
                let copied = StretchBlt(
                    memory,
                    0,
                    0,
                    small_width as i32,
                    small_height as i32,
                    Some(screen),
                    left,
                    top,
                    width as i32,
                    height as i32,
                    SRCCOPY | CAPTUREBLT,
                )
                .as_bool();
                let _ = GdiFlush();
                let pixels = slice::from_raw_parts(
                    bits.cast::<u8>(),
                    small_width as usize * small_height as usize * 4,
                );
                let rgb = rgb_from_bgra(pixels);
                SelectObject(memory, previous);
                let _ = DeleteObject(bitmap.into());
                if copied {
                    Ok(Glance {
                        rgb,
                        width: small_width,
                        height: small_height,
                    })
                } else {
                    Err(anyhow::anyhow!("Windows did not copy the desktop"))
                }
            }
            Ok(bitmap) => {
                let _ = DeleteObject(bitmap.into());
                Err(anyhow::anyhow!("Windows gave no memory for the preview"))
            }
            Err(error) => Err(error).context("could not create the preview bitmap"),
        };
        let _ = DeleteDC(memory);
        ReleaseDC(None, screen);
        result
    }
}

/// Reads one area of the desktop. Only Windows has a desktop to read.
#[cfg(not(windows))]
pub fn glance(_left: i32, _top: i32, _width: u32, _height: u32, _max_edge: u32) -> Result<Glance> {
    anyhow::bail!("the desktop preview is available on Windows only")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_picture_shrinks_to_the_limit_and_keeps_its_shape() {
        assert_eq!(fitted_size(2_560, 1_440, 1_280), (1_280, 720));
        assert_eq!(fitted_size(1_080, 1_920, 960), (540, 960));
        assert_eq!(fitted_size(800, 600, 1_280), (800, 600));
        assert_eq!(fitted_size(10_000, 1, 100), (100, 1));
        assert_eq!(fitted_size(0, 0, 0), (1, 1));
    }

    #[test]
    fn bgra_pixels_become_rgb_pixels() {
        assert_eq!(
            rgb_from_bgra(&[1, 2, 3, 255, 10, 20, 30, 0]),
            [3, 2, 1, 30, 20, 10]
        );
        assert!(rgb_from_bgra(&[]).is_empty());
    }
}
