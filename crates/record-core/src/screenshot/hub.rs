//! The resident capture thread: global hotkeys, the tray icon, and auto capture.
//!
//! One thread owns a Windows message queue. Hotkeys, tray menu commands,
//! timer ticks, and requests from the terminal hub all arrive in that queue,
//! and the thread runs one capture at a time. The region overlay opens its
//! window on the same thread. Requests that arrive during a capture are
//! dropped, so a held key cannot stack captures.
//!
//! The hotkeys come from the settings. A request registers them again after
//! a change, and each registration reports the state of every hotkey.

use std::{
    mem, ptr,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
};

use anyhow::{Context, Result};
use crossbeam_channel::{Receiver, Sender, bounded};
use windows::{
    Win32::{
        Foundation::{ERROR_ALREADY_EXISTS, GetLastError, HWND, LPARAM, LRESULT, POINT, WPARAM},
        Graphics::Gdi::{CreateBitmap, DeleteObject},
        System::{
            LibraryLoader::GetModuleHandleW,
            Threading::{CreateMutexW, GetCurrentThreadId},
        },
        UI::{
            Input::KeyboardAndMouse::{
                HOT_KEY_MODIFIERS, MOD_NOREPEAT, RegisterHotKey, UnregisterHotKey,
            },
            Shell::{
                NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NOTIFYICONDATAW,
                Shell_NotifyIconW,
            },
            WindowsAndMessaging::{
                AppendMenuW, CreateIconIndirect, CreatePopupMenu, CreateWindowExW, DefWindowProcW,
                DestroyIcon, DestroyMenu, DestroyWindow, DispatchMessageW, GetCursorPos,
                GetMessageW, HICON, ICONINFO, KillTimer, MF_SEPARATOR, MF_STRING, MSG, PM_NOREMOVE,
                PM_REMOVE, PeekMessageW, PostThreadMessageW, RegisterClassW, SetForegroundWindow,
                SetTimer, TPM_NONOTIFY, TPM_RETURNCMD, TPM_RIGHTBUTTON, TrackPopupMenu,
                TranslateMessage, UnregisterClassW, WM_APP, WM_HOTKEY, WM_LBUTTONUP, WM_RBUTTONUP,
                WM_TIMER, WM_USER, WNDCLASSW, WS_POPUP,
            },
        },
    },
    core::{PCWSTR, w},
};

use super::{
    CaptureMode, ScreenshotConfig,
    canvas::{Area, Canvas, rgb},
    events::{CaptureHub, HubEvent, HubRequest, HubSettings},
    hotkeys::{HotkeyId, hotkey_states},
    last_region, overlay, take_screenshot, unique_output_path,
};
use crate::video::video_timestamp;

/// Thread message that carries one request code in `wParam`.
const WM_HUB_REQUEST: u32 = WM_APP + 1;
/// Thread message that ends the message loop.
const WM_HUB_QUIT: u32 = WM_APP + 2;
/// Window message that the tray icon sends for mouse input.
const WM_HUB_TRAY: u32 = WM_APP + 3;
/// Window class name of the hidden tray owner window.
const TRAY_CLASS: PCWSTR = w!("RecordScreenshotTray");
/// Name of the mutex that marks a running tray instance.
const TRAY_MUTEX: PCWSTR = w!("Local\\RecordScreenshotTray");

/// Request code bit that adds the three-second delay.
const CODE_DELAYED: usize = 0x10;
/// Request code that starts or stops auto capture.
const CODE_AUTO: usize = 0x20;
/// Request code that the tray menu sends to stop the hub.
const CODE_QUIT: usize = 0x40;
/// Request code that registers the hotkeys again from the settings.
const CODE_HOTKEYS: usize = 0x80;

/// Returns the registration number of one hotkey.
const fn hotkey_number(id: HotkeyId) -> i32 {
    id.index() as i32 + 1
}

/// Finds the hotkey of a registration number from a `WM_HOTKEY` message.
fn numbered_hotkey(number: usize) -> Option<HotkeyId> {
    HotkeyId::ALL.get(number.wrapping_sub(1)).copied()
}

/// Returns the screenshot that a hotkey takes. A video hotkey takes none.
const fn hotkey_mode(id: HotkeyId) -> Option<CaptureMode> {
    match id {
        HotkeyId::Region => Some(CaptureMode::Region),
        HotkeyId::Fullscreen => Some(CaptureMode::Fullscreen),
        HotkeyId::Window => Some(CaptureMode::Window),
        HotkeyId::VideoRegion | HotkeyId::VideoFullscreen => None,
    }
}

/// Registers every wanted hotkey of the settings on the calling thread.
///
/// Hotkeys that `registered` marks are released first. The video hotkeys
/// stay free unless `video` is set. Returns the result for each hotkey.
///
/// # Safety
///
/// The calling thread must own the hotkeys that `registered` marks.
unsafe fn register_hotkeys(
    settings: &HubSettings,
    video: bool,
    registered: [bool; 5],
) -> [bool; 5] {
    let bindings = settings.hotkeys();
    for id in HotkeyId::ALL {
        if registered[id.index()] {
            // SAFETY: This thread registered the hotkey with this number.
            let _ = unsafe { UnregisterHotKey(None, hotkey_number(id)) };
        }
    }
    HotkeyId::ALL.map(|id| {
        let binding = bindings[id.index()];
        if !binding.enabled || (id.is_video() && !video) {
            return false;
        }
        let modifiers = HOT_KEY_MODIFIERS(binding.chord.modifiers()) | MOD_NOREPEAT;
        // SAFETY: The call has no pointer arguments. A chord that another
        // program holds only makes it fail.
        unsafe {
            RegisterHotKey(
                None,
                hotkey_number(id),
                modifiers,
                u32::from(binding.chord.key),
            )
        }
        .is_ok()
    })
}

/// Converts a capture mode to its request code.
const fn mode_code(mode: CaptureMode) -> usize {
    match mode {
        CaptureMode::Region => 1,
        CaptureMode::Monitor => 2,
        CaptureMode::Fullscreen => 3,
        CaptureMode::Window => 4,
        CaptureMode::LastRegion | CaptureMode::Custom(_) => 5,
    }
}

/// Converts a request code back to its capture mode.
const fn code_mode(code: usize) -> Option<CaptureMode> {
    match code & 0x0f {
        1 => Some(CaptureMode::Region),
        2 => Some(CaptureMode::Monitor),
        3 => Some(CaptureMode::Fullscreen),
        4 => Some(CaptureMode::Window),
        5 => Some(CaptureMode::LastRegion),
        _ => None,
    }
}

/// Owns the capture thread of a terminal hub.
pub struct Hub {
    thread_id: u32,
    worker: Option<JoinHandle<()>>,
    events: Receiver<HubEvent>,
    busy: Arc<AtomicBool>,
}

impl Hub {
    /// Starts the capture thread and registers the screenshot hotkeys.
    pub fn start(settings: Arc<HubSettings>) -> Result<Self> {
        Self::start_thread(settings, false)
    }

    /// Starts the capture thread and registers every hotkey.
    ///
    /// The thread reports a video hotkey as [`HubEvent::VideoHotkey`] and
    /// leaves the recording to the caller.
    pub fn start_with_video(settings: Arc<HubSettings>) -> Result<Self> {
        Self::start_thread(settings, true)
    }

    /// Starts the capture thread. `video` also registers the video hotkeys.
    fn start_thread(settings: Arc<HubSettings>, video: bool) -> Result<Self> {
        let (event_sender, events) = bounded(64);
        let (id_sender, id_receiver) = bounded(1);
        let busy = Arc::new(AtomicBool::new(false));
        let thread_busy = Arc::clone(&busy);
        let worker = thread::Builder::new()
            .name("record-screenshot".to_owned())
            .spawn(move || {
                message_loop(
                    &settings,
                    &event_sender,
                    &thread_busy,
                    false,
                    video,
                    Some(&id_sender),
                );
            })
            .context("could not start the screenshot thread")?;
        let thread_id = id_receiver
            .recv()
            .context("the screenshot thread stopped during startup")?;
        Ok(Self {
            thread_id,
            worker: Some(worker),
            events,
            busy,
        })
    }

    /// Posts one thread message to the capture thread.
    fn post(&self, message: u32, code: usize) {
        // SAFETY: Posting to a thread id has no memory preconditions. A stale
        // id only makes the call fail.
        let _ = unsafe { PostThreadMessageW(self.thread_id, message, WPARAM(code), LPARAM(0)) };
    }
}

impl CaptureHub for Hub {
    fn events(&self) -> &Receiver<HubEvent> {
        &self.events
    }

    fn request(&self, request: HubRequest) {
        let code = match request {
            HubRequest::Capture(mode) => mode_code(mode),
            HubRequest::Delayed(mode) => mode_code(mode) | CODE_DELAYED,
            HubRequest::ToggleAuto => CODE_AUTO,
            HubRequest::ReloadHotkeys => CODE_HOTKEYS,
        };
        self.post(WM_HUB_REQUEST, code);
    }
}

impl Drop for Hub {
    fn drop(&mut self) {
        self.post(WM_HUB_QUIT, 0);
        // An open overlay keeps its thread until the user closes it. The
        // process exit ends that thread, so do not wait for it here.
        if !self.busy.load(Ordering::Relaxed)
            && let Some(worker) = self.worker.take()
        {
            let _ = worker.join();
        }
    }
}

/// Reports whether a tray instance already runs in this session.
///
/// The first caller keeps the marker until its process exits.
#[must_use]
pub fn tray_is_running() -> bool {
    // SAFETY: The name is a static string. The handle stays open on purpose,
    // because the open handle is the marker.
    unsafe {
        let mutex = CreateMutexW(None, false, TRAY_MUTEX);
        let exists = GetLastError() == ERROR_ALREADY_EXISTS;
        mem::forget(mutex);
        exists
    }
}

/// Runs the hub on the calling thread with a tray icon and no terminal.
///
/// Returns when the user selects Quit in the tray menu.
pub fn run_tray(settings: &HubSettings) {
    let (events, _unread) = bounded(1);
    message_loop(
        settings,
        &events,
        &AtomicBool::new(false),
        true,
        false,
        None,
    );
}

/// Registers hotkeys, optionally shows the tray icon, and serves requests.
fn message_loop(
    settings: &HubSettings,
    events: &Sender<HubEvent>,
    busy: &AtomicBool,
    tray: bool,
    video: bool,
    ready: Option<&Sender<u32>>,
) {
    overlay::enable_dpi_awareness();
    let report = |event: HubEvent| {
        let _ = events.try_send(event);
    };
    let mut message = MSG::default();
    // SAFETY: Every call below runs on the thread that owns the queue, the
    // hotkeys, the timer, and the tray window. Pointers refer to live locals.
    unsafe {
        // The first queue call creates the message queue of this thread.
        let _ = PeekMessageW(&mut message, None, WM_USER, WM_USER, PM_NOREMOVE);
        if let Some(ready) = ready {
            let _ = ready.send(GetCurrentThreadId());
        }
        let mut hotkeys = register_hotkeys(settings, video, [false; 5]);
        let tray_icon = if tray { TrayIcon::show() } else { None };
        report(HubEvent::Ready([hotkeys[0], hotkeys[1], hotkeys[2]]));
        report(HubEvent::Hotkeys(hotkey_states(
            &settings.hotkeys(),
            hotkeys,
        )));
        let mut auto_timer = 0_usize;
        while GetMessageW(&mut message, None, 0, 0).0 > 0 {
            if !message.hwnd.is_invalid() {
                let _ = TranslateMessage(&message);
                DispatchMessageW(&message);
                continue;
            }
            let code = message.wParam.0;
            let (mode, delayed) = match message.message {
                WM_HUB_QUIT => break,
                WM_HOTKEY => match numbered_hotkey(code) {
                    Some(id) if id.is_video() => {
                        report(HubEvent::VideoHotkey(id));
                        (None, false)
                    }
                    Some(id) => (hotkey_mode(id), false),
                    None => (None, false),
                },
                WM_TIMER if auto_timer != 0 && code == auto_timer => {
                    let mode = if last_region().is_some() {
                        CaptureMode::LastRegion
                    } else {
                        CaptureMode::Monitor
                    };
                    (Some(mode), false)
                }
                WM_HUB_REQUEST if code == CODE_QUIT => {
                    report(HubEvent::QuitRequested);
                    break;
                }
                WM_HUB_REQUEST if code == CODE_HOTKEYS => {
                    hotkeys = register_hotkeys(settings, video, hotkeys);
                    report(HubEvent::Hotkeys(hotkey_states(
                        &settings.hotkeys(),
                        hotkeys,
                    )));
                    (None, false)
                }
                WM_HUB_REQUEST if code == CODE_AUTO => {
                    if auto_timer == 0 {
                        let interval = settings
                            .options()
                            .auto_interval
                            .as_millis()
                            .clamp(250, 3_600_000);
                        auto_timer = SetTimer(None, 0, interval as u32, None);
                    } else {
                        let _ = KillTimer(None, auto_timer);
                        auto_timer = 0;
                    }
                    report(HubEvent::Auto(auto_timer != 0));
                    (None, false)
                }
                WM_HUB_REQUEST => (code_mode(code), code & CODE_DELAYED != 0),
                _ => (None, false),
            };
            let Some(mode) = mode else {
                continue;
            };
            busy.store(true, Ordering::Relaxed);
            report(HubEvent::Capturing);
            let options = settings.options();
            let config = ScreenshotConfig {
                mode,
                output: unique_output_path(
                    &options.directory,
                    &video_timestamp(),
                    options.format.extension(),
                ),
                monitor: options.monitor,
                save: settings.save.load(Ordering::Relaxed),
                copy: settings.copy.load(Ordering::Relaxed),
                delay: delayed.then_some(options.delay),
                sdr_white_nits: options.sdr_white_nits,
                upload: settings
                    .active_uploader()
                    .filter(|_| settings.upload.load(Ordering::Relaxed)),
            };
            report(match take_screenshot(&config) {
                Ok(Some(summary)) => HubEvent::Finished(summary),
                Ok(None) => HubEvent::Cancelled,
                Err(error) => HubEvent::Failed(format!("{error:#}")),
            });
            busy.store(false, Ordering::Relaxed);
            // Drop the input that arrived during the capture. A change of
            // the hotkeys is not input, so it still applies.
            let mut reload = false;
            for kind in [WM_HOTKEY, WM_TIMER, WM_HUB_REQUEST] {
                while PeekMessageW(
                    &mut message,
                    Some(HWND(-1_isize as _)),
                    kind,
                    kind,
                    PM_REMOVE,
                )
                .as_bool()
                {
                    reload |= kind == WM_HUB_REQUEST && message.wParam.0 == CODE_HOTKEYS;
                }
            }
            if reload {
                let _ = PostThreadMessageW(
                    GetCurrentThreadId(),
                    WM_HUB_REQUEST,
                    WPARAM(CODE_HOTKEYS),
                    LPARAM(0),
                );
            }
        }
        if auto_timer != 0 {
            let _ = KillTimer(None, auto_timer);
        }
        for id in HotkeyId::ALL {
            if hotkeys[id.index()] {
                let _ = UnregisterHotKey(None, hotkey_number(id));
            }
        }
        drop(tray_icon);
    }
}

/// Owns the tray icon and its hidden owner window.
struct TrayIcon {
    window: HWND,
    icon: HICON,
}

impl TrayIcon {
    /// Creates the hidden window and adds the icon to the tray.
    ///
    /// # Safety
    ///
    /// The calling thread must run a message loop while the value lives.
    unsafe fn show() -> Option<Self> {
        unsafe {
            let instance = GetModuleHandleW(None).ok()?;
            let class = WNDCLASSW {
                lpfnWndProc: Some(tray_procedure),
                hInstance: instance.into(),
                lpszClassName: TRAY_CLASS,
                ..Default::default()
            };
            RegisterClassW(&class);
            let window = CreateWindowExW(
                Default::default(),
                TRAY_CLASS,
                w!("record screenshot"),
                WS_POPUP,
                0,
                0,
                0,
                0,
                None,
                None,
                Some(instance.into()),
                None,
            )
            .ok()?;
            let icon = create_icon().unwrap_or_default();
            let tray = Self { window, icon };
            let mut data = tray.data();
            data.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP;
            data.uCallbackMessage = WM_HUB_TRAY;
            data.hIcon = icon;
            let tip: Vec<u16> = "record screenshot".encode_utf16().collect();
            data.szTip[..tip.len()].copy_from_slice(&tip);
            let _ = Shell_NotifyIconW(NIM_ADD, &data);
            Some(tray)
        }
    }

    /// Returns the structure that identifies this icon to the shell.
    fn data(&self) -> NOTIFYICONDATAW {
        NOTIFYICONDATAW {
            cbSize: mem::size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: self.window,
            uID: 1,
            ..Default::default()
        }
    }
}

impl Drop for TrayIcon {
    fn drop(&mut self) {
        // SAFETY: The window and icon belong to this thread and are still live.
        unsafe {
            let _ = Shell_NotifyIconW(NIM_DELETE, &self.data());
            let _ = DestroyWindow(self.window);
            if !self.icon.is_invalid() {
                let _ = DestroyIcon(self.icon);
            }
            if let Ok(instance) = GetModuleHandleW(None) {
                let _ = UnregisterClassW(TRAY_CLASS, Some(instance.into()));
            }
        }
    }
}

/// Draws the 32-pixel tray icon: a pulse of sound on the dot grid, cyan for
/// the left channel, green for the right, and the red record line between.
#[must_use]
pub(crate) fn icon_image() -> Canvas {
    /// Distance between two dots in pixels.
    const PITCH: i32 = 6;
    /// Edge of one dot in pixels.
    const DOT: i32 = 4;
    /// Number of dots above and below the middle row in each column.
    const REACH: [i32; 5] = [0, 1, 2, 1, 0];
    let mut icon = Canvas::new(32, 32, rgb(0x0c, 0x0c, 0x0c));
    for (column, reach) in (0_i32..).zip(REACH) {
        let color = match column {
            0 | 1 => rgb(0x41, 0xd6, 0xe0),
            2 => rgb(0xf7, 0x4b, 0x67),
            _ => rgb(0x48, 0xd6, 0x93),
        };
        for step in -reach..=reach {
            let (left, top) = (2 + column * PITCH, 2 + (2 + step) * PITCH);
            icon.fill_rect(Area::new(left, top, left + DOT, top + DOT), color, 255);
        }
    }
    icon
}

/// Converts the drawn icon to a Windows icon handle.
///
/// # Safety
///
/// The caller must destroy the returned icon with `DestroyIcon`.
unsafe fn create_icon() -> Option<HICON> {
    let image = icon_image();
    let color: Vec<u32> = image
        .pixels()
        .iter()
        .map(|pixel| pixel | 0xff00_0000)
        .collect();
    let mask = [0_u8; 32 * 32 / 8];
    unsafe {
        let color_bitmap = CreateBitmap(32, 32, 1, 32, Some(color.as_ptr().cast()));
        let mask_bitmap = CreateBitmap(32, 32, 1, 1, Some(mask.as_ptr().cast()));
        let info = ICONINFO {
            fIcon: true.into(),
            hbmMask: mask_bitmap,
            hbmColor: color_bitmap,
            ..Default::default()
        };
        let icon = CreateIconIndirect(&info).ok();
        let _ = DeleteObject(color_bitmap.into());
        let _ = DeleteObject(mask_bitmap.into());
        icon
    }
}

/// Posts one request code to the message loop of the current thread.
fn post_request(code: usize) {
    // SAFETY: The tray window procedure runs on the hub thread, so the
    // current thread id is the hub thread id.
    unsafe {
        let _ = PostThreadMessageW(
            GetCurrentThreadId(),
            WM_HUB_REQUEST,
            WPARAM(code),
            LPARAM(0),
        );
    }
}

/// Handles tray icon clicks. A left click captures a region. A right click
/// opens the capture menu.
unsafe extern "system" fn tray_procedure(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if message != WM_HUB_TRAY {
        // SAFETY: Unhandled messages go to the default procedure unchanged.
        return unsafe { DefWindowProcW(window, message, wparam, lparam) };
    }
    match lparam.0 as u32 {
        WM_LBUTTONUP => post_request(mode_code(CaptureMode::Region)),
        WM_RBUTTONUP => {
            let items: [(usize, PCWSTR); 7] = [
                (mode_code(CaptureMode::Region), w!("Region\tCtrl+PrtSc")),
                (mode_code(CaptureMode::Fullscreen), w!("Fullscreen\tPrtSc")),
                (
                    mode_code(CaptureMode::Window),
                    w!("Active window\tAlt+PrtSc"),
                ),
                (mode_code(CaptureMode::Monitor), w!("Active monitor")),
                (mode_code(CaptureMode::LastRegion), w!("Last region")),
                (CODE_AUTO, w!("Auto capture on/off")),
                (CODE_QUIT, w!("Quit")),
            ];
            // SAFETY: The menu lives only inside this block. The window is the
            // live tray owner window of this thread.
            unsafe {
                let Ok(menu) = CreatePopupMenu() else {
                    return LRESULT(0);
                };
                for (code, label) in items {
                    if code == CODE_AUTO {
                        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR(ptr::null()));
                    }
                    let _ = AppendMenuW(menu, MF_STRING, code, label);
                }
                let mut pointer = POINT::default();
                let _ = GetCursorPos(&mut pointer);
                // The menu closes on an outside click only for a foreground window.
                let _ = SetForegroundWindow(window);
                let chosen = TrackPopupMenu(
                    menu,
                    TPM_RETURNCMD | TPM_NONOTIFY | TPM_RIGHTBUTTON,
                    pointer.x,
                    pointer.y,
                    None,
                    window,
                    None,
                );
                let _ = DestroyMenu(menu);
                if chosen.0 > 0 {
                    // The active window is this menu owner, so wait for the
                    // user to return to the window they want.
                    let code = chosen.0 as usize;
                    let delay = if code == mode_code(CaptureMode::Window) {
                        CODE_DELAYED
                    } else {
                        0
                    };
                    post_request(code | delay);
                }
            }
        }
        _ => {}
    }
    LRESULT(0)
}
