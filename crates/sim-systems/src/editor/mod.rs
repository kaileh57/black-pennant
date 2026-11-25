//! Contract-owned types (no work unit). The headless side of the editor: everything the dry dock computes
//! from a blueprint without Godot. Units: `gauges.rs` = `sys-gauges`, `notes.rs` = `sys-notes`,
//! `suggest.rs` = `sys-suggest`, `trials.rs` = `sys-trials`, `fitcheck.rs` = `sys-fitcheck`,
//! `quartermaster.rs` = `sys-quartermaster`, `splash.rs` = `sys-splash`.
//! DESIGN.md L118-276 (the editor), L801-807 (Fit Check, Quartermaster).
//!
//! The interactive editing model (handles, placement, selection, the single undo history) belongs to the
//! client; it calls these functions on its background worker after each edit or save. None of them reads a
//! clock: wall-time limits (gauge latency, Quartermaster's 300 ms and 5 s) are enforced by the caller through
//! [`Deadline`].
pub mod fitcheck;
pub mod gauges;
pub mod notes;
pub mod quartermaster;
pub mod splash;
pub mod suggest;
pub mod trials;

use crate::prelude::*;
use content::blueprint::PartPlacement;
use content::cell::CellPos;
use content::kit::PartRot;

/// Check state of a gauge or note (L168): Pass, Caution (triangle), Fail (cross). Green never means pass.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Check { #[default] Pass, Caution, Fail }

/// A mechanical change to a blueprint that a Ship's Note Fix, a Suggest button or the Quartermaster proposes.
/// The client previews it as a ghost before the player accepts (L188).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum BlueprintEdit {
    MovePart { slot: PartSlot, to: CellPos, rot: PartRot },
    AddPart(PartPlacement),
    RemovePart(PartSlot),
    AddKit { kit: KitId, pos: CellPos, rot: PartRot, mirrored: bool },
    RemoveKit(KitSlot),
    SwapKit { slot: KitSlot, kit: KitId },
    SetLoad { slot: PartSlot, load: f64 },
    /// Hull handle by stable name (`beam`, `draft`, `deadrise`, `flare`, `sheer`, `bow_rake`, `transom`) at a
    /// station index, to a value in metres or degrees.
    SetHullHandle { station: u8, handle: String, value: f64 },
    AddBulkhead { x_cell: i16 },
    PaintArmour { cells: Vec<CellPos>, tier: u8 },
    SetSlider { key: String, value: f64 },
    SetBurstTrigger { range_m: f64 },
    SetMountLimits { slot: PartSlot, limits: content::graph::MountLimits },
    UnpinNode(GraphNodeId),
    UnpinRoute { index: u16 },
    Several(Vec<BlueprintEdit>),
}

/// Caller-supplied wall-clock guard. The background worker implements it with a real clock; tests and CI use
/// a counter so results stay deterministic.
pub trait Deadline {
    /// Called between candidates and between simulated seconds; true = stop now and return the best so far.
    fn expired(&mut self) -> bool;
    /// Start timing one candidate (Quartermaster's per-candidate limit).
    fn start_candidate(&mut self) {}
    /// True when the current candidate has used its own limit.
    fn candidate_expired(&mut self) -> bool { false }
}
/// A deadline that never expires (CI and tests).
pub struct NoDeadline;
impl Deadline for NoDeadline { fn expired(&mut self) -> bool { false } }
