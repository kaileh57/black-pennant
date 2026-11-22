//! Unit `sys-officer-stages`. The `StageHost` the graph VM calls: reads sources from a ship's view and
//! marshals stage node ports to the typed stage functions. DESIGN.md L340 (every stage of every officer is a
//! node with typed ports), L346 (internal gains and per-tick results are readable).
//!
//! Port order and parameter keys come from `crate::graph::library::stage_sig`; this unit must match them.
//! Stage rates: helm, solver, guidance, emission controller and load shedder every graph tick (30 Hz); track
//! fusion, threat ranking, mount assignment and the damage-control planner at 10 Hz, holding outputs between.
//!
//! # How the stages are wired to their typed functions
//! Every stage reads its inputs from its ports, never from another stage's memory, so a player subgraph that
//! replaces one stage (or an L2 rule that retargets a mount) drives the stages after it:
//! - `TrackFusion` is the ship's window on the side picture (the fusion itself runs in the Sim's track phase,
//!   see `crate::graph`): its track list is every live track of `ShipView::picture`, ascending id, so it is
//!   current at 30 Hz with no hold. Its detection inputs are not read here. Gains are the spec constants of
//!   `crate::tracks`.
//! - `ThreatRanking` ranks the tracks of its input list with `engage::threat_ranking`.
//! - `MountAssignment` runs `engage::mount_assignment` over the ranking on its input port, rebuilding
//!   `RankedThreat` rows (category, range, inbound test) for ids the officer's ranking of this period does not
//!   hold, so a replaced ranking keeps its order and its rows stay current. The list it used is stored in `EngageState::ranked`, so the Bridge Board
//!   and "Why?" explain what the mounts actually did. Per mount it outputs the officer's target (also while
//!   holding, so guns train onto the target before they may fire) and fire: a level for guns (`Engaging`), a
//!   one-graph-tick pulse for guided weapons on the tick the assignment orders a launch.
//! - `BallisticSolver` solves for the track on its `target` input with `guidance::gun::solve` and fires only
//!   while its `fire` input holds and the solution is in range and inside the mount's limits. A track
//!   farther than `gun::LAY_BEYOND_CAP` times the band's cap range is only laid on (`gun::lay`): never in range,
//!   so never fired at.
//! - `PnGuidance` launches on a rising edge of `fire`, or when the target changes while `fire` holds: one
//!   munition per edge. The officer's pulses are edges, and a rule or subgraph that holds `fire` true launches
//!   once per target instead of emptying the launcher. N comes from `gunnery.pn_air` for air and weapon
//!   targets and `gunnery.pn_surface` otherwise; the midcourse aim point is left `None` (the track's predicted
//!   position).
//! - Weapons never fire or launch at Friend or Neutral tracks from these stages, whatever drives them (L486).
//!
//! # Parameters
//! Stage parameter keys are the L1 slider keys (`officers::tuning`), so the runner hands typed functions a
//! slider map built from the node's parameters. `helm.cruise_speed` is also passed as `helm.cruise`, the key
//! `officers::helm` reads. Parameters that a typed function reads from the tuning table reach it through a
//! patched copy: `sensors.burst_rotations`, `sensors.burst_repeat_s` and `sensors.pd_radar_range_m` into
//! `DoctrineTuning`, `dc.magazine_flood_s` into `FireTuning::autoflood_remaining_s`. `gunnery.open_fire_range`
//! is written to `EngageState::open_fire_range` before the assignment runs. Orders' `weapons_tight` withholds
//! fire permission from every mount that is not point defence; a struck hull withholds it from every mount.
//!
//! # Held outputs
//! The VM hands every stage an all-`None` output buffer each tick, so the 10 Hz stages re-emit their held
//! results on the ticks between runs. A 10 Hz stage runs on the first graph tick of each 100 ms period
//! (`tick / 6`), whatever phase the ship's graph ticks have, and again if the clock went backwards (rewind,
//! keyframe). Held results live in [`StageHold`] (in `StageStates`, so keyframes carry them). A stage asked
//! twice in one period (a player copy of the node) returns the held results instead of stepping its state
//! twice.
//!
//! # Script host API
//! `HostApi::call` answers the registry of `crate::script::host_api()` by index (`script::host_fn`), with the
//! semantics that registry documents; the registry is sys-script's, and this runner adds no functions to it.
//! Arity and kinds are checked first with `script::check_args` (`ScriptFault::Error`); anything missing,
//! non-finite or out of range answers `None`. Track queries take an index into the picture's live tracks
//! (ascending id) and detection queries an index into the ship's detections. The live-track index and the
//! inbound flags are built once per graph tick on first use ([`HostCache`]), so every call is constant time
//! except `track_index`, one binary search.
use crate::doctrine::{DoctrineTable, MountInfo};
use crate::emcon::{EmconIn, EmitterInfo};
use crate::engage::{AssignIn, OwnState, RankIn, RankedThreat};
use crate::graph::library::{assignment_ports, dc_door_port, dc_pump_ports, emission_class_port, shedder_port};
use crate::graph::{PortValue, StageHost};
use crate::input::ControlState;
use crate::officers::damage_control::DcOut;
use crate::officers::helm::{self, HelmIn, HelmOut};
use crate::prelude::*;
use crate::protocol::command::ControlRole;
use crate::protocol::event::ScriptFault;
use crate::sensors::{Detection, Measurement};
use crate::ship::{ShipOrders, ShipView, StageStates, TurnModel};
use crate::tracks::Track;
use content::graph::{OfficerSettings, OfficerStage, SourceKind, TargetCategory, WeaponClass};
use content::ids::PartSlot;
use content::part::{PartFunction, SensorKind};
use sim_core::parts::{PartRuntime, PartState};
use sim_core::time::{GRAPH_HZ, KN, TICK_HZ, TRACK_HZ};
use sim_core::vessel::Vessel;
use std::borrow::Cow;

/// Ticks per 10 Hz stage period.
const SLOW_PERIOD_TICKS: u64 = (TICK_HZ / TRACK_HZ) as u64;
/// Graph tick length in seconds.
const GRAPH_DT: f64 = 1.0 / GRAPH_HZ as f64;

/// Results of the 10 Hz stages held between their runs, and the guidance nodes' edge memory. Persisted in
/// `StageStates::hold`. `build_controls` (unit `sys-controls`) reads [`StageHold::dc`] for the damage-control
/// orders that have no graph port: team orders, open flood valves and magazine floods.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct StageHold {
    /// 100 ms period of the last `ThreatRanking` run, its ranked ids and how many of them were inbound.
    pub rank_period: Option<u64>,
    pub rank_ids: Vec<TrackId>,
    pub rank_inbound: u32,
    /// 100 ms period and tick of the last `MountAssignment` run, and the launches it ordered (slot, count).
    pub assign_period: Option<u64>,
    pub assign_tick: u64,
    pub launches: Vec<(PartSlot, u8)>,
    /// 100 ms period of the last `DamageControlPlanner` run and its complete orders.
    pub dc_period: Option<u64>,
    pub dc: DcOut,
    /// Per launcher, ascending slot: `fire` and `target` seen on the last graph tick (launch edges).
    pub launchers: Vec<(PartSlot, bool, TrackId)>,
}

/// The 100 ms period a tick falls in.
pub fn slow_period(tick: u64) -> u64 { tick / SLOW_PERIOD_TICKS }
/// Whether a 10 Hz stage last run in period `last` must run again at `tick`.
fn slow_due(last: Option<u64>, tick: u64) -> bool { last != Some(slow_period(tick)) }

/// Borrowed runner for one ship's graph tick.
pub struct StageRunner<'a> {
    pub view: ShipView<'a>,
    pub stages: &'a mut StageStates,
    /// Work units spent in stages this tick (gun iterations, engagement pairs).
    pub gun_iterations: u32,
    pub engagement_pairs: u32,
    /// Own-ship facts of the `OwnShip` source, computed once per graph tick on first use.
    own: Option<OwnFacts>,
    /// Host API tables, built once per graph tick on first use.
    host: HostCache,
    /// Signed heel, computed once per graph tick on first use.
    heel: Option<f64>,
}
impl<'a> StageRunner<'a> {
    pub fn new(view: ShipView<'a>, stages: &'a mut StageStates) -> Self { Self { view, stages, gun_iterations: 0, engagement_pairs: 0, own: None, heel: None, host: HostCache::default() } }

    /// The plain-data view the stage bodies read. Constant time.
    fn env(&mut self) -> StageEnv<'a> {
        let v = self.view;
        let vessel: &'a Vessel = v.vessel;
        let heel_rad = *self.heel.get_or_insert_with(|| finite(vessel.heel_trim().0));
        let networks = &vessel.networks;
        StageEnv {
            tick: v.ctx.tick.0,
            tuning: v.ctx.tuning,
            catalogue: v.ctx.catalogue,
            vessel_id: vessel.id,
            side: vessel.side,
            kin: vessel.kin,
            heel_rad,
            power_margin_kw: finite(networks.generation_kw - networks.demand_kw),
            hull_health: (1.0 - vessel.cells_lost_fraction()).clamp(0.0, 1.0),
            parts: &vessel.parts,
            mounts: &v.design.mounts,
            emitters: &v.design.emitters,
            consumer_slots: &v.design.consumer_slots,
            pumps: &v.design.pumps,
            doors: &v.design.doors,
            turn: &v.design.turn,
            settings: v.settings,
            orders: v.orders,
            doctrine: v.doctrine,
            tracks: &v.picture.tracks,
            detections: v.detections,
            own_side_hulls: v.own_side_hulls,
            esm_sites: v.esm_sites,
            control: v.control,
            world: Some(v.world),
            vessel: Some(vessel),
            range_tables: Some(&v.design.range_tables),
            accuracy_mult: v.proficiency.aim_error_mult,
            sea: v.ctx.weather.sea_state,
        }
    }

    fn own_facts(&mut self) -> OwnFacts {
        if let Some(o) = self.own { return o; }
        let o = OwnFacts::of(&self.env());
        self.own = Some(o);
        o
    }
}

impl StageHost for StageRunner<'_> {
    /// Unit: `sys-officer-stages`.
    fn read_source(&mut self, source: &SourceKind, out: &mut [PortValue]) {
        let own = if matches!(source, SourceKind::OwnShip) { Some(self.own_facts()) } else { None };
        read_source_env(&self.env(), own.as_ref(), source, &[], out);
    }
    /// Unit: `sys-officer-stages`.
    fn eval_stage(&mut self, stage: OfficerStage, part: Option<PartSlot>, inputs: &[PortValue], params: &[PortValue], out: &mut [PortValue]) {
        let env = self.env();
        let mut work = Work::default();
        eval_stage_env(&env, self.stages, &mut work, stage, part, inputs, params, out);
        self.gun_iterations = self.gun_iterations.saturating_add(work.gun_iterations);
        self.engagement_pairs = self.engagement_pairs.saturating_add(work.engagement_pairs);
    }
    fn read_source_with(&mut self, source: &SourceKind, params: &[PortValue], out: &mut [PortValue]) {
        let own = if matches!(source, SourceKind::OwnShip) { Some(self.own_facts()) } else { None };
        read_source_env(&self.env(), own.as_ref(), source, params, out);
    }
    fn host_api(&mut self) -> Option<&mut dyn crate::script::HostApi> { Some(self) }
}
impl crate::script::HostApi for StageRunner<'_> {
    /// Host API calls from scripts read the same view. Unit: `sys-officer-stages`.
    fn call(&mut self, index: u16, args: &[PortValue]) -> Result<PortValue, crate::protocol::event::ScriptFault> {
        let env = self.env();
        host_call(&env, &mut self.host, index, args)
    }
}

// ---------------------------------------------------------------------------------------------------------
// The plain-data view.

/// Everything the stage bodies read, borrowed from a ship's view. `world` and `vessel` are `None` only in
/// tests: the damage-control planner, the load shedder, formation leaders and the grounding guard need them.
pub(crate) struct StageEnv<'a> {
    pub tick: u64,
    pub tuning: &'a TuningTable,
    pub catalogue: &'a Catalogue,
    pub vessel_id: EntityId,
    pub side: SideId,
    pub kin: Kinematics,
    /// Signed heel, radians (`Vessel::heel_trim`).
    pub heel_rad: f64,
    pub power_margin_kw: f64,
    /// 1 - the fraction of the hull's cells lost.
    pub hull_health: f64,
    pub parts: &'a [PartRuntime],
    pub mounts: &'a [MountInfo],
    pub emitters: &'a [EmitterInfo],
    pub consumer_slots: &'a [PartSlot],
    pub pumps: &'a [PartSlot],
    pub doors: &'a [PartSlot],
    pub turn: &'a TurnModel,
    pub settings: &'a OfficerSettings,
    pub orders: &'a ShipOrders,
    pub doctrine: &'a DoctrineTable,
    /// The side picture this ship reads, ascending id.
    pub tracks: &'a [Track],
    /// This ship's detections, ascending (sensor, tick).
    pub detections: &'a [Detection],
    pub own_side_hulls: &'a [(DVec3, DVec3)],
    pub esm_sites: &'a [[f64; 2]],
    pub control: &'a ControlState,
    pub world: Option<&'a World>,
    pub vessel: Option<&'a Vessel>,
    /// The design's gun range tables, the officers' accuracy multiplier and the sea state, for the open-fire
    /// test (`crate::engage::hit_chance`); `None` tables skip it (test fixtures).
    pub range_tables: Option<&'a BTreeMap<PartSlot, Arc<Vec<crate::guidance::gun::RangeRow>>>>,
    pub accuracy_mult: f64,
    pub sea: content::weather::SeaState,
}
impl StageEnv<'_> {
    fn pos(&self) -> DVec3 { fin3(self.kin.pose.pos) }
    fn vel(&self) -> DVec3 { fin3(self.kin.lin_vel) }
    fn own_state(&self) -> OwnState {
        OwnState { vessel: self.vessel_id, side: self.side, pose: self.kin.pose, vel: self.vel(), ang_vel: fin3(self.kin.ang_vel), heel_rad: self.heel_rad }
    }
    fn part(&self, slot: PartSlot) -> Option<&PartRuntime> {
        self.part_at(slot).or_else(|| self.parts.iter().find(|p| p.slot == slot))
    }
    /// Constant-time part lookup (`parts[i].slot == PartSlot(i)` for every built vessel), for the host API.
    fn part_at(&self, slot: PartSlot) -> Option<&PartRuntime> { self.parts.get(slot.index()).filter(|p| p.slot == slot) }
    fn part_def(&self, p: &PartRuntime) -> Option<&PartFunction> { self.catalogue.parts.get_index(p.def.index()).map(|(_, d)| &d.function) }
    fn track(&self, id: TrackId) -> Option<&Track> { find_track(self.tracks, id) }
    /// Live tracks as a list, ascending id.
    fn live_ids(&self) -> Vec<TrackId> { self.tracks.iter().filter(|t| t.is_live()).map(|t| t.id).collect() }
    fn inbound(&self, t: &Track) -> bool { is_inbound(t, self.own_side_hulls, self.tick, self.tuning) }
}

/// Work units spent by one stage evaluation.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Work { pub gun_iterations: u32, pub engagement_pairs: u32 }

/// Own-ship facts of the `OwnShip` source and the host API.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct OwnFacts {
    pub pos: DVec3,
    pub vel: DVec3,
    pub heading_rad: f64,
    pub speed_kn: f64,
    pub heel_rad: f64,
    pub power_margin_kw: f64,
    pub fuel_fraction: f64,
    pub hull_health: f64,
    pub inbound_weapons: u32,
    pub sortie_time_s: f64,
}
impl OwnFacts {
    pub(crate) fn of(env: &StageEnv<'_>) -> Self {
        // Forward speed along the bow (vessel +X).
        let bow = env.kin.pose.rot * DVec3::X;
        let speed = env.vel().dot(bow);
        OwnFacts {
            pos: env.pos(),
            vel: env.vel(),
            heading_rad: finite(env.kin.pose.heading_rad()),
            speed_kn: finite(speed / KN),
            heel_rad: env.heel_rad,
            power_margin_kw: env.power_margin_kw,
            fuel_fraction: finite(crate::officers::engineer::fuel_fraction(env.parts, env.catalogue)).clamp(0.0, 1.0),
            hull_health: env.hull_health,
            inbound_weapons: env.tracks.iter().filter(|t| env.inbound(t)).count().min(u32::MAX as usize) as u32,
            sortie_time_s: env.tick as f64 / TICK_HZ as f64,
        }
    }
}

// ---------------------------------------------------------------------------------------------------------
// Small helpers.

fn finite(v: f64) -> f64 { if v.is_finite() { v } else { 0.0 } }
fn fin3(v: DVec3) -> DVec3 { if v.is_finite() { v } else { DVec3::ZERO } }
fn set(out: &mut [PortValue], i: usize, v: PortValue) { if let Some(o) = out.get_mut(i) { *o = v; } }
/// Write the port `back` places from the end (the trailing summary ports of per-entry stages).
fn set_back(out: &mut [PortValue], back: usize, v: PortValue) { if let Some(i) = out.len().checked_sub(back) { set(out, i, v); } }
fn scalar(x: f64) -> PortValue { if x.is_finite() { PortValue::Scalar(x) } else { PortValue::None } }
fn vec3(v: DVec3) -> PortValue { if v.is_finite() { PortValue::Vec3(v) } else { PortValue::None } }
fn opt(v: Option<f64>) -> PortValue { v.map(scalar).unwrap_or_default() }
fn track_out(id: TrackId) -> PortValue { if id.is_none() { PortValue::None } else { PortValue::Track(id) } }
fn list(ids: Vec<TrackId>) -> PortValue { PortValue::TrackList(Arc::new(ids)) }
fn par(params: &[PortValue], i: usize, default: f64) -> f64 {
    match params.get(i) { Some(PortValue::Scalar(x)) if x.is_finite() => *x, Some(PortValue::Bool(b)) => if *b { 1.0 } else { 0.0 }, _ => default }
}
fn truth(v: Option<&PortValue>) -> bool { matches!(v, Some(PortValue::Bool(true))) || matches!(v, Some(PortValue::Scalar(x)) if x.is_finite() && *x != 0.0) }
fn track_in(v: Option<&PortValue>) -> TrackId { match v { Some(PortValue::Track(t)) => *t, _ => TrackId::NONE } }
fn ids_in(v: Option<&PortValue>) -> Option<&[TrackId]> { match v { Some(PortValue::TrackList(l)) => Some(l.as_slice()), _ => None } }
fn horizontal(a: DVec3, b: DVec3) -> f64 { let (dx, dz) = (a.x - b.x, a.z - b.z); (dx * dx + dz * dz).sqrt() }
/// Compass bearing (0 north = -Z, clockwise) from `a` to `b`; `None` when they coincide.
fn bearing(a: DVec3, b: DVec3) -> Option<f64> {
    let (dx, dz) = (b.x - a.x, b.z - a.z);
    let d2 = dx * dx + dz * dz;
    if d2.is_nan() || d2 <= 1e-12 { return None; }
    let h = libm::atan2(dx, -dz);
    Some(if h < 0.0 { h + core::f64::consts::TAU } else { h })
}
fn domain_index(d: Domain) -> f64 { match d { Domain::Surface => 0.0, Domain::Subsurface => 1.0, Domain::Air => 2.0, Domain::Weapon => 3.0 } }
fn rung_index(r: IdRung) -> f64 { match r { IdRung::Unknown => 0.0, IdRung::Domain => 1.0, IdRung::Type => 2.0, IdRung::Class => 3.0, IdRung::Exact => 4.0 } }
fn grade_index(g: TrackGrade) -> f64 { match g { TrackGrade::Bearing => 0.0, TrackGrade::Coarse => 1.0, TrackGrade::Fine => 2.0, TrackGrade::Lock => 3.0 } }

/// Live track with a finite estimate, by id: binary search over an ascending picture, linear otherwise.
fn find_track(tracks: &[Track], id: TrackId) -> Option<&Track> {
    if id.is_none() { return None; }
    let t = match tracks.binary_search_by(|t| t.id.cmp(&id)) { Ok(i) => Some(&tracks[i]), Err(_) => tracks.iter().find(|t| t.id == id) };
    t.filter(|t| t.is_live() && t.pos.is_finite())
}

/// A track these stages may point a weapon at: Suspect or Hostile, never Neutral or Friend (L486).
fn engageable(t: &Track) -> bool { t.stance >= Stance::Suspect }

/// The inbound test of `crate::engage` on the tracks it applies to: Suspect or Hostile weapons, and air tracks
/// not yet identified to Type, whose closest approach to an own-side hull is inside 500 m within 60 s.
pub(crate) fn is_inbound(t: &Track, hulls: &[(DVec3, DVec3)], tick: u64, tuning: &TuningTable) -> bool {
    if !t.is_live() || t.stance < Stance::Suspect { return false; }
    let eligible = t.domain == Domain::Weapon || (t.domain == Domain::Air && t.rung < IdRung::Type);
    eligible && crate::engage::inbound(t, hulls, tick, tuning).is_some()
}

/// A slider map from a stage's parameters (signature order, clamped to their ranges), with the aliases the
/// typed functions read.
pub(crate) fn slider_map(stage: OfficerStage, params: &[PortValue]) -> BTreeMap<String, f64> {
    let mut m = BTreeMap::new();
    for (i, spec) in crate::graph::library::stage_params_spec(stage).iter().enumerate() {
        let v = par(params, i, spec.default);
        let v = if spec.min <= spec.max { v.clamp(spec.min, spec.max) } else { v };
        m.insert(spec.key.to_string(), v);
        if spec.key == "helm.cruise_speed" { m.insert(helm::SLIDER_CRUISE.to_string(), v); }
    }
    m
}

/// The tuning table with stage parameters patched in; borrowed when the patch changes nothing.
fn patched<'t>(base: &'t TuningTable, f: impl Fn(&mut TuningTable)) -> Cow<'t, TuningTable> {
    let mut t = base.clone();
    f(&mut t);
    if t == *base { Cow::Borrowed(base) } else { Cow::Owned(t) }
}

// ---------------------------------------------------------------------------------------------------------
// Sources.

/// Track list filter of the `TrackList` source (`library::source_sig`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct TrackFilter { pub min_stance: Stance, pub domain: Option<Domain>, pub min_grade: TrackGrade }
impl TrackFilter {
    pub(crate) fn from_params(params: &[PortValue]) -> Self {
        let idx = |i: usize, d: f64, lo: f64, hi: f64| libm::round(par(params, i, d).clamp(lo, hi)) as i64;
        let min_stance = match idx(0, 2.0, 0.0, 3.0) { 0 => Stance::Friend, 1 => Stance::Neutral, 2 => Stance::Suspect, _ => Stance::Hostile };
        let domain = match idx(1, -1.0, -1.0, 3.0) { 0 => Some(Domain::Surface), 1 => Some(Domain::Subsurface), 2 => Some(Domain::Air), 3 => Some(Domain::Weapon), _ => None };
        let min_grade = match idx(2, 0.0, 0.0, 3.0) { 0 => TrackGrade::Bearing, 1 => TrackGrade::Coarse, 2 => TrackGrade::Fine, _ => TrackGrade::Lock };
        TrackFilter { min_stance, domain, min_grade }
    }
    fn admits(&self, t: &Track) -> bool {
        t.is_live() && t.pos.is_finite() && t.stance >= self.min_stance && self.domain.is_none_or(|d| t.domain == d) && t.grade >= self.min_grade
    }
}

/// A filtered track list: ids nearest first (horizontal range, ties by id), the nearest range and how many of
/// them are inbound.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct FilteredTracks { pub ids: Vec<TrackId>, pub nearest_range_m: Option<f64>, pub inbound: u32 }

pub(crate) fn filter_tracks(env: &StageEnv<'_>, filter: &TrackFilter) -> FilteredTracks {
    let own = env.pos();
    let mut rows: Vec<(f64, TrackId)> = Vec::new();
    let mut inbound = 0u32;
    for t in env.tracks.iter().filter(|t| filter.admits(t)) {
        let r = horizontal(t.predict(env.tick), own);
        if !r.is_finite() { continue; }
        if env.inbound(t) { inbound = inbound.saturating_add(1); }
        rows.push((r, t.id));
    }
    rows.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
    FilteredTracks { nearest_range_m: rows.first().map(|r| r.0), ids: rows.into_iter().map(|r| r.1).collect(), inbound }
}

/// Fill a source node's outputs. `own` must be given for `OwnShip`.
pub(crate) fn read_source_env(env: &StageEnv<'_>, own: Option<&OwnFacts>, source: &SourceKind, params: &[PortValue], out: &mut [PortValue]) {
    for o in out.iter_mut() { *o = PortValue::None; }
    match source {
        SourceKind::OwnShip => {
            let Some(o) = own else { return };
            set(out, 0, vec3(o.pos));
            set(out, 1, vec3(o.vel));
            set(out, 2, scalar(o.heading_rad));
            set(out, 3, scalar(o.speed_kn));
            set(out, 4, scalar(o.heel_rad));
            set(out, 5, scalar(o.power_margin_kw));
            set(out, 6, scalar(o.fuel_fraction));
            set(out, 7, scalar(o.hull_health));
            set(out, 8, scalar(o.inbound_weapons as f64));
            set(out, 9, scalar(o.sortie_time_s));
        }
        SourceKind::PartState(slot) => {
            let Some(p) = env.part(*slot) else { return };
            let f = part_facts(env, p);
            set(out, 0, PortValue::Part(*slot));
            set(out, 1, scalar(p.health.clamp(0.0, 1.0)));
            set(out, 2, PortValue::Bool(p.works()));
            set(out, 3, PortValue::Bool(f.powered));
            set(out, 4, opt(f.ammo));
            set(out, 5, opt(f.train));
            set(out, 6, opt(f.elevation));
        }
        SourceKind::Detections(slot) => {
            let dets: Vec<Detection> = env.detections.iter().filter(|d| d.sensor.carrier == env.vessel_id && d.sensor.index == slot.0).copied().collect();
            let last = dets.last().copied();
            set(out, 1, scalar(dets.len() as f64));
            set(out, 0, PortValue::Detections(Arc::new(dets)));
            if let Some(d) = last {
                if matches!(d.measurement, Measurement::RangeBearing { .. }) { set(out, 2, scalar(d.sigma_range_m)); }
                set(out, 3, scalar(d.sigma_bearing_rad));
            }
        }
        SourceKind::TrackList => {
            let f = filter_tracks(env, &TrackFilter::from_params(params));
            set(out, 1, scalar(f.ids.len() as f64));
            set(out, 2, f.ids.first().map(|id| PortValue::Track(*id)).unwrap_or_default());
            set(out, 3, opt(f.nearest_range_m));
            set(out, 4, scalar(f.inbound as f64));
            set(out, 0, list(f.ids));
        }
        SourceKind::TheatreOrders => {
            let o = env.orders;
            set(out, 0, PortValue::Orders);
            set(out, 1, track_out(o.designated));
            set(out, 2, opt(o.speed_kn));
            set(out, 3, PortValue::Bool(o.weapons_tight));
            set(out, 4, PortValue::Bool(o.evade));
            set(out, 5, opt(o.keep_range_m));
        }
        SourceKind::Constant(v) => set(out, 0, PortValue::from_value(v)),
    }
}

/// Readable facts of one part beyond its health.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct PartFacts { pub powered: bool, pub ammo: Option<f64>, pub train: Option<f64>, pub elevation: Option<f64> }

fn ratio(have: f64, cap: f64) -> Option<f64> { if cap > 0.0 && have.is_finite() { Some((have / cap).clamp(0.0, 1.0)) } else { None } }

/// Whether a part works, is not shed and gets power. Constant time.
fn part_powered(p: &PartRuntime) -> bool { p.works() && !p.links.shed && p.links.power_supplied > 0.0 }
/// Training (mounts, launchers, staring sensors) and elevation (mounts) now. Constant time.
fn part_angles(p: &PartRuntime) -> (Option<f64>, Option<f64>) {
    let (t, e) = match &p.state {
        PartState::Mount(m) => (Some(m.train_rad), Some(m.elevation_rad)),
        PartState::Launcher { train_rad, .. } | PartState::Sensor { train_rad, .. } => (Some(*train_rad), None),
        _ => (None, None),
    };
    (t.filter(|x| x.is_finite()), e.filter(|x| x.is_finite()))
}

/// Everything readable about a part. At most `MAX_FEED` constant-time magazine lookups.
pub(crate) fn part_facts(env: &StageEnv<'_>, p: &PartRuntime) -> PartFacts {
    let def = env.part_def(p);
    let (train, elevation) = part_angles(p);
    let mut f = PartFacts { powered: part_powered(p), train, elevation, ammo: None };
    match (&p.state, def) {
        (PartState::Mount(m), def) => {
            // The stock of the magazines feeding the gun when it has a feed, else its ready-use rounds.
            let (mut have, mut cap) = (0.0, 0.0);
            for slot in p.feed().magazines() {
                if let Some(mp) = env.part_at(slot) {
                    if let (PartState::Magazine { rounds, .. }, Some(PartFunction::Magazine { rounds: c, .. })) = (&mp.state, env.part_def(mp)) {
                        have += *rounds as f64;
                        cap += *c as f64;
                    }
                }
            }
            f.ammo = ratio(have, cap).or_else(|| match def { Some(PartFunction::Gun(g)) => ratio(m.ready_rounds as f64, g.ready_rounds as f64), _ => None });
        }
        (PartState::Launcher { ready, .. }, Some(PartFunction::Launcher(l))) => f.ammo = ratio(*ready as f64, l.cells as f64),
        (PartState::Magazine { rounds, .. }, Some(PartFunction::Magazine { rounds: c, .. })) => f.ammo = ratio(*rounds as f64, *c as f64),
        (PartState::Decoy { rounds, .. }, Some(PartFunction::Decoy(d))) => f.ammo = ratio(*rounds as f64, d.rounds as f64),
        (PartState::MineRail { mines, .. }, Some(PartFunction::MineRail { capacity, .. })) => f.ammo = ratio(*mines as f64, *capacity as f64),
        _ => {}
    }
    f
}

// ---------------------------------------------------------------------------------------------------------
// Stages.

/// Run one stage over the plain-data view. `out` is cleared first.
#[allow(clippy::too_many_arguments)]
pub(crate) fn eval_stage_env(env: &StageEnv<'_>, st: &mut StageStates, work: &mut Work, stage: OfficerStage, part: Option<PartSlot>, inputs: &[PortValue], params: &[PortValue], out: &mut [PortValue]) {
    for o in out.iter_mut() { *o = PortValue::None; }
    match stage {
        OfficerStage::HelmAutopilot | OfficerStage::StationKeeping | OfficerStage::Evasion => helm_stage(env, st, stage, inputs, params, out),
        OfficerStage::TrackFusion => track_fusion(env, out),
        OfficerStage::ThreatRanking => threat_ranking(env, st, work, inputs, params, out),
        OfficerStage::MountAssignment => mount_assignment(env, st, work, inputs, params, out),
        OfficerStage::BallisticSolver => if let Some(slot) = part { ballistic_solver(env, st, work, slot, inputs, params, out) },
        OfficerStage::PnGuidance => if let Some(slot) = part { pn_guidance(env, st, slot, inputs, params, out) },
        OfficerStage::LoadShedder => load_shedder(env, st, out),
        OfficerStage::DamageControlPlanner => damage_control(env, st, params, out),
        OfficerStage::EmissionController => emission_controller(env, st, inputs, params, out),
    }
}

// ---- Helm ----

fn helm_stage(env: &StageEnv<'_>, st: &mut StageStates, stage: OfficerStage, inputs: &[PortValue], params: &[PortValue], out: &mut [PortValue]) {
    let sliders = slider_map(stage, params);
    let leader = env.orders.formation.and_then(|f| env.world.and_then(|w| w.vessel(f.leader))).map(|v| v.kin).filter(|k| k.pose.pos.is_finite());
    let depth_ahead_m = depth_ahead(env);
    // Evasion dodges the weapons on its port, inbound to this hull (a weapon passing a consort is not ours to
    // dodge) and never Friend weapons (our own launches start inside the 500 m radius).
    let threats: Vec<Track> = if stage == OfficerStage::Evasion {
        let hull = [(env.pos(), env.vel())];
        ids_in(inputs.get(1)).unwrap_or(&[]).iter().filter_map(|id| env.track(*id))
            .filter(|t| t.domain == Domain::Weapon && t.stance > Stance::Friend)
            .map(|t| { let mut c = t.clone(); c.inbound = crate::engage::inbound(t, &hull, env.tick, env.tuning).is_some(); c })
            .collect()
    } else { Vec::new() };
    let input = HelmIn {
        pose: env.kin.pose, vel: env.vel(), yaw_rate: finite(-env.kin.ang_vel.y), orders: env.orders, turn: env.turn, leader,
        threats: &threats, sliders: &sliders, depth_ahead_m, dt: GRAPH_DT,
    };
    let write = |out: &mut [PortValue], o: &HelmOut| { set(out, 0, scalar(o.heading_rad)); set(out, 1, scalar(o.speed_kn)); };
    match stage {
        OfficerStage::HelmAutopilot => {
            let o = helm::autopilot(&input, &mut st.helm);
            write(out, &o);
            set(out, 2, opt(cross_track_m(&env.orders.route, st.helm.waypoint as usize, st.helm.route_done, env.pos())));
        }
        OfficerStage::StationKeeping => {
            let o = helm::station_keeping(&input, &mut st.helm);
            if let Some(o) = &o { write(out, o); }
            set(out, 2, PortValue::Bool(o.is_some()));
            set(out, 3, opt(station_distance_m(env.orders, leader.as_ref(), env.pos())));
        }
        _ => {
            let o = helm::evasion(&input, &mut st.helm);
            if let Some(o) = &o { write(out, o); }
            set(out, 2, PortValue::Bool(o.is_some()));
        }
    }
}

/// Water depth one stop distance ahead on the current heading (grounding guard); deep water without a world.
fn depth_ahead(env: &StageEnv<'_>) -> f64 {
    let Some(world) = env.world else { return f64::INFINITY };
    let h = finite(env.kin.pose.heading_rad());
    let stop = if env.turn.stop_distance_m.is_finite() { env.turn.stop_distance_m.clamp(0.0, 20_000.0) } else { 0.0 };
    let p = env.pos() + DVec3::new(libm::sin(h), 0.0, -libm::cos(h)) * stop;
    let d = world.terrain().depth_at(p.x, p.z);
    if d.is_finite() { d } else { f64::INFINITY }
}

/// Signed distance from the current route leg, positive to starboard of the direction of travel; `None` with
/// no leg (no route, heading for the first waypoint, or past the last).
pub(crate) fn cross_track_m(route: &[DVec2], waypoint: usize, done: bool, pos: DVec3) -> Option<f64> {
    if done || waypoint == 0 || waypoint >= route.len() { return None; }
    let (a, b) = (route[waypoint - 1], route[waypoint]);
    let d = b - a;
    let len = d.length();
    if !(len.is_finite() && len > 1e-6) { return None; }
    let starboard = DVec2::new(-d.y, d.x) / len;
    let x = (DVec2::new(pos.x, pos.z) - a).dot(starboard);
    x.is_finite().then_some(x)
}

/// Horizontal distance to the formation slot (offset in the leader's frame, x ahead and y starboard) or else
/// to the ordered station point.
pub(crate) fn station_distance_m(orders: &ShipOrders, leader: Option<&Kinematics>, pos: DVec3) -> Option<f64> {
    let own = DVec2::new(pos.x, pos.z);
    let target = if let (Some(slot), Some(l)) = (orders.formation.as_ref(), leader) {
        let h = l.pose.heading_rad();
        let ahead = DVec2::new(libm::sin(h), -libm::cos(h));
        let starboard = DVec2::new(libm::cos(h), libm::sin(h));
        DVec2::new(l.pose.pos.x, l.pose.pos.z) + ahead * slot.offset_m.x + starboard * slot.offset_m.y
    } else if let Some((p, _)) = orders.station {
        p
    } else {
        return None;
    };
    let d = (target - own).length();
    d.is_finite().then_some(d)
}

// ---- Sensors ----

fn track_fusion(env: &StageEnv<'_>, out: &mut [PortValue]) {
    let ids = env.live_ids();
    set(out, 1, scalar(ids.len() as f64));
    set(out, 0, list(ids));
    set(out, 2, scalar(env.tuning.tracks.gate_sigma));
    set(out, 3, scalar(crate::tracks::alpha_for(SensorKind::FireControlRadar)));
    set(out, 4, scalar(crate::tracks::alpha_for(SensorKind::SearchRadar)));
    set(out, 5, scalar(crate::tracks::alpha_for(SensorKind::Esm)));
}

/// Whether a list is exactly the picture's live tracks in ascending order (the unreplaced fusion output).
fn same_as_live(env: &StageEnv<'_>, ids: &[TrackId]) -> bool {
    let mut live = env.tracks.iter().filter(|t| t.is_live()).map(|t| t.id);
    ids.iter().all(|id| live.next() == Some(*id)) && live.next().is_none()
}

/// The tracks a list names, borrowing the picture when the list is the fusion output.
fn tracks_of<'t>(env: &StageEnv<'t>, ids: &[TrackId]) -> Cow<'t, [Track]> {
    if same_as_live(env, ids) { Cow::Borrowed(env.tracks) } else { Cow::Owned(ids.iter().filter_map(|id| find_track(env.tracks, *id)).cloned().collect()) }
}

fn emission_controller(env: &StageEnv<'_>, st: &mut StageStates, inputs: &[PortValue], params: &[PortValue], out: &mut [PortValue]) {
    let setting = match inputs.get(1) { Some(PortValue::Emcon(e)) => *e, _ => env.settings.emcon };
    let d = &env.tuning.doctrine;
    let trigger = par(params, 0, d.burst_trigger_m).clamp(0.0, 1.0e6);
    let rotations = libm::round(par(params, 1, d.burst_rotations as f64).clamp(1.0, 6.0)) as u8;
    let repeat = par(params, 2, d.burst_repeat_s).clamp(1.0, 3_600.0);
    let pd_range = par(params, 3, d.silent_pd_radar_m).clamp(0.0, 1.0e6);
    let tuning = patched(env.tuning, |t| { t.doctrine.burst_rotations = rotations; t.doctrine.burst_repeat_s = repeat; t.doctrine.silent_pd_radar_m = pd_range; });
    // The tracks on the port; the whole picture when the port carries no list.
    let tracks = match ids_in(inputs.first()) { Some(ids) => tracks_of(env, ids), None => Cow::Borrowed(env.tracks) };
    let own = env.pos();
    let inbound_weapon_m = tracks.iter().filter(|t| t.domain == Domain::Weapon && env.inbound(t))
        .map(|t| t.predict(env.tick).distance(own)).filter(|r| r.is_finite()).min_by(|a, b| a.total_cmp(b));
    let engage = &st.engage;
    let engaging = |pd: bool| env.mounts.iter().filter(move |m| m.point_defence == pd)
        .filter_map(|m| engage.mounts.iter().find(|me| me.slot == m.slot))
        .filter_map(|me| match me.status { EngageStatus::Engaging { track, .. } => Some(track), _ => None });
    let point_defence_engaging = engaging(true).next().is_some();
    // A gun holding on a contact inside its doctrine range, for the grade or the hit chance its fire control
    // cannot yet give, has that contact assigned too (L508: bursts every 10 s while a track with a weapon
    // assigned is below Lock), so Burst refreshes the lock the open-fire test needs (DECISION, k2).
    let waiting = env.mounts.iter().filter(|m| !m.point_defence && m.weapon == WeaponClass::Gun)
        .filter_map(|m| engage.mounts.iter().find(|me| me.slot == m.slot).map(|me| (m, me.status)))
        .filter_map(|(m, status)| match status {
            EngageStatus::Holding { track, reason: HoldReason::OutOfRange { .. } | HoldReason::Grade { .. } } => {
                let t = env.track(track)?;
                let limit = env.doctrine.get(m.slot, crate::doctrine::target_category(t)).map(|e| e.effective_range_m)?;
                (t.predict(env.tick).distance(env.pos()) <= limit).then_some(track)
            }
            _ => None,
        });
    let assigned_below_lock = engaging(false).chain(engaging(true)).chain(waiting).any(|id| env.track(id).is_some_and(|t| t.grade < TrackGrade::Lock));
    // A gun engaging has shells in flight: Burst holds the fire-control lock through the shoot (L509), so the
    // gun does not fall to Coarse between 10 s bursts and stop firing (DECISION, q5).
    let guns_firing = env.mounts.iter().filter(|m| !m.point_defence && m.weapon == WeaponClass::Gun)
        .filter_map(|m| engage.mounts.iter().find(|me| me.slot == m.slot))
        .any(|me| matches!(me.status, EngageStatus::Engaging { .. }));
    let input = EmconIn {
        setting, trigger_range_m: trigger, own_pos: own, emitters: env.emitters, tracks: &tracks, inbound_weapon_m,
        own_weapons_in_flight: engage.weapons_in_flight > 0 || guns_firing, point_defence_engaging, assigned_below_lock, esm_sites: env.esm_sites, tick: env.tick,
    };
    let o = crate::emcon::emission_controller(&input, &mut st.emcon, &tuning);
    emission_outputs(env.emitters, &o.radiate, o.lights, o.esm_warning, out);
}

/// One output per emitter class: true while any emitter of the class radiates.
pub(crate) fn emission_outputs(emitters: &[EmitterInfo], radiate: &[bool], lights: bool, esm: bool, out: &mut [PortValue]) {
    let mut ports = [false; 5];
    for (e, r) in emitters.iter().zip(radiate) {
        if *r { if let Some(p) = ports.get_mut(emission_class_port(e.kind, e.point_defence) as usize) { *p = true; } }
    }
    for (i, r) in ports.iter().enumerate() { set(out, i, PortValue::Bool(*r)); }
    set(out, 5, PortValue::Bool(lights));
    set(out, 6, PortValue::Bool(esm));
}

// ---- Gunnery ----

fn threat_ranking(env: &StageEnv<'_>, st: &mut StageStates, work: &mut Work, inputs: &[PortValue], params: &[PortValue], out: &mut [PortValue]) {
    if slow_due(st.hold.rank_period, env.tick) {
        let sliders = slider_map(OfficerStage::ThreatRanking, params);
        let tracks = match ids_in(inputs.first()) { Some(ids) => tracks_of(env, ids), None => Cow::Owned(Vec::new()) };
        let input = RankIn { own: env.own_state(), tracks: &tracks, designated: env.orders.designated, sliders: &sliders, own_side_hulls: env.own_side_hulls, tick: env.tick };
        let rows = crate::engage::threat_ranking(&input, env.doctrine, env.tuning);
        let pairs = tracks.len().saturating_mul(env.own_side_hulls.len().max(1));
        work.engagement_pairs = work.engagement_pairs.saturating_add(pairs.min(u32::MAX as usize) as u32);
        st.hold.rank_ids = rows.iter().map(|r| r.track).collect();
        st.hold.rank_inbound = rows.iter().filter(|r| r.inbound).count().min(u32::MAX as usize) as u32;
        st.hold.rank_period = Some(slow_period(env.tick));
        st.engage.ranked = rows;
    }
    let h = &st.hold;
    set(out, 1, h.rank_ids.first().map(|id| PortValue::Track(*id)).unwrap_or_default());
    set(out, 2, scalar(h.rank_inbound as f64));
    set(out, 0, list(h.rank_ids.clone()));
}

/// The rows `mount_assignment` ranks: the officer's own rows when the list is its ranking, else rows rebuilt
/// in the list's order (unknown, dropped and repeated ids are skipped; Neutral and Friend tracks never rank).
pub(crate) fn ranked_rows(env: &StageEnv<'_>, ids: &[TrackId], officer: &[RankedThreat]) -> Vec<RankedThreat> {
    if ids.len() == officer.len() && ids.iter().zip(officer).all(|(a, b)| *a == b.track) { return officer.to_vec(); }
    let own = env.pos();
    let n = ids.len();
    let mut out: Vec<RankedThreat> = Vec::with_capacity(n);
    for (i, id) in ids.iter().enumerate() {
        if out.iter().any(|r| r.track == *id) { continue; }
        let Some(t) = env.track(*id) else { continue };
        if !engageable(t) { continue; }
        let designated = !env.orders.designated.is_none() && *id == env.orders.designated;
        if t.out_of_action && !designated { continue; }
        let score = (n - i) as f64;
        if let Some(r) = officer.iter().find(|r| r.track == *id) { out.push(RankedThreat { score, ..*r }); continue; }
        let eligible = t.domain == Domain::Weapon || (t.domain == Domain::Air && t.rung < IdRung::Type);
        let cpa = if eligible { crate::engage::inbound(t, env.own_side_hulls, env.tick, env.tuning) } else { None };
        let inbound = cpa.is_some();
        let category = if t.domain == Domain::Weapon || inbound { TargetCategory::InboundWeapon } else { crate::doctrine::target_category(t) };
        out.push(RankedThreat {
            track: *id, category, score, range_m: finite(t.predict(env.tick).distance(own)), time_to_cpa_s: cpa.map(|c| c.time_s), inbound,
            designated,
        });
    }
    out
}

/// Mounts the player holds, by mount and by weapon group; none while the officers have command (L838).
pub(crate) fn held_mounts(control: &ControlState, mounts: &[MountInfo]) -> Vec<PartSlot> {
    if control.officers_in_command { return Vec::new(); }
    let mut held: Vec<PartSlot> = Vec::new();
    for role in &control.held {
        match role {
            ControlRole::Mount(slot) => held.push(*slot),
            ControlRole::WeaponGroup(g) => held.extend(mounts.iter().filter(|m| m.group == *g).map(|m| m.slot)),
            _ => {}
        }
    }
    held.sort();
    held.dedup();
    held
}

/// Fire permission per mount from the orders: `weapons_tight` withholds it from every mount that is not point
/// defence; a hull that has struck its colours withholds it from every mount, point defence included
/// (DECISION, r3: ruling 1 says a struck hull stops firing, and its point defence kept covering its consorts).
pub(crate) fn fire_permission(mounts: &[MountInfo], tight: bool, struck: bool) -> Vec<Option<bool>> {
    mounts.iter().map(|m| if struck || (tight && !m.point_defence) { Some(false) } else { None }).collect()
}

fn mount_assignment(env: &StageEnv<'_>, st: &mut StageStates, work: &mut Work, inputs: &[PortValue], params: &[PortValue], out: &mut [PortValue]) {
    if slow_due(st.hold.assign_period, env.tick) {
        // The officer's rows are fresh only when its ranking ran this period; otherwise (ranking replaced, its
        // stock stage never evaluated) every row is rebuilt so range, inbound and designation stay current.
        let officer: &[RankedThreat] = if st.hold.rank_period == Some(slow_period(env.tick)) { &st.engage.ranked } else { &[] };
        let rows = ranked_rows(env, ids_in(inputs.first()).unwrap_or(&[]), officer);
        st.engage.open_fire_range = Some(par(params, 1, 1.0).clamp(0.5, 1.0));
        let held = held_mounts(env.control, env.mounts);
        let struck = env.vessel.is_some_and(|v| v.struck);
        let permission = fire_permission(env.mounts, env.orders.weapons_tight, struck);
        let gunnery = env.range_tables.map(|tables| crate::engage::GunneryFacts { tables, catalogue: env.catalogue, accuracy_mult: env.accuracy_mult, sea: env.sea });
        let input = AssignIn {
            own: env.own_state(), ranked: &rows, tracks: env.tracks, mounts: env.mounts, parts: env.parts, doctrine: env.doctrine,
            held: &held, permission: &permission, tick: env.tick, gunnery,
        };
        let assigned = crate::engage::mount_assignment(&input, &mut st.engage, env.tuning);
        let pairs = env.mounts.len().saturating_mul(rows.len().min(crate::engage::MAX_THREATS_PER_MOUNT));
        work.engagement_pairs = work.engagement_pairs.saturating_add(pairs.min(u32::MAX as usize) as u32);
        st.hold.launches = assigned.iter().filter(|a| a.launch > 0).map(|a| (a.slot, a.launch)).collect();
        st.hold.assign_period = Some(slow_period(env.tick));
        st.hold.assign_tick = env.tick;
        st.engage.ranked = rows;
    }
    assignment_outputs(env.mounts, st, env.tick, out);
}

/// Per mount (target, fire), then the number of mounts engaging. Guided weapons pulse fire on the tick the
/// assignment ordered a launch; decoy launchers never carry a target or fire (decoy control fires them).
pub(crate) fn assignment_outputs(mounts: &[MountInfo], st: &StageStates, tick: u64, out: &mut [PortValue]) {
    let mut engaged = 0u32;
    for (i, m) in mounts.iter().enumerate() {
        let Some((tp, fp)) = assignment_ports(i) else { break };
        let status = st.engage.mounts.get(i).filter(|me| me.slot == m.slot).or_else(|| st.engage.mounts.iter().find(|me| me.slot == m.slot)).map(|me| me.status).unwrap_or_default();
        let (target, engaging) = match status {
            EngageStatus::Engaging { track, .. } => (track, true),
            EngageStatus::Holding { track, .. } => (track, false),
            EngageStatus::Idle => (TrackId::NONE, false),
        };
        if engaging && m.weapon != WeaponClass::Decoy { engaged += 1; }
        let fire = match m.weapon {
            WeaponClass::Decoy => false,
            WeaponClass::Gun | WeaponClass::PointDefenceGun => engaging,
            _ => engaging && st.hold.assign_tick == tick && st.hold.launches.iter().any(|(s, n)| *s == m.slot && *n > 0),
        };
        if m.weapon != WeaponClass::Decoy { set(out, tp as usize, track_out(target)); }
        set(out, fp as usize, PortValue::Bool(fire));
    }
    set_back(out, 1, scalar(engaged as f64));
}

fn ballistic_solver(env: &StageEnv<'_>, st: &mut StageStates, work: &mut Work, slot: PartSlot, inputs: &[PortValue], params: &[PortValue], out: &mut [PortValue]) {
    let Some(m) = env.mounts.iter().find(|m| m.slot == slot) else { return };
    let Some(p) = env.part(slot) else { return };
    let PartState::Mount(ms) = p.state else { return };
    let target = env.track(track_in(inputs.first())).filter(|t| engageable(t));
    let gun = match env.part_def(p) { Some(PartFunction::Gun(g)) => Some(g), _ => None };
    let (Some(t), Some(g), true) = (target, gun, p.works()) else {
        // No target to solve for: hold the current lay.
        set(out, 1, PortValue::Bool(false));
        set(out, 2, scalar(ms.train_rad));
        set(out, 3, scalar(ms.elevation_rad));
        set(out, 5, scalar(0.0));
        set(out, 7, PortValue::Bool(false));
        set(out, 8, PortValue::Bool(false));
        return;
    };
    let drag_k = env.catalogue.bands.iter().find(|b| b.band == g.band).map(|b| b.drag_k).filter(|k| k.is_finite() && *k >= 0.0).unwrap_or(0.0);
    let muzzle_mps = if m.muzzle_mps.is_finite() && m.muzzle_mps > 0.0 { m.muzzle_mps } else { g.muzzle_mps };
    // Slew rates as sim-core runs them: the rated rate times damage output and power supply.
    let factor = (p.output_factor() * p.links.power_supplied.clamp(0.0, 1.0)).max(0.0);
    let rate = |dps: f64| finite(dps.to_radians() * factor).max(0.0);
    let cap = libm::round(par(params, 0, env.tuning.guidance.gun_iteration_cap as f64).clamp(1.0, 10.0)) as u8;
    let converge = par(params, 1, env.tuning.guidance.gun_converge_m).clamp(0.01, 100.0);
    // Lead on the fire-control estimate (`Track::gun_target`): the motion fit of the track's fixes, not the
    // filter's noisy velocity (L462 gains stay the filter's).
    let (mut target_pos, target_vel) = t.gun_target(env.tick);
    if t.domain == Domain::Surface { target_pos.y += crate::guidance::gun::wave_lay_m(t.rcs_m2 as f64, env.sea); }
    let input = crate::guidance::gun::GunSolveIn {
        muzzle: env.kin.pose.to_world(p.centre), own_vel: env.vel(), pose: env.kin.pose, zero_rad: m.zero_rad, limits: m.limits,
        target_pos, target_vel, muzzle_mps, drag_k,
        train_rad: ms.train_rad, elevation_rad: ms.elevation_rad, train_rate: rate(g.train_rate_dps), elevation_rate: rate(g.elevation_rate_dps),
        iteration_cap: cap, converge_m: converge,
    };
    // Beyond the band's effective-range cap (with a margin) the mount is laid, not solved: see `gun::lay`.
    let cap_m = env.catalogue.bands.iter().find(|b| b.band == g.band).map(|b| b.cap_range_m).filter(|c| c.is_finite() && *c > 0.0);
    let to = input.target_pos - input.muzzle;
    let horizontal_m = libm::sqrt(to.x * to.x + to.z * to.z);
    let s = match cap_m {
        Some(cap) if horizontal_m > crate::guidance::gun::LAY_BEYOND_CAP * cap => crate::guidance::gun::lay(&input, cap),
        _ => crate::guidance::gun::solve(&input),
    };
    work.gun_iterations = work.gun_iterations.saturating_add(s.iterations as u32);
    let reachable = s.in_range && s.train_out_of_limits.is_none() && s.elevation_out_of_limits.is_none() && s.aim_point.is_finite();
    set(out, 0, vec3(s.aim_point));
    set(out, 1, PortValue::Bool(truth(inputs.get(1)) && reachable));
    set(out, 2, scalar(s.train_rad));
    set(out, 3, scalar(s.elevation_rad));
    set(out, 4, scalar(s.time_of_flight_s));
    set(out, 5, scalar(s.iterations as f64));
    set(out, 6, scalar(s.residual_m));
    set(out, 7, PortValue::Bool(s.converged));
    set(out, 8, PortValue::Bool(s.in_range));
}

fn pn_guidance(env: &StageEnv<'_>, st: &mut StageStates, slot: PartSlot, inputs: &[PortValue], params: &[PortValue], out: &mut [PortValue]) {
    let target = env.track(track_in(inputs.first())).filter(|t| engageable(t));
    let fire = truth(inputs.get(1)) && target.is_some();
    let id = target.map(|t| t.id).unwrap_or(TrackId::NONE);
    let launch = launch_edge(&mut st.hold.launchers, slot, fire, id);
    let works = env.part(slot).is_some_and(|p| p.works());
    set(out, 0, PortValue::Bool(launch && works));
    set(out, 1, track_out(id));
    if let Some(t) = target { set(out, 2, scalar(pn_constant(t.domain, params))); }
}

/// Launch on a rising edge of `fire` or a change of target while it holds; records this tick's inputs.
pub(crate) fn launch_edge(mem: &mut Vec<(PartSlot, bool, TrackId)>, slot: PartSlot, fire: bool, target: TrackId) -> bool {
    let i = match mem.binary_search_by(|e| e.0.cmp(&slot)) {
        Ok(i) => i,
        Err(i) => { mem.insert(i, (slot, false, TrackId::NONE)); i }
    };
    let (_, was, prev) = mem[i];
    mem[i] = (slot, fire, target);
    fire && (!was || prev != target)
}

/// Navigation constant N for a target domain (L520): air and weapon targets `gunnery.pn_air`, others
/// `gunnery.pn_surface`.
pub(crate) fn pn_constant(domain: Domain, params: &[PortValue]) -> f64 {
    let (i, d) = if matches!(domain, Domain::Air | Domain::Weapon) { (1, 4.0) } else { (0, 3.0) };
    par(params, i, d).clamp(2.0, 6.0)
}

// ---- Engineer ----

fn load_shedder(env: &StageEnv<'_>, st: &mut StageStates, out: &mut [PortValue]) {
    let Some(v) = env.vessel else { return };
    let prio = crate::officers::engineer::load_shedder(env.parts, &env.settings.power_priority, &v.networks, env.catalogue, &mut st.engineer);
    shedder_outputs(env.consumer_slots, &prio, &st.engineer, out);
}

pub(crate) fn shedder_outputs(consumers: &[PartSlot], prio: &[(PartSlot, u8)], eng: &crate::officers::engineer::EngineerState, out: &mut [PortValue]) {
    for (i, slot) in consumers.iter().enumerate() {
        let Some(p) = shedder_port(i) else { break };
        if let Ok(k) = prio.binary_search_by(|(s, _)| s.cmp(slot)) { set(out, p as usize, scalar(prio[k].1 as f64)); }
    }
    set_back(out, 3, scalar(eng.generation_kw));
    set_back(out, 2, scalar(eng.demand_kw));
    set_back(out, 1, scalar(eng.shed.len() as f64));
}

// ---- Damage control ----

/// Water below this is bilge residue, not flooding (the planner's own pump threshold).
const DC_WATER_MIN_M3: f64 = 0.05;

fn damage_control(env: &StageEnv<'_>, st: &mut StageStates, params: &[PortValue], out: &mut [PortValue]) {
    if slow_due(st.hold.dc_period, env.tick) {
        if let Some(v) = env.vessel {
            let flood_s = par(params, 2, env.tuning.fire.autoflood_remaining_s).clamp(0.0, 3_600.0);
            let tuning = patched(env.tuning, |t| t.fire.autoflood_remaining_s = flood_s);
            st.hold.dc = crate::officers::damage_control::planner(v, &env.settings.repair_order, &mut st.dc, env.catalogue, &tuning);
            st.hold.dc_period = Some(slow_period(env.tick));
        }
    }
    let (flooding, fires) = env.vessel.map(|v| {
        let c = &v.compartments.compartments;
        (c.iter().filter(|c| c.water_m3.is_finite() && c.water_m3 > DC_WATER_MIN_M3).count(), c.iter().filter(|c| c.fire > 0.0).count())
    }).unwrap_or((0, 0));
    dc_outputs(env.pumps, env.doors, &st.hold.dc, flooding, fires, out);
}

/// Per pump (on, compartment), per door closed, then teams busy, compartments flooding and on fire.
pub(crate) fn dc_outputs(pumps: &[PartSlot], doors: &[PartSlot], dc: &DcOut, flooding: usize, fires: usize, out: &mut [PortValue]) {
    for (i, slot) in pumps.iter().enumerate() {
        let Some((on, comp)) = dc_pump_ports(i) else { break };
        if let Some((_, run, target)) = dc.pumps.iter().find(|(s, _, _)| s == slot) {
            set(out, on as usize, PortValue::Bool(*run));
            if let Some(c) = target { set(out, comp as usize, scalar(c.0 as f64)); }
        }
    }
    for (i, slot) in doors.iter().enumerate() {
        let Some(p) = dc_door_port(pumps.len(), i) else { break };
        if let Some((_, closed)) = dc.doors.iter().find(|(s, _)| s == slot) { set(out, p as usize, PortValue::Bool(*closed)); }
    }
    let busy = dc.teams.iter().filter(|t| **t != sim_core::parts::TeamOrder::Idle).count();
    set_back(out, 3, scalar(busy as f64));
    set_back(out, 2, scalar(flooding as f64));
    set_back(out, 1, scalar(fires as f64));
}


// ---------------------------------------------------------------------------------------------------------
// Script host API.

/// Per-graph-tick tables the host API indexes, built on first use so that every call stays inside its declared
/// bound: the picture's live tracks (indices into `StageEnv::tracks`, ascending id) and their inbound flags.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct HostCache { live: Option<Vec<u32>>, inbound: Option<Vec<bool>> }
impl HostCache {
    fn live(&mut self, env: &StageEnv<'_>) -> &[u32] {
        self.live.get_or_insert_with(|| {
            env.tracks.iter().enumerate().filter(|(_, t)| t.is_live()).map(|(i, _)| i.min(u32::MAX as usize) as u32).collect()
        })
    }
    /// The live track at picture index `i`.
    fn track<'t>(&mut self, env: &StageEnv<'t>, i: Option<usize>) -> Option<&'t Track> {
        let tracks = env.tracks;
        i.and_then(|i| self.live(env).get(i).copied()).and_then(|k| tracks.get(k as usize))
    }
    /// The inbound test of the live track at picture index `i`; the flags of every live track are computed
    /// together on the first query of a tick.
    fn inbound(&mut self, env: &StageEnv<'_>, i: Option<usize>) -> Option<bool> {
        let i = i?;
        if self.inbound.is_none() {
            let flags = self.live(env).iter().map(|k| env.tracks.get(*k as usize).is_some_and(|t| env.inbound(t))).collect();
            self.inbound = Some(flags);
        }
        self.inbound.as_ref().and_then(|f| f.get(i).copied())
    }
    /// Picture index of a track, by binary search over the live tracks.
    fn index_of(&mut self, env: &StageEnv<'_>, id: TrackId) -> Option<usize> {
        if id.is_none() { return None; }
        let tracks = env.tracks;
        self.live(env).binary_search_by(|k| tracks.get(*k as usize).map_or(TrackId::NONE, |t| t.id).cmp(&id)).ok()
    }
}

/// Registry rows [`host_call`] answers (`host_fn::OWN_POSITION ..= host_fn::TRACK_INDEX`). A test fails when
/// sys-script appends a row, so a new function is never silently answered `None`.
pub(crate) const HANDLED_HOST_ROWS: u16 = crate::script::host_fn::TRACK_INDEX + 1;

/// Magnitude above which angle wrapping answers `None` (precision is gone long before).
const MAX_ANGLE_ARG: f64 = 1.0e12;

/// A number argument: `None` for a missing or non-finite value.
fn arg_num(a: &PortValue) -> Result<Option<f64>, ScriptFault> {
    match a { PortValue::Scalar(x) => Ok(x.is_finite().then_some(*x)), PortValue::Bool(b) => Ok(Some(if *b { 1.0 } else { 0.0 })), PortValue::None => Ok(None), _ => Err(ScriptFault::Error) }
}
/// A 0-based index or part slot, truncated toward zero; negative, non-finite and huge values name nothing.
fn arg_index(a: &PortValue) -> Result<Option<usize>, ScriptFault> {
    Ok(arg_num(a)?.filter(|x| *x >= 0.0 && *x < u32::MAX as f64).map(|x| libm::trunc(x) as usize))
}
fn arg_vec(a: &PortValue) -> Result<Option<DVec3>, ScriptFault> {
    match a { PortValue::Vec3(v) => Ok(v.is_finite().then_some(*v)), PortValue::None => Ok(None), _ => Err(ScriptFault::Error) }
}
fn arg_track(a: &PortValue) -> Result<TrackId, ScriptFault> {
    match a { PortValue::Track(t) => Ok(*t), PortValue::None => Ok(TrackId::NONE), _ => Err(ScriptFault::Error) }
}
/// Wrap to `[-pi, pi)`.
fn wrap_pi(x: f64) -> f64 { let t = core::f64::consts::TAU; x - t * libm::floor((x + core::f64::consts::PI) / t) }

/// Ready rounds over capacity of a mount, launcher or decoy launcher; `None` for every other part.
fn ready_fraction(env: &StageEnv<'_>, p: &PartRuntime) -> Option<f64> {
    match (&p.state, env.part_def(p)) {
        (PartState::Mount(m), Some(PartFunction::Gun(g))) => ratio(m.ready_rounds as f64, g.ready_rounds as f64),
        (PartState::Launcher { ready, .. }, Some(PartFunction::Launcher(l))) => ratio(*ready as f64, l.cells as f64),
        (PartState::Decoy { rounds, .. }, Some(PartFunction::Decoy(d))) => ratio(*rounds as f64, d.rounds as f64),
        _ => None,
    }
}

/// Answer registry function `index` of `crate::script::host_api()` over the ship's view, with the semantics
/// that registry documents: arity and kinds are checked first (`ScriptFault::Error`); anything missing,
/// non-finite or out of range answers `Ok(PortValue::None)`. Every row is constant time once the per-tick
/// [`HostCache`] is built, except `track_index`, one binary search. Never panics on script data.
pub(crate) fn host_call(env: &StageEnv<'_>, cache: &mut HostCache, index: u16, args: &[PortValue]) -> Result<PortValue, ScriptFault> {
    use crate::script::host_fn as h;
    crate::script::check_args(index, args)?;
    let a = |i: usize| args.get(i).unwrap_or(&PortValue::None);
    let part = |i: usize| -> Result<Option<&PartRuntime>, ScriptFault> { Ok(arg_index(a(i))?.and_then(|s| u16::try_from(s).ok()).and_then(|s| env.part_at(PartSlot(s)))) };
    let detection = |i: usize| -> Result<Option<&Detection>, ScriptFault> { Ok(arg_index(a(i))?.and_then(|k| env.detections.get(k))) };
    let pos = env.pos();
    let v = match index {
        h::OWN_POSITION => vec3(pos),
        h::OWN_VELOCITY => vec3(env.vel()),
        h::OWN_HEADING => scalar(env.kin.pose.heading_rad()),
        h::OWN_SPEED_KN => scalar(env.vel().dot(env.kin.pose.rot * DVec3::X) / KN),
        h::OWN_HEEL => scalar(env.heel_rad),
        h::SORTIE_TIME_S => scalar(env.tick as f64 / TICK_HZ as f64),
        h::HULL_HEALTH => scalar(env.hull_health),
        h::PART_HEALTH => opt(part(0)?.map(|p| p.health.clamp(0.0, 1.0))),
        h::PART_WORKS => part(0)?.map(|p| PortValue::Bool(p.works())).unwrap_or_default(),
        h::PART_AMMO_FRACTION => opt(part(0)?.and_then(|p| ready_fraction(env, p))),
        h::PART_TRAIN => opt(part(0)?.filter(|p| matches!(p.state, PartState::Mount(_) | PartState::Sensor { .. })).and_then(|p| part_angles(p).0)),
        h::PART_ELEVATION => opt(part(0)?.and_then(|p| part_angles(p).1)),
        h::TRACK_COUNT => scalar(cache.live(env).len() as f64),
        h::TRACK_ID => cache.track(env, arg_index(a(0))?).map(|t| track_out(t.id)).unwrap_or_default(),
        h::TRACK_POSITION => cache.track(env, arg_index(a(0))?).map(|t| vec3(t.predict(env.tick))).unwrap_or_default(),
        h::TRACK_VELOCITY => cache.track(env, arg_index(a(0))?).map(|t| vec3(t.vel)).unwrap_or_default(),
        h::TRACK_STANCE => cache.track(env, arg_index(a(0))?).map(|t| PortValue::Stance(crate::input::effective_stance(t))).unwrap_or_default(),
        h::TRACK_DOMAIN => opt(cache.track(env, arg_index(a(0))?).map(|t| domain_index(t.domain))),
        h::TRACK_GRADE => opt(cache.track(env, arg_index(a(0))?).map(|t| grade_index(t.grade))),
        h::TRACK_RUNG => opt(cache.track(env, arg_index(a(0))?).map(|t| rung_index(t.rung))),
        h::TRACK_RADIUS95_M => opt(cache.track(env, arg_index(a(0))?).map(|t| t.radius95_m(env.tick, &env.tuning.tracks))),
        h::TRACK_INBOUND => cache.inbound(env, arg_index(a(0))?).map(PortValue::Bool).unwrap_or_default(),
        h::DETECTION_COUNT => scalar(env.detections.len() as f64),
        h::DETECTION_SENSOR => opt(detection(0)?.filter(|d| d.sensor.carrier == env.vessel_id && d.sensor.index != crate::ids::SensorRef::SEEKER).map(|d| d.sensor.index as f64)),
        h::DETECTION_BEARING => opt(detection(0)?.map(|d| match d.measurement { Measurement::RangeBearing { bearing_rad, .. } | Measurement::BearingOnly { bearing_rad } => bearing_rad })),
        h::DETECTION_RANGE_M => opt(detection(0)?.and_then(|d| match d.measurement { Measurement::RangeBearing { range_m, .. } => Some(range_m), Measurement::BearingOnly { .. } => None })),
        h::DESIGNATED_TRACK => track_out(env.orders.designated),
        h::EMCON => PortValue::Emcon(env.settings.emcon),
        h::BEARING_TO => opt(arg_vec(a(0))?.and_then(|p| bearing(pos, p))),
        h::RANGE_TO => opt(arg_vec(a(0))?.map(|p| horizontal(pos, p))),
        h::WRAP_ANGLE => opt(arg_num(a(0))?.filter(|x| x.abs() < MAX_ANGLE_ARG).map(wrap_pi)),
        h::CLAMP => match (arg_num(a(0))?, arg_num(a(1))?, arg_num(a(2))?) { (Some(x), Some(lo), Some(hi)) if lo <= hi => scalar(x.clamp(lo, hi)), _ => PortValue::None },
        h::INTERCEPT_POINT => match (arg_vec(a(0))?, arg_vec(a(1))?, arg_num(a(2))?) {
            (Some(p), Some(v), Some(s)) => crate::script::intercept_point(pos, p, v, s).map(vec3).unwrap_or_default(),
            _ => PortValue::None,
        },
        h::TRACK_INDEX => opt(cache.index_of(env, arg_track(a(0))?).map(|i| i as f64)),
        // A registry row this runner does not know yet (rows are append-only): no value rather than a fault.
        _ => PortValue::None,
    };
    Ok(crate::graph::library::sanitize(v))
}

#[cfg(test)]
#[path = "stages_tests.rs"]
mod tests;
