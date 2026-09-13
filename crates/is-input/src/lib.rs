//! This machine's actual hardware: its screens, its keyboard, its mouse.
//!
//! Three jobs, in increasing order of how much damage they can do:
//!
//! - [`displays`] — read the real monitor layout. Harmless.
//! - [`inject`] — replay events that arrived from another machine. Visible, but
//!   recoverable: the worst case is a stray click.
//! - [`capture`] — read the local keyboard and mouse, and optionally *swallow*
//!   the events so they do not also act on this machine.
//!
//! That last one is the dangerous part and the module says so at length. A
//! global low-level hook that suppresses input is, by construction, one bug away
//! from a computer that no longer responds to its own keyboard. Everything about
//! the design in [`capture`] — the default of off, the watchdog, the emergency
//! release — exists because of that, not as polish.

pub mod capture;
pub mod displays;
pub mod inject;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("this platform is not supported yet: {0}")]
    Unsupported(&'static str),

    #[error("the operating system refused: {0}")]
    Os(String),

    #[error("input capture is already running")]
    AlreadyRunning,
}

/// Re-exported so callers of this crate do not need to reach into `is-core`
/// for the type they are about to send or replay.
pub use is_core::{InputEvent, MouseButton};

#[cfg(windows)]
#[path = "platform/windows.rs"]
mod platform;

#[cfg(target_os = "macos")]
#[path = "platform/macos.rs"]
mod platform;

#[cfg(not(any(windows, target_os = "macos")))]
#[path = "platform/other.rs"]
mod platform;

/// The Mac key table, compiled here so its tests run on any machine.
///
/// It is the one part of the macOS backend that is pure data and pure logic, and
/// the part most likely to be wrong. Checking it should not require owning a
/// Mac.
#[cfg(all(test, not(target_os = "macos")))]
#[path = "platform/mac_keys.rs"]
mod mac_keys;
