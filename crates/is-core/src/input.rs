//! The vocabulary of input events, shared by every crate that touches them.
//!
//! It lives here, next to the workspace document, because it is the thing the
//! product moves: the capture side produces these, the network carries them, the
//! injection side replays them. Putting it in the platform crate instead would
//! drag Windows headers into the networking code for no reason.

use serde::{Deserialize, Serialize};

/// One input event, in a form that means the same thing on every machine.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InputEvent {
    /// Movement as a delta, never as a position.
    ///
    /// The sending machine has no idea where the cursor sits inside the
    /// receiving machine's screens, and coordinates from one desktop mean
    /// nothing on another — they would put the pointer somewhere arbitrary.
    MouseMove {
        dx: i32,
        dy: i32,
    },
    /// The cursor has just crossed onto the receiving machine, at this point in
    /// that machine's own coordinates.
    ///
    /// Sent once per crossing. Without it the pointer would appear wherever it
    /// happened to be left last time, usually on the wrong edge of the wrong
    /// screen.
    CursorEnter {
        x: i32,
        y: i32,
    },
    MouseButton {
        button: MouseButton,
        down: bool,
    },
    Wheel {
        delta: i32,
        horizontal: bool,
    },
    Key {
        /// Virtual key code, as the sending machine saw it.
        vk: u16,
        /// Hardware scan code. Replay uses this rather than the virtual key, so
        /// two machines with different keyboard layouts still produce the key
        /// that was physically pressed.
        scan: u16,
        down: bool,
        extended: bool,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MouseButton {
    Left,
    Right,
    Middle,
    X1,
    X2,
}
