//! One-frame monitor capture through DXGI desktop duplication.
//!
//! Windows composes an HDR desktop in linear scRGB. The classic duplication
//! call converts that to 8-bit BGRA with a fixed curve, which gives the bright,
//! washed-out picture that HDR screenshots are known for. This module asks for
//! the half-float surface with `IDXGIOutput5::DuplicateOutput1` and converts
//! it with the SDR white level of the monitor. SDR monitors keep the direct
//! 8-bit path.

use std::{mem, ptr, slice, thread, time::Duration};

use anyhow::{Context, Result, anyhow, bail};
use windows::{
    Win32::{
        Devices::Display::{
            DISPLAYCONFIG_DEVICE_INFO_GET_SDR_WHITE_LEVEL,
            DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME, DISPLAYCONFIG_MODE_INFO,
            DISPLAYCONFIG_PATH_INFO, DISPLAYCONFIG_SDR_WHITE_LEVEL,
            DISPLAYCONFIG_SOURCE_DEVICE_NAME, DisplayConfigGetDeviceInfo,
            GetDisplayConfigBufferSizes, QDC_ONLY_ACTIVE_PATHS, QueryDisplayConfig,
        },
        Foundation::{POINT, RECT},
        Graphics::{
            Direct3D11::{
                D3D11_CPU_ACCESS_READ, D3D11_MAP_READ, D3D11_MAPPED_SUBRESOURCE,
                D3D11_TEXTURE2D_DESC, D3D11_USAGE_STAGING, ID3D11Device, ID3D11Texture2D,
            },
            Dwm::{DWMWA_EXTENDED_FRAME_BOUNDS, DwmGetWindowAttribute},
            Dxgi::{
                Common::{
                    DXGI_COLOR_SPACE_RGB_FULL_G2084_NONE_P2020, DXGI_FORMAT_B8G8R8A8_UNORM,
                    DXGI_FORMAT_R16G16B16A16_FLOAT, DXGI_MODE_ROTATION_ROTATE90,
                    DXGI_MODE_ROTATION_ROTATE180, DXGI_MODE_ROTATION_ROTATE270,
                },
                DXGI_ERROR_WAIT_TIMEOUT, DXGI_OUTPUT_DESC, IDXGIAdapter1, IDXGIDevice, IDXGIOutput,
                IDXGIOutput1, IDXGIOutput5, IDXGIOutput6, IDXGIOutputDuplication,
            },
            Gdi::{MONITOR_DEFAULTTOPRIMARY, MonitorFromPoint},
        },
        UI::{
            HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI},
            WindowsAndMessaging::{GetCursorPos, GetForegroundWindow, GetWindowRect},
        },
    },
    core::Interface,
};

use super::{
    MonitorChoice,
    canvas::{Area, Canvas, Point},
    color::{ToneMap, half_to_f32},
};
use crate::video::create_d3d_device;

/// SDR white level assumed when Windows does not report one.
const FALLBACK_SDR_WHITE_NITS: f32 = 240.0;

/// One captured monitor with the facts the overlay needs to cover it.
pub(crate) struct CapturedMonitor {
    /// The desktop image in SDR.
    pub(crate) image: Canvas,
    /// Left edge of the monitor in virtual desktop pixels.
    pub(crate) left: i32,
    /// Top edge of the monitor in virtual desktop pixels.
    pub(crate) top: i32,
    /// Display scale of the monitor, where 1.0 is 96 DPI.
    pub(crate) scale: f32,
    /// Reports whether the monitor was in HDR mode.
    pub(crate) hdr: bool,
    /// SDR white level used for the conversion, in nits.
    pub(crate) sdr_white_nits: f32,
}

/// Captures the current image of one monitor.
pub(crate) fn capture_monitor(
    choice: MonitorChoice,
    sdr_white_override: Option<f32>,
) -> Result<CapturedMonitor> {
    let device = create_d3d_device()?;
    let mut outputs = enumerate_outputs(&device)?;
    let position = match choice {
        MonitorChoice::Index(index) => {
            if index as usize >= outputs.len() {
                bail!("monitor {index} does not exist; run record video --monitor list");
            }
            Some(index as usize)
        }
        MonitorChoice::Primary => outputs.iter().position(|(_, description)| {
            description.DesktopCoordinates.left == 0 && description.DesktopCoordinates.top == 0
        }),
        MonitorChoice::Pointer => {
            let mut pointer = POINT::default();
            // SAFETY: The pointer refers to a live local.
            let monitor = unsafe {
                // A failure leaves the origin, which selects the primary monitor.
                let _ = GetCursorPos(&mut pointer);
                MonitorFromPoint(pointer, MONITOR_DEFAULTTOPRIMARY)
            };
            outputs
                .iter()
                .position(|(_, description)| description.Monitor == monitor)
        }
    };
    let (output, description) = outputs.swap_remove(position.unwrap_or(0));
    capture_output(&device, &output, &description, sdr_white_override)
}

/// Captures every monitor and joins the images into one virtual desktop.
///
/// The result uses the virtual desktop origin as `left` and `top`. Areas that
/// no monitor covers stay black.
pub(crate) fn capture_desktop(sdr_white_override: Option<f32>) -> Result<CapturedMonitor> {
    let device = create_d3d_device()?;
    let mut monitors = Vec::new();
    for (output, description) in enumerate_outputs(&device)? {
        monitors.push(capture_output(
            &device,
            &output,
            &description,
            sdr_white_override,
        )?);
    }
    if monitors.len() == 1 {
        return monitors.pop().context("no monitor was captured");
    }
    let edge = |pick: fn(&CapturedMonitor) -> i32, wide: bool| {
        let values = monitors.iter().map(pick);
        if wide { values.max() } else { values.min() }.unwrap_or(0)
    };
    let left = edge(|monitor| monitor.left, false);
    let top = edge(|monitor| monitor.top, false);
    let right = edge(|monitor| monitor.left + monitor.image.width() as i32, true);
    let bottom = edge(|monitor| monitor.top + monitor.image.height() as i32, true);
    let mut image = Canvas::new((right - left) as u32, (bottom - top) as u32, 0);
    for monitor in &monitors {
        image.paste(
            &monitor.image,
            Point::new(monitor.left - left, monitor.top - top),
        );
    }
    let primary = monitors
        .iter()
        .find(|monitor| monitor.left == 0 && monitor.top == 0)
        .unwrap_or(&monitors[0]);
    Ok(CapturedMonitor {
        image,
        left,
        top,
        scale: primary.scale,
        hdr: monitors.iter().any(|monitor| monitor.hdr),
        sdr_white_nits: primary.sdr_white_nits,
    })
}

/// Returns the visible bounds of the foreground window in desktop pixels.
pub(crate) fn foreground_window_area() -> Result<Area> {
    // SAFETY: The call has no arguments and returns a handle or null.
    let window = unsafe { GetForegroundWindow() };
    if window.is_invalid() {
        bail!("no window is active");
    }
    let mut bounds = RECT::default();
    // The extended frame excludes the invisible resize border of the window.
    // SAFETY: The pointer and size describe the live local rectangle.
    let framed = unsafe {
        DwmGetWindowAttribute(
            window,
            DWMWA_EXTENDED_FRAME_BOUNDS,
            ptr::from_mut(&mut bounds).cast(),
            mem::size_of::<RECT>() as u32,
        )
    };
    if framed.is_err() {
        // SAFETY: The pointer refers to the live local rectangle.
        unsafe { GetWindowRect(window, &mut bounds) }
            .context("could not read the active window position")?;
    }
    Ok(Area::new(
        bounds.left,
        bounds.top,
        bounds.right,
        bounds.bottom,
    ))
}

/// Lists the DXGI outputs of the default adapter with their descriptions.
fn enumerate_outputs(device: &ID3D11Device) -> Result<Vec<(IDXGIOutput, DXGI_OUTPUT_DESC)>> {
    let dxgi_device: IDXGIDevice = device
        .cast()
        .context("the D3D11 device does not expose DXGI")?;
    // SAFETY: The device is live. The adapter is returned as an owned object.
    let adapter: IDXGIAdapter1 = unsafe { dxgi_device.GetAdapter() }?.cast()?;
    let mut outputs = Vec::new();
    // SAFETY: EnumOutputs fails with DXGI_ERROR_NOT_FOUND after the last output.
    while let Ok(output) = unsafe { adapter.EnumOutputs(outputs.len() as u32) } {
        // SAFETY: The output is live and GetDesc returns a value.
        let description = unsafe { output.GetDesc() }?;
        outputs.push((output, description));
    }
    if outputs.is_empty() {
        bail!("no monitor output is available for capture");
    }
    Ok(outputs)
}

/// Captures one DXGI output and converts it to SDR.
fn capture_output(
    device: &ID3D11Device,
    output: &IDXGIOutput,
    description: &DXGI_OUTPUT_DESC,
    sdr_white_override: Option<f32>,
) -> Result<CapturedMonitor> {
    let hdr = output
        .cast::<IDXGIOutput6>()
        .ok()
        // SAFETY: The output is a live COM object and GetDesc1 fills a value.
        .and_then(|output| unsafe { output.GetDesc1() }.ok())
        .is_some_and(|desc| desc.ColorSpace == DXGI_COLOR_SPACE_RGB_FULL_G2084_NONE_P2020);
    let sdr_white_nits = sdr_white_override
        .or_else(|| hdr.then(|| sdr_white_level(description)).flatten())
        .unwrap_or(FALLBACK_SDR_WHITE_NITS);
    let duplication = duplicate(device, output, hdr)?;
    // SAFETY: The duplication object is live and GetDesc returns a value.
    let rotation = unsafe { duplication.GetDesc() }.Rotation;
    // SAFETY: The device owns the duplication and every texture it returns.
    let surface = unsafe { read_frame(device, &duplication, sdr_white_nits) }?;
    let quarter_turns = match rotation {
        DXGI_MODE_ROTATION_ROTATE90 => 1,
        DXGI_MODE_ROTATION_ROTATE180 => 2,
        DXGI_MODE_ROTATION_ROTATE270 => 3,
        _ => 0,
    };
    let image = if quarter_turns == 0 {
        surface
    } else {
        surface.rotated(quarter_turns)
    };
    let coords = description.DesktopCoordinates;
    let (mut dpi_x, mut dpi_y) = (96, 96);
    // SAFETY: Both pointers refer to live locals for the duration of the call.
    let dpi = unsafe {
        GetDpiForMonitor(
            description.Monitor,
            MDT_EFFECTIVE_DPI,
            &mut dpi_x,
            &mut dpi_y,
        )
    };
    let scale = if dpi.is_ok() {
        dpi_x as f32 / 96.0
    } else {
        1.0
    };
    Ok(CapturedMonitor {
        image,
        left: coords.left,
        top: coords.top,
        scale,
        hdr,
        sdr_white_nits,
    })
}

/// Starts desktop duplication, with a half-float surface on HDR monitors.
fn duplicate(
    device: &ID3D11Device,
    output: &IDXGIOutput,
    hdr: bool,
) -> Result<IDXGIOutputDuplication> {
    if hdr && let Ok(output5) = output.cast::<IDXGIOutput5>() {
        let formats = [DXGI_FORMAT_R16G16B16A16_FLOAT, DXGI_FORMAT_B8G8R8A8_UNORM];
        // SAFETY: The device and output are live. The format list is a live slice.
        if let Ok(duplication) = unsafe { output5.DuplicateOutput1(device, 0, &formats) } {
            return Ok(duplication);
        }
    }
    let output1: IDXGIOutput1 = output
        .cast()
        .context("this Windows version cannot duplicate the desktop")?;
    // SAFETY: The device and output are live COM objects.
    unsafe { output1.DuplicateOutput(device) }.context("Windows refused desktop duplication")
}

/// Waits for a desktop frame and converts it to an SDR image.
///
/// # Safety
///
/// `device` must be the device that created `duplication`.
unsafe fn read_frame(
    device: &ID3D11Device,
    duplication: &IDXGIOutputDuplication,
    sdr_white_nits: f32,
) -> Result<Canvas> {
    let context = unsafe { device.GetImmediateContext()? };
    let mut staging: Option<ID3D11Texture2D> = None;
    let mut description = D3D11_TEXTURE2D_DESC::default();
    // The first frames can arrive without a desktop image. Keep the newest
    // copy and stop at the first frame that Windows marks as presented.
    for _ in 0..20 {
        let mut frame_info = Default::default();
        let mut resource = None;
        let acquired = unsafe { duplication.AcquireNextFrame(100, &mut frame_info, &mut resource) };
        match acquired {
            Ok(()) => {}
            Err(error) if error.code() == DXGI_ERROR_WAIT_TIMEOUT => {
                if staging.is_some() {
                    break;
                }
                continue;
            }
            Err(error) => return Err(error).context("desktop duplication returned an error"),
        }
        let copied = (|| -> Result<()> {
            let texture: ID3D11Texture2D = resource
                .ok_or_else(|| anyhow!("desktop duplication returned no frame resource"))?
                .cast()
                .context("the duplicated frame is not a texture")?;
            unsafe { texture.GetDesc(&mut description) };
            if staging.is_none() {
                let staging_description = D3D11_TEXTURE2D_DESC {
                    MipLevels: 1,
                    ArraySize: 1,
                    Usage: D3D11_USAGE_STAGING,
                    BindFlags: 0,
                    CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
                    MiscFlags: 0,
                    ..description
                };
                unsafe { device.CreateTexture2D(&staging_description, None, Some(&mut staging))? };
            }
            let target = staging
                .as_ref()
                .context("CreateTexture2D returned no staging texture")?;
            unsafe { context.CopyResource(target, &texture) };
            Ok(())
        })();
        // The surface must return to Windows before the next acquire call.
        let _ = unsafe { duplication.ReleaseFrame() };
        copied?;
        if frame_info.LastPresentTime != 0 {
            break;
        }
        thread::sleep(Duration::from_millis(10));
    }
    let staging = staging.context("Windows did not deliver a desktop frame")?;
    let (width, height) = (description.Width as usize, description.Height as usize);
    let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
    unsafe { context.Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))? };
    let stride = mapped.RowPitch as usize;
    let mut pixels = Vec::with_capacity(width * height);
    let converted = match description.Format {
        DXGI_FORMAT_R16G16B16A16_FLOAT => {
            let tone_map = ToneMap::new(sdr_white_nits);
            for row in 0..height {
                // SAFETY: A mapped row holds `width` pixels of four `u16`
                // values. D3D aligns mapped rows to at least 16 bytes.
                let texels = unsafe {
                    slice::from_raw_parts(mapped.pData.cast::<u8>().add(row * stride), width * 8)
                };
                pixels.extend(texels.as_chunks::<8>().0.iter().map(|texel| {
                    let channel = |index: usize| {
                        half_to_f32(u16::from_le_bytes([texel[index], texel[index + 1]]))
                    };
                    tone_map.map(channel(0), channel(2), channel(4))
                }));
            }
            Ok(())
        }
        DXGI_FORMAT_B8G8R8A8_UNORM => {
            for row in 0..height {
                // SAFETY: A mapped row holds `width` four-byte BGRA pixels.
                let texels = unsafe {
                    slice::from_raw_parts(mapped.pData.cast::<u8>().add(row * stride), width * 4)
                };
                pixels.extend(
                    texels
                        .as_chunks::<4>()
                        .0
                        .iter()
                        .map(|texel| u32::from_le_bytes([texel[0], texel[1], texel[2], 0])),
                );
            }
            Ok(())
        }
        format => Err(anyhow!(
            "the desktop uses the unsupported surface format {}",
            format.0
        )),
    };
    unsafe { context.Unmap(&staging, 0) };
    converted?;
    Canvas::from_pixels(width as u32, height as u32, pixels)
        .context("the desktop frame has an unexpected size")
}

/// Asks Windows for the SDR white level of the monitor behind a DXGI output.
fn sdr_white_level(output: &DXGI_OUTPUT_DESC) -> Option<f32> {
    let (mut path_count, mut mode_count) = (0, 0);
    // SAFETY: Both pointers refer to live locals.
    unsafe { GetDisplayConfigBufferSizes(QDC_ONLY_ACTIVE_PATHS, &mut path_count, &mut mode_count) }
        .ok()
        .ok()?;
    let mut paths = vec![DISPLAYCONFIG_PATH_INFO::default(); path_count as usize];
    let mut modes = vec![DISPLAYCONFIG_MODE_INFO::default(); mode_count as usize];
    // SAFETY: Both buffers hold the element counts that the call receives.
    unsafe {
        QueryDisplayConfig(
            QDC_ONLY_ACTIVE_PATHS,
            &mut path_count,
            paths.as_mut_ptr(),
            &mut mode_count,
            modes.as_mut_ptr(),
            None,
        )
    }
    .ok()
    .ok()?;
    paths.truncate(path_count as usize);
    for path in &paths {
        let mut source = DISPLAYCONFIG_SOURCE_DEVICE_NAME::default();
        source.header.r#type = DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME;
        source.header.size = mem::size_of::<DISPLAYCONFIG_SOURCE_DEVICE_NAME>() as u32;
        source.header.adapterId = path.sourceInfo.adapterId;
        source.header.id = path.sourceInfo.id;
        // SAFETY: The header describes the complete structure it starts.
        if unsafe { DisplayConfigGetDeviceInfo(&mut source.header) } != 0
            || source.viewGdiDeviceName != output.DeviceName
        {
            continue;
        }
        let mut level = DISPLAYCONFIG_SDR_WHITE_LEVEL::default();
        level.header.r#type = DISPLAYCONFIG_DEVICE_INFO_GET_SDR_WHITE_LEVEL;
        level.header.size = mem::size_of::<DISPLAYCONFIG_SDR_WHITE_LEVEL>() as u32;
        level.header.adapterId = path.targetInfo.adapterId;
        level.header.id = path.targetInfo.id;
        // SAFETY: The header describes the complete structure it starts.
        if unsafe { DisplayConfigGetDeviceInfo(&mut level.header) } == 0 && level.SDRWhiteLevel > 0
        {
            // Windows reports the level in thousandths of 80 nits.
            return Some(level.SDRWhiteLevel as f32 * 80.0 / 1000.0);
        }
    }
    None
}
