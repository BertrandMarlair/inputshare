//! Reading the real monitor layout.

use is_core::DisplayInfo;

use crate::{platform, Result};

/// This machine's monitors, in the coordinate space the OS uses for the whole
/// desktop: the primary monitor's top-left is the origin, and monitors placed
/// to its left or above it have negative coordinates.
///
/// That is exactly what `MachineRecord::displays` wants, so the workspace
/// topology ends up describing the screens the user actually has rather than one
/// assumed rectangle.
pub fn enumerate() -> Result<Vec<DisplayInfo>> {
    platform::enumerate_displays()
}

/// Whether two layouts differ in any way that matters to the workspace.
///
/// Used to avoid rewriting the document — and bumping its revision on every
/// peer — when a machine restarts with the monitors it already had.
pub fn differ(a: &[DisplayInfo], b: &[DisplayInfo]) -> bool {
    a.len() != b.len() || a.iter().zip(b).any(|(x, y)| x != y)
}
