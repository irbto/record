//! The window of the plain region selection.
//!
//! The window covers one monitor and shows its frozen image, as the
//! annotation overlay does. The user drags one rectangle. A mouse release or
//! Enter confirms it. Esc or the right mouse button cancels. The state and
//! the drawing live in the `select` module.

use anyhow::{Context, Result, anyhow, bail};
use windows::{
    Win32::{
        Foundation::{HWND, LPARAM, LRESULT, WPARAM},
        Graphics::Gdi::{
            BeginPaint, DIB_RGB_COLORS, EndPaint, InvalidateRect, PAINTSTRUCT, SetDIBitsToDevice,
        },
        System::LibraryLoader::GetModuleHandleW,
        UI::{
            Input::KeyboardAndMouse::{ReleaseCapture, SetCapture, SetFocus, VK_ESCAPE, VK_RETURN},
            WindowsAndMessaging::{
                CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GWLP_USERDATA,
                GetMessageW, GetWindowLongPtrW, IDC_CROSS, LoadCursorW, MSG, PostQuitMessage,
                RegisterClassW, SW_SHOW, SetForegroundWindow, SetWindowLongPtrW, ShowWindow,
                TranslateMessage, UnregisterClassW, WM_DESTROY, WM_ERASEBKGND, WM_KEYDOWN,
                WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEMOVE, WM_PAINT, WM_RBUTTONUP, WNDCLASSW,
                WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
            },
        },
    },
    core::{PCWSTR, w},
};

use super::{
    DesktopRect, MonitorChoice,
    canvas::{Area, Canvas},
    capture,
    overlay::{self, bitmap_info, pointer_position},
    select::{Selector, Step},
};

/// Window class name of the picker.
const CLASS_NAME: PCWSTR = w!("RecordRegionPicker");

/// Everything the window procedure needs between messages.
struct PickerState {
    /// The selection in progress.
    selector: Selector,
    /// The frame that the window shows.
    frame: Canvas,
    /// The confirmed selection.
    chosen: Option<Area>,
}

/// Lets the user drag one rectangle on a frozen image of one monitor.
///
/// Returns the rectangle in desktop pixels, or `None` when the user
/// cancelled. The call owns a window, so it blocks its thread until then.
pub(crate) fn select_region(monitor: MonitorChoice) -> Result<Option<DesktopRect>> {
    overlay::enable_dpi_awareness();
    let captured = capture::capture_monitor(monitor, None)?;
    let (width, height) = (captured.image.width(), captured.image.height());
    let state = Box::into_raw(Box::new(PickerState {
        frame: captured.image.clone(),
        selector: Selector::new(captured.image),
        chosen: None,
    }));
    // SAFETY: `state` stays valid until `Box::from_raw` below. The window
    // procedure is the only other user, and it runs on this thread inside
    // the message loop.
    let shown = unsafe { run_window(state, captured.left, captured.top, width, height) };
    // SAFETY: `run_window` destroyed the window, so nothing else holds the pointer.
    let state = unsafe { Box::from_raw(state) };
    shown?;
    Ok(state.chosen.map(|area| DesktopRect {
        left: captured.left + area.left,
        top: captured.top + area.top,
        width: area.width() as u32,
        height: area.height() as u32,
    }))
}

/// Creates the picker window and runs its message loop.
///
/// # Safety
///
/// `state` must point to a live `PickerState` for the complete call.
unsafe fn run_window(
    state: *mut PickerState,
    left: i32,
    top: i32,
    width: u32,
    height: u32,
) -> Result<()> {
    // SAFETY: The call has no preconditions.
    let instance = unsafe { GetModuleHandleW(None) }.context("could not find the module handle")?;
    let class = WNDCLASSW {
        lpfnWndProc: Some(window_procedure),
        hInstance: instance.into(),
        // SAFETY: The cursor is a shared system resource.
        hCursor: unsafe { LoadCursorW(None, IDC_CROSS) }.unwrap_or_default(),
        lpszClassName: CLASS_NAME,
        ..Default::default()
    };
    // SAFETY: The class structure and its strings live for the call.
    if unsafe { RegisterClassW(&class) } == 0 {
        bail!("could not register the region window class");
    }
    // SAFETY: The class is registered and every argument lives for the call.
    let window = unsafe {
        CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW,
            CLASS_NAME,
            w!("record region"),
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
        // SAFETY: The window is live and belongs to this thread. The caller
        // keeps `state` alive until the loop ends.
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
        Err(error) => Err(anyhow!(error).context("could not create the region window")),
    };
    // SAFETY: No window of this class exists now.
    let _ = unsafe { UnregisterClassW(CLASS_NAME, Some(instance.into())) };
    result
}

impl PickerState {
    /// Draws the current frame into the window.
    ///
    /// # Safety
    ///
    /// `window` must be the live picker window and the call must come from
    /// its `WM_PAINT` handler.
    unsafe fn paint(&mut self, window: HWND) {
        self.selector.compose(&mut self.frame);
        let (width, height) = (self.frame.width(), self.frame.height());
        let info = bitmap_info(width as i32, -(height as i32));
        let mut paint = PAINTSTRUCT::default();
        // SAFETY: The frame holds `width * height` pixels in the layout that
        // `info` describes. The window is live.
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

    /// Applies one input message to the selection.
    fn input(&mut self, message: u32, wparam: WPARAM, lparam: LPARAM) -> Step {
        match message {
            WM_LBUTTONDOWN => self.selector.pointer_down(pointer_position(lparam)),
            WM_MOUSEMOVE => self.selector.pointer_move(pointer_position(lparam)),
            WM_LBUTTONUP => return self.selector.pointer_up(pointer_position(lparam)),
            WM_RBUTTONUP => return Step::Cancel,
            WM_KEYDOWN if wparam.0 as u16 == VK_RETURN.0 => return self.selector.confirm(),
            WM_KEYDOWN if wparam.0 as u16 == VK_ESCAPE.0 => return Step::Cancel,
            _ => {}
        }
        Step::Continue
    }
}

/// Handles the picker messages on the thread that created the window.
unsafe extern "system" fn window_procedure(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match message {
        WM_DESTROY => {
            // SAFETY: The call only posts a message to this thread.
            unsafe { PostQuitMessage(0) };
            return LRESULT(0);
        }
        // The paint handler covers every pixel.
        WM_ERASEBKGND => return LRESULT(1),
        WM_PAINT | WM_LBUTTONDOWN | WM_LBUTTONUP | WM_RBUTTONUP | WM_MOUSEMOVE | WM_KEYDOWN => {}
        // SAFETY: Unhandled messages go to the default procedure unchanged.
        _ => return unsafe { DefWindowProcW(window, message, wparam, lparam) },
    }
    // SAFETY: The slot holds null or the pointer that `run_window` stored.
    let state = unsafe { GetWindowLongPtrW(window, GWLP_USERDATA) } as *mut PickerState;
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
    let step = {
        // SAFETY: `run_window` keeps the state alive while the window exists.
        // Nothing in this block sends a message, so the borrow is unique.
        let state = unsafe { &mut *state };
        if message == WM_PAINT {
            // SAFETY: This is the WM_PAINT handler of the picker window.
            unsafe { state.paint(window) };
            return LRESULT(0);
        }
        let step = state.input(message, wparam, lparam);
        if let Step::Confirm(area) = step {
            state.chosen = Some(area);
        }
        step
    };
    // SAFETY: The window is live. Each call accepts the failure cases here.
    unsafe {
        if message == WM_LBUTTONUP {
            let _ = ReleaseCapture();
        }
        if step == Step::Continue {
            let _ = InvalidateRect(Some(window), None, false);
        } else {
            let _ = DestroyWindow(window);
        }
    }
    LRESULT(0)
}
