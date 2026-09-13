//! Requirement 8: the topology stays configured, but the cursor must never be
//! handed to a machine that is offline.

mod common;

use std::collections::HashSet;

use common::machine;
use is_core::layout::{fallback_owner, resolve_cursor};
use is_core::{
    MachineId, MachineRecord, OfflineEdgeBehavior, Point, Resolution, WorkspaceDoc,
    WorkspaceSettings,
};

/// The layout from the requirements: Mac left of PC1, PC2 above PC1.
struct Desk {
    doc: WorkspaceDoc,
    mac: MachineRecord,
    pc1: MachineRecord,
    pc2: MachineRecord,
}

fn desk() -> Desk {
    let mac = machine("Mac", Point::new(-1920, 0));
    let pc1 = machine("PC1", Point::new(0, 0));
    let pc2 = machine("PC2", Point::new(0, -1080));

    let mut doc = WorkspaceDoc::create("My Desk", pc1.clone());
    doc.upsert_machine(pc1.id, mac.clone());
    doc.upsert_machine(pc1.id, pc2.clone());

    Desk { doc, mac, pc1, pc2 }
}

fn online(ids: impl IntoIterator<Item = MachineId>) -> HashSet<MachineId> {
    ids.into_iter().collect()
}

fn set_offline_edge(doc: &mut WorkspaceDoc, origin: MachineId, behavior: OfflineEdgeBehavior) {
    doc.set_settings(
        origin,
        WorkspaceSettings {
            offline_edge: behavior,
            ..WorkspaceSettings::default()
        },
    );
}

#[test]
fn cursor_crosses_to_an_online_neighbour() {
    let desk = desk();
    let up = online([desk.pc1.id, desk.mac.id]);

    // Leaving PC1 past its left edge.
    let resolution = resolve_cursor(&desk.doc, &up, desk.pc1.id, Point::new(-5, 400));

    match resolution {
        Resolution::Move { machine, point } => {
            assert_eq!(machine, desk.mac.id);
            // Enters just inside the Mac's right edge, same height.
            assert_eq!(point, Point::new(-1, 400));
        }
        other => panic!("expected a handoff to the Mac, got {other:?}"),
    }
}

#[test]
fn cursor_is_blocked_at_the_edge_of_an_offline_machine() {
    let desk = desk();
    let up = online([desk.pc1.id]); // Mac and PC2 are both down.

    let resolution = resolve_cursor(&desk.doc, &up, desk.pc1.id, Point::new(-5, 400));

    assert_eq!(
        resolution,
        Resolution::Stay(Point::new(0, 400)),
        "the cursor must stop on PC1, not disappear onto the offline Mac"
    );
}

#[test]
fn cursor_crosses_upwards_to_pc2_when_it_is_up() {
    let desk = desk();
    let up = online([desk.pc1.id, desk.pc2.id]);

    let resolution = resolve_cursor(&desk.doc, &up, desk.pc1.id, Point::new(800, -10));

    match resolution {
        Resolution::Move { machine, point } => {
            assert_eq!(machine, desk.pc2.id);
            assert_eq!(point, Point::new(800, -1));
        }
        other => panic!("expected a handoff to PC2, got {other:?}"),
    }
}

#[test]
fn offline_machine_keeps_its_place_in_the_topology() {
    let desk = desk();

    // PC2 is offline, yet it is still configured above PC1 — the route comes
    // back by itself once it returns.
    assert!(desk.doc.machine(desk.pc2.id).is_some());

    let down = online([desk.pc1.id]);
    assert!(matches!(
        resolve_cursor(&desk.doc, &down, desk.pc1.id, Point::new(800, -10)),
        Resolution::Stay(_)
    ));

    let back = online([desk.pc1.id, desk.pc2.id]);
    assert!(matches!(
        resolve_cursor(&desk.doc, &back, desk.pc1.id, Point::new(800, -10)),
        Resolution::Move { .. }
    ));
}

#[test]
fn skip_over_reaches_the_machine_beyond_an_offline_one() {
    // Three machines in a row: far, middle, near. The middle one is down.
    let far = machine("Far", Point::new(-3840, 0));
    let middle = machine("Middle", Point::new(-1920, 0));
    let near = machine("Near", Point::new(0, 0));

    let mut doc = WorkspaceDoc::create("Row", near.clone());
    doc.upsert_machine(near.id, middle.clone());
    doc.upsert_machine(near.id, far.clone());

    let up = online([near.id, far.id]);
    let heading_left = Point::new(-5, 400);

    set_offline_edge(&mut doc, near.id, OfflineEdgeBehavior::Block);
    assert!(
        matches!(
            resolve_cursor(&doc, &up, near.id, heading_left),
            Resolution::Stay(_)
        ),
        "Block stops at the offline machine"
    );

    set_offline_edge(&mut doc, near.id, OfflineEdgeBehavior::SkipOver);
    match resolve_cursor(&doc, &up, near.id, heading_left) {
        Resolution::Move { machine, .. } => assert_eq!(machine, far.id),
        other => panic!("expected SkipOver to reach the far machine, got {other:?}"),
    }
}

#[test]
fn movement_inside_the_current_machine_is_left_alone() {
    let desk = desk();
    let up = online([desk.pc1.id, desk.mac.id, desk.pc2.id]);

    assert_eq!(
        resolve_cursor(&desk.doc, &up, desk.pc1.id, Point::new(960, 540)),
        Resolution::Stay(Point::new(960, 540))
    );
}

#[test]
fn an_edge_with_no_machine_behind_it_blocks() {
    let desk = desk();
    let up = online([desk.pc1.id, desk.mac.id, desk.pc2.id]);

    // Nothing is configured to the right of PC1.
    assert_eq!(
        resolve_cursor(&desk.doc, &up, desk.pc1.id, Point::new(2000, 400)),
        Resolution::Stay(Point::new(1919, 400))
    );
}

#[test]
fn a_machine_off_the_path_is_not_a_crossing_target() {
    let desk = desk();
    let up = online([desk.pc1.id, desk.mac.id, desk.pc2.id]);

    // Heading left from the top of PC1. PC2 sits above, not to the left, so the
    // Mac is the only machine on this path.
    match resolve_cursor(&desk.doc, &up, desk.pc1.id, Point::new(-5, 10)) {
        Resolution::Move { machine, .. } => assert_eq!(machine, desk.mac.id),
        other => panic!("expected the Mac, got {other:?}"),
    }
}

#[test]
fn cursor_ownership_falls_back_to_an_online_machine() {
    let desk = desk();

    // The local machine is up: it keeps the cursor.
    let up = online([desk.pc1.id, desk.mac.id]);
    assert_eq!(
        fallback_owner(&desk.doc, &up, desk.pc1.id),
        Some(desk.pc1.id)
    );

    // The preferred machine is down: pick deterministically among the rest.
    let without_pc1 = online([desk.mac.id, desk.pc2.id]);
    let chosen = fallback_owner(&desk.doc, &without_pc1, desk.pc1.id);
    assert_eq!(chosen, Some(desk.mac.id.min(desk.pc2.id)));

    // Nothing is up at all.
    assert_eq!(
        fallback_owner(&desk.doc, &HashSet::new(), desk.pc1.id),
        None
    );
}

/// A machine with two monitors is one machine occupying one shape.
#[test]
fn a_machine_with_two_screens_covers_both_of_them() {
    let mut laptop = machine("Laptop", Point::new(0, 0));
    // A second monitor to the left of the built-in one, which is what makes the
    // machine's own origin and the corner of what it covers different.
    laptop.displays.push(is_core::DisplayInfo {
        id: "display-1".into(),
        name: "External".into(),
        x: -2560,
        y: -200,
        width: 2560,
        height: 1440,
        scale: 1.0,
        primary: false,
    });

    let bounds = laptop.bounds().expect("it has screens");
    assert_eq!(bounds.x, -2560, "the shape starts at the leftmost screen");
    assert_eq!(bounds.y, -200, "and at the topmost one");
    assert_eq!(bounds.max_x(), 1920, "and ends at the rightmost one");
    assert_eq!(
        bounds.max_y(),
        1240,
        "the taller external screen sets the bottom, not the built-in one"
    );

    let desk = machine("Desk", Point::new(2000, 0));
    let mut doc = WorkspaceDoc::create("Two screens", laptop.clone());
    doc.upsert_machine(laptop.id, desk.clone());
    let up = online([laptop.id, desk.id]);

    // A point on the second monitor still belongs to the laptop.
    assert_eq!(
        resolve_cursor(&doc, &up, laptop.id, Point::new(-1000, 500)),
        Resolution::Stay(Point::new(-1000, 500)),
        "the far monitor is part of the same machine, not a crossing"
    );

    // And leaving the whole shape on the right crosses to the other machine.
    match resolve_cursor(&doc, &up, laptop.id, Point::new(1925, 400)) {
        Resolution::Move { machine, .. } => assert_eq!(machine, desk.id),
        other => panic!("expected a crossing past the rightmost screen, got {other:?}"),
    }
}
