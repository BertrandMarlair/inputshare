//! Reading the local keyboard and mouse — and, when asked, taking them away
//! from this machine.
//!
//! Read this before changing anything here.
//!
//! Suppression means a global low-level hook returns "handled" instead of
//! passing the event on. While it is on, this process is the only thing that
//! sees the keyboard and mouse. If it stops behaving — a deadlock, a panic in
//! the wrong place, a channel that nobody drains — the computer stops responding
//! to its own input devices, and the user may have no way to click the thing
//! that would fix it.
//!
//! So there are four independent ways out, and none of them depend on the code
//! that asked for suppression still working:
//!
//! 1. **It starts off.** Capture begins in observe-only mode; events are
//!    reported and still act locally. Swallowing is a separate call.
//! 2. **A watchdog.** The owner has to keep saying it is alive. Miss the
//!    deadline and the hook releases on its own.
//! 3. **An emergency release.** Ctrl+Alt+F12 is checked inside the hook itself,
//!    so it works even if everything above it is wedged.
//! 4. **Windows itself.** A hook that takes too long is dropped by the OS. That
//!    is a backstop, not a design.
//!
//! The rule behind all of it: fail open. A dropped keystroke is an annoyance; a
//! machine that ignores its own keyboard is an emergency.

pub use crate::platform::Capture;
