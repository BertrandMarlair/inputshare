//! Replaying events that arrived from another machine.

use crate::{platform, InputEvent, Result};

/// Replays one event on this machine.
///
/// Everything injected is marked, and the capture hook ignores anything marked.
/// Without that, a machine that both captures and injects would feed its own
/// replay straight back into the stream.
pub fn inject(event: InputEvent) -> Result<()> {
    platform::inject(event)
}

pub fn cursor_position() -> Result<(i32, i32)> {
    platform::cursor_position()
}

pub fn set_cursor_position(x: i32, y: i32) -> Result<()> {
    platform::set_cursor_position(x, y)
}
