//! Contract-owned types (no work unit). Expert network edits stored in a blueprint (DESIGN.md L299).
//! Routes themselves are never stored: the deterministic router in sim-core recomputes them from the design.
use crate::cell::CellPos;
use crate::enums::NetworkKind;
use crate::ids::PartSlot;
use serde::{Deserialize, Serialize};

/// A route the player pinned; it survives regeneration. If a design change breaks it, it becomes a Ship's Note.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PinnedRoute { pub network: NetworkKind, pub from: PartSlot, pub to: PartSlot, pub cells: Vec<CellPos> }

/// A breaker splitting a network into zones at one conduit cell.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Breaker { pub network: NetworkKind, pub cell: CellPos, pub closed: bool }

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct NetworkEdits {
    pub pinned_routes: Vec<PinnedRoute>,
    pub breakers: Vec<Breaker>,
    /// Explicit extra Data links between two parts.
    pub data_links: Vec<(PartSlot, PartSlot)>,
}
impl NetworkEdits { pub fn is_empty(&self) -> bool { self.pinned_routes.is_empty() && self.breakers.is_empty() && self.data_links.is_empty() } }
