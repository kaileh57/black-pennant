//! Unit `sim-core-structure` (hard: connectivity under a fixed work budget with deterministic carry-over).
//! Structural splitting, pieces and wrecks. DESIGN.md L682-699.
//!
//! When a cell is destroyed its vessel is marked dirty. At the end of the tick a flood fill seeded from the
//! removed cells' neighbours runs over face-adjacent solid cells; parts disconnected from the root cell split
//! into their own floating body. The root cell is the keel cell nearest the design centre of mass, fixed at
//! spawn. If the root is destroyed, the piece with the most cells keeps the vessel id (ties: lowest cell
//! index). New pieces get body ids ascending by lowest cell index. Pieces under 64 cells leave the simulation
//! as client debris. Larger pieces are wrecks with their own compartments and buoyancy. A wreck despawns when
//! fully submerged more than 20 m deep or after 600 s; at most 16 wrecks per sortie, oldest removed first.
//! Work budget: [`crate::time::SPLIT_WORK_BUDGET`] cells visited per tick for splitting plus compartment
//! recompute, shared across vessels in ascending vessel id; leftover work carries to the next tick in the same
//! order. FALLBACK noted for later (L697-699): if the backlog gate fails, vessels tracked by any player-side
//! sensor are processed before untracked ones.
//!
//! Implementation notes (binding for the world unit that drives these functions):
//! - A job takes every cell of `Vessel::pending_destroyed` when it starts. Cells destroyed while a job is in
//!   progress wait in `pending_destroyed` for the next job, so the fill never chases a moving target: the job
//!   treats them as still solid (seeding, the root test and the fill), so it answers for the grid it started
//!   on and the next job, which takes them, starts from a hull that was one body. Pieces list live cells only.
//! - A job runs in three phases: compartment recompute (`CompartmentSet::update_after_damage` on the job's
//!   cells, so the world's own call in phase 11 finds nothing left to do), seeding (each live face-neighbour of
//!   a destroyed cell starts a fill), and one multi-source breadth-first fill over live solid cells in which
//!   fills that meet are merged (union-find). A fill whose frontier empties without touching the root is a
//!   piece. While the root is alive, the job ends once at most one fill is still growing and it is the root's
//!   fill or no fill has reached the root yet: the hull was one body before the job, so the last growing fill
//!   is the hull. With the root destroyed, the job ends once the one growing fill already holds more cells than
//!   any finished fill; that fill keeps the vessel id and the root moves to its keel cell nearest the design
//!   centre of mass. If every fill finished and none reached a live root, the largest fill keeps the id only
//!   when it holds more cells than the rest of the hull; otherwise all of them split off.
//! - The failure mode of a hull that was not one body at job start (a disconnected design) is a missed split,
//!   never the main body emitted as a piece.
//! - Pieces are reported only when a job completes, ascending by lowest cell index. Debris pieces (under
//!   `StructureTuning::min_piece_cells`) are detached from the vessel inside `step_split` (cells removed without
//!   change events, parts wholly inside wrecked and emptied, compartments inside dropped, the fracture opened to
//!   the sea, conduits through them cut); the caller only reports them. Wreck pieces stay in the grid until the
//!   caller passes each one to [`carve_piece`]; `step_split` returns as soon as a job yields one, so the caller
//!   carves before any further splitting runs.
//! - Work units: one per destroyed cell seeded, one per seed started, one per cell popped from the fill, one
//!   per debris cell detached, plus the cells visited by the compartment recompute.
pub(crate) mod carve;
pub(crate) mod split;
#[cfg(test)]
mod tests;

use crate::entity::EntityId;
use crate::vessel::Vessel;
use content::ids::CellIndex;
use serde::{Deserialize, Serialize};

/// Resumable connectivity work for one vessel, kept across ticks when the budget runs out.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SplitJob {
    /// Seeds still to flood from, ascending.
    pub seeds: Vec<CellIndex>,
    /// Frontier and visited state of the fill in progress (implementation-defined encoding, serialisable so a
    /// keyframe can carry it).
    ///
    /// Encoding: `frontier[head..]` is the breadth-first queue shared by every fill. Its entries are packed
    /// coordinates `x | y << 9 | z << 18`, not cell indices, so the fill never divides. `visited` marks every
    /// cell (by index) labelled by a fill; it is sized to the grid while a fill runs and empty otherwise.
    pub frontier: Vec<CellIndex>,
    pub visited: crate::bits::BitSet,
    pub in_progress: bool,
    /// Destroyed cells this job processes, ascending.
    #[serde(default)]
    pub(crate) destroyed: Vec<CellIndex>,
    /// 0 = compartment recompute, 1 = seeding, 2 = filling.
    #[serde(default)]
    pub(crate) phase: u8,
    /// Read position in `frontier`.
    #[serde(default)]
    pub(crate) head: u32,
    /// Fill label + 1 per cell (0 = unvisited). The buffer is kept zeroed between jobs and serialised sparsely.
    #[serde(default, with = "split::sparse_labels")]
    pub(crate) labels: Vec<u32>,
    #[serde(default)]
    pub(crate) fills: Vec<split::Fill>,
    /// Fills whose frontier is non-empty (union-find roots only).
    #[serde(default)]
    pub(crate) active: u32,
    /// Fills whose frontier emptied, in the order they finished.
    #[serde(default)]
    pub(crate) done: Vec<u32>,
    /// Largest cell count of a finished fill.
    #[serde(default)]
    pub(crate) done_max: u32,
    /// Whether the root cell was live when the job started seeding.
    #[serde(default)]
    pub(crate) root_alive: bool,
    /// Fill that reached the root cell, `u32::MAX` while none has (0 in a default job, which never reads it).
    #[serde(default = "split::no_fill")]
    pub(crate) root_fill: u32,
}

/// One piece found disconnected from the root.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Piece {
    /// Cells of the piece in ascending index (indices in the parent's grid).
    pub cells: Vec<CellIndex>,
    /// True when the piece is under the minimum size and becomes client debris only.
    pub debris: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SplitResult {
    /// Pieces ascending by lowest cell index. Empty when the vessel is still one body.
    pub pieces: Vec<Piece>,
    /// True when the root was lost and the largest piece kept the vessel id (its cells stay; `pieces` lists the
    /// others).
    pub rerooted: Option<CellIndex>,
    pub cells_visited: u32,
    /// Work remains for the next tick.
    pub unfinished: bool,
}

/// Run splitting for one vessel within `budget` cells. Consumes `vessel.pending_destroyed` into the job.
/// Does not create new vessels; the world does that from the result so entity allocation stays in one place.
/// Unit: `sim-core-structure`.
///
/// Also runs the compartment recompute for the job's cells, detaches debris pieces itself and sets
/// `vessel.root_cell` on a reroot (see the module notes). Returns as soon as a job yields a wreck piece: carve
/// every wreck piece with [`carve_piece`], then call again with the budget left while `unfinished`. The debris
/// threshold is `StructureTuning::default().min_piece_cells` (64, L686): the signature carries no tuning table.
/// `cells_visited` can exceed `budget` by one compartment merge or one debris detach.
pub fn step_split(vessel: &mut Vessel, job: &mut SplitJob, budget: u32) -> SplitResult { split::step(vessel, job, budget) }

/// Carve a piece out of a parent into a new wreck vessel sharing the parent's design grid frame (same cell
/// indices, so replicated split events are cell sets plus a new body id, L920): moves cells, parts wholly
/// inside, compartments and their water; recomputes mass, cuboids and blocks for both.
/// Unit: `sim-core-structure`.
///
/// The wreck has status `Wreck`, no player, no crew or networks, `body == BodyHandle::NONE` (the world adds
/// its physics body) and `spawn_tick` copied from the parent: the caller must set `spawn_tick` to the current
/// tick, which wreck despawning counts from. Parts cut across the fracture stay with the parent at reduced
/// health; the wreck carries a wrecked, empty copy so their mass splits by cells. Compartment ids are shared:
/// the wreck's compartment list has the parent's length, with empty shells for compartments it does not hold,
/// and the parent keeps an empty shell for each compartment that moved. A compartment the fracture cuts is
/// split between both, its water shared by volume and both halves open to the sea along the cut. Both bodies'
/// velocities are centre-of-mass velocities after the split (the parent's `kin.lin_vel` is updated for its
/// shifted centre of mass; the caller pushes it and the new mass to the physics body).
///
/// Cost: not counted against the split budget and not resumable; it is proportional to the vessel's grid
/// (about 6 ms for half of a 90 m hull on the development Mac), so carving at most one wreck per tick keeps a
/// break-up from stacking.
pub fn carve_piece(parent: &mut Vessel, piece: &Piece, new_id: EntityId, catalogue: &content::catalogue::Catalogue) -> Vessel { carve::carve_piece(parent, piece, new_id, catalogue) }

/// Pick the root cell: the keel cell nearest the design centre of mass. Unit: `sim-core-structure`.
///
/// Keel cells are the lowest live cell of each (x, z) column, counting only columns whose lowest cell is
/// structure (not part body) unless no column's is. Nearest is measured in plan (x, z) so the root sits
/// on the keel under the centre of mass; ties go to the lower cell, then the lower index. A non-finite centre
/// of mass uses the grid centre. A grid with no live cell returns `CellIndex(0)`, which reads as not solid.
pub fn pick_root_cell(grid: &crate::grid::VesselGrid, com_local: glam::DVec3) -> CellIndex { split::pick_root(grid, com_local, None).unwrap_or(CellIndex(0)) }

/// Wrecks to despawn this tick: submerged deeper than 20 m, older than 600 s, or beyond the cap of 16 (oldest
/// first). Unit: `sim-core-structure`.
///
/// Each entry is (wreck id, spawn tick, depth in metres of the wreck's highest point below the local sea
/// surface, positive under water). Age saturates at zero for a spawn tick in the future; a NaN depth counts as
/// not submerged; a non-finite TTL or depth limit disables that rule. Wrecks kept by those rules beyond
/// `max_wrecks` go oldest first (lowest spawn tick, then lowest id). Duplicate ids count once. The result is
/// ascending by id.
pub fn wrecks_to_despawn(wrecks: &[(EntityId, u64, f64)], tick: u64, tuning: &content::tuning::StructureTuning) -> Vec<EntityId> {
    use std::collections::BTreeSet;
    let ttl_ticks = (tuning.wreck_ttl_s.is_finite() && tuning.wreck_ttl_s >= 0.0).then(|| libm::ceil(tuning.wreck_ttl_s * crate::time::TICK_HZ as f64).min(u64::MAX as f64) as u64);
    let depth_limit = tuning.wreck_despawn_depth_m;
    let mut seen: BTreeSet<EntityId> = BTreeSet::new();
    let mut out: BTreeSet<EntityId> = BTreeSet::new();
    let mut kept: Vec<(u64, EntityId)> = Vec::new();
    for &(id, spawn, depth) in wrecks {
        if !seen.insert(id) { continue; }
        let age = tick.saturating_sub(spawn);
        let deep = depth_limit.is_finite() && depth > depth_limit;
        let old = ttl_ticks.is_some_and(|t| age >= t);
        if deep || old { out.insert(id); } else { kept.push((spawn, id)); }
    }
    kept.sort_unstable();
    let cap = tuning.max_wrecks as usize;
    if kept.len() > cap {
        for &(_, id) in &kept[..kept.len() - cap] { out.insert(id); }
    }
    out.into_iter().collect()
}
