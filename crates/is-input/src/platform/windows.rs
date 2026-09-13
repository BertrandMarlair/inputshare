//! Windows implementation: monitor enumeration, `SendInput`, low-level hooks.

use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU64, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use is_core::{DisplayInfo, InputEvent, MouseButton};
use tracing::{info, warn};
use windows_sys::Win32::Foundation::{BOOL, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{
    EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFO, MONITORINFOEXW,
};
use windows_sys::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE, KEYBDINPUT, KEYBD_EVENT_FLAGS,
    KEYEVENTF_EXTENDEDKEY, KEYEVENTF_KEYUP, KEYEVENTF_SCANCODE, MOUSEEVENTF_HWHEEL,
    MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP, MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP,
    MOUSEEVENTF_MOVE, MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP, MOUSEEVENTF_WHEEL,
    MOUSEEVENTF_XDOWN, MOUSEEVENTF_XUP, MOUSEINPUT, VK_CONTROL, VK_F12, VK_MENU,
};
use windows_sys::Win32::UI::Input::{
    GetRawInputData, RegisterRawInputDevices, HRAWINPUT, RAWINPUT, RAWINPUTDEVICE, RAWINPUTHEADER,
    RIDEV_INPUTSINK, RID_INPUT,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetMessageW,
    PostThreadMessageW, RegisterClassW, SetWindowsHookExW, TranslateMessage, UnhookWindowsHookEx,
    KBDLLHOOKSTRUCT, MSG, MSLLHOOKSTRUCT, WH_KEYBOARD_LL, WH_MOUSE_LL, WM_INPUT, WM_LBUTTONDOWN,
    WM_LBUTTONUP, WM_MBUTTONDOWN, WM_MBUTTONUP, WM_MOUSEHWHEEL, WM_MOUSEMOVE, WM_MOUSEWHEEL,
    WM_QUIT, WM_RBUTTONDOWN, WM_RBUTTONUP, WM_SYSKEYDOWN, WM_SYSKEYUP, WM_XBUTTONDOWN,
    WM_XBUTTONUP, WNDCLASSW, XBUTTON1,
};

use crate::{Error, Result};

/// `MONITORINFOF_PRIMARY`. Not re-exported by this version of `windows-sys`, and
/// it is a documented constant rather than something to look up at runtime.
const MONITORINFOF_PRIMARY: u32 = 1;

/// A window that exists only to receive messages — never shown, never painted.
const HWND_MESSAGE_ONLY: HWND = -3isize as HWND;

/// HID usage page and usage for a generic mouse.
const HID_USAGE_PAGE_GENERIC: u16 = 0x01;
const HID_USAGE_GENERIC_MOUSE: u16 = 0x02;

/// The raw report carried absolute coordinates rather than a movement.
const MOUSE_MOVE_ABSOLUTE: u16 = 0x01;
const RIM_TYPEMOUSE: u32 = 0;

// ----------------------------------------------------------------- displays

struct Collected {
    displays: Vec<DisplayInfo>,
}

/// Reads the real monitor layout, in the same coordinate space Windows uses for
/// the virtual desktop: the primary monitor's top-left is the origin and other
/// monitors may sit at negative coordinates.
pub fn enumerate_displays() -> Result<Vec<DisplayInfo>> {
    let mut collected = Collected {
        displays: Vec::new(),
    };
    let ok = unsafe {
        EnumDisplayMonitors(
            std::ptr::null_mut(),
            std::ptr::null(),
            Some(monitor_callback),
            &mut collected as *mut Collected as LPARAM,
        )
    };
    if ok == 0 {
        return Err(Error::Os("EnumDisplayMonitors failed".into()));
    }
    // Stable order, so an unchanged layout produces an unchanged record and does
    // not churn the workspace revision on every startup.
    collected.displays.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(collected.displays)
}

unsafe extern "system" fn monitor_callback(
    monitor: HMONITOR,
    _hdc: HDC,
    _clip: *mut RECT,
    data: LPARAM,
) -> BOOL {
    let collected = &mut *(data as *mut Collected);

    let mut info: MONITORINFOEXW = std::mem::zeroed();
    info.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;
    if GetMonitorInfoW(
        monitor,
        &mut info as *mut MONITORINFOEXW as *mut MONITORINFO,
    ) == 0
    {
        return 1; // skip this one, keep enumerating
    }

    let rect = info.monitorInfo.rcMonitor;
    let name = String::from_utf16_lossy(&info.szDevice)
        .trim_end_matches('\0')
        .to_string();

    // Effective DPI, which is what the user actually sees; 96 is 100%.
    let mut dpi_x = 96u32;
    let mut dpi_y = 96u32;
    let _ = GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y);

    collected.displays.push(DisplayInfo {
        id: name.clone(),
        name,
        x: rect.left,
        y: rect.top,
        width: (rect.right - rect.left).max(0) as u32,
        height: (rect.bottom - rect.top).max(0) as u32,
        scale: dpi_x as f32 / 96.0,
        primary: info.monitorInfo.dwFlags & MONITORINFOF_PRIMARY != 0,
    });
    1
}

// ---------------------------------------------------------------- injection

pub fn inject(event: InputEvent) -> Result<()> {
    // Placing the cursor is not an injected event; it is a direct move, and it
    // has to happen before the motion that follows it.
    if let InputEvent::CursorEnter { x, y } = event {
        return set_cursor_position(x, y);
    }

    let input = match event {
        InputEvent::CursorEnter { .. } => unreachable!("handled above"),
        InputEvent::MouseMove { dx, dy } => mouse_input(dx, dy, 0, MOUSEEVENTF_MOVE),
        InputEvent::MouseButton { button, down } => {
            let (flags, data) = match (button, down) {
                (MouseButton::Left, true) => (MOUSEEVENTF_LEFTDOWN, 0),
                (MouseButton::Left, false) => (MOUSEEVENTF_LEFTUP, 0),
                (MouseButton::Right, true) => (MOUSEEVENTF_RIGHTDOWN, 0),
                (MouseButton::Right, false) => (MOUSEEVENTF_RIGHTUP, 0),
                (MouseButton::Middle, true) => (MOUSEEVENTF_MIDDLEDOWN, 0),
                (MouseButton::Middle, false) => (MOUSEEVENTF_MIDDLEUP, 0),
                (MouseButton::X1, true) => (MOUSEEVENTF_XDOWN, 1),
                (MouseButton::X1, false) => (MOUSEEVENTF_XUP, 1),
                (MouseButton::X2, true) => (MOUSEEVENTF_XDOWN, 2),
                (MouseButton::X2, false) => (MOUSEEVENTF_XUP, 2),
            };
            mouse_input(0, 0, data, flags)
        }
        InputEvent::Wheel { delta, horizontal } => mouse_input(
            0,
            0,
            delta,
            if horizontal {
                MOUSEEVENTF_HWHEEL
            } else {
                MOUSEEVENTF_WHEEL
            },
        ),
        InputEvent::Key {
            vk,
            scan,
            down,
            extended,
        } => {
            // Scan codes rather than virtual keys: the two machines may have
            // different keyboard layouts, and the user pressed a physical key.
            let mut flags: KEYBD_EVENT_FLAGS = KEYEVENTF_SCANCODE;
            if !down {
                flags |= KEYEVENTF_KEYUP;
            }
            if extended {
                flags |= KEYEVENTF_EXTENDEDKEY;
            }
            INPUT {
                r#type: INPUT_KEYBOARD,
                Anonymous: INPUT_0 {
                    ki: KEYBDINPUT {
                        wVk: vk,
                        wScan: scan,
                        dwFlags: flags,
                        time: 0,
                        dwExtraInfo: INJECTED_MARKER,
                    },
                },
            }
        }
    };

    let sent = unsafe { SendInput(1, &input, std::mem::size_of::<INPUT>() as i32) };
    if sent == 1 {
        Ok(())
    } else {
        Err(Error::Os("SendInput was blocked".into()))
    }
}

fn mouse_input(dx: i32, dy: i32, data: i32, flags: u32) -> INPUT {
    INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dx,
                dy,
                mouseData: data as u32,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: INJECTED_MARKER,
            },
        },
    }
}

/// Stamped on everything this process injects, so the capture hook can tell its
/// own replay apart from a real key press. Without it, injecting on a machine
/// that is also capturing would feed every event straight back into the loop.
const INJECTED_MARKER: usize = 0x1_4E_50_55; // "INPU"

// ------------------------------------------------------------------ capture

/// Whether captured input is swallowed instead of also acting locally.
static SUPPRESSING: AtomicBool = AtomicBool::new(false);
/// Last time the owner said it was still alive, in milliseconds.
static WATCHDOG: AtomicU64 = AtomicU64::new(0);
/// Accumulated cursor position, so motion can be sent as deltas.
static LAST_X: AtomicI32 = AtomicI32::new(0);
static LAST_Y: AtomicI32 = AtomicI32::new(0);
static HAVE_LAST: AtomicBool = AtomicBool::new(false);

/// A hook that has stopped hearing from the rest of the program stops
/// suppressing. If the agent deadlocks while the cursor is on another machine,
/// this is what gives the keyboard back without a reboot.
const WATCHDOG_LIMIT_MS: u64 = 2_000;

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn suppressing_now() -> bool {
    if !SUPPRESSING.load(Ordering::Relaxed) {
        return false;
    }
    let last = WATCHDOG.load(Ordering::Relaxed);
    if now_ms().saturating_sub(last) > WATCHDOG_LIMIT_MS {
        // Fail open, always. A missed keystroke is an annoyance; a machine that
        // ignores its own keyboard is an emergency.
        SUPPRESSING.store(false, Ordering::Relaxed);
        warn!("input capture: owner went quiet, releasing the keyboard and mouse");
        return false;
    }
    true
}

static SINK: OnceLock<Mutex<Option<Sender<InputEvent>>>> = OnceLock::new();

fn sink() -> &'static Mutex<Option<Sender<InputEvent>>> {
    SINK.get_or_init(|| Mutex::new(None))
}

fn emit(event: InputEvent) {
    if let Ok(guard) = sink().lock() {
        if let Some(sender) = guard.as_ref() {
            let _ = sender.send(event);
        }
    }
}

/// A running capture. Dropping it removes the hooks.
pub struct Capture {
    thread_id: u32,
    joiner: Option<std::thread::JoinHandle<()>>,
}

static RUNNING: AtomicBool = AtomicBool::new(false);

impl Capture {
    /// Installs the hooks on a dedicated thread.
    ///
    /// Capture starts in observe-only mode: events are reported, and they still
    /// reach this machine as normal. Nothing is swallowed until
    /// [`Capture::set_suppressing`] is called, which is a separate, deliberate
    /// act.
    pub fn start(sender: Sender<InputEvent>) -> Result<Self> {
        if RUNNING.swap(true, Ordering::SeqCst) {
            return Err(Error::AlreadyRunning);
        }
        *sink().lock().expect("sink") = Some(sender);
        SUPPRESSING.store(false, Ordering::SeqCst);
        WATCHDOG.store(now_ms(), Ordering::SeqCst);
        HAVE_LAST.store(false, Ordering::SeqCst);

        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let joiner = std::thread::Builder::new()
            .name("inputshare-hooks".into())
            .spawn(move || hook_thread(ready_tx))
            .map_err(|error| Error::Os(error.to_string()))?;

        match ready_rx.recv() {
            Ok(Ok(thread_id)) => {
                info!("input capture running in observe-only mode");
                Ok(Self {
                    thread_id,
                    joiner: Some(joiner),
                })
            }
            Ok(Err(error)) => {
                RUNNING.store(false, Ordering::SeqCst);
                Err(error)
            }
            Err(_) => {
                RUNNING.store(false, Ordering::SeqCst);
                Err(Error::Os("hook thread died on startup".into()))
            }
        }
    }

    /// Starts or stops swallowing local input.
    ///
    /// Only ever turn this on while a remote machine is confirmed to be
    /// receiving the events, and keep calling [`Capture::heartbeat`] while it is
    /// on. The hook releases on its own if you stop.
    pub fn set_suppressing(&self, suppress: bool) {
        WATCHDOG.store(now_ms(), Ordering::Relaxed);
        SUPPRESSING.store(suppress, Ordering::Relaxed);
        if suppress {
            HAVE_LAST.store(false, Ordering::Relaxed);
        }
    }

    pub fn is_suppressing(&self) -> bool {
        SUPPRESSING.load(Ordering::Relaxed)
    }

    /// Tells the hook the rest of the program is still alive. Must be called
    /// more often than [`WATCHDOG_LIMIT_MS`] while suppressing.
    pub fn heartbeat(&self) {
        WATCHDOG.store(now_ms(), Ordering::Relaxed);
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        SUPPRESSING.store(false, Ordering::SeqCst);
        unsafe {
            PostThreadMessageW(self.thread_id, WM_QUIT, 0, 0);
        }
        if let Some(joiner) = self.joiner.take() {
            let _ = joiner.join();
        }
        *sink().lock().expect("sink") = None;
        RUNNING.store(false, Ordering::SeqCst);
    }
}

/// Window procedure for the message-only window that receives raw input.
unsafe extern "system" fn raw_input_proc(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if message != WM_INPUT {
        return DefWindowProcW(window, message, wparam, lparam);
    }

    let mut size = 0u32;
    let header = std::mem::size_of::<RAWINPUTHEADER>() as u32;
    GetRawInputData(
        lparam as HRAWINPUT,
        RID_INPUT,
        std::ptr::null_mut(),
        &mut size,
        header,
    );
    if size == 0 || size as usize > std::mem::size_of::<RAWINPUT>() * 4 {
        return DefWindowProcW(window, message, wparam, lparam);
    }

    let mut buffer = vec![0u8; size as usize];
    let written = GetRawInputData(
        lparam as HRAWINPUT,
        RID_INPUT,
        buffer.as_mut_ptr() as *mut _,
        &mut size,
        header,
    );
    if written == u32::MAX || (written as usize) < std::mem::size_of::<RAWINPUTHEADER>() {
        return DefWindowProcW(window, message, wparam, lparam);
    }

    let raw = &*(buffer.as_ptr() as *const RAWINPUT);
    if raw.header.dwType == RIM_TYPEMOUSE {
        let mouse = &raw.data.mouse;
        // Our own replay comes back here too; the marker is what tells it apart.
        if mouse.ulExtraInformation as usize != INJECTED_MARKER
            && mouse.usFlags & MOUSE_MOVE_ABSOLUTE == 0
            && (mouse.lLastX != 0 || mouse.lLastY != 0)
        {
            emit(InputEvent::MouseMove {
                dx: mouse.lLastX,
                dy: mouse.lLastY,
            });
        }
    }

    DefWindowProcW(window, message, wparam, lparam)
}

/// Creates the message-only window and subscribes it to raw mouse reports.
///
/// `RIDEV_INPUTSINK` is the part that matters: without it the reports stop the
/// moment this process is not the foreground window, which is almost always.
unsafe fn start_raw_input() -> Option<HWND> {
    let class_name: Vec<u16> = "InputShareRawInput\0".encode_utf16().collect();
    let instance = windows_sys::Win32::System::LibraryLoader::GetModuleHandleW(std::ptr::null());

    let class = WNDCLASSW {
        style: 0,
        lpfnWndProc: Some(raw_input_proc),
        cbClsExtra: 0,
        cbWndExtra: 0,
        hInstance: instance,
        hIcon: std::ptr::null_mut(),
        hCursor: std::ptr::null_mut(),
        hbrBackground: std::ptr::null_mut(),
        lpszMenuName: std::ptr::null(),
        lpszClassName: class_name.as_ptr(),
    };
    // A duplicate class registration is fine: it means capture ran before in
    // this process, and the class is still there.
    RegisterClassW(&class);

    let window = CreateWindowExW(
        0,
        class_name.as_ptr(),
        class_name.as_ptr(),
        0,
        0,
        0,
        0,
        0,
        HWND_MESSAGE_ONLY,
        std::ptr::null_mut(),
        instance,
        std::ptr::null(),
    );
    if window.is_null() {
        warn!("input capture: could not create the raw input window");
        return None;
    }

    let devices = [RAWINPUTDEVICE {
        usUsagePage: HID_USAGE_PAGE_GENERIC,
        usUsage: HID_USAGE_GENERIC_MOUSE,
        dwFlags: RIDEV_INPUTSINK,
        hwndTarget: window,
    }];
    if RegisterRawInputDevices(
        devices.as_ptr(),
        devices.len() as u32,
        std::mem::size_of::<RAWINPUTDEVICE>() as u32,
    ) == 0
    {
        warn!("input capture: the system refused raw mouse input");
        DestroyWindow(window);
        return None;
    }
    Some(window)
}

fn hook_thread(ready: Sender<Result<u32>>) {
    let thread_id = unsafe { windows_sys::Win32::System::Threading::GetCurrentThreadId() };

    let mouse =
        unsafe { SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_hook), std::ptr::null_mut(), 0) };
    let keyboard =
        unsafe { SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard_hook), std::ptr::null_mut(), 0) };

    if mouse.is_null() || keyboard.is_null() {
        unsafe {
            if !mouse.is_null() {
                UnhookWindowsHookEx(mouse);
            }
            if !keyboard.is_null() {
                UnhookWindowsHookEx(keyboard);
            }
        }
        let _ = ready.send(Err(Error::Os("could not install the input hooks".into())));
        return;
    }
    // Motion comes from here; the hooks provide everything else and the
    // suppression.
    let raw_window = unsafe { start_raw_input() };
    if raw_window.is_none() {
        warn!("input capture: running without raw input, the pointer will not cross edges");
    }

    let _ = ready.send(Ok(thread_id));

    // Low-level hooks are delivered to this thread's message queue, so it has to
    // keep pumping. A thread that stops pumping is a thread whose hooks Windows
    // silently drops after its timeout — which is a safety net, not a plan.
    let mut message: MSG = unsafe { std::mem::zeroed() };
    while unsafe { GetMessageW(&mut message, std::ptr::null_mut(), 0, 0) } > 0 {
        unsafe {
            TranslateMessage(&message);
            DispatchMessageW(&message);
        }
    }

    unsafe {
        if let Some(window) = raw_window {
            DestroyWindow(window);
        }
        UnhookWindowsHookEx(mouse);
        UnhookWindowsHookEx(keyboard);
    }
    SUPPRESSING.store(false, Ordering::SeqCst);
    info!("input capture stopped, hooks removed");
}

const SWALLOW: LRESULT = 1;

unsafe extern "system" fn mouse_hook(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code < 0 {
        return CallNextHookEx(std::ptr::null_mut(), code, wparam, lparam);
    }
    let data = &*(lparam as *const MSLLHOOKSTRUCT);
    if data.dwExtraInfo == INJECTED_MARKER {
        // Our own replay. Never capture it, or the two machines feed each other.
        return CallNextHookEx(std::ptr::null_mut(), code, wparam, lparam);
    }

    let event = match wparam as u32 {
        // Movement deliberately does not come from here.
        //
        // This hook reports where the pointer *is*, and Windows stops the
        // pointer at the edge of the screen. Measuring motion from it means that
        // the moment somebody pushes towards another computer, the numbers go to
        // zero — the exact instant the reading is needed. Raw input reports what
        // the device did instead, and is unaffected by the pointer being pinned.
        //
        // The hook still handles the message, because swallowing it is how local
        // input is suppressed while the pointer is on another machine.
        WM_MOUSEMOVE => None,
        WM_LBUTTONDOWN => Some(button(MouseButton::Left, true)),
        WM_LBUTTONUP => Some(button(MouseButton::Left, false)),
        WM_RBUTTONDOWN => Some(button(MouseButton::Right, true)),
        WM_RBUTTONUP => Some(button(MouseButton::Right, false)),
        WM_MBUTTONDOWN => Some(button(MouseButton::Middle, true)),
        WM_MBUTTONUP => Some(button(MouseButton::Middle, false)),
        WM_XBUTTONDOWN | WM_XBUTTONUP => {
            let which = if (data.mouseData >> 16) as u16 == XBUTTON1 {
                MouseButton::X1
            } else {
                MouseButton::X2
            };
            Some(button(which, wparam as u32 == WM_XBUTTONDOWN))
        }
        WM_MOUSEWHEEL | WM_MOUSEHWHEEL => Some(InputEvent::Wheel {
            delta: (data.mouseData >> 16) as i16 as i32,
            horizontal: wparam as u32 == WM_MOUSEHWHEEL,
        }),
        _ => None,
    };

    if let Some(event) = event {
        emit(event);
    }
    if suppressing_now() {
        return SWALLOW;
    }
    CallNextHookEx(std::ptr::null_mut(), code, wparam, lparam)
}

fn button(button: MouseButton, down: bool) -> InputEvent {
    InputEvent::MouseButton { button, down }
}

unsafe extern "system" fn keyboard_hook(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code < 0 {
        return CallNextHookEx(std::ptr::null_mut(), code, wparam, lparam);
    }
    let data = &*(lparam as *const KBDLLHOOKSTRUCT);
    if data.dwExtraInfo == INJECTED_MARKER {
        return CallNextHookEx(std::ptr::null_mut(), code, wparam, lparam);
    }

    let message = wparam as u32;
    let down = message != WM_KEYUP_U32 && message != WM_SYSKEYUP;
    let is_key = matches!(
        message,
        WM_KEYDOWN_U32 | WM_KEYUP_U32 | WM_SYSKEYDOWN | WM_SYSKEYUP
    );
    if !is_key {
        return CallNextHookEx(std::ptr::null_mut(), code, wparam, lparam);
    }

    // The escape hatch. Checked inside the hook itself, so it works even when
    // everything above this function has stopped responding: Ctrl+Alt+F12 hands
    // the keyboard and mouse straight back.
    if down && data.vkCode as u16 == VK_F12 && held(VK_CONTROL) && held(VK_MENU) {
        if SUPPRESSING.swap(false, Ordering::Relaxed) {
            warn!("input capture: emergency release, local input restored");
        }
        return CallNextHookEx(std::ptr::null_mut(), code, wparam, lparam);
    }

    emit(InputEvent::Key {
        vk: data.vkCode as u16,
        scan: data.scanCode as u16,
        down,
        extended: data.flags & 0x01 != 0,
    });

    if suppressing_now() {
        return SWALLOW;
    }
    CallNextHookEx(std::ptr::null_mut(), code, wparam, lparam)
}

const WM_KEYDOWN_U32: u32 = 0x0100;
const WM_KEYUP_U32: u32 = 0x0101;

fn held(vk: u16) -> bool {
    unsafe {
        windows_sys::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState(vk as i32) as u16 & 0x8000
            != 0
    }
}

/// Where the cursor is, in virtual-desktop coordinates.
pub fn cursor_position() -> Result<(i32, i32)> {
    let mut point = POINT { x: 0, y: 0 };
    let ok = unsafe { windows_sys::Win32::UI::WindowsAndMessaging::GetCursorPos(&mut point) };
    if ok == 0 {
        return Err(Error::Os("GetCursorPos failed".into()));
    }
    Ok((point.x, point.y))
}

/// Puts the cursor somewhere specific on this machine.
pub fn set_cursor_position(x: i32, y: i32) -> Result<()> {
    let ok = unsafe { windows_sys::Win32::UI::WindowsAndMessaging::SetCursorPos(x, y) };
    if ok == 0 {
        return Err(Error::Os("SetCursorPos failed".into()));
    }
    LAST_X.store(x, Ordering::Relaxed);
    LAST_Y.store(y, Ordering::Relaxed);
    HAVE_LAST.store(true, Ordering::Relaxed);
    Ok(())
}

// Silence the unused warning for a type only referenced through a raw pointer.
const _: Option<HWND> = None;
