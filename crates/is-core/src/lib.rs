#![forbid(unsafe_code)]

//! Core domain of InputShare.
//!
//! This crate owns the part of the product that has to be right for the promise
//! in `docs/requirements.md`: a workspace that survives a full reboot of every
//! machine and converges to the same state on all of them.
//!
//! It deliberately knows nothing about sockets or the OS input stack:
//!
//! - [`identity`] — the permanent machine ID and signing key (requirement 1).
//! - [`stamp`] — Lamport stamps and last-writer-wins registers, the ordering
//!   primitive behind deterministic conflict resolution (requirements 9, 11).
//! - [`model`] — the synchronized workspace document and its merge.
//! - [`layout`] — cursor routing, including the offline-machine guard
//!   (requirement 8).
//! - [`store`] — crash-safe local persistence (requirements 2, 12).

pub mod error;
pub mod identity;
pub mod input;
pub mod layout;
pub mod model;
pub mod paths;
pub mod stamp;
pub mod store;

pub use error::{Error, Result};
pub use identity::{Identity, PubKey};
pub use input::{InputEvent, MouseButton};
pub use layout::{Direction, OfflineEdgeBehavior, Resolution};
pub use model::{
    DisplayInfo, InputAssignment, MachineRecord, MachineSlot, Op, Platform, Point, Rect, SlotState,
    WorkspaceDoc, WorkspaceSettings, WorkspaceUpdate,
};
pub use stamp::{Lww, Stamp};
pub use store::{Backup, LocalPrefs, PeerHint, PeerHints, Store};

use std::time::{SystemTime, UNIX_EPOCH};

/// Permanent identifier of an installation. Never derived from network state.
pub type MachineId = uuid::Uuid;

/// Identifier of a workspace, minted by whichever machine creates it.
pub type WorkspaceId = uuid::Uuid;

/// Wall-clock milliseconds since the Unix epoch.
///
/// Carried as metadata only. Ordering decisions use Lamport stamps, because a
/// machine that has been powered off for a week cannot be trusted to agree with
/// its peers about the time.
pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}
