//! Unit `sys-theatre-abstract`. The abstract-combat model: a closed-form resolver that stands in for a
//! full-physics sortie. DESIGN.md L879 (campaign bots resolve sorties with it), L1549 (abstract combat is
//! also used for enemy-versus-enemy fights no player sensor observes).
//!
//! OWNER DECISION PENDING (L879): the model's inputs, outcome rule and its Salvage and Alert outputs must be
//! decided by the owner before the bots are built. This file fixes only the seam: the trait every resolver
//! implements and the shape of the model's input. The unit must not invent the rule; until it is decided
//! `AbstractResolver::resolve` returns a neutral aborted result (never a panic) and the bot gates run on `FullPhysicsResolver`
//! (the spec's own fallback when agreement is below 85%: 20 full-physics Act 1 runs per policy).
//! Validity check (weekly): 20 sampled sorties replayed with full physics; the bot gates count only while
//! agreement is at least 85% (same win or loss, Salvage within 15%, Alert change within 0.5).
//!
//! # How the full-physics resolver plays a sortie (choices the spec leaves open)
//! - Setup before the sortie: a `reference_setup` names a stock blueprint; its officer settings and rules
//!   replace those of every player hull (hull, parts and kits stay the player's). The changed design travels
//!   inline under its recomputed hash. A reference that does not load leaves the stock settings, so a broken
//!   path fails the "reference wins" gate instead of passing it.
//! - At the first tick every player hull gets, as L0 to L2 commands: the EMCON setting (the setup's, or
//!   Silent when ghosts are attempted and a ghost condition forbids emissions or detection), and for a
//!   single-method setup one rule per weapon of another method holding its fire (gun mounts only while no
//!   weapon is inbound, so they still defend the ship; launchers always). Methods with no weapon class of
//!   their own (mine, fire, capture, collision) hold nothing. Rules go above the hull's own and the list is
//!   cut at `MAX_RULES`.
//! - Helm, when `follow_briefing_route` is set: the briefing route (player spawn, the objective point, the
//!   exit if the objective names one), rebuilt from the config by the generator's rule. Then, by objective:
//!   - destroy all, group or site: after the last waypoint the Helm closes on the nearest hostile surface
//!     track (re-aimed every 15 s), else holds station at the objective point;
//!   - insert or extract: hold station on the shore marker for the hold, then leave by the nearest exit;
//!   - lay mines: a line through the marker along the approach course, one mine per 50 m of the count plus
//!     200 m run-in at each end, sailed at 10 kn;
//!   - photograph: the nearest unphotographed non-Friend surface track, approached to 4.5 km (at most 75% of
//!     the weather's visibility) and held, closing to half that while the camera reports it too far, beyond
//!     visibility or out of sight;
//!   - capture and cutting-out: close on a disabled enemy hull in visual range, hold 100 m off it while the
//!     boarding runs, order the prize home and (cutting-out) sail for the exit;
//!   - feint: once the hold is served, withdraw and sail for the nearest exit;
//!   - survive and escort: the route ends at the exit.
//!
//!   Without `follow_briefing_route` the hull gets no Helm orders, designations or withdrawals: its
//!   officers and standing orders do everything.
//! - Every course carries an ordered speed, because a player hull spawns under a stop order: the hull's
//!   `helm.cruise` fraction (else the Helm's default 0.7) of the Speed gauge's top speed, 95% of it when an
//!   attempted ghost condition runs against the clock, and 6 to 10 kn on a mine line.
//! - Targets: the hunt and the camera prefer contacts identified as a class of the hulls the objective
//!   names (its tag, or the site), then the nearest.
//! - While hunting, the pilot designates the track it closes on for the officers.
//! - Station actions the objective needs are started at the first tick where their target is a marker or a
//!   line (a started action waits paused until its conditions hold) and once a target exists for track
//!   targets; an aborted action is restarted after 30 s.
//! - The policy reads only the player's own snapshot (`Viewer::Player`), at 1 Hz, never the truth.
//! - A sortie the `Sim` refuses to build returns `Outcome::Aborted` (no result applies, 0 minutes).
//! - Call `resolve` inside the 3-worker sim pool for the shipping thread count; outside one it runs on
//!   rayon's global pool and the result is the same (results never depend on the pool, L1150).
use super::TheatreData;
use crate::editor::trials::{run_headless, HeadlessPolicy};
use crate::editor::NoDeadline;
use crate::prelude::*;
use crate::protocol::command::{ActionTarget, Command, CommandEnvelope, PrizeDecision};
use crate::protocol::config::{BlueprintRef, Faction, Outcome, SortieConfig, SortieResult, SortieStats, VesselRole};
use crate::protocol::snapshot::{ActionBlock, ActionState, ObjectiveState, Snapshot, Viewer};
use crate::sim::{Sim, SimDeps};
use content::blueprint::Blueprint;
use content::campaign::{GhostKind, KillMethod, MarkerKind, Objective, StationAction};
use content::graph::{Cmp, Condition, Operand, Rule, RuleValue, SinkKind, Value};
use content::part::weapons::MunitionKind;
use content::part::PartFunction;
use sim_core::time::TICK_HZ;
use sim_core::vessel::VesselStatus;

/// How the player side fights in a resolved sortie.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PlayerSetup {
    /// L0 preset, sliders and rules to apply over each hull's stored settings; `None` = stock officers.
    pub reference_setup: Option<String>,
    /// Preferred kill method for single-method policies.
    pub method: Option<content::campaign::KillMethod>,
    pub emcon: Option<Emcon>,
    /// Attempt ghost conditions.
    pub ghosts: bool,
    /// Follow the briefing route (the stock-officer winnability gate, L317).
    pub follow_briefing_route: bool,
}

/// Anything that turns a sortie config into a result.
pub trait SortieResolver {
    fn resolve(&mut self, config: &SortieConfig, setup: &PlayerSetup, data: &TheatreData) -> SortieResult;
    /// Simulated sortie minutes of the last resolve (proxy length gate, L81).
    fn last_duration_min(&self) -> f64;
}

/// Summary inputs an abstract model would read. Shape only; see the owner decision above.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AbstractInput { pub player_hulls: u8, pub player_tonnage_t: f64, pub player_build_cost: u32, pub enemy_hulls: u16, pub enemy_tonnage_t: f64, pub enemy_tiers: Vec<u8>, pub sea_state: u8, pub time_of_day: TimeOfDay, pub region_alert: f64, pub active_cards: u8, pub setup: PlayerSetup }

/// Reduce a config to the abstract input. Unit: `sys-theatre-abstract`.
///
/// - Player hulls: the vessels the config's players sail, plus any other `PlayerHull`-role vessel; tonnage
///   and build cost from `content::stats` of their designs (inline or stock).
/// - Enemy hulls: every vessel on a side hostile to a players' side (scheduled arrivals included, as the
///   commitment rule counts them, L94), except sites; tonnage from the class blueprint, else the vessel's
///   own design. `enemy_tiers` lists the class tier (1 to 5) of each enemy hull with a class, config order.
/// - `region_alert` is 0.0: neither the config nor the theatre data carries the region's Alert. A caller
///   holding the ledger (`TheatreState::regions`) overwrites it.
pub fn abstract_input(config: &SortieConfig, setup: &PlayerSetup, data: &TheatreData) -> AbstractInput {
    let player_sides: Vec<SideId> = config.sides.iter().filter(|s| s.faction == Faction::Players).map(|s| s.id).collect();
    let mut player_vessels: BTreeSet<usize> = config.players.iter().filter_map(|p| p.vessel).map(|v| v as usize).filter(|&v| v < config.vessels.len()).collect();
    for (i, v) in config.vessels.iter().enumerate() { if v.role == VesselRole::PlayerHull { player_vessels.insert(i); } }
    let hostile = |side: SideId| -> bool {
        if player_sides.contains(&side) { return false; }
        let Some(def) = config.sides.iter().find(|s| s.id == side) else { return false };
        def.faction == Faction::Combine || player_sides.iter().any(|p| def.hostile_to.has(*p))
            || config.sides.iter().any(|s| player_sides.contains(&s.id) && s.hostile_to.has(side))
    };
    let (mut p_t, mut p_cost) = (0.0f64, 0u32);
    for &i in &player_vessels {
        let (t, c) = design_facts(config, &config.vessels[i].blueprint, data);
        p_t += t;
        p_cost = p_cost.saturating_add(c);
    }
    let (mut e_hulls, mut e_t, mut tiers) = (0u16, 0.0f64, Vec::new());
    for (i, v) in config.vessels.iter().enumerate() {
        if player_vessels.contains(&i) || !hostile(v.side) || v.role == VesselRole::Site { continue; }
        e_hulls = e_hulls.saturating_add(1);
        let facts = v.class.as_ref().and_then(|c| super::opgen::class_facts(data, c)).map(|f| (f.tonnage_t, f.cost));
        e_t += facts.unwrap_or_else(|| design_facts(config, &v.blueprint, data)).0;
        if let Some(def) = v.class.as_ref().and_then(|c| data.campaign.classes.get(c)) { tiers.push(def.tier as u8 + 1); }
    }
    AbstractInput {
        player_hulls: player_vessels.len().min(u8::MAX as usize) as u8, player_tonnage_t: finite_or_zero(p_t), player_build_cost: p_cost,
        enemy_hulls: e_hulls, enemy_tonnage_t: finite_or_zero(e_t), enemy_tiers: tiers, sea_state: config.weather.sea_state.0, time_of_day: config.time_of_day,
        region_alert: 0.0, active_cards: config.cards.len().min(u8::MAX as usize) as u8, setup: setup.clone(),
    }
}

/// The closed-form resolver. Blocked on the owner decision.
#[derive(Default)]
pub struct AbstractResolver { pub last_min: f64 }
impl SortieResolver for AbstractResolver {
    /// Unit: `sys-theatre-abstract`.
    ///
    /// BLOCKED (DESIGN.md L879, and the 13 November 2026 decisions entry "Owner-pending items get a seam
    /// only"): the outcome rule and its Salvage and Alert outputs are the owner's to decide and must not be
    /// invented here. The server refuses `--resolver abstract` until then; bots use [`FullPhysicsResolver`].
    /// Until the rule exists the resolver never panics: every sortie comes back `Outcome::Aborted` with no
    /// kills, Salvage inputs or Alert-moving facts, the same neutral result a full-physics run that cannot
    /// start returns, so a caller that reaches it by mistake changes no theatre state.
    fn resolve(&mut self, config: &SortieConfig, _setup: &PlayerSetup, _data: &TheatreData) -> SortieResult {
        self.last_min = 0.0;
        aborted_result(config)
    }
    fn last_duration_min(&self) -> f64 { self.last_min }
}

/// Resolves by running the real `Sim` headless with stock officers (or the reference setup) through
/// `editor::trials::run_headless`. Slow but exact; the reference for the agreement check.
///
/// `make_deps` is not `Send`, so the resolver is not either: build it on the thread that will call
/// `resolve` (inside the sim pool's `install` closure, not outside it). About 45 times real time on the
/// owner's Mac for an Act 1 sortie with one player hull.
pub struct FullPhysicsResolver { pub make_deps: Box<dyn FnMut() -> SimDeps>, pub max_ticks: u64, pub last_min: f64 }
impl SortieResolver for FullPhysicsResolver {
    /// Unit: `sys-theatre-abstract`. See the module docs for how the sortie is played.
    fn resolve(&mut self, config: &SortieConfig, setup: &PlayerSetup, data: &TheatreData) -> SortieResult {
        self.last_min = 0.0;
        let prepared = prepare_config(config, setup);
        let mut pilot = Pilot::new(&prepared, setup, data);
        let deps = (self.make_deps)();
        match run_headless(prepared, deps, self.max_ticks, &mut pilot, &mut NoDeadline) {
            Ok(r) => {
                let min = r.duration_ticks as f64 / (TICK_HZ as f64 * 60.0);
                self.last_min = if min.is_finite() { min } else { 0.0 };
                r
            }
            Err(_) => aborted_result(config),
        }
    }
    fn last_duration_min(&self) -> f64 { self.last_min }
}

/// Agreement of two results by the weekly check's rule. Unit: `sys-theatre-abstract`.
///
/// Same win or loss: both succeeded or both did not, success being what the theatre pays as one (victory,
/// or a withdrawal after the primary objective was done). Salvage within 15% of the full-physics figure
/// (`salvage_b`; when it is 0 the abstract figure must be 0 too). Alert change within 0.5. An aborted result
/// on either side, or a non-finite Alert change, never agrees.
pub fn agree(abstract_result: &SortieResult, full_result: &SortieResult, salvage_a: u32, salvage_b: u32, alert_a: f64, alert_b: f64) -> bool {
    if abstract_result.outcome == Outcome::Aborted || full_result.outcome == Outcome::Aborted { return false; }
    if !alert_a.is_finite() || !alert_b.is_finite() { return false; }
    if success(abstract_result) != success(full_result) { return false; }
    let diff = (salvage_a as f64 - salvage_b as f64).abs();
    if diff > 0.15 * salvage_b as f64 + 1e-9 { return false; }
    (alert_a - alert_b).abs() <= 0.5 + 1e-12
}

/// The theatre's success rule (`Theatre::apply_result`).
pub(crate) fn success(r: &SortieResult) -> bool { r.outcome == Outcome::Victory || (r.outcome == Outcome::Withdrawn && r.objective_done) }

// ---------------------------------------------------------------------------------------------------------
// Config preparation
// ---------------------------------------------------------------------------------------------------------

fn finite_or_zero(v: f64) -> f64 { if v.is_finite() { v.max(0.0) } else { 0.0 } }

/// The blueprint a reference names in this config, if any.
fn config_blueprint(config: &SortieConfig, r: &BlueprintRef) -> Option<Blueprint> {
    match r {
        BlueprintRef::Inline(h) => config.blueprints.iter().find(|b| b.header.hash == *h)
            .or_else(|| config.blueprints.iter().find(|b| content::blueprint::io::compute_hash(b) == *h)).cloned(),
        BlueprintRef::Stock(p) => content::campaign::load::stock_blueprint(p).ok(),
    }
}

/// (tonnage, build cost) of a design reference; (0, 0) when it does not resolve.
fn design_facts(config: &SortieConfig, r: &BlueprintRef, data: &TheatreData) -> (f64, u32) {
    if let BlueprintRef::Stock(p) = r { return super::opgen::blueprint_facts(p, data).map_or((0.0, 0), |f| (f.tonnage_t, f.cost)); }
    config_blueprint(config, r).map_or((0.0, 0), |bp| {
        let s = content::stats::compute_stats(&bp, &data.catalogue);
        (finite_or_zero(s.tonnage_t), s.cost)
    })
}

/// The config the resolver runs: the reference setup laid over every player hull, and every player hull's
/// inline design stored under its own header hash (the form `Sim` looks designs up by).
pub(crate) fn prepare_config(config: &SortieConfig, setup: &PlayerSetup) -> SortieConfig {
    let mut c = config.clone();
    let reference = setup.reference_setup.as_deref().and_then(|p| content::campaign::load::stock_blueprint(p).ok());
    let hulls: Vec<usize> = c.players.iter().filter_map(|p| p.vessel).map(|v| v as usize).filter(|&v| v < c.vessels.len()).collect();
    for i in hulls {
        let r = c.vessels[i].blueprint.clone();
        let header_known = matches!(&r, BlueprintRef::Inline(h) if c.blueprints.iter().any(|b| b.header.hash == *h));
        if reference.is_none() && (header_known || matches!(r, BlueprintRef::Stock(_))) { continue; }
        let Some(mut bp) = config_blueprint(&c, &r) else { continue };
        if let Some(rf) = &reference {
            bp.officers = rf.officers.clone();
            bp.rules = rf.rules.clone();
        }
        let canon = crate::editor::splash::canonical(&bp);
        let h = canon.header.hash;
        if !c.blueprints.iter().any(|b| b.header.hash == h) { c.blueprints.push(canon); }
        c.vessels[i].blueprint = BlueprintRef::Inline(h);
    }
    c
}

/// A result that applies nothing (the sortie could not be built).
pub(crate) fn aborted_result(config: &SortieConfig) -> SortieResult {
    SortieResult {
        operation: config.operation.clone(), region: config.region.clone(), seed: config.seed, outcome: Outcome::Aborted, winner: None, duration_ticks: 0, objective_done: false,
        ghosts: vec![false; config.ghosts.len()], kills: Vec::new(), survivors: Vec::new(), prizes: Vec::new(), aces: Vec::new(), players: Vec::new(), sightings: 0, evidence: 0,
        photos: 0, survivors_recovered: 0, crew_captured: 0, passengers_delivered: 0, mines_laid: 0, site_destroyed: false, tender_lost: false, highest_rung: 0, belief: Vec::new(),
        stats: SortieStats::default(),
    }
}

/// Rules that hold the fire of every weapon not of `method` (see module docs).
pub(crate) fn method_rules(bp: &Blueprint, cat: &Catalogue, method: KillMethod) -> Vec<Rule> {
    let weaponless = matches!(method, KillMethod::Mine | KillMethod::Fire | KillMethod::Capture | KillMethod::Collision);
    if weaponless { return Vec::new(); }
    let mut out = Vec::new();
    for (i, pl) in bp.parts.iter().enumerate() {
        let Ok(slot) = u16::try_from(i) else { break };
        let slot = PartSlot(slot);
        let Some(def) = cat.part(&pl.part) else { continue };
        match &def.function {
            PartFunction::Gun(_) if method != KillMethod::Gunfire => out.push(Rule {
                when: Condition::Compare { lhs: Operand::InboundWeapons, cmp: Cmp::Le, rhs: Operand::Const(0.0) },
                sink: SinkKind::MountFirePermission(slot), value: RuleValue::Set(Value::Bool(false)), enabled: true,
            }),
            PartFunction::Launcher(l) => {
                let own = match cat.munition(&l.munition).map(|m| m.kind) {
                    Some(MunitionKind::AntiShipMissile) => KillMethod::Missile,
                    Some(MunitionKind::Torpedo) => KillMethod::Torpedo,
                    Some(MunitionKind::DepthCharge) => KillMethod::DepthCharge,
                    _ => continue,
                };
                if own != method { out.push(Rule { when: Condition::All(Vec::new()), sink: SinkKind::MissileLaunch(slot), value: RuleValue::Set(Value::Bool(false)), enabled: true }); }
            }
            _ => {}
        }
    }
    out
}

// ---------------------------------------------------------------------------------------------------------
// The pilot: the headless policy that plays the player hulls
// ---------------------------------------------------------------------------------------------------------

/// Snapshot cadence of the pilot, in ticks (1 Hz).
const PILOT_PERIOD: u64 = TICK_HZ as u64;
/// A waypoint counts as reached inside this distance.
const WAYPOINT_M: f64 = 400.0;
/// Re-aim a closing course on a contact this often, in pilot periods.
const HUNT_PERIODS: u64 = 15;
/// Restart an aborted or missing station action after this many pilot periods.
const ACTION_RETRY_PERIODS: u64 = 30;
/// Hold-station range of a shore hold or a boarding approach.
const HOLD_APPROACH_M: f64 = 1_500.0;
/// Photograph from this range in clear weather (inside the 6 km camera, L796).
const PHOTO_STANDOFF_M: f64 = 4_500.0;
/// Speed of a mine-laying run (at least the 6 kn the rails need, L795).
const MINE_RUN_KN: f64 = 10.0;
/// Run-in before the first drop point of a mine line.
const MINE_RUN_IN_M: f64 = 200.0;
/// The mine rails lay only at this speed or more (L795).
const MINE_MIN_KN: f64 = 6.0;
/// The Helm's cruise fraction of top speed when the hull's settings carry none (`officers::helm`).
const HELM_CRUISE_DEFAULT: f64 = 0.7;
/// Fraction of top speed when a ghost condition runs against the clock.
const HURRY_FRACTION: f64 = 0.95;
/// Course speed when the design's top speed cannot be measured.
const DEFAULT_COURSE_KN: f64 = 12.0;
/// Pilot periods on the objective with no usable contact (or no fix on the camera's target) before the Helm
/// orders the radars on (EMCON Active) to find one.
const SEARCH_PERIODS: u64 = 60;
/// An escort has met its charge inside this distance.
const ESCORT_MEET_M: f64 = 1_000.0;
/// The hunt closes only on contacts a sensor updated within this many ticks (60 s).
const HUNT_FRESH_TICKS: u64 = 60 * TICK_HZ as u64;
/// On the approach to a destroy objective, radiate inside this distance of the objective point while no
/// usable contact is held (a raid does not sail into the target's guns blind).
const ATTACK_RADIATE_M: f64 = 8_000.0;

/// Ordered course speed of a design. A player hull spawns with a stop order (0 kn), so every course the
/// pilot lays carries a speed: the hull's `helm.cruise` fraction (else the Helm's default) of the Speed
/// gauge's top speed, or nearly all of it when racing an "under time" ghost condition.
fn cruise_speed(bp: &Blueprint, cat: &Catalogue, hurry: bool) -> f64 {
    let prefab = sim_core::vessel::build_prefab(bp, cat);
    let top = crate::editor::gauges::speed_gauge(&prefab, cat).top_speed_kn;
    let frac = bp.officers.sliders.get(crate::officers::helm::SLIDER_CRUISE).copied().filter(|f| f.is_finite() && *f > 0.0 && *f <= 1.0).unwrap_or(HELM_CRUISE_DEFAULT);
    let kn = top * if hurry { HURRY_FRACTION } else { frac };
    if kn.is_finite() && kn > 0.5 { kn } else { DEFAULT_COURSE_KN }
}

/// What the Helm is doing for one player hull.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Phase { Route, Objective, Leaving }

struct Hull {
    slot: PlayerSlot,
    seq: u32,
    /// Waypoints still to sail.
    route: Vec<DVec2>,
    phase: Phase,
    last_order: Option<Command>,
    /// Pilot period of the last station action start.
    action_started: Option<u64>,
    /// Entities ordered home as prizes.
    prized: BTreeSet<EntityId>,
    /// Tracks photographed (their action finished Done).
    photographed: BTreeSet<TrackId>,
    photo_target: Option<TrackId>,
    withdrawn: bool,
    /// The mine line of a mine-laying objective.
    line: Option<(DVec2, DVec2)>,
    /// The first orders went out (the hull is in the water).
    started: bool,
    /// Ordered speed on every course: the Helm's cruise fraction of the design's top speed.
    cruise_kn: f64,
    /// Track last designated for the officers.
    designated: TrackId,
    /// Hull the running boarding was started on.
    boarding_target: Option<EntityId>,
    /// Ordered speed once the objective is done and the hull leaves: nearly all of the design's top speed.
    leave_kn: f64,
    /// The emission state the Helm last ordered (`None`: the first orders' state still stands).
    emcon_ordered: Option<Emcon>,
    /// Pilot period since which the objective phase has held no usable contact.
    no_contact_since: Option<u64>,
    /// Escort: the escorted hull has been met; the hull now keeps company with it to the exit.
    met: bool,
}

pub(crate) struct Pilot {
    hulls: Vec<Hull>,
    objective: Objective,
    objective_pos: DVec2,
    exits: Vec<DVec2>,
    named_exit: Option<DVec2>,
    actions: Vec<StationAction>,
    follow: bool,
    emcon: Option<Emcon>,
    /// Rules per player slot to send at the first tick.
    rules: BTreeMap<PlayerSlot, Vec<Rule>>,
    marker_names: BTreeMap<String, DVec2>,
    /// Photograph from this range: inside the camera's 6 km and the weather's visibility.
    photo_standoff_m: f64,
    /// Classes of the hulls the objective names (its tag, or the site), which the hunt prefers.
    target_classes: BTreeSet<ClassId>,
    period: u64,
}

impl Pilot {
    pub(crate) fn new(config: &SortieConfig, setup: &PlayerSetup, data: &TheatreData) -> Pilot {
        let finite = |p: &[f64; 2]| p.iter().all(|v| v.is_finite());
        let markers: BTreeMap<String, DVec2> = config.map.markers.iter().filter(|m| finite(&m.pos_m)).map(|m| (m.name.clone(), DVec2::from(m.pos_m))).collect();
        let exits: Vec<DVec2> = config.map.markers.iter().filter(|m| matches!(m.kind, MarkerKind::Exit | MarkerKind::ConvoyExit) && finite(&m.pos_m)).map(|m| DVec2::from(m.pos_m)).collect();
        let objective_pos = objective_point(config, data);
        let named_exit = objective_exit(&config.objective).and_then(|e| markers.get(e).copied());
        let forbids_emission = config.ghosts.iter().any(|g| matches!(g.kind, GhostKind::NoRadarEmissions | GhostKind::NeverDetected));
        let emcon = if setup.ghosts && forbids_emission { Some(Emcon::Silent) } else { setup.emcon };
        let mut rules = BTreeMap::new();
        let mut hulls = Vec::new();
        for p in &config.players {
            let Some(vi) = p.vessel.map(|v| v as usize).filter(|&v| v < config.vessels.len()) else { continue };
            let v = &config.vessels[vi];
            let spawn = DVec2::new(v.pos.x, v.pos.z);
            let bp = config_blueprint(config, &v.blueprint);
            let hurry = setup.ghosts && config.ghosts.iter().any(|g| matches!(g.kind, GhostKind::UnderTime { .. }));
            let cruise_kn = bp.as_ref().map_or(DEFAULT_COURSE_KN, |b| cruise_speed(b, &data.catalogue, hurry));
            let leave_kn = bp.as_ref().map_or(DEFAULT_COURSE_KN, |b| cruise_speed(b, &data.catalogue, true)).max(cruise_kn);
            if let Some(m) = setup.method {
                if let Some(bp) = &bp {
                    let mut list = method_rules(bp, &data.catalogue, m);
                    if !list.is_empty() {
                        list.extend(bp.rules.iter().cloned());
                        list.truncate(content::graph::MAX_RULES);
                        rules.insert(p.slot, list);
                    }
                }
            }
            let mut route = vec![objective_pos];
            let mut line = None;
            if let Objective::LayMines { count, .. } = &config.objective {
                let (a, b) = mine_line(spawn, objective_pos, *count);
                route = vec![a, b];
                line = Some((a, b));
            }
            if let Some(e) = named_exit { route.push(e); }
            if spawn.distance(route[0]) < WAYPOINT_M && route.len() > 1 { route.remove(0); }
            hulls.push(Hull { slot: p.slot, seq: 0, route, phase: Phase::Route, last_order: None, action_started: None, prized: BTreeSet::new(), photographed: BTreeSet::new(), photo_target: None, withdrawn: false, line, started: false, cruise_kn, designated: TrackId::NONE, boarding_target: None, leave_kn, emcon_ordered: None, no_contact_since: None, met: false });
        }
        let tag = match &config.objective { Objective::DestroyGroup { tag, .. } | Objective::Capture { tag } | Objective::CutOut { tag, .. } | Objective::Photograph { tag, .. } | Objective::Escort { tag, .. } => Some(tag.as_str()), _ => None };
        let target_classes = config.vessels.iter()
            .filter(|v| match (&config.objective, tag) { (Objective::DestroySite { .. }, _) => v.role == VesselRole::Site, (_, Some(t)) => !t.is_empty() && v.tag == t, _ => false })
            .filter_map(|v| v.class.clone()).collect();
        let vis = config.weather.visibility_m;
        let photo_standoff_m = if vis.is_finite() && vis > 0.0 { PHOTO_STANDOFF_M.min(0.75 * vis).max(300.0) } else { PHOTO_STANDOFF_M };
        Pilot {
            photo_standoff_m, target_classes, hulls, objective: config.objective.clone(), objective_pos, exits, named_exit, actions: config.actions.clone(), follow: setup.follow_briefing_route, emcon, rules,
            marker_names: markers, period: 0,
        }
    }

    fn allowed(&self, a: StationAction) -> bool { self.actions.contains(&a) }
}

/// The objective point the briefing route runs to: the generator's rule (`opgen`'s objective point).
fn objective_point(config: &SortieConfig, data: &TheatreData) -> DVec2 {
    let find = |name: &str| config.map.markers.iter().find(|m| m.name == name).map(|m| DVec2::from(m.pos_m));
    let named = match &config.objective { Objective::DestroySite { marker } | Objective::Extract { marker, .. } | Objective::LayMines { marker, .. } => find(marker), _ => None };
    let p = named
        .or_else(|| config.map.markers.iter().find(|m| matches!(m.kind, MarkerKind::Objective | MarkerKind::DepotSite | MarkerKind::RadarSite | MarkerKind::Berth)).map(|m| DVec2::from(m.pos_m)))
        .or_else(|| config.operation.as_ref().and_then(|o| data.campaign.operations.get(o)).and_then(|op| op.force.first()).and_then(|f| find(&f.marker)))
        .unwrap_or(DVec2::ZERO);
    if p.x.is_finite() && p.y.is_finite() { p } else { DVec2::ZERO }
}

fn objective_exit(o: &Objective) -> Option<&str> {
    match o { Objective::Escort { exit, .. } | Objective::Survive { exit, .. } | Objective::CutOut { exit, .. } if !exit.is_empty() => Some(exit.as_str()), _ => None }
}

/// A mine line through `marker` along the approach from `spawn`: (run-in start, end).
fn mine_line(spawn: DVec2, marker: DVec2, count: u16) -> (DVec2, DVec2) {
    let d = marker - spawn;
    let dir = if d.length() > 1.0 { d / d.length() } else { DVec2::X };
    let half = 0.5 * (count.max(1) as f64 * 50.0) + MINE_RUN_IN_M;
    (marker - dir * half, marker + dir * half)
}

fn nearest(from: DVec2, points: &[DVec2]) -> Option<DVec2> {
    points.iter().copied().min_by(|a, b| a.distance_squared(from).total_cmp(&b.distance_squared(from)))
}

fn plane(p: DVec3) -> DVec2 { DVec2::new(p.x, p.z) }

impl HeadlessPolicy for Pilot {
    fn commands(&mut self, sim: &Sim, out: &mut Vec<CommandEnvelope>) {
        let tick = sim.tick().0;
        // Settings and the first orders go out once the hull is in the water (it spawns on the first step).
        for i in 0..self.hulls.len() {
            if self.hulls[i].started { continue; }
            if sim.snapshot(Viewer::Player(self.hulls[i].slot)).ship.is_some() {
                self.hulls[i].started = true;
                self.first_orders(i, tick, out);
            }
        }
        if !tick.is_multiple_of(PILOT_PERIOD) { return; }
        self.period += 1;
        for i in 0..self.hulls.len() {
            if !self.hulls[i].started { continue; }
            let snap = sim.snapshot(Viewer::Player(self.hulls[i].slot));
            self.steer(i, &snap, tick, out);
        }
    }
}

impl Pilot {
    fn push(&mut self, i: usize, tick: u64, command: Command, out: &mut Vec<CommandEnvelope>) {
        let h = &mut self.hulls[i];
        h.seq = h.seq.saturating_add(1);
        out.push(CommandEnvelope { player: h.slot, seq: h.seq, client_render_tick: tick, rtt_ms: 0, command });
    }

    /// A Helm order, sent only when it differs from the last one.
    fn order(&mut self, i: usize, tick: u64, command: Command, out: &mut Vec<CommandEnvelope>) {
        if !self.follow { return; }
        if self.hulls[i].last_order.as_ref() == Some(&command) { return; }
        self.hulls[i].last_order = Some(command.clone());
        self.push(i, tick, command, out);
    }

    fn first_orders(&mut self, i: usize, tick: u64, out: &mut Vec<CommandEnvelope>) {
        if let Some(e) = self.emcon { self.push(i, tick, Command::SetEmcon(e), out); }
        if let Some(r) = self.rules.get(&self.hulls[i].slot).cloned() { self.push(i, tick, Command::SetRules(r), out); }
        let route = self.hulls[i].route.clone();
        if !route.is_empty() {
            let cruise = self.hulls[i].cruise_kn;
            let speed = if matches!(self.objective, Objective::LayMines { .. }) { MINE_RUN_KN.min(cruise).max(MINE_MIN_KN) } else { cruise };
            self.order(i, tick, Command::SetCourse { waypoints: route, speed_kn: Some(speed), append: false }, out);
        }
        self.start_marker_action(i, tick, out);
    }

    /// Start the objective's station action whose target is a marker or a line.
    fn start_marker_action(&mut self, i: usize, tick: u64, out: &mut Vec<CommandEnvelope>) {
        let start = match &self.objective {
            Objective::Extract { marker, .. } if self.allowed(StationAction::InsertOrExtract) => Some((StationAction::InsertOrExtract, ActionTarget::Marker(marker.clone()))),
            Objective::LayMines { .. } if self.allowed(StationAction::LayMines) => self.hulls[i].line.map(|(a, b)| (StationAction::LayMines, ActionTarget::Line { a, b })),
            _ => None,
        };
        if let Some((action, target)) = start {
            self.hulls[i].action_started = Some(self.period);
            self.push(i, tick, Command::StartAction { action, target }, out);
        }
    }

    fn steer(&mut self, i: usize, snap: &Snapshot, tick: u64, out: &mut Vec<CommandEnvelope>) {
        let Some(ship) = snap.ship.as_ref() else { return };
        let Some(me) = snap.vessels.iter().find(|v| v.entity == ship.vessel) else { return };
        let pos = plane(me.kin.pose.pos);
        if !(pos.x.is_finite() && pos.y.is_finite()) { return; }

        // Route progress.
        while self.hulls[i].route.first().is_some_and(|w| w.distance(pos) < WAYPOINT_M) { self.hulls[i].route.remove(0); }
        if self.hulls[i].phase == Phase::Route && (self.hulls[i].route.is_empty() || pos.distance(self.objective_pos) < WAYPOINT_M) { self.hulls[i].phase = Phase::Objective; }
        let primary = snap.objectives.iter().find(|o| !o.ghost);
        let primary_done = primary.is_some_and(|o| o.state == ObjectiveState::Done);
        let period = self.period;
        let retry_due = |h: &Hull| h.action_started.is_none_or(|s| period.saturating_sub(s) >= ACTION_RETRY_PERIODS);
        let action_state = |a: StationAction| -> (bool, bool) {
            let done = ship.actions.iter().any(|x| x.action == a && x.state == ActionState::Done);
            let running = ship.actions.iter().any(|x| x.action == a && matches!(x.state, ActionState::Running | ActionState::Paused));
            (done, running)
        };

        match self.objective.clone() {
            Objective::DestroyAll | Objective::DestroyGroup { .. } | Objective::DestroySite { .. } => {
                // A tracked hull the objective names ends the route at once: the Helm intercepts it rather
                // than sailing to a fixed point while a convoy or patrol moves on.
                let named = !self.target_classes.is_empty() && snap.tracks.iter().any(|t| !t.last_known && t.domain == Domain::Surface && matches!(t.stance, Stance::Hostile | Stance::Suspect)
                    && tick.saturating_sub(t.last_update_tick) <= HUNT_FRESH_TICKS && t.class.as_ref().is_some_and(|c| self.target_classes.contains(c)));
                if self.hulls[i].phase != Phase::Route || (named && !matches!(self.objective, Objective::DestroySite { .. })) { self.hunt(i, pos, snap, tick, out); } else {
                    self.designate_wanted(i, snap, tick, out);
                    let contact = snap.tracks.iter().any(|t| !t.last_known && t.domain == Domain::Surface && matches!(t.stance, Stance::Hostile | Stance::Suspect) && matches!(t.grade, TrackGrade::Lock | TrackGrade::Fine | TrackGrade::Coarse));
                    if !contact && pos.distance(self.objective_pos) <= ATTACK_RADIATE_M { self.radiate(i, tick, out); }
                }
            }
            Objective::Extract { marker, .. } => {
                let at = self.marker_names.get(&marker).copied().unwrap_or(self.objective_pos);
                let (done, running) = action_state(StationAction::InsertOrExtract);
                // A finished hold leaves the action list, so the objective's own progress (0.5 once held) says
                // the extraction is done as well.
                let held = primary.is_some_and(|o| o.progress >= 0.5);
                if done || held || self.hulls[i].phase == Phase::Leaving {
                    self.hulls[i].phase = Phase::Leaving;
                    self.leave(i, pos, tick, true, out);
                } else {
                    if !running && retry_due(&self.hulls[i]) { self.start_marker_action(i, tick, out); }
                    if pos.distance(at) < HOLD_APPROACH_M { self.order(i, tick, Command::HoldStation { at, heading_rad: None }, out); }
                }
            }
            Objective::LayMines { .. } => {
                let (done, running) = action_state(StationAction::LayMines);
                if !done && !running && retry_due(&self.hulls[i]) { self.start_marker_action(i, tick, out); }
                if done || primary_done { self.hulls[i].phase = Phase::Leaving; }
                if self.hulls[i].phase == Phase::Leaving { self.leave(i, pos, tick, true, out); }
                else if self.hulls[i].route.is_empty() { self.order(i, tick, Command::HoldStation { at: self.objective_pos, heading_rad: None }, out); }
            }
            Objective::Photograph { .. } => self.photograph(i, pos, snap, tick, out),
            Objective::Capture { .. } | Objective::CutOut { .. } => self.board(i, pos, snap, tick, out),
            Objective::Feint { .. } => {
                let held = primary.is_some_and(|o| o.progress >= 0.9);
                if held && !self.hulls[i].withdrawn && self.follow {
                    self.hulls[i].withdrawn = true;
                    self.hulls[i].phase = Phase::Leaving;
                    self.push(i, tick, Command::Withdraw, out);
                }
                if self.hulls[i].phase == Phase::Leaving { self.leave(i, pos, tick, true, out); }
                else if self.hulls[i].phase == Phase::Objective { self.order(i, tick, Command::HoldStation { at: self.objective_pos, heading_rad: None }, out); }
            }
            Objective::Escort { .. } => {
                // Meet the escorted hull on its way out (the nearest Neutral contact, its class first), then
                // keep company with it to the exit; with no contact, sail the briefing route.
                if !self.hulls[i].met {
                    let wanted = |t: &&crate::protocol::snapshot::TrackView| t.class.as_ref().is_some_and(|c| self.target_classes.contains(c));
                    let charge = snap.tracks.iter()
                        .filter(|t| !t.last_known && t.domain == Domain::Surface && t.stance == Stance::Neutral && t.pos.x.is_finite() && t.pos.z.is_finite())
                        .min_by(|a, b| (!wanted(a)).cmp(&!wanted(b)).then(plane(a.pos).distance_squared(pos).total_cmp(&plane(b.pos).distance_squared(pos))).then(a.id.cmp(&b.id)))
                        .map(|t| plane(t.pos));
                    match charge {
                        Some(at) if at.distance(pos) <= ESCORT_MEET_M => self.hulls[i].met = true,
                        Some(at) => {
                            if self.period.is_multiple_of(HUNT_PERIODS) || !matches!(self.hulls[i].last_order, Some(Command::SetCourse { ref waypoints, .. }) if waypoints.len() == 1) {
                                self.order(i, tick, Command::SetCourse { waypoints: vec![at], speed_kn: Some(self.hulls[i].cruise_kn), append: false }, out);
                            }
                        }
                        None => {}
                    }
                }
                if (self.hulls[i].met || self.hulls[i].route.is_empty()) && !primary_done { self.leave(i, pos, tick, false, out); }
            }
            Objective::Survive { .. } => {
                if self.hulls[i].route.is_empty() && !primary_done { self.leave(i, pos, tick, false, out); }
            }
        }
    }

    /// Sail for the objective's exit, else the nearest exit marker; with none, withdraw in place. `done`: the
    /// objective is done and the hull leaves at nearly its top speed (else at the Helm's cruise, keeping
    /// company with an escorted hull).
    fn leave(&mut self, i: usize, pos: DVec2, tick: u64, done: bool, out: &mut Vec<CommandEnvelope>) {
        let exit = self.named_exit.or_else(|| nearest(pos, &self.exits));
        let kn = if done { self.hulls[i].leave_kn } else { self.hulls[i].cruise_kn };
        match exit {
            Some(e) if e.distance(pos) >= 100.0 => self.order(i, tick, Command::SetCourse { waypoints: vec![e], speed_kn: Some(kn), append: false }, out),
            Some(e) => self.order(i, tick, Command::HoldStation { at: e, heading_rad: None }, out),
            None => if !self.hulls[i].withdrawn && self.follow { self.hulls[i].withdrawn = true; self.push(i, tick, Command::Withdraw, out); },
        }
    }

    /// Order the radars on (EMCON Active) once: what a player does when the objective needs a contact or a fix
    /// that passive sensors have not given.
    fn radiate(&mut self, i: usize, tick: u64, out: &mut Vec<CommandEnvelope>) {
        if !self.follow || self.hulls[i].emcon_ordered == Some(Emcon::Active) { return; }
        self.hulls[i].emcon_ordered = Some(Emcon::Active);
        self.push(i, tick, Command::SetEmcon(Emcon::Active), out);
    }

    /// Count pilot periods without a usable contact (or fix); radiate once [`SEARCH_PERIODS`] have passed.
    fn search(&mut self, i: usize, found: bool, tick: u64, out: &mut Vec<CommandEnvelope>) {
        if found { self.hulls[i].no_contact_since = None; return; }
        let since = *self.hulls[i].no_contact_since.get_or_insert(self.period);
        if self.period.saturating_sub(since) >= SEARCH_PERIODS { self.radiate(i, tick, out); }
    }

    /// Close on the nearest hostile surface contact (re-aimed every [`HUNT_PERIODS`]), else hold at the
    /// objective point.
    fn hunt(&mut self, i: usize, pos: DVec2, snap: &Snapshot, tick: u64, out: &mut Vec<CommandEnvelope>) {
        let holding = matches!(self.hulls[i].last_order, Some(Command::HoldStation { .. }));
        // The objective's own hulls first (by class, once identified), then the nearest hostile.
        let wanted = |t: &&crate::protocol::snapshot::TrackView| t.class.as_ref().is_some_and(|c| self.target_classes.contains(c));
        // Only contacts a sensor has updated lately: a coasting track's extrapolation runs off across the map.
        let fresh_track = |t: &&crate::protocol::snapshot::TrackView| tick.saturating_sub(t.last_update_tick) <= HUNT_FRESH_TICKS;
        let target = snap.tracks.iter()
            .filter(|t| !t.last_known && t.domain == Domain::Surface && matches!(t.stance, Stance::Hostile | Stance::Suspect) && t.pos.x.is_finite() && t.pos.z.is_finite())
            .filter(fresh_track)
            .min_by(|a, b| (!wanted(a)).cmp(&!wanted(b)).then(plane(a.pos).distance_squared(pos).total_cmp(&plane(b.pos).distance_squared(pos))))
            .map(|t| {
                // Lead a moving contact: aim where it will be when the hull gets there (one step, at most 10 min).
                let p = plane(t.pos);
                let v = DVec2::new(t.vel.x, t.vel.z);
                let own = (self.hulls[i].cruise_kn * sim_core::time::KN).max(1.0);
                let tgo = (p.distance(pos) / own).min(600.0);
                let aim = if v.is_finite() { p + v * tgo } else { p };
                (t.id, if aim.is_finite() { aim } else { p })
            });
        // Re-aim on the cadence, or at once when a contact appears while holding or the route just ended.
        let fresh = self.hulls[i].phase == Phase::Route || (holding && target.is_some());
        self.hulls[i].phase = Phase::Objective;
        self.search(i, target.is_some(), tick, out);
        if !(fresh || self.period.is_multiple_of(HUNT_PERIODS)) { return; }
        match target {
            Some((id, at)) => {
                if self.hulls[i].designated != id && self.follow {
                    self.hulls[i].designated = id;
                    self.push(i, tick, Command::Designate { track: Some(id) }, out);
                }
                self.order(i, tick, Command::SetCourse { waypoints: vec![at], speed_kn: Some(self.hulls[i].cruise_kn), append: false }, out)
            }
            None => self.order(i, tick, Command::HoldStation { at: self.objective_pos, heading_rad: None }, out),
        }
    }

    /// On the approach, designate the objective's own hull (its tag's class, or the site) as soon as a track of
    /// it exists, so the officers engage it rather than holding on a Suspect contact (designation lifts it).
    fn designate_wanted(&mut self, i: usize, snap: &Snapshot, tick: u64, out: &mut Vec<CommandEnvelope>) {
        if !self.follow || self.target_classes.is_empty() { return; }
        let pos = snap.ship.as_ref().and_then(|sh| snap.vessels.iter().find(|v| v.entity == sh.vessel)).map(|v| plane(v.kin.pose.pos)).unwrap_or(DVec2::ZERO);
        let wanted = snap.tracks.iter()
            .filter(|t| !t.last_known && t.domain == Domain::Surface && matches!(t.stance, Stance::Hostile | Stance::Suspect) && t.class.as_ref().is_some_and(|c| self.target_classes.contains(c)) && t.pos.x.is_finite() && t.pos.z.is_finite())
            .min_by(|a, b| plane(a.pos).distance_squared(pos).total_cmp(&plane(b.pos).distance_squared(pos)).then(a.id.cmp(&b.id)))
            .map(|t| t.id);
        if let Some(id) = wanted {
            if self.hulls[i].designated != id {
                self.hulls[i].designated = id;
                self.push(i, tick, Command::Designate { track: Some(id) }, out);
            }
        }
    }

    fn photograph(&mut self, i: usize, pos: DVec2, snap: &Snapshot, tick: u64, out: &mut Vec<CommandEnvelope>) {
        let Some(ship) = snap.ship.as_ref() else { return };
        let photo = ship.actions.iter().rfind(|a| a.action == StationAction::Photograph);
        let running = photo.is_some_and(|a| matches!(a.state, ActionState::Running | ActionState::Paused));
        if let (Some(a), Some(t)) = (photo, self.hulls[i].photo_target) {
            if a.state == ActionState::Done { self.hulls[i].photographed.insert(t); self.hulls[i].photo_target = None; }
            else if a.state == ActionState::Aborted && !running { self.hulls[i].photo_target = None; }
        }
        // A target is live while a sensor still updates it (a coasting ESM range guess drifts off).
        let live = |id: TrackId| snap.tracks.iter().find(|t| t.id == id && !t.last_known && tick.saturating_sub(t.last_update_tick) <= HUNT_FRESH_TICKS);
        if self.hulls[i].photo_target.is_some_and(|t| live(t).is_none()) { self.hulls[i].photo_target = None; }
        let allowed = self.allowed(StationAction::Photograph);
        if self.hulls[i].photo_target.is_none() {
            let done = &self.hulls[i].photographed;
            // The objective's classes first, then hostile contacts, then any other non-Friend contact.
            let rank = |t: &crate::protocol::snapshot::TrackView| (t.grade == TrackGrade::Bearing, !t.class.as_ref().is_some_and(|c| self.target_classes.contains(c)), !matches!(t.stance, Stance::Hostile | Stance::Suspect));
            let pick = snap.tracks.iter()
                .filter(|t| !t.last_known && t.domain == Domain::Surface && t.stance != Stance::Friend && !done.contains(&t.id) && t.pos.x.is_finite() && t.pos.z.is_finite() && tick.saturating_sub(t.last_update_tick) <= HUNT_FRESH_TICKS)
                .min_by(|a, b| rank(a).cmp(&rank(b)).then(plane(a.pos).distance_squared(pos).total_cmp(&plane(b.pos).distance_squared(pos))))
                .map(|t| t.id);
            if let Some(t) = pick {
                self.hulls[i].photo_target = Some(t);
                if allowed {
                    self.hulls[i].action_started = Some(self.period);
                    self.push(i, tick, Command::StartAction { action: StationAction::Photograph, target: ActionTarget::Track(t) }, out);
                }
            }
        } else if !running && allowed && self.hulls[i].action_started.is_none_or(|s| self.period.saturating_sub(s) >= ACTION_RETRY_PERIODS) {
            if let Some(t) = self.hulls[i].photo_target {
                self.hulls[i].action_started = Some(self.period);
                self.push(i, tick, Command::StartAction { action: StationAction::Photograph, target: ActionTarget::Track(t) }, out);
            }
        }
        // No target, or one without a position fix (bearing-only ESM range guesses), for too long: radiate.
        let fix = self.hulls[i].photo_target.and_then(live).is_some_and(|t| matches!(t.grade, TrackGrade::Lock | TrackGrade::Fine));
        if self.hulls[i].phase != Phase::Route || self.hulls[i].photo_target.is_some() { self.search(i, fix, tick, out); }
        match self.hulls[i].photo_target.and_then(live) {
            Some(t) => {
                let at = plane(t.pos);
                // Keep closing while the camera reports the target too far or beyond visibility.
                let short = photo.is_some_and(|a| matches!(a.block, Some(ActionBlock::TooFar | ActionBlock::BeyondVisibility | ActionBlock::NoLineOfSight)));
                let standoff = if short { 0.5 * self.photo_standoff_m } else { self.photo_standoff_m };
                if at.distance(pos) > standoff {
                    let reaim = self.period.is_multiple_of(10) || !matches!(self.hulls[i].last_order, Some(Command::SetCourse { .. })) || self.hulls[i].phase == Phase::Route;
                    if reaim {
                        self.hulls[i].phase = Phase::Objective;
                        self.order(i, tick, Command::SetCourse { waypoints: vec![at], speed_kn: Some(self.hulls[i].cruise_kn), append: false }, out);
                    }
                } else if !matches!(self.hulls[i].last_order, Some(Command::HoldStation { .. })) || short {
                    self.hulls[i].phase = Phase::Objective;
                    let hold = if short { at + (pos - at).normalize_or_zero() * standoff } else { pos };
                    self.order(i, tick, Command::HoldStation { at: hold, heading_rad: None }, out);
                }
            }
            None => if self.hulls[i].phase != Phase::Route { self.order(i, tick, Command::HoldStation { at: self.objective_pos, heading_rad: None }, out); },
        }
    }

    fn board(&mut self, i: usize, pos: DVec2, snap: &Snapshot, tick: u64, out: &mut Vec<CommandEnvelope>) {
        let Some(ship) = snap.ship.as_ref() else { return };
        let boarding = ship.actions.iter().rfind(|a| a.action == StationAction::BoardAndCapture);
        let running = boarding.is_some_and(|a| matches!(a.state, ActionState::Running | ActionState::Paused));
        let boarded = boarding.is_some_and(|a| a.state == ActionState::Done);
        let hostile_track = |id: TrackId| !id.is_none() && snap.tracks.iter().any(|t| t.id == id && t.stance != Stance::Friend);
        let dist = |v: &crate::protocol::snapshot::VesselView| plane(v.kin.pose.pos).distance_squared(pos);
        // A boarded hull goes home under its own Helm: the one hull this boarding was started on, once.
        if boarded {
            if let Some(e) = self.hulls[i].boarding_target.take() {
                if self.hulls[i].prized.insert(e) { self.push(i, tick, Command::Prize { prize: e, decision: PrizeDecision::SailHome }, out); }
            }
            if matches!(self.objective, Objective::CutOut { .. }) { self.hulls[i].phase = Phase::Leaving; }
        }
        if self.hulls[i].phase == Phase::Leaving { self.leave(i, pos, tick, true, out); return; }
        // A disabled enemy hull in visual range: close and board.
        let target = snap.vessels.iter()
            .filter(|v| v.entity != ship.vessel && v.status == VesselStatus::Disabled && hostile_track(v.track) && !self.hulls[i].prized.contains(&v.entity))
            .min_by(|a, b| dist(a).total_cmp(&dist(b)));
        match target {
            Some(v) => {
                let at = plane(v.kin.pose.pos);
                self.hulls[i].phase = Phase::Objective;
                let due = self.hulls[i].action_started.is_none_or(|s| self.period.saturating_sub(s) >= ACTION_RETRY_PERIODS);
                if !running && self.allowed(StationAction::BoardAndCapture) && due {
                    self.hulls[i].action_started = Some(self.period);
                    self.hulls[i].boarding_target = Some(v.entity);
                    self.push(i, tick, Command::StartAction { action: StationAction::BoardAndCapture, target: ActionTarget::Track(v.track) }, out);
                }
                let d = at.distance(pos);
                if d > HOLD_APPROACH_M {
                    if self.period.is_multiple_of(10) || !matches!(self.hulls[i].last_order, Some(Command::SetCourse { .. })) { self.order(i, tick, Command::SetCourse { waypoints: vec![at], speed_kn: Some(self.hulls[i].cruise_kn), append: false }, out); }
                } else {
                    let off = if d > 1.0 { (pos - at) / d * 100.0 } else { DVec2::new(100.0, 0.0) };
                    let hold = at + off;
                    let moved = !matches!(&self.hulls[i].last_order, Some(Command::HoldStation { at: h, .. }) if h.distance(hold) < 50.0);
                    if moved { self.order(i, tick, Command::HoldStation { at: hold, heading_rad: None }, out); }
                }
            }
            None => if self.hulls[i].phase != Phase::Route { self.hunt(i, pos, snap, tick, out); },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(crate) fn data() -> TheatreData {
        let catalogue = Arc::new(content::catalogue::Catalogue::load_embedded().expect("catalogue"));
        let campaign = Arc::new(content::campaign::load::load_embedded(&catalogue).expect("campaign data"));
        TheatreData { campaign, catalogue }
    }

    fn convoy_raid(d: &TheatreData) -> SortieConfig {
        crate::theatre::opgen::generate_standalone(content::campaign::MissionType::ConvoyRaid, Act::Act1, 1, 3, &content::campaign::DifficultySliders::NORMAL, None, d).expect("an Act 1 convoy raid").config
    }

    fn result(c: &SortieConfig, outcome: Outcome, done: bool) -> SortieResult {
        let mut r = aborted_result(c);
        r.outcome = outcome;
        r.objective_done = done;
        r
    }

    /// The pending-decision resolver never panics and returns the neutral aborted result.
    #[test]
    fn abstract_resolver_is_safe_and_neutral() {
        let d = data();
        let c = convoy_raid(&d);
        let mut r = AbstractResolver { last_min: 7.0 };
        let setup = PlayerSetup { reference_setup: None, method: None, emcon: None, ghosts: false, follow_briefing_route: true };
        let out = r.resolve(&c, &setup, &d);
        assert_eq!(out, aborted_result(&c));
        assert_eq!(r.last_duration_min(), 0.0);
    }

    #[test]
    fn agreement_rule() {
        let d = data();
        let c = convoy_raid(&d);
        let win = result(&c, Outcome::Victory, true);
        let loss = result(&c, Outcome::Defeat, false);
        let withdrew_done = result(&c, Outcome::Withdrawn, true);
        let withdrew = result(&c, Outcome::Withdrawn, false);
        assert!(agree(&win, &win, 100, 100, 1.0, 1.0));
        assert!(agree(&win, &withdrew_done, 100, 100, 1.0, 1.0), "a withdrawal after the objective is a win");
        assert!(agree(&loss, &withdrew, 0, 0, 0.0, 0.0));
        assert!(!agree(&win, &loss, 100, 100, 1.0, 1.0));
        assert!(!success(&result(&c, Outcome::Draw, true)));
        // Salvage within 15% of the full-physics figure.
        assert!(agree(&win, &win, 115, 100, 1.0, 1.0));
        assert!(agree(&win, &win, 85, 100, 1.0, 1.0));
        assert!(!agree(&win, &win, 116, 100, 1.0, 1.0));
        assert!(!agree(&win, &win, 84, 100, 1.0, 1.0));
        assert!(!agree(&win, &win, 1, 0, 1.0, 1.0));
        // Alert change within 0.5.
        assert!(agree(&win, &win, 100, 100, 1.5, 1.0));
        assert!(!agree(&win, &win, 100, 100, 1.6, 1.0));
        assert!(!agree(&win, &win, 100, 100, f64::NAN, 1.0));
        assert!(!agree(&result(&c, Outcome::Aborted, false), &loss, 0, 0, 0.0, 0.0));
    }

    #[test]
    fn abstract_input_counts_both_sides() {
        let d = data();
        let c = convoy_raid(&d);
        let setup = PlayerSetup { reference_setup: None, method: None, emcon: None, ghosts: false, follow_briefing_route: true };
        let a = abstract_input(&c, &setup, &d);
        assert_eq!(a.player_hulls, 1);
        assert!(a.player_tonnage_t > 0.0 && a.player_build_cost > 0);
        assert!(a.enemy_hulls >= 5, "the commitment rule puts at least 5 hostile hulls in an Act 1 sortie, found {}", a.enemy_hulls);
        assert!(a.enemy_tonnage_t > 0.0);
        assert!(!a.enemy_tiers.is_empty() && a.enemy_tiers.iter().all(|t| (1..=5).contains(t)));
        assert_eq!(a.sea_state, c.weather.sea_state.0);
        assert_eq!(a.setup, setup);
        // Garbage in, no panic.
        let mut bad = c.clone();
        bad.players[0].vessel = Some(9_999);
        bad.vessels.iter_mut().for_each(|v| { v.blueprint = BlueprintRef::Inline(ContentHash::EMPTY); v.class = None; });
        let b = abstract_input(&bad, &setup, &d);
        assert_eq!((b.player_tonnage_t, b.enemy_tonnage_t), (0.0, 0.0));
    }

    #[test]
    fn reference_setup_is_laid_over_the_player_hull() {
        let d = data();
        let c = convoy_raid(&d);
        let path = "reference/act2/duel_underwriter.bp.ron";
        let setup = PlayerSetup { reference_setup: Some(path.into()), method: None, emcon: None, ghosts: false, follow_briefing_route: true };
        let p = prepare_config(&c, &setup);
        let vi = p.players[0].vessel.unwrap() as usize;
        let BlueprintRef::Inline(h) = p.vessels[vi].blueprint else { panic!("the player hull travels inline") };
        let bp = p.blueprints.iter().find(|b| b.header.hash == h).expect("stored under its header hash");
        assert_eq!(bp.header.hash, content::blueprint::io::compute_hash(bp));
        let rf = content::campaign::load::stock_blueprint(path).unwrap();
        assert_eq!((&bp.officers, &bp.rules), (&rf.officers, &rf.rules));
        let own = config_blueprint(&c, &c.vessels[vi].blueprint).unwrap();
        assert_eq!(bp.parts, own.parts, "the hull and parts stay the player's");
        // No reference: a stock hull's config is unchanged.
        let none = PlayerSetup { reference_setup: None, ..setup.clone() };
        assert_eq!(prepare_config(&c, &none), c);
        // A path that does not load leaves the stock settings.
        let broken = PlayerSetup { reference_setup: Some("reference/none.bp.ron".into()), ..setup };
        assert_eq!(prepare_config(&c, &broken), c);
    }

    #[test]
    fn single_method_rules_hold_other_weapons() {
        let d = data();
        let bp = content::campaign::load::stock_blueprint("starters/corvette40.bp.ron").expect("the 40 m starter");
        let missiles = method_rules(&bp, &d.catalogue, KillMethod::Missile);
        let guns = method_rules(&bp, &d.catalogue, KillMethod::Gunfire);
        assert!(!missiles.is_empty() && !guns.is_empty(), "the corvette carries guns and missiles");
        assert!(missiles.iter().all(|r| matches!(r.sink, SinkKind::MountFirePermission(_))), "missile-only holds the guns");
        assert!(guns.iter().all(|r| matches!(r.sink, SinkKind::MissileLaunch(_))), "gun-only holds the missiles");
        assert!(missiles.iter().all(|r| matches!(r.when, Condition::Compare { lhs: Operand::InboundWeapons, .. })), "guns still defend the ship");
        assert!(method_rules(&bp, &d.catalogue, KillMethod::Capture).is_empty());
    }

    #[test]
    fn mine_line_runs_through_the_marker_along_the_approach() {
        let (a, b) = mine_line(DVec2::new(-5_000.0, 0.0), DVec2::ZERO, 10);
        assert!((a - DVec2::new(-450.0, 0.0)).length() < 1e-9 && (b - DVec2::new(450.0, 0.0)).length() < 1e-9);
        let (a, b) = mine_line(DVec2::ZERO, DVec2::ZERO, 0);
        assert!(a.is_finite() && b.is_finite() && a.distance(b) > 0.0);
    }

    #[test]
    fn route_runs_from_spawn_to_the_objective_and_exit() {
        let d = data();
        let c = crate::theatre::opgen::generate_standalone(content::campaign::MissionType::HunterGroupSurvival, Act::Act1, 1, 1, &content::campaign::DifficultySliders::NORMAL, None, &d).expect("hunter group").config;
        let setup = PlayerSetup { reference_setup: None, method: None, emcon: None, ghosts: false, follow_briefing_route: true };
        let p = Pilot::new(&c, &setup, &d);
        assert_eq!(p.hulls.len(), 1);
        if let Some(e) = p.named_exit { assert_eq!(p.hulls[0].route.last(), Some(&e)); }
        assert!(p.hulls[0].route.iter().all(|w| w.is_finite()));
    }
}

/// Hand-run trace of one full-physics sortie: `BP_OP` (operation id), `BP_SEED`. Prints the pilot's view
/// once a simulated minute. `cargo test -p sim-systems --lib trace_sortie -- --ignored --nocapture`.
#[cfg(test)]
mod trace {
    use super::*;
    struct Trace<'a>(&'a mut Pilot);
    impl HeadlessPolicy for Trace<'_> {
        fn commands(&mut self, sim: &Sim, out: &mut Vec<CommandEnvelope>) {
            let n = out.len();
            self.0.commands(sim, out);
            for c in &out[n..] { eprintln!("t={:>5}s cmd {:?}", sim.tick().0 / 60, c.command); }
            let every: u64 = std::env::var("BP_EVERY").ok().and_then(|s| s.parse().ok()).unwrap_or(60 * 60);
            if sim.tick().0.is_multiple_of(every.max(1)) {
                let s = sim.snapshot(Viewer::Player(PlayerSlot(0)));
                let me = s.ship.as_ref().and_then(|sh| s.vessels.iter().find(|v| v.entity == sh.vessel));
                let tracks: Vec<String> = s.tracks.iter().filter(|t| !t.last_known).map(|t| format!("{}:{:?}({:?})/{:?}@{:.0},{:.0}", t.id.0, t.stance, t.stance_reason, t.domain, t.pos.x, t.pos.z)).collect();
                eprintln!("t={:>5}s mounts {:?} stats {:?}", sim.tick().0 / 60, s.ship.as_ref().map(|x| x.mounts.iter().map(|m| (m.slot.0, m.status, m.effective_range_m)).collect::<Vec<_>>()), s.tracks.iter().filter(|t| !t.last_known).map(|t| (t.id.0, t.rung, t.grade, t.class.clone())).collect::<Vec<_>>());
                if let Some(h) = self.0.hulls.first() {
                    let tp = h.photo_target.and_then(|id| s.tracks.iter().find(|t| t.id == id)).map(|t| (t.id.0, t.pos.x.round(), t.pos.z.round(), t.class.clone()));
                    eprintln!("pilot phase {:?} route {:?} last {:?} photo {:?} photographed {:?}", h.phase, h.route, h.last_order, tp, h.photographed);
                }
                if std::env::var("BP_TRUTH").is_ok() {
                    let own = s.ship.as_ref().map(|x| x.vessel);
                    let op = own.and_then(|o| sim.world().vessel(o)).map(|v| v.kin.pose.pos).unwrap_or_default();
                    let dets = own.and_then(|o| sim.ships().get(o)).map(|sh| sh.detections.iter().map(|d| format!("{:?}", d.kind)).collect::<Vec<_>>());
                    let tv: Vec<String> = sim.world().vessels().map(|x| x.1).filter(|v| Some(v.id) != own).map(|v| format!("{}s{}:{:?}@{:.0}m", v.id.index, v.side.0, v.status, (v.kin.pose.pos - op).length())).collect();
                    eprintln!("truth {} | dets {:?}", tv.join(" "), dets);
                }
                eprintln!("heel {:?} trim {:?} comps {:?}", s.ship.as_ref().map(|x| x.heel_deg), s.ship.as_ref().map(|x| x.trim_deg), s.ship.as_ref().map(|x| x.compartments.iter().filter(|c| c.fill > 0 || c.breached).map(|c| (c.id.0, c.fill, c.breached)).collect::<Vec<_>>()));
                eprintln!("t={:>5}s pos {:?} speed {:?} status {:?} obj {:?} actions {:?} tracks {}", sim.tick().0 / 60,
                    me.map(|v| (v.kin.pose.pos.x.round(), v.kin.pose.pos.z.round())), s.ship.as_ref().map(|x| x.speed_kn), me.map(|v| v.status),
                    s.objectives.iter().map(|o| (o.ghost, o.state, o.progress)).collect::<Vec<_>>(), s.ship.as_ref().map(|x| x.actions.iter().map(|a| (a.action, a.state, a.block, a.progress)).collect::<Vec<_>>()), tracks.join(" "));
            }
        }
    }
    #[test]
    #[ignore = "full-physics trace"]
    fn trace_sortie() {
        let d = tests::data();
        let op = OperationId::new(std::env::var("BP_OP").unwrap_or_else(|_| "op.act1.convoy_raid".into()));
        let index: u32 = std::env::var("BP_SEED").ok().and_then(|s| s.parse().ok()).unwrap_or(0);
        let minutes: u64 = std::env::var("BP_MIN").ok().and_then(|s| s.parse().ok()).unwrap_or(45);
        let starter = match std::env::var("BP_STARTER") { Ok(p) => content::campaign::load::stock_blueprint(&p).unwrap(), Err(_) => crate::theatre::bot::op_starter(&d.campaign.operations[&op], Act::Act1, &d) };
        eprintln!("starter {}", starter.header.name);
        let g = crate::theatre::bot::winnability_config(&op, Act::Act1, index, &starter, &d).expect("generates");
        eprintln!("objective {:?} actions {:?} time limit {:?} markers {:?}", g.objective, g.actions, g.time_limit_s, g.map.markers.iter().map(|m| (m.kind, m.name.clone(), m.pos_m)).collect::<Vec<_>>());
        for v in &g.vessels { eprintln!("vessel {:?} side {:?} role {:?} tag {:?} at {:.0},{:.0} arrives {} class {:?}", v.blueprint, v.side.0, v.role, v.tag, v.pos.x, v.pos.z, v.arrives_s, v.class); }
        let follow = std::env::var("BP_FOLLOW").map_or(true, |v| v != "0");
        let setup = PlayerSetup { reference_setup: None, method: None, emcon: None, ghosts: false, follow_briefing_route: follow };
        eprintln!("weather {:?}", g.weather);
        let prepared = prepare_config(&g, &setup);
        let mut pilot = Pilot::new(&prepared, &setup, &d);
        eprintln!("route {:?} objective point {:?}", pilot.hulls[0].route, pilot.objective_pos);
        let deps = crate::sim::SimDeps { catalogue: d.catalogue.clone(), campaign: d.campaign.clone(), designs: Default::default(), scripts: Box::new(crate::script::NullScriptHost), guest_gate_passed: false, budgets: Default::default(), ladder: Default::default() };
        let pool = rayon::ThreadPoolBuilder::new().num_threads(3).build().unwrap();
        let r = pool.install(|| run_headless(prepared, deps, minutes * 3600, &mut Trace(&mut pilot), &mut NoDeadline)).expect("runs");
        eprintln!("outcome {:?} done {} min {:.1} kills {:?} players {:?}", r.outcome, r.objective_done, r.duration_ticks as f64 / 3600.0, r.kills.iter().map(|k| (k.method, k.fate)).collect::<Vec<_>>(), r.players.iter().map(|p| p.hull_lost).collect::<Vec<_>>());
    }
}
