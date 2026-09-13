//! Everything that is neither Windows nor macOS: Linux, for now.
//!
//! Linux needs evdev, or a compositor protocol on Wayland, and which one depends
//! on the session rather than on the distribution. That is real work rather than
//! a port of this file, so it reports honestly instead of silently doing
//! nothing: a machine here still pairs, syncs and holds its place in the layout,
//! and says plainly that it cannot take over a keyboard.

use std::sync::mpsc::Sender;

use is_core::{DisplayInfo, InputEvent};

use crate::{Error, Result};

pub fn enumerate_displays() -> Result<Vec<DisplayInfo>> {
    Err(Error::Unsupported("display enumeration"))
}

pub fn inject(_event: InputEvent) -> Result<()> {
    Err(Error::Unsupported("input injection"))
}

pub fn cursor_position() -> Result<(i32, i32)> {
    Err(Error::Unsupported("reading the cursor position"))
}

pub fn set_cursor_position(_x: i32, _y: i32) -> Result<()> {
    Err(Error::Unsupported("moving the cursor"))
}

pub struct Capture;

impl Capture {
    pub fn start(_sender: Sender<InputEvent>) -> Result<Self> {
        Err(Error::Unsupported("input capture"))
    }
    pub fn set_suppressing(&self, _suppress: bool) {}
    pub fn is_suppressing(&self) -> bool {
        false
    }
    pub fn heartbeat(&self) {}
}
