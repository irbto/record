//! Files and folders through the Windows shell.
//!
//! The desktop program shows a file in the file explorer, opens a file or a
//! folder with its program, moves a file to the Recycle Bin, and asks for a
//! small picture of a file. Each of these is one shell call. A command line
//! for `explorer.exe` is not reliable for them, because the explorer reads
//! its arguments in its own way.
//!
//! Every call runs on a new thread with its own COM apartment, so the caller
//! needs no setup and no message loop.

use std::path::Path;

use anyhow::{Context, Result, bail};

/// Smallest edge of a thumbnail in pixels.
pub const THUMBNAIL_MIN: u32 = 16;
/// Largest edge of a thumbnail in pixels.
pub const THUMBNAIL_MAX: u32 = 1_024;

/// Removes the long path prefix that a canonical Windows path carries.
///
/// The shell does not accept `\\?\C:\folder`. A network path in that form,
/// `\\?\UNC\server\share`, becomes `\\server\share`.
#[must_use]
pub fn plain_path(path: &str) -> String {
    match path.strip_prefix(r"\\?\") {
        Some(rest) => match rest.strip_prefix(r"UNC\") {
            Some(share) => format!(r"\\{share}"),
            None => rest.to_owned(),
        },
        None => path.to_owned(),
    }
}

/// Converts a path to UTF-16 with one terminator, in the form the shell reads.
#[must_use]
pub fn wide_path(path: &Path) -> Vec<u16> {
    plain_path(&path.display().to_string())
        .encode_utf16()
        .chain([0])
        .collect()
}

/// Converts a path to a UTF-16 list of one path, which ends with two
/// terminators.
#[must_use]
pub fn wide_path_list(path: &Path) -> Vec<u16> {
    let mut list = wide_path(path);
    list.push(0);
    list
}

/// Converts the pixels of a shell bitmap to straight RGBA.
///
/// The shell gives premultiplied BGRA. A picture without transparency often
/// has an alpha of zero in every pixel. Such a picture is opaque.
#[must_use]
pub fn rgba_from_shell(bgra: &[u8]) -> Vec<u8> {
    let (pixels, _) = bgra.as_chunks::<4>();
    let opaque = pixels.iter().all(|pixel| pixel[3] == 0);
    pixels
        .iter()
        .flat_map(|&[blue, green, red, alpha]| {
            if opaque || alpha == 255 {
                return [red, green, blue, 255];
            }
            if alpha == 0 {
                return [0; 4];
            }
            let straight = |channel: u8| {
                ((u32::from(channel) * 255 + u32::from(alpha) / 2) / u32::from(alpha)).min(255)
                    as u8
            };
            [straight(red), straight(green), straight(blue), alpha]
        })
        .collect()
}

/// Encodes straight RGBA pixels as a PNG file.
pub fn png(rgba: Vec<u8>, width: u32, height: u32) -> Result<Vec<u8>> {
    use image::{ExtendedColorType, ImageEncoder, codecs::png::PngEncoder};

    if width == 0 || height == 0 || rgba.len() != width as usize * height as usize * 4 {
        bail!("the picture has an unexpected size");
    }
    let mut bytes = Vec::new();
    PngEncoder::new(&mut bytes)
        .write_image(&rgba, width, height, ExtendedColorType::Rgba8)
        .context("could not encode the picture")?;
    Ok(bytes)
}

/// Runs one shell call on a new thread with a COM apartment.
#[cfg(windows)]
fn on_shell_thread<T: Send>(work: impl FnOnce() -> Result<T> + Send) -> Result<T> {
    use std::thread;

    use windows::Win32::System::Com::{
        COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoInitializeEx, CoUninitialize,
    };

    thread::scope(|scope| {
        thread::Builder::new()
            .name("shell".to_owned())
            .spawn_scoped(scope, || {
                // SAFETY: The thread is new, so it has no apartment yet. The
                // matching uninitialize call runs on the same thread.
                let initialized = unsafe {
                    CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE)
                };
                let result = work();
                if initialized.is_ok() {
                    // SAFETY: The initialize call above succeeded on this
                    // thread. Every COM object of `work` is released.
                    unsafe { CoUninitialize() };
                }
                result
            })
            .context("could not start the shell thread")?
            .join()
            .map_err(|_| anyhow::anyhow!("the shell call failed"))?
    })
}

/// Opens the folder of a file or a folder in the file explorer and selects
/// it there.
#[cfg(windows)]
pub fn reveal(path: &Path) -> Result<()> {
    use windows::{
        Win32::UI::Shell::{ILCreateFromPathW, ILFree, SHOpenFolderAndSelectItems},
        core::PCWSTR,
    };

    let wide = wide_path(path);
    on_shell_thread(move || {
        // SAFETY: `wide` is a terminated string that lives for the call. The
        // item list is checked, used once, and freed here.
        unsafe {
            let item = ILCreateFromPathW(PCWSTR(wide.as_ptr()));
            if item.is_null() {
                bail!("the file explorer does not find {}", path.display());
            }
            // Without a list of children the shell opens the parent folder
            // and selects the item itself.
            let shown = SHOpenFolderAndSelectItems(item, None, 0);
            ILFree(Some(item));
            shown.with_context(|| format!("could not show {}", path.display()))
        }
    })
}

/// Opens a file with the program that Windows uses for it, or a folder in
/// the file explorer.
#[cfg(windows)]
pub fn open(path: &Path) -> Result<()> {
    use windows::{
        Win32::UI::{Shell::ShellExecuteW, WindowsAndMessaging::SW_SHOWNORMAL},
        core::{PCWSTR, w},
    };

    /// Largest result of `ShellExecuteW` that reports a failure.
    const FAILURE_LIMIT: isize = 32;
    let wide = wide_path(path);
    on_shell_thread(move || {
        // SAFETY: Both strings are terminated and live for the call.
        let result = unsafe {
            ShellExecuteW(
                None,
                w!("open"),
                PCWSTR(wide.as_ptr()),
                PCWSTR::null(),
                PCWSTR::null(),
                SW_SHOWNORMAL,
            )
        };
        let code = result.0 as isize;
        if code > FAILURE_LIMIT {
            Ok(())
        } else {
            bail!("Windows could not open {} (code {code})", path.display())
        }
    })
}

/// Moves a file or a folder to the Recycle Bin.
///
/// Windows asks the user first when it can only delete the item for good.
#[cfg(windows)]
pub fn recycle(path: &Path) -> Result<()> {
    use windows::{
        Win32::UI::Shell::{
            FO_DELETE, FOF_ALLOWUNDO, FOF_NOCONFIRMATION, FOF_NOERRORUI, FOF_SILENT,
            FOF_WANTNUKEWARNING, SHFILEOPSTRUCTW, SHFileOperationW,
        },
        core::PCWSTR,
    };

    let list = wide_path_list(path);
    on_shell_thread(move || {
        let flags =
            FOF_ALLOWUNDO | FOF_NOCONFIRMATION | FOF_NOERRORUI | FOF_SILENT | FOF_WANTNUKEWARNING;
        let mut operation = SHFILEOPSTRUCTW {
            wFunc: FO_DELETE,
            pFrom: PCWSTR(list.as_ptr()),
            fFlags: flags.0 as u16,
            ..Default::default()
        };
        // SAFETY: The structure and the path list live for the call. The
        // list ends with two terminators, as the call requires.
        let code = unsafe { SHFileOperationW(&mut operation) };
        if code != 0 {
            bail!(
                "Windows could not move {} to the Recycle Bin (code {code})",
                path.display()
            );
        }
        if operation.fAnyOperationsAborted.as_bool() {
            bail!("{} was not deleted", path.display());
        }
        Ok(())
    })
}

/// Returns a small PNG picture of a file, at most `edge` pixels in each
/// direction.
///
/// Windows gives the picture that the file explorer shows: a scaled image, a
/// frame of a video, the cover of an audio file, or the icon of the file
/// type.
#[cfg(windows)]
pub fn thumbnail_png(path: &Path, edge: u32) -> Result<Vec<u8>> {
    use std::mem;

    use windows::{
        Win32::{
            Foundation::SIZE,
            Graphics::Gdi::{
                BI_RGB, BITMAP, BITMAPINFO, BITMAPINFOHEADER, CreateCompatibleDC, DIB_RGB_COLORS,
                DeleteDC, DeleteObject, GetDIBits, GetObjectW,
            },
            UI::Shell::{
                IShellItemImageFactory, SHCreateItemFromParsingName, SIIGBF_BIGGERSIZEOK,
                SIIGBF_RESIZETOFIT,
            },
        },
        core::PCWSTR,
    };

    let edge = edge.clamp(THUMBNAIL_MIN, THUMBNAIL_MAX) as i32;
    let wide = wide_path(path);
    let (bgra, width, height) = on_shell_thread(move || {
        // SAFETY: `wide` is a terminated string that lives for the call. The
        // bitmap is checked, read into a buffer of its exact size, and
        // deleted before the block ends.
        unsafe {
            let factory: IShellItemImageFactory =
                SHCreateItemFromParsingName(PCWSTR(wide.as_ptr()), None)
                    .with_context(|| format!("the shell does not find {}", path.display()))?;
            let size = SIZE { cx: edge, cy: edge };
            let bitmap = factory
                .GetImage(size, SIIGBF_RESIZETOFIT)
                // A cached larger picture is better than none.
                .or_else(|_| factory.GetImage(size, SIIGBF_BIGGERSIZEOK))
                .with_context(|| format!("Windows has no picture of {}", path.display()))?;
            let mut shape = BITMAP::default();
            let read = GetObjectW(
                bitmap.into(),
                mem::size_of::<BITMAP>() as i32,
                Some((&raw mut shape).cast()),
            );
            let (width, height) = (shape.bmWidth, shape.bmHeight.abs());
            let limit = THUMBNAIL_MAX as i32 * 4;
            if read == 0 || width <= 0 || height <= 0 || width > limit || height > limit {
                let _ = DeleteObject(bitmap.into());
                bail!("the picture of {} has no size", path.display());
            }
            let mut info = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: mem::size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: width,
                    // A negative height asks for the top row first.
                    biHeight: -height,
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB.0,
                    ..Default::default()
                },
                ..Default::default()
            };
            let mut bgra = vec![0_u8; width as usize * height as usize * 4];
            let context = CreateCompatibleDC(None);
            let rows = GetDIBits(
                context,
                bitmap,
                0,
                height as u32,
                Some(bgra.as_mut_ptr().cast()),
                &mut info,
                DIB_RGB_COLORS,
            );
            let _ = DeleteDC(context);
            let _ = DeleteObject(bitmap.into());
            if rows != height {
                bail!("could not read the picture of {}", path.display());
            }
            Ok((bgra, width as u32, height as u32))
        }
    })?;
    png(rgba_from_shell(&bgra), width, height)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    #[test]
    fn the_long_path_prefix_is_removed() {
        assert_eq!(plain_path(r"\\?\C:\Users\me\a.mp4"), r"C:\Users\me\a.mp4");
        assert_eq!(
            plain_path(r"\\?\UNC\server\share\a.mp4"),
            r"\\server\share\a.mp4"
        );
        assert_eq!(plain_path(r"C:\plain\a.mp4"), r"C:\plain\a.mp4");
        assert_eq!(plain_path(r"\\server\share"), r"\\server\share");
    }

    #[test]
    fn shell_strings_end_with_the_terminators_the_shell_needs() {
        let path = PathBuf::from(r"\\?\C:\a b\c.mp3");
        let wide = wide_path(&path);
        assert_eq!(String::from_utf16_lossy(&wide), "C:\\a b\\c.mp3\0");
        let list = wide_path_list(&path);
        assert_eq!(list.len(), wide.len() + 1);
        assert_eq!(&list[list.len() - 2..], [0, 0]);
    }

    #[test]
    fn shell_pixels_become_straight_rgba() {
        // An alpha of zero everywhere marks an opaque picture.
        assert_eq!(
            rgba_from_shell(&[10, 20, 30, 0, 1, 2, 3, 0]),
            [30, 20, 10, 255, 3, 2, 1, 255]
        );
        // A picture with transparency is premultiplied.
        assert_eq!(
            rgba_from_shell(&[0, 0, 0, 0, 64, 32, 128, 128, 5, 6, 7, 255]),
            [0, 0, 0, 0, 255, 64, 128, 128, 7, 6, 5, 255]
        );
        assert!(rgba_from_shell(&[]).is_empty());
    }

    #[test]
    fn a_picture_encodes_to_a_png_that_decodes_again() {
        let rgba = vec![255, 0, 0, 255, 0, 255, 0, 128];
        let bytes = png(rgba.clone(), 2, 1).unwrap();
        assert_eq!(&bytes[..8], b"\x89PNG\r\n\x1a\n");
        let decoded = image::load_from_memory(&bytes).unwrap().into_rgba8();
        assert_eq!((decoded.width(), decoded.height()), (2, 1));
        assert_eq!(decoded.into_raw(), rgba);
        assert!(png(vec![0; 3], 2, 1).is_err());
        assert!(png(Vec::new(), 0, 0).is_err());
    }
}
