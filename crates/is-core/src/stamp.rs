//! Lamport stamps and last-writer-wins registers.
//!
//! Every synchronized value in the workspace carries a [`Stamp`]. Merging two
//! values is "keep the one with the higher stamp", and because stamps are
//! totally ordered, every machine reaches the same answer regardless of the
//! order updates arrive in.

use std::cmp::Ordering;

use serde::{Deserialize, Serialize};

use crate::MachineId;

/// A Lamport counter tagged with the machine that produced it.
///
/// Ordering is `(lamport, origin)` and nothing else. `wall_clock_ms` is kept for
/// display and audit; it never participates in comparison, because clocks on
/// machines that boot hours apart are not a reliable ordering.
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct Stamp {
    pub lamport: u64,
    pub origin: MachineId,
    pub wall_clock_ms: u64,
}

impl Stamp {
    pub fn new(lamport: u64, origin: MachineId) -> Self {
        Self {
            lamport,
            origin,
            wall_clock_ms: crate::now_ms(),
        }
    }

    /// The comparison key. Machine ID breaks ties so that two concurrent edits
    /// at the same logical time resolve the same way everywhere.
    fn key(&self) -> (u64, [u8; 16]) {
        (self.lamport, *self.origin.as_bytes())
    }
}

impl PartialEq for Stamp {
    fn eq(&self, other: &Self) -> bool {
        self.key() == other.key()
    }
}

impl Eq for Stamp {}

impl Ord for Stamp {
    fn cmp(&self, other: &Self) -> Ordering {
        self.key().cmp(&other.key())
    }
}

impl PartialOrd for Stamp {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// A value plus the stamp that last wrote it.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Lww<T> {
    pub value: T,
    pub stamp: Stamp,
}

impl<T> Lww<T> {
    pub fn new(value: T, stamp: Stamp) -> Self {
        Self { value, stamp }
    }

    /// Adopts `incoming` if it is newer. Returns whether `self` changed.
    ///
    /// Equal stamps mean the same write seen twice, so the incoming copy is
    /// dropped and the merge stays idempotent.
    pub fn merge(&mut self, incoming: Lww<T>) -> bool {
        if incoming.stamp > self.stamp {
            *self = incoming;
            true
        } else {
            false
        }
    }
}
