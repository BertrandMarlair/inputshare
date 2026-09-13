//! macOS implementation: CoreGraphics display list, `CGEventPost`, `CGEventTap`.
//!
//! The shape mirrors the Windows file, because the difficult parts are the same
//! difficult parts:
//!
//! - Motion is read from the event's *delta* fields, not from where the cursor
//!   ended up. macOS stops the cursor at the edge of the screen exactly like
//!   Windows does, so a reading taken from the cursor goes to zero at the one
//!   moment it is needed — when somebody is pushing towards the next computer.
//! - Everything injected is stamped, and the tap ignores anything stamped, or a
//!   machine that both captures and injects feeds its own replay back into the
//!   loop.
//! - Suppression starts off, is watched by a watchdog, and has an escape hatch
//!   checked inside the callback itself.
//!
//! ## Permissions
//!
//! An event tap needs **Accessibility** (System Settings → Privacy & Security →
//! Accessibility), and reading keystrokes needs **Input Monitoring**. Both are
//! granted by the user, per application, and neither can be granted by the
//! application itself. `CGEventTapCreate` returns null when Accessibility has
//! not been granted, which is the only signal macOS gives, so that is reported
//! as a permission problem rather than as a mysterious failure.

#![allow(non_upper_case_globals, non_snake_case)]

use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Mutex, OnceLock};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use is_core::{DisplayInfo, InputEvent, MouseButton};
use tracing::{info, warn};

use crate::{Error, Result};

#[path = "mac_keys.rs"]
mod keys;

// ---------------------------------------------------------------- bindings

type CGDirectDisplayID = u32;
type CGError = i32;
type CGFloat = f64;
type CGEventRef = *mut c_void;
type CGEventSourceRef = *mut c_void;
type CGEventTapProxy = *mut c_void;
type CFMachPortRef = *mut c_void;
type CFRunLoopSourceRef = *mut c_void;
type CFRunLoopRef = *mut c_void;
type CFStringRef = *const c_void;
type CFAllocatorRef = *const c_void;
type CGEventMask = u64;
type CGEventType = u32;
type CGEventField = u32;
type CGEventFlags = u64;

type CGEventTapCallBack =
    unsafe extern "C" fn(CGEventTapProxy, CGEventType, CGEventRef, *mut c_void) -> CGEventRef;

#[repr(C)]
#[derive(Clone, Copy)]
struct CGPoint {
    x: CGFloat,
    y: CGFloat,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct CGSize {
    width: CGFloat,
    height: CGFloat,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct CGRect {
    origin: CGPoint,
    size: CGSize,
}

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGGetActiveDisplayList(
        max_displays: u32,
        active: *mut CGDirectDisplayID,
        count: *mut u32,
    ) -> CGError;
    fn CGMainDisplayID() -> CGDirectDisplayID;
    fn CGDisplayBounds(display: CGDirectDisplayID) -> CGRect;
    fn CGDisplayPixelsWide(display: CGDirectDisplayID) -> usize;
    fn CGDisplayPixelsHigh(display: CGDirectDisplayID) -> usize;

    fn CGEventCreate(source: CGEventSourceRef) -> CGEventRef;
    fn CGEventGetLocation(event: CGEventRef) -> CGPoint;
    fn CGEventCreateMouseEvent(
        source: CGEventSourceRef,
        mouse_type: CGEventType,
        position: CGPoint,
        button: u32,
    ) -> CGEventRef;
    fn CGEventCreateKeyboardEvent(
        source: CGEventSourceRef,
        keycode: u16,
        key_down: bool,
    ) -> CGEventRef;
    fn CGEventCreateScrollWheelEvent(
        source: CGEventSourceRef,
        units: u32,
        wheel_count: u32,
        wheel1: i32,
        ...
    ) -> CGEventRef;
    fn CGEventPost(tap: u32, event: CGEventRef);
    fn CGEventSetIntegerValueField(event: CGEventRef, field: CGEventField, value: i64);
    fn CGEventGetIntegerValueField(event: CGEventRef, field: CGEventField) -> i64;
    fn CGEventSetFlags(event: CGEventRef, flags: CGEventFlags);
    fn CGEventGetFlags(event: CGEventRef) -> CGEventFlags;

    fn CGWarpMouseCursorPosition(position: CGPoint) -> CGError;
    fn CGAssociateMouseAndMouseCursorPosition(connected: bool) -> CGError;

    fn CGEventTapCreate(
        tap: u32,
        place: u32,
        options: u32,
        events_of_interest: CGEventMask,
        callback: CGEventTapCallBack,
        user_info: *mut c_void,
    ) -> CFMachPortRef;
    fn CGEventTapEnable(port: CFMachPortRef, enable: bool);
}

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    static kCFRunLoopCommonModes: CFStringRef;

    fn CFMachPortCreateRunLoopSource(
        allocator: CFAllocatorRef,
        port: CFMachPortRef,
        order: isize,
    ) -> CFRunLoopSourceRef;
    fn CFRunLoopGetCurrent() -> CFRunLoopRef;
    fn CFRunLoopAddSource(rl: CFRunLoopRef, source: CFRunLoopSourceRef, mode: CFStringRef);
    fn CFRunLoopRun();
    fn CFRunLoopStop(rl: CFRunLoopRef);
    fn CFRelease(object: *const c_void);
}

// Event types.
const kCGEventLeftMouseDown: CGEventType = 1;
const kCGEventLeftMouseUp: CGEventType = 2;
const kCGEventRightMouseDown: CGEventType = 3;
const kCGEventRightMouseUp: CGEventType = 4;
const kCGEventMouseMoved: CGEventType = 5;
const kCGEventLeftMouseDragged: CGEventType = 6;
const kCGEventRightMouseDragged: CGEventType = 7;
const kCGEventKeyDown: CGEventType = 10;
const kCGEventKeyUp: CGEventType = 11;
const kCGEventFlagsChanged: CGEventType = 12;
const kCGEventScrollWheel: CGEventType = 22;
const kCGEventOtherMouseDown: CGEventType = 25;
const kCGEventOtherMouseUp: CGEventType = 26;
const kCGEventOtherMouseDragged: CGEventType = 27;
/// The system switched the tap off because a callback took too long, or because
/// the user did something that invalidated it. Both are recoverable by turning
/// it back on, and both are silent if nobody does.
const kCGEventTapDisabledByTimeout: CGEventType = 0xFFFF_FFFE;
const kCGEventTapDisabledByUserInput: CGEventType = 0xFFFF_FFFF;

// Event fields.
const kCGMouseEventButtonNumber: CGEventField = 3;
const kCGMouseEventDeltaX: CGEventField = 4;
const kCGMouseEventDeltaY: CGEventField = 5;
const kCGKeyboardEventKeycode: CGEventField = 9;
const kCGScrollWheelEventDeltaAxis1: CGEventField = 11;
const kCGScrollWheelEventDeltaAxis2: CGEventField = 12;
const kCGEventSourceUserData: CGEventField = 42;

// Flags.
const kCGEventFlagMaskShift: CGEventFlags = 0x0002_0000;
const kCGEventFlagMaskControl: CGEventFlags = 0x0004_0000;
const kCGEventFlagMaskAlternate: CGEventFlags = 0x0008_0000;
const kCGEventFlagMaskCommand: CGEventFlags = 0x0010_0000;

// Places and options.
const kCGHIDEventTap: u32 = 0;
const kCGHeadInsertEventTap: u32 = 0;
const kCGEventTapOptionDefault: u32 = 0;
const kCGScrollEventUnitLine: u32 = 1;

const kCGMouseButtonLeft: u32 = 0;
const kCGMouseButtonRight: u32 = 1;
const kCGMouseButtonCenter: u32 = 2;

/// Stamped on everything this process injects, so the tap can tell its own
/// replay apart from a real key press.
const INJECTED_MARKER: i64 = 0x1_4E_50_55; // "INPU"

/// A wheel notch, in the units the wire uses — which are Windows' units, since
/// that is what the protocol was defined in. macOS counts in lines instead.
const WHEEL_NOTCH: i32 = 120;

// ---------------------------------------------------------------- displays

/// The monitors, in the coordinate space macOS uses for the whole desktop: the
/// main display's top-left is the origin, and displays placed to its left or
/// above it have negative coordinates — the same convention as Windows, which
/// is what lets one workspace document describe both.
pub fn enumerate_displays() -> Result<Vec<DisplayInfo>> {
    let mut ids = [0 as CGDirectDisplayID; 32];
    let mut count: u32 = 0;
    let status =
        unsafe { CGGetActiveDisplayList(ids.len() as u32, ids.as_mut_ptr(), &mut count) };
    if status != 0 {
        return Err(Error::Os(format!(
            "CGGetActiveDisplayList failed with {status}"
        )));
    }

    let main = unsafe { CGMainDisplayID() };
    let mut displays: Vec<DisplayInfo> = ids[..count as usize]
        .iter()
        .map(|&id| {
            let bounds = unsafe { CGDisplayBounds(id) };
            let pixels_wide = unsafe { CGDisplayPixelsWide(id) } as f32;
            // Bounds are in points and the pixel count is in pixels; their ratio
            // is the backing scale, which is what "this is a Retina display"
            // actually means.
            let scale = if bounds.size.width > 0.0 {
                pixels_wide / bounds.size.width as f32
            } else {
                1.0
            };
            let _ = unsafe { CGDisplayPixelsHigh(id) };
            DisplayInfo {
                id: format!("display-{id}"),
                name: if id == main {
                    "Main display".to_string()
                } else {
                    format!("Display {id}")
                },
                x: bounds.origin.x.round() as i32,
                y: bounds.origin.y.round() as i32,
                width: bounds.size.width.round().max(0.0) as u32,
                height: bounds.size.height.round().max(0.0) as u32,
                scale,
                primary: id == main,
            }
        })
        .collect();

    // Stable order, so an unchanged layout produces an unchanged record and does
    // not churn the workspace revision on every startup.
    displays.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(displays)
}

/// The rectangle covering every display.
#[derive(Clone, Copy)]
struct Desktop {
    min_x: i32,
    min_y: i32,
    max_x: i32,
    max_y: i32,
}

#[derive(Clone, Copy)]
struct Cached {
    rect: Desktop,
    at: Instant,
}

/// The rectangle covering every display, used to keep injected motion on screen.
///
/// Re-read now and then rather than on every movement: this runs once per mouse
/// event and a display list call per event at a thousand events a second is not
/// free, while somebody plugging in a monitor can wait a second.
fn desktop_bounds() -> Option<Desktop> {
    static CACHE: OnceLock<Mutex<Option<Cached>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(None));
    let mut guard = cache.lock().ok()?;
    if let Some(cached) = *guard {
        if cached.at.elapsed().as_secs() < 2 {
            return Some(cached.rect);
        }
    }
    let displays = enumerate_displays().ok()?;
    let mut iter = displays.iter();
    let first = iter.next()?;
    let mut rect = Desktop {
        min_x: first.x,
        min_y: first.y,
        max_x: first.x + first.width as i32,
        max_y: first.y + first.height as i32,
    };
    for display in iter {
        rect.min_x = rect.min_x.min(display.x);
        rect.min_y = rect.min_y.min(display.y);
        rect.max_x = rect.max_x.max(display.x + display.width as i32);
        rect.max_y = rect.max_y.max(display.y + display.height as i32);
    }
    *guard = Some(Cached {
        rect,
        at: Instant::now(),
    });
    Some(rect)
}

// ---------------------------------------------------------------- injection

/// Where this machine believes the cursor is, so movement given as a delta can
/// be turned into the absolute point macOS wants.
static LAST_X: AtomicI32 = AtomicI32::new(0);
static LAST_Y: AtomicI32 = AtomicI32::new(0);
static HAVE_LAST: AtomicBool = AtomicBool::new(false);
/// Which buttons this machine is holding down on behalf of a remote mouse.
/// Movement while a button is held has to be a drag, or nothing drags.
static HELD_BUTTONS: AtomicU32 = AtomicU32::new(0);
/// Modifier state as the remote keyboard has left it. macOS carries modifiers on
/// every event rather than inferring them, so each injected event has to say.
static INJECT_FLAGS: AtomicU64 = AtomicU64::new(0);

pub fn inject(event: InputEvent) -> Result<()> {
    match event {
        // Placing the cursor is not an injected event; it is a direct move, and
        // it has to happen before the motion that follows it.
        InputEvent::CursorEnter { x, y } => set_cursor_position(x, y),
        InputEvent::MouseMove { dx, dy } => inject_move(dx, dy),
        InputEvent::MouseButton { button, down } => inject_button(button, down),
        InputEvent::Wheel { delta, horizontal } => inject_wheel(delta, horizontal),
        InputEvent::Key {
            vk,
            scan,
            down,
            extended,
        } => inject_key(vk, scan, down, extended),
    }
}

/// Finishes an event the same way for everything posted from here: stamp it so
/// the tap will not read it back, carry the modifier state, and release it.
unsafe fn post(event: CGEventRef) {
    if event.is_null() {
        return;
    }
    CGEventSetIntegerValueField(event, kCGEventSourceUserData, INJECTED_MARKER);
    CGEventSetFlags(
        event,
        CGEventGetFlags(event) | INJECT_FLAGS.load(Ordering::Relaxed),
    );
    CGEventPost(kCGHIDEventTap, event);
    CFRelease(event);
}

fn current_point() -> (i32, i32) {
    if HAVE_LAST.load(Ordering::Relaxed) {
        return (LAST_X.load(Ordering::Relaxed), LAST_Y.load(Ordering::Relaxed));
    }
    cursor_position().unwrap_or((0, 0))
}

fn inject_move(dx: i32, dy: i32) -> Result<()> {
    let (x, y) = current_point();
    let (mut nx, mut ny) = (x + dx, y + dy);
    // Clamped to the desktop. Without this the remembered position drifts off
    // the screen while the cursor stays pinned at the edge, and coming back
    // takes as many movements as it took to leave.
    if let Some(desktop) = desktop_bounds() {
        nx = nx.clamp(desktop.min_x, desktop.max_x - 1);
        ny = ny.clamp(desktop.min_y, desktop.max_y - 1);
    }

    let held = HELD_BUTTONS.load(Ordering::Relaxed);
    let (kind, button) = if held & (1 << 0) != 0 {
        (kCGEventLeftMouseDragged, kCGMouseButtonLeft)
    } else if held & (1 << 1) != 0 {
        (kCGEventRightMouseDragged, kCGMouseButtonRight)
    } else if held != 0 {
        (kCGEventOtherMouseDragged, kCGMouseButtonCenter)
    } else {
        (kCGEventMouseMoved, kCGMouseButtonLeft)
    };

    unsafe {
        let event = CGEventCreateMouseEvent(
            std::ptr::null_mut(),
            kind,
            CGPoint {
                x: nx as CGFloat,
                y: ny as CGFloat,
            },
            button,
        );
        if event.is_null() {
            return Err(Error::Os("CGEventCreateMouseEvent returned nothing".into()));
        }
        // Applications that read motion rather than position — anything with a
        // 3D camera in it — need the delta as well as the destination.
        CGEventSetIntegerValueField(event, kCGMouseEventDeltaX, (nx - x) as i64);
        CGEventSetIntegerValueField(event, kCGMouseEventDeltaY, (ny - y) as i64);
        post(event);
    }
    remember(nx, ny);
    Ok(())
}

fn inject_button(button: MouseButton, down: bool) -> Result<()> {
    let (kind, number, bit) = match (button, down) {
        (MouseButton::Left, true) => (kCGEventLeftMouseDown, kCGMouseButtonLeft, 0),
        (MouseButton::Left, false) => (kCGEventLeftMouseUp, kCGMouseButtonLeft, 0),
        (MouseButton::Right, true) => (kCGEventRightMouseDown, kCGMouseButtonRight, 1),
        (MouseButton::Right, false) => (kCGEventRightMouseUp, kCGMouseButtonRight, 1),
        (MouseButton::Middle, true) => (kCGEventOtherMouseDown, kCGMouseButtonCenter, 2),
        (MouseButton::Middle, false) => (kCGEventOtherMouseUp, kCGMouseButtonCenter, 2),
        (MouseButton::X1, true) => (kCGEventOtherMouseDown, 3, 3),
        (MouseButton::X1, false) => (kCGEventOtherMouseUp, 3, 3),
        (MouseButton::X2, true) => (kCGEventOtherMouseDown, 4, 4),
        (MouseButton::X2, false) => (kCGEventOtherMouseUp, 4, 4),
    };
    if down {
        HELD_BUTTONS.fetch_or(1 << bit, Ordering::Relaxed);
    } else {
        HELD_BUTTONS.fetch_and(!(1 << bit), Ordering::Relaxed);
    }

    let (x, y) = current_point();
    unsafe {
        let event = CGEventCreateMouseEvent(
            std::ptr::null_mut(),
            kind,
            CGPoint {
                x: x as CGFloat,
                y: y as CGFloat,
            },
            number,
        );
        if event.is_null() {
            return Err(Error::Os("CGEventCreateMouseEvent returned nothing".into()));
        }
        CGEventSetIntegerValueField(event, kCGMouseEventButtonNumber, number as i64);
        post(event);
    }
    Ok(())
}

fn inject_wheel(delta: i32, horizontal: bool) -> Result<()> {
    // The wire counts in Windows' notches of 120; macOS counts in lines. A
    // fraction of a notch still has to move something, or a trackpad's small
    // steps all round down to nothing.
    let lines = if delta == 0 {
        return Ok(());
    } else {
        let scaled = delta / WHEEL_NOTCH;
        if scaled == 0 {
            delta.signum()
        } else {
            scaled
        }
    };
    unsafe {
        let event = CGEventCreateScrollWheelEvent(
            std::ptr::null_mut(),
            kCGScrollEventUnitLine,
            if horizontal { 2 } else { 1 },
            if horizontal { 0 } else { lines },
        );
        if event.is_null() {
            return Err(Error::Os("CGEventCreateScrollWheelEvent failed".into()));
        }
        if horizontal {
            CGEventSetIntegerValueField(event, kCGScrollWheelEventDeltaAxis2, lines as i64);
        }
        post(event);
    }
    Ok(())
}

fn inject_key(vk: u16, scan: u16, down: bool, extended: bool) -> Result<()> {
    let Some(keycode) = keys::from_pc(scan, vk, extended) else {
        // Unmapped keys are dropped rather than guessed: pressing an arbitrary
        // key is worse than pressing none.
        return Ok(());
    };

    // Modifiers are state on this side, not events. Track what the remote
    // keyboard is holding so the next letter arrives capitalised.
    let mask = match keycode {
        56 | 60 => kCGEventFlagMaskShift,
        59 | 62 => kCGEventFlagMaskControl,
        58 | 61 => kCGEventFlagMaskAlternate,
        55 | 54 => kCGEventFlagMaskCommand,
        _ => 0,
    };
    if mask != 0 {
        if down {
            INJECT_FLAGS.fetch_or(mask, Ordering::Relaxed);
        } else {
            INJECT_FLAGS.fetch_and(!mask, Ordering::Relaxed);
        }
    }

    unsafe {
        let event = CGEventCreateKeyboardEvent(std::ptr::null_mut(), keycode, down);
        if event.is_null() {
            return Err(Error::Os("CGEventCreateKeyboardEvent failed".into()));
        }
        post(event);
    }
    Ok(())
}

fn remember(x: i32, y: i32) {
    LAST_X.store(x, Ordering::Relaxed);
    LAST_Y.store(y, Ordering::Relaxed);
    HAVE_LAST.store(true, Ordering::Relaxed);
}

/// Where the cursor is, in desktop coordinates.
pub fn cursor_position() -> Result<(i32, i32)> {
    unsafe {
        let event = CGEventCreate(std::ptr::null_mut());
        if event.is_null() {
            return Err(Error::Os("CGEventCreate returned nothing".into()));
        }
        let point = CGEventGetLocation(event);
        CFRelease(event);
        Ok((point.x.round() as i32, point.y.round() as i32))
    }
}

/// Puts the cursor somewhere specific on this machine.
pub fn set_cursor_position(x: i32, y: i32) -> Result<()> {
    let status = unsafe {
        CGWarpMouseCursorPosition(CGPoint {
            x: x as CGFloat,
            y: y as CGFloat,
        })
    };
    if status != 0 {
        return Err(Error::Os(format!(
            "CGWarpMouseCursorPosition failed with {status}"
        )));
    }
    // Warping decouples the cursor from the mouse for a quarter of a second,
    // during which the local mouse appears dead. Reconnecting them immediately
    // is the documented way out, and without it every crossing feels broken.
    //
    // Unless the pointer is deliberately detached because it is on another
    // computer, in which case reattaching here would undo the suppression the
    // sharing loop asked for.
    if !DETACHED.load(Ordering::SeqCst) {
        unsafe {
            CGAssociateMouseAndMouseCursorPosition(true);
        }
    }
    remember(x, y);
    Ok(())
}

// ------------------------------------------------------------------ capture

/// Whether captured input is swallowed instead of also acting locally.
static SUPPRESSING: AtomicBool = AtomicBool::new(false);
/// Whether the physical mouse has been unhooked from this Mac's cursor.
static DETACHED: AtomicBool = AtomicBool::new(false);
/// Last time the owner said it was still alive, in milliseconds.
static WATCHDOG: AtomicU64 = AtomicU64::new(0);
/// The run loop the tap is attached to, so it can be stopped from elsewhere.
static RUNLOOP: AtomicUsize = AtomicUsize::new(0);
/// The tap itself, needed inside the callback to switch it back on after macOS
/// switches it off.
static TAP: AtomicUsize = AtomicUsize::new(0);
static RUNNING: AtomicBool = AtomicBool::new(false);

/// A tap that has stopped hearing from the rest of the program stops
/// suppressing. If the agent deadlocks while the cursor is on another machine,
/// this is what gives the keyboard back without a reboot.
const WATCHDOG_LIMIT_MS: u64 = 2_000;

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Unhooks the physical mouse from this Mac's cursor, or hooks it back up.
///
/// Swallowing the event is not enough on macOS, and this is the difference
/// between the Mac working and the Mac looking haunted. A tap sees an event
/// *after* the HID system has already moved the cursor with it, so returning
/// null stops applications from being told about the movement but does not stop
/// the arrow from sliding across the screen. While the pointer is on another
/// computer, the Mac's own cursor would wander off on its own.
///
/// So the mouse is detached from the cursor for as long as this machine is not
/// the one the pointer is on. Deltas still arrive — that is the whole point —
/// they simply stop moving anything here.
///
/// Every path that stops suppressing must come back through here, including the
/// watchdog and the emergency release: a Mac left with its mouse detached is a
/// Mac whose cursor does not move, which is precisely the emergency the rest of
/// this file exists to prevent.
fn detach_pointer(detach: bool) {
    if DETACHED.swap(detach, Ordering::SeqCst) == detach {
        return;
    }
    unsafe {
        CGAssociateMouseAndMouseCursorPosition(!detach);
    }
}

/// Sets suppression and the pointer attachment together, because they are two
/// halves of one thing and a path that changed only one of them would either
/// leak input or freeze the cursor.
fn set_suppression(suppress: bool) {
    SUPPRESSING.store(suppress, Ordering::SeqCst);
    detach_pointer(suppress);
}

fn suppressing_now() -> bool {
    if !SUPPRESSING.load(Ordering::Relaxed) {
        return false;
    }
    let last = WATCHDOG.load(Ordering::Relaxed);
    if now_ms().saturating_sub(last) > WATCHDOG_LIMIT_MS {
        // Fail open, always. A missed keystroke is an annoyance; a machine that
        // ignores its own keyboard is an emergency.
        set_suppression(false);
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

/// A running capture. Dropping it removes the tap.
pub struct Capture {
    joiner: Option<std::thread::JoinHandle<()>>,
}

impl Capture {
    /// Installs the tap on a dedicated thread with its own run loop.
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
        set_suppression(false);
        WATCHDOG.store(now_ms(), Ordering::SeqCst);
        HAVE_LAST.store(false, Ordering::SeqCst);
        HELD_BUTTONS.store(0, Ordering::SeqCst);
        INJECT_FLAGS.store(0, Ordering::SeqCst);

        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let joiner = std::thread::Builder::new()
            .name("inputshare-tap".into())
            .spawn(move || tap_thread(ready_tx))
            .map_err(|error| Error::Os(error.to_string()))?;

        match ready_rx.recv() {
            Ok(Ok(())) => {
                info!("input capture running in observe-only mode");
                Ok(Self {
                    joiner: Some(joiner),
                })
            }
            Ok(Err(error)) => {
                RUNNING.store(false, Ordering::SeqCst);
                Err(error)
            }
            Err(_) => {
                RUNNING.store(false, Ordering::SeqCst);
                Err(Error::Os("the capture thread died on startup".into()))
            }
        }
    }

    /// Starts or stops swallowing local input.
    ///
    /// Only ever turn this on while a remote machine is confirmed to be
    /// receiving the events, and keep calling [`Capture::heartbeat`] while it is
    /// on. The tap releases on its own if you stop.
    pub fn set_suppressing(&self, suppress: bool) {
        WATCHDOG.store(now_ms(), Ordering::Relaxed);
        set_suppression(suppress);
        if suppress {
            HAVE_LAST.store(false, Ordering::Relaxed);
        }
    }

    pub fn is_suppressing(&self) -> bool {
        SUPPRESSING.load(Ordering::Relaxed)
    }

    /// Tells the tap the rest of the program is still alive. Must be called more
    /// often than [`WATCHDOG_LIMIT_MS`] while suppressing.
    pub fn heartbeat(&self) {
        WATCHDOG.store(now_ms(), Ordering::Relaxed);
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        set_suppression(false);
        let runloop = RUNLOOP.swap(0, Ordering::SeqCst);
        if runloop != 0 {
            // Safe to call from another thread; it is the documented way to ask
            // a run loop to return.
            unsafe { CFRunLoopStop(runloop as CFRunLoopRef) };
        }
        if let Some(joiner) = self.joiner.take() {
            let _ = joiner.join();
        }
        *sink().lock().expect("sink") = None;
        RUNNING.store(false, Ordering::SeqCst);
    }
}

fn event_mask() -> CGEventMask {
    let types = [
        kCGEventLeftMouseDown,
        kCGEventLeftMouseUp,
        kCGEventRightMouseDown,
        kCGEventRightMouseUp,
        kCGEventMouseMoved,
        kCGEventLeftMouseDragged,
        kCGEventRightMouseDragged,
        kCGEventKeyDown,
        kCGEventKeyUp,
        kCGEventFlagsChanged,
        kCGEventScrollWheel,
        kCGEventOtherMouseDown,
        kCGEventOtherMouseUp,
        kCGEventOtherMouseDragged,
    ];
    types.iter().fold(0u64, |mask, &t| mask | (1u64 << t))
}

fn tap_thread(ready: Sender<Result<()>>) {
    unsafe {
        let tap = CGEventTapCreate(
            // At the HID level, which is before any application sees the event —
            // the only place a suppressed key really disappears.
            kCGHIDEventTap,
            kCGHeadInsertEventTap,
            kCGEventTapOptionDefault,
            event_mask(),
            tap_callback,
            std::ptr::null_mut(),
        );
        if tap.is_null() {
            let _ = ready.send(Err(Error::Os(
                "macOS refused the event tap. Give InputShare permission in \
                 System Settings > Privacy & Security > Accessibility, and in \
                 Input Monitoring, then try again."
                    .into(),
            )));
            return;
        }
        TAP.store(tap as usize, Ordering::SeqCst);

        let source = CFMachPortCreateRunLoopSource(std::ptr::null(), tap, 0);
        if source.is_null() {
            CFRelease(tap);
            TAP.store(0, Ordering::SeqCst);
            let _ = ready.send(Err(Error::Os("could not attach the event tap".into())));
            return;
        }

        let runloop = CFRunLoopGetCurrent();
        CFRunLoopAddSource(runloop, source, kCFRunLoopCommonModes);
        CGEventTapEnable(tap, true);
        RUNLOOP.store(runloop as usize, Ordering::SeqCst);

        let _ = ready.send(Ok(()));

        // Events are delivered to this run loop, so the thread has to keep
        // running it. A tap whose run loop stops is a tap that silently receives
        // nothing.
        CFRunLoopRun();

        CGEventTapEnable(tap, false);
        CFRelease(source);
        CFRelease(tap);
        TAP.store(0, Ordering::SeqCst);
    }
    set_suppression(false);
    info!("input capture stopped, the event tap is gone");
}

unsafe extern "C" fn tap_callback(
    _proxy: CGEventTapProxy,
    kind: CGEventType,
    event: CGEventRef,
    _user: *mut c_void,
) -> CGEventRef {
    // macOS switches a tap off if a callback is ever too slow, and says so
    // exactly once. Not turning it back on means input sharing stops working
    // with no error anywhere.
    if kind == kCGEventTapDisabledByTimeout || kind == kCGEventTapDisabledByUserInput {
        let tap = TAP.load(Ordering::Relaxed);
        if tap != 0 {
            warn!("input capture: the system disabled the tap, turning it back on");
            CGEventTapEnable(tap as CFMachPortRef, true);
        }
        return event;
    }

    // Our own replay. Never capture it, or the two machines feed each other.
    if CGEventGetIntegerValueField(event, kCGEventSourceUserData) == INJECTED_MARKER {
        return event;
    }

    let flags = CGEventGetFlags(event);

    let reported = match kind {
        kCGEventMouseMoved
        | kCGEventLeftMouseDragged
        | kCGEventRightMouseDragged
        | kCGEventOtherMouseDragged => {
            // Deliberately the delta fields rather than the location: the cursor
            // stops at the edge of the screen, and a reading taken from it goes
            // to zero at the exact moment somebody is pushing towards the next
            // computer.
            let dx = CGEventGetIntegerValueField(event, kCGMouseEventDeltaX) as i32;
            let dy = CGEventGetIntegerValueField(event, kCGMouseEventDeltaY) as i32;
            if dx == 0 && dy == 0 {
                None
            } else {
                Some(InputEvent::MouseMove { dx, dy })
            }
        }
        kCGEventLeftMouseDown => Some(button(MouseButton::Left, true)),
        kCGEventLeftMouseUp => Some(button(MouseButton::Left, false)),
        kCGEventRightMouseDown => Some(button(MouseButton::Right, true)),
        kCGEventRightMouseUp => Some(button(MouseButton::Right, false)),
        kCGEventOtherMouseDown | kCGEventOtherMouseUp => {
            let number = CGEventGetIntegerValueField(event, kCGMouseEventButtonNumber);
            let which = match number {
                2 => MouseButton::Middle,
                3 => MouseButton::X1,
                _ => MouseButton::X2,
            };
            Some(button(which, kind == kCGEventOtherMouseDown))
        }
        kCGEventScrollWheel => {
            let vertical = CGEventGetIntegerValueField(event, kCGScrollWheelEventDeltaAxis1) as i32;
            let horizontal =
                CGEventGetIntegerValueField(event, kCGScrollWheelEventDeltaAxis2) as i32;
            if vertical != 0 {
                Some(InputEvent::Wheel {
                    delta: vertical * WHEEL_NOTCH,
                    horizontal: false,
                })
            } else if horizontal != 0 {
                Some(InputEvent::Wheel {
                    delta: horizontal * WHEEL_NOTCH,
                    horizontal: true,
                })
            } else {
                None
            }
        }
        kCGEventKeyDown | kCGEventKeyUp | kCGEventFlagsChanged => {
            let keycode = CGEventGetIntegerValueField(event, kCGKeyboardEventKeycode) as u16;

            // A modifier does not report up or down; it reports that the set of
            // modifiers changed. Whether this one is now held is read from the
            // flags the event carries.
            let down = match kind {
                kCGEventKeyDown => true,
                kCGEventKeyUp => false,
                _ => match keycode {
                    56 | 60 => flags & kCGEventFlagMaskShift != 0,
                    59 | 62 => flags & kCGEventFlagMaskControl != 0,
                    58 | 61 => flags & kCGEventFlagMaskAlternate != 0,
                    55 | 54 => flags & kCGEventFlagMaskCommand != 0,
                    // Caps lock and fn: reported, never held in the usual sense.
                    _ => false,
                },
            };

            // The escape hatch. Checked inside the callback itself, so it works
            // even when everything above this function has stopped responding:
            // Control+Option+F12 hands the keyboard and mouse straight back.
            if down
                && keycode == 111
                && flags & kCGEventFlagMaskControl != 0
                && flags & kCGEventFlagMaskAlternate != 0
            {
                if SUPPRESSING.load(Ordering::Relaxed) {
                    set_suppression(false);
                    warn!("input capture: emergency release, local input restored");
                }
                return event;
            }

            keys::to_pc(keycode).map(|(scan, vk, extended)| InputEvent::Key {
                vk,
                scan,
                down,
                extended,
            })
        }
        _ => None,
    };

    if let Some(reported) = reported {
        emit(reported);
    }
    if suppressing_now() {
        // Null is how a tap swallows an event: nothing downstream ever sees it.
        return std::ptr::null_mut();
    }
    event
}

fn button(button: MouseButton, down: bool) -> InputEvent {
    InputEvent::MouseButton { button, down }
}
