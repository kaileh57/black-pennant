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
//!   - destroy all or a group: after the last waypoint the Helm steers for the reported position of the
//!     nearest hostile surface track that has one (re-aimed every 15 s), else holds station at the objective
//!     point;
//!   - destroy a site: the route ends at a stand-off on the approach bearing, 0.8 of the longest gun's
//!     effective range from the site and never inside 1,000 m, and the hull holds station there. If no track
//!     of the site is held after 60 s at the stand-off and a gun still has ammunition, it closes once, to 0.75
//!     of the weather's visibility (still never inside 1,000 m). It never closes with its magazines empty;
//!   - insert or extract: hold station on the shore marker for the hold, then leave by the nearest exit;
//!   - lay mines: a line through the marker along the approach course, one mine per 50 m of the count plus
//!     200 m run-in at each end; the hull cruises to the line's start and sails the line at 10 kn;
//!   - photograph: the nearest unphotographed non-Friend surface track inside 1.5 stand-offs, else the best
//!     other one, approached to 4.5 km (at most 75% of the weather's visibility) and held, closing to half
//!     that while the camera reports it too far, beyond visibility or out of sight. A target whose range has
//!     not closed in 60 s is given up for 5 minutes;
//!   - capture and cutting-out: close the prize's contact to 1,000 m and hold there until it is disabled
//!     (Gunnery fires on a Suspect track inside self-defence range), then close on the disabled hull, hold
//!     150 m off it while the boarding runs, order each boarded hull home when its boarding finishes and
//!     (cutting-out) sail for the point abeam of the exit. A cutting-out's route ends 1,000 m short of where
//!     the prize lies, not at the map's first objective marker;
//!   - feint: once the hold is served, withdraw and sail for the nearest exit;
//!   - survive and escort: the route ends at the exit; an escort first meets its charge.
//!
//!   Without `follow_briefing_route` the hull gets no Helm orders or withdrawals: its officers and standing
//!   orders do everything.
//! - What the pilot never orders (22 November 2026, the reading of L317 "stock officers and the Helm officer
//!   following the briefing route"): it never changes the emission state after the first tick, never
//!   designates a track, never leads a moving contact and never orders more than the Helm's cruise. Emissions
//!   are the Sensors officer's under the L1 setting the hull sails in at (L507), and what Gunnery fires on is
//!   set by stance and grade (L484, L498). The orders it does give are the player's one-button orders of the
//!   shipped game: a course (L108), a station hold, a station action and a prize decision (L786), and the
//!   withdrawal.
//! - Every course carries an ordered speed, because a player hull spawns under a stop order: the hull's
//!   `helm.cruise` fraction (else the Helm's default 0.7) of the Speed gauge's top speed, 95% of it when an
//!   attempted ghost condition runs against the clock, and 6 to 10 kn on a mine line.
//! - Exits: the objective's named exit, else the nearest Exit-kind marker. A convoy lane's end is not a way
//!   out for the pilot (the straight course to one grounded the hull on Mareby's shore).
//! - Targets: the hunt and the camera prefer contacts identified as a class of the hulls the objective
//!   names (its tag, or the site), then the nearest.
//! - Station actions the objective needs are started at the first tick where their target is a marker or a
//!   line (a started action waits paused until its conditions hold) and once a target exists for track
//!   targets; an aborted action is restarted after 30 s.
//! - The policy reads only the player's own snapshot (`Viewer::Player`), at 1 Hz, never the truth, plus the
//!   list of its own finished station actions (`SpecOpsState::completed` for its slot). A finished action
//!   leaves the snapshot's action list in the tick it finishes, so the snapshot alone never shows one done;
//!   a client learns it from the action-changed event.
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
/// A hull to be boarded is closed to this range and no nearer until it is disabled: inside the range at which
/// Gunnery fires on a Suspect track (self-defence range, L484), and clear of a collision with it.
const BOARD_STANDOFF_M: f64 = 1_000.0;
/// Station kept off a disabled hull while the boarding runs (the boarding needs 200 m or less).
const BOARD_HOLD_M: f64 = 150.0;
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
/// An escort has met its charge inside this distance (inside the 1,500 m at which the charge is released).
const ESCORT_MEET_M: f64 = 1_300.0;
/// An escort keeps this far abeam of its charge's way out, and waits this far abeam of the exit.
const ESCORT_FLANK_M: f64 = 1_000.0;
/// The hunt closes only on contacts a sensor updated within this many ticks (60 s).
const HUNT_FRESH_TICKS: u64 = 60 * TICK_HZ as u64;
/// A site is engaged from this share of the longest gun's effective range, and never from inside the floor.
const SITE_STANDOFF_FRACTION: f64 = 0.8;
const SITE_STANDOFF_MIN_M: f64 = 1_000.0;
/// Pilot periods at the stand-off with no track of the site before the hull closes to visual range, as a
/// share of the weather's visibility.
const SITE_SEARCH_PERIODS: u64 = 60;
const SITE_VISUAL_FRACTION: f64 = 0.75;
/// A track within this distance of the site's marker is the site.
const SITE_TRACK_M: f64 = 1_000.0;
/// The camera gives up a target whose range has not closed by `PHOTO_CLOSING_M` in this many pilot periods,
/// and leaves it alone for `PHOTO_SKIP_PERIODS`. A target inside this many stand-offs is taken first.
const PHOTO_STALL_PERIODS: u64 = 60;
const PHOTO_CLOSING_M: f64 = 100.0;
const PHOTO_SKIP_PERIODS: u64 = 300;
const PHOTO_NEAR_STANDOFFS: f64 = 1.5;

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
    /// Where the hull spawned (the start of the approach bearing).
    spawn: DVec2,
    /// Waypoints still to sail.
    route: Vec<DVec2>,
    phase: Phase,
    last_order: Option<Command>,
    /// Pilot period of the last station action start.
    action_started: Option<u64>,
    /// Entities ordered home as prizes.
    prized: BTreeSet<EntityId>,
    /// Tracks photographed, and how many of its own finished photographs the pilot has counted.
    photographed: BTreeSet<TrackId>,
    photos_done: usize,
    photo_target: Option<TrackId>,
    /// Closest range to the camera's target so far and the pilot period it was reached.
    photo_best: Option<(f64, u64)>,
    /// Targets given up because the range would not close, with the pilot period they were given up at.
    photo_skipped: BTreeMap<TrackId, u64>,
    withdrawn: bool,
    /// The mine line of a mine-laying objective, and whether the line's speed has been ordered.
    line: Option<(DVec2, DVec2)>,
    mine_run: bool,
    /// The first orders went out (the hull is in the water).
    started: bool,
    /// Ordered speed on every course: the Helm's cruise fraction of the design's top speed.
    cruise_kn: f64,
    /// Gun mounts of the design (the site stand-off is measured from their effective range).
    guns: BTreeSet<PartSlot>,
    /// Site objective: where the hull holds, the pilot period it got there, and whether it has closed to
    /// visual range.
    standoff: Option<DVec2>,
    standoff_since: Option<u64>,
    site_closed: bool,
    /// Escort: the escorted hull has been met; the hull now keeps company with it to the exit.
    met: bool,
}

/// Which of the pilot's orders beyond sailing the briefing route go out. The gate plays with all of them
/// ([`PilotOrders::ALL`], the default of [`Pilot::new`]); the diagnosis harness can withhold them to see what
/// the route alone is worth (`bot::tests::gate`, `BP_PILOT=route`). Harness only.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct PilotOrders {
    /// May give any order after the first ones: the hunt, holds, station actions, prizes, withdrawals, the
    /// escort meeting and the leaving course. Off, the hull only sails the briefing route it got at the start.
    pub beyond_route: bool,
}
impl PilotOrders {
    pub(crate) const ALL: PilotOrders = PilotOrders { beyond_route: true };
}

pub(crate) struct Pilot {
    orders: PilotOrders,
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
    /// The weather's visibility (infinite when the config carries none).
    visibility_m: f64,
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
        let exits: Vec<DVec2> = config.map.markers.iter().filter(|m| m.kind == MarkerKind::Exit && finite(&m.pos_m)).map(|m| DVec2::from(m.pos_m)).collect();
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
            let guns: BTreeSet<PartSlot> = bp.as_ref().map(|b| b.parts.iter().enumerate().filter(|(_, pl)| matches!(data.catalogue.part(&pl.part).map(|d| &d.function), Some(PartFunction::Gun(_)))).filter_map(|(k, _)| u16::try_from(k).ok().map(PartSlot)).collect()).unwrap_or_default();
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
            // A cutting-out's route ends a stand-off short of the prize, not on top of it; its way out is laid
            // once the prize is taken (`leave`).
            let cut_out = matches!(config.objective, Objective::CutOut { .. });
            if cut_out { route = vec![site_standoff(spawn, objective_pos, BOARD_STANDOFF_M)]; }
            if let Some(e) = named_exit.filter(|_| !cut_out) { route.push(e); }
            if spawn.distance(route[0]) < WAYPOINT_M && route.len() > 1 { route.remove(0); }
            hulls.push(Hull {
                slot: p.slot, seq: 0, spawn, route, phase: Phase::Route, last_order: None, action_started: None, prized: BTreeSet::new(), photographed: BTreeSet::new(), photos_done: 0, photo_target: None, photo_best: None,
                photo_skipped: BTreeMap::new(), withdrawn: false, line, mine_run: false, started: false, cruise_kn, guns, standoff: None, standoff_since: None, site_closed: false, met: false,
            });
        }
        let tag = match &config.objective { Objective::DestroyGroup { tag, .. } | Objective::Capture { tag } | Objective::CutOut { tag, .. } | Objective::Photograph { tag, .. } | Objective::Escort { tag, .. } => Some(tag.as_str()), _ => None };
        let target_classes = config.vessels.iter()
            .filter(|v| match (&config.objective, tag) { (Objective::DestroySite { .. }, _) => v.role == VesselRole::Site, (_, Some(t)) => !t.is_empty() && v.tag == t, _ => false })
            .filter_map(|v| v.class.clone()).collect();
        let vis = config.weather.visibility_m;
        let photo_standoff_m = if vis.is_finite() && vis > 0.0 { PHOTO_STANDOFF_M.min(0.75 * vis).max(300.0) } else { PHOTO_STANDOFF_M };
        let mut pilot = Pilot {
            orders: PilotOrders::ALL, visibility_m: if vis.is_finite() && vis > 0.0 { vis } else { f64::INFINITY }, photo_standoff_m, target_classes, hulls, objective: config.objective.clone(), objective_pos, exits, named_exit, actions: config.actions.clone(), follow: setup.follow_briefing_route, emcon, rules,
            marker_names: markers, period: 0,
        };
        if matches!(pilot.objective, Objective::Escort { .. }) {
            for i in 0..pilot.hulls.len() { pilot.hulls[i].route = vec![pilot.flank(i, pilot.objective_pos)]; }
        }
        pilot
    }

    fn allowed(&self, a: StationAction) -> bool { self.actions.contains(&a) }

    /// Withhold the pilot's orders beyond the route (diagnosis only; the gate uses [`PilotOrders::ALL`]).
    pub(crate) fn with_orders(mut self, orders: PilotOrders) -> Pilot { self.orders = orders; self }
    /// The objective point the briefing route runs to.
    pub(crate) fn objective_pos(&self) -> DVec2 { self.objective_pos }
}

/// The objective point the briefing route runs to: the generator's rule (`opgen`'s objective point). A
/// cutting-out ends where its prize lies (the tagged prize's own position in the config), which the generator
/// puts at the berth its briefing route ends at, not at the map's first objective-kind marker.
fn objective_point(config: &SortieConfig, data: &TheatreData) -> DVec2 {
    let find = |name: &str| config.map.markers.iter().find(|m| m.name == name).map(|m| DVec2::from(m.pos_m));
    let named = match &config.objective { Objective::DestroySite { marker } | Objective::Extract { marker, .. } | Objective::LayMines { marker, .. } => find(marker), _ => None };
    let prize = match &config.objective {
        Objective::CutOut { tag, .. } if !tag.is_empty() => config.vessels.iter().find(|v| v.tag == *tag && v.role == VesselRole::Prize).map(|v| DVec2::new(v.pos.x, v.pos.z)).filter(|p| p.is_finite()),
        _ => None,
    };
    let p = named.or(prize)
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
            let slot = self.hulls[i].slot;
            let snap = sim.snapshot(Viewer::Player(slot));
            // The hull's own finished station actions: they leave the snapshot's list as they finish.
            let finished: Vec<(StationAction, Option<EntityId>)> = sim.specops.completed.iter().filter(|c| c.player == Some(slot)).map(|c| (c.action, c.target)).collect();
            self.steer(i, &snap, &finished, tick, out);
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
            // Cruise all the way in; a mine line's own speed is ordered at the line (`steer`).
            let cruise = self.hulls[i].cruise_kn;
            self.order(i, tick, Command::SetCourse { waypoints: route, speed_kn: Some(cruise), append: false }, out);
        }
        if self.orders.beyond_route { self.start_marker_action(i, tick, out); }
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

    fn steer(&mut self, i: usize, snap: &Snapshot, finished: &[(StationAction, Option<EntityId>)], tick: u64, out: &mut Vec<CommandEnvelope>) {
        let Some(ship) = snap.ship.as_ref() else { return };
        let Some(me) = snap.vessels.iter().find(|v| v.entity == ship.vessel) else { return };
        let pos = plane(me.kin.pose.pos);
        if !(pos.x.is_finite() && pos.y.is_finite()) { return; }

        // Route progress.
        while self.hulls[i].route.first().is_some_and(|w| w.distance(pos) < WAYPOINT_M) { self.hulls[i].route.remove(0); }
        if self.hulls[i].phase == Phase::Route && (self.hulls[i].route.is_empty() || pos.distance(self.objective_pos) < WAYPOINT_M) { self.hulls[i].phase = Phase::Objective; }
        if !self.orders.beyond_route { return; }
        let primary = snap.objectives.iter().find(|o| !o.ghost);
        let primary_done = primary.is_some_and(|o| o.state == ObjectiveState::Done);
        let period = self.period;
        let retry_due = |h: &Hull| h.action_started.is_none_or(|s| period.saturating_sub(s) >= ACTION_RETRY_PERIODS);
        let action_state = |a: StationAction| -> (bool, bool) {
            let done = finished.iter().any(|x| x.0 == a);
            let running = ship.actions.iter().any(|x| x.action == a && matches!(x.state, ActionState::Running | ActionState::Paused));
            (done, running)
        };

        match self.objective.clone() {
            Objective::DestroySite { .. } => self.strike_site(i, pos, snap, tick, out),
            Objective::DestroyAll | Objective::DestroyGroup { .. } => {
                // A hull the objective names, tracked with a position, ends the route at once: the Helm steers
                // for it rather than sailing to a fixed point while a convoy or patrol moves on.
                let named = !self.target_classes.is_empty() && snap.tracks.iter().any(|t| huntable(t, tick) && t.grade != TrackGrade::Bearing && t.class.as_ref().is_some_and(|c| self.target_classes.contains(c)));
                if self.hulls[i].phase != Phase::Route || named { self.hunt(i, pos, snap, tick, out); }
            }
            Objective::Extract { marker, .. } => {
                let at = self.marker_names.get(&marker).copied().unwrap_or(self.objective_pos);
                let (done, running) = action_state(StationAction::InsertOrExtract);
                // The objective's own progress (0.5 once held) says the extraction is done as well.
                let held = primary.is_some_and(|o| o.progress >= 0.5);
                if done || held || self.hulls[i].phase == Phase::Leaving {
                    self.hulls[i].phase = Phase::Leaving;
                    self.leave(i, pos, tick, out);
                } else {
                    if !running && retry_due(&self.hulls[i]) { self.start_marker_action(i, tick, out); }
                    if pos.distance(at) < HOLD_APPROACH_M { self.order(i, tick, Command::HoldStation { at, heading_rad: None }, out); }
                }
            }
            Objective::LayMines { .. } => {
                let (done, running) = action_state(StationAction::LayMines);
                if !done && !running && retry_due(&self.hulls[i]) { self.start_marker_action(i, tick, out); }
                if done || primary_done { self.hulls[i].phase = Phase::Leaving; }
                if self.hulls[i].phase == Phase::Leaving { self.leave(i, pos, tick, out); }
                else if self.hulls[i].route.is_empty() { self.order(i, tick, Command::HoldStation { at: self.objective_pos, heading_rad: None }, out); }
                else if let Some((a, _)) = self.hulls[i].line {
                    // On the line (its start is behind): the rest of the route at the mine-laying speed.
                    if !self.hulls[i].mine_run && self.hulls[i].route.first() != Some(&a) {
                        self.hulls[i].mine_run = true;
                        let kn = MINE_RUN_KN.min(self.hulls[i].cruise_kn).max(MINE_MIN_KN);
                        self.order(i, tick, Command::SetCourse { waypoints: self.hulls[i].route.clone(), speed_kn: Some(kn), append: false }, out);
                    }
                }
            }
            Objective::Photograph { .. } => self.photograph(i, pos, snap, finished, tick, out),
            Objective::Capture { .. } | Objective::CutOut { .. } => self.board(i, pos, snap, finished, tick, out),
            Objective::Feint { .. } => {
                let held = primary.is_some_and(|o| o.progress >= 0.9);
                if held && !self.hulls[i].withdrawn && self.follow {
                    self.hulls[i].withdrawn = true;
                    self.hulls[i].phase = Phase::Leaving;
                    self.push(i, tick, Command::Withdraw, out);
                }
                if self.hulls[i].phase == Phase::Leaving { self.leave(i, pos, tick, out); }
                else if self.hulls[i].phase == Phase::Objective { self.order(i, tick, Command::HoldStation { at: self.objective_pos, heading_rad: None }, out); }
            }
            Objective::Escort { .. } => {
                // Meet the escorted hull (the nearest Neutral contact, its class first; with no contact, where
                // the briefing puts it) from abeam, then keep company with it to the exit on a parallel track.
                // The escort never sails or stops on the charge's own way out: see `flank`.
                let wanted = |t: &&crate::protocol::snapshot::TrackView| t.class.as_ref().is_some_and(|c| self.target_classes.contains(c));
                let contact = snap.tracks.iter()
                    .filter(|t| !t.last_known && t.domain == Domain::Surface && t.stance == Stance::Neutral && t.pos.x.is_finite() && t.pos.z.is_finite())
                    .min_by(|a, b| (!wanted(a)).cmp(&!wanted(b)).then(plane(a.pos).distance_squared(pos).total_cmp(&plane(b.pos).distance_squared(pos))).then(a.id.cmp(&b.id)))
                    .map(|t| plane(t.pos));
                if !self.hulls[i].met {
                    match contact {
                        Some(at) if at.distance(pos) <= ESCORT_MEET_M => self.hulls[i].met = true,
                        Some(at) => {
                            if self.period.is_multiple_of(HUNT_PERIODS) || !matches!(self.hulls[i].last_order, Some(Command::SetCourse { ref waypoints, .. }) if waypoints.len() == 1) {
                                let abeam = self.flank(i, at);
                                self.order(i, tick, Command::SetCourse { waypoints: vec![abeam], speed_kn: Some(self.hulls[i].cruise_kn), append: false }, out);
                            }
                        }
                        None => {}
                    }
                }
                if (self.hulls[i].met || self.hulls[i].route.is_empty()) && !primary_done { self.leave(i, pos, tick, out); }
            }
            Objective::Survive { .. } => {
                if self.hulls[i].route.is_empty() && !primary_done { self.leave(i, pos, tick, out); }
            }
        }
    }

    /// The point [`ESCORT_FLANK_M`] abeam of `at`, across the leg from the objective point to the objective's
    /// exit, on the side the hull came in from. An escort and a prize's captor sail and wait there, never on
    /// the leg itself. Observed on the defector escort with no hostile hull present (22 November 2026, seed
    /// 0): with the escort sailing ahead of it and then waiting on the exit marker, the defector shuttled
    /// between its anchorage and a point about 3 km short of the escort for 30 minutes and never reached the
    /// exit; with the escort abeam it sails straight there. The cause inside the AI (it looks like the
    /// planned route being taken again from its first waypoint after a turn away from a hull ahead) is not
    /// traced.
    fn flank(&self, i: usize, at: DVec2) -> DVec2 {
        let Some(exit) = self.named_exit else { return at };
        let leg = (exit - self.objective_pos).normalize_or_zero();
        if leg == DVec2::ZERO { return at; }
        let side = leg.perp();
        let sign = if (self.hulls[i].spawn - self.objective_pos).dot(side) < 0.0 { -1.0 } else { 1.0 };
        at + side * sign * ESCORT_FLANK_M
    }

    /// Sail at the Helm's cruise for the objective's exit, else the nearest exit marker; with none, withdraw
    /// in place. Where the objective is finished by another hull reaching the exit (the escorted hull, the
    /// prize) the hull makes for the point abeam of the exit instead.
    fn leave(&mut self, i: usize, pos: DVec2, tick: u64, out: &mut Vec<CommandEnvelope>) {
        let exit = self.named_exit.or_else(|| nearest(pos, &self.exits));
        let exit = if matches!(self.objective, Objective::Escort { .. } | Objective::CutOut { .. }) { exit.map(|e| self.flank(i, e)) } else { exit };
        let kn = self.hulls[i].cruise_kn;
        match exit {
            Some(e) if e.distance(pos) >= 100.0 => self.order(i, tick, Command::SetCourse { waypoints: vec![e], speed_kn: Some(kn), append: false }, out),
            Some(e) => self.order(i, tick, Command::HoldStation { at: e, heading_rad: None }, out),
            None => if !self.hulls[i].withdrawn && self.follow { self.hulls[i].withdrawn = true; self.push(i, tick, Command::Withdraw, out); },
        }
    }

    /// A site objective: sail to the stand-off on the approach bearing and hold there. See the module docs.
    fn strike_site(&mut self, i: usize, pos: DVec2, snap: &Snapshot, tick: u64, out: &mut Vec<CommandEnvelope>) {
        let Some(ship) = snap.ship.as_ref() else { return };
        let site = self.objective_pos;
        let (spawn, cruise) = (self.hulls[i].spawn, self.hulls[i].cruise_kn);
        let guns: Vec<&crate::protocol::snapshot::MountView> = ship.mounts.iter().filter(|m| self.hulls[i].guns.contains(&m.slot)).collect();
        if self.hulls[i].standoff.is_none() {
            let reach = guns.iter().map(|m| m.effective_range_m as f64).filter(|r| r.is_finite()).fold(0.0, f64::max);
            // The range is not in the first snapshots of a hull whose doctrine has not run yet: wait for it.
            if reach <= 0.0 && self.period < 5 { return; }
            let at = site_standoff(spawn, site, standoff_range(reach));
            self.hulls[i].standoff = Some(at);
            self.hulls[i].route = vec![at];
        }
        let Some(at) = self.hulls[i].standoff else { return };
        if pos.distance(at) >= WAYPOINT_M {
            self.hulls[i].phase = Phase::Route;
            self.order(i, tick, Command::SetCourse { waypoints: vec![at], speed_kn: Some(cruise), append: false }, out);
            return;
        }
        self.hulls[i].phase = Phase::Objective;
        let since = *self.hulls[i].standoff_since.get_or_insert(self.period);
        // No track of the site from the stand-off: close once to visual range, unless every gun is empty.
        let tracked = snap.tracks.iter().any(|t| !t.last_known && t.domain == Domain::Surface && t.pos.x.is_finite() && t.pos.z.is_finite() && (plane(t.pos).distance(site) <= SITE_TRACK_M || t.class.as_ref().is_some_and(|c| self.target_classes.contains(c))));
        let empty = !guns.is_empty() && guns.iter().all(|m| matches!(m.status, crate::protocol::text::EngageStatus::Holding { reason: crate::protocol::text::HoldReason::NoAmmunition, .. }));
        if !tracked && !empty && !self.hulls[i].site_closed && self.period.saturating_sub(since) >= SITE_SEARCH_PERIODS {
            let closer = site_standoff(spawn, site, (SITE_VISUAL_FRACTION * self.visibility_m).max(SITE_STANDOFF_MIN_M));
            self.hulls[i].site_closed = true;
            if closer.distance(site) < at.distance(site) {
                self.hulls[i].standoff = Some(closer);
                self.hulls[i].standoff_since = None;
                self.hulls[i].route = vec![closer];
                return;
            }
        }
        self.order(i, tick, Command::HoldStation { at, heading_rad: None }, out);
    }

    /// Steer for the reported position of the nearest hostile surface contact that has one (re-aimed every
    /// [`HUNT_PERIODS`]), else hold at the objective point.
    fn hunt(&mut self, i: usize, pos: DVec2, snap: &Snapshot, tick: u64, out: &mut Vec<CommandEnvelope>) {
        let holding = matches!(self.hulls[i].last_order, Some(Command::HoldStation { .. }));
        // The objective's own hulls first (by class, once identified), then the nearest hostile. A contact
        // with a position before a bearing-only one, whose range is a guess; only contacts a sensor has
        // updated lately, since a coasting track's extrapolation runs off across the map.
        let wanted = |t: &&crate::protocol::snapshot::TrackView| t.class.as_ref().is_some_and(|c| self.target_classes.contains(c));
        let rank = |t: &&crate::protocol::snapshot::TrackView| (t.grade == TrackGrade::Bearing, !wanted(t));
        let target = snap.tracks.iter().filter(|t| huntable(t, tick))
            .min_by(|a, b| rank(a).cmp(&rank(b)).then(plane(a.pos).distance_squared(pos).total_cmp(&plane(b.pos).distance_squared(pos))).then(a.id.cmp(&b.id)))
            .map(|t| plane(t.pos));
        // Re-aim on the cadence, or at once when a contact appears while holding or the route just ended.
        let fresh = self.hulls[i].phase == Phase::Route || (holding && target.is_some());
        self.hulls[i].phase = Phase::Objective;
        if !(fresh || self.period.is_multiple_of(HUNT_PERIODS)) { return; }
        match target {
            Some(at) => self.order(i, tick, Command::SetCourse { waypoints: vec![at], speed_kn: Some(self.hulls[i].cruise_kn), append: false }, out),
            None => self.order(i, tick, Command::HoldStation { at: self.objective_pos, heading_rad: None }, out),
        }
    }

    fn photograph(&mut self, i: usize, pos: DVec2, snap: &Snapshot, finished: &[(StationAction, Option<EntityId>)], tick: u64, out: &mut Vec<CommandEnvelope>) {
        let Some(ship) = snap.ship.as_ref() else { return };
        let photo = ship.actions.iter().rfind(|a| a.action == StationAction::Photograph);
        let running = photo.is_some_and(|a| matches!(a.state, ActionState::Running | ActionState::Paused));
        // A photograph that finished since the last look was of the target in hand.
        let done_now = finished.iter().filter(|x| x.0 == StationAction::Photograph).count();
        if done_now > self.hulls[i].photos_done {
            self.hulls[i].photos_done = done_now;
            if let Some(t) = self.hulls[i].photo_target.take() { self.hulls[i].photographed.insert(t); }
            self.hulls[i].photo_best = None;
        }
        // A target is live while a sensor still updates it (a coasting ESM range guess drifts off).
        let live = |id: TrackId| snap.tracks.iter().find(|t| t.id == id && !t.last_known && tick.saturating_sub(t.last_update_tick) <= HUNT_FRESH_TICKS);
        if self.hulls[i].photo_target.is_some_and(|t| live(t).is_none()) { self.hulls[i].photo_target = None; self.hulls[i].photo_best = None; }
        // A target whose range has not closed for a while is given up for a time: it is sailing away at the
        // hull's own speed, or lies behind land.
        let period = self.period;
        self.hulls[i].photo_skipped.retain(|_, at| period.saturating_sub(*at) < PHOTO_SKIP_PERIODS);
        if let Some(t) = self.hulls[i].photo_target.and_then(live) {
            let range = plane(t.pos).distance(pos);
            let (stalled, best) = photo_stalled(self.hulls[i].photo_best, range, period);
            self.hulls[i].photo_best = best;
            if stalled && !running_unblocked(photo) {
                self.hulls[i].photo_skipped.insert(t.id, period);
                self.hulls[i].photo_target = None;
                self.hulls[i].photo_best = None;
                if let Some(a) = photo.filter(|_| running) { self.push(i, tick, Command::CancelAction(a.id), out); }
            }
        }
        let allowed = self.allowed(StationAction::Photograph);
        if self.hulls[i].photo_target.is_none() {
            let h = &self.hulls[i];
            let near = PHOTO_NEAR_STANDOFFS * self.photo_standoff_m;
            // A contact with a position inside 1.5 stand-offs first (the nearest), then the objective's
            // classes, then hostile contacts, then any other non-Friend contact.
            let rank = |t: &crate::protocol::snapshot::TrackView| (t.grade == TrackGrade::Bearing, plane(t.pos).distance(pos) > near, !t.class.as_ref().is_some_and(|c| self.target_classes.contains(c)), !matches!(t.stance, Stance::Hostile | Stance::Suspect));
            let pick = snap.tracks.iter()
                .filter(|t| !t.last_known && t.domain == Domain::Surface && t.stance != Stance::Friend && !h.photographed.contains(&t.id) && !h.photo_skipped.contains_key(&t.id) && t.pos.x.is_finite() && t.pos.z.is_finite() && tick.saturating_sub(t.last_update_tick) <= HUNT_FRESH_TICKS)
                .min_by(|a, b| rank(a).cmp(&rank(b)).then(plane(a.pos).distance_squared(pos).total_cmp(&plane(b.pos).distance_squared(pos))).then(a.id.cmp(&b.id)))
                .map(|t| t.id);
            if let Some(t) = pick {
                self.hulls[i].photo_target = Some(t);
                self.hulls[i].photo_best = None;
                // The camera is started on a contact with a position; a bearing-only one is only steered for
                // (the retry below starts the camera once it has a position).
                if allowed && live(t).is_some_and(|t| t.grade != TrackGrade::Bearing) {
                    self.hulls[i].action_started = Some(self.period);
                    self.push(i, tick, Command::StartAction { action: StationAction::Photograph, target: ActionTarget::Track(t) }, out);
                }
            }
        } else if !running && allowed && self.hulls[i].action_started.is_none_or(|s| self.period.saturating_sub(s) >= ACTION_RETRY_PERIODS) {
            if let Some(t) = self.hulls[i].photo_target.filter(|t| live(*t).is_some_and(|t| t.grade != TrackGrade::Bearing)) {
                self.hulls[i].action_started = Some(self.period);
                self.push(i, tick, Command::StartAction { action: StationAction::Photograph, target: ActionTarget::Track(t) }, out);
            }
        }
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

    fn board(&mut self, i: usize, pos: DVec2, snap: &Snapshot, finished: &[(StationAction, Option<EntityId>)], tick: u64, out: &mut Vec<CommandEnvelope>) {
        let Some(ship) = snap.ship.as_ref() else { return };
        let running = ship.actions.iter().any(|a| a.action == StationAction::BoardAndCapture && matches!(a.state, ActionState::Running | ActionState::Paused));
        // Never a Neutral hull: a decoy convoy sails hulls of the prize's own classes.
        let hostile_track = |id: TrackId| !id.is_none() && snap.tracks.iter().any(|t| t.id == id && matches!(t.stance, Stance::Hostile | Stance::Suspect));
        let dist = |v: &crate::protocol::snapshot::VesselView| plane(v.kin.pose.pos).distance_squared(pos);
        // Every boarded hull goes home under its own Helm, once, when its boarding finishes.
        for e in prizes_due(finished, &self.hulls[i].prized) {
            self.hulls[i].prized.insert(e);
            self.push(i, tick, Command::Prize { prize: e, decision: PrizeDecision::SailHome }, out);
        }
        if !self.hulls[i].prized.is_empty() && matches!(self.objective, Objective::CutOut { .. }) { self.hulls[i].phase = Phase::Leaving; }
        if self.hulls[i].phase == Phase::Leaving { self.leave(i, pos, tick, out); return; }
        // A disabled enemy hull in visual range: close and board. Only a hull whose track carries the
        // objective's class (a prize is briefing-named, so its track does from first contact): a disabled
        // escort is not the prize, and boarding one kept the hull alongside the convoy's guns for nothing.
        let class_of = |id: TrackId| snap.tracks.iter().find(|t| t.id == id).and_then(|t| t.class.as_ref());
        let is_prize = |v: &&crate::protocol::snapshot::VesselView| self.target_classes.is_empty() || class_of(v.track).is_some_and(|c| self.target_classes.contains(c));
        let target = snap.vessels.iter()
            .filter(|v| v.entity != ship.vessel && v.status == VesselStatus::Disabled && hostile_track(v.track) && !self.hulls[i].prized.contains(&v.entity))
            .filter(is_prize)
            .min_by(|a, b| dist(a).total_cmp(&dist(b)).then(a.entity.cmp(&b.entity)));
        match target {
            Some(v) => {
                let at = plane(v.kin.pose.pos);
                self.hulls[i].phase = Phase::Objective;
                let due = self.hulls[i].action_started.is_none_or(|s| self.period.saturating_sub(s) >= ACTION_RETRY_PERIODS);
                if !running && self.allowed(StationAction::BoardAndCapture) && due {
                    self.hulls[i].action_started = Some(self.period);
                    self.push(i, tick, Command::StartAction { action: StationAction::BoardAndCapture, target: ActionTarget::Track(v.track) }, out);
                }
                let d = at.distance(pos);
                if d > HOLD_APPROACH_M {
                    if self.period.is_multiple_of(10) || !matches!(self.hulls[i].last_order, Some(Command::SetCourse { .. })) { self.order(i, tick, Command::SetCourse { waypoints: vec![at], speed_kn: Some(self.hulls[i].cruise_kn), append: false }, out); }
                } else {
                    let off = if d > 1.0 { (pos - at) / d * BOARD_HOLD_M } else { DVec2::new(BOARD_HOLD_M, 0.0) };
                    let hold = at + off;
                    let moved = !matches!(&self.hulls[i].last_order, Some(Command::HoldStation { at: h, .. }) if h.distance(hold) < 50.0);
                    if moved { self.order(i, tick, Command::HoldStation { at: hold, heading_rad: None }, out); }
                }
            }
            None => if self.hulls[i].phase != Phase::Route { self.close_to_board(i, pos, snap, tick, out); },
        }
    }

    /// No hull is ready to board yet: close the nearest contact that could be the prize (the objective's class
    /// first) to [`BOARD_STANDOFF_M`] and hold there, where Gunnery engages a Suspect track and the hull does
    /// not run the prize down; with no contact, hold at the objective point.
    fn close_to_board(&mut self, i: usize, pos: DVec2, snap: &Snapshot, tick: u64, out: &mut Vec<CommandEnvelope>) {
        let wanted = |t: &&crate::protocol::snapshot::TrackView| t.class.as_ref().is_some_and(|c| self.target_classes.contains(c));
        let rank = |t: &&crate::protocol::snapshot::TrackView| (t.grade == TrackGrade::Bearing, !wanted(t));
        let target = snap.tracks.iter().filter(|t| huntable(t, tick))
            .min_by(|a, b| rank(a).cmp(&rank(b)).then(plane(a.pos).distance_squared(pos).total_cmp(&plane(b.pos).distance_squared(pos))).then(a.id.cmp(&b.id)))
            .map(|t| plane(t.pos));
        let holding = matches!(self.hulls[i].last_order, Some(Command::HoldStation { .. }));
        let fresh = self.hulls[i].phase == Phase::Route || (holding && target.is_some());
        self.hulls[i].phase = Phase::Objective;
        if !(fresh || self.period.is_multiple_of(HUNT_PERIODS)) { return; }
        match target {
            Some(at) if at.distance(pos) > BOARD_STANDOFF_M + WAYPOINT_M => {
                let short = site_standoff(pos, at, BOARD_STANDOFF_M);
                self.order(i, tick, Command::SetCourse { waypoints: vec![short], speed_kn: Some(self.hulls[i].cruise_kn), append: false }, out)
            }
            Some(_) => { if !holding { self.order(i, tick, Command::HoldStation { at: pos, heading_rad: None }, out); } }
            None => self.order(i, tick, Command::HoldStation { at: self.objective_pos, heading_rad: None }, out),
        }
    }
}

/// A contact the hunt may steer for: a live hostile or suspect surface track with a finite position that a
/// sensor updated within [`HUNT_FRESH_TICKS`].
fn huntable(t: &crate::protocol::snapshot::TrackView, tick: u64) -> bool {
    !t.last_known && t.domain == Domain::Surface && matches!(t.stance, Stance::Hostile | Stance::Suspect) && t.pos.x.is_finite() && t.pos.z.is_finite() && tick.saturating_sub(t.last_update_tick) <= HUNT_FRESH_TICKS
}

/// Range a site is engaged from: [`SITE_STANDOFF_FRACTION`] of the longest gun's effective range, never inside
/// [`SITE_STANDOFF_MIN_M`] (a hull with no gun range holds at the floor).
fn standoff_range(gun_range_m: f64) -> f64 {
    let r = if gun_range_m.is_finite() { SITE_STANDOFF_FRACTION * gun_range_m } else { 0.0 };
    r.max(SITE_STANDOFF_MIN_M)
}

/// The point `range` from the site on the bearing the hull approaches from; the spawn itself when the hull
/// starts inside that range.
fn site_standoff(spawn: DVec2, site: DVec2, range: f64) -> DVec2 {
    let d = spawn.distance(site);
    if !(d.is_finite() && d > range) { return spawn; }
    site + (spawn - site) / d * range
}

/// The boarded hulls whose prize decision is due: every hull one of the pilot's own boardings finished on
/// that has not been ordered home yet, in the order the boardings finished.
fn prizes_due(finished: &[(StationAction, Option<EntityId>)], prized: &BTreeSet<EntityId>) -> Vec<EntityId> {
    let mut due: Vec<EntityId> = Vec::new();
    for e in finished.iter().filter(|x| x.0 == StationAction::BoardAndCapture).filter_map(|x| x.1) {
        if !prized.contains(&e) && !due.contains(&e) { due.push(e); }
    }
    due
}

/// Whether the camera's target has stalled, and the new record of its closest range: stalled when the range
/// has not come down by [`PHOTO_CLOSING_M`] for [`PHOTO_STALL_PERIODS`] (the caller keeps a target the
/// camera is running on).
fn photo_stalled(best: Option<(f64, u64)>, range: f64, period: u64) -> (bool, Option<(f64, u64)>) {
    let best = match best { Some((r, at)) if range > r - PHOTO_CLOSING_M => (r, at), _ => (range, period) };
    (period.saturating_sub(best.1) >= PHOTO_STALL_PERIODS, Some(best))
}

/// The camera is running with nothing in its way (a photograph in progress is never given up).
fn running_unblocked(photo: Option<&crate::protocol::snapshot::ActionView>) -> bool { photo.is_some_and(|a| a.state == ActionState::Running && a.block.is_none()) }

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

    // ---- the pilot on hand-built snapshots (no simulation) ----

    use crate::protocol::snapshot::{MountView, ShipStatus, TrackView, UncertaintyShape, VesselView};
    use crate::protocol::text::{EngageStatus, HoldReason};
    use content::campaign::MissionType;

    const SETUP: PlayerSetup = PlayerSetup { reference_setup: None, method: None, emcon: None, ghosts: false, follow_briefing_route: true };

    fn template(m: MissionType, d: &TheatreData) -> SortieConfig {
        crate::theatre::opgen::generate_standalone(m, Act::Act1, 1, 3, &content::campaign::DifficultySliders::NORMAL, None, d).expect("an Act 1 template").config
    }

    fn own() -> EntityId { EntityId::from_u64(1) }

    /// The player's snapshot with the hull at `pos`.
    fn snapshot(pos: DVec2, tracks: Vec<TrackView>, mounts: Vec<MountView>) -> Snapshot {
        let mut kin = Kinematics::default();
        kin.pose.pos = DVec3::new(pos.x, 0.0, pos.y);
        let me = VesselView { entity: own(), kin, status: VesselStatus::default(), flags: Default::default(), turrets: Vec::new(), rudder: 0.0, power: 0.0, track: TrackId::NONE, relevance: Default::default() };
        Snapshot { ship: Some(ShipStatus { vessel: own(), mounts, ..Default::default() }), vessels: vec![me], tracks, ..Default::default() }
    }

    fn track(id: u32, stance: Stance, grade: TrackGrade, pos: DVec2, vel: DVec2, class: Option<ClassId>, tick: u64) -> TrackView {
        TrackView {
            id: TrackId(id), domain: Domain::Surface, stance, stance_reason: Default::default(), rung: Default::default(), rung_reason: Default::default(), grade, pos: DVec3::new(pos.x, 0.0, pos.y), vel: DVec3::new(vel.x, 0.0, vel.y),
            shape: UncertaintyShape::Circle { radius_m: 50.0 }, last_update_tick: tick, last_known: false, class, candidates: 1, sources: Default::default(), trend: Default::default(), inbound: false, engaged_by: 0, envelope_m: 0.0, emitting: false, relevance: Default::default(), struck: false,
        }
    }

    fn gun(slot: u16, range_m: f32, status: EngageStatus) -> MountView { MountView { slot: PartSlot(slot), status, holder: Default::default(), solution: None, effective_range_m: range_m, local_control: false } }

    /// Steer hull 0 for `periods` pilot periods on the snapshot `at(period)` gives; returns every command.
    fn steer_for(p: &mut Pilot, periods: u64, finished: &[(StationAction, Option<EntityId>)], mut at: impl FnMut(u64, u64) -> Snapshot) -> Vec<Command> {
        let mut out = Vec::new();
        for _ in 0..periods {
            p.period += 1;
            let tick = p.period * PILOT_PERIOD;
            let snap = at(p.period, tick);
            p.steer(0, &snap, finished, tick, &mut out);
        }
        out.into_iter().map(|c| c.command).collect()
    }

    fn sensor_orders(cmds: &[Command]) -> usize { cmds.iter().filter(|c| matches!(c, Command::SetEmcon(_) | Command::Designate { .. })).count() }

    /// The hunt steers for where the contact is reported, at cruise, and the pilot neither designates it nor
    /// brings the radars up, with a contact or without one (L108, L307, L507).
    #[test]
    fn the_hunt_steers_for_the_reported_position_and_leaves_sensors_and_designation_alone() {
        let d = data();
        let c = convoy_raid(&d);
        let mut p = Pilot::new(&c, &SETUP, &d);
        let class = p.target_classes.iter().next().cloned().expect("the convoy has a class");
        let cruise = p.hulls[0].cruise_kn;
        let (objective, contact, vel) = (p.objective_pos, p.objective_pos + DVec2::new(6_000.0, 2_000.0), DVec2::new(0.0, 7.5));
        // Ten minutes at the objective point with no contact: a hold, and no order to radiate.
        let quiet = steer_for(&mut p, 600, &[], |_, _| snapshot(objective, Vec::new(), Vec::new()));
        assert_eq!(sensor_orders(&quiet), 0, "{quiet:?}");
        assert!(quiet.iter().any(|c| matches!(c, Command::HoldStation { .. })));
        // A moving contact of the convoy's class: the course is its reported position, not a point led ahead.
        let chase = steer_for(&mut p, 120, &[], |_, tick| snapshot(objective, vec![track(7, Stance::Hostile, TrackGrade::Coarse, contact, vel, Some(class.clone()), tick)], Vec::new()));
        assert_eq!(sensor_orders(&chase), 0, "{chase:?}");
        let courses: Vec<&Command> = chase.iter().filter(|c| matches!(c, Command::SetCourse { .. })).collect();
        assert!(!courses.is_empty());
        for c in courses {
            let Command::SetCourse { waypoints, speed_kn, .. } = c else { unreachable!() };
            assert_eq!((waypoints.as_slice(), *speed_kn), ([contact].as_slice(), Some(cruise)), "no lead, and never more than the Helm's cruise");
        }
        // A bearing-only contact has no position to steer for while one with a position is held.
        let mut p = Pilot::new(&c, &SETUP, &d);
        let far = objective + DVec2::new(-9_000.0, 0.0);
        let both = steer_for(&mut p, 30, &[], |_, tick| snapshot(objective, vec![track(8, Stance::Hostile, TrackGrade::Bearing, far, DVec2::ZERO, Some(class.clone()), tick), track(9, Stance::Hostile, TrackGrade::Coarse, contact, DVec2::ZERO, None, tick)], Vec::new()));
        assert!(both.iter().all(|c| !matches!(c, Command::SetCourse { waypoints, .. } if waypoints[0] == far)), "{both:?}");
    }

    /// A site is engaged from 0.8 of the longest gun's effective range on the approach bearing, never from
    /// inside 1,000 m; the hull closes once to visual range when it holds no track of the site, and never
    /// with its magazines empty.
    #[test]
    fn a_site_is_held_from_the_stand_off() {
        let d = data();
        let c = template(MissionType::DepotStrike, &d);
        let fresh = |vis: f64| { let mut p = Pilot::new(&c, &SETUP, &d); p.hulls[0].guns = [PartSlot(4)].into_iter().collect(); p.visibility_m = vis; p };
        let closest = |cmds: &[Command], site: DVec2| cmds.iter().filter_map(|c| match c { Command::SetCourse { waypoints, .. } => waypoints.iter().map(|w| w.distance(site)).reduce(f64::min), Command::HoldStation { at, .. } => Some(at.distance(site)), _ => None }).fold(f64::INFINITY, f64::min);
        let mut p = fresh(20_000.0);
        let (site, spawn) = (p.objective_pos, p.hulls[0].spawn);
        let bearing = (spawn - site).normalize();
        let ready = || vec![gun(4, 8_000.0, EngageStatus::Idle), gun(9, 30_000.0, EngageStatus::Idle)];
        // On the way in: a course for the stand-off (slot 9 is not a gun of the design and does not count).
        let approach = steer_for(&mut p, 3, &[], |_, _| snapshot(spawn, Vec::new(), ready()));
        let standoff = site + bearing * 6_400.0;
        assert!(approach.iter().any(|c| matches!(c, Command::SetCourse { waypoints, .. } if waypoints.len() == 1 && waypoints[0].distance(standoff) < 1.0)), "{approach:?}");
        // At the stand-off with the site tracked: a hold there, for as long as it takes.
        let hold = steer_for(&mut p, 600, &[], |_, tick| snapshot(standoff, vec![track(3, Stance::Hostile, TrackGrade::Fine, site, DVec2::ZERO, None, tick)], ready()));
        assert!(hold.iter().any(|c| matches!(c, Command::HoldStation { at, .. } if at.distance(standoff) < 1.0)));
        assert!((closest(&hold, site) - 6_400.0).abs() < 1.0 && sensor_orders(&hold) == 0, "{hold:?}");
        // No track of the site in 2,000 m visibility: after a minute it closes to 1,500 m, once.
        let mut p = fresh(2_000.0);
        let blind = steer_for(&mut p, 300, &[], |_, _| snapshot(standoff, Vec::new(), ready()));
        assert!((closest(&blind, site) - 1_500.0).abs() < 1.0 && sensor_orders(&blind) == 0, "{blind:?}");
        // In 500 m visibility the floor holds.
        let mut p = fresh(500.0);
        let fog = steer_for(&mut p, 300, &[], |_, _| snapshot(standoff, Vec::new(), ready()));
        assert!((closest(&fog, site) - SITE_STANDOFF_MIN_M).abs() < 1.0, "{fog:?}");
        // Magazines empty: it stays at the stand-off.
        let mut p = fresh(2_000.0);
        let out_of_ammo = EngageStatus::Holding { track: TrackId::NONE, reason: HoldReason::NoAmmunition };
        let spent = steer_for(&mut p, 300, &[], |_, _| snapshot(standoff, Vec::new(), vec![gun(4, 8_000.0, out_of_ammo)]));
        assert!((closest(&spent, site) - 6_400.0).abs() < 1.0, "{spent:?}");
        // The pure rules.
        assert_eq!((standoff_range(8_000.0), standoff_range(600.0), standoff_range(f64::NAN)), (6_400.0, 1_000.0, 1_000.0));
        assert_eq!(site_standoff(DVec2::new(500.0, 0.0), DVec2::ZERO, 1_000.0), DVec2::new(500.0, 0.0), "a hull that starts inside the range holds where it is");
    }

    /// The approach to a mine line is sailed at the Helm's cruise and only the line at the mine-laying speed.
    #[test]
    fn the_mine_line_alone_is_sailed_slowly() {
        let d = data();
        let c = template(MissionType::Minelaying, &d);
        let mut p = Pilot::new(&c, &SETUP, &d);
        let (a, b) = p.hulls[0].line.expect("a mine line");
        let cruise = p.hulls[0].cruise_kn;
        assert!(cruise > MINE_RUN_KN + 1.0, "the starter cruises at {cruise:.1} kn");
        let mut out = Vec::new();
        p.first_orders(0, 0, &mut out);
        let first: Vec<Command> = out.into_iter().map(|c| c.command).collect();
        assert!(first.iter().any(|c| matches!(c, Command::SetCourse { waypoints, speed_kn, .. } if waypoints[0] == a && *speed_kn == Some(cruise))), "{first:?}");
        // Short of the line's start: nothing new. At it: the rest of the route at 10 kn.
        let spawn = p.hulls[0].spawn;
        let early = steer_for(&mut p, 5, &[], |_, _| snapshot(spawn, Vec::new(), Vec::new()));
        assert!(early.iter().all(|c| !matches!(c, Command::SetCourse { .. })), "{early:?}");
        let on_line = steer_for(&mut p, 5, &[], |_, _| snapshot(a, Vec::new(), Vec::new()));
        let runs: Vec<&Command> = on_line.iter().filter(|c| matches!(c, Command::SetCourse { .. })).collect();
        assert_eq!(runs.len(), 1, "{on_line:?}");
        assert!(matches!(runs[0], Command::SetCourse { waypoints, speed_kn, .. } if waypoints[0] == b && *speed_kn == Some(MINE_RUN_KN)));
    }

    /// The pilot leaves by Exit-kind markers, at cruise: a convoy lane's end is not a way out.
    #[test]
    fn the_pilot_leaves_by_exit_markers_at_cruise() {
        let d = data();
        let c = template(MissionType::AgentExtraction, &d);
        let mut p = Pilot::new(&c, &SETUP, &d);
        let kind_at = |w: DVec2| c.map.markers.iter().find(|m| DVec2::from(m.pos_m) == w).map(|m| m.kind);
        assert!(!p.exits.is_empty() && p.exits.iter().all(|e| kind_at(*e) == Some(MarkerKind::Exit)), "{:?}", p.exits);
        assert!(c.map.markers.iter().any(|m| m.kind == MarkerKind::ConvoyExit), "the map has a lane end to leave alone");
        // The extraction done: a course for the exit at cruise, not at 95% of top speed.
        let (landing, cruise) = (p.objective_pos, p.hulls[0].cruise_kn);
        let leaving = steer_for(&mut p, 3, &[(StationAction::InsertOrExtract, None)], |_, _| snapshot(landing, Vec::new(), Vec::new()));
        let exit = p.exits[0];
        assert!(leaving.iter().any(|c| matches!(c, Command::SetCourse { waypoints, speed_kn, .. } if waypoints[0] == exit && *speed_kn == Some(cruise))), "{leaving:?}");
    }

    /// A cutting-out's route ends where the prize lies; every boarded hull is ordered home once, when its
    /// boarding finishes (a finished action is in the finished list, never in the snapshot); then the hull
    /// makes for the point abeam of the exit, off the prize's way.
    #[test]
    fn a_boarded_prize_is_ordered_home_and_the_route_ends_at_it() {
        let d = data();
        let c = template(MissionType::CuttingOut, &d);
        let mut p = Pilot::new(&c, &SETUP, &d);
        let prize = c.vessels.iter().find(|v| v.role == VesselRole::Prize).expect("a prize");
        let berth = DVec2::new(prize.pos.x, prize.pos.z);
        assert_eq!(p.objective_pos, berth);
        let short = p.hulls[0].route[0];
        assert!((short.distance(berth) - BOARD_STANDOFF_M).abs() < 1e-6 && (short - berth).normalize().dot((p.hulls[0].spawn - berth).normalize()) > 0.999_999, "the route ends a stand-off short of the prize, on the approach");
        let first_objective_marker = c.map.markers.iter().find(|m| matches!(m.kind, MarkerKind::Objective | MarkerKind::DepotSite | MarkerKind::RadarSite | MarkerKind::Berth)).map(|m| DVec2::from(m.pos_m)).unwrap();
        assert!(first_objective_marker.distance(berth) > 1_000.0, "this map tells the two rules apart");
        // The prize tracked and not yet disabled: the hull closes to the stand-off and no nearer.
        let class = p.target_classes.iter().next().cloned();
        // (At the route's end with the prize 4 km off, as when it has moved: a course to 1,000 m short of it.)
        let away = berth + (berth - short).normalize() * 3_000.0;
        let closing = steer_for(&mut p, 40, &[], |_, tick| snapshot(short, vec![track(4, Stance::Suspect, TrackGrade::Fine, away, DVec2::ZERO, class.clone(), tick)], Vec::new()));
        let berth_was = berth;
        let berth = away;
        let nearest = closing.iter().filter_map(|c| match c { Command::SetCourse { waypoints, .. } => waypoints.iter().map(|w| w.distance(berth)).reduce(f64::min), Command::HoldStation { at, .. } => Some(at.distance(berth)), _ => None }).fold(f64::INFINITY, f64::min);
        assert!((nearest - BOARD_STANDOFF_M).abs() < 1.0, "{closing:?}");
        let berth = berth_was;
        // Nothing finished: no decision. The boarding finished: one decision for that hull, then the way out.
        let none = steer_for(&mut p, 5, &[], |_, _| snapshot(berth, Vec::new(), Vec::new()));
        assert!(none.iter().all(|c| !matches!(c, Command::Prize { .. })));
        let taken = EntityId::from_u64(42);
        let done = [(StationAction::BoardAndCapture, Some(taken))];
        let after = steer_for(&mut p, 5, &done, |_, _| snapshot(berth, Vec::new(), Vec::new()));
        let decisions: Vec<&Command> = after.iter().filter(|c| matches!(c, Command::Prize { .. })).collect();
        assert_eq!(decisions.len(), 1, "{after:?}");
        assert!(matches!(decisions[0], Command::Prize { prize, decision: PrizeDecision::SailHome } if *prize == taken));
        let exit = p.named_exit.expect("a cutting-out names its exit");
        let abeam = p.flank(0, exit);
        assert!((abeam.distance(exit) - ESCORT_FLANK_M).abs() < 1e-6 && (abeam - exit).dot(exit - berth).abs() < 1e-3);
        assert!(after.iter().any(|c| matches!(c, Command::SetCourse { waypoints, .. } if waypoints[0] == abeam)), "{after:?}");
        // A disabled escort beside the disabled prize: the boarding is started on the prize, the farther one.
        let mut p = Pilot::new(&c, &SETUP, &d);
        let other = ClassId::new("class.not_the_prize");
        let disabled = |e: u64, at: DVec2, t: u32| { let mut v = snapshot(at, Vec::new(), Vec::new()).vessels.remove(0); v.entity = EntityId::from_u64(e); v.status = VesselStatus::Disabled; v.track = TrackId(t); v };
        let boarding = steer_for(&mut p, 3, &[], |_, tick| {
            let mut s = snapshot(short, vec![track(11, Stance::Hostile, TrackGrade::Fine, short + DVec2::new(200.0, 0.0), DVec2::ZERO, Some(other.clone()), tick), track(12, Stance::Suspect, TrackGrade::Fine, berth, DVec2::ZERO, class.clone(), tick)], Vec::new());
            s.vessels.push(disabled(11, short + DVec2::new(200.0, 0.0), 11));
            s.vessels.push(disabled(12, berth, 12));
            s
        });
        let boarded: Vec<u32> = boarding.iter().filter_map(|c| match c { Command::StartAction { action: StationAction::BoardAndCapture, target: ActionTarget::Track(t) } => Some(t.0), _ => None }).collect();
        assert_eq!(boarded, vec![12], "{boarding:?}");
        // A disabled hull of no known class is not boarded either, nor a Neutral one of the prize's class (a
        // decoy).
        let mut p = Pilot::new(&c, &SETUP, &d);
        let unknown = steer_for(&mut p, 3, &[], |_, tick| {
            let mut s = snapshot(short, vec![track(13, Stance::Suspect, TrackGrade::Fine, short + DVec2::new(200.0, 0.0), DVec2::ZERO, None, tick), track(14, Stance::Neutral, TrackGrade::Fine, short + DVec2::new(-200.0, 0.0), DVec2::ZERO, class.clone(), tick)], Vec::new());
            s.vessels.push(disabled(13, short + DVec2::new(200.0, 0.0), 13));
            s.vessels.push(disabled(14, short + DVec2::new(-200.0, 0.0), 14));
            s
        });
        assert!(unknown.iter().all(|c| !matches!(c, Command::StartAction { .. })), "{unknown:?}");
        // The pure rule: every finished boarding once, in order, less the hulls already ordered home.
        let (e1, e2, e3) = (EntityId::from_u64(1), EntityId::from_u64(2), EntityId::from_u64(3));
        let finished = [(StationAction::BoardAndCapture, Some(e1)), (StationAction::Photograph, Some(e2)), (StationAction::BoardAndCapture, Some(e3)), (StationAction::BoardAndCapture, Some(e1)), (StationAction::BoardAndCapture, None)];
        assert_eq!(prizes_due(&finished, &[e3].into_iter().collect()), vec![e1]);
        assert_eq!(prizes_due(&finished, &BTreeSet::new()), vec![e1, e3]);
    }

    /// An escort meets its charge from abeam and leaves on a parallel track: it never sails or waits on the
    /// leg from the charge to the exit.
    #[test]
    fn an_escort_keeps_off_its_charges_way_out() {
        let d = data();
        let c = template(MissionType::DefectorEscort, &d);
        let mut p = Pilot::new(&c, &SETUP, &d);
        let (charge, exit, spawn) = (p.objective_pos, p.named_exit.expect("an escort names its exit"), p.hulls[0].spawn);
        let leg = (exit - charge).normalize();
        let off_leg = |w: DVec2| (w - charge).perp_dot(leg).abs();
        let abeam = p.hulls[0].route[0];
        assert_eq!(p.hulls[0].route.len(), 1);
        assert!((off_leg(abeam) - ESCORT_FLANK_M).abs() < 1e-6 && (abeam - charge).dot(leg).abs() < 1e-6, "the first waypoint is abeam of the charge");
        assert!((spawn - charge).perp_dot(leg) * (abeam - charge).perp_dot(leg) >= 0.0, "on the side the hull comes in from");
        // The charge in sight 3 km off: a course abeam of it. Met: a course abeam of the exit.
        let closing = steer_for(&mut p, 2, &[], |_, tick| snapshot(charge + (spawn - charge).normalize() * 3_000.0, vec![track(2, Stance::Neutral, TrackGrade::Fine, charge, DVec2::ZERO, None, tick)], Vec::new()));
        assert!(closing.iter().any(|c| matches!(c, Command::SetCourse { waypoints, .. } if waypoints[0].distance(abeam) < 1e-6)), "{closing:?}");
        let met = steer_for(&mut p, 2, &[], |_, tick| snapshot(abeam, vec![track(2, Stance::Neutral, TrackGrade::Fine, charge, DVec2::ZERO, None, tick)], Vec::new()));
        let out = p.flank(0, exit);
        assert!(met.iter().any(|c| matches!(c, Command::SetCourse { waypoints, .. } if waypoints[0] == out)), "{met:?}");
        assert!((off_leg(out) - ESCORT_FLANK_M).abs() < 1e-6);
        for cmd in closing.iter().chain(&met) {
            if let Command::SetCourse { waypoints, .. } = cmd { assert!(waypoints.iter().all(|w| off_leg(*w) > ESCORT_FLANK_M - 1.0), "{cmd:?}"); }
        }
    }

    /// The camera counts a photograph from the finished list, gives up a target that will not close, and
    /// takes a contact inside 1.5 stand-offs before a farther one of the objective's class.
    #[test]
    fn the_camera_moves_on_from_a_finished_or_a_stalled_target() {
        let d = data();
        let c = template(MissionType::PhotoReconnaissance, &d);
        let mut p = Pilot::new(&c, &SETUP, &d);
        let class = p.target_classes.iter().next().cloned().expect("the targets have a class");
        let here = p.objective_pos;
        let (near, far) = (here + DVec2::new(1.2 * p.photo_standoff_m, 0.0), here + DVec2::new(0.0, 3.0 * p.photo_standoff_m));
        let picture = |tick: u64| vec![track(5, Stance::Hostile, TrackGrade::Fine, far, DVec2::ZERO, Some(class.clone()), tick), track(6, Stance::Suspect, TrackGrade::Fine, near, DVec2::ZERO, None, tick)];
        let started = |cmds: &[Command]| cmds.iter().filter_map(|c| match c { Command::StartAction { action: StationAction::Photograph, target: ActionTarget::Track(t) } => Some(t.0), _ => None }).collect::<Vec<_>>();
        let first = steer_for(&mut p, 1, &[], |_, tick| snapshot(here, picture(tick), Vec::new()));
        assert_eq!(started(&first), vec![6], "the contact inside 1.5 stand-offs first");
        // Its photograph finishes: the other target is next, and the first is not taken again.
        let done = [(StationAction::Photograph, Some(EntityId::from_u64(60)))];
        let next = steer_for(&mut p, 40, &done, |_, tick| snapshot(here, picture(tick), Vec::new()));
        assert!(started(&next).iter().all(|t| *t == 5) && !started(&next).is_empty(), "{:?}", started(&next));
        // The second target never closes: given up after a minute, and the pilot holds no target then.
        let stalled = steer_for(&mut p, PHOTO_STALL_PERIODS + 5, &done, |_, tick| snapshot(here, picture(tick), Vec::new()));
        assert!(p.hulls[0].photo_skipped.contains_key(&TrackId(5)) && p.hulls[0].photo_target.is_none(), "{stalled:?}");
        assert_eq!(sensor_orders(&first) + sensor_orders(&next) + sensor_orders(&stalled), 0);
        // The pure rule: no 100 m of closing in 60 periods is a stall; closing resets the count.
        assert_eq!(photo_stalled(None, 5_000.0, 10), (false, Some((5_000.0, 10))));
        assert_eq!(photo_stalled(Some((5_000.0, 10)), 4_950.0, 69), (false, Some((5_000.0, 10))));
        assert_eq!(photo_stalled(Some((5_000.0, 10)), 4_950.0, 70), (true, Some((5_000.0, 10))));
        assert_eq!(photo_stalled(Some((5_000.0, 10)), 4_800.0, 70), (false, Some((4_800.0, 70))));
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
