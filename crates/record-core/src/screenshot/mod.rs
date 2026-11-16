//! HDR-aware screenshots with an on-screen annotation overlay.
//!
//! A capture copies the desktop from the GPU. Region capture freezes one monitor in a borderless window and
//! lets the user select a region and draw on the frozen image. That overlay is
//! a native window because a terminal cannot draw over the desktop.
//!
//! The capture requests linear scRGB frames when a monitor is in HDR mode.
//! The `color` module divides them by the SDR white level of the monitor, so
//! SDR content keeps its original colors instead of clipping to white. The
//! drawing code in `canvas` and `editor` has no Windows dependency.

use std::{
    env, fs,
    path::{Path, PathBuf},
    time::Duration,
};

mod canvas;
#[cfg(windows)]
mod capture;
mod color;
mod editor;
/// Types shared by the capture thread and the terminal hub.
pub mod events;
/// File formats for saved screenshots.
pub mod format;
/// Saved screenshot files for the hub.
pub mod history;
#[cfg(windows)]
mod http;
/// The capture thread with global hotkeys.
#[cfg(windows)]
pub mod hub;
mod icons;
#[cfg(windows)]
mod overlay;
/// Stored defaults of the hub.
pub mod saved;
/// Upload destinations in the ShareX custom uploader format.
pub mod upload;

/// Selects the monitor for region and monitor captures.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MonitorChoice {
    /// The monitor that contains the mouse pointer.
    Pointer,
    /// The primary monitor.
    Primary,
    /// A monitor index from `record video --monitor list`.
    Index(u32),
}

/// One rectangle in virtual desktop pixels.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DesktopRect {
    /// Left edge. Monitors left of the primary monitor have negative values.
    pub left: i32,
    /// Top edge. Monitors above the primary monitor have negative values.
    pub top: i32,
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
}

impl DesktopRect {
    /// Parses `LEFT,TOP,WIDTH,HEIGHT`.
    pub fn parse(value: &str) -> Result<Self, String> {
        let parts: Vec<&str> = value.split(',').map(str::trim).collect();
        let [left, top, width, height] = parts.as_slice() else {
            return Err("region must be LEFT,TOP,WIDTH,HEIGHT".to_owned());
        };
        let number = |text: &str, label: &str| {
            text.parse::<i32>()
                .map_err(|_| format!("invalid region {label} in {value}"))
        };
        let (left, top) = (number(left, "left")?, number(top, "top")?);
        let (width, height) = (number(width, "width")?, number(height, "height")?);
        if width <= 0 || height <= 0 {
            return Err("region width and height must be greater than zero".to_owned());
        }
        Ok(Self {
            left,
            top,
            width: width as u32,
            height: height as u32,
        })
    }

    /// Formats the rectangle in the form that [`Self::parse`] reads.
    #[must_use]
    pub fn to_text(&self) -> String {
        format!("{},{},{},{}", self.left, self.top, self.width, self.height)
    }
}

/// Selects what a screenshot contains.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CaptureMode {
    /// Freezes one monitor and opens the selection and annotation overlay.
    Region,
    /// One complete monitor.
    Monitor,
    /// Every monitor, joined into one image.
    Fullscreen,
    /// The window that has the keyboard focus.
    Window,
    /// The region that the last overlay or custom capture used.
    LastRegion,
    /// A fixed rectangle in virtual desktop pixels.
    Custom(DesktopRect),
}

/// Contains every setting of one screenshot.
#[derive(Clone, Debug)]
pub struct ScreenshotConfig {
    /// What the screenshot contains.
    pub mode: CaptureMode,
    /// Image file. Its extension selects the format, and PNG is the default.
    pub output: PathBuf,
    /// Monitor for the region and monitor modes.
    pub monitor: MonitorChoice,
    /// Writes the PNG file.
    pub save: bool,
    /// Places the image on the clipboard.
    pub copy: bool,
    /// Waits this long before the capture.
    pub delay: Option<Duration>,
    /// Overrides the SDR white level that Windows reports, in nits.
    pub sdr_white_nits: Option<f32>,
    /// Uploads the image to this destination and copies the link.
    pub upload: Option<upload::Uploader>,
}

/// Describes one finished screenshot.
#[derive(Clone, Debug)]
pub struct ScreenshotSummary {
    /// The written PNG file. `None` when the image only went to the clipboard.
    pub output: Option<PathBuf>,
    /// Image width in pixels.
    pub width: u32,
    /// Image height in pixels.
    pub height: u32,
    /// Reports whether the image is on the clipboard.
    pub copied: bool,
    /// The reason the clipboard copy failed, when it did.
    pub clipboard_error: Option<String>,
    /// The links of the uploaded image. The clipboard then holds the link.
    pub uploaded: Option<upload::UploadResult>,
    /// The reason the upload failed, when it did.
    pub upload_error: Option<String>,
    /// Reports whether a captured monitor was in HDR mode.
    pub hdr: bool,
    /// The SDR white level used for the HDR conversion, in nits.
    pub sdr_white_nits: f32,
}

/// Returns the folder that receives screenshots without an explicit path.
#[must_use]
pub fn default_directory() -> PathBuf {
    env::var_os("USERPROFILE").map_or_else(
        || PathBuf::from("."),
        |home| PathBuf::from(home).join("Pictures").join("Screenshots"),
    )
}

/// Returns an unused `screenshot-TIMESTAMP` path with the given file
/// extension inside a directory.
#[must_use]
pub fn unique_output_path(directory: &Path, timestamp: &str, extension: &str) -> PathBuf {
    let first = directory.join(format!("screenshot-{timestamp}.{extension}"));
    if !first.exists() {
        return first;
    }
    (2..)
        .map(|count| directory.join(format!("screenshot-{timestamp}-{count}.{extension}")))
        .find(|path| !path.exists())
        .unwrap_or(first)
}

/// Returns the file that remembers the last captured region.
fn last_region_file() -> Option<PathBuf> {
    env::var_os("LOCALAPPDATA")
        .map(|data| PathBuf::from(data).join("record").join("last-region.txt"))
}

/// Reads the region that the last region or custom capture used.
#[must_use]
pub fn last_region() -> Option<DesktopRect> {
    let text = fs::read_to_string(last_region_file()?).ok()?;
    DesktopRect::parse(text.trim()).ok()
}

/// Remembers a region for the last-region mode. Failures are not important.
fn remember_region(region: DesktopRect) {
    if let Some(file) = last_region_file() {
        if let Some(parent) = file.parent() {
            let _ = fs::create_dir_all(parent);
        }
        let _ = fs::write(file, region.to_text());
    }
}

/// Captures the desktop in the configured mode and saves or copies the result.
///
/// Returns `None` when the user closes the overlay without saving.
#[cfg(windows)]
pub fn take_screenshot(config: &ScreenshotConfig) -> anyhow::Result<Option<ScreenshotSummary>> {
    use anyhow::{Context, bail};
    use canvas::Area;
    use editor::Outcome;

    overlay::enable_dpi_awareness();
    if let Some(delay) = config.delay {
        std::thread::sleep(delay);
    }
    let white = config.sdr_white_nits;
    // Cuts a desktop rectangle from a capture of every monitor.
    let crop_desktop = |region: DesktopRect| -> anyhow::Result<_> {
        let desktop = capture::capture_desktop(white)?;
        let area = Area::new(
            region.left - desktop.left,
            region.top - desktop.top,
            region.left - desktop.left + region.width as i32,
            region.top - desktop.top + region.height as i32,
        )
        .intersect(&desktop.image.bounds());
        if area.is_empty() {
            bail!("the region {} is outside the desktop", region.to_text());
        }
        Ok((
            desktop.image.crop(area),
            desktop.hdr,
            desktop.sdr_white_nits,
        ))
    };
    let mut write_file = config.save;
    let mut copy_only_is_off = true;
    let (image, hdr, sdr_white_nits) = match config.mode {
        CaptureMode::Region => {
            let captured = capture::capture_monitor(config.monitor, white)?;
            let (left, top) = (captured.left, captured.top);
            let (hdr, nits) = (captured.hdr, captured.sdr_white_nits);
            let (outcome, image, selection) = overlay::edit(captured)?;
            match outcome {
                Outcome::Save => {}
                Outcome::CopyOnly => {
                    write_file = false;
                    copy_only_is_off = false;
                }
                Outcome::Cancel | Outcome::Continue => return Ok(None),
            }
            if let Some(selection) = selection {
                remember_region(DesktopRect {
                    left: left + selection.left,
                    top: top + selection.top,
                    width: selection.width() as u32,
                    height: selection.height() as u32,
                });
            }
            (image, hdr, nits)
        }
        CaptureMode::Monitor => {
            let captured = capture::capture_monitor(config.monitor, white)?;
            (captured.image, captured.hdr, captured.sdr_white_nits)
        }
        CaptureMode::Fullscreen => {
            let desktop = capture::capture_desktop(white)?;
            (desktop.image, desktop.hdr, desktop.sdr_white_nits)
        }
        CaptureMode::Window => {
            let area = capture::foreground_window_area()?;
            crop_desktop(DesktopRect {
                left: area.left,
                top: area.top,
                width: area.width() as u32,
                height: area.height() as u32,
            })?
        }
        CaptureMode::LastRegion => {
            crop_desktop(last_region().context("no region is stored yet; capture a region first")?)?
        }
        CaptureMode::Custom(region) => {
            remember_region(region);
            crop_desktop(region)?
        }
    };
    // Encode once. The file and the upload use the same bytes.
    let upload_wanted = config.upload.is_some() && copy_only_is_off;
    // The extension of the output path selects the format.
    let format = format::ImageFormat::from_path(&config.output).unwrap_or_default();
    let encoded = if write_file || upload_wanted {
        format.encode(image.to_rgb_bytes(), image.width(), image.height())?
    } else {
        Vec::new()
    };
    let output = if write_file {
        if let Some(parent) = config
            .output
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            fs::create_dir_all(parent)
                .with_context(|| format!("could not create {}", parent.display()))?;
        }
        fs::write(&config.output, &encoded)
            .with_context(|| format!("could not write {}", config.output.display()))?;
        Some(config.output.clone())
    } else {
        None
    };
    let (mut uploaded, mut upload_error) = (None, None);
    if let Some(uploader) = config.upload.as_ref().filter(|_| upload_wanted) {
        let file_name = config.output.file_name().map_or_else(
            || "screenshot.png".to_owned(),
            |name| name.to_string_lossy().into_owned(),
        );
        let sent = http::send(&uploader.request(&file_name, &encoded))
            .map_err(|error| format!("{error:#}"))
            .and_then(|response| uploader.result(&response, &file_name));
        match sent {
            Ok(result) => {
                upload::log_upload(&file_name, &result);
                uploaded = Some(result);
            }
            Err(error) => upload_error = Some(error),
        }
    }
    // After an upload the clipboard holds the link. Otherwise it holds the
    // image, and an image that reaches no file must at least reach it.
    let clipboard = match &uploaded {
        Some(result) => Some(overlay::copy_text_to_clipboard(&result.url)),
        None => (config.copy || !write_file).then(|| overlay::copy_to_clipboard(&image)),
    };
    Ok(Some(ScreenshotSummary {
        output,
        width: image.width(),
        height: image.height(),
        copied: matches!(clipboard, Some(Ok(()))),
        clipboard_error: clipboard
            .and_then(Result::err)
            .map(|error| format!("{error:#}")),
        uploaded,
        upload_error,
        hdr,
        sdr_white_nits,
    }))
}

/// Returns the largest size inside a box that keeps the shape of a picture.
///
/// A picture that fits keeps its size.
#[must_use]
pub fn size_inside(width: u32, height: u32, box_width: u32, box_height: u32) -> (u32, u32) {
    if width <= box_width && height <= box_height {
        return (width.max(1), height.max(1));
    }
    let scale = (f64::from(box_width) / f64::from(width.max(1)))
        .min(f64::from(box_height) / f64::from(height.max(1)));
    (
        ((f64::from(width) * scale).floor() as u32).max(1),
        ((f64::from(height) * scale).floor() as u32).max(1),
    )
}

/// Opens a saved screenshot in the annotation overlay and saves the result
/// over the file.
///
/// The image appears on the selected monitor with itself selected. An image
/// larger than the monitor is shown smaller, and the saved result then has
/// the smaller size. Returns `None` when the user closes the overlay without
/// saving.
#[cfg(windows)]
pub fn edit_saved_image(
    path: &Path,
    monitor: MonitorChoice,
    copy: bool,
) -> anyhow::Result<Option<ScreenshotSummary>> {
    use anyhow::Context;
    use canvas::{Area, Canvas, Point, rgb};
    use editor::Outcome;

    overlay::enable_dpi_awareness();
    let decoded = image::open(path)
        .with_context(|| format!("could not read {}", path.display()))?
        .into_rgb8();
    // The capture gives the place and the size of the monitor.
    let mut screen = capture::capture_monitor(monitor, None)?;
    let (screen_width, screen_height) = (screen.image.width(), screen.image.height());
    let (width, height) = size_inside(
        decoded.width(),
        decoded.height(),
        screen_width,
        screen_height,
    );
    let shown = if (width, height) == (decoded.width(), decoded.height()) {
        decoded
    } else {
        image::imageops::resize(
            &decoded,
            width,
            height,
            image::imageops::FilterType::Triangle,
        )
    };
    let pixels = shown
        .pixels()
        .map(|pixel| rgb(pixel[0], pixel[1], pixel[2]))
        .collect();
    let picture =
        Canvas::from_pixels(width, height, pixels).context("the image has an unexpected size")?;
    let left = ((screen_width - width) / 2) as i32;
    let top = ((screen_height - height) / 2) as i32;
    let mut frame = Canvas::new(screen_width, screen_height, rgb(0x0c, 0x0c, 0x0c));
    frame.paste(&picture, Point::new(left, top));
    screen.image = frame;
    let selection = Area::new(left, top, left + width as i32, top + height as i32);
    let (outcome, image, _) = overlay::edit_selected(screen, Some(selection))?;
    let write_file = match outcome {
        Outcome::Save => true,
        Outcome::CopyOnly => false,
        Outcome::Cancel | Outcome::Continue => return Ok(None),
    };
    if write_file {
        let format = format::ImageFormat::from_path(path).unwrap_or_default();
        let encoded = format.encode(image.to_rgb_bytes(), image.width(), image.height())?;
        fs::write(path, encoded).with_context(|| format!("could not write {}", path.display()))?;
    }
    let clipboard = (copy || !write_file).then(|| overlay::copy_to_clipboard(&image));
    Ok(Some(ScreenshotSummary {
        output: write_file.then(|| path.to_path_buf()),
        width: image.width(),
        height: image.height(),
        copied: matches!(clipboard, Some(Ok(()))),
        clipboard_error: clipboard
            .and_then(Result::err)
            .map(|error| format!("{error:#}")),
        uploaded: None,
        upload_error: None,
        hdr: false,
        sdr_white_nits: 80.0,
    }))
}

/// Reports that image editing is not available on this platform.
#[cfg(not(windows))]
pub fn edit_saved_image(
    _path: &Path,
    _monitor: MonitorChoice,
    _copy: bool,
) -> anyhow::Result<Option<ScreenshotSummary>> {
    anyhow::bail!("screenshot editing is available on Windows only")
}

/// Places a saved PNG file on the clipboard.
#[cfg(windows)]
pub fn copy_file_to_clipboard(path: &Path) -> anyhow::Result<()> {
    use anyhow::Context;

    let decoded = image::open(path)
        .with_context(|| format!("could not read {}", path.display()))?
        .into_rgb8();
    let pixels = decoded
        .pixels()
        .map(|pixel| canvas::rgb(pixel[0], pixel[1], pixel[2]))
        .collect();
    let image = canvas::Canvas::from_pixels(decoded.width(), decoded.height(), pixels)
        .context("the image has an unexpected size")?;
    overlay::copy_to_clipboard(&image)
}

/// Returns the platform support error without creating an output file.
#[cfg(not(windows))]
pub fn take_screenshot(_config: &ScreenshotConfig) -> anyhow::Result<Option<ScreenshotSummary>> {
    anyhow::bail!("record currently takes screenshots on Windows 10/11 only")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_picture_fits_a_box_and_keeps_its_shape() {
        assert_eq!(size_inside(800, 600, 1_920, 1_080), (800, 600));
        assert_eq!(size_inside(3_840, 2_160, 1_920, 1_080), (1_920, 1_080));
        assert_eq!(size_inside(5_120, 1_440, 2_560, 1_440), (2_560, 720));
        assert_eq!(size_inside(1_000, 4_000, 1_920, 1_080), (270, 1_080));
        assert_eq!(size_inside(0, 0, 10, 10), (1, 1));
    }

    #[test]
    fn desktop_rect_round_trips_and_accepts_negative_origins() {
        let region = DesktopRect::parse(" -1920, 40 ,800,600").unwrap();
        assert_eq!((region.left, region.top), (-1920, 40));
        assert_eq!((region.width, region.height), (800, 600));
        assert_eq!(DesktopRect::parse(&region.to_text()), Ok(region));
    }

    #[test]
    fn desktop_rect_rejects_bad_shapes() {
        for value in ["1,2,3", "1,2,0,4", "1,2,3,-4", "a,b,c,d", "1,2,3,4,5"] {
            assert!(DesktopRect::parse(value).is_err(), "{value} should fail");
        }
    }

    #[test]
    fn output_names_do_not_replace_existing_files() {
        let directory = tempfile::tempdir().unwrap();
        let first = unique_output_path(directory.path(), "20260101-120000", "png");
        assert!(first.ends_with("screenshot-20260101-120000.png"));
        fs::write(&first, b"x").unwrap();
        let second = unique_output_path(directory.path(), "20260101-120000", "png");
        assert!(second.ends_with("screenshot-20260101-120000-2.png"));
    }
}
