//! The countdown before a recording, with a glow along the screen edges.
//!
//! The desktop program can wait some seconds before a recording starts. In
//! that time a window covers the monitor that will be recorded. It shows a
//! moving gradient along the four edges and the seconds that are left in the
//! middle. The window is transparent everywhere else, and mouse input passes
//! through it. It closes before the capture starts, so it is never part of
//! the recording.
//!
//! The pixel math has no Windows dependency. A frame is a buffer of
//! premultiplied BGRA pixels in top-down rows, which is the form that a
//! layered window takes.

use std::{
    f32::consts::TAU,
    sync::atomic::{AtomicBool, Ordering},
    thread,
    time::{Duration, Instant},
};

use crate::{
    screenshot::DesktopRect,
    theme::{BACKGROUND, CYAN, GREEN, PURPLE, RED, Rgb, TEXT},
};

/// The colors of the glow in the order in which they follow each other.
const STOPS: [Rgb; 4] = [CYAN, GREEN, PURPLE, RED];
/// Number of times that the colors repeat along the four edges.
const CYCLES: f32 = 2.0;
/// Speed of the colors along the edges in color cycles for each second.
const SPEED: f32 = 0.35;
/// Number of bright waves along the four edges.
const WAVES: f32 = 6.0;
/// Speed of the bright waves in waves for each second.
const WAVE_SPEED: f32 = 0.9;
/// Opacity of the glow directly at the screen edge, from 0 to 1.
const PEAK: f32 = 0.92;
/// Opacity of the plate behind the number, from 0 to 255.
const PLATE_ALPHA: u32 = 165;
/// Time between two frames of the animation.
const FRAME_TIME: Duration = Duration::from_millis(33);
/// Time in seconds in which the overlay fades in.
const FADE_IN: f32 = 0.25;

/// Returns the thickness of the glow in pixels for one frame size.
#[must_use]
pub fn band(width: u32, height: u32) -> u32 {
    let short = width.min(height);
    (short / 12).clamp(16, 140).min(short / 2)
}

/// Returns the glow color at one phase of the color cycle.
///
/// The phase wraps at 1. The result holds red, green, and blue from 0 to 255.
#[must_use]
pub fn palette(phase: f32) -> [f32; 3] {
    let scaled = phase.rem_euclid(1.0) * STOPS.len() as f32;
    let index = (scaled as usize).min(STOPS.len() - 1);
    let step = scaled - index as f32;
    // A smooth step keeps each pure color visible for a while.
    let blend = step * step * (3.0 - 2.0 * step);
    let (from, to) = (STOPS[index], STOPS[(index + 1) % STOPS.len()]);
    let mix = |from: u8, to: u8| f32::from(from) + (f32::from(to) - f32::from(from)) * blend;
    [mix(from.0, to.0), mix(from.1, to.1), mix(from.2, to.2)]
}

/// Returns the length of the path along the four edges in pixels.
const fn perimeter(width: u32, height: u32) -> u32 {
    2 * (width + height)
}

/// Finds the nearest screen edge of one pixel.
///
/// Returns the distance to that edge in pixels and the position of the pixel
/// on the path that runs clockwise along the four edges from the top left
/// corner.
#[must_use]
pub fn nearest_edge(width: u32, height: u32, x: u32, y: u32) -> (u32, u32) {
    let (right, bottom) = (width - 1 - x, height - 1 - y);
    let distance = y.min(right).min(bottom).min(x);
    let position = if distance == y {
        x
    } else if distance == right {
        width + y
    } else if distance == bottom {
        width + height + right
    } else {
        2 * width + height + bottom
    };
    (distance, position)
}

/// Returns the color and the brightness at one position of the edge path.
///
/// The first three values are red, green, and blue from 0 to 255. The last
/// value is the brightness from 0 to 1.
fn path_light(length: u32, position: u32, time: f32) -> [f32; 4] {
    let place = position as f32 / length.max(1) as f32;
    let [red, green, blue] = palette(place * CYCLES - time * SPEED);
    let wave = 0.8 + 0.2 * (TAU * (place * WAVES + time * WAVE_SPEED)).sin();
    [red, green, blue, wave]
}

/// Returns how much of the glow reaches a pixel, from 0 to 1.
fn falloff(distance: u32, band: u32) -> f32 {
    let reach = (1.0 - (distance as f32 + 0.5) / band.max(1) as f32).max(0.0);
    reach * reach
}

/// Converts a light value and a reach to one premultiplied BGRA pixel.
fn lit_pixel(light: [f32; 4], reach: f32) -> [u8; 4] {
    let alpha = (PEAK * light[3] * reach).clamp(0.0, 1.0);
    let channel = |value: f32| (value * alpha + 0.5) as u8;
    [
        channel(light[2]),
        channel(light[1]),
        channel(light[0]),
        (alpha * 255.0 + 0.5) as u8,
    ]
}

/// Returns the glow at one pixel as premultiplied BGRA.
///
/// `time` is the time since the countdown started, in seconds. A pixel
/// outside the glow is fully transparent.
#[must_use]
pub fn glow_pixel(width: u32, height: u32, x: u32, y: u32, time: f32) -> [u8; 4] {
    if x >= width || y >= height {
        return [0; 4];
    }
    let (distance, position) = nearest_edge(width, height, x, y);
    let band = band(width, height);
    if distance >= band {
        return [0; 4];
    }
    lit_pixel(
        path_light(perimeter(width, height), position, time),
        falloff(distance, band),
    )
}

/// Draws the glow of one moment into a frame.
///
/// Only the pixels of the glow change. The middle of the frame keeps what it
/// holds. A buffer that does not hold `width * height` pixels stays as it is.
pub fn draw_glow(pixels: &mut [u8], width: u32, height: u32, time: f32) {
    if width == 0 || height == 0 || pixels.len() != width as usize * height as usize * 4 {
        return;
    }
    let band = band(width, height);
    let length = perimeter(width, height);
    // The light depends only on the place along the edges, so one pass
    // computes it for every pixel of the glow.
    let lights: Vec<[f32; 4]> = (0..length)
        .map(|position| path_light(length, position, time))
        .collect();
    let reaches: Vec<f32> = (0..band).map(|distance| falloff(distance, band)).collect();
    let (rows, _) = pixels.as_chunks_mut::<4>();
    for y in 0..height {
        let row = &mut rows[y as usize * width as usize..][..width as usize];
        let full_row = y < band || y >= height - band;
        let mut light = |x: u32| {
            let (distance, position) = nearest_edge(width, height, x, y);
            row[x as usize] = lit_pixel(lights[position as usize], reaches[distance as usize]);
        };
        if full_row {
            (0..width).for_each(&mut light);
        } else {
            (0..band).for_each(&mut light);
            (width - band..width).for_each(&mut light);
        }
    }
}

/// One rectangle in frame pixels with an exclusive right and bottom edge.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Block {
    /// First column inside the rectangle.
    pub left: i32,
    /// First row inside the rectangle.
    pub top: i32,
    /// First column after the rectangle.
    pub right: i32,
    /// First row after the rectangle.
    pub bottom: i32,
}

impl Block {
    /// Moves the rectangle.
    const fn offset(self, dx: i32, dy: i32) -> Self {
        Self {
            left: self.left + dx,
            top: self.top + dy,
            right: self.right + dx,
            bottom: self.bottom + dy,
        }
    }
}

/// Returns the lit segments of one digit as bits.
///
/// Bit 0 is the top bar. The bits go clockwise from there, and bit 6 is the
/// middle bar, as on a seven-segment display.
#[must_use]
pub const fn digit_segments(digit: u32) -> u8 {
    match digit {
        0 => 0b011_1111,
        1 => 0b000_0110,
        2 => 0b101_1011,
        3 => 0b100_1111,
        4 => 0b110_0110,
        5 => 0b110_1101,
        6 => 0b111_1101,
        7 => 0b000_0111,
        8 => 0b111_1111,
        _ => 0b110_1111,
    }
}

/// Returns the seven segments of a digit cell, in the bit order of
/// [`digit_segments`].
fn segment_blocks(width: i32, height: i32, thickness: i32) -> [Block; 7] {
    let middle = height / 2;
    let half = thickness / 2;
    let block = |left, top, right, bottom| Block {
        left,
        top,
        right,
        bottom,
    };
    [
        block(0, 0, width, thickness),
        block(width - thickness, 0, width, middle + half),
        block(width - thickness, middle - half, width, height),
        block(0, height - thickness, width, height),
        block(0, middle - half, thickness, height),
        block(0, 0, thickness, middle + half),
        block(0, middle - half, width, middle - half + thickness),
    ]
}

/// The place of the number in the middle of a frame.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NumberLayout {
    /// The dark plate behind the digits.
    pub plate: Block,
    /// Corner radius of the plate in pixels.
    pub radius: i32,
    /// Every lit bar of every digit.
    pub bars: Vec<Block>,
}

/// Places a number in the middle of a frame.
#[must_use]
pub fn number_layout(width: u32, height: u32, value: u32) -> NumberLayout {
    let short = width.min(height) as i32;
    let cell_height = (short / 4).clamp(24, 420).min((short * 3 / 5).max(8));
    let cell_width = cell_height * 11 / 20;
    let thickness = (cell_height / 8).max(2);
    let gap = cell_height / 5;
    let padding = cell_height * 2 / 5;
    let digits: Vec<u32> = value
        .to_string()
        .bytes()
        .map(|byte| u32::from(byte - b'0'))
        .collect();
    let count = digits.len() as i32;
    let total = count * cell_width + (count - 1) * gap;
    let left = (width as i32 - total) / 2;
    let top = (height as i32 - cell_height) / 2;
    let blocks = segment_blocks(cell_width, cell_height, thickness);
    let mut bars = Vec::new();
    for (place, digit) in (0_i32..).zip(digits) {
        let lit = digit_segments(digit);
        let origin = left + place * (cell_width + gap);
        for (bit, block) in blocks.iter().enumerate() {
            if lit & (1 << bit) != 0 {
                bars.push(block.offset(origin, top));
            }
        }
    }
    NumberLayout {
        plate: Block {
            left: left - padding,
            top: top - padding,
            right: left + total + padding,
            bottom: top + cell_height + padding,
        },
        radius: padding / 2,
        bars,
    }
}

/// Reports whether a pixel lies inside a rectangle with round corners.
fn inside_round(block: Block, radius: i32, x: i32, y: i32) -> bool {
    // The distance to the inner rectangle decides at the corners.
    let dx = (block.left + radius - x)
        .max(x - (block.right - 1 - radius))
        .max(0);
    let dy = (block.top + radius - y)
        .max(y - (block.bottom - 1 - radius))
        .max(0);
    dx * dx + dy * dy <= radius * radius
}

/// Calls `paint` for each pixel of a rectangle that lies inside the frame.
fn for_each_pixel(
    pixels: &mut [u8],
    width: u32,
    height: u32,
    block: Block,
    mut paint: impl FnMut(i32, i32, &mut [u8; 4]),
) {
    if pixels.len() != width as usize * height as usize * 4 {
        return;
    }
    let (rows, _) = pixels.as_chunks_mut::<4>();
    for y in block.top.max(0)..block.bottom.min(height as i32) {
        for x in block.left.max(0)..block.right.min(width as i32) {
            paint(x, y, &mut rows[y as usize * width as usize + x as usize]);
        }
    }
}

/// Draws a number on a dark plate in the middle of a frame.
///
/// Returns the rectangle that the plate covers, for [`clear_block`].
pub fn draw_number(pixels: &mut [u8], width: u32, height: u32, value: u32) -> Block {
    let layout = number_layout(width, height, value);
    let premultiplied = |channel: u8| ((u32::from(channel) * PLATE_ALPHA + 127) / 255) as u8;
    let plate = [
        premultiplied(BACKGROUND.2),
        premultiplied(BACKGROUND.1),
        premultiplied(BACKGROUND.0),
        PLATE_ALPHA as u8,
    ];
    for_each_pixel(pixels, width, height, layout.plate, |x, y, pixel| {
        *pixel = if inside_round(layout.plate, layout.radius, x, y) {
            plate
        } else {
            [0; 4]
        };
    });
    for bar in &layout.bars {
        for_each_pixel(pixels, width, height, *bar, |_, _, pixel| {
            *pixel = [TEXT.2, TEXT.1, TEXT.0, 255];
        });
    }
    layout.plate
}

/// Makes one rectangle of a frame fully transparent.
pub fn clear_block(pixels: &mut [u8], width: u32, height: u32, block: Block) {
    for_each_pixel(pixels, width, height, block, |_, _, pixel| *pixel = [0; 4]);
}

/// Returns the whole seconds that are left, or `None` when the wait is over.
///
/// The value is the one that a countdown shows: it starts at `total` and
/// stays at 1 during the last second.
#[must_use]
pub fn seconds_left(total: u32, elapsed: Duration) -> Option<u32> {
    let left = Duration::from_secs(u64::from(total)).checked_sub(elapsed)?;
    (!left.is_zero()).then(|| left.as_secs_f64().ceil() as u32)
}

/// Waits for the countdown and draws its frames.
///
/// `tick` receives the seconds that are left, once for each value. `frame`
/// receives the time since the start in seconds and the seconds that are
/// left, about 30 times each second. Returns `false` when `cancel` was set
/// before the wait ended.
pub fn wait(
    seconds: u32,
    cancel: &AtomicBool,
    tick: &mut dyn FnMut(u32),
    frame: &mut dyn FnMut(f32, u32),
) -> bool {
    let started = Instant::now();
    let mut shown = None;
    loop {
        if cancel.load(Ordering::Relaxed) {
            return false;
        }
        let elapsed = started.elapsed();
        let Some(left) = seconds_left(seconds, elapsed) else {
            return true;
        };
        if shown != Some(left) {
            shown = Some(left);
            tick(left);
        }
        let drawing = Instant::now();
        frame(elapsed.as_secs_f32(), left);
        thread::sleep(
            FRAME_TIME
                .saturating_sub(drawing.elapsed())
                .max(Duration::from_millis(4)),
        );
    }
}

/// Shows the countdown over one area of the desktop and waits for its end.
///
/// `area` is the monitor that will be recorded. `tick` receives the seconds
/// that are left, once for each value. The overlay is gone when the call
/// returns. Returns `false` when `cancel` was set before the wait ended.
/// Without a window the wait still runs, only nothing shows.
#[cfg(windows)]
pub fn run(
    area: DesktopRect,
    seconds: u32,
    cancel: &AtomicBool,
    tick: &mut dyn FnMut(u32),
) -> bool {
    let mut surface = window::Surface::create(area);
    let mut plate = Block::default();
    let completed = wait(seconds, cancel, tick, &mut |time, left| {
        if let Some(surface) = &mut surface {
            let (width, height) = (area.width, area.height);
            let pixels = surface.pixels();
            draw_glow(pixels, width, height, time);
            clear_block(pixels, width, height, plate);
            plate = draw_number(pixels, width, height, left);
            surface.present((time / FADE_IN).clamp(0.0, 1.0));
        }
    });
    if let Some(surface) = surface {
        surface.close();
    }
    completed
}

/// Waits for the countdown. Only Windows has the overlay.
#[cfg(not(windows))]
pub fn run(
    _area: DesktopRect,
    seconds: u32,
    cancel: &AtomicBool,
    tick: &mut dyn FnMut(u32),
) -> bool {
    wait(seconds, cancel, tick, &mut |_, _| {})
}

/// The layered window that shows the frames.
#[cfg(windows)]
mod window {
    use std::{mem, ptr, slice, thread, time::Duration};

    use windows::{
        Win32::{
            Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, SIZE, WPARAM},
            Graphics::{
                Dwm::DwmFlush,
                Gdi::{
                    AC_SRC_ALPHA, AC_SRC_OVER, BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BLENDFUNCTION,
                    CreateCompatibleDC, CreateDIBSection, DIB_RGB_COLORS, DeleteDC, DeleteObject,
                    GdiFlush, HBITMAP, HDC, HGDIOBJ, SelectObject,
                },
            },
            System::LibraryLoader::GetModuleHandleW,
            UI::{
                HiDpi::{DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetThreadDpiAwarenessContext},
                WindowsAndMessaging::{
                    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, MSG,
                    PM_REMOVE, PeekMessageW, RegisterClassW, SW_SHOWNOACTIVATE,
                    SetWindowDisplayAffinity, ShowWindow, TranslateMessage, ULW_ALPHA,
                    UnregisterClassW, UpdateLayeredWindow, WDA_EXCLUDEFROMCAPTURE, WNDCLASSW,
                    WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST,
                    WS_EX_TRANSPARENT, WS_POPUP,
                },
            },
        },
        core::{PCWSTR, w},
    };

    use crate::screenshot::DesktopRect;

    /// Window class name of the countdown overlay.
    const CLASS_NAME: PCWSTR = w!("RecordCountdown");
    /// Largest edge of an area that the overlay covers, in pixels.
    const EDGE_LIMIT: u32 = 16_384;
    /// Time that the desktop gets to draw itself without the overlay.
    const SETTLE_TIME: Duration = Duration::from_millis(60);

    /// The overlay window and the bitmap that holds its pixels.
    pub(super) struct Surface {
        /// The layered window.
        window: HWND,
        /// Memory device context that holds the bitmap.
        context: HDC,
        /// The bitmap whose pixels the window shows.
        bitmap: HBITMAP,
        /// The bitmap that the device context held before.
        previous: HGDIOBJ,
        /// First byte of the bitmap pixels.
        bits: *mut u8,
        /// The area that the window covers.
        area: DesktopRect,
    }

    /// Passes every message of the overlay to the default procedure.
    unsafe extern "system" fn window_procedure(
        window: HWND,
        message: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        // SAFETY: The arguments come from Windows unchanged.
        unsafe { DefWindowProcW(window, message, wparam, lparam) }
    }

    impl Surface {
        /// Creates the window over one area. It shows nothing until the
        /// first [`Self::present`].
        pub(super) fn create(area: DesktopRect) -> Option<Self> {
            if area.width == 0
                || area.height == 0
                || area.width > EDGE_LIMIT
                || area.height > EDGE_LIMIT
            {
                return None;
            }
            let info = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: mem::size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: area.width as i32,
                    // A negative height stores the top row first.
                    biHeight: -(area.height as i32),
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB.0,
                    ..Default::default()
                },
                ..Default::default()
            };
            // SAFETY: Every handle is checked before use. A failure releases
            // what was created before it. The window belongs to this thread.
            unsafe {
                // Window positions are physical pixels in this mode.
                SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
                let instance = GetModuleHandleW(None).ok()?;
                let class = WNDCLASSW {
                    lpfnWndProc: Some(window_procedure),
                    hInstance: instance.into(),
                    lpszClassName: CLASS_NAME,
                    ..Default::default()
                };
                // A second countdown at the same time finds the class
                // registered. The window creation decides.
                RegisterClassW(&class);
                let context = CreateCompatibleDC(None);
                if context.is_invalid() {
                    return None;
                }
                let mut bits = ptr::null_mut();
                let bitmap =
                    CreateDIBSection(Some(context), &info, DIB_RGB_COLORS, &mut bits, None, 0);
                let bitmap = match bitmap {
                    Ok(bitmap) if !bits.is_null() => bitmap,
                    Ok(bitmap) => {
                        let _ = DeleteObject(bitmap.into());
                        let _ = DeleteDC(context);
                        return None;
                    }
                    Err(_) => {
                        let _ = DeleteDC(context);
                        return None;
                    }
                };
                let previous = SelectObject(context, bitmap.into());
                let window = CreateWindowExW(
                    WS_EX_LAYERED
                        | WS_EX_TRANSPARENT
                        | WS_EX_TOPMOST
                        | WS_EX_TOOLWINDOW
                        | WS_EX_NOACTIVATE,
                    CLASS_NAME,
                    w!("record countdown"),
                    WS_POPUP,
                    area.left,
                    area.top,
                    area.width as i32,
                    area.height as i32,
                    None,
                    None,
                    Some(instance.into()),
                    None,
                );
                let Ok(window) = window else {
                    SelectObject(context, previous);
                    let _ = DeleteObject(bitmap.into());
                    let _ = DeleteDC(context);
                    return None;
                };
                // A recording that already runs does not show the overlay.
                // Older Windows versions refuse this, which changes nothing
                // for the recording that waits.
                let _ = SetWindowDisplayAffinity(window, WDA_EXCLUDEFROMCAPTURE);
                Some(Self {
                    window,
                    context,
                    bitmap,
                    previous,
                    bits: bits.cast(),
                    area,
                })
            }
        }

        /// Returns the pixels of the next frame as premultiplied BGRA.
        pub(super) fn pixels(&mut self) -> &mut [u8] {
            let length = self.area.width as usize * self.area.height as usize * 4;
            // SAFETY: The bitmap holds `length` bytes and lives as long as
            // `self`. The exclusive borrow of `self` makes the slice unique.
            unsafe { slice::from_raw_parts_mut(self.bits, length) }
        }

        /// Shows the pixels at an opacity from 0 to 1 and handles the
        /// messages of the window.
        pub(super) fn present(&mut self, opacity: f32) {
            let position = POINT {
                x: self.area.left,
                y: self.area.top,
            };
            let size = SIZE {
                cx: self.area.width as i32,
                cy: self.area.height as i32,
            };
            let origin = POINT::default();
            let blend = BLENDFUNCTION {
                BlendOp: AC_SRC_OVER as u8,
                BlendFlags: 0,
                SourceConstantAlpha: (opacity.clamp(0.0, 1.0) * 255.0) as u8,
                AlphaFormat: AC_SRC_ALPHA as u8,
            };
            // SAFETY: The window, the device context, and the bitmap are
            // live and belong to this thread. Every pointer refers to a
            // local that outlives the call.
            unsafe {
                let _ = GdiFlush();
                let _ = UpdateLayeredWindow(
                    self.window,
                    None,
                    Some(&position),
                    Some(&size),
                    Some(self.context),
                    Some(&origin),
                    COLORREF(0),
                    Some(&blend),
                    ULW_ALPHA,
                );
                let _ = ShowWindow(self.window, SW_SHOWNOACTIVATE);
                let mut message = MSG::default();
                while PeekMessageW(&mut message, None, 0, 0, PM_REMOVE).as_bool() {
                    let _ = TranslateMessage(&message);
                    DispatchMessageW(&message);
                }
            }
        }

        /// Removes the window and waits until the desktop no longer shows it.
        pub(super) fn close(self) {
            drop(self);
            // SAFETY: The call has no arguments. It waits for one pass of
            // the desktop composition.
            unsafe {
                let _ = DwmFlush();
                let _ = DwmFlush();
            }
            thread::sleep(SETTLE_TIME);
        }
    }

    impl Drop for Surface {
        fn drop(&mut self) {
            // SAFETY: Every handle is live and belongs to this thread. The
            // pixel slice cannot outlive `self`.
            unsafe {
                let _ = DestroyWindow(self.window);
                SelectObject(self.context, self.previous);
                let _ = DeleteObject(self.bitmap.into());
                let _ = DeleteDC(self.context);
                if let Ok(instance) = GetModuleHandleW(None) {
                    let _ = UnregisterClassW(CLASS_NAME, Some(instance.into()));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Reads one pixel of a frame.
    fn pixel(pixels: &[u8], width: u32, x: u32, y: u32) -> [u8; 4] {
        let start = (y as usize * width as usize + x as usize) * 4;
        [
            pixels[start],
            pixels[start + 1],
            pixels[start + 2],
            pixels[start + 3],
        ]
    }

    #[test]
    fn the_band_follows_the_short_edge_within_limits() {
        assert_eq!(band(1_920, 1_080), 90);
        assert_eq!(band(3_840, 2_160), 140);
        assert_eq!(band(640, 100), 16);
        assert_eq!(band(20, 20), 10);
        assert_eq!(band(0, 0), 0);
    }

    #[test]
    fn the_palette_passes_through_the_four_colors_and_wraps() {
        let color = |rgb: Rgb| [f32::from(rgb.0), f32::from(rgb.1), f32::from(rgb.2)];
        assert_eq!(palette(0.0), color(CYAN));
        assert_eq!(palette(0.25), color(GREEN));
        assert_eq!(palette(0.5), color(PURPLE));
        assert_eq!(palette(0.75), color(RED));
        assert_eq!(palette(1.0), color(CYAN));
        assert_eq!(palette(-0.75), color(GREEN));
        let between = palette(0.125);
        assert!(between[0] > 65.0 && between[0] < 72.0, "{between:?}");
        assert!((between[2] - 185.5).abs() < 0.01, "{between:?}");
    }

    #[test]
    fn the_edge_path_runs_clockwise_without_a_jump() {
        let (width, height) = (40, 30);
        assert_eq!(nearest_edge(width, height, 0, 0), (0, 0));
        assert_eq!(nearest_edge(width, height, 39, 0), (0, 39));
        assert_eq!(nearest_edge(width, height, 39, 5), (0, 45));
        assert_eq!(nearest_edge(width, height, 39, 29), (0, 69));
        assert_eq!(nearest_edge(width, height, 10, 29), (0, 99));
        assert_eq!(nearest_edge(width, height, 0, 29), (0, 109));
        assert_eq!(nearest_edge(width, height, 0, 10), (0, 129));
        assert_eq!(nearest_edge(width, height, 20, 4), (4, 20));
        assert_eq!(nearest_edge(width, height, 3, 15), (3, 124));
        // Every position stays on the path.
        for y in 0..height {
            for x in 0..width {
                let (distance, position) = nearest_edge(width, height, x, y);
                assert!(position < perimeter(width, height));
                assert!(distance <= 14);
            }
        }
    }

    #[test]
    fn the_glow_is_bright_at_the_edge_and_gone_in_the_middle() {
        let (width, height) = (640, 360);
        let edge = glow_pixel(width, height, 320, 0, 0.0);
        let inner = glow_pixel(width, height, 320, 15, 0.0);
        assert!(edge[3] > 150, "{edge:?}");
        assert!(inner[3] < edge[3] && inner[3] > 0, "{inner:?}");
        assert_eq!(glow_pixel(width, height, 320, 30, 0.0), [0; 4]);
        assert_eq!(glow_pixel(width, height, 320, 180, 0.0), [0; 4]);
        assert_eq!(glow_pixel(width, height, 640, 0, 0.0), [0; 4]);
        // The top left corner starts the path with cyan.
        let corner = glow_pixel(width, height, 0, 0, 0.0);
        assert!(corner[0] > corner[2], "{corner:?}");
        // The pixel moves with the time.
        assert_ne!(glow_pixel(width, height, 320, 0, 1.0), edge);
    }

    #[test]
    fn every_glow_pixel_is_premultiplied() {
        let (width, height) = (96, 64);
        for time in [0.0, 0.7, 12.3] {
            for y in 0..height {
                for x in 0..width {
                    let [blue, green, red, alpha] = glow_pixel(width, height, x, y, time);
                    assert!(blue <= alpha && green <= alpha && red <= alpha);
                }
            }
        }
    }

    #[test]
    fn the_drawn_frame_equals_the_single_pixels_and_keeps_the_middle() {
        let (width, height) = (96_u32, 64_u32);
        let mut pixels = vec![7_u8; (width * height * 4) as usize];
        draw_glow(&mut pixels, width, height, 1.25);
        let band = band(width, height);
        for y in 0..height {
            for x in 0..width {
                let (distance, _) = nearest_edge(width, height, x, y);
                let expected = if distance < band {
                    glow_pixel(width, height, x, y, 1.25)
                } else {
                    [7; 4]
                };
                assert_eq!(pixel(&pixels, width, x, y), expected, "{x},{y}");
            }
        }
        // A buffer of the wrong size stays as it is.
        let mut short = vec![1_u8; 10];
        draw_glow(&mut short, width, height, 0.0);
        assert_eq!(short, [1; 10]);
    }

    #[test]
    fn digits_light_the_segments_of_a_seven_segment_display() {
        assert_eq!(digit_segments(8).count_ones(), 7);
        assert_eq!(digit_segments(1).count_ones(), 2);
        assert_eq!(digit_segments(7).count_ones(), 3);
        assert_eq!(digit_segments(0) & 0b100_0000, 0);
        let lit: Vec<u32> = (0..10)
            .map(|digit| digit_segments(digit).count_ones())
            .collect();
        assert_eq!(lit, [6, 2, 5, 5, 4, 5, 6, 3, 7, 6]);
    }

    #[test]
    fn the_number_sits_in_the_middle_of_the_frame() {
        let one = number_layout(1_920, 1_080, 3);
        assert_eq!(one.bars.len(), 5);
        assert_eq!(one.plate.left + one.plate.right, 1_920);
        assert_eq!(one.plate.top + one.plate.bottom, 1_080);
        assert_eq!(one.plate.bottom - one.plate.top, 270 + 2 * 108);
        let two = number_layout(1_920, 1_080, 10);
        assert_eq!(two.bars.len(), 2 + 6);
        assert!(two.plate.right - two.plate.left > one.plate.right - one.plate.left);
        for bar in one.bars.iter().chain(&two.bars) {
            assert!(bar.left < bar.right && bar.top < bar.bottom, "{bar:?}");
            assert!(bar.left >= two.plate.left && bar.right <= two.plate.right);
            assert!(bar.top >= two.plate.top && bar.bottom <= two.plate.bottom);
        }
    }

    #[test]
    fn the_number_is_drawn_on_a_plate_and_can_be_cleared() {
        let (width, height) = (400_u32, 300_u32);
        let mut pixels = vec![0_u8; (width * height * 4) as usize];
        let plate = draw_number(&mut pixels, width, height, 1);
        // The digit 1 has its bars at the right side of its cell.
        let layout = number_layout(width, height, 1);
        let bar = layout.bars[0];
        assert_eq!(
            pixel(&pixels, width, bar.left as u32, bar.top as u32),
            [TEXT.2, TEXT.1, TEXT.0, 255]
        );
        let beside = pixel(&pixels, width, plate.left as u32 + 20, 150);
        assert_eq!(beside[3], PLATE_ALPHA as u8);
        assert!(beside[0] <= beside[3] && beside[2] <= beside[3]);
        // The corner of the plate is round.
        assert_eq!(
            pixel(&pixels, width, plate.left as u32, plate.top as u32),
            [0; 4]
        );
        assert_eq!(pixel(&pixels, width, 5, 5), [0; 4]);
        clear_block(&mut pixels, width, height, plate);
        assert!(pixels.iter().all(|byte| *byte == 0));
        // A plate that leaves a tiny frame is clipped.
        let mut tiny = vec![0_u8; 8 * 8 * 4];
        draw_number(&mut tiny, 8, 8, 60);
    }

    #[test]
    fn the_seconds_count_down_to_one_and_then_end() {
        let at = |millis| seconds_left(3, Duration::from_millis(millis));
        assert_eq!(at(0), Some(3));
        assert_eq!(at(999), Some(3));
        assert_eq!(at(1_000), Some(2));
        assert_eq!(at(2_001), Some(1));
        assert_eq!(at(2_999), Some(1));
        assert_eq!(at(3_000), None);
        assert_eq!(at(9_000), None);
        assert_eq!(seconds_left(0, Duration::ZERO), None);
    }

    #[test]
    fn the_wait_ticks_once_for_each_second_and_stops_on_a_cancel() {
        let cancel = AtomicBool::new(false);
        let mut ticks = Vec::new();
        let mut frames = 0;
        let completed = wait(1, &cancel, &mut |left| ticks.push(left), &mut |_, left| {
            assert_eq!(left, 1);
            frames += 1;
        });
        assert!(completed);
        assert_eq!(ticks, [1]);
        assert!(frames > 5, "{frames}");
        // No countdown ends at once.
        assert!(wait(0, &cancel, &mut |_| panic!("no tick"), &mut |_, _| {}));
        // A cancel ends the wait early.
        let started = Instant::now();
        let mut seen = 0;
        let completed = wait(30, &cancel, &mut |_| {}, &mut |_, _| {
            seen += 1;
            if seen == 3 {
                cancel.store(true, Ordering::Relaxed);
            }
        });
        assert!(!completed);
        assert!(started.elapsed() < Duration::from_secs(5));
    }
}
