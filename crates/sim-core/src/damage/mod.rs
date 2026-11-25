//! Unit `sim-core-damage`. Applying cell damage to a vessel and everything that follows directly from it.
//! DESIGN.md L561 (buckets, part health, destroyed cells), L626 (breaches), L662 (ignition), L394 (conduits).
//!
//! Order of one application (both entry points share it):
//! 1. The damage list is sanitised: sorted by cell, duplicate entries summed (saturating at 8), zero entries
//!    and cells outside the grid dropped. Input from persistent damage or the network never panics.
//! 2. For [`apply_hit`] with `can_ignite`, fuel and ammunition proximity is read *before* any bucket lands, so a
//!    hit that destroys the tank it struck still counts it.
//! 3. Buckets go on cell by cell in ascending cell order through `VesselGrid::add_damage`, which records the
//!    replicated [`content::cell::CellChange`] stream. A cell counts as destroyed by this application only when
//!    its bucket went from below 8 to 8.
//! 4. Burst scorch, part health (`parts::refresh_health`), conduit cuts (`networks::cut_conduits`), breaches and
//!    holes, patch reopening, `pending_destroyed`, then the ignition roll.
//!
//! # Implementation decisions
//! - Openings. The authoritative compartment recompute is `CompartmentSet::update_after_damage` in tick phase
//!   11, under the shared split budget; this unit never writes `cell_to_compartment`. So that flooding (phase 10)
//!   sees a hole the same tick, the removed cells are classified here with exactly the rules of that recompute,
//!   on a private label overlay: a removed cell touching a compartment and the sea (outside the box, exterior
//!   air, exterior equipment, or an already open cell) is a breach cell with one 0.25 m^2 opening per face it
//!   shares with a compartment; a removed cell touching only compartments becomes their air, with a breach on
//!   each open neighbour's face toward it; a removed cell joining two or more compartments adds an
//!   [`OpeningKind::Hole`] (`a` the lowest id, `b` the other, face toward `a`, 0.25 m^2) for each joined pair,
//!   which the phase-11 merge then drops. Compartments joined by a pending hole (this or an earlier
//!   application) count as one, filed under the lowest id, so every breach added here has the key the
//!   recompute would give it and is never doubled. The overlay covers the connected cluster of removed cells no
//!   recompute has placed yet around this application's destroyed cells (at most [`MAX_CLUSTER`] cells), taken
//!   in ascending index, with walled-in cells re-tested when a neighbour opens, as the recompute does.
//! - Breaches clear the compartment's `sealed` flag (the recompute does the same and refreshes the sealed-air
//!   bits).
//! - Patch reopening. Breach cells are removed cells and no hit deals damage to a removed cell, so "further
//!   damage to the cell" (L646) is read as damage landing on the opening's cell or on any cell face-adjacent to
//!   it, i.e. the plating round a patched hole. Any bucket from this application reopens the patch, and its
//!   compartment is reported in `breached`.
//! - Ignition ("its compartment", L662). The hit's reference cell is the burst cell, or else the affected cell
//!   that took the most buckets (lowest index on ties). The hit's compartment is the compartment nearest that
//!   cell within the ignition radius (Euclidean; cells destroyed by this hit count as the air they became),
//!   lowest cell index on equal distance. With none in reach (a burst deep inside a large tank or magazine)
//!   it falls back to the compartment of the part owning the flammable cell that set the chance; a part in no
//!   compartment (exterior) cannot ignite one. The chance is
//!   `ignite_near_ammo` when any affected cell lies within `ignite_radius_cells` of a live ammunition cell, else
//!   `ignite_near_fuel` near a fuel cell. The draw is `Stream::FireIgnition` keyed (seed, tick, vessel id,
//!   compartment id) with the reference cell index as the draw number, so two hits on one compartment in one
//!   tick roll independently. Lighting goes through `fire::ignite_in` with the tuned flooded fraction.
//! - Scorch. Hits that burst (and explosive causes passed to [`apply_cells`]) add [`SCORCH_PER_BUCKET`] per
//!   bucket to each surviving damaged cell and mark its render chunk dirty.
//! - Collision damage (spec silent; private constants below). The impulse first pays for a velocity change of
//!   [`COLLISION_DV_MIN_MPS`] of the vessel's own mass, so resting or grinding contact (about g x dt per tick)
//!   does nothing. The rest buys buckets at [`COLLISION_NS_PER_BUCKET`] N s each, armour cells costing
//!   `1 + thickness_mm / 10` times as much. Buckets spread from the live cell nearest the contact point with an
//!   HE-like profile `min(8, ceil(16 (1 - d / (r + 1))))`, at the largest radius r (at most
//!   [`COLLISION_R_MAX`] cells) the budget pays for, plus a proportional share of the next radius, so damage
//!   grows smoothly with impulse. A 150 t craft ramming at 10 m/s crushes roughly a 1.5 m ball of its bow, a
//!   gentle bump scratches a cell or nothing, and the heavier party in a ram takes less.
use crate::armour::{CellDamage, HitResult};
use crate::compartments::{opposite, CompartmentSet, Dims, Opening, OpeningKind, MAX_COMPARTMENTS, NO_COMPARTMENT, OPEN_CELL, VOID_CELL};
use crate::entity::EntityId;
use crate::grid::VesselGrid;
use crate::networks::Networks;
use crate::parts::PartRuntime;
use crate::rng::{chance, Key, Stream};
use crate::vessel::Vessel;
use crate::world::StepContext;
use content::cell::{ArmourTier, CellPos, Face, RenderChunkCoord, BUCKET_DESTROYED, CELL_FACE_M2, CELL_M};
use content::ids::{CellIndex, CompartmentId, PartSlot};
use content::part::Flammable;
use content::tuning::FireTuning;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[cfg(test)]
mod tests;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum DamageCause { Shell, Missile, Torpedo, Mine, DepthCharge, CookOff, Collision, Grounding, Fire }

/// What one application changed, for events and follow-up systems.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct DamageOutcome {
    /// Cells that reached bucket 8 this application, ascending. Also appended to `Vessel::pending_destroyed`.
    pub destroyed: Vec<CellIndex>,
    /// Parts whose damage level changed, ascending slot.
    pub parts_changed: Vec<PartSlot>,
    pub conduits_cut: Vec<u16>,
    /// Compartments that gained a breach or hole.
    pub breached: Vec<CompartmentId>,
    /// Compartment set alight by this hit, if the ignition draw succeeded.
    pub ignited: Option<CompartmentId>,
}

/// Apply a hit result to a vessel: add buckets cell by cell in ascending cell order, remove destroyed cells,
/// refresh part health, cut conduits through destroyed cells, add breaches and holes, reopen patched faces whose
/// cell took damage, and roll ignition once per hit: 15% when any affected cell is within 2 cells of a fuel
/// cell, 25% within 2 cells of an ammunition cell, keyed (seed, tick, vessel, compartment) on
/// `rng::Stream::FireIgnition` (L660-662). A compartment over 50% flooded cannot ignite.
/// Unit: `sim-core-damage`.
pub fn apply_hit(vessel: &mut Vessel, result: &HitResult, cause: DamageCause, attacker: EntityId, ctx: &StepContext<'_>) -> DamageOutcome {
    let ign = result.can_ignite.then_some(IgnitionCtx { seed: ctx.seed, tick: ctx.tick.0, vessel: vessel.id.to_u64(), fire: &ctx.tuning.fire, flooded: ctx.tuning.flooding.flooded_fraction });
    apply_inner(io_of(vessel), &result.damage, result.burst.is_some(), ign, result.burst)
}

/// Apply raw cell damage with no ignition roll (collision, grounding, cook-off blast, persistent damage at
/// spawn). Unit: `sim-core-damage`.
pub fn apply_cells(vessel: &mut Vessel, damage: &[CellDamage], cause: DamageCause, ctx: &StepContext<'_>) -> DamageOutcome {
    apply_inner(io_of(vessel), damage, explosive(cause), None, None)
}

/// Turn a contact impulse into cell damage at the contact point (ramming, grounding). Unit: `sim-core-damage`.
///
/// `impulse` is the contact impulse magnitude in N s for this step. See the module notes for the model.
/// Returns one entry per cell, ascending; empty for non-finite input or an impulse under the threshold.
pub fn collision_damage(vessel: &Vessel, world_point: glam::DVec3, impulse: f64) -> Vec<CellDamage> {
    if !world_point.is_finite() { return Vec::new(); }
    // The larger of the two, so a mass not yet refreshed (default 1 kg) cannot collapse the threshold.
    let mass = [vessel.mass.mass_kg, vessel.dry_mass.mass_kg].into_iter().filter(|m| m.is_finite() && *m > 0.0).fold(0.0, f64::max);
    let Some(budget) = collision_budget(mass, impulse) else { return Vec::new() };
    let local = vessel.kin.pose.to_local(world_point) / CELL_M;
    if !local.is_finite() { return Vec::new(); }
    crush(&vessel.grid, [local.x, local.y, local.z], budget)
}

// ---------------------------------------------------------------------------------------------------------
// Constants.

/// Scorch added to a surviving cell per bucket of burst damage.
pub(crate) const SCORCH_PER_BUCKET: u8 = 16;
/// Most removed cells the opening overlay examines around one application.
pub(crate) const MAX_CLUSTER: usize = 8_192;
/// Velocity change of the vessel's own mass the contact impulse must exceed before it damages anything, m/s.
pub(crate) const COLLISION_DV_MIN_MPS: f64 = 0.5;
/// Impulse per bucket of crush damage to an unarmoured cell, N s.
pub(crate) const COLLISION_NS_PER_BUCKET: f64 = 4_000.0;
/// Peak of the crush profile (buckets at the centre before the clamp to 8).
const COLLISION_PEAK: f64 = 16.0;
/// Largest crush radius in cells (4 m).
pub(crate) const COLLISION_R_MAX: i32 = 8;
/// Extra crush cost per millimetre of armour, as a multiple of the unarmoured cost.
const COLLISION_ARMOUR_PER_MM: f64 = 0.1;
/// Largest ignition radius honoured, whatever the tuning says (keeps the proximity scan bounded).
const MAX_IGNITE_RADIUS: u8 = 4;

fn explosive(cause: DamageCause) -> bool {
    matches!(cause, DamageCause::Shell | DamageCause::Missile | DamageCause::Torpedo | DamageCause::Mine | DamageCause::DepthCharge | DamageCause::CookOff)
}

// ---------------------------------------------------------------------------------------------------------
// Shared application.

/// The parts of a vessel one application touches (split out so tests run on hand-built fixtures).
pub(crate) struct DamageIo<'a> {
    pub grid: &'a mut VesselGrid,
    pub parts: &'a mut [PartRuntime],
    pub compartments: &'a mut CompartmentSet,
    pub networks: &'a mut Networks,
    pub pending: &'a mut Vec<CellIndex>,
}

fn io_of(v: &mut Vessel) -> DamageIo<'_> {
    DamageIo { grid: &mut v.grid, parts: &mut v.parts, compartments: &mut v.compartments, networks: &mut v.networks, pending: &mut v.pending_destroyed }
}

/// Keys and rules for the ignition roll of one hit.
#[derive(Clone, Copy)]
pub(crate) struct IgnitionCtx<'a> { pub seed: u64, pub tick: u64, pub vessel: u64, pub fire: &'a FireTuning, pub flooded: f64 }

/// Sort by cell, sum duplicates (saturating at 8), drop zero entries and cells outside the grid.
fn sanitise(grid: &VesselGrid, damage: &[CellDamage]) -> Vec<CellDamage> {
    let n = grid.damage.len().min(grid.design.cells.len());
    let mut map: BTreeMap<u32, u8> = BTreeMap::new();
    for d in damage {
        if d.buckets == 0 || d.cell.index() >= n { continue; }
        let e = map.entry(d.cell.0).or_insert(0);
        *e = e.saturating_add(d.buckets).min(BUCKET_DESTROYED);
    }
    map.into_iter().map(|(c, b)| CellDamage { cell: CellIndex(c), buckets: b }).collect()
}

pub(crate) fn apply_inner(io: DamageIo<'_>, damage: &[CellDamage], scorch: bool, ignition: Option<IgnitionCtx<'_>>, burst: Option<CellIndex>) -> DamageOutcome {
    let DamageIo { grid, parts, compartments, networks, pending } = io;
    let mut out = DamageOutcome::default();
    let damage = sanitise(grid, damage);
    if damage.is_empty() { return out; }

    // Ignition inputs, read on the grid as it was before the hit.
    let flame = ignition.and_then(|ig| {
        let (p, source) = proximity_chance(grid, &damage, ig.fire)?;
        let reference = reference_cell(grid, &damage, burst)?;
        Some((ig, p, reference, source))
    });

    // Buckets, ascending.
    let mut changed: Vec<CellIndex> = Vec::with_capacity(damage.len());
    let mut destroyed: Vec<CellIndex> = Vec::new();
    for d in &damage {
        let before = grid.bucket(d.cell);
        let after = grid.add_damage(d.cell, d.buckets);
        if after == before { continue; }
        changed.push(d.cell);
        if after >= BUCKET_DESTROYED && before < BUCKET_DESTROYED { destroyed.push(d.cell); }
    }
    if changed.is_empty() { return out; }

    if scorch { add_scorch(grid, &damage); }

    out.parts_changed = crate::parts::refresh_health(parts, grid, &changed);
    out.parts_changed.sort_unstable();
    out.parts_changed.dedup();
    out.conduits_cut = crate::networks::cut_conduits(networks, &destroyed);
    out.conduits_cut.sort_unstable();
    out.conduits_cut.dedup();

    let mut breached: BTreeSet<u16> = BTreeSet::new();
    let overlay = add_openings(grid, compartments, &destroyed, &mut breached);
    reopen_patches(grid, compartments, &changed, &mut breached);
    out.breached = breached.into_iter().map(CompartmentId).collect();

    if !destroyed.is_empty() {
        pending.extend_from_slice(&destroyed);
        pending.sort_unstable();
        pending.dedup();
    }

    if let Some((ig, p, reference, source)) = flame {
        let radius = ig.fire.ignite_radius_cells.clamp(1, MAX_IGNITE_RADIUS);
        let near = hit_compartment(grid, compartments, &overlay, reference, radius);
        let fallback = || source.and_then(|s| crate::compartments::find_part(parts, s)).and_then(|p| p.compartment).filter(|c| is_comp(compartments, c.0));
        if let Some(c) = near.or_else(fallback) {
            let key = Key::new(ig.seed, ig.tick, Stream::FireIgnition, ig.vessel, c.0 as u64).draw(reference.0);
            if chance(key, p) && crate::fire::ignite_in(compartments, c, ig.fire, ig.flooded) { out.ignited = Some(c); }
        }
    }
    out.destroyed = destroyed;
    out
}

// ---------------------------------------------------------------------------------------------------------
// Scorch.

fn add_scorch(grid: &mut VesselGrid, damage: &[CellDamage]) {
    let n = grid.design.cells.len();
    if n == 0 || grid.damage.len() != n { return; }
    if grid.scorch.len() != n { grid.scorch = vec![0; n]; }
    let mut dirty: BTreeSet<RenderChunkCoord> = BTreeSet::new();
    for d in damage {
        let p = grid.pos(d.cell);
        if !grid.is_solid(p) { continue; }
        let add = (d.buckets as u16 * SCORCH_PER_BUCKET as u16).min(u8::MAX as u16) as u8;
        let s = &mut grid.scorch[d.cell.index()];
        if *s < u8::MAX && add > 0 { *s = s.saturating_add(add); dirty.insert(RenderChunkCoord::of(p)); }
    }
    grid.dirty_render.extend(dirty);
}

// ---------------------------------------------------------------------------------------------------------
// Ignition.

/// Ignition chance from fuel and ammunition near the affected cells, on the pre-hit grid, with the part owning
/// the first flammable cell that set it; `None` when no flammable cell is near or the chance is zero.
fn proximity_chance(grid: &VesselGrid, damage: &[CellDamage], t: &FireTuning) -> Option<(f64, Option<PartSlot>)> {
    if !grid.types.iter().any(|ty| ty.flammable != Flammable::No) { return None; }
    let offsets = sphere_offsets(t.ignite_radius_cells.min(MAX_IGNITE_RADIUS) as i32);
    let mut seen: BTreeSet<u32> = BTreeSet::new();
    let mut fuel: Option<Option<PartSlot>> = None;
    for d in damage {
        let p = grid.pos(d.cell);
        for &(dx, dy, dz, _) in &offsets {
            let q = CellPos::new(p.x + dx as i16, p.y + dy as i16, p.z + dz as i16);
            let Some(i) = grid.index(q) else { continue };
            if !seen.insert(i.0) { continue; }
            let ty = grid.cell_type(q);
            match ty.flammable {
                Flammable::Ammunition => return prob(t.ignite_near_ammo).map(|p| (p, ty.part)),
                Flammable::Fuel => { if fuel.is_none() { fuel = Some(ty.part); } }
                Flammable::No => {}
            }
        }
    }
    fuel.and_then(|part| prob(t.ignite_near_fuel).map(|p| (p, part)))
}

fn prob(p: f64) -> Option<f64> { (p.is_finite() && p > 0.0).then(|| p.min(1.0)) }

/// Integer offsets within Euclidean radius `r` with their squared length, sorted by (length, y, z, x).
fn sphere_offsets(r: i32) -> Vec<(i32, i32, i32, i32)> {
    let mut v = Vec::new();
    for dy in -r..=r { for dz in -r..=r { for dx in -r..=r {
        let d2 = dx * dx + dy * dy + dz * dz;
        if d2 <= r * r { v.push((dx, dy, dz, d2)); }
    } } }
    v.sort_by_key(|&(dx, dy, dz, d2)| (d2, dy, dz, dx));
    v
}

/// The burst cell, else the affected cell with the most buckets (lowest index on ties).
fn reference_cell(grid: &VesselGrid, damage: &[CellDamage], burst: Option<CellIndex>) -> Option<CellIndex> {
    if let Some(b) = burst { if b.index() < grid.damage.len() { return Some(b); } }
    let mut best: Option<CellDamage> = None;
    for d in damage { if best.is_none_or(|b| d.buckets > b.buckets) { best = Some(*d); } }
    best.map(|d| d.cell)
}

/// Compartment nearest the reference cell within `radius` cells (see the module notes).
fn hit_compartment(grid: &VesselGrid, comps: &CompartmentSet, overlay: &BTreeMap<u32, u16>, reference: CellIndex, radius: u8) -> Option<CompartmentId> {
    let n = comps.cell_to_compartment.len();
    if n != grid.damage.len() || reference.index() >= n { return None; }
    let p = grid.pos(reference);
    let mut best: Option<(i32, u32, u16)> = None;
    for (dx, dy, dz, d2) in sphere_offsets(radius as i32) {
        if best.is_some_and(|b| b.0 < d2) { break; }
        let q = CellPos::new(p.x + dx as i16, p.y + dy as i16, p.z + dz as i16);
        let Some(i) = grid.index(q) else { continue };
        let lab = overlay.get(&i.0).copied().unwrap_or(comps.cell_to_compartment[i.index()]);
        if !is_comp(comps, lab) { continue; }
        if best.is_none_or(|b| (d2, i.0) < (b.0, b.1)) { best = Some((d2, i.0, lab)); }
    }
    best.map(|b| CompartmentId(b.2))
}

// ---------------------------------------------------------------------------------------------------------
// Openings.

fn is_comp(comps: &CompartmentSet, lab: u16) -> bool { (lab as usize) < MAX_COMPARTMENTS && (lab as usize) < comps.compartments.len() }

/// Sort key of an opening, identical to the compartments unit's order (a, cell, face, b, kind).
fn opening_key(o: &Opening) -> (u16, u32, u8, u32, u8) {
    let kind = match o.kind { OpeningKind::Breach => 0, OpeningKind::Hole => 1, OpeningKind::Door(_) => 2, OpeningKind::FloodValve => 3 };
    (o.a.0, o.cell.0, o.face as u8, o.b.map_or(u32::MAX, |b| b.0 as u32), kind)
}

/// Label overlay over `cell_to_compartment`, plus the union of compartments joined by pending holes.
struct Overlay<'a> {
    d: Dims,
    c2c: &'a [u16],
    /// Compartments in the set; labels at or above are not ids.
    ncomp: usize,
    labels: BTreeMap<u32, u16>,
    parent: BTreeMap<u16, u16>,
    voids: Vec<usize>,
}
impl Overlay<'_> {
    fn label(&self, i: usize) -> u16 { self.labels.get(&(i as u32)).copied().unwrap_or(self.c2c[i]) }
    fn comp(&self, lab: u16) -> bool { (lab as usize) < MAX_COMPARTMENTS && (lab as usize) < self.ncomp }
    fn root(&self, mut c: u16) -> u16 {
        // Parents always point to a lower id, so this terminates.
        while let Some(&p) = self.parent.get(&c) { if p >= c { break; } c = p; }
        c
    }
    fn union(&mut self, a: u16, b: u16) {
        let (ra, rb) = (self.root(a), self.root(b));
        if ra != rb { self.parent.insert(ra.max(rb), ra.min(rb)); }
    }
    fn set_label(&mut self, x: usize, lab: u16) {
        self.labels.insert(x as u32, lab);
        for f in Face::ALL { if let Some(j) = self.d.step(x, f) { if self.label(j) == VOID_CELL { self.voids.push(j); } } }
    }
}

/// Openings collected by one application.
struct NewOpenings { keys: BTreeSet<(u16, u32, u8, u32, u8)>, list: Vec<Opening>, unsealed: BTreeSet<u16> }
impl NewOpenings {
    fn push(&mut self, o: Opening) { if self.keys.insert(opening_key(&o)) { self.list.push(o); } }
    fn breach(&mut self, a: u16, cell: usize, face: Face) {
        self.push(Opening { a: CompartmentId(a), b: None, cell: CellIndex(cell as u32), face, area_m2: CELL_FACE_M2, kind: OpeningKind::Breach, patched: false, open: true });
        self.unsealed.insert(a);
    }
    fn hole(&mut self, a: u16, b: u16, cell: usize, face: Face) {
        self.push(Opening { a: CompartmentId(a), b: Some(CompartmentId(b)), cell: CellIndex(cell as u32), face, area_m2: CELL_FACE_M2, kind: OpeningKind::Hole, patched: false, open: true });
    }
}

/// Add the breaches and holes the destroyed cells open (see the module notes). Returns the overlay labels of
/// the removed cells it classified.
fn add_openings(grid: &VesselGrid, comps: &mut CompartmentSet, destroyed: &[CellIndex], breached: &mut BTreeSet<u16>) -> BTreeMap<u32, u16> {
    let d = Dims::new(grid.size());
    let n = d.len();
    if destroyed.is_empty() || n == 0 || comps.cell_to_compartment.len() != n || grid.design.cells.len() != n || grid.damage.len() != n { return BTreeMap::new(); }
    let removed = |i: usize| !grid.is_solid(d.pos(i)) && !grid.design.cells[i].is_empty();
    let (labels, new) = {
        let c2c: &[u16] = &comps.cell_to_compartment;
        // Connected cluster of removed cells no recompute has placed yet.
        let unplaced = |i: usize| removed(i) && matches!(c2c[i], NO_COMPARTMENT | VOID_CELL);
        let mut cluster: BTreeSet<u32> = BTreeSet::new();
        let mut stack: Vec<usize> = Vec::new();
        for c in destroyed { if c.index() < n && cluster.insert(c.0) { stack.push(c.index()); } }
        'flood: while let Some(x) = stack.pop() {
            for f in Face::ALL {
                let Some(j) = d.step(x, f) else { continue };
                if cluster.len() >= MAX_CLUSTER { break 'flood; }
                if unplaced(j) && cluster.insert(j as u32) { stack.push(j); }
            }
        }
        let mut ov = Overlay { d, c2c, ncomp: comps.compartments.len(), labels: BTreeMap::new(), parent: BTreeMap::new(), voids: Vec::new() };
        for o in &comps.openings {
            if o.kind == OpeningKind::Hole { if let Some(b) = o.b { if ov.comp(o.a.0) && ov.comp(b.0) { ov.union(o.a.0, b.0); } } }
        }
        let mut new = NewOpenings { keys: comps.openings.iter().map(opening_key).collect(), list: Vec::new(), unsealed: BTreeSet::new() };
        // The recompute takes removed, unlabelled cells in ascending order, and walled-in cells only when a
        // neighbour opens.
        for &x in &cluster {
            let x = x as usize;
            if c2c[x] != NO_COMPARTMENT || !removed(x) || ov.labels.contains_key(&(x as u32)) { continue; }
            classify(grid, &mut ov, &mut new, breached, x);
            while let Some(v) = ov.voids.pop() {
                if ov.label(v) == VOID_CELL { classify(grid, &mut ov, &mut new, breached, v); }
            }
        }
        (ov.labels, new)
    };
    for &a in &new.unsealed { if let Some(c) = comps.compartments.get_mut(a as usize) { c.sealed = false; } }
    if !new.list.is_empty() {
        comps.openings.extend(new.list);
        comps.openings.sort_by_key(opening_key);
    }
    labels
}

/// Classify one removed cell on the overlay, by the recompute's rules.
fn classify(grid: &VesselGrid, ov: &mut Overlay<'_>, new: &mut NewOpenings, breached: &mut BTreeSet<u16>, x: usize) {
    let d = ov.d;
    if ov.comp(ov.label(x)) { return; }
    let mut touch: Vec<(u16, Face)> = Vec::new();
    let mut sea = false;
    let mut open_nb: Vec<(usize, Face)> = Vec::new();
    for f in Face::ALL {
        let Some(j) = d.step(x, f) else { sea = true; continue };
        let lab = ov.label(j);
        if ov.comp(lab) { touch.push((ov.root(lab), f)); continue; }
        match lab {
            OPEN_CELL => open_nb.push((j, f)),
            VOID_CELL => {}
            _ => {
                let p = d.pos(j);
                if grid.is_solid(p) { if !grid.is_watertight(p) { sea = true; } } else if grid.design.cells[j].is_empty() { sea = true; }
                // A removed neighbour not yet classified counts as wall until it is classified itself.
            }
        }
    }
    if touch.is_empty() {
        if sea || !open_nb.is_empty() { ov.set_label(x, OPEN_CELL); } else { ov.labels.insert(x as u32, VOID_CELL); }
        return;
    }
    let mut ids: Vec<u16> = touch.iter().map(|c| c.0).collect();
    ids.sort_unstable();
    ids.dedup();
    let target = ids[0];
    let toward_target = touch.iter().find(|c| c.0 == target).map_or(Face::XNeg, |c| c.1);
    for &b in &ids[1..] {
        new.hole(target, b, x, toward_target);
        ov.union(target, b);
        breached.insert(b);
        breached.insert(target);
    }
    if sea {
        ov.set_label(x, OPEN_CELL);
        for &(_, f) in &touch { new.breach(target, x, f); }
        breached.insert(target);
    } else {
        ov.set_label(x, target);
        for &(j, f) in &open_nb { new.breach(target, j, opposite(f)); }
        if !open_nb.is_empty() { breached.insert(target); }
    }
}

/// Reopen patched openings whose cell, or a cell face-adjacent to it, took damage this application.
fn reopen_patches(grid: &VesselGrid, comps: &mut CompartmentSet, changed: &[CellIndex], breached: &mut BTreeSet<u16>) {
    if changed.is_empty() || !comps.openings.iter().any(|o| o.patched) { return; }
    let d = Dims::new(grid.size());
    let n = d.len();
    for o in comps.openings.iter_mut().filter(|o| o.patched) {
        let c = o.cell.index();
        if c >= n { continue; }
        let hit = changed.binary_search(&o.cell).is_ok() || Face::ALL.iter().any(|&f| d.step(c, f).is_some_and(|j| changed.binary_search(&CellIndex(j as u32)).is_ok()));
        if hit { o.patched = false; breached.insert(o.a.0); }
    }
}

// ---------------------------------------------------------------------------------------------------------
// Collision.

/// Impulse left to spend on crush damage after the threshold, or `None` when it buys nothing.
pub(crate) fn collision_budget(mass_kg: f64, impulse: f64) -> Option<f64> {
    if !impulse.is_finite() || impulse <= 0.0 { return None; }
    let m = if mass_kg.is_finite() && mass_kg > 0.0 { mass_kg } else { 0.0 };
    let budget = impulse - m * COLLISION_DV_MIN_MPS;
    (budget >= COLLISION_NS_PER_BUCKET).then_some(budget)
}

fn crush_cost(armour: ArmourTier) -> f64 { COLLISION_NS_PER_BUCKET * (1.0 + armour.thickness_mm() * COLLISION_ARMOUR_PER_MM) }

/// Buckets at squared distance `d2` for crush radius `r`.
fn crush_profile(d2: i32, r: i32) -> u8 {
    if d2 > r * r { return 0; }
    let frac = 1.0 - libm::sqrt(d2 as f64) / (r as f64 + 1.0);
    libm::ceil(COLLISION_PEAK * frac - 1e-9).clamp(1.0, BUCKET_DESTROYED as f64) as u8
}

/// Spread `budget` N s of crush damage from the live cell nearest `at` (vessel cell units). Ascending output.
pub(crate) fn crush(grid: &VesselGrid, at: [f64; 3], budget: f64) -> Vec<CellDamage> {
    let s = grid.size();
    let n = s[0] as usize * s[1] as usize * s[2] as usize;
    if n == 0 || grid.design.cells.len() != n || grid.damage.len() != n || !at.iter().all(|v| v.is_finite()) || budget.is_nan() || budget <= 0.0 { return Vec::new(); }
    let dims = [s[0] as i64, s[1] as i64, s[2] as i64];
    let reach = COLLISION_R_MAX as i64 + 1;
    let base = [libm::floor(at[0]), libm::floor(at[1]), libm::floor(at[2])];
    // The contact must lie within reach of the grid box.
    for a in 0..3 { if base[a] < -(reach as f64) || base[a] > (dims[a] + reach) as f64 { return Vec::new(); } }
    let b = [base[0] as i64, base[1] as i64, base[2] as i64];
    let lo = |a: usize| (b[a] - reach).max(0);
    let hi = |a: usize| (b[a] + reach).min(dims[a] - 1);
    let mut centre: Option<(f64, u32, CellPos)> = None;
    for y in lo(1)..=hi(1) { for z in lo(2)..=hi(2) { for x in lo(0)..=hi(0) {
        let p = CellPos::new(x as i16, y as i16, z as i16);
        if !grid.is_solid(p) { continue; }
        let Some(i) = grid.index(p) else { continue };
        let dd = [x as f64 + 0.5 - at[0], y as f64 + 0.5 - at[1], z as f64 + 0.5 - at[2]];
        let d2 = dd[0] * dd[0] + dd[1] * dd[1] + dd[2] * dd[2];
        if d2 > (reach * reach) as f64 { continue; }
        if centre.is_none_or(|c| (d2, i.0) < (c.0, c.1)) { centre = Some((d2, i.0, p)); }
    } } }
    let Some((_, _, c)) = centre else { return Vec::new() };
    // Live cells within the largest radius: (index, squared distance, cost per bucket), nearest first.
    let mut cells: Vec<(u32, i32, f64)> = Vec::new();
    for (dx, dy, dz, d2) in sphere_offsets(COLLISION_R_MAX) {
        let q = CellPos::new(c.x + dx as i16, c.y + dy as i16, c.z + dz as i16);
        if !grid.is_solid(q) { continue; }
        let Some(i) = grid.index(q) else { continue };
        cells.push((i.0, d2, crush_cost(grid.cell_type(q).armour)));
    }
    let cost = |r: i32| -> f64 { cells.iter().map(|&(_, d2, k)| crush_profile(d2, r) as f64 * k).sum() };
    let mut out: BTreeMap<u32, u8> = BTreeMap::new();
    let c0 = cost(0);
    if c0 > budget {
        // Only the contact cell, partly.
        let Some(&(i, _, k)) = cells.first() else { return Vec::new() };
        let buckets = libm::floor(budget / k).clamp(0.0, BUCKET_DESTROYED as f64) as u8;
        if buckets > 0 { out.insert(i, buckets); }
    } else {
        let mut r = 0;
        let mut cr = c0;
        let mut next = cost(1);
        while r < COLLISION_R_MAX && next <= budget {
            r += 1;
            cr = next;
            next = if r < COLLISION_R_MAX { cost(r + 1) } else { next };
        }
        let (frac, r1) = if r < COLLISION_R_MAX && next > cr { (((budget - cr) / (next - cr)).clamp(0.0, 1.0), r + 1) } else { (0.0, r) };
        for &(i, d2, _) in &cells {
            let p0 = crush_profile(d2, r);
            let p1 = crush_profile(d2, r1);
            let extra = libm::floor(frac * p1.saturating_sub(p0) as f64) as u8;
            let total = p0.saturating_add(extra).min(BUCKET_DESTROYED);
            if total > 0 { out.insert(i, total); }
        }
    }
    out.into_iter().map(|(c, b)| CellDamage { cell: CellIndex(c), buckets: b }).collect()
}
