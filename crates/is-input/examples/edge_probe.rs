//! Does the capture path still see movement once the pointer is pinned against
//! the edge of the screen?
//!
//!     cargo run -p is-input --example edge_probe
//!
//! This is the question the whole feature rests on. Crossing to another computer
//! means noticing that somebody kept pushing *past* the edge — but the operating
//! system stops the pointer there, so if motion is measured from where the
//! pointer is, there is nothing left to measure and the cursor never leaves.
//!
//! Suppression is never switched on here, so this cannot take anyone's keyboard
//! away.

use std::sync::mpsc;
use std::time::Duration;

use is_input::capture::Capture;
use is_input::InputEvent;

#[cfg(windows)]
mod win {
    #[link(name = "user32")]
    extern "system" {
        pub fn mouse_event(flags: u32, dx: i32, dy: i32, data: u32, extra: usize);
    }
    pub const MOUSEEVENTF_MOVE: u32 = 0x0001;
}

fn main() {
    let (tx, rx) = mpsc::channel();
    let capture = match Capture::start(tx) {
        Ok(capture) => capture,
        Err(error) => {
            eprintln!("could not install the hooks: {error}");
            return;
        }
    };

    let origin = is_input::inject::cursor_position().expect("cursor position");
    println!("cursor starts at {origin:?}");

    // Pin the pointer against the left edge, then keep pushing left.
    let _ = is_input::inject::set_cursor_position(0, origin.1);
    std::thread::sleep(Duration::from_millis(300));
    while rx.try_recv().is_ok() {}
    let pinned = is_input::inject::cursor_position().expect("cursor position");
    println!("pointer parked at {pinned:?}");

    println!("pointer parked at x = 0; pushing left 12 more times");
    #[cfg(windows)]
    for _ in 0..12 {
        unsafe { win::mouse_event(win::MOUSEEVENTF_MOVE, -20, 0, 0, 0) };
        std::thread::sleep(Duration::from_millis(40));
    }
    std::thread::sleep(Duration::from_millis(400));

    let mut moves = 0;
    let mut leftward = 0;
    let mut deltas = Vec::new();
    while let Ok(event) = rx.try_recv() {
        if let InputEvent::MouseMove { dx, dy } = event {
            moves += 1;
            if dx < 0 {
                leftward += 1;
            }
            deltas.push((dx, dy));
        }
    }
    let parked = is_input::inject::cursor_position().expect("cursor position");
    println!("pointer ended at {parked:?}");
    println!("deltas: {deltas:?}");

    let _ = is_input::inject::set_cursor_position(origin.0, origin.1);
    drop(capture);

    println!("\nwhile pushing past the edge, the capture path saw:");
    println!("  {moves} movement events, {leftward} of them leftward");
    if leftward == 0 {
        println!("\nNothing. The pointer is clamped at the edge, so measuring motion");
        println!("from its position yields zero and the cursor can never cross.");
        println!("Motion has to come from the device instead of from the pointer.");
    } else {
        println!("\nMovement is still reported past the edge.");
    }
}
