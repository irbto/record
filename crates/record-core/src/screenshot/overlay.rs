//! The full-monitor overlay window, GDI text, and the clipboard.
//!
//! The window covers exactly one monitor and shows the frozen capture, so the
//! desktop appears unchanged until the user draws. Every message becomes one
//! [`Editor`] call. `WM_PAINT` asks the editor for a complete frame and copies
//! it to the screen. Windows merges pending paint requests, so fast pointer
//! motion cannot queue more frames than the screen can show.

use std::{mem, ptr, slice};

use anyhow::{Context, Result, anyhow, bail};
use windows::{
    Win32::{
        Foundation::{COLORREF, GlobalFree, HANDLE, HWND, LPARAM, LRESULT, RECT, WPARAM},
        Graphics::Gdi::{
            ANTIALIASED_QUALITY, BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BeginPaint,
            CLIP_DEFAULT_PRECIS, CreateCompatibleDC, CreateDIBSection, CreateFontW,
            DEFAULT_CHARSET, DIB_RGB_COLORS, DT_CALCRECT, DT_NOPREFIX, DT_SINGLELINE, DeleteDC,
            DeleteObject, DrawTextW, EndPaint, GdiFlush, InvalidateRect, OUT_DEFAULT_PRECIS,
            PAINTSTRUCT, SelectObject, SetBkMode, SetDIBitsToDevice, SetTextColor, TRANSPARENT,
        },
        System::{
            DataExchange::{CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData},
            LibraryLoader::GetModuleHandleW,
            Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalUnlock},
        },
        UI::{
            HiDpi::{DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext},
            Input::KeyboardAndMouse::{
                GetKeyState, ReleaseCapture, SetCapture, SetFocus, VK_BACK, VK_CONTROL, VK_DOWN,
                VK_ESCAPE, VK_LEFT, VK_RETURN, VK_RIGHT, VK_SHIFT, VK_TAB, VK_UP,
            },
            WindowsAndMessaging::{
                CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GWLP_USERDATA,
                GetMessageW, GetWindowLongPtrW, IDC_CROSS, LoadCursorW, MSG, PostQuitMessage,
                RegisterClassW, SW_SHOW, SetForegroundWindow, SetWindowLongPtrW, ShowWindow,
                TranslateMessage, UnregisterClassW, WM_CHAR, WM_DESTROY, WM_ERASEBKGND, WM_KEYDOWN,
                WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_PAINT, WNDCLASSW,
                WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
            },
        },
    },
    core::{PCWSTR, w},
};

use super::{
    canvas::{Area, Canvas, Mask, Point},
    capture::CapturedMonitor,
    editor::{Editor, Key, Outcome, TextRasterizer},
};

/// Window class name of the overlay.
const CLASS_NAME: PCWSTR = w!("RecordScreenshotOverlay");
/// Standard clipboard format number of a device-independent bitmap.
const CF_DIB: u32 = 8;

/// Makes window and capture coordinates equal to physical monitor pixels.
///
/// Windows also requires this mode before it returns HDR duplication frames.
pub(crate) fn enable_dpi_awareness() {
    // SAFETY: The call has no pointer arguments. It fails harmlessly when the
    // process already has a DPI mode.
    let _ = unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
}

/// Everything the window procedure needs between messages.
struct OverlayState {
    editor: Editor,
    frame: Canvas,
    outcome: Outcome,
}

/// Shows the overlay on the captured monitor until the user finishes.
///
/// Returns how the session ended, the final cropped image, and the selection
/// in monitor pixels.
pub(crate) fn edit(captured: CapturedMonitor) -> Result<(Outcome, Canvas, Option<Area>)> {
    edit_selected(captured, None)
}

/// Opens the overlay with a selection already in place.
///
/// A saved image that is opened again starts with itself selected.
pub(crate) fn edit_selected(
    captured: CapturedMonitor,
    selection: Option<Area>,
) -> Result<(Outcome, Canvas, Option<Area>)> {
    let (width, height) = (captured.image.width(), captured.image.height());
    let mut editor = Editor::new(captured.image.clone(), captured.scale, Box::new(GdiText));
    if selection.is_some() {
        editor.set_selection(selection);
    }
    let state = Box::new(OverlayState {
        frame: captured.image,
        editor,
        outcome: Outcome::Cancel,
    });
    let state = Box::into_raw(state);
    // SAFETY: `state` stays valid until `Box::from_raw` below. The window
    // procedure is the only other user, and it runs on this thread inside
    // the message loop.
    let shown = unsafe { run_window(state, captured.left, captured.top, width, height) };
    // SAFETY: `run_window` destroyed the window, so nothing else holds the pointer.
    let state = unsafe { Box::from_raw(state) };
    shown?;
    Ok((
        state.outcome,
        state.editor.result(),
        state.editor.selection(),
    ))
}

/// Creates the overlay window and runs its message loop.
///
/// # Safety
///
/// `state` must point to a live `OverlayState` for the complete call.
unsafe fn run_window(
    state: *mut OverlayState,
    left: i32,
    top: i32,
    width: u32,
    height: u32,
) -> Result<()> {
    let instance = unsafe { GetModuleHandleW(None) }.context("could not find the module handle")?;
    let class = WNDCLASSW {
        lpfnWndProc: Some(window_procedure),
        hInstance: instance.into(),
        hCursor: unsafe { LoadCursorW(None, IDC_CROSS) }.unwrap_or_default(),
        lpszClassName: CLASS_NAME,
        ..Default::default()
    };
    if unsafe { RegisterClassW(&class) } == 0 {
        bail!("could not register the overlay window class");
    }
    let window = unsafe {
        CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW,
            CLASS_NAME,
            w!("record screenshot"),
            WS_POPUP,
            left,
            top,
            width as i32,
            height as i32,
            None,
            None,
            Some(instance.into()),
            None,
        )
    };
    let result = match window {
        Ok(window) => unsafe {
            SetWindowLongPtrW(window, GWLP_USERDATA, state as isize);
            let _ = ShowWindow(window, SW_SHOW);
            let _ = SetForegroundWindow(window);
            let _ = SetFocus(Some(window));
            let mut message = MSG::default();
            while GetMessageW(&mut message, None, 0, 0).0 > 0 {
                let _ = TranslateMessage(&message);
                DispatchMessageW(&message);
            }
            Ok(())
        },
        Err(error) => Err(anyhow!(error).context("could not create the overlay window")),
    };
    let _ = unsafe { UnregisterClassW(CLASS_NAME, Some(instance.into())) };
    result
}

/// Reads the signed pointer position from a mouse message.
pub(super) fn pointer_position(lparam: LPARAM) -> Point {
    let x = (lparam.0 & 0xffff) as u16 as i16;
    let y = ((lparam.0 >> 16) & 0xffff) as u16 as i16;
    Point::new(i32::from(x), i32::from(y))
}

/// Reports whether a virtual key is down for the current message.
fn key_is_down(key: u16) -> bool {
    // SAFETY: GetKeyState has no preconditions.
    unsafe { GetKeyState(i32::from(key)) < 0 }
}

/// Translates a `WM_KEYDOWN` virtual key to an editor key.
fn translate_key(virtual_key: u16) -> Option<Key> {
    let control = key_is_down(VK_CONTROL.0);
    let resize = key_is_down(VK_SHIFT.0);
    let arrow = |dx, dy| Some(Key::Arrow { dx, dy, resize });
    match virtual_key {
        key if key == VK_RETURN.0 => Some(Key::Enter),
        key if key == VK_ESCAPE.0 => Some(Key::Escape),
        key if key == VK_BACK.0 => Some(Key::Backspace),
        key if key == VK_TAB.0 => Some(Key::Tab),
        key if key == VK_LEFT.0 => arrow(-1, 0),
        key if key == VK_RIGHT.0 => arrow(1, 0),
        key if key == VK_UP.0 => arrow(0, -1),
        key if key == VK_DOWN.0 => arrow(0, 1),
        key if control && key == u16::from(b'Z') => Some(Key::Undo),
        key if control && key == u16::from(b'Y') => Some(Key::Redo),
        key if control && key == u16::from(b'C') => Some(Key::Copy),
        key if control && key == u16::from(b'S') => Some(Key::Save),
        _ => None,
    }
}

impl OverlayState {
    /// Applies one input message to the editor.
    fn input(&mut self, message: u32, wparam: WPARAM, lparam: LPARAM) -> Outcome {
        match message {
            WM_LBUTTONDOWN => return self.editor.pointer_down(pointer_position(lparam)),
            WM_MOUSEMOVE => self.editor.pointer_move(pointer_position(lparam)),
            WM_LBUTTONUP => self.editor.pointer_up(pointer_position(lparam)),
            WM_MOUSEWHEEL => {
                let delta = i32::from((wparam.0 >> 16) as u16 as i16);
                self.editor.wheel(if delta.abs() >= 120 {
                    delta / 120
                } else {
                    delta.signum()
                });
            }
            WM_KEYDOWN => {
                if let Some(key) = translate_key(wparam.0 as u16) {
                    return self.editor.key(key);
                }
            }
            WM_CHAR => {
                if let Some(character) =
                    char::from_u32(wparam.0 as u32).filter(|character| !character.is_control())
                {
                    return self.editor.key(Key::Char(character));
                }
            }
            _ => {}
        }
        Outcome::Continue
    }

    /// Draws the current overlay frame into the window.
    ///
    /// # Safety
    ///
    /// `window` must be the live overlay window and the call must come from
    /// its `WM_PAINT` handler.
    unsafe fn paint(&mut self, window: HWND) {
        self.editor.compose(&mut self.frame);
        let (width, height) = (self.frame.width(), self.frame.height());
        let info = bitmap_info(width as i32, -(height as i32));
        let mut paint = PAINTSTRUCT::default();
        unsafe {
            let context = BeginPaint(window, &mut paint);
            SetDIBitsToDevice(
                context,
                0,
                0,
                width,
                height,
                0,
                0,
                0,
                height,
                self.frame.pixels().as_ptr().cast(),
                &info,
                DIB_RGB_COLORS,
            );
            let _ = EndPaint(window, &paint);
        }
    }
}

/// Describes a 32-bit bitmap. A negative height stores the top row first.
pub(super) fn bitmap_info(width: i32, height: i32) -> BITMAPINFO {
    BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: width,
            biHeight: height,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        ..Default::default()
    }
}

/// Handles the overlay messages on the thread that created the window.
unsafe extern "system" fn window_procedure(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match message {
        WM_ERASEBKGND => return LRESULT(1),
        WM_DESTROY => {
            // SAFETY: The call only posts WM_QUIT to this thread.
            unsafe { PostQuitMessage(0) };
            return LRESULT(0);
        }
        WM_PAINT | WM_LBUTTONDOWN | WM_LBUTTONUP | WM_MOUSEMOVE | WM_MOUSEWHEEL | WM_KEYDOWN
        | WM_CHAR => {}
        // SAFETY: Unhandled messages go to the default procedure unchanged.
        _ => return unsafe { DefWindowProcW(window, message, wparam, lparam) },
    }
    // SAFETY: The slot holds null or the pointer that `run_window` stored.
    let state = unsafe { GetWindowLongPtrW(window, GWLP_USERDATA) } as *mut OverlayState;
    if state.is_null() {
        // SAFETY: Messages before the pointer is stored need no state.
        return unsafe { DefWindowProcW(window, message, wparam, lparam) };
    }
    // Capture calls can send messages back into this procedure. They stay
    // outside the exclusive borrow below, and those messages use no state.
    if message == WM_LBUTTONDOWN {
        // SAFETY: The window is live and belongs to this thread.
        unsafe { SetCapture(window) };
    }
    let outcome = {
        // SAFETY: `run_window` keeps the state alive while the window exists.
        // Nothing in this block sends a message, so the borrow is unique.
        let state = unsafe { &mut *state };
        if message == WM_PAINT {
            // SAFETY: This is the WM_PAINT handler of the overlay window.
            unsafe { state.paint(window) };
            return LRESULT(0);
        }
        let outcome = state.input(message, wparam, lparam);
        if outcome != Outcome::Continue {
            state.outcome = outcome;
        }
        outcome
    };
    // SAFETY: The window is live. Each call accepts the failure cases here.
    unsafe {
        if message == WM_LBUTTONUP {
            let _ = ReleaseCapture();
        }
        if outcome == Outcome::Continue {
            let _ = InvalidateRect(Some(window), None, false);
        } else {
            let _ = DestroyWindow(window);
        }
    }
    LRESULT(0)
}

/// Renders text with the Windows interface font.
struct GdiText;

impl TextRasterizer for GdiText {
    fn rasterize(&self, text: &str, pixel_height: u32) -> Mask {
        // SAFETY: The function creates and releases all GDI objects it uses.
        unsafe { render_text(text, pixel_height) }.unwrap_or_default()
    }
}

/// Draws white text on a black memory bitmap and returns it as coverage.
///
/// # Safety
///
/// The caller must be on a thread that may use GDI.
unsafe fn render_text(text: &str, pixel_height: u32) -> Option<Mask> {
    let mut wide: Vec<u16> = text.encode_utf16().collect();
    if wide.is_empty() || pixel_height == 0 {
        return None;
    }
    let format = DT_SINGLELINE | DT_NOPREFIX;
    unsafe {
        let context = CreateCompatibleDC(None);
        if context.is_invalid() {
            return None;
        }
        let font = CreateFontW(
            pixel_height as i32,
            0,
            0,
            0,
            600,
            0,
            0,
            0,
            DEFAULT_CHARSET,
            OUT_DEFAULT_PRECIS,
            CLIP_DEFAULT_PRECIS,
            ANTIALIASED_QUALITY,
            0,
            w!("Segoe UI"),
        );
        let previous_font = SelectObject(context, font.into());
        let mut bounds = RECT::default();
        DrawTextW(context, &mut wide, &mut bounds, format | DT_CALCRECT);
        let (width, height) = (bounds.right.clamp(0, 16_384), bounds.bottom.clamp(0, 4_096));
        let mut mask = None;
        if width > 0 && height > 0 {
            let info = bitmap_info(width, -height);
            let mut bits = ptr::null_mut();
            if let Ok(bitmap) =
                CreateDIBSection(Some(context), &info, DIB_RGB_COLORS, &mut bits, None, 0)
                && !bits.is_null()
            {
                let previous_bitmap = SelectObject(context, bitmap.into());
                SetTextColor(context, COLORREF(0x00ff_ffff));
                SetBkMode(context, TRANSPARENT);
                DrawTextW(context, &mut wide, &mut bounds, format);
                let _ = GdiFlush();
                let pixels = slice::from_raw_parts(bits.cast::<u32>(), (width * height) as usize);
                mask = Some(Mask {
                    width: width as u32,
                    height: height as u32,
                    coverage: pixels.iter().map(|pixel| (pixel >> 8) as u8).collect(),
                });
                SelectObject(context, previous_bitmap);
                let _ = DeleteObject(bitmap.into());
            }
        }
        SelectObject(context, previous_font);
        let _ = DeleteObject(font.into());
        let _ = DeleteDC(context);
        mask
    }
}

/// Places text on the clipboard.
pub(crate) fn copy_text_to_clipboard(text: &str) -> Result<()> {
    /// Standard clipboard format number of UTF-16 text.
    const CF_UNICODETEXT: u32 = 13;
    let wide: Vec<u16> = text.encode_utf16().chain([0]).collect();
    // SAFETY: The block holds every UTF-16 unit including the terminator.
    // The clipboard owns the block only after SetClipboardData succeeds, and
    // the block is freed here in every other case.
    unsafe {
        let block = GlobalAlloc(GMEM_MOVEABLE, wide.len() * 2)
            .context("could not allocate clipboard data")?;
        let target = GlobalLock(block).cast::<u16>();
        if target.is_null() {
            let _ = GlobalFree(Some(block));
            bail!("could not lock clipboard data");
        }
        ptr::copy_nonoverlapping(wide.as_ptr(), target, wide.len());
        let _ = GlobalUnlock(block);
        if let Err(error) = OpenClipboard(None) {
            let _ = GlobalFree(Some(block));
            return Err(anyhow!(error).context("could not open the clipboard"));
        }
        let _ = EmptyClipboard();
        let stored = SetClipboardData(CF_UNICODETEXT, Some(HANDLE(block.0)));
        let _ = CloseClipboard();
        if let Err(error) = stored {
            let _ = GlobalFree(Some(block));
            return Err(anyhow!(error).context("could not store the link on the clipboard"));
        }
    }
    Ok(())
}

/// Places an image on the clipboard as a device-independent bitmap.
pub(crate) fn copy_to_clipboard(image: &Canvas) -> Result<()> {
    let (width, height) = (image.width() as usize, image.height() as usize);
    let header = bitmap_info(width as i32, height as i32).bmiHeader;
    let header_size = mem::size_of::<BITMAPINFOHEADER>();
    let size = header_size + width * height * 4;
    // SAFETY: The block is `size` bytes. The header and every pixel row stay
    // inside it. The clipboard owns the block only after SetClipboardData
    // succeeds, and the block is freed here in every other case.
    unsafe {
        let block =
            GlobalAlloc(GMEM_MOVEABLE, size).context("could not allocate clipboard data")?;
        let target = GlobalLock(block).cast::<u8>();
        if target.is_null() {
            let _ = GlobalFree(Some(block));
            bail!("could not lock clipboard data");
        }
        ptr::copy_nonoverlapping(ptr::from_ref(&header).cast::<u8>(), target, header_size);
        let rows = target.add(header_size).cast::<u32>();
        // A positive bitmap height stores the bottom row first.
        for (index, row) in image.pixels().chunks_exact(width.max(1)).rev().enumerate() {
            for (column, pixel) in row.iter().enumerate() {
                rows.add(index * width + column)
                    .write_unaligned(pixel | 0xff00_0000);
            }
        }
        let _ = GlobalUnlock(block);
        if let Err(error) = OpenClipboard(None) {
            let _ = GlobalFree(Some(block));
            return Err(anyhow!(error).context("could not open the clipboard"));
        }
        let _ = EmptyClipboard();
        let stored = SetClipboardData(CF_DIB, Some(HANDLE(block.0)));
        let _ = CloseClipboard();
        if let Err(error) = stored {
            let _ = GlobalFree(Some(block));
            return Err(anyhow!(error).context("could not store the image on the clipboard"));
        }
    }
    Ok(())
}
