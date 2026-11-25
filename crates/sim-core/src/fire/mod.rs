//! Unit `sim-core-fire`. Fire spread, burn-down, suppression and magazine cook-off. DESIGN.md L660-678.
//!
//! Draws use the counter-based RNG keyed (sortie seed, tick, vessel id, compartment id, draw kind).
//! Spread: each burning compartment ignites each adjacent compartment (including across decks) at 1% per
//! second, halved across a closed bulkhead. Intensity starts at 100 and never grows. A compartment over 50%
//! flooded cannot burn. Every part in a burning compartment loses 1% health per second; structural cells do
//! not burn. A magazine in a burning compartment cooks off after 120 s unless flooded, destroying every cell
//! within r = clamp(4 + floor(cbrt(stored ammunition kg)), 4, 24) cells of the magazine centroid.
//! Each assigned team removes 2 intensity per second.
//!
//! Implementation notes:
//! - Order per tick, compartments ascending: put out fires in compartments over the flooded fraction or
//!   holding a flooded magazine; apply suppression; burn parts down and run magazine cook-off timers; then
//!   spread from the compartments that were burning at the start of the tick and still are, so a fire lit this
//!   tick spreads from the next one. The spread draw is `Stream::FireSpread` keyed (vessel id, target
//!   compartment) with the source compartment as the draw number; the chance per tick is 1 - (1 - p)^dt.
//! - "Closed bulkhead": a bulkhead or deck with a door between the two compartments where every such door is
//!   shut halves the chance. A plain bulkhead or deck, or an open door, passes fire at the full rate. This
//!   keeps the L676 arithmetic (1% per second into the magazine compartment next door) and makes shutting
//!   doors the player's fire break.
//! - Suppression per team is `suppression_per_team` divided by the crew's berth factor, the same scaling as
//!   every other damage-control job; with no surviving berth no team works.
//! - Burn-down lowers part health directly (cells are untouched; structural cells never burn); the damage
//!   level follows the bucket scale and only zero health is Destroyed.
//! - Timers count base ticks (`time::DT`), so a hull stepping at 30 Hz burns as fast as one at 60 Hz. A
//!   magazine's timer runs while its compartment burns and resets when the fire goes out. An empty magazine
//!   (no rounds and no ammunition mass) does not cook off. A cook-off empties the magazine and destroys the
//!   part, so it reports once.
//! - Scorch: once per simulated second each burning compartment adds 8 to the scorch of the solid cells around
//!   its air (saturating at 255) and marks their render chunks dirty.
//! - `ignite` only has `FireTuning`; it uses the spec's 50% line ([`NO_FIRE_FILL`]). `step_fire` uses the
//!   tuning table's `flooding.flooded_fraction`.
use crate::compartments::{find_part, CompartmentSet};
use crate::crew::level_from_health;
use crate::grid::VesselGrid;
use crate::parts::{PartRuntime, PartState};
use crate::rng::{chance, Key, Stream};
use crate::time::DT;
use crate::vessel::Vessel;
use crate::world::StepContext;
use content::cell::{CellPos, Face, RenderChunkCoord};
use content::enums::DamageLevel;
use content::ids::{CompartmentId, PartSlot};
use content::tuning::FireTuning;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[cfg(test)]
mod tests;

/// Fill fraction above which a compartment cannot burn (L664), for callers without the tuning table.
pub const NO_FIRE_FILL: f64 = 0.5;
/// Scorch added per simulated second of burning.
const SCORCH_PER_S: u8 = 8;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct FireOutcome {
    pub ignited: Vec<CompartmentId>,
    pub extinguished: Vec<CompartmentId>,
    /// Magazines that cooked off this tick with their blast radius in cells; the caller applies the blast
    /// through `damage::apply_cells`.
    pub cooked_off: Vec<(PartSlot, u8)>,
    pub parts_changed: Vec<PartSlot>,
}

/// Start a fire in a compartment at intensity 100 unless it is over 50% flooded or already burning.
/// Returns whether it lit. Unit: `sim-core-fire`.
pub fn ignite(vessel: &mut Vessel, compartment: CompartmentId, tuning: &content::tuning::FireTuning) -> bool {
    ignite_in(&mut vessel.compartments, compartment, tuning, NO_FIRE_FILL)
}

pub(crate) fn ignite_in(comps: &mut CompartmentSet, compartment: CompartmentId, tuning: &FireTuning, flooded: f64) -> bool {
    let Some(c) = comps.get_mut(compartment) else { return false };
    if c.is_absorbed() || c.is_burning() || c.fill_fraction() > flooded { return false; }
    let start = if tuning.start_intensity.is_finite() && tuning.start_intensity > 0.0 { tuning.start_intensity } else { 100.0 };
    c.fire = start;
    c.fire_ticks = 0;
    true
}

/// One tick of fire for one vessel, compartments ascending. `teams_on[c]` is the number of damage-control
/// teams fighting fire in compartment c this tick. Unit: `sim-core-fire`.
pub fn step_fire(vessel: &mut Vessel, teams_on: &[(CompartmentId, u8)], ctx: &StepContext<'_>) -> FireOutcome {
    let io = FireIo { comps: &mut vessel.compartments, parts: &mut vessel.parts, grid: &mut vessel.grid, berth_factor: vessel.crew.berth_factor() };
    let keys = FireKeys { seed: ctx.seed, tick: ctx.tick.0, vessel: vessel.id.to_u64() };
    fire_tick(io, teams_on, keys, &ctx.tuning.fire, ctx.tuning.flooding.flooded_fraction, ctx.dt, ctx.time_s)
}

pub(crate) struct FireIo<'a> {
    pub comps: &'a mut CompartmentSet,
    pub parts: &'a mut [PartRuntime],
    pub grid: &'a mut VesselGrid,
    pub berth_factor: Option<f64>,
}

#[derive(Clone, Copy)]
pub(crate) struct FireKeys { pub seed: u64, pub tick: u64, pub vessel: u64 }

/// Base ticks one step of `dt` seconds stands for.
fn base_ticks(dt: f64) -> u32 { if dt.is_finite() && dt > 0.0 { libm::round(dt / DT).clamp(1.0, 600.0) as u32 } else { 0 } }

fn per_team_rate(t: &FireTuning, factor: Option<f64>) -> f64 {
    let base = if t.suppression_per_team.is_finite() { t.suppression_per_team.max(0.0) } else { 0.0 };
    match factor { Some(f) if f.is_finite() && f > 0.0 => base / f, _ => 0.0 }
}

pub(crate) fn fire_tick(io: FireIo<'_>, teams_on: &[(CompartmentId, u8)], keys: FireKeys, t: &FireTuning, flooded: f64, dt: f64, time_s: f64) -> FireOutcome {
    let FireIo { comps, parts, grid, berth_factor } = io;
    let mut out = FireOutcome::default();
    let dt = if dt.is_finite() && dt > 0.0 { dt } else { 0.0 };
    let flooded = if flooded.is_finite() { flooded } else { NO_FIRE_FILL };
    let ticks = base_ticks(dt);
    comps.sync_part_compartments(parts);
    let rate = per_team_rate(t, berth_factor);
    let mut teams: BTreeMap<u16, u32> = BTreeMap::new();
    for &(c, n) in teams_on { *teams.entry(c.0).or_insert(0) += n as u32; }
    let burning_at_start: Vec<u16> = comps.compartments.iter().filter(|c| c.is_burning()).map(|c| c.id.0).collect();
    let mut changed: BTreeSet<PartSlot> = BTreeSet::new();

    for &ci in &burning_at_start {
        let i = ci as usize;
        let mag_flooded = comps.compartments[i].parts.iter().any(|&s| find_part(parts, s).is_some_and(|p| matches!(p.state, PartState::Magazine { flooded: true, .. })));
        let c = &mut comps.compartments[i];
        let mut out_now = c.is_absorbed() || c.fill_fraction() > flooded || mag_flooded;
        if !out_now {
            let n = teams.get(&ci).copied().unwrap_or(0) as f64;
            c.fire -= n * rate * dt;
            if !(c.fire > 0.0) { out_now = true; }
        }
        if out_now {
            c.fire = 0.0;
            c.fire_ticks = 0;
            out.extinguished.push(c.id);
            let slots = c.parts.clone();
            for s in slots {
                if let Some(p) = part_mut(parts, s) { if let PartState::Magazine { cookoff_ticks, .. } = &mut p.state { *cookoff_ticks = 0; } }
            }
            continue;
        }
        c.fire_ticks = c.fire_ticks.saturating_add(ticks);
        // Burn-down and cook-off.
        let slots = c.parts.clone();
        for s in slots {
            let Some(p) = part_mut(parts, s) else { continue };
            if p.works() {
                let burn = if t.burn_health_per_s.is_finite() { t.burn_health_per_s.max(0.0) } else { 0.0 };
                p.health = (p.health - burn * dt).max(0.0);
                let lvl = level_from_health(p.health).max(p.level);
                if lvl != p.level { p.level = lvl; changed.insert(s); }
            }
            if let PartState::Magazine { rounds, ammo_kg, flooded: false, cookoff_ticks } = &mut p.state {
                if *rounds == 0 && !(*ammo_kg > 0.0) { *cookoff_ticks = 0; continue; }
                *cookoff_ticks = cookoff_ticks.saturating_add(ticks);
                let delay = if t.cookoff_delay_s.is_finite() { t.cookoff_delay_s.max(0.0) } else { 120.0 };
                if *cookoff_ticks as f64 * DT + 1e-9 >= delay {
                    let r = cookoff_radius_cells(if ammo_kg.is_finite() { *ammo_kg } else { 0.0 }, t.cookoff_r_min_cells, t.cookoff_r_max_cells.max(t.cookoff_r_min_cells));
                    out.cooked_off.push((s, r));
                    *rounds = 0;
                    *ammo_kg = 0.0;
                    *cookoff_ticks = 0;
                    p.health = 0.0;
                    if p.level != DamageLevel::Destroyed { p.level = DamageLevel::Destroyed; changed.insert(s); }
                }
            }
        }
    }

    // Spread from fires that burned all tick.
    let p_s = if t.spread_per_s.is_finite() { t.spread_per_s.clamp(0.0, 1.0) } else { 0.0 };
    let mut lit: BTreeSet<u16> = BTreeSet::new();
    for &ci in &burning_at_start {
        if !comps.compartments[ci as usize].is_burning() { continue; }
        let src = CompartmentId(ci);
        for nb in comps.neighbours(src) {
            let Some(k) = comps.get(nb) else { continue };
            if k.is_burning() || k.is_absorbed() || k.fill_fraction() > flooded || lit.contains(&nb.0) { continue; }
            let closed = comps.adjacency.iter().any(|a| ((a.a == src && a.b == nb) || (a.a == nb && a.b == src)) && a.has_door) && !comps.open_door_between(src, nb);
            let p = if closed { p_s * 0.5 } else { p_s };
            let p_tick = 1.0 - libm::pow(1.0 - p, dt);
            let key = Key::new(keys.seed, keys.tick, Stream::FireSpread, keys.vessel, nb.0 as u64).draw(ci as u32);
            if chance(key, p_tick) && ignite_in(comps, nb, t, flooded) { lit.insert(nb.0); }
        }
    }
    out.ignited = lit.into_iter().map(CompartmentId).collect();

    // Scorch once per simulated second.
    if dt > 0.0 && time_s.is_finite() && libm::floor(time_s + dt) > libm::floor(time_s) {
        scorch(comps, grid);
    }
    out.extinguished.sort_unstable();
    out.parts_changed = changed.into_iter().collect();
    out
}

fn part_mut(parts: &mut [PartRuntime], s: PartSlot) -> Option<&mut PartRuntime> {
    let i = if parts.get(s.index()).is_some_and(|p| p.slot == s) { Some(s.index()) } else { parts.iter().position(|p| p.slot == s) }?;
    parts.get_mut(i)
}

fn scorch(comps: &CompartmentSet, grid: &mut VesselGrid) {
    let n = grid.design.cells.len();
    if n == 0 || grid.damage.len() != n { return; }
    if grid.scorch.len() != n { grid.scorch = vec![0; n]; }
    let mut dirty: BTreeSet<RenderChunkCoord> = BTreeSet::new();
    for c in comps.compartments.iter().filter(|c| c.is_burning()) {
        for &cell in &c.cells {
            if cell.index() >= n { continue; }
            let p = grid.pos(cell);
            for f in Face::ALL {
                let (dx, dy, dz) = f.offset();
                let q = CellPos::new(p.x + dx, p.y + dy, p.z + dz);
                let Some(j) = grid.index(q) else { continue };
                if !grid.is_solid(q) { continue; }
                let s = &mut grid.scorch[j.index()];
                if *s < u8::MAX { *s = s.saturating_add(SCORCH_PER_S); dirty.insert(RenderChunkCoord::of(q)); }
            }
        }
    }
    grid.dirty_render.extend(dirty);
}

/// Cook-off blast radius in cells for a stored ammunition mass (L666): 1,800 kg gives 16.
pub fn cookoff_radius_cells(ammo_kg: f64, min: u8, max: u8) -> u8 { (4.0 + libm::floor(libm::cbrt(ammo_kg.max(0.0)))).clamp(min as f64, max as f64) as u8 }

/// Seconds until a burning magazine cooks off, and whether the fire is projected out before then at the
/// current suppression rate; the default Damage Control officer floods the magazine when 60 s remain and the
/// fire will not be out in time (L669). Unit: `sim-core-fire`.
///
/// `None` when the slot is not a magazine, is flooded or empty, or its compartment is not burning. The rate
/// is `teams` times the per-team suppression divided by the crew's berth factor (zero with no berths).
pub fn cookoff_forecast(vessel: &Vessel, magazine: PartSlot, teams: u8, tuning: &content::tuning::FireTuning) -> Option<(f64, bool)> {
    forecast_in(&vessel.compartments, &vessel.parts, vessel.crew.berth_factor(), magazine, teams, tuning)
}

pub(crate) fn forecast_in(comps: &CompartmentSet, parts: &[PartRuntime], factor: Option<f64>, magazine: PartSlot, teams: u8, t: &FireTuning) -> Option<(f64, bool)> {
    let p = find_part(parts, magazine)?;
    let PartState::Magazine { rounds, ammo_kg, flooded: false, cookoff_ticks } = p.state else { return None };
    if rounds == 0 && !(ammo_kg > 0.0) { return None; }
    let comp = comps.compartments.iter().find(|c| c.parts.binary_search(&magazine).is_ok()).or_else(|| p.compartment.and_then(|c| comps.get(c)))?;
    if !comp.is_burning() { return None; }
    let delay = if t.cookoff_delay_s.is_finite() { t.cookoff_delay_s.max(0.0) } else { 120.0 };
    let remaining = (delay - cookoff_ticks as f64 * DT).max(0.0);
    let rate = teams as f64 * per_team_rate(t, factor);
    let out_in_time = rate > 0.0 && comp.fire / rate < remaining;
    Some((remaining, out_in_time))
}
