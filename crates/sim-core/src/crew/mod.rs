//! Unit `sim-core-crew`. Crew as capacity: berths, damage-control teams and their timed jobs.
//! DESIGN.md L701-727, L642-646 (patching), L396 (jury rig).
//!
//! There is no walking crew. Teams = floor(berths / 4), minimum 1, maximum 6; one job per team at a time.
//! Berth factor: job time is multiplied by original berths / surviving berths; berths in destroyed, burning or
//! over-50%-flooded compartments do not count; with no surviving berths no job starts.
//! Jobs: jury-rig a cut conduit to 50% capacity (20 s); patch one 0.25 m^2 breach face (10 s, only while the
//! compartment is under 50% flooded); fight fire (2 intensity per second); repair a part (30 s per job, +25
//! percentage points of health, capped at 75%). Destroyed parts and cells are repaired only at the tender.
//! Which job each team does is decided by the Damage Control officer in sim-systems (`TeamOrder`).
//!
//! Implementation notes:
//! - Berths are recounted every tick from the catalogue (`Berths` parts): a berth part counts while it works
//!   and its compartment is neither burning nor over the flooded fraction. `Compartment::berths` is refreshed
//!   with the design berths of each compartment. When the prefab gave no berth total, the first tick adopts
//!   the design count.
//! - Team count follows the original berths, so losing berths slows jobs (the berth factor) rather than
//!   removing teams. With every berth lost (for example all in one burning compartment) no team works, as the
//!   spec says, and only flooding or the fire burning out ends the fire.
//! - Job progress is kept in base seconds: each tick adds dt / berth factor, and a job finishes at its base
//!   time. A job that stops being possible (fire out, conduit already rigged, compartment over half flooded)
//!   pauses at zero progress. A stopped crew (no berths) keeps its progress.
//! - Fire fighting is a rate, not a timed job: the teams per compartment go to `fire::step_fire`, which applies
//!   the berth factor to the suppression rate. `JobDone::FireFought(c)` is reported once, on the first crew
//!   tick after the fire in c is out (fought down or drowned), for teams that had worked on it under that
//!   order; it is checked before new orders apply.
//! - Patching seals the deepest unpatched breach face in world space (ties: opening order). Jury-rigging uses
//!   `networks` so zones are recomputed at once. A repair raises part health by `repair_gain` up to
//!   `repair_cap` and lowers each live footprint cell by the same two buckets, never below bucket 2, so the
//!   cell-mean health agrees.
//! - Passengers (`passengers`, set by the sortie) are lost when the working passenger berths that are neither
//!   burning nor flooded can no longer hold them; survivors are assumed to move to the safe berths. A hull with
//!   no passenger berths never loses passengers here.
//! - Door openings are refreshed from door part state here, before flooding runs in the same phase.
use crate::compartments::{find_part, CompartmentSet, OpeningKind};
use crate::grid::VesselGrid;
use crate::math::{DVec3, Pose};
use crate::networks::{ConduitState, Networks};
use crate::parts::{PartRuntime, TeamOrder};
use crate::vessel::Vessel;
use crate::world::StepContext;
use content::catalogue::Catalogue;
use content::enums::DamageLevel;
use content::ids::{CompartmentId, PartSlot};
use content::part::PartFunction;
use content::tuning::CrewTuning;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[cfg(test)]
mod tests;

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Team { pub order: TeamOrder, /// Seconds of work done on the current job.
    pub progress_s: f64 }

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct CrewState {
    pub original_berths: u16,
    pub surviving_berths: u16,
    pub passengers: u16,
    pub passengers_lost: u16,
    pub teams: Vec<Team>,
    /// 0.0..=0.15 veterancy carried in from the save. OWNER DECISION PENDING: which stats it affects.
    pub veterancy: f64,
}
impl CrewState {
    /// Job time multiplier (L704); `None` when no berth survives.
    pub fn berth_factor(&self) -> Option<f64> { if self.surviving_berths == 0 { None } else { Some(self.original_berths as f64 / self.surviving_berths as f64) } }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum JobDone { ConduitJuryRigged(u16), FacePatched(CompartmentId), PartRepaired(PartSlot), FireFought(CompartmentId) }

/// Team count for a berth count (L703).
/// The count is clamped before narrowing, and a tuning row with `min_teams > max_teams` uses `max_teams`.
pub fn team_count(berths: u16, tuning: &content::tuning::CrewTuning) -> u8 {
    let max = tuning.max_teams as u16;
    let min = (tuning.min_teams as u16).min(max);
    (berths / tuning.berths_per_team.max(1) as u16).clamp(min, max) as u8
}

/// Health to damage level, on the bucket scale (health = 1 - bucket / 8, rounded to the nearest bucket):
/// only zero health is Destroyed.
pub(crate) fn level_from_health(h: f64) -> DamageLevel {
    if !(h > 0.0) { return DamageLevel::Destroyed; }
    let b = libm::round((1.0 - h.min(1.0)) * 8.0).clamp(0.0, 7.0) as u8;
    DamageLevel::from_bucket(b)
}

/// Initial crew state for a prefab. Unit: `sim-core-crew`.
///
/// Original berths are the prefab's berth count, else the sum of its compartments' berths. Every berth starts
/// alive, teams start idle, passengers and veterancy start at zero (the sortie sets them).
pub fn build_crew(prefab: &crate::vessel::VesselPrefab, tuning: &content::tuning::CrewTuning) -> CrewState {
    let berths = if prefab.berths > 0 { prefab.berths } else { prefab.compartments.compartments.iter().fold(0u16, |a, c| a.saturating_add(c.berths)) };
    CrewState { original_berths: berths, surviving_berths: berths, passengers: 0, passengers_lost: 0, teams: vec![Team::default(); team_count(berths, tuning) as usize], veterancy: 0.0 }
}

/// One tick: recount surviving berths and passengers, take each team's order from `vessel.controls.teams`
/// (a changed order resets progress), advance jobs by dt scaled by the berth factor, and complete them
/// (patch deepest breach face first; jury-rig sets the conduit to 50%). Fire fighting is reported as team
/// counts per compartment for `fire::step_fire`. Unit: `sim-core-crew`.
pub fn step_crew(vessel: &mut Vessel, ctx: &StepContext<'_>, done: &mut Vec<JobDone>) -> Vec<(CompartmentId, u8)> {
    let pose = vessel.kin.pose;
    let io = CrewIo {
        crew: &mut vessel.crew, comps: &mut vessel.compartments, parts: &mut vessel.parts, net: &mut vessel.networks, grid: &mut vessel.grid,
        orders: &vessel.controls.teams, pose: &pose,
    };
    crew_tick(io, ctx.catalogue, &ctx.tuning.crew, ctx.tuning.flooding.flooded_fraction, ctx.dt, done)
}

/// The parts of a vessel one crew tick touches.
pub(crate) struct CrewIo<'a> {
    pub crew: &'a mut CrewState,
    pub comps: &'a mut CompartmentSet,
    pub parts: &'a mut [PartRuntime],
    pub net: &'a mut Networks,
    pub grid: &'a mut VesselGrid,
    pub orders: &'a [TeamOrder],
    pub pose: &'a Pose,
}

fn def_fn<'c>(catalogue: &'c Catalogue, p: &PartRuntime) -> Option<&'c PartFunction> { catalogue.parts.get_index(p.def.index()).map(|(_, d)| &d.function) }

pub(crate) fn crew_tick(io: CrewIo<'_>, catalogue: &Catalogue, t: &CrewTuning, flooded: f64, dt: f64, done: &mut Vec<JobDone>) -> Vec<(CompartmentId, u8)> {
    let CrewIo { crew, comps, parts, net, grid, orders, pose } = io;
    let dt = if dt.is_finite() && dt > 0.0 { dt } else { 0.0 };
    let flooded = if flooded.is_finite() { flooded } else { 0.5 };
    comps.sync_part_compartments(parts);
    comps.sync_doors(parts);

    // ---- Berths and passengers ----
    let safe = |comps: &CompartmentSet, c: Option<CompartmentId>| match c {
        None => true,
        Some(c) => comps.get(c).is_some_and(|k| !k.is_absorbed() && !k.is_burning() && k.fill_fraction() <= flooded),
    };
    for c in &mut comps.compartments { c.berths = 0; }
    let (mut design, mut surviving, mut pax_design, mut pax_safe) = (0u32, 0u32, 0u32, 0u32);
    for p in parts.iter() {
        match def_fn(catalogue, p) {
            Some(PartFunction::Berths { count }) => {
                let n = *count as u32;
                design += n;
                if let Some(c) = p.compartment.and_then(|c| comps.get_mut(c)) { c.berths = c.berths.saturating_add(n as u16); }
                if p.works() && safe(comps, p.compartment) { surviving += n; }
            }
            Some(PartFunction::PassengerBerths { count }) => {
                pax_design += *count as u32;
                if p.works() && safe(comps, p.compartment) { pax_safe += *count as u32; }
            }
            _ => {}
        }
    }
    let to16 = |v: u32| v.min(u16::MAX as u32) as u16;
    if crew.original_berths == 0 && design > 0 { crew.original_berths = to16(design); }
    crew.surviving_berths = to16(surviving).min(crew.original_berths);
    if pax_design > 0 && crew.passengers as u32 > pax_safe {
        let lost = crew.passengers - to16(pax_safe);
        crew.passengers -= lost;
        crew.passengers_lost = crew.passengers_lost.saturating_add(lost);
    }

    // ---- Fires put out since the last tick ----
    // Checked against the orders the teams were working, before new orders replace them, so the report
    // survives the officer moving the team on in the same tick it sees the fire out.
    let mut fought: BTreeSet<u16> = BTreeSet::new();
    for team in &mut crew.teams {
        if let TeamOrder::FightFire(c) = team.order {
            if team.progress_s > 0.0 && !comps.get(c).is_some_and(|k| k.is_burning()) {
                fought.insert(c.0);
                team.progress_s = 0.0;
            }
        }
    }
    done.extend(fought.into_iter().map(|c| JobDone::FireFought(CompartmentId(c))));

    // ---- Teams and orders ----
    let n = team_count(crew.original_berths, t) as usize;
    crew.teams.resize(n, Team::default());
    for (i, team) in crew.teams.iter_mut().enumerate() {
        let order = orders.get(i).copied().unwrap_or_default();
        if team.order != order { team.order = order; team.progress_s = 0.0; }
    }
    let Some(factor) = crew.berth_factor() else { return Vec::new() };
    let step = if factor.is_finite() && factor > 0.0 { dt / factor } else { 0.0 };
    let cap = if t.repair_cap.is_finite() { t.repair_cap.clamp(0.0, 1.0) } else { 0.75 };
    let mut fire_teams: BTreeMap<u16, u8> = BTreeMap::new();
    for ti in 0..crew.teams.len() {
        let order = crew.teams[ti].order;
        let valid = match order {
            TeamOrder::Idle => false,
            TeamOrder::FightFire(c) => comps.get(c).is_some_and(|k| k.is_burning()),
            TeamOrder::Patch { compartment } => comps.get(compartment).is_some_and(|k| k.fill_fraction() < flooded) && deepest_breach(comps, grid, pose, compartment).is_some(),
            TeamOrder::JuryRig { conduit } => net.conduits.get(conduit as usize).is_some_and(|k| k.state == ConduitState::Cut),
            TeamOrder::Repair(s) => find_part(parts, s).is_some_and(|p| p.works() && p.health < cap - 1e-9),
        };
        let team = &mut crew.teams[ti];
        if !valid { team.progress_s = 0.0; continue; }
        team.progress_s += step;
        match order {
            TeamOrder::FightFire(c) => { let e = fire_teams.entry(c.0).or_insert(0); *e = e.saturating_add(1); }
            TeamOrder::Patch { compartment } => {
                let base = pos_or(t.patch_face_s, 10.0);
                if team.progress_s + 1e-9 >= base {
                    team.progress_s = (team.progress_s - base).max(0.0);
                    if let Some(oi) = deepest_breach(comps, grid, pose, compartment) {
                        comps.openings[oi].patched = true;
                        done.push(JobDone::FacePatched(compartment));
                    }
                }
            }
            TeamOrder::JuryRig { conduit } => {
                if team.progress_s + 1e-9 >= pos_or(t.jury_rig_s, 20.0) {
                    team.progress_s = 0.0;
                    if crate::networks::jury_rig(net, conduit) { done.push(JobDone::ConduitJuryRigged(conduit)); }
                }
            }
            TeamOrder::Repair(s) => {
                if team.progress_s + 1e-9 >= pos_or(t.repair_job_s, 30.0) {
                    team.progress_s = 0.0;
                    if repair(parts, grid, s, pos_or(t.repair_gain, 0.25), cap) { done.push(JobDone::PartRepaired(s)); }
                }
            }
            TeamOrder::Idle => {}
        }
    }
    fire_teams.into_iter().map(|(c, n)| (CompartmentId(c), n)).collect()
}

fn pos_or(v: f64, d: f64) -> f64 { if v.is_finite() && v > 0.0 { v } else { d } }

/// Index of the deepest unpatched breach face of a compartment, by world height of the opening cell centre.
fn deepest_breach(comps: &CompartmentSet, grid: &VesselGrid, pose: &Pose, c: CompartmentId) -> Option<usize> {
    let n = grid.design.cells.len();
    let mut best: Option<(f64, usize)> = None;
    for (i, o) in comps.openings.iter().enumerate() {
        if o.a != c || o.b.is_some() || o.kind != OpeningKind::Breach || o.patched || !o.open { continue; }
        let y = if o.cell.index() < n { pose.to_world(DVec3::from(grid.pos(o.cell).centre_m())).y } else { f64::INFINITY };
        let y = if y.is_finite() { y } else { f64::MAX };
        if best.is_none_or(|(by, _)| y < by) { best = Some((y, i)); }
    }
    best.map(|b| b.1)
}

/// One repair job on a part. Returns whether it was applied.
fn repair(parts: &mut [PartRuntime], grid: &mut VesselGrid, s: PartSlot, gain: f64, cap: f64) -> bool {
    let Some(i) = (if parts.get(s.index()).is_some_and(|p| p.slot == s) { Some(s.index()) } else { parts.iter().position(|p| p.slot == s) }) else { return false };
    let p = &mut parts[i];
    if !p.works() || p.health >= cap { return false; }
    p.health = (p.health + gain).min(cap);
    p.level = level_from_health(p.health);
    let floor = libm::round((1.0 - cap) * 8.0).clamp(0.0, 8.0) as u8;
    let lift = libm::round(gain * 8.0).clamp(0.0, 8.0) as u8;
    let n = grid.damage.len();
    for c in p.cells.clone() {
        if c.index() >= n { continue; }
        let b = grid.bucket(c);
        if b > floor && b < content::cell::BUCKET_DESTROYED { grid.set_bucket(c, b.saturating_sub(lift).max(floor)); }
    }
    true
}
