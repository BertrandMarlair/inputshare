//! Proves the capture hooks install and report events — without ever swallowing
//! anything.
//!
//!     cargo run -p is-input --example capture_probe
//!
//! Suppression is never switched on here. The hooks run in observe-only mode,
//! so every event still reaches this machine exactly as it would with the
//! program not running. This is the one way to exercise the capture path that
//! cannot leave a computer unable to respond to its own keyboard.

use std::sync::mpsc;
use std::time::{Duration, Instant};

use is_input::capture::Capture;
use is_input::InputEvent;

fn main() {
    let (tx, rx) = mpsc::channel();
    let capture = match Capture::start(tx) {
        Ok(capture) => capture,
        Err(error) => {
            eprintln!("could not install the hooks: {error}");
            return;
        }
    };
    assert!(
        !capture.is_suppressing(),
        "capture must start in observe-only mode"
    );
    println!("hooks installed, observe-only (nothing is being swallowed)");

    let origin = is_input::inject::cursor_position().expect("cursor position");
    println!("cursor is at {origin:?}");
    println!("watching for 4 seconds — move the mouse or type");

    // Note: `SetCursorPos` deliberately cannot be used to test this. It moves
    // the pointer without putting an event into the input stream, so the
    // low-level hook never sees it. Only real hardware and `SendInput` do — and
    // this crate marks everything it injects so the hook ignores its own replay.
    // Which leaves genuine input as the only thing that can exercise this path.

    let mut moves = 0;
    let mut keys = 0;
    let mut others = 0;
    let deadline = Instant::now() + Duration::from_secs(4);
    while let Some(remaining) = deadline.checked_duration_since(Instant::now()) {
        match rx.recv_timeout(remaining) {
            Ok(InputEvent::MouseMove { .. }) => moves += 1,
            Ok(InputEvent::Key { .. }) => keys += 1,
            Ok(_) => others += 1,
            Err(_) => break,
        }
    }

    let _ = is_input::inject::set_cursor_position(origin.0, origin.1);
    drop(capture);

    println!("saw {moves} mouse moves, {keys} key events, {others} other events");
    println!("hooks removed, cursor restored");

    if moves == 0 && keys == 0 && others == 0 {
        eprintln!(
            "nothing was observed — either nobody touched anything, or the hook is not receiving"
        );
        std::process::exit(1);
    }
}
