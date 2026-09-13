//! Cursor routing across the workspace, and the offline-machine guard.
//!
//! The topology is geometric: each machine occupies a rectangle in workspace
//! coordinates, and the cursor crosses to whichever machine owns the space it
//! is heading into. The topology is part of the persisted document, so it
//! survives a machine going offline (requirement 3).
//!
//! Routing, however, is computed against the set of machines that are online
//! right now. That separation is the whole point of requirement 8: keep the
//! configured layout, but never hand the cursor to a machine that cannot take
//! it — otherwise the pointer vanishes and the user has no way to get it back.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};

use crate::{MachineId, Point, Rect, WorkspaceDoc};

/// What to do when the cursor reaches an edge leading to an offline machine.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OfflineEdgeBehavior {
    /// Treat the offline machine as a wall: the cursor stops at the boundary.
    /// The default, because it is the behavior the user can always recover
    /// from, and because it keeps the geometry honest — the machine really is
    /// over there, it just cannot accept the cursor.
    Block,
    /// Treat the offline machine as temporarily non-existent and hand the
    /// cursor to the next machine beyond it along the same direction.
    SkipOver,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    Left,
    Right,
    Up,
    Down,
}

impl Direction {
    fn is_horizontal(self) -> bool {
        matches!(self, Direction::Left | Direction::Right)
    }
}

/// Where the cursor ends up.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Resolution {
    /// The cursor stays on the machine it is already on, at this point.
    Stay(Point),
    /// The cursor hands off, entering `machine` at `point`.
    Move { machine: MachineId, point: Point },
}

/// A machine whose rectangle lies along the ray leaving the current machine.
#[derive(Clone, Copy, Debug)]
struct Candidate {
    machine: MachineId,
    bounds: Rect,
    /// Gap between the exit point and this machine's near edge, in pixels.
    distance: i32,
}

/// Resolves a cursor move that would leave the current machine.
///
/// `target` is where the accumulated mouse motion wants to put the cursor, in
/// workspace coordinates. `online` is the set of machines with a live
/// authenticated connection; the current machine is expected to be in it.
pub fn resolve_cursor(
    doc: &WorkspaceDoc,
    online: &HashSet<MachineId>,
    current: MachineId,
    target: Point,
) -> Resolution {
    let Some(bounds) = doc.machine(current).and_then(|m| m.bounds()) else {
        // The current machine has not reported displays, so there is no
        // geometry to route against. Leave the cursor alone.
        return Resolution::Stay(target);
    };

    if bounds.contains(target) {
        return Resolution::Stay(target);
    }

    let exit = bounds.clamp(target);
    let Some(direction) = direction_of(exit, target) else {
        return Resolution::Stay(exit);
    };

    let candidates = candidates_along(doc, current, exit, direction);
    let chosen = match doc.settings.value.offline_edge {
        OfflineEdgeBehavior::Block => match candidates.first() {
            // The nearest machine in that direction is offline: stop at the
            // boundary rather than sending the cursor somewhere it cannot come
            // back from.
            Some(c) if !online.contains(&c.machine) => None,
            other => other.copied(),
        },
        OfflineEdgeBehavior::SkipOver => candidates
            .iter()
            .find(|c| online.contains(&c.machine))
            .copied(),
    };

    match chosen {
        Some(c) => Resolution::Move {
            machine: c.machine,
            point: entry_point(c.bounds, exit, direction),
        },
        None => Resolution::Stay(exit),
    }
}

/// The machine that should hold the cursor when its current owner is not
/// available — after that machine goes offline, or on startup.
///
/// Prefers `preferred` (normally the local machine, which by definition can
/// always display a cursor) and otherwise takes the lowest-ID online machine so
/// that every peer independently picks the same one.
pub fn fallback_owner(
    doc: &WorkspaceDoc,
    online: &HashSet<MachineId>,
    preferred: MachineId,
) -> Option<MachineId> {
    if online.contains(&preferred) && doc.machine(preferred).is_some() {
        return Some(preferred);
    }
    doc.machines_present()
        .map(|m| m.id)
        .filter(|id| online.contains(id))
        .min()
}

/// Dominant axis of travel out of the current machine. `None` when the target
/// is not actually outside, which the caller has already ruled out.
fn direction_of(exit: Point, target: Point) -> Option<Direction> {
    let dx = target.x - exit.x;
    let dy = target.y - exit.y;
    if dx == 0 && dy == 0 {
        return None;
    }
    if dx.abs() >= dy.abs() {
        Some(if dx > 0 {
            Direction::Right
        } else {
            Direction::Left
        })
    } else {
        Some(if dy > 0 {
            Direction::Down
        } else {
            Direction::Up
        })
    }
}

/// Machines lying on the ray from `exit` along `direction`, nearest first.
///
/// A machine only counts if the cursor would actually run into it: its
/// perpendicular span has to cover the exit point. A monitor stacked above the
/// one the cursor is leaving from is not on the path.
fn candidates_along(
    doc: &WorkspaceDoc,
    current: MachineId,
    exit: Point,
    direction: Direction,
) -> Vec<Candidate> {
    let mut found: Vec<Candidate> = doc
        .machines_present()
        .filter(|m| m.id != current)
        .filter_map(|m| {
            let bounds = m.bounds()?;
            let distance = match direction {
                Direction::Right => {
                    (exit.y >= bounds.y && exit.y < bounds.max_y() && bounds.x > exit.x)
                        .then(|| bounds.x - exit.x)
                }
                Direction::Left => {
                    (exit.y >= bounds.y && exit.y < bounds.max_y() && bounds.max_x() <= exit.x)
                        .then(|| exit.x - bounds.max_x())
                }
                Direction::Down => {
                    (exit.x >= bounds.x && exit.x < bounds.max_x() && bounds.y > exit.y)
                        .then(|| bounds.y - exit.y)
                }
                Direction::Up => {
                    (exit.x >= bounds.x && exit.x < bounds.max_x() && bounds.max_y() <= exit.y)
                        .then(|| exit.y - bounds.max_y())
                }
            }?;
            Some(Candidate {
                machine: m.id,
                bounds,
                distance,
            })
        })
        .collect();

    // Machine ID breaks distance ties so every peer resolves a crossing the
    // same way when two machines are configured at the same offset.
    found.sort_by_key(|c| (c.distance, c.machine));
    found
}

/// Where the cursor appears on the machine it crosses into: just inside the
/// near edge, keeping its position along the other axis.
fn entry_point(bounds: Rect, exit: Point, direction: Direction) -> Point {
    let point = if direction.is_horizontal() {
        let x = match direction {
            Direction::Right => bounds.x,
            _ => bounds.max_x().saturating_sub(1),
        };
        Point::new(x, exit.y)
    } else {
        let y = match direction {
            Direction::Down => bounds.y,
            _ => bounds.max_y().saturating_sub(1),
        };
        Point::new(exit.x, y)
    };
    bounds.clamp(point)
}
