//! The synchronized workspace document.
//!
//! One document describes the whole workspace: which machines belong to it,
//! where their displays sit relative to each other, which machines contribute
//! input devices, and the shared settings. Every machine keeps a full copy and
//! persists it, so the workspace is a shared distributed configuration rather
//! than three unrelated local ones (requirement 11).
//!
//! Two things follow from that, and they are the reason this module exists:
//!
//! - A machine being offline is not a change to the document. Peer liveness is
//!   runtime state and lives elsewhere (requirement 3).
//! - Reconnecting machines merge documents rather than overwrite them, so stale
//!   state can never clobber newer state (requirement 9).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::layout::OfflineEdgeBehavior;
use crate::{Error, Lww, MachineId, PubKey, Result, Stamp, WorkspaceId};

/// A point in workspace coordinates.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Point {
    pub x: i32,
    pub y: i32,
}

impl Point {
    pub fn new(x: i32, y: i32) -> Self {
        Self { x, y }
    }
}

/// An axis-aligned rectangle in workspace coordinates.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

impl Rect {
    pub fn new(x: i32, y: i32, width: u32, height: u32) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    pub fn max_x(&self) -> i32 {
        self.x.saturating_add(self.width as i32)
    }

    pub fn max_y(&self) -> i32 {
        self.y.saturating_add(self.height as i32)
    }

    /// Half-open on the far edges, so two adjacent rectangles never both claim
    /// the same pixel column.
    pub fn contains(&self, p: Point) -> bool {
        p.x >= self.x && p.x < self.max_x() && p.y >= self.y && p.y < self.max_y()
    }

    /// Nearest point inside the rectangle. A degenerate rectangle clamps to its
    /// origin rather than panicking.
    pub fn clamp(&self, p: Point) -> Point {
        let hi_x = self.max_x().saturating_sub(1).max(self.x);
        let hi_y = self.max_y().saturating_sub(1).max(self.y);
        Point {
            x: p.x.clamp(self.x, hi_x),
            y: p.y.clamp(self.y, hi_y),
        }
    }

    pub fn translate(self, by: Point) -> Rect {
        Rect {
            x: self.x.saturating_add(by.x),
            y: self.y.saturating_add(by.y),
            ..self
        }
    }

    pub fn union(self, other: Rect) -> Rect {
        let x = self.x.min(other.x);
        let y = self.y.min(other.y);
        let max_x = self.max_x().max(other.max_x());
        let max_y = self.max_y().max(other.max_y());
        Rect {
            x,
            y,
            width: (max_x - x).max(0) as u32,
            height: (max_y - y).max(0) as u32,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Platform {
    Windows,
    MacOs,
    Linux,
    Other,
}

impl Platform {
    pub fn current() -> Self {
        if cfg!(target_os = "windows") {
            Platform::Windows
        } else if cfg!(target_os = "macos") {
            Platform::MacOs
        } else if cfg!(target_os = "linux") {
            Platform::Linux
        } else {
            Platform::Other
        }
    }
}

/// One display, in the owning machine's local coordinates.
///
/// `scale` is carried because a 2x Mac display and a 1x PC display of the same
/// pixel size are not the same physical size, and cursor handoff across that
/// boundary looks wrong if the difference is ignored.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DisplayInfo {
    pub id: String,
    pub name: String,
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    pub scale: f32,
    pub primary: bool,
}

impl DisplayInfo {
    pub fn rect(&self) -> Rect {
        Rect::new(self.x, self.y, self.width, self.height)
    }
}

/// Which physical input devices a machine contributes to the workspace
/// (requirement 2).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct InputAssignment {
    pub provides_keyboard: bool,
    pub provides_mouse: bool,
}

/// A machine as the workspace knows it.
///
/// Note what is absent: no IP address, no hostname, no interface. Those change
/// when a laptop moves from Ethernet to Wi-Fi, and they belong to runtime state
/// rather than to the persisted workspace (requirement 1).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MachineRecord {
    pub id: MachineId,
    pub display_name: String,
    pub platform: Platform,
    /// Recorded at pairing time; peers authenticate against it on every
    /// reconnect.
    pub public_key: PubKey,
    /// Origin of this machine's display arrangement in workspace coordinates.
    pub position: Point,
    pub displays: Vec<DisplayInfo>,
    pub input: InputAssignment,
    pub paired_at_ms: u64,
}

impl MachineRecord {
    /// Bounding box of this machine's displays in workspace coordinates.
    ///
    /// `None` for a machine that has not reported its displays yet: it is in the
    /// workspace, but there is nowhere to route a cursor to.
    pub fn bounds(&self) -> Option<Rect> {
        self.displays
            .iter()
            .map(|d| d.rect().translate(self.position))
            .reduce(Rect::union)
    }
}

/// Whether a machine is in the workspace, or was explicitly removed from it.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SlotState {
    Present(MachineRecord),
    /// A tombstone. Unpairing has to outlive a merge with a peer that still
    /// remembers the machine, otherwise the removal would be undone the moment
    /// that peer reconnects.
    Removed,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MachineSlot {
    pub stamp: Stamp,
    pub state: SlotState,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WorkspaceSettings {
    pub clipboard_sync: bool,
    pub offline_edge: OfflineEdgeBehavior,
    pub discovery_port: u16,
    pub transport_port: u16,
    /// Interface names to prefer when several are usable. Empty means "any".
    pub preferred_interfaces: Vec<String>,
    pub autostart: bool,
}

impl Default for WorkspaceSettings {
    fn default() -> Self {
        Self {
            clipboard_sync: true,
            offline_edge: OfflineEdgeBehavior::Block,
            discovery_port: 47451,
            transport_port: 47452,
            preferred_interfaces: Vec::new(),
            autostart: true,
        }
    }
}

/// A single change to the workspace, carrying enough metadata to order it
/// against any other change (requirement 11).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WorkspaceUpdate {
    pub workspace_id: WorkspaceId,
    /// The document clock before this change. Lets a peer notice it has missed
    /// updates and ask for a full document instead of applying a delta onto a
    /// document it cannot place.
    pub prev_lamport: u64,
    pub stamp: Stamp,
    pub op: Op,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Op {
    SetName(String),
    UpsertMachine(MachineRecord),
    RemoveMachine(MachineId),
    SetSettings(WorkspaceSettings),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WorkspaceDoc {
    pub workspace_id: WorkspaceId,
    pub schema_version: u32,
    pub name: Lww<String>,
    /// Highest Lamport time this machine has seen. Local edits take
    /// `lamport + 1`, which is what makes them sort after everything known.
    pub lamport: u64,
    pub machines: BTreeMap<MachineId, MachineSlot>,
    pub settings: Lww<WorkspaceSettings>,
}

impl WorkspaceDoc {
    pub const SCHEMA_VERSION: u32 = 1;

    /// Creates a workspace whose first member is `founder`.
    pub fn create(name: impl Into<String>, founder: MachineRecord) -> Self {
        let origin = founder.id;
        let stamp = Stamp::new(1, origin);
        let mut machines = BTreeMap::new();
        machines.insert(
            origin,
            MachineSlot {
                stamp,
                state: SlotState::Present(founder),
            },
        );
        Self {
            workspace_id: Uuid::new_v4(),
            schema_version: Self::SCHEMA_VERSION,
            name: Lww::new(name.into(), stamp),
            lamport: 1,
            machines,
            settings: Lww::new(WorkspaceSettings::default(), stamp),
        }
    }

    /// Advances the clock and returns a stamp for a local change.
    pub fn tick(&mut self, origin: MachineId) -> Stamp {
        self.lamport += 1;
        Stamp::new(self.lamport, origin)
    }

    /// Pulls the clock past anything seen from a peer, so the next local edit
    /// sorts after it.
    fn observe(&mut self, lamport: u64) {
        self.lamport = self.lamport.max(lamport);
    }

    pub fn set_name(&mut self, origin: MachineId, name: impl Into<String>) -> WorkspaceUpdate {
        let prev_lamport = self.lamport;
        let stamp = self.tick(origin);
        let name = name.into();
        self.name = Lww::new(name.clone(), stamp);
        WorkspaceUpdate {
            workspace_id: self.workspace_id,
            prev_lamport,
            stamp,
            op: Op::SetName(name),
        }
    }

    pub fn set_settings(
        &mut self,
        origin: MachineId,
        settings: WorkspaceSettings,
    ) -> WorkspaceUpdate {
        let prev_lamport = self.lamport;
        let stamp = self.tick(origin);
        self.settings = Lww::new(settings.clone(), stamp);
        WorkspaceUpdate {
            workspace_id: self.workspace_id,
            prev_lamport,
            stamp,
            op: Op::SetSettings(settings),
        }
    }

    /// Adds a machine, or replaces what is known about one. Also the path a
    /// machine takes to publish its own display layout after a resolution
    /// change.
    pub fn upsert_machine(&mut self, origin: MachineId, record: MachineRecord) -> WorkspaceUpdate {
        let prev_lamport = self.lamport;
        let stamp = self.tick(origin);
        self.machines.insert(
            record.id,
            MachineSlot {
                stamp,
                state: SlotState::Present(record.clone()),
            },
        );
        WorkspaceUpdate {
            workspace_id: self.workspace_id,
            prev_lamport,
            stamp,
            op: Op::UpsertMachine(record),
        }
    }

    /// Unpairs a machine, leaving a tombstone behind.
    pub fn remove_machine(&mut self, origin: MachineId, id: MachineId) -> WorkspaceUpdate {
        let prev_lamport = self.lamport;
        let stamp = self.tick(origin);
        self.machines.insert(
            id,
            MachineSlot {
                stamp,
                state: SlotState::Removed,
            },
        );
        WorkspaceUpdate {
            workspace_id: self.workspace_id,
            prev_lamport,
            stamp,
            op: Op::RemoveMachine(id),
        }
    }

    /// Applies a peer's single update. Returns whether local state changed.
    pub fn apply(&mut self, update: WorkspaceUpdate) -> Result<bool> {
        self.check_workspace(update.workspace_id)?;
        self.observe(update.stamp.lamport);
        let stamp = update.stamp;
        Ok(match update.op {
            Op::SetName(name) => self.name.merge(Lww::new(name, stamp)),
            Op::SetSettings(settings) => self.settings.merge(Lww::new(settings, stamp)),
            Op::UpsertMachine(record) => {
                let id = record.id;
                self.merge_slot(
                    id,
                    MachineSlot {
                        stamp,
                        state: SlotState::Present(record),
                    },
                )
            }
            Op::RemoveMachine(id) => self.merge_slot(
                id,
                MachineSlot {
                    stamp,
                    state: SlotState::Removed,
                },
            ),
        })
    }

    /// Merges a peer's whole document — the reconnection path (requirement 9).
    ///
    /// Neither side is assumed to be authoritative. The result is the same
    /// whichever machine merges first, and merging twice changes nothing.
    pub fn merge(&mut self, other: &WorkspaceDoc) -> Result<bool> {
        self.check_workspace(other.workspace_id)?;
        self.observe(other.lamport);
        let mut changed = self.name.merge(other.name.clone());
        changed |= self.settings.merge(other.settings.clone());
        for (id, slot) in &other.machines {
            self.observe(slot.stamp.lamport);
            changed |= self.merge_slot(*id, slot.clone());
        }
        Ok(changed)
    }

    fn merge_slot(&mut self, id: MachineId, incoming: MachineSlot) -> bool {
        match self.machines.get_mut(&id) {
            Some(existing) if existing.stamp >= incoming.stamp => false,
            Some(existing) => {
                *existing = incoming;
                true
            }
            None => {
                self.machines.insert(id, incoming);
                true
            }
        }
    }

    fn check_workspace(&self, incoming: WorkspaceId) -> Result<()> {
        if incoming == self.workspace_id {
            Ok(())
        } else {
            Err(Error::WorkspaceMismatch {
                incoming,
                local: self.workspace_id,
            })
        }
    }

    pub fn machine(&self, id: MachineId) -> Option<&MachineRecord> {
        match self.machines.get(&id).map(|slot| &slot.state) {
            Some(SlotState::Present(record)) => Some(record),
            _ => None,
        }
    }

    /// Machines that belong to the workspace, online or not.
    pub fn machines_present(&self) -> impl Iterator<Item = &MachineRecord> + '_ {
        self.machines.values().filter_map(|slot| match &slot.state {
            SlotState::Present(record) => Some(record),
            SlotState::Removed => None,
        })
    }

    pub fn is_removed(&self, id: MachineId) -> bool {
        matches!(
            self.machines.get(&id).map(|slot| &slot.state),
            Some(SlotState::Removed)
        )
    }
}
