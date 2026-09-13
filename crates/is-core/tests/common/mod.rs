#![allow(dead_code)]

//! Fixtures shared by the integration tests.

use is_core::{
    DisplayInfo, Identity, InputAssignment, MachineRecord, Platform, Point, WorkspaceDoc,
};

pub fn display(width: u32, height: u32) -> DisplayInfo {
    DisplayInfo {
        id: "display-0".into(),
        name: "Built-in".into(),
        x: 0,
        y: 0,
        width,
        height,
        scale: 1.0,
        primary: true,
    }
}

/// A machine with one 1920x1080 display, positioned at `position`.
pub fn machine(name: &str, position: Point) -> MachineRecord {
    let identity = Identity::generate();
    MachineRecord {
        id: identity.machine_id,
        display_name: name.into(),
        platform: Platform::current(),
        public_key: identity.public_key(),
        position,
        displays: vec![display(1920, 1080)],
        input: InputAssignment {
            provides_keyboard: true,
            provides_mouse: true,
        },
        paired_at_ms: 0,
    }
}

/// Comparable form of a document, for asserting that two replicas converged.
pub fn snapshot(doc: &WorkspaceDoc) -> serde_json::Value {
    serde_json::to_value(doc).expect("workspace document serializes")
}
