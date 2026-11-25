//! Unit `sys-trials`. The trial battery, the generic headless runner and Complexity points.
//! DESIGN.md L227-229 (trial battery), L973-976 (complexity points), L1300 (mac-vs-win tolerances),
//! L297, L491, L727 (gates that run on this harness).
//!
//! The battery runs on each save in the background Rust worker and in CI on hosted Windows and macOS:
//! turning circle, acceleration, heel recovery and a 1-vs-3 fight. The fight pits the design against three
//! Combine hulls drawn by a fixed seed from the enemy classes of the lowest act whose tonnage cap the design
//! fits (if it fits none, the heaviest act with classes in the build), for 180 simulated seconds at
//! Beaufort 4. Score = 100 x (share of enemy tonnage disabled) - 50 x (share of own cells lost), averaged
//! over seeds: 3 on save, 20 in CI. Output: a scorecard and a Compare view against earlier versions, within
//! 60 s of the save (p95). Fallback when that gate fails: on-save fight of 1 seed and 90 s, scorecard marked
//! "Short trial"; CI keeps the full battery. The battery fails CI when a gauge prediction drifts more than
//! 10% from simulation.
//! Complexity: deterministic work counts from a 10 s headless trial (Beaufort 4, all systems powered, firing
//! at a drone): compound cuboids, buoyancy samples, conduit segments, compartments, graph node evaluations,
//! Luau interrupts. Points = sum(count x coefficient) from `ComplexityCoefficients`.
//!
//! # How each trial runs (choices the spec leaves open)
//! - Every trial is a `GameMode::Trial` sortie on the theatre's basin map, widened to [`TRIAL_MAP_M`] so a
//!   long run never leaves the chart, under the basin ruleset (no tonnage or complexity cap; a Fit Check or
//!   lobby decides those). The design sails on side 0 as player slot 0.
//! - Turning, acceleration and heel recovery run in the sea state `run_battery` passes them: calm water,
//!   because the Speed gauge they are compared with predicts calm water, and a wave train would measure the
//!   weather rather than the gauge. The fight and the complexity trial run at the battery's sea (Beaufort 4).
//! - The helm is taken by hand (`TakeControl { Helm }` and `PlayerInput`), so the trials measure the hull and
//!   not the Helm officer's caution: full throttle, rudder hard to starboard (the side the gauge predicts).
//! - Acceleration: from rest at full throttle until the speed stops rising (the last 10 s average under
//!   0.2% above the 10 s before, after at least 30 s) or [`ACCEL_MAX_S`]. Top speed is the mean of the last
//!   5 s; time to 90% is the first tick the speed reaches 90% of that.
//! - Turning circle: the steady turning diameter of the centre of mass, as the gauge predicts it. The hull
//!   starts at the gauge's top speed, runs straight for [`TURN_LEAD_S`], then holds the rudder hard over; from
//!   90 degrees of turn on, the track is sampled at 10 Hz for one full circle and a least-squares circle is
//!   fitted. A hull that turns less than 180 degrees in [`TURN_MAX_S`] cannot turn: diameter 0, as the gauge
//!   writes it.
//! - Heel recovery: at the gauge's top speed, a hard-over turn of 90 degrees, then rudder amidships. Peak heel
//!   is the largest heel seen in the turn and after it; recovery is the time from rudder amidships until the
//!   heel stays within [`HEEL_SETTLED_DEG`] of the heel the hull carried on the straight run for
//!   [`HEEL_HOLD_S`]. A hull that never settles in [`HEEL_RECOVER_MAX_S`] reads that limit.
//! - Fight: the design starts at the basin spawn, steering at its top speed for the three opponents, which
//!   start [`FIGHT_RANGE_M`] off (bearing and range jittered by the seed) line abreast and hunt it as one
//!   group. 3 km is inside every gun's effective-range cap: in probes on the starters, opening at 4.5 km or
//!   more left most 180 s fights without a single hit, which would score every design the same. The stock officers fight the design. A seed ends early once every opponent or the design is out
//!   of the fight. Seeds are 1, 2, 3, ..., so the on-save seeds are the first CI seeds. A seed is a win when
//!   every opponent is disabled or lost and the design is not; a loss when the design is lost or disabled.
//!   Rounds per kill counts every round, missile and torpedo the design fired over all seeds per opponent
//!   taken out; with no kill it is the rounds fired (a lower bound). The first-salvo hit rate is, per gun
//!   mount and seed, whether any round of the mount's first salvo hit an opponent.
//! - Complexity: the design and one target drone 3 km off the bow, every emitter radiating (EMCON Active),
//!   Beaufort 4, 10 s. The world and systems counters are scene totals, so the same 10 s with the drone
//!   alone is subtracted, leaving the design's own per-tick work. The coefficients in `tuning.ron` are
//!   unfitted (0.0) until the owner's Mac fit; points are then 0 and the counts still show.
//! - Drift: the Speed gauge's top speed and turning circle against the acceleration and turning trials.
//!   Every pair is listed; the server fails CI on any over 10%.
//! - Reference scenes: LANTERN is the 90 m starter destroyer. Enemy hulls are drawn by seed from the
//!   Combine combatant classes of every act; capitals are the classes marked `capital`. The swarm's drones
//!   are the drone bays of the hulls it fields (the player's destroyer and enemy frigates that carry them).
use super::Deadline;
use crate::prelude::*;
use crate::lod::HostLoad;
use crate::protocol::command::{Command, CommandEnvelope, ControlRole, PlayerInput};
use crate::protocol::config::{BlueprintRef, BodyInit, Faction, GroupInit, GroupOrders, ModeRules, Outcome, PlayerInit, SideDef, SortieConfig, SortieIntel, SortieResult, VesselInit, VesselRole};
use crate::protocol::event::OutEvent;
use crate::sim::{Sim, SimDeps, SimError};
use content::blueprint::Blueprint;
use content::campaign::{CampaignData, ClassRole, EnemyClassDef, FleetKind, Formation, MapDef, Marker, MarkerKind, Objective, MAX_MAP_M};
use content::settings::SimSettings;
use content::tuning::ComplexityCoefficients;
use content::weather::SeaState;
use sim_core::events::PhysEvent;
use sim_core::projectiles::ProjectileId;
use sim_core::rng::{Key, Stream};
use sim_core::vessel::VesselStatus;

/// Something that plays a headless sortie in place of a human: issues commands each tick. Used by the
/// winnability gate (stock officers, Helm following the briefing route), gate scenarios and campaign bots.
pub trait HeadlessPolicy {
    /// Called once per tick before `Sim::step`; push commands for any player slot.
    fn commands(&mut self, sim: &Sim, out: &mut Vec<CommandEnvelope>);
    /// Stop early (scenario-specific end).
    fn done(&mut self, _sim: &Sim) -> bool { false }
}
/// Issues no commands: the hull's officers and its standing orders do everything.
pub struct NoCommands;
impl HeadlessPolicy for NoCommands { fn commands(&mut self, _sim: &Sim, _out: &mut Vec<CommandEnvelope>) {} }

/// Run a config headless to its end or `max_ticks`, with `HostLoad::IDLE`, and return the result. Must be
/// called inside the sim rayon pool. Unit: `sys-trials`.
///
/// The loop is bounded by iterations, not by the sim clock, so a policy that pauses cannot hang it. A run
/// stopped by `max_ticks` or by the policy ends as a draw unless the sortie decided itself; a run stopped by
/// the deadline ends as aborted (no result applies).
pub fn run_headless(config: SortieConfig, deps: SimDeps, max_ticks: u64, policy: &mut dyn HeadlessPolicy, deadline: &mut dyn Deadline) -> Result<SortieResult, SimError> {
    let mut sim = Sim::new(config, deps)?;
    let mut commands = Vec::new();
    let mut stop = Outcome::Draw;
    for i in 0..max_ticks {
        if i % TICKS_PER_S == 0 && deadline.expired() { stop = Outcome::Aborted; break; }
        if sim.is_over() || policy.done(&sim) { break; }
        commands.clear();
        policy.commands(&sim, &mut commands);
        if !commands.is_empty() { sim.apply(&commands); }
        sim.set_host_load(HostLoad::IDLE);
        sim.step();
        drop(sim.drain_events());
    }
    Ok(match sim.result() { Some(r) => r.clone(), None => sim.finish(stop).clone() })
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct BatterySpec { pub seeds: u8, pub fight_s: f64, pub sea: SeaState, pub short: bool }
impl BatterySpec {
    pub const ON_SAVE: BatterySpec = BatterySpec { seeds: 3, fight_s: 180.0, sea: SeaState::BEAUFORT_4, short: false };
    pub const CI: BatterySpec = BatterySpec { seeds: 20, fight_s: 180.0, sea: SeaState::BEAUFORT_4, short: false };
    pub const SHORT: BatterySpec = BatterySpec { seeds: 1, fight_s: 90.0, sea: SeaState::BEAUFORT_4, short: true };
}

/// Deterministic work counts of the 10 s complexity trial (L973).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ComplexityCounts { pub cuboids: u32, pub buoyancy_samples: u32, pub conduit_segments: u32, pub compartments: u32, pub graph_node_evals: u32, pub luau_interrupts: u32 }
/// Points = sum(count x coefficient). Real code: spec formula.
pub fn complexity_points(c: &ComplexityCounts, k: &ComplexityCoefficients) -> f64 {
    c.cuboids as f64 * k.cuboid + c.buoyancy_samples as f64 * k.buoyancy_sample + c.conduit_segments as f64 * k.conduit_segment + c.compartments as f64 * k.compartment + c.graph_node_evals as f64 * k.graph_node_eval + c.luau_interrupts as f64 * k.luau_interrupt
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct FightScore {
    pub score: f64,
    pub per_seed: Vec<f64>,
    pub wins: u8,
    pub mean_hull_fraction_left: f64,
    pub rounds_per_kill: f64,
    pub first_salvo_hit_rate: f64,
    /// Per seed, in `per_seed` order: 2 won, 1 neither side out, 0 lost. Read by the cross-platform check.
    /// Added by `sys-trials` (additive).
    #[serde(default)]
    pub outcomes: Vec<u8>,
    /// Per seed, in `per_seed` order: share of the design's cells left at the end. Added by `sys-trials`.
    #[serde(default)]
    pub hull_left: Vec<f64>,
}

/// A gauge prediction against its simulated value (fails CI over 10%, L229).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Drift { pub gauge: String, pub predicted: f64, pub simulated: f64 }

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Scorecard {
    pub design: ContentHash,
    pub tuning_version: u32,
    pub turning_circle_m: f64,
    pub time_to_90pct_speed_s: f64,
    pub top_speed_kn: f64,
    pub heel_recovery_s: f64,
    pub peak_heel_deg: f64,
    pub fight: FightScore,
    pub complexity: ComplexityCounts,
    pub complexity_points: f64,
    pub drift: Vec<Drift>,
    /// "Short trial" marker (fallback, L227).
    pub short_trial: bool,
    pub opponents: Vec<ClassId>,
}

// ---------------------------------------------------------------------------------------------------------
// Constants.

pub(crate) const TICKS_PER_S: u64 = sim_core::time::TICK_HZ as u64;
/// Side of the design and of its opponents.
const ME: SideId = SideId(0);
const THEM: SideId = SideId(1);
/// The design's player slot.
const SLOT: PlayerSlot = PlayerSlot(0);
/// Seed of the manoeuvring and complexity trials (calm or fixed sea; the seed only picks the wave train).
const TRIAL_SEED: u64 = 1;
/// Side of the widened basin map, metres.
pub const TRIAL_MAP_M: f64 = 40_000.0;
pub const ACCEL_MAX_S: f64 = 240.0;
/// Acceleration plateau: windows of this length, and the rise under which the speed counts as steady.
const PLATEAU_WINDOW_S: f64 = 10.0;
const PLATEAU_RISE: f64 = 0.002;
const PLATEAU_MIN_S: f64 = 30.0;
const TOP_MEAN_S: f64 = 5.0;
pub const TURN_LEAD_S: f64 = 10.0;
pub const TURN_MAX_S: f64 = 300.0;
/// Turn sampling: from this much heading change, for one full circle.
const TURN_SAMPLE_FROM_DEG: f64 = 90.0;
const TURN_SAMPLE_TO_DEG: f64 = 450.0;
/// Least heading change for a turning circle to be measured at all (half a circle of samples).
const TURN_MIN_DEG: f64 = 180.0;
const TURN_SAMPLE_TICKS: u64 = 6;
pub const HEEL_LEAD_S: f64 = 10.0;
/// The heel trial's turn: this much heading change, at most this long.
const HEEL_TURN_DEG: f64 = 90.0;
const HEEL_TURN_MAX_S: f64 = 60.0;
pub const HEEL_SETTLED_DEG: f64 = 1.0;
/// The settled band is never narrower than this many degrees of heel.
const HEEL_SETTLED_MIN_DEG: f64 = 2.0;
pub const HEEL_HOLD_S: f64 = 2.0;
pub const HEEL_RECOVER_MAX_S: f64 = 60.0;
/// Fight start geometry.
pub const FIGHT_RANGE_M: f64 = 3_000.0;
const FIGHT_RANGE_JITTER_M: f64 = 300.0;
const FIGHT_BEARING_JITTER_DEG: f64 = 20.0;
const FIGHT_SPACING_M: f64 = 500.0;
const FIGHT_ENEMY_SPEED_KN: f64 = 18.0;
/// The design's start speed in the fight.
const FIGHT_OWN_SPEED_KN: f64 = 15.0;
pub const OPPONENTS: usize = 3;
pub const COMPLEXITY_S: f64 = 10.0;
const COMPLEXITY_DRONE_M: f64 = 3_000.0;
/// Salts of the trial draws (counter-hash RNG, `Stream::Generator`).
const SALT_OPPONENT: u64 = 0x5452_4941_4C5F_4F50;
const SALT_FIGHT: u64 = 0x5452_4941_4C5F_4647;
const SALT_SCENE: u64 = 0x5452_4941_4C5F_5343;
/// LANTERN in the reference scenes, and the co-op reference's four player hulls.
const LANTERN: &str = "starters/destroyer90.bp.ron";
const COOP_HULLS: [&str; 4] = ["starters/destroyer90.bp.ron", "starters/corvette60.bp.ron", "starters/corvette40.bp.ron", "starters/fac22.bp.ron"];
/// Reference scenes are trials of this many ticks (the server stops them sooner).
const SCENE_TICKS: u64 = 60 * 60 * TICKS_PER_S;
/// Drift entries.
pub const DRIFT_TOP_SPEED: &str = "speed.top_speed_kn";
pub const DRIFT_TURNING_CIRCLE: &str = "speed.turning_circle_m";

// ---------------------------------------------------------------------------------------------------------
// Opponents.

/// Roles that fight: the 1-vs-3 pool and the reference scenes' hostile hulls.
fn combatant(role: ClassRole) -> bool { matches!(role, ClassRole::PatrolBoat | ClassRole::FastAttack | ClassRole::Corvette | ClassRole::Frigate | ClassRole::Destroyer | ClassRole::Picket | ClassRole::Minesweeper) }

/// Combine combatant classes of one act in data order: no civilians, auxiliaries, drones, submarines,
/// capitals or ace ships, and only classes with a blueprint.
fn pool(campaign: &CampaignData, act: Option<Act>) -> Vec<(&ClassId, &EnemyClassDef)> {
    let aces: BTreeSet<&ClassId> = campaign.aces.values().map(|a| &a.ship).collect();
    campaign.classes.iter()
        .filter(|(id, d)| act.is_none_or(|a| d.act == a) && combatant(d.role) && !d.submersible && !d.capital && !d.blueprint.trim().is_empty() && !aces.contains(id))
        .collect()
}

/// Acts in the build: those whose act data is in this build's phase (an act without data counts).
fn act_in_build(campaign: &CampaignData, act: Act) -> bool {
    campaign.acts.iter().find(|a| a.act == act).is_none_or(|a| a.phase <= content::catalogue::BUILD_PHASE)
}

/// The act the fight draws from: the lowest act in the build whose cap the design fits and that has classes,
/// else the heaviest act in the build with classes.
fn fight_act(tonnage_t: f64, campaign: &CampaignData) -> Option<Act> {
    let t = if tonnage_t.is_finite() { tonnage_t.max(0.0) } else { f64::INFINITY };
    let with_classes: Vec<Act> = Act::ALL.iter().copied().filter(|&a| act_in_build(campaign, a) && !pool(campaign, Some(a)).is_empty()).collect();
    with_classes.iter().copied().find(|a| t <= a.hull_cap_t()).or_else(|| with_classes.last().copied())
}

/// The three opponents for a design and seed (rule above). Unit: `sys-trials`.
///
/// Drawn with replacement from the act's pool by the counter-hash RNG keyed on the seed; empty when the
/// campaign data has no combatant class at all.
pub fn opponents(tonnage_t: f64, seed: u64, campaign: &CampaignData) -> Vec<ClassId> {
    let Some(act) = fight_act(tonnage_t, campaign) else { return Vec::new() };
    let pool = pool(campaign, Some(act));
    if pool.is_empty() { return Vec::new(); }
    (0..OPPONENTS as u64).map(|i| {
        let k = sim_core::rng::below(Key::new(seed, 0, Stream::Generator, SALT_OPPONENT, i), pool.len().min(u32::MAX as usize) as u32) as usize;
        pool[k.min(pool.len() - 1)].0.clone()
    }).collect()
}

// ---------------------------------------------------------------------------------------------------------
// Configs.

/// Unit vector of a compass bearing in world (x, z): 0 = north = -Z, clockwise.
fn compass(bearing_rad: f64) -> DVec2 { DVec2::new(libm::sin(bearing_rad), -libm::cos(bearing_rad)) }
/// Compass bearing from `a` toward `b`.
fn bearing_to(a: DVec2, b: DVec2) -> f64 { let d = b - a; libm::atan2(d.x, -d.y).rem_euclid(core::f64::consts::TAU) }

/// The basin map with unusable values replaced, widened to [`TRIAL_MAP_M`], with a spawn marker.
fn trial_map(campaign: &CampaignData) -> MapDef {
    let m = &campaign.theatre.basin;
    let size = |s: f64| if s.is_finite() && s > 0.0 { s.clamp(TRIAL_MAP_M, MAX_MAP_M) } else { TRIAL_MAP_M };
    let mut out = MapDef {
        size_m: [size(m.size_m[0]), size(m.size_m[1])],
        depth_m: if m.depth_m.is_finite() && m.depth_m > 0.0 { m.depth_m } else { 100.0 },
        islands: m.islands.iter().filter(|i| i.centre_m.iter().all(|c| c.is_finite()) && i.radius_m.is_finite() && i.radius_m > 0.0 && i.height_m.is_finite()).copied().collect(),
        markers: m.markers.iter().filter(|k| k.pos_m.iter().all(|c| c.is_finite()) && k.heading_deg.is_finite()).cloned().collect(),
        seed: m.seed,
    };
    if !out.markers.iter().any(|k| k.kind == MarkerKind::PlayerSpawn) {
        out.markers.push(Marker { kind: MarkerKind::PlayerSpawn, name: "spawn".into(), pos_m: [0.0, 0.0], heading_deg: 0.0 });
    }
    out
}

fn spawn_of(map: &MapDef) -> (DVec2, f64) {
    map.markers.iter().find(|m| m.kind == MarkerKind::PlayerSpawn).map(|m| (DVec2::from(m.pos_m), m.heading_deg.to_radians().rem_euclid(core::f64::consts::TAU))).unwrap_or((DVec2::ZERO, 0.0))
}

fn vessel(blueprint: BlueprintRef, side: SideId, class: Option<ClassId>, group: Option<GroupId>, pos: DVec2, heading_rad: f64, speed_kn: f64, tag: &str, role: VesselRole) -> VesselInit {
    VesselInit {
        blueprint, side, class, group, pos: DVec3::new(pos.x, 0.0, pos.y), heading_rad: heading_rad.rem_euclid(core::f64::consts::TAU), speed_kn: if speed_kn.is_finite() { speed_kn.max(0.0) } else { 0.0 }, arrives_s: 0,
        damage: None, fuel_fraction: 1.0, ammo_fraction: 1.0, ammo: BTreeMap::new(), hull_record: None, veterancy: 0.0, tag: tag.into(),
        declared: if side == ME { None } else { Some(Stance::Hostile) }, emcon: None, passengers: 0, role,
    }
}

fn sides(them: Faction) -> Vec<SideDef> {
    vec![
        SideDef { id: ME, faction: Faction::Players, name: TextKey::new("trial.side.design"), hostile_to: SideMask::only(THEM), allied: SideMask::only(ME), pvp_team: None },
        SideDef { id: THEM, faction: them, name: TextKey::new("trial.side.opponents"), hostile_to: SideMask::only(ME), allied: SideMask::only(THEM), pvp_team: None },
    ]
}

/// A trial sortie: the design (canonical form, inline) at the basin spawn as player slot 0 on side 0.
pub(crate) fn trial_base(bp: &Blueprint, sea: SeaState, seed: u64, campaign: &CampaignData, ticks: u64, speed_kn: f64) -> SortieConfig {
    let bp = super::splash::canonical(bp);
    let map = trial_map(campaign);
    let (spawn, heading) = spawn_of(&map);
    let me = vessel(BlueprintRef::Inline(bp.header.hash), ME, None, None, spawn, heading, speed_kn, super::splash::PLAYER_TAG, VesselRole::PlayerHull);
    SortieConfig {
        protocol: crate::protocol::PROTOCOL_VERSION, seed, mode: GameMode::Trial, rules: ModeRules::Trial { ticks: ticks.max(1) },
        settings: SimSettings { difficulty: Default::default(), ironman: false, ruleset: super::splash::basin_ruleset(), guest_scripts: false, sim_threads: 3 },
        act: Act::Act1, weather: super::splash::basin_weather(sea, seed), weather_changes: Vec::new(), time_of_day: TimeOfDay::Day, map, layer_depth_m: 50.0,
        sides: sides(Faction::Combine),
        players: vec![PlayerInit { slot: SLOT, client_id: 0, name: bp.header.name.clone(), side: ME, vessel: Some(0), human: true }],
        blueprints: vec![bp], vessels: vec![me], bodies: Vec::new(), groups: Vec::new(), objective: Objective::DestroyAll, ghosts: Vec::new(), tags: Vec::new(),
        actions: Vec::new(), intel: SortieIntel::default(), cards: Vec::new(), operation: None, region: None, theatre_hour: 12, splash: None, tutorial: None, time_limit_s: None,
    }
}

pub(crate) fn ticks_of(seconds: f64) -> u64 { if seconds.is_finite() && seconds > 0.0 { libm::round(seconds * TICKS_PER_S as f64).min(1.0e9) as u64 } else { 0 } }

/// The Speed gauge's top speed of a design, knots (the start speed of the turning and heel trials).
fn predicted_top_kn(bp: &Blueprint) -> f64 {
    let cat = content::catalogue::catalogue();
    let prefab = sim_core::vessel::build_prefab(bp, cat);
    let kn = super::gauges::speed_gauge(&prefab, cat).top_speed_kn;
    if kn.is_finite() { kn.max(0.0) } else { 0.0 }
}

/// Sortie configs of the individual trials, all on the basin map. Unit: `sys-trials`.
pub fn turning_config(bp: &Blueprint, sea: SeaState, campaign: &CampaignData) -> SortieConfig {
    trial_base(bp, sea, TRIAL_SEED, campaign, ticks_of(TURN_LEAD_S + TURN_MAX_S), predicted_top_kn(bp))
}
pub fn acceleration_config(bp: &Blueprint, sea: SeaState, campaign: &CampaignData) -> SortieConfig {
    trial_base(bp, sea, TRIAL_SEED, campaign, ticks_of(ACCEL_MAX_S), 0.0)
}
pub fn heel_recovery_config(bp: &Blueprint, sea: SeaState, campaign: &CampaignData) -> SortieConfig {
    trial_base(bp, sea, TRIAL_SEED, campaign, ticks_of(HEEL_LEAD_S + HEEL_TURN_MAX_S + HEEL_RECOVER_MAX_S), predicted_top_kn(bp))
}
pub fn fight_config(bp: &Blueprint, seed: u64, spec: &BatterySpec, campaign: &CampaignData) -> SortieConfig {
    let tonnage = content::stats::compute_stats(bp, content::catalogue::catalogue()).tonnage_t;
    fight_config_for(bp, tonnage, seed, spec, campaign)
}

fn fight_config_for(bp: &Blueprint, tonnage_t: f64, seed: u64, spec: &BatterySpec, campaign: &CampaignData) -> SortieConfig {
    let mut c = trial_base(bp, spec.sea, seed, campaign, ticks_of(spec.fight_s).max(1), FIGHT_OWN_SPEED_KN);
    let (spawn, heading) = spawn_of(&c.map);
    let draw = |i: u64| sim_core::rng::uniform(Key::new(seed, 0, Stream::Generator, SALT_FIGHT, i));
    let bearing = heading + (2.0 * draw(0) - 1.0) * FIGHT_BEARING_JITTER_DEG.to_radians();
    let range = FIGHT_RANGE_M + (2.0 * draw(1) - 1.0) * FIGHT_RANGE_JITTER_M;
    let centre = spawn + compass(bearing) * range;
    let toward = bearing_to(centre, spawn);
    let across = compass(bearing + core::f64::consts::FRAC_PI_2);
    let ids = opponents(tonnage_t, seed, campaign);
    let n = ids.len();
    for (i, id) in ids.iter().enumerate() {
        let Some(def) = campaign.classes.get(id) else { continue };
        let offset = (i as f64 - (n as f64 - 1.0) * 0.5) * FIGHT_SPACING_M;
        c.vessels.push(vessel(BlueprintRef::Stock(def.blueprint.clone()), THEM, Some(id.clone()), Some(GroupId(0)), centre + across * offset, toward, FIGHT_ENEMY_SPEED_KN, "opponent", VesselRole::Combatant));
        if def.act > c.act { c.act = def.act; }
        c.intel.order_of_battle.push(id.clone());
        c.intel.declared_hostile.push(id.clone());
    }
    c.intel.order_of_battle.sort();
    c.intel.order_of_battle.dedup();
    c.intel.declared_hostile = c.intel.order_of_battle.clone();
    if n > 0 {
        c.groups.push(GroupInit { id: GroupId(0), side: THEM, kind: FleetKind::HunterKiller, formation: Formation::LineAbreast, orders: GroupOrders::Hunt, ace: None, theatre_group: None, tag: "opponents".into() });
    }
    c
}

/// The complexity trial's target drone: the lowest drone class, else the lowest patrol boat (as the splash).
fn drone_class(campaign: &CampaignData) -> Option<(&ClassId, &EnemyClassDef)> {
    let lowest = |role: ClassRole| campaign.classes.iter().filter(|(_, d)| d.role == role && !d.submersible && !d.capital && !d.blueprint.trim().is_empty()).min_by_key(|(_, d)| (d.act, d.tier));
    lowest(ClassRole::Drone).or_else(|| lowest(ClassRole::PatrolBoat))
}

pub fn complexity_config(bp: &Blueprint, campaign: &CampaignData) -> SortieConfig {
    let mut c = trial_base(bp, SeaState::BEAUFORT_4, TRIAL_SEED, campaign, ticks_of(COMPLEXITY_S), 0.0);
    c.sides = sides(Faction::Target);
    c.vessels[0].emcon = Some(Emcon::Active);
    let (spawn, heading) = spawn_of(&c.map);
    if let Some((id, def)) = drone_class(campaign) {
        let at = spawn + compass(heading) * COMPLEXITY_DRONE_M;
        c.vessels.push(vessel(BlueprintRef::Stock(def.blueprint.clone()), THEM, Some(id.clone()), None, at, heading + core::f64::consts::FRAC_PI_2, 0.0, super::splash::DRONE_TAG, VesselRole::Target));
        c.act = def.act;
        c.intel.order_of_battle.push(id.clone());
        c.intel.declared_hostile.push(id.clone());
    }
    c
}

/// The complexity trial with the design taken out: the baseline whose work is subtracted.
fn complexity_baseline(config: &SortieConfig) -> SortieConfig {
    let mut c = config.clone();
    c.vessels.remove(0);
    c.players.clear();
    c.blueprints.clear();
    c
}

// ---------------------------------------------------------------------------------------------------------
// Driving a trial.

fn helm_command() -> CommandEnvelope { CommandEnvelope { player: SLOT, seq: 0, client_render_tick: 0, rtt_ms: 0, command: Command::TakeControl { role: ControlRole::Helm, on: true } } }

/// The design's hull, while it exists.
pub(crate) fn own_vessel(sim: &Sim) -> Option<EntityId> { sim.ships().of_player(SLOT).map(|s| s.vessel) }

#[derive(Clone, Copy, Debug)]
pub(crate) struct Sample { pub com: DVec2, pub speed_mps: f64, pub heading_rad: f64, pub heel_deg: f64, pub afloat: bool }

pub(crate) fn sample(sim: &Sim, e: EntityId) -> Option<Sample> {
    let v = sim.world().vessel(e)?;
    let pose = v.kin.pose;
    if !(pose.pos.is_finite() && pose.rot.is_finite()) { return None; }
    let com = pose.pos + pose.rot * if v.mass.com.is_finite() { v.mass.com } else { DVec3::ZERO };
    let vel = if v.kin.lin_vel.is_finite() { v.kin.lin_vel } else { DVec3::ZERO };
    let fwd = pose.rot * DVec3::X;
    Some(Sample {
        com: DVec2::new(com.x, com.z), speed_mps: libm::sqrt(vel.x * vel.x + vel.z * vel.z), heading_rad: libm::atan2(fwd.x, -fwd.z),
        heel_deg: super::splash::heel_deg(&pose), afloat: matches!(v.status, VesselStatus::Active | VesselStatus::Disabled),
    })
}

/// Step a sim up to `max_ticks`, calling `each` after every step with that step's events; `each` sets the
/// next tick's input and returns false to stop. Checks the deadline once per simulated second. Returns false
/// when the deadline stopped it.
fn drive(sim: &mut Sim, max_ticks: u64, deadline: &mut dyn Deadline, each: &mut dyn FnMut(&mut Sim, u64, &[OutEvent]) -> bool) -> bool {
    for i in 0..max_ticks {
        if i % TICKS_PER_S == 0 && i > 0 && deadline.expired() { return false; }
        sim.set_host_load(HostLoad::IDLE);
        sim.step();
        let events = sim.drain_events();
        if !each(sim, i + 1, &events) || sim.is_over() { break; }
    }
    true
}

pub(crate) fn set_helm(sim: &mut Sim, tick: u64, throttle: f32, rudder: f32) {
    sim.set_input(SLOT, PlayerInput { input_tick: tick + 1, client_render_tick: tick, throttle, rudder, ..Default::default() });
}

/// A started trial sim with the helm in hand: full throttle, rudder amidships.
pub(crate) fn start_manual(config: SortieConfig, deps: SimDeps) -> Option<Sim> {
    let mut sim = Sim::new(config, deps).ok()?;
    sim.apply(&[helm_command()]);
    set_helm(&mut sim, 0, 1.0, 0.0);
    Some(sim)
}

/// Accumulated heading change from a start, unwrapped (positive clockwise).
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct HeadingMeter { last: Option<f64>, turned_rad: f64 }
impl HeadingMeter {
    pub(crate) fn update(&mut self, heading: f64) -> f64 {
        if let Some(l) = self.last {
            let mut d = (heading - l).rem_euclid(core::f64::consts::TAU);
            if d > core::f64::consts::PI { d -= core::f64::consts::TAU; }
            if d.is_finite() { self.turned_rad += d; }
        }
        self.last = Some(heading);
        self.turned_rad.abs().to_degrees()
    }
}

/// Acceleration measurement from per-tick speeds (m/s): (top speed, time to 90% of it in s).
pub(crate) fn top_and_t90(speeds: &[f64]) -> (f64, f64) {
    let n = speeds.len();
    if n == 0 { return (0.0, 0.0); }
    let k = (ticks_of(TOP_MEAN_S) as usize).clamp(1, n);
    let top = speeds[n - k..].iter().map(|v| if v.is_finite() { v.max(0.0) } else { 0.0 }).sum::<f64>() / k as f64;
    if top <= 0.05 * sim_core::time::KN { return (0.0, 0.0); }
    let i = speeds.iter().position(|v| *v >= 0.9 * top).unwrap_or(n - 1);
    (top, i as f64 / TICKS_PER_S as f64)
}

/// True once the last window's mean speed is less than [`PLATEAU_RISE`] above the window before it.
pub(crate) fn plateaued(speeds: &[f64]) -> bool {
    let w = ticks_of(PLATEAU_WINDOW_S) as usize;
    if w == 0 || (speeds.len() as u64) < ticks_of(PLATEAU_MIN_S) || speeds.len() < 2 * w { return false; }
    let n = speeds.len();
    let mean = |s: &[f64]| s.iter().sum::<f64>() / s.len() as f64;
    let (a, b) = (mean(&speeds[n - 2 * w..n - w]), mean(&speeds[n - w..]));
    b - a <= PLATEAU_RISE * b.abs().max(1e-6)
}

/// Least-squares circle through points (Kasa fit): centre and radius. `None` for fewer than 3 points or
/// points on a line.
pub(crate) fn fit_circle(points: &[DVec2]) -> Option<(DVec2, f64)> {
    if points.len() < 3 { return None; }
    let n = points.len() as f64;
    let mean = points.iter().fold(DVec2::ZERO, |a, p| a + *p) / n;
    let (mut suu, mut svv, mut suv, mut suuu, mut svvv, mut suvv, mut svuu) = (0.0f64, 0.0f64, 0.0f64, 0.0f64, 0.0f64, 0.0f64, 0.0f64);
    for p in points {
        let (u, v) = (p.x - mean.x, p.y - mean.y);
        suu += u * u; svv += v * v; suv += u * v;
        suuu += u * u * u; svvv += v * v * v; suvv += u * v * v; svuu += v * u * u;
    }
    let det = suu * svv - suv * suv;
    let scale = (suu + svv).max(1e-12);
    if det.abs() <= 1e-9 * scale * scale || det.is_nan() { return None; }
    let (r1, r2) = (0.5 * (suuu + suvv), 0.5 * (svvv + svuu));
    let uc = (r1 * svv - r2 * suv) / det;
    let vc = (suu * r2 - suv * r1) / det;
    let r = libm::sqrt(uc * uc + vc * vc + (suu + svv) / n);
    if r.is_finite() && uc.is_finite() && vc.is_finite() { Some((mean + DVec2::new(uc, vc), r)) } else { None }
}

/// Settling time of a heel record taken after rudder amidships (one value per tick): seconds until the heel
/// stays within `band` for [`HEEL_HOLD_S`]; [`HEEL_RECOVER_MAX_S`] when it never does.
pub(crate) fn settle_time(heel_deg: &[f64], band: f64) -> f64 {
    let hold = ticks_of(HEEL_HOLD_S).max(1) as usize;
    let mut run = 0usize;
    for (i, h) in heel_deg.iter().enumerate() {
        if h.is_finite() && *h <= band { run += 1; } else { run = 0; }
        if run >= hold { return (i + 1 - hold) as f64 / TICKS_PER_S as f64; }
    }
    HEEL_RECOVER_MAX_S
}

/// (top speed in knots, time to 90% in s), or `None` when the deadline stopped the run.
fn run_acceleration(config: SortieConfig, deps: SimDeps, deadline: &mut dyn Deadline) -> Option<(f64, f64)> {
    let mut sim = start_manual(config, deps)?;
    let own = own_vessel(&sim)?;
    let mut speeds: Vec<f64> = Vec::with_capacity(ticks_of(ACCEL_MAX_S) as usize);
    let finished = drive(&mut sim, ticks_of(ACCEL_MAX_S), deadline, &mut |sim, t, _| {
        let Some(s) = sample(sim, own) else { return false };
        speeds.push(s.speed_mps);
        set_helm(sim, t, 1.0, 0.0);
        s.afloat && !(t % TICKS_PER_S == 0 && plateaued(&speeds))
    });
    if !finished { return None; }
    let (top, t90) = top_and_t90(&speeds);
    Some((top / sim_core::time::KN, t90))
}

/// Steady turning diameter in metres (0 = cannot turn), or `None` when the deadline stopped the run.
fn run_turning(config: SortieConfig, deps: SimDeps, deadline: &mut dyn Deadline) -> Option<f64> {
    let mut sim = start_manual(config, deps)?;
    let own = own_vessel(&sim)?;
    let lead = ticks_of(TURN_LEAD_S);
    let mut meter = HeadingMeter::default();
    let mut points: Vec<DVec2> = Vec::new();
    let mut turned = 0.0;
    let finished = drive(&mut sim, lead + ticks_of(TURN_MAX_S), deadline, &mut |sim, t, _| {
        let Some(s) = sample(sim, own) else { return false };
        if t < lead { set_helm(sim, t, 1.0, 0.0); return s.afloat; }
        turned = meter.update(s.heading_rad);
        if turned >= TURN_SAMPLE_FROM_DEG && t % TURN_SAMPLE_TICKS == 0 { points.push(s.com); }
        set_helm(sim, t, 1.0, 1.0);
        s.afloat && turned < TURN_SAMPLE_TO_DEG
    });
    if !finished { return None; }
    if turned < TURN_MIN_DEG { return Some(0.0); }
    Some(fit_circle(&points).map_or(0.0, |(_, r)| 2.0 * r))
}

/// (heel recovery s, peak heel deg), or `None` when the deadline stopped the run.
fn run_heel(config: SortieConfig, deps: SimDeps, deadline: &mut dyn Deadline) -> Option<(f64, f64)> {
    let mut sim = start_manual(config, deps)?;
    let own = own_vessel(&sim)?;
    let lead = ticks_of(HEEL_LEAD_S);
    let base_from = lead.saturating_sub(ticks_of(3.0));
    let turn_max = ticks_of(HEEL_TURN_MAX_S);
    let mut meter = HeadingMeter::default();
    let (mut baseline, mut peak) = (0.0f64, 0.0f64);
    let mut released: Option<u64> = None;
    let mut after: Vec<f64> = Vec::new();
    let mut capsized = false;
    let finished = drive(&mut sim, lead + turn_max + ticks_of(HEEL_RECOVER_MAX_S), deadline, &mut |sim, t, _| {
        let Some(s) = sample(sim, own) else { capsized = true; return false };
        if !s.afloat { capsized = true; peak = peak.max(s.heel_deg); return false; }
        if t < lead {
            if t >= base_from { baseline = baseline.max(s.heel_deg); }
            set_helm(sim, t, 1.0, 0.0);
            return true;
        }
        peak = peak.max(s.heel_deg);
        match released {
            None => {
                let turned = meter.update(s.heading_rad);
                if turned >= HEEL_TURN_DEG || t >= lead + turn_max { released = Some(t); set_helm(sim, t, 1.0, 0.0); } else { set_helm(sim, t, 1.0, 1.0); }
                true
            }
            Some(_) => {
                after.push(s.heel_deg);
                set_helm(sim, t, 1.0, 0.0);
                // Stop once settled for the hold time.
                settle_time(&after, settled_band(baseline)) >= HEEL_RECOVER_MAX_S
            }
        }
    });
    if !finished { return None; }
    let recovery = if capsized || released.is_none() { HEEL_RECOVER_MAX_S } else { settle_time(&after, settled_band(baseline)) };
    Some((recovery, if peak.is_finite() { peak } else { 180.0 }))
}

fn settled_band(baseline_deg: f64) -> f64 { (if baseline_deg.is_finite() { baseline_deg } else { 0.0 } + HEEL_SETTLED_DEG).max(HEEL_SETTLED_MIN_DEG) }

/// What one fight seed produced.
#[derive(Clone, Debug, Default, PartialEq)]
struct SeedFight { score: f64, outcome: u8, hull_left: f64, rounds: u64, kills: u64, salvos: u32, salvo_hits: u32 }

/// Per gun mount: the first salvo's tick, its projectiles, and whether one hit.
#[derive(Default)]
struct SalvoLog { first: BTreeMap<PartSlot, (u64, BTreeSet<ProjectileId>, bool)> }
impl SalvoLog {
    fn observe(&mut self, own: EntityId, events: &[OutEvent]) {
        for e in events {
            if !e.audience.admits(SLOT, ME) { continue; }
            let crate::protocol::event::SimEvent::Phys(p) = &e.event else { continue };
            match p {
                PhysEvent::ProjectileSpawned(ps) if ps.shooter == own && !ps.point_defence => {
                    let entry = self.first.entry(ps.mount).or_insert_with(|| (ps.tick, BTreeSet::new(), false));
                    if entry.0 == ps.tick { entry.1.insert(ps.id); }
                }
                PhysEvent::ShellImpact { projectile, vessel, shooter, .. } if *shooter == own && *vessel != own => {
                    for (_, ids, hit) in self.first.values_mut() { if ids.contains(projectile) { *hit = true; } }
                }
                _ => {}
            }
        }
    }
    fn tally(&self) -> (u32, u32) {
        let salvos = self.first.values().filter(|(_, ids, _)| !ids.is_empty()).count() as u32;
        let hits = self.first.values().filter(|(_, ids, hit)| !ids.is_empty() && *hit).count() as u32;
        (salvos, hits)
    }
}

fn out_of_fight(sim: &Sim, e: EntityId) -> bool { sim.world().vessel(e).is_none_or(|v| v.status != VesselStatus::Active) }

fn run_fight(config: SortieConfig, deps: SimDeps, deadline: &mut dyn Deadline) -> Option<SeedFight> {
    let ticks = match config.rules { ModeRules::Trial { ticks } => ticks, _ => ticks_of(180.0) };
    let mut sim = Sim::new(config, deps).ok()?;
    let own = own_vessel(&sim)?;
    let enemies: Vec<EntityId> = sim.ships().ships.iter().filter(|s| s.side != ME).map(|s| s.vessel).collect();
    // Steer for the opponents at top speed; the officers fight.
    let centre = {
        let pts: Vec<DVec2> = enemies.iter().filter_map(|e| sim.world().vessel(*e)).map(|v| DVec2::new(v.kin.pose.pos.x, v.kin.pose.pos.z)).collect();
        if pts.is_empty() { None } else { Some(pts.iter().fold(DVec2::ZERO, |a, p| a + *p) / pts.len() as f64) }
    };
    if let Some(c) = centre {
        let top = sim.ships().get(own).map(|s| s.design.turn.top_speed_mps / sim_core::time::KN).filter(|v| v.is_finite() && *v > 0.0).unwrap_or(FIGHT_OWN_SPEED_KN);
        sim.apply(&[CommandEnvelope { player: SLOT, seq: 0, client_render_tick: 0, rtt_ms: 0, command: Command::SetCourse { waypoints: vec![c], speed_kn: Some(top), append: false } }]);
    }
    let mut salvos = SalvoLog::default();
    let finished = drive(&mut sim, ticks, deadline, &mut |sim, _, events| {
        salvos.observe(own, events);
        !(out_of_fight(sim, own) || (!enemies.is_empty() && enemies.iter().all(|e| out_of_fight(sim, *e))))
    });
    if !finished { return None; }
    let own_out = out_of_fight(&sim, own);
    let enemies_out = !enemies.is_empty() && enemies.iter().all(|e| out_of_fight(&sim, *e));
    let result = match sim.result() { Some(r) => r.clone(), None => sim.finish(Outcome::Draw).clone() };
    let s = &result.stats;
    let (salvos, salvo_hits) = salvos.tally();
    let fin = |v: f64| if v.is_finite() { v } else { 0.0 };
    Some(SeedFight {
        score: fin(crate::objectives::trial_score(s)),
        outcome: if own_out { 0 } else if enemies_out { 2 } else { 1 },
        hull_left: (1.0 - fin(s.own_cells_lost_fraction)).clamp(0.0, 1.0),
        rounds: s.rounds_fired as u64 + s.missiles_fired as u64 + s.torpedoes_fired as u64,
        kills: result.kills.iter().filter(|k| k.side != ME).count() as u64,
        salvos, salvo_hits,
    })
}

fn fight_score(seeds: &[SeedFight]) -> FightScore {
    if seeds.is_empty() { return FightScore::default(); }
    let n = seeds.len() as f64;
    let rounds: u64 = seeds.iter().map(|s| s.rounds).sum();
    let kills: u64 = seeds.iter().map(|s| s.kills).sum();
    let (salvos, hits) = seeds.iter().fold((0u32, 0u32), |(a, b), s| (a + s.salvos, b + s.salvo_hits));
    FightScore {
        score: seeds.iter().map(|s| s.score).sum::<f64>() / n,
        per_seed: seeds.iter().map(|s| s.score).collect(),
        wins: seeds.iter().filter(|s| s.outcome == 2).count().min(u8::MAX as usize) as u8,
        mean_hull_fraction_left: seeds.iter().map(|s| s.hull_left).sum::<f64>() / n,
        rounds_per_kill: if kills > 0 { rounds as f64 / kills as f64 } else { rounds as f64 },
        first_salvo_hit_rate: if salvos > 0 { hits as f64 / salvos as f64 } else { 0.0 },
        outcomes: seeds.iter().map(|s| s.outcome).collect(),
        hull_left: seeds.iter().map(|s| s.hull_left).collect(),
    }
}

/// Scene totals of the work counters over a run, and the ticks run.
#[derive(Clone, Copy, Debug, Default)]
struct WorkSum { cuboids: u64, buoyancy: u64, conduits: u64, compartments: u64, graph: u64, luau: u64, ticks: u64 }

fn run_work(config: SortieConfig, deps: SimDeps, deadline: &mut dyn Deadline) -> Option<WorkSum> {
    let ticks = match config.rules { ModeRules::Trial { ticks } => ticks, _ => ticks_of(COMPLEXITY_S) };
    let mut sim = Sim::new(config, deps).ok()?;
    let mut sum = WorkSum::default();
    let finished = drive(&mut sim, ticks, deadline, &mut |sim, _, _| {
        let (w, s) = (*sim.world().work(), *sim.work());
        sum.cuboids += w.cuboids as u64;
        sum.buoyancy += w.buoyancy_samples as u64;
        sum.conduits += w.conduit_segments as u64;
        sum.compartments += w.compartments as u64;
        sum.graph += s.graph_node_evals as u64;
        sum.luau += s.luau_interrupts as u64;
        sum.ticks += 1;
        true
    });
    finished.then_some(sum)
}

/// Per-tick work of the design: the scene's totals less the drone-only baseline, averaged over the run.
pub(crate) fn design_counts(scene: (u64, u64, u64, u64, u64, u64), baseline: (u64, u64, u64, u64, u64, u64), ticks: u64) -> ComplexityCounts {
    if ticks == 0 { return ComplexityCounts::default(); }
    let per = |a: u64, b: u64| (a.saturating_sub(b) as f64 / ticks as f64).round().min(u32::MAX as f64) as u32;
    ComplexityCounts {
        cuboids: per(scene.0, baseline.0), buoyancy_samples: per(scene.1, baseline.1), conduit_segments: per(scene.2, baseline.2),
        compartments: per(scene.3, baseline.3), graph_node_evals: per(scene.4, baseline.4), luau_interrupts: per(scene.5, baseline.5),
    }
}

fn run_complexity(config: SortieConfig, make_deps: &mut dyn FnMut() -> SimDeps, design: &Arc<crate::ship::ShipDesign>, deadline: &mut dyn Deadline) -> Option<ComplexityCounts> {
    let baseline_config = complexity_baseline(&config);
    let scene = run_work(config, with_design(make_deps(), design), deadline)?;
    let base = if baseline_config.vessels.is_empty() { WorkSum::default() } else { run_work(baseline_config, make_deps(), deadline)? };
    let t = |w: &WorkSum| (w.cuboids, w.buoyancy, w.conduits, w.compartments, w.graph, w.luau);
    // The baseline ran as long as the scene unless one stopped early; scale it to the scene's length.
    let scaled = if base.ticks > 0 && base.ticks != scene.ticks {
        let f = |v: u64| ((v as f64) * scene.ticks as f64 / base.ticks as f64) as u64;
        (f(base.cuboids), f(base.buoyancy), f(base.conduits), f(base.compartments), f(base.graph), f(base.luau))
    } else { t(&base) };
    Some(design_counts(t(&scene), scaled, scene.ticks))
}

fn with_design(mut deps: SimDeps, design: &Arc<crate::ship::ShipDesign>) -> SimDeps { deps.designs.insert(design.clone()); deps }

/// Run the whole battery for a design. `make_deps` builds fresh `SimDeps` per run (the designs cache is
/// shared through it). Stops early and returns the partial scorecard when the deadline expires.
/// Unit: `sys-trials`.
///
/// Order: acceleration, turning, heel recovery, complexity, then the fight seeds, so a deadline keeps the
/// cheap numbers. A design that does not validate under the basin ruleset gets an empty scorecard.
pub fn run_battery(bp: &Blueprint, spec: &BatterySpec, campaign: &CampaignData, make_deps: &mut dyn FnMut() -> SimDeps, deadline: &mut dyn Deadline) -> Scorecard {
    let bp = super::splash::canonical(bp);
    let catalogue = make_deps().catalogue;
    let mut card = Scorecard { design: bp.header.hash, tuning_version: catalogue.tuning.version, short_trial: spec.short, ..Default::default() };
    let prefab = Arc::new(sim_core::vessel::build_prefab(&bp, &catalogue));
    let design = Arc::new(crate::ship::ShipDesign::build(&bp, prefab.clone(), &catalogue, &super::splash::basin_ruleset()));
    let gauge = super::gauges::speed_gauge(&prefab, &catalogue);
    let tonnage = super::gauges::calc::loaded_mass(&prefab, &catalogue).mass_kg / 1_000.0;
    let fight_seeds = (1..=spec.seeds.max(1) as u64).collect::<Vec<_>>();
    // Every class the seeds draw, first seen first.
    for &s in &fight_seeds {
        for c in opponents(tonnage, s, campaign) { if !card.opponents.contains(&c) { card.opponents.push(c); } }
    }
    let manoeuvre_sea = SeaState::CALM;
    let fresh = |make_deps: &mut dyn FnMut() -> SimDeps| with_design(make_deps(), &design);

    if deadline.expired() { return card; }
    let mut cfg = acceleration_config(&bp, manoeuvre_sea, campaign);
    let Some((top, t90)) = run_acceleration(cfg, fresh(make_deps), deadline) else { return card };
    card.top_speed_kn = top;
    card.time_to_90pct_speed_s = t90;

    if deadline.expired() { return finish_card(card, &gauge, &catalogue); }
    cfg = trial_base(&bp, manoeuvre_sea, TRIAL_SEED, campaign, ticks_of(TURN_LEAD_S + TURN_MAX_S), gauge.top_speed_kn);
    let Some(circle) = run_turning(cfg, fresh(make_deps), deadline) else { return finish_card(card, &gauge, &catalogue) };
    card.turning_circle_m = circle;

    if deadline.expired() { return finish_card(card, &gauge, &catalogue); }
    cfg = trial_base(&bp, manoeuvre_sea, TRIAL_SEED, campaign, ticks_of(HEEL_LEAD_S + HEEL_TURN_MAX_S + HEEL_RECOVER_MAX_S), gauge.top_speed_kn);
    let Some((recovery, peak)) = run_heel(cfg, fresh(make_deps), deadline) else { return finish_card(card, &gauge, &catalogue) };
    card.heel_recovery_s = recovery;
    card.peak_heel_deg = peak;

    if deadline.expired() { return finish_card(card, &gauge, &catalogue); }
    let mut cc = complexity_config(&bp, campaign);
    cc.weather = super::splash::basin_weather(spec.sea, TRIAL_SEED);
    let Some(counts) = run_complexity(cc, make_deps, &design, deadline) else { return finish_card(card, &gauge, &catalogue) };
    card.complexity = counts;
    card.complexity_points = complexity_points(&counts, &catalogue.tuning.complexity);

    let mut seeds: Vec<SeedFight> = Vec::with_capacity(fight_seeds.len());
    for &seed in &fight_seeds {
        if deadline.expired() { break; }
        let cfg = fight_config_for(&bp, tonnage, seed, spec, campaign);
        let Some(f) = run_fight(cfg, fresh(make_deps), deadline) else { break };
        seeds.push(f);
    }
    card.fight = fight_score(&seeds);
    finish_card(card, &gauge, &catalogue)
}

/// Drift entries for what was measured.
fn finish_card(mut card: Scorecard, gauge: &super::gauges::SpeedGauge, _catalogue: &Catalogue) -> Scorecard {
    let mut push = |name: &str, predicted: f64, simulated: f64| {
        let (p, s) = (if predicted.is_finite() { predicted } else { 0.0 }, if simulated.is_finite() { simulated } else { 0.0 });
        if p > 0.0 || s > 0.0 { card.drift.push(Drift { gauge: name.into(), predicted: p, simulated: s }); }
    };
    if card.top_speed_kn > 0.0 || card.time_to_90pct_speed_s > 0.0 || gauge.top_speed_kn > 0.0 { push(DRIFT_TOP_SPEED, gauge.top_speed_kn, card.top_speed_kn); }
    let circle = card.turning_circle_m;
    push(DRIFT_TURNING_CIRCLE, gauge.turning_circle_m, circle);
    card
}

/// The fixed scenes every performance gate and the in-game benchmark use (L1445-1447).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ReferenceScene {
    /// LANTERN plus 40 full-physics hulls, 4 of them 150 m capitals, 2,000 live projectiles.
    ReferenceBattle,
    /// 4 player hulls against a 25-hull base operation plus 3 task groups of 5.
    CoopReference,
    /// 20 full-physics hostiles (at most 4 over 90 m), 8 drones, 6 enemy aircraft, up to 24 missiles in
    /// flight, 1,000 projectiles.
    MinimumSwarm,
}

/// Capital classes (the hulls over 90 m), data order.
fn capitals(campaign: &CampaignData) -> Vec<(&ClassId, &EnemyClassDef)> {
    campaign.classes.iter().filter(|(_, d)| d.capital && !d.submersible && !d.blueprint.trim().is_empty()).collect()
}

/// `n` hostile classes for a scene: `caps` capitals first (cycled), the rest drawn by seed from every act's
/// combatant pool.
fn scene_classes(campaign: &CampaignData, seed: u64, n: usize, caps: usize) -> Vec<(ClassId, String)> {
    let capital = capitals(campaign);
    let pool = pool(campaign, None);
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let pick = if i < caps && !capital.is_empty() { Some(capital[i % capital.len()]) } else if !pool.is_empty() {
            let k = sim_core::rng::below(Key::new(seed, 0, Stream::Generator, SALT_SCENE, i as u64), pool.len().min(u32::MAX as usize) as u32) as usize;
            Some(pool[k.min(pool.len() - 1)])
        } else { None };
        if let Some((id, d)) = pick { out.push((id.clone(), d.blueprint.clone())); }
    }
    out
}

/// Build a reference scene's config from stock designs. Deterministic for the seed. Unit: `sys-trials`.
///
/// - Reference battle: LANTERN (the 90 m starter) at the basin spawn against 40 hostile hulls in 8 hunting
///   task groups of 5 on a ring 9 to 15 km out, the first 4 groups each led by a capital.
/// - Co-op reference: the 4 starter hulls in line against a 25-hull base operation (5 patrols of 5 guarding
///   markers around the players) and 3 hunting task groups of 5.
/// - Minimum swarm: LANTERN against 20 hostile hulls (4 capitals) in 4 hunting groups and 6 strike aircraft
///   on air patrol over the player; drones come from the drone bays of the hulls fielded.
///
/// All at Beaufort 4 by day, Act 4 rules, as a trial of [`SCENE_TICKS`] ticks.
pub fn reference_config(scene: ReferenceScene, seed: u64, campaign: &CampaignData) -> SortieConfig {
    let map = trial_map(campaign);
    let (spawn, heading) = spawn_of(&map);
    let mut c = SortieConfig {
        protocol: crate::protocol::PROTOCOL_VERSION, seed, mode: GameMode::Trial, rules: ModeRules::Trial { ticks: SCENE_TICKS },
        settings: SimSettings { difficulty: Default::default(), ironman: false, ruleset: super::splash::basin_ruleset(), guest_scripts: false, sim_threads: 3 },
        act: Act::Act4, weather: super::splash::basin_weather(SeaState::BEAUFORT_4, seed), weather_changes: Vec::new(), time_of_day: TimeOfDay::Day, map, layer_depth_m: 50.0,
        sides: sides(Faction::Combine), players: Vec::new(), blueprints: Vec::new(), vessels: Vec::new(), bodies: Vec::new(), groups: Vec::new(),
        objective: Objective::DestroyAll, ghosts: Vec::new(), tags: Vec::new(), actions: Vec::new(), intel: SortieIntel::default(), cards: Vec::new(),
        operation: None, region: None, theatre_hour: 12, splash: None, tutorial: None, time_limit_s: None,
    };
    let player_hulls: Vec<&str> = match scene { ReferenceScene::CoopReference => COOP_HULLS.to_vec(), _ => vec![LANTERN] };
    let across = compass(heading + core::f64::consts::FRAC_PI_2);
    for (i, path) in player_hulls.iter().enumerate() {
        let at = spawn + across * ((i as f64 - (player_hulls.len() as f64 - 1.0) * 0.5) * 400.0);
        c.vessels.push(vessel(BlueprintRef::Stock((*path).into()), ME, None, None, at, heading, 15.0, super::splash::PLAYER_TAG, VesselRole::PlayerHull));
        c.players.push(PlayerInit { slot: PlayerSlot(i as u8), client_id: i as u64 + 1, name: format!("Player {}", i + 1), side: ME, vessel: Some(i as u16), human: true });
    }
    // Hostile groups: (count per group, groups, capitals, orders, ring radius from, to).
    let (sizes, caps): (Vec<(usize, GroupOrders, FleetKind)>, usize) = match scene {
        ReferenceScene::ReferenceBattle => ((0..8).map(|_| (5, GroupOrders::Hunt, FleetKind::TaskGroup)).collect(), 4),
        ReferenceScene::CoopReference => {
            let mut g: Vec<(usize, GroupOrders, FleetKind)> = (0..5).map(|k| (5, GroupOrders::Guard { marker: format!("scene.guard.{k}"), radius_m: 3_000.0 }, FleetKind::Garrison)).collect();
            g.extend((0..3).map(|_| (5, GroupOrders::Hunt, FleetKind::TaskGroup)));
            (g, 0)
        }
        ReferenceScene::MinimumSwarm => ((0..4).map(|_| (5, GroupOrders::Hunt, FleetKind::TaskGroup)).collect(), 4),
    };
    let total: usize = sizes.iter().map(|g| g.0).sum();
    let classes = scene_classes(campaign, seed, total, caps);
    let draw = |i: u64| sim_core::rng::uniform(Key::new(seed, 1, Stream::Generator, SALT_SCENE, i));
    let groups_n = sizes.len().max(1);
    let mut next = 0usize;
    for (g, (count, orders, kind)) in sizes.into_iter().enumerate() {
        let gid = GroupId(g.min(u16::MAX as usize) as u16);
        let bearing = heading + core::f64::consts::TAU * (g as f64 + 0.5 * draw(g as u64)) / groups_n as f64;
        let range = 9_000.0 + 6_000.0 * draw(100 + g as u64);
        let centre = spawn + compass(bearing) * range;
        if let GroupOrders::Guard { marker, .. } = &orders {
            c.map.markers.push(Marker { kind: MarkerKind::Objective, name: marker.clone(), pos_m: [centre.x, centre.y], heading_deg: 0.0 });
        }
        let toward = bearing_to(centre, spawn);
        // In a capital-led group the capital comes first.
        let members: Vec<(ClassId, String)> = if caps > 0 && g < caps && g < classes.len() {
            let mut m = vec![classes[g].clone()];
            let others: Vec<(ClassId, String)> = classes.iter().skip(caps).skip(next).take(count.saturating_sub(1)).cloned().collect();
            next += others.len();
            m.extend(others);
            m
        } else {
            let m: Vec<(ClassId, String)> = classes.iter().skip(caps).skip(next).take(count).cloned().collect();
            next += m.len();
            m
        };
        let side_vec = compass(toward + core::f64::consts::FRAC_PI_2);
        for (k, (id, path)) in members.iter().enumerate() {
            let at = centre + side_vec * ((k as f64 - (members.len() as f64 - 1.0) * 0.5) * 500.0);
            c.vessels.push(vessel(BlueprintRef::Stock(path.clone()), THEM, Some(id.clone()), Some(gid), at, toward, 15.0, "scene", VesselRole::Combatant));
            c.intel.order_of_battle.push(id.clone());
        }
        c.groups.push(GroupInit { id: gid, side: THEM, kind, formation: Formation::Wedge, orders, ace: None, theatre_group: None, tag: format!("scene.group.{g}") });
    }
    if scene == ReferenceScene::MinimumSwarm {
        let gid = GroupId(groups_n.min(u16::MAX as usize - 1) as u16);
        let marker = "scene.air".to_string();
        c.map.markers.push(Marker { kind: MarkerKind::Objective, name: marker.clone(), pos_m: [spawn.x, spawn.y], heading_deg: 0.0 });
        c.groups.push(GroupInit { id: gid, side: THEM, kind: FleetKind::AirPatrol, formation: Formation::Column, orders: GroupOrders::AirPatrol { marker, radius_m: 6_000.0 }, ace: None, theatre_group: None, tag: "scene.air".into() });
        for k in 0..6u64 {
            let b = heading + core::f64::consts::TAU * k as f64 / 6.0;
            let at = spawn + compass(b) * 16_000.0;
            let dir = compass(bearing_to(at, spawn));
            c.bodies.push(BodyInit { munition: MunitionId::new("aircraft.strike"), side: THEM, group: Some(gid), pos: DVec3::new(at.x, 1_000.0, at.y), vel: DVec3::new(dir.x * 120.0, 0.0, dir.y * 120.0), arrives_s: 0 });
        }
    }
    c.intel.order_of_battle.sort();
    c.intel.order_of_battle.dedup();
    c.intel.declared_hostile = c.intel.order_of_battle.clone();
    c
}

/// Compare view: per-metric change between two scorecards of one lineage. Unit: `sys-trials`.
///
/// Every metric, in a fixed order, as (name, before, after); the view draws the change.
pub fn compare(before: &Scorecard, after: &Scorecard) -> Vec<(String, f64, f64)> {
    let row = |name: &str, f: &dyn Fn(&Scorecard) -> f64| (name.to_string(), f(before), f(after));
    vec![
        row("turning_circle_m", &|s| s.turning_circle_m),
        row("time_to_90pct_speed_s", &|s| s.time_to_90pct_speed_s),
        row("top_speed_kn", &|s| s.top_speed_kn),
        row("heel_recovery_s", &|s| s.heel_recovery_s),
        row("peak_heel_deg", &|s| s.peak_heel_deg),
        row("fight.score", &|s| s.fight.score),
        row("fight.wins", &|s| s.fight.wins as f64),
        row("fight.mean_hull_fraction_left", &|s| s.fight.mean_hull_fraction_left),
        row("fight.rounds_per_kill", &|s| s.fight.rounds_per_kill),
        row("fight.first_salvo_hit_rate", &|s| s.fight.first_salvo_hit_rate),
        row("complexity_points", &|s| s.complexity_points),
    ]
}

/// Relative difference of two values against the larger; 0 when both are 0.
fn rel(a: f64, b: f64) -> f64 {
    if !(a.is_finite() && b.is_finite()) { return if a == b { 0.0 } else { f64::INFINITY }; }
    let m = a.abs().max(b.abs());
    if m <= 1e-12 { 0.0 } else { (a - b).abs() / m }
}

/// Mac-versus-Windows tolerance check of two scorecards from the same seeds (L1300): turning circle and
/// time to 90% speed 1%; heel recovery and peak heel 2%; top speed 0.5%; at least 9 of 10 fight outcomes
/// match and mean remaining hull fraction within 5 points. Returns the failing metric names.
/// Unit: `sys-trials`.
///
/// The fight is compared over the seeds both cards ran (the shorter card's count): outcomes must match on
/// at least 9 in 10 of them, and the mean remaining hull fraction over those seeds must agree within 0.05.
/// Cards whose seed counts differ, or that carry no per-seed outcomes, fail `fight.seeds`.
pub fn cross_platform_check(a: &Scorecard, b: &Scorecard) -> Vec<String> {
    let mut out = Vec::new();
    let mut scalar = |name: &str, x: f64, y: f64, tol: f64| if rel(x, y) > tol + 1e-12 { out.push(name.to_string()); };
    scalar("turning_circle_m", a.turning_circle_m, b.turning_circle_m, 0.01);
    scalar("time_to_90pct_speed_s", a.time_to_90pct_speed_s, b.time_to_90pct_speed_s, 0.01);
    scalar("heel_recovery_s", a.heel_recovery_s, b.heel_recovery_s, 0.02);
    scalar("peak_heel_deg", a.peak_heel_deg, b.peak_heel_deg, 0.02);
    scalar("top_speed_kn", a.top_speed_kn, b.top_speed_kn, 0.005);
    let (fa, fb) = (&a.fight, &b.fight);
    let n = fa.outcomes.len().min(fb.outcomes.len());
    if fa.per_seed.len() != fb.per_seed.len() || (n == 0 && !(fa.per_seed.is_empty() && fb.per_seed.is_empty())) {
        out.push("fight.seeds".into());
        return out;
    }
    if n > 0 {
        let matches = fa.outcomes.iter().zip(&fb.outcomes).take(n).filter(|(x, y)| x == y).count();
        if matches * 10 < 9 * n { out.push("fight.outcomes".into()); }
        let mean = |v: &[f64]| { let k = v.len().min(n); if k == 0 { 0.0 } else { v[..k].iter().sum::<f64>() / k as f64 } };
        let (ha, hb) = (if fa.hull_left.is_empty() { fa.mean_hull_fraction_left } else { mean(&fa.hull_left) }, if fb.hull_left.is_empty() { fb.mean_hull_fraction_left } else { mean(&fb.hull_left) });
        if !((ha - hb).abs() <= 0.05 + 1e-12) { out.push("fight.hull_fraction".into()); }
    }
    out
}

#[cfg(test)]
mod tests;
