//! Reading the real monitors of whatever machine runs the test.
//!
//! Deliberately not mocked: the value of this crate is that it agrees with the
//! operating system, and a fake monitor list would agree with nothing.

#[test]
#[cfg(windows)]
fn the_real_monitor_layout_is_readable() {
    let displays = is_input::displays::enumerate().expect("monitors are readable");

    assert!(!displays.is_empty(), "a desktop has at least one monitor");
    assert_eq!(
        displays.iter().filter(|d| d.primary).count(),
        1,
        "exactly one monitor is primary"
    );
    for display in &displays {
        assert!(
            display.width > 0 && display.height > 0,
            "a monitor with no pixels would break cursor routing: {display:?}"
        );
        assert!(display.scale > 0.0, "scale is a ratio, never zero");
        assert!(!display.id.is_empty(), "a display needs a stable identity");
    }
}

#[test]
#[cfg(windows)]
fn enumeration_is_stable_between_calls() {
    // The workspace document is rewritten whenever the layout changes, and a
    // rewrite is broadcast to every peer. Unstable ordering here would bump the
    // revision on every startup for no reason.
    let first = is_input::displays::enumerate().expect("monitors are readable");
    let second = is_input::displays::enumerate().expect("monitors are readable");
    assert!(!is_input::displays::differ(&first, &second));
}

#[test]
fn an_unchanged_layout_is_not_reported_as_a_change() {
    let one = is_core::DisplayInfo {
        id: "one".into(),
        name: "one".into(),
        x: 0,
        y: 0,
        width: 1920,
        height: 1080,
        scale: 1.0,
        primary: true,
    };
    let a = vec![one.clone()];
    let mut b = a.clone();
    assert!(!is_input::displays::differ(&a, &b));

    b[0].x = 10;
    assert!(is_input::displays::differ(&a, &b), "a move is a change");

    b[0].x = 0;
    b.push(one);
    assert!(
        is_input::displays::differ(&a, &b),
        "a new monitor is a change"
    );
}
