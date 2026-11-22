//! Unit `sys-ai-unit`. Vessel captains: one behaviour tree per AI hull. DESIGN.md L761 (a behaviour tree on
//! top of the same sensor and fire-control pipeline the player's ship uses, at the difficulty's accuracy and
//! reaction time), L537 (strikes colours under the Combine surrender rule, see [`surrender_due`]),
//! L834 and L1035 (officers keep running a dropped player's hull).
//!
//! A captain never steers or aims directly. It sets `ShipOrders` (route, speed, station, designated track,
//! weapons tight, evasion, engagement range), the L0 preset and the emission setting; the ship's officers do
//! the rest exactly as for a player. Reaction time delays what the captain perceives: a change in the side
//! picture becomes actionable `ai_reaction_s` after it happened. Accuracy is the ship's
//! `Proficiency::aim_error_mult`.
//!
//! What a captain knows: its own hull (pose, damage, weapons, design manoeuvring numbers), own-side hulls it
//! follows or screens, the terrain, its group directive, and the tracks of its side's picture once each change
//! has waited out the reaction time. It never reads `Track::truth` or any world state of another side, so
//! decoys, emission control and dark running fool it exactly as they would fool a player.
//!
//! The trees, in brief:
//! - `Combatant`: holds formation until a contact is perceived, then fights at its doctrine range: closes on
//!   an intercept course, turns broadside inside the range band (zigzagging the beam side), opens on a
//!   quarter course when the target gets inside the band. Investigates Suspect contacts to visual
//!   identification range. Withdraws, latched, when hurt.
//! - `Picket`: shadows any contact just outside its own weapon range, radar on, fires only in self-defence.
//! - `Striker`: closes silent to missile launch range, holds there until a salvo goes, then opens for 90 s
//!   before the next run.
//! - `Escort`: stations between the threat and its charge, engages only inside its gun range of the charge.
//! - `Merchant`: holds its course; flees from threats for 5 minutes after the last one.
//! - `Static`: holds its anchorage and fights anything in range.
//! - `Duelist`: a straightforward gun duel at 70% of gun range, never withdraws: a ship that closes inside that
//!   range is met on the beam, not run from.
//! - `Proxy`: keeps the player's or the briefing's orders, adding only evasion against inbound weapons.
//!
//! Doctrine flags read from `DoctrineMods::flags`: `close_in` (range x0.7), `stand_off` (range x1.3),
//! `aggressive` (Aggressive preset), `hold_ground` (never withdraw), `silent_approach` (Silent while closing).
use super::group::{Directive, GroupMode};
use super::DoctrineMods;
use crate::prelude::*;
use crate::ship::{FormationSlot, Proficiency, Ship, ShipOrders};
use crate::tracks::{SidePicture, Track};
use content::campaign::{ClassRole, SurrenderRule};
use content::graph::{TargetCategory, WeaponClass};
use content::part::PartFunction;
use sim_core::vessel::{Vessel, VesselStatus};

/// Behaviour tree selected by `EnemyClassDef::behaviour`. Unknown keys fall back to `Combatant`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Behaviour {
    #[default] Combatant,
    /// Patrol boat, picket: reports and shadows, fights only in self-defence range.
    Picket,
    /// Missile boat: closes to launch range, fires, opens.
    Striker,
    /// Escort: stays between the threat and its charge.
    Escort,
    /// Freighter, tanker, civilian: holds course, flees when attacked.
    Merchant,
    /// Site or anchored hull.
    Static,
    /// Splash patrol boat that fights back, and trial opponents.
    Duelist,
    /// Stock officers running a player hull (dropped player, prize sailing home, campaign bot).
    Proxy,
    /// Act 3 AI submarine; tree in `super::sub`.
    Submarine,
}
/// Map a behaviour key from data. Real code: the key set is contract for the data units.
pub fn behaviour_from_key(key: &str) -> Behaviour {
    match key { "picket" => Behaviour::Picket, "striker" => Behaviour::Striker, "escort" => Behaviour::Escort, "merchant" => Behaviour::Merchant, "static" => Behaviour::Static, "duelist" => Behaviour::Duelist, "proxy" => Behaviour::Proxy, "submarine" => Behaviour::Submarine, _ => Behaviour::Combatant }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum CaptainMode { #[default] Following, Investigating, Attacking, Opening, Evading, Fleeing, Withdrawing, Struck, Adrift }

/// A perception waiting out the reaction time.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Percept { pub at_tick: u64, pub track: TrackId, pub kind: PerceptKind }
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PerceptKind { NewContact, BecameHostile, Inbound, UnderFire, LostContact, LeaderLost }

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct CaptainState {
    pub behaviour: Behaviour,
    pub role: Option<ClassRole>,
    pub group: Option<GroupId>,
    pub mode: CaptainMode,
    pub directive: Directive,
    pub target: TrackId,
    pub pending: Vec<Percept>,
    pub next_decision_tick: u64,
    pub surrender: SurrenderRule,
    pub ace: Option<AceId>,
    /// Route the briefing or a bot policy gave a `Proxy` captain.
    pub route: Vec<DVec2>,
    /// What the captain has perceived and decided so far (unit `sys-ai-unit`). Build captains with
    /// [`new_captain`].
    #[serde(default)]
    pub(crate) mem: CaptainMemory,
}
impl CaptainState {
    /// True while the captain is breaking off (withdrawing or fleeing). `sys-ai` may OR this into
    /// `ace::AceFacts::withdrawing`.
    pub fn is_withdrawing(&self) -> bool { matches!(self.mode, CaptainMode::Withdrawing | CaptainMode::Fleeing) }
}

/// A track the captain has perceived (its `NewContact` matured), with the matured stance and inbound flags.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct Known { pub(crate) id: TrackId, pub(crate) hostile: bool, pub(crate) inbound: bool }

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct CaptainMemory {
    /// Perceived tracks, ascending id.
    pub(crate) known: Vec<Known>,
    pub(crate) last_decision_tick: u64,
    /// Tick of the latest matured under-fire percept.
    pub(crate) under_fire_tick: Option<u64>,
    pub(crate) withdraw_latched: bool,
    /// Formation leader the captain is following (changes wait out the reaction time).
    pub(crate) leader: Option<EntityId>,
    pub(crate) leader_init: bool,
    /// Broadside side: +1 steers the target bearing plus 90 degrees (target to port), -1 minus 90 (target to
    /// starboard); 0 unset.
    pub(crate) beam_sign: i8,
    pub(crate) next_zig_tick: u64,
    /// Static hulls: where they anchor.
    pub(crate) anchor: Option<DVec2>,
    /// Striker: start of the current attack run and the end of the opening leg after a salvo.
    pub(crate) attack_start_tick: u64,
    pub(crate) opening_until_tick: u64,
    /// Merchant: flee until this tick.
    pub(crate) flee_until_tick: u64,
    /// Compass bearing toward the last threat seen, for withdrawing after contact is lost.
    pub(crate) threat_bearing: Option<f64>,
    /// Surrender rule: the check tick since which a strike condition has held unbroken.
    pub(crate) strike_since: Option<u64>,
    /// Surrender rule: no check before this tick (the rule runs at a fixed one-second cadence).
    pub(crate) next_strike_check: u64,
    /// Collision avoidance took the speed of a plan that keeps its speed: (the ordered speed before, knots or
    /// cruise, and the speed avoidance wrote), so the next clear decision puts it back ([`steer_clear`]).
    #[serde(default)]
    pub(crate) avoid_speed: Option<(Option<f64>, f64)>,
}

/// Create a captain for a class (or a `Proxy` for a player hull). Unit: `sys-ai-unit`.
///
/// A `Combatant` key on a class whose role is plainly not a warship is refined: freighters, tankers,
/// tenders and civilians become `Merchant`, pickets become `Picket`.
pub fn new_captain(behaviour: Behaviour, role: Option<ClassRole>, group: Option<GroupId>, surrender: SurrenderRule) -> CaptainState {
    let behaviour = match (behaviour, role) {
        (Behaviour::Combatant, Some(ClassRole::Freighter | ClassRole::Tanker | ClassRole::Tender | ClassRole::Civilian)) => Behaviour::Merchant,
        (Behaviour::Combatant, Some(ClassRole::Picket)) => Behaviour::Picket,
        (Behaviour::Combatant, Some(ClassRole::Submarine)) => Behaviour::Submarine,
        (b, _) => b,
    };
    // Player hulls never strike (L537); a malformed fraction never strikes either. `Combine` resolves to the
    // campaign's spec value here; `Sim` passes the sortie campaign's own value as `Strike`.
    let surrender = match surrender.cells_lost(content::campaign::CampaignRules::default().strike_cells_lost) {
        Some(cells_lost) if behaviour != Behaviour::Proxy => SurrenderRule::Strike { cells_lost },
        _ => SurrenderRule::Never,
    };
    CaptainState { behaviour, role, group, surrender, ..Default::default() }
}

/// What a captain decided.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CaptainOut { pub orders: Option<ShipOrders>, pub preset: Option<DoctrinePreset>, pub emcon: Option<Emcon>, pub strike: bool }

/// One captain decision (called when `tick >= next_decision_tick`): absorb percepts older than the reaction
/// time, run the behaviour tree against the side picture and the group directive, and produce orders.
/// Draws use `rng::Stream::AiDecision` keyed (vessel, decision code). Unit: `sys-ai-unit`.
///
/// Fields of `CaptainOut` are `Some` only when they change the ship's current orders or settings, so pending
/// launches and decoys in `ShipOrders` survive and the Helm does not replan for nothing. The next decision is
/// scheduled half a reaction time ahead (0.25 to 1 s, jittered), or sooner when a pending percept matures.
pub fn decide(captain: &mut CaptainState, ship: &Ship, world: &World, picture: &SidePicture, mods: &DoctrineMods, proficiency: Proficiency, followers: &[EntityId], ctx: &SimCtx<'_>) -> CaptainOut {
    let now = ctx.tick.0;
    let reaction = reaction_ticks(captain.behaviour, proficiency, ctx);
    let draw = |code: u64| sim_core::rng::uniform(ctx.key(sim_core::rng::Stream::AiDecision, ship.vessel.to_u64(), code));
    scan_picture(captain, picture, now);
    absorb(captain, picture, now, reaction);

    if captain.behaviour == Behaviour::Submarine {
        let out = super::sub::decide(captain, ship, world, picture, mods, ctx);
        finish_decision(captain, now, reaction, draw(1));
        return out;
    }

    let Some(vessel) = world.vessel(ship.vessel) else {
        captain.mode = CaptainMode::Adrift;
        finish_decision(captain, now, reaction, draw(1));
        return CaptainOut::default();
    };
    let own = own_state(ship, vessel, world, captain, ctx);
    let (threats, inbound) = perceived(captain, picture, &own, now);
    let draws = Draws { side: draw(2), zig: draw(3) };
    let terrain = world.terrain();
    let depth = |p: DVec2| terrain.depth_at(p.x, p.y);
    let sit = Situation { own: &own, threats: &threats, inbound: &inbound, mods, now, draws, depth: &depth };
    let mut plan = think(captain, &sit);
    let near = near_hulls(&plan, vessel, world, picture, &own, followers);
    steer_clear(captain, &mut plan, &vessel.kin, &near, &own, ship.orders.speed_kn, cruise_kn(ship, &own), world.terrain());
    let out = build_out(&plan, &ship.orders, ship.settings.emcon, ship.settings.preset, &own, &depth);
    finish_decision(captain, now, reaction, draw(1));
    out
}

/// Hulls nearer than this are checked for a collision course, metres.
const AVOID_LOOK_M: f64 = 4_000.0;

/// Collision avoidance (L766: it adjusts heading and speed only, never the path). When an own-side hull or a
/// hull in the side picture will pass inside the safe distance in the next 90 s, or is already inside the
/// separation radius, this decision steers the flocking rules' heading and speed (`group::flock_adjust`)
/// instead of the plan's course; the next decision with the way clear returns to the plan's route. Convoys in
/// column and scattering hulls on one route ran into each other dozens of times a sortie before this existed.
/// Own-side hulls come from the world (a side knows its own ships); others only from the side picture.
/// Only the give-way hull acts (DECISION, q5, after the steering rules at sea): a hull keeps course for a
/// hull more than 112.5 degrees abaft its beam (the overtaking hull keeps clear) and for a hull keeping
/// station on it. When both acted, a convoy lead and the escort stationed ahead of it swerved off each other
/// for twenty minutes and the convoy never left its entry point. Unit: q5 fix.
/// Hulls a captain checks for a collision course: own-side hulls within [`AVOID_LOOK_M`] (not its followers)
/// and the side picture's ranged surface contacts, as kinematics. Empty when the plan holds a station or stops,
/// or the hull cannot steer.
fn near_hulls(plan: &Plan, vessel: &Vessel, world: &World, picture: &SidePicture, own: &OwnState, followers: &[EntityId]) -> Vec<Kinematics> {
    let mut near: Vec<Kinematics> = Vec::new();
    if matches!(plan.course, Course::Stop | Course::Station(..)) || own.status != OwnStatus::Active { return near; }
    let p = xz(vessel.kin.pose.pos);
    for (_, v) in world.vessels() {
        if v.id == vessel.id || v.side != vessel.side || followers.contains(&v.id) || !matches!(v.status, VesselStatus::Active | VesselStatus::Disabled) { continue; }
        if xz(v.kin.pose.pos).distance(p) <= AVOID_LOOK_M { near.push(v.kin); }
    }
    for t in picture.tracks.iter().filter(|t| t.is_live() && t.domain == Domain::Surface && t.stance != Stance::Friend && t.bearing_only.is_none()) {
        if !finite2(xz(t.pos)) || xz(t.pos).distance(p) > AVOID_LOOK_M { continue; }
        let h = if xz(t.vel).length() > 0.5 { bearing(DVec2::ZERO, xz(t.vel)) } else { 0.0 };
        near.push(Kinematics { pose: Pose::from_heading(DVec3::new(t.pos.x, 0.0, t.pos.z), h), lin_vel: DVec3::new(t.vel.x, 0.0, t.vel.z), ang_vel: DVec3::ZERO });
    }
    near
}

/// The hull's cruise speed when no speed is ordered, knots: the Helm's `helm.cruise` slider share of top speed
/// (0.7 when unset, as `officers::helm`).
fn cruise_kn(ship: &Ship, own: &OwnState) -> f64 {
    let s = &ship.settings.sliders;
    let f = s.get(crate::officers::helm::SLIDER_CRUISE).or_else(|| s.get("helm.cruise_speed")).copied().filter(|f| f.is_finite() && *f > 0.0 && *f <= 1.0).unwrap_or(0.7);
    own.top_speed_mps * f / KN
}

/// Collision avoidance around one decision, keeping the plan's speed honest. A plan that keeps its speed
/// (`Speed::Keep`: the proxy captain of a prize, a dropped player's hull or a bot keeps the speed its owner
/// ordered) gets back the speed it had before avoidance took it, once the way is clear, unless someone else
/// ordered a speed since (the orders no longer hold what avoidance wrote).
///
/// DECISION, s2: avoidance scales the speed the plan asks for (the ordered speed for `Keep`, the cruise speed
/// for `Cruise`), never the speed over ground. Scaling the speed over ground ratcheted down decision by
/// decision, and a `Keep` plan never wrote a speed again: a prize taken and ordered home in cutting_out
/// (`BP_DROP=all`) kept 1.02 kn in seed 1 (it never got home), 0.19 kn in bursars_berth seed 2 and 8.13 kn in
/// cutting_out seed 3, whatever it had when the boarding hull last lay alongside.
fn steer_clear(c: &mut CaptainState, plan: &mut Plan, me: &Kinematics, near: &[Kinematics], own: &OwnState, ordered_kn: Option<f64>, cruise_kn: f64, terrain: &sim_core::terrain::Terrain) {
    let keeps = plan.speed == Speed::Keep;
    let mut before = ordered_kn;
    match c.mem.avoid_speed.take() {
        Some((was, wrote)) if keeps && ordered_kn == Some(wrote) => { before = was; plan.speed = was.map_or(Speed::Cruise, Speed::Knots); }
        _ => {}
    }
    let Some(kn) = avoid_collisions(plan, me, near, own, before.unwrap_or(cruise_kn), cruise_kn, terrain) else { return };
    if keeps { c.mem.avoid_speed = Some((before, kn)); }
}

/// The geometry of collision avoidance: when a hull in `near` will pass inside the safe distance in the next
/// 90 s, or is already inside the separation radius, steer the flocking rules' heading and speed instead of the
/// plan's course. `kept_kn` is what a `Keep` speed means (the ordered speed), `cruise_kn` what `Cruise` means.
/// Returns the speed written, knots, when it acted.
fn avoid_collisions(plan: &mut Plan, me: &Kinematics, near: &[Kinematics], own: &OwnState, kept_kn: f64, cruise_kn: f64, terrain: &sim_core::terrain::Terrain) -> Option<f64> {
    if matches!(plan.course, Course::Stop | Course::Station(..)) || own.status != OwnStatus::Active { return None; }
    let p = xz(me.pose.pos);
    // Hulls abaft the beam by more than 112.5 degrees are overtaking: they keep clear.
    let fwd = dir(own.heading);
    let near: Vec<Kinematics> = near.iter().copied().filter(|n| { let d = xz(n.pose.pos) - p; d.length() < 1.0 || d.normalize().dot(fwd) > libm::cos(deg(112.5)) }).collect();
    if near.is_empty() { return None; }
    // The same radii as the flocking rules.
    let r = if own.turn_radius_m.is_finite() && own.turn_radius_m >= 10.0 { own.turn_radius_m.min(5_000.0) } else { 200.0 };
    let r_sep = (0.75 * r).clamp(80.0, 500.0);
    let d_safe = 1.5 * r_sep;
    let v = xz(me.lin_vel);
    let risk = near.iter().any(|n| {
        let rel = xz(n.pose.pos) - p;
        if rel.length() < r_sep { return true; }
        let w = xz(n.lin_vel) - v;
        let w2 = w.length_squared();
        if w2 < 1e-6 { return false; }
        let tcpa = -rel.dot(w) / w2;
        tcpa > 0.0 && tcpa < 90.0 && (rel + w * tcpa).length() < d_safe
    });
    if !risk { return None; }
    let (dh, sf) = super::group::flock_adjust(me, p, &near, own.turn_radius_m, terrain);
    let ordered_kn = match plan.speed {
        Speed::Knots(k) => k,
        Speed::Fraction(f) => own.top_speed_mps * f / KN,
        Speed::Flank => own.top_speed_mps / KN,
        Speed::Keep => kept_kn,
        Speed::Cruise => cruise_kn,
    };
    plan.course = Course::Heading(wrap_tau(own.heading + dh));
    if !ordered_kn.is_finite() { return None; }
    let kn = (ordered_kn * sf.clamp(0.2, 1.2)).max(0.0);
    plan.speed = Speed::Knots(kn);
    Some(kn)
}

/// Feed a captain the picture changes of this tick as percepts (new contact, stance change, inbound weapon,
/// hits on own hull). Unit: `sys-ai-unit`.
///
/// Stance and inbound changes have no `TrackChange`; `decide` finds them by comparing the picture with what
/// the captain already perceived. Merges and splits carry what was perceived over at once (the plot keeps the
/// symbol); new contacts and losses of contact wait out the reaction time.
pub fn perceive(captain: &mut CaptainState, changes: &[crate::tracks::TrackChange], under_fire: bool, tick: u64) {
    use crate::tracks::TrackChange as C;
    for ch in changes {
        match *ch {
            C::Created(id) => push_percept(captain, Percept { at_tick: tick, track: id, kind: PerceptKind::NewContact }),
            C::Dropped(id) | C::Expired(id) => push_percept(captain, Percept { at_tick: tick, track: id, kind: PerceptKind::LostContact }),
            C::Merged { from, into } => {
                if let Some(k) = known(captain, from).copied() {
                    remove_known(captain, from);
                    if known(captain, into).is_none() { insert_known(captain, Known { id: into, ..k }); }
                }
                for p in &mut captain.pending { if p.track == from { p.track = into; } }
                if captain.target == from { captain.target = into; }
            }
            C::Split { from, new } => {
                if let Some(k) = known(captain, from).copied() { if known(captain, new).is_none() { insert_known(captain, Known { id: new, ..k }); } }
            }
            C::Grade(..) => {}
        }
    }
    if under_fire && !captain.pending.iter().any(|p| p.kind == PerceptKind::UnderFire && tick.saturating_sub(p.at_tick) < TICKS_PER_S) {
        push_percept(captain, Percept { at_tick: tick, track: TrackId::NONE, kind: PerceptKind::UnderFire });
    }
}

// ---------------------------------------------------------------------------------------------------------
// Constants (the spec fixes reaction time and accuracy only; these are this unit's play choices)
// ---------------------------------------------------------------------------------------------------------

const TICKS_PER_S: u64 = sim_core::time::TICK_HZ as u64;
const KN: f64 = sim_core::time::KN;
const MAX_PENDING: usize = 256;
/// An under-fire percept keeps the captain "under fire" this long, s.
const UNDER_FIRE_S: u64 = 30;
/// Range band around the doctrine range: the larger of this fraction and the floor, metres.
const BAND_FRACTION: f64 = 0.12;
const BAND_MIN_M: f64 = 400.0;
/// Beam leg length before the broadside side flips: 60 s plus up to 60 s drawn.
const ZIG_MIN_S: f64 = 60.0;
const ZIG_SPREAD_S: f64 = 60.0;
/// Striker opening leg after a salvo, s.
const STRIKER_OPEN_S: u64 = 90;
/// Merchant keeps fleeing this long after the last threat, s.
const FLEE_S: u64 = 300;
/// Merchant treats a Hostile contact inside this range as a threat, metres.
const MERCHANT_THREAT_M: f64 = 10_000.0;
/// Fraction of the visual reference range at which a Combatant investigates a Suspect contact.
const INVESTIGATE_FRACTION: f64 = 0.4;
/// Clear water needed under the keel beyond the draft when picking a heading, metres (as the clearance mask).
const KEEL_CLEARANCE_M: f64 = 2.0;

// ---------------------------------------------------------------------------------------------------------
// Hull health (shared with `ace::facts`)
// ---------------------------------------------------------------------------------------------------------

/// Damage summary of a hull: lost cell fraction, weapons and propulsion parts working.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct HullHealth { pub(crate) cells_lost: f64, pub(crate) weapons_total: u16, pub(crate) weapons_working: u16, pub(crate) propulsion_total: u16, pub(crate) propulsion_working: u16 }

/// What the surrender rule reads besides the hull's health, from the hull itself.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct StrikeFacts {
    /// Some weapon (not point defence) can fire: not destroyed, powered, and with rounds.
    pub(crate) weapon_works: bool,
    /// A compartment burns or holds water.
    pub(crate) burning_or_flooding: bool,
}
impl Default for StrikeFacts { fn default() -> Self { StrikeFacts { weapon_works: true, burning_or_flooding: false } } }

/// The surrender rule runs once a second.
const STRIKE_CHECK_TICKS: u64 = TICKS_PER_S;
/// A strike condition must hold this long, s.
const STRIKE_CONFIRM_S: u64 = 5;
/// The hull must be under fire, or have been hit, within this many seconds.
const STRIKE_FIRE_WINDOW_S: u64 = 60;
/// A compartment with more water than this is flooding, m^3.
const STRIKE_FLOOD_M3: f64 = 1.0;

/// The Combine surrender rule (L537; ruling 1 of 20 November 2026). Unit: `sys-ai-unit`.
///
/// A hull whose rule strikes (`SurrenderRule::Strike`, never a player hull, a proxy or an ace) strikes its colours
/// when, while it is under fire or was hit in the last 60 s, one of these holds at every once-a-second check for
/// 5 s: (a) it has lost the rule's fraction of its original cells; (b) none of its weapons works (an unarmed hull
/// has none) and a hostile can reach it; (c) it cannot make way and is burning or flooding (it is Disabled already,
/// and this only shows Struck). DECISION: the captain never reads truth (module doc), so "a hostile inside that
/// hostile's weapon range" is read from the picture: a perceived Hostile contact that has fired on this side, or one
/// inside the own self-defence range. Returns true on the check that strikes.
pub(crate) fn surrender_due(c: &mut CaptainState, own: &OwnState, threats: &[Contact], now: u64) -> bool {
    let SurrenderRule::Strike { cells_lost } = c.surrender else { return false };
    if own.is_player || c.behaviour == Behaviour::Proxy || c.ace.is_some() || own.status == OwnStatus::Lost { return false; }
    if now < c.mem.next_strike_check { return false; }
    c.mem.next_strike_check = (now / STRIKE_CHECK_TICKS + 1) * STRIKE_CHECK_TICKS;
    let fired_on = c.mem.under_fire_tick.is_some_and(|t| now.saturating_sub(t) <= STRIKE_FIRE_WINDOW_S * TICKS_PER_S);
    let reachable = threats.iter().any(|t| t.hostile && (t.fired_on_us || t.range <= own.self_defence_m));
    let a = own.health.cells_lost >= cells_lost;
    let b = !own.strike.weapon_works && reachable;
    let cc = own.status == OwnStatus::Disabled && own.strike.burning_or_flooding;
    if !(fired_on && (a || b || cc)) { c.mem.strike_since = None; return false; }
    let since = *c.mem.strike_since.get_or_insert(now);
    now.saturating_sub(since) >= STRIKE_CONFIRM_S * TICKS_PER_S
}

pub(crate) fn hull_health(vessel: &Vessel, catalogue: &Catalogue) -> HullHealth {
    let cl = vessel.cells_lost_fraction();
    let mut h = HullHealth { cells_lost: if cl.is_finite() { cl.clamp(0.0, 1.0) } else { 0.0 }, ..Default::default() };
    for p in &vessel.parts {
        let Some((_, def)) = catalogue.parts.get_index(p.def.index()) else { continue };
        let working = p.level != DamageLevel::Destroyed && p.health > 0.05;
        match def.function {
            PartFunction::Gun(_) | PartFunction::Launcher(_) => { h.weapons_total += 1; if working { h.weapons_working += 1; } }
            PartFunction::Engine(_) | PartFunction::Propulsor(_) => { h.propulsion_total += 1; if working { h.propulsion_working += 1; } }
            _ => {}
        }
    }
    h
}

/// Surrender rule b's "a weapon left that works": a mount that is not point defence and not a decoy launcher
/// can still fire (DECISION, r3: a decoy launcher is listed in `ShipDesign::mounts` as `WeaponClass::Decoy`
/// and holds no ammunition gate, so counting it kept every decoy-carrying hull from ever striking by rule b).
pub(crate) fn strike_weapon_works(mounts: &[crate::doctrine::MountInfo], parts: &[sim_core::parts::PartRuntime]) -> bool {
    mounts.iter().filter(|m| !m.point_defence && m.weapon != WeaponClass::Decoy).any(|m| crate::engage::weapon_can_fire(parts.get(m.slot.index()), parts))
}

/// The hurt rule: a hull breaks off when it has lost its behaviour's share of cells, every weapon, or more
/// than half its propulsion. Aces hold on to 30% whatever their tree. Merchants flee instead; sites, duelists
/// and proxies never withdraw.
pub(crate) fn wants_withdraw(h: &HullHealth, behaviour: Behaviour, ace: bool) -> bool {
    let threshold = match behaviour {
        Behaviour::Merchant | Behaviour::Static | Behaviour::Duelist | Behaviour::Proxy => return false,
        _ if ace => 0.30,
        Behaviour::Picket => 0.12,
        Behaviour::Striker | Behaviour::Submarine => 0.15,
        Behaviour::Combatant => 0.25,
        Behaviour::Escort => 0.30,
    };
    h.cells_lost >= threshold || (h.weapons_total > 0 && h.weapons_working == 0) || (h.propulsion_total > 0 && h.propulsion_working * 2 < h.propulsion_total)
}

// ---------------------------------------------------------------------------------------------------------
// Perception
// ---------------------------------------------------------------------------------------------------------

fn known(c: &CaptainState, id: TrackId) -> Option<&Known> { c.mem.known.binary_search_by(|k| k.id.cmp(&id)).ok().map(|i| &c.mem.known[i]) }
fn known_mut(c: &mut CaptainState, id: TrackId) -> Option<&mut Known> { match c.mem.known.binary_search_by(|k| k.id.cmp(&id)) { Ok(i) => Some(&mut c.mem.known[i]), Err(_) => None } }
fn insert_known(c: &mut CaptainState, k: Known) { if let Err(i) = c.mem.known.binary_search_by(|x| x.id.cmp(&k.id)) { c.mem.known.insert(i, k); } }
fn remove_known(c: &mut CaptainState, id: TrackId) { if let Ok(i) = c.mem.known.binary_search_by(|k| k.id.cmp(&id)) { c.mem.known.remove(i); } }
fn has_pending(c: &CaptainState, id: TrackId, kind: PerceptKind) -> bool { c.pending.iter().any(|p| p.track == id && p.kind == kind) }

fn push_percept(c: &mut CaptainState, p: Percept) {
    if c.pending.len() >= MAX_PENDING {
        // Oldest first out; anything lost is found again by the next picture scan.
        c.pending.remove(0);
    }
    c.pending.push(p);
}

/// Turn picture differences into percepts: tracks the captain has not perceived (stamped with their creation
/// tick), stance changes to Hostile and new inbound flags (stamped midway since the last look).
fn scan_picture(c: &mut CaptainState, picture: &SidePicture, now: u64) {
    let seen_at = c.mem.last_decision_tick.min(now) / 2 + now / 2;
    for t in &picture.tracks {
        if !t.is_live() || t.stance == Stance::Friend { continue; }
        match known(c, t.id).copied() {
            None => {
                if !has_pending(c, t.id, PerceptKind::NewContact) { push_percept(c, Percept { at_tick: t.created_tick.min(now), track: t.id, kind: PerceptKind::NewContact }); }
            }
            Some(k) => {
                let hostile = t.stance == Stance::Hostile;
                if hostile && !k.hostile && !has_pending(c, t.id, PerceptKind::BecameHostile) { push_percept(c, Percept { at_tick: seen_at, track: t.id, kind: PerceptKind::BecameHostile }); }
                if !hostile && k.hostile { if let Some(m) = known_mut(c, t.id) { m.hostile = false; } }
                if t.inbound && !k.inbound && !has_pending(c, t.id, PerceptKind::Inbound) { push_percept(c, Percept { at_tick: seen_at, track: t.id, kind: PerceptKind::Inbound }); }
                if !t.inbound && k.inbound { if let Some(m) = known_mut(c, t.id) { m.inbound = false; } }
            }
        }
    }
    if c.directive.leader != c.mem.leader {
        if !c.mem.leader_init { c.mem.leader = c.directive.leader; c.mem.leader_init = true; }
        else if !has_pending(c, TrackId::NONE, PerceptKind::LeaderLost) { push_percept(c, Percept { at_tick: now, track: TrackId::NONE, kind: PerceptKind::LeaderLost }); }
    }
    c.mem.leader_init = true;
}

/// Apply every percept that has waited out the reaction time; forget tracks gone from the picture.
fn absorb(c: &mut CaptainState, picture: &SidePicture, now: u64, reaction: u64) {
    let mut keep = Vec::with_capacity(c.pending.len());
    let pending = core::mem::take(&mut c.pending);
    for p in pending {
        if p.at_tick.saturating_add(reaction) > now { keep.push(p); continue; }
        match p.kind {
            PerceptKind::NewContact => if let Some(t) = picture.get(p.track).filter(|t| t.is_live()) {
                // A contact perceived late is perceived as it is now, stance and all, once its own changes
                // have matured too.
                let settled = t.created_tick.saturating_add(reaction) <= now;
                insert_known(c, Known { id: p.track, hostile: settled && t.stance == Stance::Hostile, inbound: settled && t.inbound });
            },
            PerceptKind::BecameHostile => if let Some(k) = known_mut(c, p.track) { k.hostile = true; },
            PerceptKind::Inbound => if let Some(k) = known_mut(c, p.track) { k.inbound = true; },
            PerceptKind::UnderFire => c.mem.under_fire_tick = Some(c.mem.under_fire_tick.map_or(p.at_tick, |t| t.max(p.at_tick))),
            PerceptKind::LostContact => remove_known(c, p.track),
            PerceptKind::LeaderLost => c.mem.leader = c.directive.leader,
        }
    }
    // Pending percepts about tracks that no longer exist are moot.
    keep.retain(|p| p.track.is_none() || picture.get(p.track).is_some_and(|t| t.is_live()));
    c.pending = keep;
    c.mem.known.retain(|k| picture.get(k.id).is_some_and(|t| t.is_live() && t.stance != Stance::Friend));
    if !c.target.is_none() && known(c, c.target).is_none() { c.target = TrackId::NONE; }
}

fn reaction_ticks(behaviour: Behaviour, prof: Proficiency, ctx: &SimCtx<'_>) -> u64 {
    let r = if prof.reaction_s.is_finite() && prof.reaction_s > 0.0 { prof.reaction_s } else if behaviour == Behaviour::Proxy { 0.0 } else { ctx.difficulty.clamped().ai_reaction_s };
    (r.clamp(0.0, 10.0) * TICKS_PER_S as f64).round() as u64
}

/// Schedule the next decision: half a reaction time ahead (0.25 to 1 s) jittered by +-15%, or when the
/// earliest pending percept matures if that is sooner; always at least one tick ahead.
fn finish_decision(c: &mut CaptainState, now: u64, reaction: u64, jitter: f64) {
    let base = (reaction as f64 * 0.5).clamp(0.25 * TICKS_PER_S as f64, TICKS_PER_S as f64);
    let j = if jitter.is_finite() { jitter.clamp(0.0, 1.0) } else { 0.5 };
    let mut next = now + ((base * (0.85 + 0.3 * j)).round() as u64).max(1);
    if let Some(m) = c.pending.iter().map(|p| p.at_tick.saturating_add(reaction)).min() { next = next.min(m.max(now + 1)); }
    c.next_decision_tick = next;
    c.mem.last_decision_tick = now;
}

// ---------------------------------------------------------------------------------------------------------
// Situation
// ---------------------------------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum OwnStatus { Active, Disabled, Lost }

/// What a captain knows about its own hull this decision.
#[derive(Clone, Copy, Debug)]
pub(crate) struct OwnState {
    pub(crate) vessel: EntityId,
    pub(crate) pos: DVec2,
    pub(crate) heading: f64,
    pub(crate) status: OwnStatus,
    pub(crate) health: HullHealth,
    pub(crate) top_speed_mps: f64,
    pub(crate) turn_radius_m: f64,
    pub(crate) draft_m: f64,
    /// Longest effective range of a working gun (not point defence), metres; 0 = none.
    pub(crate) gun_range_m: f64,
    /// Longest effective range of a working anti-ship missile or torpedo launcher, metres; 0 = none.
    pub(crate) missile_range_m: f64,
    pub(crate) self_defence_m: f64,
    pub(crate) visual_ref_m: f64,
    pub(crate) last_fired_tick: u64,
    pub(crate) is_player: bool,
    /// Position and heading of the matured formation leader or charge, own side only.
    pub(crate) leader: Option<(DVec2, f64)>,
    pub(crate) strike: StrikeFacts,
}

/// A perceived opposing surface or subsurface contact, or a perceived inbound weapon.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Contact { pub(crate) id: TrackId, pub(crate) pos: DVec2, pub(crate) vel: DVec2, pub(crate) range: f64, pub(crate) hostile: bool, pub(crate) fired_on_us: bool, pub(crate) grade: TrackGrade }

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Draws { pub(crate) side: f64, pub(crate) zig: f64 }

pub(crate) struct Situation<'a> {
    pub(crate) own: &'a OwnState,
    pub(crate) threats: &'a [Contact],
    pub(crate) inbound: &'a [Contact],
    pub(crate) mods: &'a DoctrineMods,
    pub(crate) now: u64,
    pub(crate) draws: Draws,
    /// Water depth at a world (x, z) point, 0 on land.
    pub(crate) depth: &'a dyn Fn(DVec2) -> f64,
}

fn xz(v: DVec3) -> DVec2 { DVec2::new(v.x, v.z) }
fn finite2(v: DVec2) -> bool { v.x.is_finite() && v.y.is_finite() }

/// Longest effective range of a working gun and of a working anti-ship missile or torpedo launcher (point
/// defence excluded), metres; 0 = none. `range_of` gives a mount's range (its doctrine entry, else the design's).
///
/// DECISION, u1: a launcher counts only while it can still fire (`engage::weapon_can_fire`: a round ready or
/// reloading, and power). The Arrears (duel.auditor) launched its four missiles by 534 s and its striker tree
/// then held at missile launch range, 13 km off, for the rest of the sortie with empty cells, so its 76 mm
/// never came into range: the duel had no fight. An emptied striker now fights on as a combatant with its gun.
pub(crate) fn weapon_ranges(mounts: &[crate::doctrine::MountInfo], parts: &[sim_core::parts::PartRuntime], range_of: impl Fn(&crate::doctrine::MountInfo) -> f64) -> (f64, f64) {
    let (mut gun, mut missile) = (0.0f64, 0.0f64);
    for m in mounts {
        let part = parts.get(m.slot.index());
        let working = part.is_some_and(|p| p.level != DamageLevel::Destroyed && p.health > 0.05);
        if !working || m.point_defence { continue; }
        let r = range_of(m);
        if !r.is_finite() || r <= 0.0 { continue; }
        match m.weapon {
            WeaponClass::Gun => gun = gun.max(r),
            WeaponClass::AntiShipMissile | WeaponClass::Torpedo if crate::engage::weapon_can_fire(part, parts) => missile = missile.max(r),
            _ => {}
        }
    }
    (gun, missile)
}

pub(crate) fn own_state(ship: &Ship, vessel: &Vessel, world: &World, c: &CaptainState, ctx: &SimCtx<'_>) -> OwnState {
    let pos = xz(vessel.kin.pose.pos);
    let heading = vessel.kin.pose.heading_rad();
    let status = match vessel.status { VesselStatus::Active => OwnStatus::Active, VesselStatus::Disabled => OwnStatus::Disabled, _ => OwnStatus::Lost };
    let health = hull_health(vessel, world.catalogue());
    let (gun, missile) = weapon_ranges(&ship.design.mounts, &vessel.parts, |m| {
        let cat = if m.weapon == WeaponClass::Torpedo { TargetCategory::Subsurface } else { TargetCategory::Surface };
        ship.doctrine.get(m.slot, cat).map(|e| e.effective_range_m).unwrap_or(m.effective_range_m)
    });
    let sd = [ship.doctrine.self_defence_range_m, ship.settings.self_defence_range_m.unwrap_or(0.0), ctx.tuning.doctrine.self_defence_range_m].into_iter().find(|r| r.is_finite() && *r > 0.0).unwrap_or(4_000.0);
    let leader = c.mem.leader.filter(|l| *l != ship.vessel).and_then(|l| world.vessel(l)).filter(|v| v.side == ship.side && v.status == VesselStatus::Active).map(|v| (xz(v.kin.pose.pos), v.kin.pose.heading_rad())).filter(|(p, h)| finite2(*p) && h.is_finite());
    let t = &ship.design.turn;
    let weapon_works = strike_weapon_works(&ship.design.mounts, &vessel.parts);
    let burning_or_flooding = vessel.compartments.compartments.iter().any(|c| c.fire > 0.0 || c.water_m3 > STRIKE_FLOOD_M3);
    OwnState {
        vessel: ship.vessel,
        pos: if finite2(pos) { pos } else { DVec2::ZERO },
        heading: if heading.is_finite() { heading } else { 0.0 },
        status,
        health,
        top_speed_mps: if t.top_speed_mps.is_finite() && t.top_speed_mps > 0.0 { t.top_speed_mps } else { 0.0 },
        turn_radius_m: if t.turn_radius_m.is_finite() && t.turn_radius_m > 0.0 { t.turn_radius_m } else { 200.0 },
        draft_m: if t.draft_m.is_finite() && t.draft_m > 0.0 { t.draft_m } else { 3.0 },
        gun_range_m: gun,
        missile_range_m: missile,
        self_defence_m: sd,
        visual_ref_m: ctx.tuning.sensors.visual_ref_range_m,
        last_fired_tick: vessel.last_fired_tick,
        is_player: ship.player.is_some() || vessel.player.is_some(),
        leader,
        strike: StrikeFacts { weapon_works, burning_or_flooding },
    }
}

/// Perceived threats (opposing surface and subsurface contacts) and inbound weapons, nearest first.
pub(crate) fn perceived(c: &CaptainState, picture: &SidePicture, own: &OwnState, now: u64) -> (Vec<Contact>, Vec<Contact>) {
    let mut threats = Vec::new();
    let mut inbound = Vec::new();
    for k in &c.mem.known {
        let Some(t) = picture.get(k.id) else { continue };
        if !t.is_live() || matches!(t.stance, Stance::Friend | Stance::Neutral) { continue; }
        let Some(ct) = contact_of(t, k, own.pos, now) else { continue };
        match t.domain {
            Domain::Surface | Domain::Subsurface => threats.push(ct),
            Domain::Weapon | Domain::Air => if k.inbound && t.inbound { inbound.push(ct) },
        }
    }
    threats.sort_by(|a, b| a.range.total_cmp(&b.range).then(a.id.cmp(&b.id)));
    inbound.sort_by(|a, b| a.range.total_cmp(&b.range).then(a.id.cmp(&b.id)));
    (threats, inbound)
}

/// A track as the captain sees it now: dead-reckoned from its last update (constant velocity, at most 60 s).
fn contact_of(t: &Track, k: &Known, me: DVec2, now: u64) -> Option<Contact> {
    let vel = if finite2(xz(t.vel)) { xz(t.vel) } else { DVec2::ZERO };
    let dt = (now.saturating_sub(t.last_update_tick) as f64 / TICKS_PER_S as f64).min(60.0);
    let pos = xz(t.pos) + vel * dt;
    if !finite2(pos) { return None; }
    Some(Contact { id: t.id, pos, vel, range: pos.distance(me), hostile: k.hostile, fired_on_us: t.fired_on_us, grade: t.grade })
}

// ---------------------------------------------------------------------------------------------------------
// Geometry
// ---------------------------------------------------------------------------------------------------------

/// Compass bearing from `a` to `b` (0 = north = -Z, clockwise).
fn bearing(a: DVec2, b: DVec2) -> f64 { let d = b - a; wrap_tau(libm::atan2(d.x, -d.y)) }
/// Unit vector of a compass heading in (x, z).
fn dir(h: f64) -> DVec2 { DVec2::new(libm::sin(h), -libm::cos(h)) }
fn wrap_tau(a: f64) -> f64 { let t = core::f64::consts::TAU; let x = a % t; if x < 0.0 { x + t } else { x } }
fn wrap_pi(a: f64) -> f64 { let x = wrap_tau(a); if x > core::f64::consts::PI { x - core::f64::consts::TAU } else { x } }
fn deg(d: f64) -> f64 { d * core::f64::consts::PI / 180.0 }

/// The heading closest to `want` whose probe line (`len` metres) stays in water deep enough for the keel.
fn safe_heading(want: f64, pos: DVec2, len: f64, draft: f64, depth: &dyn Fn(DVec2) -> f64) -> f64 {
    let need = draft + KEEL_CLEARANCE_M;
    let clear = |h: f64| (1..=4).all(|i| { let d = depth(pos + dir(h) * (len * i as f64 / 4.0)); d.is_finite() && d >= need });
    for step in 0..=12 {
        let off = deg(15.0 * step as f64);
        for s in [1.0, -1.0] {
            let h = wrap_tau(want + s * off);
            if clear(h) { return h; }
            if step == 0 || step == 12 { break; }
        }
    }
    wrap_tau(want)
}

// ---------------------------------------------------------------------------------------------------------
// Behaviour trees
// ---------------------------------------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Course {
    /// Leave route, station and formation as they are.
    Keep,
    /// Steer a compass heading (turned into a one-waypoint route clear of land).
    Heading(f64),
    Route(Vec<DVec2>),
    Formation { leader: EntityId, offset: DVec2, route: Vec<DVec2> },
    Station(DVec2, Option<f64>),
    /// All stop where it is.
    Stop,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Speed { Keep, Cruise, Flank, Fraction(f64), Knots(f64) }

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Plan {
    pub(crate) mode: CaptainMode,
    pub(crate) course: Course,
    pub(crate) speed: Speed,
    /// `None` keeps the current value.
    pub(crate) designated: Option<TrackId>,
    pub(crate) weapons_tight: Option<bool>,
    pub(crate) evade: Option<bool>,
    pub(crate) keep_range_m: Option<Option<f64>>,
    pub(crate) emcon: Option<Emcon>,
    pub(crate) preset: Option<DoctrinePreset>,
    pub(crate) strike: bool,
    /// The hull lies still (at anchor, or struck): `ShipOrders::at_rest`, so the Helm's own evasion does not
    /// get it under way.
    pub(crate) at_rest: bool,
}
impl Plan {
    fn new(mode: CaptainMode) -> Self { Plan { mode, course: Course::Keep, speed: Speed::Keep, designated: None, weapons_tight: None, evade: None, keep_range_m: None, emcon: None, preset: None, strike: false, at_rest: false } }
}

fn under_fire(c: &CaptainState, now: u64) -> bool { c.mem.under_fire_tick.is_some_and(|t| now.saturating_sub(t) < UNDER_FIRE_S * TICKS_PER_S) }
fn has_flag(s: &Situation<'_>, f: &str) -> bool { s.mods.flags.contains(f) }

/// Run the tree for this captain. Pure over its inputs (no world, no RNG, no clock).
pub(crate) fn think(c: &mut CaptainState, s: &Situation<'_>) -> Plan {
    let own = s.own;
    if own.status == OwnStatus::Lost {
        c.mode = CaptainMode::Adrift;
        return Plan::new(CaptainMode::Adrift);
    }
    if c.mode == CaptainMode::Struck {
        let mut p = Plan::new(CaptainMode::Struck);
        p.course = Course::Stop;
        p.speed = Speed::Knots(0.0);
        p.designated = Some(TrackId::NONE);
        p.weapons_tight = Some(true);
        p.evade = Some(false);
        p.at_rest = true;
        return p;
    }
    // Strike colours under the surrender rule (never a player hull, a proxy or an ace).
    {
        if surrender_due(c, own, s.threats, s.now) {
            c.mode = CaptainMode::Struck;
            let mut p = Plan::new(CaptainMode::Struck);
            p.course = Course::Stop;
            p.speed = Speed::Knots(0.0);
            p.designated = Some(TrackId::NONE);
            p.weapons_tight = Some(true);
            p.evade = Some(false);
            p.at_rest = true;
            p.strike = true;
            c.target = TrackId::NONE;
            return p;
        }
    }
    if c.behaviour == Behaviour::Proxy {
        let p = proxy(c, s);
        c.mode = p.mode;
        return p;
    }
    if own.status == OwnStatus::Disabled {
        // No propulsion: keep fighting where it lies.
        let tgt = pick_target(c, s);
        let mut p = Plan::new(CaptainMode::Adrift);
        p.course = Course::Stop;
        p.speed = Speed::Knots(0.0);
        p.designated = Some(tgt.map_or(TrackId::NONE, |t| t.id));
        p.weapons_tight = Some(false);
        p.evade = Some(false);
        c.mode = CaptainMode::Adrift;
        c.target = p.designated.unwrap_or_default();
        return p;
    }
    if let Some(t) = s.threats.first() { c.mem.threat_bearing = Some(bearing(own.pos, t.pos)); }
    if c.directive.anchored {
        let p = at_anchor(c, s);
        c.mode = p.mode;
        if let Some(d) = p.designated { c.target = d; }
        return p;
    }
    if !c.mem.withdraw_latched && !has_flag(s, "hold_ground") && wants_withdraw(&own.health, c.behaviour, c.ace.is_some()) { c.mem.withdraw_latched = true; }

    let target = pick_target(c, s);
    let mut p = if c.mem.withdraw_latched || c.directive.mode == GroupMode::Withdrawing && !matches!(c.behaviour, Behaviour::Static | Behaviour::Merchant) {
        withdraw(c, s)
    } else {
        match c.behaviour {
            Behaviour::Merchant => merchant(c, s),
            Behaviour::Static => stationary(c, s, target),
            Behaviour::Picket => picket(c, s, target),
            Behaviour::Striker if own.missile_range_m > 0.0 => striker(c, s, target),
            Behaviour::Escort => escort(c, s, target),
            Behaviour::Duelist => duelist(c, s, target),
            _ => combatant(c, s, target),
        }
    };
    overlay_threat_response(c, s, &mut p);
    p.preset = preset_for(c, s, &p);
    c.mode = p.mode;
    if let Some(d) = p.designated { c.target = d; }
    p
}

/// Best target: Hostile over Suspect, those that fired on us, the group's contact, nearer first. The current
/// target is kept unless another scores 40% better.
fn pick_target<'a>(c: &CaptainState, s: &Situation<'a>) -> Option<Contact> {
    let score = |t: &Contact| {
        let mut v = if t.hostile { 2.0 } else { 1.0 };
        if t.fired_on_us { v *= 1.5; }
        if t.id == c.directive.contact && !t.id.is_none() { v *= 1.5; }
        v / (1.0 + t.range / 10_000.0)
    };
    let best = s.threats.iter().max_by(|a, b| score(a).total_cmp(&score(b)).then(b.id.cmp(&a.id))).copied()?;
    if let Some(cur) = s.threats.iter().find(|t| t.id == c.target) {
        if score(cur) * 1.4 >= score(&best) { return Some(*cur); }
    }
    Some(best)
}

/// Doctrine range for fighting this target: the directive's (group, class and cards already applied), else
/// weapon-derived and scaled by the cards' engagement range factor and flags.
fn doctrine_range(c: &CaptainState, s: &Situation<'_>, prefer_missile: bool) -> f64 {
    let own = s.own;
    if let Some(r) = c.directive.keep_range_m.filter(|r| r.is_finite() && *r > 0.0) { return r.clamp(500.0, 100_000.0); }
    let weapon = if prefer_missile && own.missile_range_m > 0.0 { 0.9 * own.missile_range_m }
        else if own.gun_range_m > 0.0 { 0.7 * own.gun_range_m }
        else if own.missile_range_m > 0.0 { 0.85 * own.missile_range_m }
        else { own.self_defence_m };
    let f = if s.mods.engagement_range_factor.is_finite() { s.mods.engagement_range_factor.clamp(0.25, 4.0) } else { 1.0 };
    let mut r = weapon * f;
    if has_flag(s, "close_in") { r *= 0.7; }
    if has_flag(s, "stand_off") { r *= 1.3; }
    r.clamp(800.0, 100_000.0)
}

/// Keep `rs` metres from a target: intercept when far, broadside in the band (zigzagging the side), open on a
/// quarter course when close. Returns (heading, speed, mode).
fn range_keeping(c: &mut CaptainState, s: &Situation<'_>, t: &Contact, rs: f64) -> (f64, Speed, CaptainMode) {
    let own = s.own;
    let band = (BAND_FRACTION * rs).max(BAND_MIN_M);
    let to_t = bearing(own.pos, t.pos);
    if t.range > rs + band {
        let closing = own.top_speed_mps.max(3.0);
        let lead_t = (t.range / closing).min(300.0);
        let lead = t.pos + t.vel * lead_t;
        let h = if finite2(lead) { bearing(own.pos, lead) } else { to_t };
        return (h, Speed::Flank, CaptainMode::Attacking);
    }
    if c.mem.beam_sign == 0 {
        // Pick the broadside that needs the smaller turn; a near tie is drawn.
        let a = wrap_pi(to_t + core::f64::consts::FRAC_PI_2 - own.heading).abs();
        let b = wrap_pi(to_t - core::f64::consts::FRAC_PI_2 - own.heading).abs();
        c.mem.beam_sign = if (a - b).abs() < deg(30.0) { if s.draws.side < 0.5 { 1 } else { -1 } } else if a < b { 1 } else { -1 };
        c.mem.next_zig_tick = s.now + ((ZIG_MIN_S + ZIG_SPREAD_S * s.draws.zig.clamp(0.0, 1.0)) * TICKS_PER_S as f64) as u64;
    }
    let sign = c.mem.beam_sign as f64;
    if t.range < rs - band {
        // Open on a quarter course so the after mounts keep bearing.
        let away = wrap_tau(to_t + core::f64::consts::PI);
        return (wrap_tau(away - sign * deg(40.0)), Speed::Flank, CaptainMode::Opening);
    }
    if s.now >= c.mem.next_zig_tick {
        c.mem.beam_sign = -c.mem.beam_sign;
        c.mem.next_zig_tick = s.now + ((ZIG_MIN_S + ZIG_SPREAD_S * s.draws.zig.clamp(0.0, 1.0)) * TICKS_PER_S as f64) as u64;
    }
    let sign = c.mem.beam_sign as f64;
    // Beam course, turned in when beyond the doctrine range and out when inside it.
    let err = ((t.range - rs) / band).clamp(-1.0, 1.0);
    let off = core::f64::consts::FRAC_PI_2 - err * deg(25.0);
    (wrap_tau(to_t + sign * off), Speed::Fraction(0.8), CaptainMode::Attacking)
}

/// A hull this close to a route's first leg is on it, metres (the group replans a leader that strays farther,
/// `group::STRAY_M`).
const ON_ROUTE_M: f64 = 1_500.0;

/// The group's route from the leg the hull is on: leading waypoints it has already passed are dropped. A
/// waypoint is passed when the hull lies beyond it along the leg that leaves it, within [`ON_ROUTE_M`] of that
/// leg, and (for any waypoint after the first) beyond it along the leg that reaches it too, so a hull short of
/// a sharp turn never cuts the corner.
///
/// DECISION, u1: a planned route starts where the leader stood when it was planned, and the Helm restarts a
/// route at its first waypoint whenever its orders change. Each time collision avoidance (or a flight from a
/// threat) took the course for a decision and the captain then handed the group's route back, the hull turned
/// round for that stale start point. The prize tanker of prize_capture, ringed by its escorts, did this for 45
/// minutes at 13 to 16 kn within about 500 m of convoy_entry, 19 km from the end of its route.
fn resume_route(route: Vec<DVec2>, pos: DVec2) -> Vec<DVec2> {
    if !finite2(pos) { return route; }
    let mut first = 0;
    while first + 1 < route.len() {
        let (a, b) = (route[first], route[first + 1]);
        let t = super::path::proj_t(a, b, pos);
        let off = a.lerp(b, t.clamp(0.0, 1.0)).distance(pos);
        let reached = first == 0 || super::path::proj_t(route[first - 1], a, pos) >= 1.0;
        if !(reached && t > 0.0 && off <= ON_ROUTE_M) { break; }
        first += 1;
    }
    route[first..].to_vec()
}

fn follow_directive(c: &CaptainState, s: &Situation<'_>) -> Plan {
    let d = &c.directive;
    let mode = if d.mode == GroupMode::Investigating { CaptainMode::Investigating } else { CaptainMode::Following };
    let mut p = Plan::new(mode);
    let route: Vec<DVec2> = d.route.iter().copied().filter(|w| finite2(*w)).collect();
    p.course = match (c.mem.leader, s.own.leader) {
        (Some(l), Some(_)) if l != s.own.vessel && finite2(d.slot) => Course::Formation { leader: l, offset: d.slot, route },
        _ if !route.is_empty() => Course::Route(resume_route(route, s.own.pos)),
        _ => Course::Keep,
    };
    p.speed = if d.speed_kn.is_finite() && d.speed_kn > 0.0 { Speed::Knots(d.speed_kn) } else { Speed::Cruise };
    p.designated = Some(TrackId::NONE);
    p.weapons_tight = Some(d.weapons_tight);
    p.keep_range_m = Some(None);
    p.emcon = Some(search_emcon(c, s));
    p
}

/// Emission setting while following the group: [`base_emcon`], except that a group investigating a contact it
/// has lost searches with radar unless the group or the doctrine card set the emissions.
///
/// DECISION, u1 (L761 leaves the tree's emissions to this unit): Burst radiates only on a passive cue (L509),
/// and most Act 1 Combine hulls carry no ESM. In feint seed 1 (base 3278162, the player hull radiating Active)
/// the task group lost its one fire-control track at about 540 s, investigated the last known position in
/// Burst until 1,200 s and then stood 6.6 to 7.6 km from the radiating hull, none of its five hulls
/// radiating, until a lookout found it at 1,860 s.
fn search_emcon(c: &CaptainState, s: &Situation<'_>) -> Emcon {
    if c.directive.mode == GroupMode::Investigating && c.directive.emcon.is_none() && s.mods.emcon.is_none() { return Emcon::Active; }
    base_emcon(c, s)
}

/// Emission setting with nothing pressing: the group's, else the doctrine card's, else the tree's default
/// (radar pickets and sites radiate, everyone else bursts).
fn base_emcon(c: &CaptainState, s: &Situation<'_>) -> Emcon {
    let role = match c.behaviour { Behaviour::Picket | Behaviour::Static | Behaviour::Duelist => Emcon::Active, _ => Emcon::Burst };
    c.directive.emcon.or(s.mods.emcon).unwrap_or(role)
}

fn fight(c: &mut CaptainState, s: &Situation<'_>, t: &Contact, rs: f64) -> Plan {
    let (h, speed, mode) = range_keeping(c, s, t, rs);
    let mut p = Plan::new(mode);
    p.course = Course::Heading(h);
    p.speed = speed;
    p.designated = Some(t.id);
    p.weapons_tight = Some(c.directive.weapons_tight && !t.fired_on_us && !under_fire(c, s.now) && t.range > s.own.self_defence_m);
    p.keep_range_m = Some(Some(rs));
    // Guns need a fire-control lock to hit: radiate once the fight is inside reach.
    let base = base_emcon(c, s);
    p.emcon = Some(if t.hostile && t.range <= rs * 1.5 { Emcon::Active } else if has_flag(s, "silent_approach") && mode == CaptainMode::Attacking && t.range > rs * 1.5 { Emcon::Silent } else { base });
    p
}

fn combatant(c: &mut CaptainState, s: &Situation<'_>, target: Option<Contact>) -> Plan {
    let d = c.directive.clone();
    match target {
        Some(t) if t.hostile => {
            let rs = doctrine_range(c, s, false);
            // A group still closing in formation keeps formation until the fight is near.
            if d.mode == GroupMode::Closing && t.range > 1.5 * rs && c.mem.leader.is_some() && s.own.leader.is_some() {
                let mut p = follow_directive(c, s);
                p.mode = CaptainMode::Attacking;
                p.designated = Some(t.id);
                return p;
            }
            fight(c, s, &t, rs)
        }
        Some(t) => {
            // Suspect: close to visual identification range, never inside self-defence range unless doctrine
            // says close in.
            let id_range = (INVESTIGATE_FRACTION * s.own.visual_ref_m).max(s.own.self_defence_m * if has_flag(s, "close_in") { 0.6 } else { 1.1 });
            let mut p = fight(c, s, &t, id_range);
            if p.mode == CaptainMode::Attacking { p.mode = CaptainMode::Investigating; }
            p.emcon = Some(base_emcon(c, s));
            p
        }
        None if d.mode == GroupMode::Screening => escort(c, s, None),
        None => follow_directive(c, s),
    }
}

fn duelist(c: &mut CaptainState, s: &Situation<'_>, target: Option<Contact>) -> Plan {
    match target {
        Some(t) => {
            let rs = if s.own.gun_range_m > 0.0 { 0.7 * s.own.gun_range_m } else { doctrine_range(c, s, false) };
            let mut p = fight(c, s, &t, rs);
            // Never withdraws: a challenger inside the duel range is met on the beam, not run from, so a ship
            // whose guns reach less than the duelist's can still close and fight it.
            if t.range < rs {
                p.course = Course::Heading(wrap_tau(bearing(s.own.pos, t.pos) + c.mem.beam_sign as f64 * core::f64::consts::FRAC_PI_2));
                p.speed = Speed::Fraction(0.8);
                p.mode = CaptainMode::Attacking;
            }
            p.weapons_tight = Some(false);
            p.emcon = Some(Emcon::Active);
            p
        }
        None => { let mut p = follow_directive(c, s); p.emcon = Some(Emcon::Active); p }
    }
}

fn picket(c: &mut CaptainState, s: &Situation<'_>, target: Option<Contact>) -> Plan {
    let Some(t) = target else { return follow_directive(c, s) };
    let reach = s.own.gun_range_m.max(s.own.missile_range_m);
    let rs = (reach * 1.3).clamp(8_000.0, 16_000.0).max(2.0 * s.own.self_defence_m);
    let (h, speed, mode) = range_keeping(c, s, &t, rs);
    let mut p = Plan::new(if mode == CaptainMode::Opening { CaptainMode::Opening } else { CaptainMode::Investigating });
    p.course = Course::Heading(h);
    p.speed = speed;
    p.designated = Some(t.id);
    p.weapons_tight = Some(!(under_fire(c, s.now) || t.range <= s.own.self_defence_m));
    p.keep_range_m = Some(Some(rs));
    p.emcon = Some(base_emcon(c, s));
    p
}

fn striker(c: &mut CaptainState, s: &Situation<'_>, target: Option<Contact>) -> Plan {
    let own = s.own;
    let launch = doctrine_range(c, s, true);
    let Some(t) = target else { return follow_directive(c, s) };
    let away = wrap_tau(bearing(own.pos, t.pos) + core::f64::consts::PI);
    let opening = |c: &mut CaptainState| {
        let mut p = Plan::new(CaptainMode::Opening);
        p.course = Course::Heading(away);
        p.speed = Speed::Flank;
        p.designated = Some(t.id);
        p.weapons_tight = Some(false);
        p.keep_range_m = Some(Some(launch));
        p.emcon = Some(Emcon::Silent);
        c.mem.attack_start_tick = s.now;
        p
    };
    if !t.hostile {
        // Wait out the identification just beyond launch range, dark.
        let (h, speed, mode) = range_keeping(c, s, &t, launch * 1.2);
        let mut p = Plan::new(if mode == CaptainMode::Opening { CaptainMode::Opening } else { CaptainMode::Investigating });
        p.course = Course::Heading(h);
        p.speed = speed;
        p.designated = Some(t.id);
        p.weapons_tight = Some(true);
        p.keep_range_m = Some(Some(launch * 1.2));
        p.emcon = Some(Emcon::Silent);
        return p;
    }
    if s.now < c.mem.opening_until_tick { return opening(c); }
    if c.mode != CaptainMode::Attacking { c.mem.attack_start_tick = s.now; }
    let fired = own.last_fired_tick > c.mem.attack_start_tick && own.last_fired_tick <= s.now;
    if fired && t.range <= launch * 1.15 {
        c.mem.opening_until_tick = s.now + STRIKER_OPEN_S * TICKS_PER_S;
        return opening(c);
    }
    let (h, speed, mode) = range_keeping(c, s, &t, launch);
    let mut p = Plan::new(if mode == CaptainMode::Opening { CaptainMode::Opening } else { CaptainMode::Attacking });
    p.course = Course::Heading(h);
    p.speed = speed;
    p.designated = Some(t.id);
    p.weapons_tight = Some(false);
    p.keep_range_m = Some(Some(launch));
    // Dark on the run in; radiate at launch range only when the track is too coarse for the missiles.
    p.emcon = Some(if t.range > launch * 1.15 { Emcon::Silent } else if t.grade < TrackGrade::Coarse { Emcon::Active } else { Emcon::Burst });
    p
}

fn escort(c: &mut CaptainState, s: &Situation<'_>, target: Option<Contact>) -> Plan {
    let own = s.own;
    let Some((charge, _)) = own.leader.filter(|_| c.mem.leader.is_some_and(|l| l != own.vessel)) else {
        return combatant_no_escort(c, s, target);
    };
    let Some(t) = target else { return follow_directive(c, s) };
    let reach = if own.gun_range_m > 0.0 { own.gun_range_m } else { own.self_defence_m };
    let screen = (0.5 * reach).clamp(1_500.0, 4_000.0);
    if t.hostile && t.pos.distance(charge) <= screen + reach && own.pos.distance(charge) <= screen + reach {
        return fight(c, s, &t, doctrine_range(c, s, false));
    }
    let toward = (t.pos - charge).normalize_or_zero();
    let station = charge + toward * screen;
    let mut p = Plan::new(CaptainMode::Following);
    p.course = Course::Station(station, Some(bearing(charge, t.pos)));
    p.speed = Speed::Flank;
    p.designated = Some(t.id);
    p.weapons_tight = Some(c.directive.weapons_tight && !under_fire(c, s.now));
    p.keep_range_m = Some(None);
    p.emcon = Some(base_emcon(c, s));
    p
}

fn combatant_no_escort(c: &mut CaptainState, s: &Situation<'_>, target: Option<Contact>) -> Plan {
    let b = c.behaviour;
    c.behaviour = Behaviour::Combatant;
    let mut p = if c.directive.mode == GroupMode::Screening && target.is_none() { follow_directive(c, s) } else { combatant(c, s, target) };
    c.behaviour = b;
    if p.emcon.is_none() { p.emcon = Some(base_emcon(c, s)); }
    p
}

fn merchant(c: &mut CaptainState, s: &Situation<'_>) -> Plan {
    let own = s.own;
    let near_hostile = s.threats.iter().find(|t| t.hostile && t.range <= MERCHANT_THREAT_M);
    if under_fire(c, s.now) || near_hostile.is_some() || !s.inbound.is_empty() { c.mem.flee_until_tick = s.now + FLEE_S * TICKS_PER_S; }
    if s.now >= c.mem.flee_until_tick { let mut p = follow_directive(c, s); p.emcon = None; p.designated = None; return p; }
    let from = near_hostile.or(s.threats.first()).or(s.inbound.first()).map(|t| bearing(own.pos, t.pos)).or(c.mem.threat_bearing);
    let mut p = Plan::new(CaptainMode::Fleeing);
    p.course = match from { Some(b) => Course::Heading(wrap_tau(b + core::f64::consts::PI)), None => Course::Keep };
    p.speed = Speed::Flank;
    p.weapons_tight = Some(false);
    p
}

/// A hull whose group orders are `Anchor` lies at anchor (owner ruling of 22 November 2026): it makes no way,
/// a merchant does not flee, and no hull turns away from inbound weapons or an unseen shooter. An armed hull
/// fights from where it lies (DECISION, s2: as a site does, weapons free once under fire or holding a Hostile
/// contact; the ruling is silent on what an anchored warship fires at). A merchant designates nothing.
/// Before, in cutting_out seed 0 the prize crept north at the group's 1 kn hold speed and ran at 24 kn from
/// 480 s, when the player came inside the merchant's 10 km threat range.
fn at_anchor(c: &mut CaptainState, s: &Situation<'_>) -> Plan {
    let merchant = c.behaviour == Behaviour::Merchant;
    let target = if merchant { None } else { pick_target(c, s) };
    let mut p = Plan::new(if target.is_some_and(|t| t.hostile) { CaptainMode::Attacking } else { CaptainMode::Following });
    p.course = Course::Stop;
    p.speed = Speed::Knots(0.0);
    p.at_rest = true;
    p.designated = if merchant { None } else { Some(target.map_or(TrackId::NONE, |t| t.id)) };
    p.weapons_tight = Some(c.directive.weapons_tight && !under_fire(c, s.now) && !target.is_some_and(|t| t.hostile));
    p.evade = Some(false);
    p.keep_range_m = Some(None);
    p.emcon = if merchant { None } else { Some(base_emcon(c, s)) };
    p.preset = preset_for(c, s, &p);
    p
}

fn stationary(c: &mut CaptainState, s: &Situation<'_>, target: Option<Contact>) -> Plan {
    let anchor = *c.mem.anchor.get_or_insert(s.own.pos);
    let mut p = Plan::new(if target.is_some() { CaptainMode::Attacking } else { CaptainMode::Following });
    p.course = Course::Station(anchor, None);
    p.speed = Speed::Knots(0.0);
    p.designated = Some(target.map_or(TrackId::NONE, |t| t.id));
    p.weapons_tight = Some(false);
    p.evade = Some(false);
    p.emcon = Some(base_emcon(c, s));
    p
}

fn withdraw(c: &mut CaptainState, s: &Situation<'_>) -> Plan {
    let own = s.own;
    // Away from the weighted threat centroid, else from the last threat seen.
    let mut sum = DVec2::ZERO;
    for t in s.threats { let w = if t.hostile { 1.0 } else { 0.5 } / (1.0 + t.range / 5_000.0); sum += (t.pos - own.pos).normalize_or_zero() * w; }
    let from = if sum.length() > 1e-6 { Some(wrap_tau(libm::atan2(sum.x, -sum.y))) } else { c.mem.threat_bearing };
    let mut p = Plan::new(CaptainMode::Withdrawing);
    p.course = match from { Some(b) => Course::Heading(wrap_tau(b + core::f64::consts::PI)), None if !c.directive.route.is_empty() => Course::Route(c.directive.route.clone()), None => Course::Keep };
    p.speed = Speed::Flank;
    // Shoot back at whatever is close; run dark unless something is already shooting.
    let close = s.threats.iter().find(|t| t.hostile && t.range <= own.gun_range_m.max(own.self_defence_m));
    p.designated = Some(close.map_or(TrackId::NONE, |t| t.id));
    p.weapons_tight = Some(false);
    p.keep_range_m = Some(None);
    p.emcon = Some(if !s.inbound.is_empty() || under_fire(c, s.now) { Emcon::Burst } else { Emcon::Silent });
    p
}

fn proxy(c: &mut CaptainState, s: &Situation<'_>) -> Plan {
    let mut p = Plan::new(CaptainMode::Following);
    let route: Vec<DVec2> = c.route.iter().copied().filter(|w| finite2(*w)).collect();
    if !route.is_empty() { p.course = Course::Route(route); }
    p.evade = Some(!s.inbound.is_empty());
    if !s.inbound.is_empty() { p.mode = CaptainMode::Evading; }
    p
}

/// Inbound weapons and fire from an unseen shooter override the manoeuvre (not for sites, merchants already
/// fleeing, or hulls breaking off).
fn overlay_threat_response(c: &mut CaptainState, s: &Situation<'_>, p: &mut Plan) {
    let own = s.own;
    p.evade = Some(!s.inbound.is_empty() && c.behaviour != Behaviour::Static);
    if matches!(c.behaviour, Behaviour::Static) || matches!(p.mode, CaptainMode::Withdrawing | CaptainMode::Fleeing) { return; }
    if let Some(w) = s.inbound.first() {
        let from = bearing(own.pos, w.pos);
        let fast = w.vel.length() > 100.0;
        // Missiles: put them on the beam (decoys stay behind, the seeker sees the chaff); torpedoes: turn away.
        let sign = if c.mem.beam_sign == 0 { 1.0 } else { c.mem.beam_sign as f64 };
        let h = if fast { wrap_tau(from + sign * core::f64::consts::FRAC_PI_2) } else { wrap_tau(from + core::f64::consts::PI) };
        p.course = Course::Heading(h);
        p.speed = Speed::Flank;
        p.mode = CaptainMode::Evading;
        p.weapons_tight = Some(false);
        if p.emcon == Some(Emcon::Silent) { p.emcon = Some(Emcon::Burst); }
        return;
    }
    if under_fire(c, s.now) && !s.threats.iter().any(|t| t.hostile) {
        // Someone we cannot see is shooting: light up to find them and make the gunner's job hard.
        p.evade = Some(true);
        p.speed = Speed::Flank;
        p.weapons_tight = Some(false);
        p.emcon = Some(Emcon::Active);
        if matches!(p.mode, CaptainMode::Following | CaptainMode::Investigating) { p.mode = CaptainMode::Evading; }
    }
}

fn preset_for(c: &CaptainState, s: &Situation<'_>, p: &Plan) -> Option<DoctrinePreset> {
    match c.behaviour {
        Behaviour::Merchant | Behaviour::Proxy | Behaviour::Static => return None,
        _ => {}
    }
    if p.mode == CaptainMode::Withdrawing { return Some(DoctrinePreset::Cautious); }
    let fired_on = s.threats.iter().any(|t| t.hostile && t.fired_on_us);
    let aggressive = c.behaviour == Behaviour::Duelist || c.ace.is_some() || has_flag(s, "aggressive") || (p.mode == CaptainMode::Attacking && fired_on);
    Some(if aggressive { DoctrinePreset::Aggressive } else { DoctrinePreset::Cautious })
}

// ---------------------------------------------------------------------------------------------------------
// Orders
// ---------------------------------------------------------------------------------------------------------

/// Turn a plan into the changes to the ship's orders and settings. Pending launches and decoys are kept; a
/// heading close to the route already ordered keeps that route.
pub(crate) fn build_out(p: &Plan, cur: &ShipOrders, emcon: Emcon, preset: DoctrinePreset, own: &OwnState, depth: &dyn Fn(DVec2) -> f64) -> CaptainOut {
    let mut o = cur.clone();
    let leg = (4.0 * own.turn_radius_m).max(2_000.0);
    match &p.course {
        Course::Keep => {}
        Course::Heading(h) => {
            let h = safe_heading(*h, own.pos, leg, own.draft_m, depth);
            let keep = o.station.is_none() && o.formation.is_none() && o.route.len() == 1 && {
                let w = o.route[0];
                finite2(w) && w.distance(own.pos) > 0.5 * leg && wrap_pi(bearing(own.pos, w) - h).abs() < deg(5.0)
            };
            if !keep { o.route = vec![own.pos + dir(h) * leg]; }
            o.station = None;
            o.formation = None;
        }
        Course::Route(r) => { if o.route != *r { o.route = r.clone(); } o.station = None; o.formation = None; }
        Course::Formation { leader, offset, route } => {
            o.formation = Some(FormationSlot { leader: *leader, offset_m: *offset });
            if o.route != *route { o.route = route.clone(); }
            o.station = None;
        }
        Course::Station(at, h) => {
            let same = o.station.is_some_and(|(a, hh)| a.distance(*at) < 100.0 && match (hh, h) { (Some(x), Some(y)) => wrap_pi(x - y).abs() < deg(10.0), (None, None) => true, _ => false });
            if !same { o.station = Some((*at, *h)); }
            o.route.clear();
            o.formation = None;
        }
        Course::Stop => { o.route.clear(); o.station = None; o.formation = None; }
    }
    o.at_rest = p.at_rest;
    let top_kn = own.top_speed_mps / KN;
    match p.speed {
        Speed::Keep => {}
        Speed::Cruise => o.speed_kn = None,
        Speed::Flank => o.speed_kn = Some(if top_kn > 0.0 { (top_kn * 10.0).round() / 10.0 } else { 60.0 }),
        Speed::Fraction(f) => o.speed_kn = if top_kn > 0.0 { Some((top_kn * f.clamp(0.0, 1.0) * 10.0).round() / 10.0) } else { None },
        Speed::Knots(k) => o.speed_kn = Some(if k.is_finite() { k.max(0.0) } else { 0.0 }),
    }
    if let Some(d) = p.designated { o.designated = d; }
    if let Some(w) = p.weapons_tight { o.weapons_tight = w; }
    if let Some(e) = p.evade { o.evade = e; }
    if let Some(k) = p.keep_range_m { o.keep_range_m = k; }
    CaptainOut {
        orders: if o != *cur { Some(o) } else { None },
        preset: p.preset.filter(|x| *x != preset),
        emcon: p.emcon.filter(|x| *x != emcon),
        strike: p.strike,
    }
}

// ---------------------------------------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::commander::tests::track;
    use crate::tracks::TrackChange;

    fn own() -> OwnState {
        OwnState {
            vessel: EntityId::from_u64(1), pos: DVec2::ZERO, heading: 0.0, status: OwnStatus::Active,
            health: HullHealth { cells_lost: 0.0, weapons_total: 2, weapons_working: 2, propulsion_total: 2, propulsion_working: 2 },
            top_speed_mps: 15.0, turn_radius_m: 200.0, draft_m: 3.0, gun_range_m: 8_000.0, missile_range_m: 0.0, self_defence_m: 4_000.0,
            visual_ref_m: 10_000.0, last_fired_tick: 0, is_player: false, leader: None, strike: StrikeFacts::default(),
        }
    }
    fn contact(id: u32, pos: DVec2, hostile: bool) -> Contact { Contact { id: TrackId(id), pos, vel: DVec2::ZERO, range: pos.length(), hostile, fired_on_us: false, grade: TrackGrade::Fine } }
    fn sea(_: DVec2) -> f64 { 100.0 }

    /// Rule b: an intact decoy launcher with rounds is not a weapon that works; an intact gun with rounds is.
    #[test]
    fn a_decoy_launcher_does_not_keep_a_hull_from_striking() {
        use crate::doctrine::tests::mount;
        use sim_core::parts::{MountState, PartState};
        let part = |slot: u16, state: PartState| sim_core::parts::PartRuntime {
            slot: PartSlot(slot), def: PartIdx(0), pos: Default::default(), rot: Default::default(), cells: Vec::new(), centre: DVec3::ZERO,
            compartment: None, health: 1.0, level: Default::default(), links: Default::default(), priority: 0, state,
        };
        let decoy = vec![mount(0, WeaponClass::Decoy, 2_000.0)];
        let parts = vec![part(0, PartState::Decoy { rounds: 6, reload_ticks: 0, streamed: false })];
        assert!(!strike_weapon_works(&decoy, &parts), "a decoy launcher is not a weapon for rule b");
        let gun = vec![mount(0, WeaponClass::Decoy, 2_000.0), mount(1, WeaponClass::Gun, 8_000.0)];
        let parts = vec![parts[0].clone(), part(1, PartState::Mount(MountState { ready_rounds: 10, ..Default::default() }))];
        assert!(strike_weapon_works(&gun, &parts), "a loaded, intact gun still works");
    }

    /// cutting_out seed 1, `BP_DROP=all`: the prize, taken and ordered home at cruise, had the boarding hull
    /// alongside; avoidance wrote its speed over ground (1.02 kn) into a proxy plan that keeps its speed, and it
    /// kept 1.02 kn for the rest of the sortie.
    #[test]
    fn a_kept_speed_comes_back_once_the_way_is_clear() {
        use content::campaign::MapDef;
        let t = sim_core::terrain::Terrain::new(MapDef { size_m: [40_000.0, 40_000.0], depth_m: 200.0, ..Default::default() });
        let o = own();
        let cruise = o.top_speed_mps * 0.7 / KN;
        // About 1 kn ahead, the captor lying 63 m off the bow.
        let me = Kinematics { pose: Pose::from_heading(DVec3::ZERO, 0.0), lin_vel: DVec3::new(0.0, 0.0, -0.5), ang_vel: DVec3::ZERO };
        let alongside = Kinematics { pose: Pose::from_heading(DVec3::new(60.0, 0.0, -20.0), 0.0), lin_vel: DVec3::ZERO, ang_vel: DVec3::ZERO };
        let mods = DoctrineMods::default();
        let mut c = new_captain(Behaviour::Proxy, None, None, SurrenderRule::Never);
        c.route = vec![DVec2::new(0.0, -10_000.0)];
        let mut orders = ShipOrders { route: c.route.clone(), speed_kn: None, ..Default::default() };
        let decide = |c: &mut CaptainState, orders: &mut ShipOrders, near: &[Kinematics], now: u64| {
            let mut p = run(c, &o, &[], &[], &mods, now);
            assert_eq!(p.speed, Speed::Keep, "a proxy keeps its speed");
            steer_clear(c, &mut p, &me, near, &o, orders.speed_kn, cruise, &t);
            if let Some(n) = build_out(&p, orders, Emcon::Burst, DoctrinePreset::Cautious, &o, &sea).orders { *orders = n; }
        };
        decide(&mut c, &mut orders, &[alongside], 10);
        let k = orders.speed_kn.expect("avoidance sets a speed");
        assert!(k >= 0.2 * cruise - 1e-9, "scaled from the cruise speed, not the 1 kn over ground: {k}");
        decide(&mut c, &mut orders, &[alongside], 20);
        assert_eq!(orders.speed_kn, Some(k), "no ratchet while the risk lasts");
        decide(&mut c, &mut orders, &[], 30);
        assert_eq!(orders.speed_kn, None, "the way clear, it cruises again");
        // An ordered speed comes back as ordered.
        orders.speed_kn = Some(12.0);
        decide(&mut c, &mut orders, &[alongside], 40);
        assert_ne!(orders.speed_kn, Some(12.0));
        decide(&mut c, &mut orders, &[], 50);
        assert_eq!(orders.speed_kn, Some(12.0));
        // A speed someone else ordered while avoidance held the hull stands.
        decide(&mut c, &mut orders, &[alongside], 60);
        orders.speed_kn = Some(5.0);
        decide(&mut c, &mut orders, &[], 70);
        assert_eq!(orders.speed_kn, Some(5.0));
        // A plan with its own speed is scaled from it, and the next clear decision writes it anew.
        let mut m = new_captain(Behaviour::Merchant, None, None, SurrenderRule::Never);
        m.directive.route = vec![DVec2::new(0.0, -10_000.0)];
        let mut p = run(&mut m, &o, &[], &[], &mods, 80);
        assert_eq!(p.speed, Speed::Cruise);
        steer_clear(&mut m, &mut p, &me, &[alongside], &o, Some(3.0), cruise, &t);
        assert!(matches!(p.speed, Speed::Knots(k) if k >= 0.2 * cruise - 1e-9), "{:?}", p.speed);
        assert_eq!(m.mem.avoid_speed, None, "only a kept speed is remembered");
    }

    /// Owner ruling of 22 November 2026: a hull whose group orders are Anchor is at anchor; it makes no way and
    /// its captain does not flee. cutting_out seed 0: the anchored prize ran at 24 kn from 480 s, when the player
    /// came inside the merchant's 10 km threat range.
    #[test]
    fn a_hull_at_anchor_makes_no_way_and_does_not_flee() {
        let mods = DoctrineMods::default();
        let o = own();
        let near = [contact(2, DVec2::new(0.0, -9_800.0), true)];
        let mut missile = contact(3, DVec2::new(0.0, -3_000.0), true);
        missile.vel = DVec2::new(0.0, 250.0);
        let anchored = |behaviour: Behaviour| {
            let mut c = new_captain(behaviour, None, None, SurrenderRule::Never);
            c.directive.anchored = true;
            c.directive.route = vec![DVec2::new(0.0, 0.0)];
            c.directive.weapons_tight = true;
            c
        };
        let mut c = anchored(Behaviour::Merchant);
        c.mem.under_fire_tick = Some(5);
        for (now, threats, inbound) in [(10, &near[..], &[][..]), (20, &near[..], &[missile][..]), (30, &[][..], &[][..])] {
            let p = run(&mut c, &o, threats, inbound, &mods, now);
            assert_eq!((p.course.clone(), p.speed, p.evade), (Course::Stop, Speed::Knots(0.0), Some(false)), "at {now}");
            assert_ne!(p.mode, CaptainMode::Fleeing);
            // The orders tell the Helm it lies still, so its own evasion does not get it under way.
            let sea = |_: DVec2| 100.0;
            let orders = build_out(&p, &ShipOrders { route: vec![DVec2::new(5.0, 5.0)], ..Default::default() }, Emcon::Burst, DoctrinePreset::Cautious, &o, &sea).orders.expect("new orders");
            assert!(orders.at_rest && orders.route.is_empty() && orders.speed_kn == Some(0.0), "{orders:?}");
        }
        // An armed hull at anchor fights from where it lies.
        let mut w = anchored(Behaviour::Combatant);
        let p = run(&mut w, &o, &[contact(2, DVec2::new(0.0, -5_000.0), true)], &[], &mods, 10);
        assert_eq!((p.course, p.speed, p.designated, p.weapons_tight, p.mode), (Course::Stop, Speed::Knots(0.0), Some(TrackId(2)), Some(false), CaptainMode::Attacking));
        // A group that only holds station is not at anchor: its merchant still flees.
        let mut h = anchored(Behaviour::Merchant);
        h.directive.anchored = false;
        let p = run(&mut h, &o, &near, &[], &mods, 10);
        assert_eq!((p.mode, p.speed), (CaptainMode::Fleeing, Speed::Flank));
    }

    #[test]
    fn a_handed_back_route_resumes_from_the_leg_the_hull_is_on() {
        let mods = DoctrineMods::default();
        // prize_capture seed 2: the tanker's planned route starts at convoy_entry, where it stood when the plan
        // was made. After a collision-avoidance turn the route came back whole and the hull turned round for it.
        let (entry, exit) = (DVec2::new(5_505.0, -19_022.0), DVec2::new(3_000.0, 19_000.0));
        let mut o = own();
        o.pos = DVec2::new(5_241.0, -18_508.0);
        let mut c = new_captain(Behaviour::Merchant, None, None, SurrenderRule::Never);
        c.directive.route = vec![entry, exit];
        assert_eq!(run(&mut c, &o, &[], &[], &mods, 10).course, Course::Route(vec![exit]), "a passed start point is dropped");
        o.pos = DVec2::new(5_505.0, -19_800.0);
        assert_eq!(run(&mut c, &o, &[], &[], &mods, 20).course, Course::Route(vec![entry, exit]), "a start point ahead is kept");
        o.pos = DVec2::new(9_000.0, -18_000.0);
        assert_eq!(run(&mut c, &o, &[], &[], &mods, 30).course, Course::Route(vec![entry, exit]), "far off the route it is kept");
        // A sharp turn: short of the corner, the hull already lies beside the next leg; it must not cut it.
        let (a, b, d) = (DVec2::new(0.0, 0.0), DVec2::new(0.0, -5_000.0), DVec2::new(800.0, 0.0));
        c.directive.route = vec![a, b, d];
        o.pos = DVec2::new(100.0, -4_000.0);
        assert_eq!(run(&mut c, &o, &[], &[], &mods, 40).course, Course::Route(vec![b, d]), "the corner is kept until it is passed");
        o.pos = DVec2::new(150.0, -4_900.0);
        assert_eq!(run(&mut c, &o, &[], &[], &mods, 50).course, Course::Route(vec![b, d]));
        o.pos = DVec2::new(400.0, -4_600.0);
        assert_eq!(run(&mut c, &o, &[], &[], &mods, 60).course, Course::Route(vec![b, d]), "short of the corner along the first leg");
        // Past an open corner on both legs: both passed points go.
        let e = DVec2::new(5_000.0, -10_000.0);
        c.directive.route = vec![a, b, e];
        o.pos = DVec2::new(600.0, -5_600.0);
        assert_eq!(run(&mut c, &o, &[], &[], &mods, 70).course, Course::Route(vec![e]));
    }

    #[test]
    fn a_group_searching_for_a_lost_contact_radiates() {
        // feint seed 1: a task group with no ESM lost the player's track and searched for it in Burst, which
        // waits for a passive cue it could never get.
        let o = own();
        let mut c = new_captain(Behaviour::Combatant, None, None, SurrenderRule::Never);
        c.directive.mode = GroupMode::Investigating;
        c.directive.route = vec![DVec2::new(0.0, -8_000.0)];
        let mods = DoctrineMods::default();
        let p = run(&mut c, &o, &[], &[], &mods, 10);
        assert_eq!((p.mode, p.emcon), (CaptainMode::Investigating, Some(Emcon::Active)));
        c.directive.mode = GroupMode::Standing;
        assert_eq!(run(&mut c, &o, &[], &[], &mods, 20).emcon, Some(Emcon::Burst), "a group standing on its orders stays in Burst");
        c.directive.mode = GroupMode::Investigating;
        let quiet = DoctrineMods { emcon: Some(Emcon::Silent), ..Default::default() };
        assert_eq!(run(&mut c, &o, &[], &[], &quiet, 30).emcon, Some(Emcon::Silent), "a doctrine card's setting stands");
        c.directive.emcon = Some(Emcon::Silent);
        assert_eq!(run(&mut c, &o, &[], &[], &mods, 40).emcon, Some(Emcon::Silent), "the group's setting stands");
    }

    #[test]
    fn an_emptied_launcher_gives_no_missile_range() {
        use crate::doctrine::tests::mount;
        use sim_core::parts::{MountState, PartState};
        let part = |slot: u16, state: PartState| sim_core::parts::PartRuntime {
            slot: PartSlot(slot), def: PartIdx(0), pos: Default::default(), rot: Default::default(), cells: Vec::new(), centre: DVec3::ZERO,
            compartment: None, health: 1.0, level: Default::default(), links: Default::default(), priority: 0, state,
        };
        let launcher = |ready: u8, reload_ticks: u32| PartState::Launcher { ready, reload_ticks, interval_ticks: 0, train_rad: 0.0 };
        let mounts = vec![mount(0, WeaponClass::AntiShipMissile, 24_000.0), mount(1, WeaponClass::Gun, 8_000.0)];
        let gun = part(1, PartState::Mount(MountState { ready_rounds: 10, ..Default::default() }));
        let range = |m: &crate::doctrine::MountInfo| m.effective_range_m;
        assert_eq!(weapon_ranges(&mounts, &[part(0, launcher(2, 0)), gun.clone()], range), (8_000.0, 24_000.0));
        assert_eq!(weapon_ranges(&mounts, &[part(0, launcher(0, 600)), gun.clone()], range), (8_000.0, 24_000.0), "a reloading launcher keeps its reach");
        // The Arrears after its fourth missile: both cells empty, no reload. Its striker tree held at missile
        // launch range for the rest of the duel; with no missile range it fights as a combatant on its gun.
        assert_eq!(weapon_ranges(&mounts, &[part(0, launcher(0, 0)), gun], range), (8_000.0, 0.0));
        let mut o = own();
        o.missile_range_m = 0.0;
        let mut c = new_captain(Behaviour::Striker, None, None, SurrenderRule::Never);
        let p = run(&mut c, &o, &[contact(2, DVec2::new(0.0, -13_300.0), true)], &[], &DoctrineMods::default(), 10);
        assert_eq!(p.mode, CaptainMode::Attacking);
        assert_eq!(p.keep_range_m, Some(Some(0.7 * 8_000.0)), "closes to its gun's doctrine range");
    }

    fn run(c: &mut CaptainState, own: &OwnState, threats: &[Contact], inbound: &[Contact], mods: &DoctrineMods, now: u64) -> Plan {
        let sit = Situation { own, threats, inbound, mods, now, draws: Draws { side: 0.25, zig: 0.5 }, depth: &sea };
        think(c, &sit)
    }
    fn heading_of(p: &Plan) -> f64 { match p.course { Course::Heading(h) => h, ref o => panic!("no heading: {o:?}") } }
    fn ang(a: f64, b: f64) -> f64 { wrap_pi(a - b).abs() }

    #[test]
    fn geometry_conventions() {
        assert!(ang(bearing(DVec2::ZERO, DVec2::new(0.0, -1.0)), 0.0) < 1e-12);
        assert!(ang(bearing(DVec2::ZERO, DVec2::new(1.0, 0.0)), deg(90.0)) < 1e-12);
        assert!(dir(deg(90.0)).distance(DVec2::new(1.0, 0.0)) < 1e-12);
        assert!(dir(0.0).distance(DVec2::new(0.0, -1.0)) < 1e-12);
    }

    #[test]
    fn new_captain_refines_roles_and_guards_surrender() {
        assert_eq!(new_captain(Behaviour::Combatant, Some(ClassRole::Freighter), None, SurrenderRule::Never).behaviour, Behaviour::Merchant);
        assert_eq!(new_captain(Behaviour::Combatant, Some(ClassRole::Picket), None, SurrenderRule::Never).behaviour, Behaviour::Picket);
        assert_eq!(new_captain(Behaviour::Striker, Some(ClassRole::Freighter), None, SurrenderRule::Never).behaviour, Behaviour::Striker);
        assert_eq!(new_captain(Behaviour::Proxy, None, None, SurrenderRule::Strike { cells_lost: 0.5 }).surrender, SurrenderRule::Never);
        assert_eq!(new_captain(Behaviour::Proxy, None, None, SurrenderRule::Combine).surrender, SurrenderRule::Never);
        assert_eq!(new_captain(Behaviour::Combatant, None, None, SurrenderRule::Strike { cells_lost: f64::NAN }).surrender, SurrenderRule::Never);
        let spec = content::campaign::CampaignRules::default().strike_cells_lost;
        assert_eq!(new_captain(Behaviour::Combatant, None, None, SurrenderRule::Combine).surrender, SurrenderRule::Strike { cells_lost: spec });
    }

    #[test]
    fn reaction_time_delays_contacts_and_stance() {
        let mut c = new_captain(Behaviour::Combatant, None, None, SurrenderRule::Never);
        let mut t = track(4, DVec2::new(0.0, -20_000.0), Stance::Suspect, Domain::Surface);
        t.created_tick = 600;
        let mut pic = SidePicture { side: SideId(1), tracks: vec![t], next_id: 5 };
        perceive(&mut c, &[TrackChange::Created(TrackId(4))], false, 600);
        let reaction = 120; // 2 s
        scan_picture(&mut c, &pic, 610);
        absorb(&mut c, &pic, 610, reaction);
        assert!(known(&c, TrackId(4)).is_none(), "not yet perceived");
        finish_decision(&mut c, 610, reaction, 0.5);
        // Cadence 1 s (half the 2 s reaction), before the contact matures at 720.
        assert_eq!(c.next_decision_tick, 670);
        absorb(&mut c, &pic, 720, reaction);
        assert!(known(&c, TrackId(4)).is_some_and(|k| !k.hostile));
        // The track turns Hostile at tick 800: perceived 2 s after the midpoint of the look interval.
        pic.tracks[0].stance = Stance::Hostile;
        c.mem.last_decision_tick = 780;
        scan_picture(&mut c, &pic, 820);
        absorb(&mut c, &pic, 820, reaction);
        assert!(!known(&c, TrackId(4)).unwrap().hostile);
        absorb(&mut c, &pic, 800 + reaction, reaction);
        assert!(known(&c, TrackId(4)).unwrap().hostile);
        // A weapon detected already inbound is perceived inbound when it matures, not a reaction time later.
        let mut w = track(6, DVec2::new(0.0, -5_000.0), Stance::Suspect, Domain::Weapon);
        w.created_tick = 900;
        w.inbound = true;
        pic.tracks.push(w);
        scan_picture(&mut c, &pic, 905);
        absorb(&mut c, &pic, 900 + reaction, reaction);
        assert!(known(&c, TrackId(6)).is_some_and(|k| k.inbound));
        // Merges carry knowledge across at once.
        perceive(&mut c, &[TrackChange::Merged { from: TrackId(4), into: TrackId(9) }], false, 950);
        assert!(known(&c, TrackId(9)).is_some_and(|k| k.hostile) && known(&c, TrackId(4)).is_none());
    }

    #[test]
    fn combatant_closes_beams_and_opens_by_doctrine_range() {
        let mods = DoctrineMods::default();
        let o = own();
        let rs = 0.7 * 8_000.0;
        // Far: intercept course straight at a stationary target (north).
        let mut c = new_captain(Behaviour::Combatant, None, None, SurrenderRule::Never);
        let far = [contact(2, DVec2::new(0.0, -20_000.0), true)];
        let p = run(&mut c, &o, &far, &[], &mods, 100);
        assert_eq!(p.mode, CaptainMode::Attacking);
        assert!(ang(heading_of(&p), 0.0) < 1e-6);
        assert_eq!(p.speed, Speed::Flank);
        assert_eq!(p.designated, Some(TrackId(2)));
        // In the band: broadside.
        let band = [contact(2, DVec2::new(0.0, -rs), true)];
        let p = run(&mut c, &o, &band, &[], &mods, 200);
        assert_eq!(p.mode, CaptainMode::Attacking);
        assert!((ang(heading_of(&p), 0.0) - deg(90.0)).abs() < deg(1.0), "beam {}", heading_of(&p));
        assert_eq!(p.emcon, Some(Emcon::Active));
        // Close: open away from the target.
        let near = [contact(2, DVec2::new(0.0, -2_000.0), true)];
        let p = run(&mut c, &o, &near, &[], &mods, 300);
        assert_eq!(p.mode, CaptainMode::Opening);
        assert!(ang(heading_of(&p), deg(180.0)) <= deg(40.0) + 1e-9);
        // Stand-off doctrine moves the band out.
        let mut m2 = DoctrineMods::default();
        m2.flags.insert("stand_off".into());
        let mut c2 = new_captain(Behaviour::Combatant, None, None, SurrenderRule::Never);
        assert_eq!(run(&mut c2, &o, &band, &[], &m2, 100).mode, CaptainMode::Opening, "5.6 km is now too close");
        let p = run(&mut c2, &o, &[contact(2, DVec2::new(0.0, -rs * 1.3), true)], &[], &m2, 200);
        assert_eq!(p.mode, CaptainMode::Attacking);
        assert_eq!(p.keep_range_m, Some(Some(rs * 1.3)));
    }

    #[test]
    fn beam_side_zigzags() {
        let mods = DoctrineMods::default();
        let o = own();
        let band = [contact(2, DVec2::new(0.0, -5_600.0), true)];
        let mut c = new_captain(Behaviour::Combatant, None, None, SurrenderRule::Never);
        let h1 = heading_of(&run(&mut c, &o, &band, &[], &mods, 0));
        let h2 = heading_of(&run(&mut c, &o, &band, &[], &mods, 200 * TICKS_PER_S));
        assert!(ang(h1, h2) > deg(150.0), "flipped side: {h1} {h2}");
    }

    #[test]
    fn withdraws_when_hurt_and_latches() {
        let mods = DoctrineMods::default();
        let mut o = own();
        o.health.cells_lost = 0.3;
        let mut c = new_captain(Behaviour::Combatant, None, None, SurrenderRule::Never);
        let th = [contact(2, DVec2::new(5_000.0, 0.0), true)];
        let p = run(&mut c, &o, &th, &[], &mods, 10);
        assert_eq!(p.mode, CaptainMode::Withdrawing);
        assert!(ang(heading_of(&p), deg(270.0)) < 1e-6, "away from the threat to the east");
        assert_eq!(p.emcon, Some(Emcon::Silent));
        assert!(c.is_withdrawing());
        o.health.cells_lost = 0.0;
        assert_eq!(run(&mut c, &o, &th, &[], &mods, 20).mode, CaptainMode::Withdrawing);
        // Duelists and hold-ground doctrine never withdraw.
        let mut d = new_captain(Behaviour::Duelist, None, None, SurrenderRule::Never);
        o.health.cells_lost = 0.6;
        assert_ne!(run(&mut d, &o, &th, &[], &mods, 10).mode, CaptainMode::Withdrawing);
        let mut hg = DoctrineMods::default();
        hg.flags.insert("hold_ground".into());
        let mut c3 = new_captain(Behaviour::Combatant, None, None, SurrenderRule::Never);
        assert_ne!(run(&mut c3, &o, &th, &[], &hg, 10).mode, CaptainMode::Withdrawing);
    }

    #[test]
    fn evades_inbound_missiles() {
        let mods = DoctrineMods::default();
        let o = own();
        let mut c = new_captain(Behaviour::Combatant, None, None, SurrenderRule::Never);
        let mut w = contact(7, DVec2::new(0.0, -6_000.0), true);
        w.vel = DVec2::new(0.0, 280.0);
        let p = run(&mut c, &o, &[contact(2, DVec2::new(0.0, -30_000.0), true)], &[w], &mods, 10);
        assert_eq!(p.mode, CaptainMode::Evading);
        assert_eq!(p.evade, Some(true));
        assert!((ang(heading_of(&p), 0.0) - deg(90.0)).abs() < 1e-6, "missile on the beam");
    }

    #[test]
    fn striker_runs_in_dark_fires_and_opens() {
        let mods = DoctrineMods::default();
        let mut o = own();
        o.gun_range_m = 0.0;
        o.missile_range_m = 40_000.0;
        let launch = 0.9 * 40_000.0;
        let mut c = new_captain(Behaviour::Striker, None, None, SurrenderRule::Never);
        let far = [contact(2, DVec2::new(0.0, -60_000.0), true)];
        let p = run(&mut c, &o, &far, &[], &mods, 10);
        assert_eq!((p.mode, p.emcon), (CaptainMode::Attacking, Some(Emcon::Silent)));
        let at = [contact(2, DVec2::new(0.0, -launch), true)];
        let p = run(&mut c, &o, &at, &[], &mods, 20);
        assert_eq!(p.mode, CaptainMode::Attacking);
        assert_eq!(p.emcon, Some(Emcon::Burst));
        o.last_fired_tick = 25;
        let p = run(&mut c, &o, &at, &[], &mods, 30);
        assert_eq!(p.mode, CaptainMode::Opening);
        assert!(ang(heading_of(&p), deg(180.0)) < 1e-6);
        assert_eq!(run(&mut c, &o, &at, &[], &mods, 30 + 60 * TICKS_PER_S).mode, CaptainMode::Opening);
        assert_eq!(run(&mut c, &o, &at, &[], &mods, 30 + 91 * TICKS_PER_S).mode, CaptainMode::Attacking);
    }

    #[test]
    fn picket_shadows_outside_reach_and_holds_fire() {
        let mods = DoctrineMods::default();
        let o = own();
        let mut c = new_captain(Behaviour::Picket, None, None, SurrenderRule::Never);
        let p = run(&mut c, &o, &[contact(2, DVec2::new(0.0, -10_400.0), true)], &[], &mods, 10);
        assert_eq!(p.mode, CaptainMode::Investigating);
        assert_eq!(p.weapons_tight, Some(true));
        assert_eq!(p.emcon, Some(Emcon::Active));
        assert_eq!(p.keep_range_m, Some(Some(10_400.0)));
    }

    #[test]
    fn merchant_flees_then_resumes() {
        let mods = DoctrineMods::default();
        let o = own();
        let mut c = new_captain(Behaviour::Merchant, None, None, SurrenderRule::Never);
        c.directive.route = vec![DVec2::new(0.0, -50_000.0)];
        let th = [contact(2, DVec2::new(0.0, -6_000.0), true)];
        let p = run(&mut c, &o, &th, &[], &mods, 10);
        assert_eq!(p.mode, CaptainMode::Fleeing);
        assert!(ang(heading_of(&p), deg(180.0)) < 1e-6);
        assert_eq!(p.preset, None);
        let p = run(&mut c, &o, &[], &[], &mods, 10 + 301 * TICKS_PER_S);
        assert_eq!(p.mode, CaptainMode::Following);
        assert_eq!(p.course, Course::Route(vec![DVec2::new(0.0, -50_000.0)]));
    }

    /// Run the tree once a second from `from` to `to` (inclusive) and return the first second that strikes.
    fn strike_second(c: &mut CaptainState, o: &OwnState, th: &[Contact], from: u64, to: u64) -> Option<u64> {
        let mods = DoctrineMods::default();
        (from..=to).find(|s| run(c, o, th, &[], &mods, s * TICKS_PER_S).strike)
    }

    #[test]
    fn surrender_only_under_the_rule_and_never_for_players() {
        let mods = DoctrineMods::default();
        let mut o = own();
        o.health.cells_lost = 0.4;
        let th = [contact(2, DVec2::new(0.0, -2_000.0), true)];
        let rule = SurrenderRule::Strike { cells_lost: 0.3 };
        // (a) Hurt past the fraction while under fire: strikes after the 5 s confirm, once.
        let mut c = new_captain(Behaviour::Combatant, None, None, rule);
        c.mem.under_fire_tick = Some(10 * TICKS_PER_S);
        assert_eq!(strike_second(&mut c, &o, &th, 10, 30), Some(15), "the condition must hold for 5 s");
        assert_eq!(c.mode, CaptainMode::Struck);
        let p = run(&mut c, &o, &th, &[], &mods, 31 * TICKS_PER_S);
        assert!(!p.strike && p.mode == CaptainMode::Struck && p.weapons_tight == Some(true) && p.course == Course::Stop, "strikes once, then lies stopped and silent");
        // Not under fire for over 60 s: no strike, however hurt.
        let mut quiet = new_captain(Behaviour::Combatant, None, None, rule);
        quiet.mem.under_fire_tick = Some(0);
        assert_eq!(strike_second(&mut quiet, &o, &th, 70, 100), None);
        // A broken condition restarts the confirm time.
        let mut c = new_captain(Behaviour::Combatant, None, None, rule);
        c.mem.under_fire_tick = Some(10 * TICKS_PER_S);
        assert_eq!(strike_second(&mut c, &o, &th, 10, 12), None);
        let mut healthy = o;
        healthy.health.cells_lost = 0.1;
        assert!(!run(&mut c, &healthy, &th, &[], &mods, 13 * TICKS_PER_S).strike);
        assert_eq!(strike_second(&mut c, &o, &th, 14, 30), Some(19));
        // Never, players and aces never strike.
        let mut n = new_captain(Behaviour::Combatant, None, None, SurrenderRule::Never);
        n.mem.under_fire_tick = Some(10 * TICKS_PER_S);
        assert_eq!(strike_second(&mut n, &o, &th, 10, 30), None);
        let mut ace = new_captain(Behaviour::Combatant, None, None, rule);
        ace.ace = Some(AceId::new("ace.test"));
        ace.mem.under_fire_tick = Some(10 * TICKS_PER_S);
        assert_eq!(strike_second(&mut ace, &o, &th, 10, 30), None);
        let mut pl_own = o;
        pl_own.is_player = true;
        let mut pl = new_captain(Behaviour::Combatant, None, None, rule);
        pl.mem.under_fire_tick = Some(10 * TICKS_PER_S);
        assert_eq!(strike_second(&mut pl, &pl_own, &th, 10, 30), None);
    }

    #[test]
    fn surrender_when_no_weapon_works_or_disabled_and_burning() {
        let rule = SurrenderRule::Strike { cells_lost: 0.9 };
        let mut o = own();
        o.health.cells_lost = 0.05;
        o.strike.weapon_works = false;
        // (b) No weapon works and a hostile that has fired on us is out at 9 km: strikes.
        let mut far = contact(2, DVec2::new(0.0, -9_000.0), true);
        far.fired_on_us = true;
        let mut c = new_captain(Behaviour::Merchant, None, None, rule);
        c.mem.under_fire_tick = Some(10 * TICKS_PER_S);
        assert_eq!(strike_second(&mut c, &o, &[far], 10, 30), Some(15));
        // ... but not against a hostile that cannot reach it (out past self-defence, never fired on us).
        let quiet_far = contact(2, DVec2::new(0.0, -9_000.0), true);
        let mut c = new_captain(Behaviour::Merchant, None, None, rule);
        c.mem.under_fire_tick = Some(10 * TICKS_PER_S);
        assert_eq!(strike_second(&mut c, &o, &[quiet_far], 10, 30), None);
        // With a working weapon, a close hostile is no reason to strike.
        let close = [contact(2, DVec2::new(0.0, -2_000.0), true)];
        let mut armed = o;
        armed.strike.weapon_works = true;
        let mut c = new_captain(Behaviour::Combatant, None, None, rule);
        c.mem.under_fire_tick = Some(10 * TICKS_PER_S);
        assert_eq!(strike_second(&mut c, &armed, &close, 10, 30), None);
        // (c) Disabled and burning: strikes for display.
        let mut dead = armed;
        dead.status = OwnStatus::Disabled;
        dead.strike.burning_or_flooding = true;
        let mut c = new_captain(Behaviour::Combatant, None, None, rule);
        c.mem.under_fire_tick = Some(10 * TICKS_PER_S);
        assert_eq!(strike_second(&mut c, &dead, &[], 10, 30), Some(15));
        dead.strike.burning_or_flooding = false;
        let mut c = new_captain(Behaviour::Combatant, None, None, rule);
        c.mem.under_fire_tick = Some(10 * TICKS_PER_S);
        assert_eq!(strike_second(&mut c, &dead, &[], 10, 30), None, "disabled but dry and not burning fights on where it lies");
    }

    #[test]
    fn proxy_keeps_player_orders() {
        let mods = DoctrineMods::default();
        let o = own();
        let mut c = new_captain(Behaviour::Proxy, None, None, SurrenderRule::Never);
        let p = run(&mut c, &o, &[contact(2, DVec2::new(0.0, -3_000.0), true)], &[], &mods, 10);
        let cur = ShipOrders { route: vec![DVec2::new(5.0, 5.0)], designated: TrackId(3), launches: vec![], ..Default::default() };
        let out = build_out(&p, &cur, Emcon::Burst, DoctrinePreset::Cautious, &o, &sea);
        assert_eq!(out, CaptainOut::default(), "nothing changes for a dropped player's hull");
    }

    #[test]
    fn build_out_keeps_launches_and_avoids_churn() {
        let o = own();
        let mut p = Plan::new(CaptainMode::Attacking);
        p.course = Course::Heading(deg(90.0));
        p.speed = Speed::Flank;
        let cur = ShipOrders { launches: vec![crate::ship::LaunchRequest { slot: PartSlot(3), track: TrackId(2), bearing_rad: None, depth_m: None }], ..Default::default() };
        let out = build_out(&p, &cur, Emcon::Burst, DoctrinePreset::Cautious, &o, &sea);
        let orders = out.orders.expect("changed");
        assert_eq!(orders.launches, cur.launches);
        assert_eq!(orders.route.len(), 1);
        assert!(ang(bearing(o.pos, orders.route[0]), deg(90.0)) < 1e-9);
        // Same plan again: nothing to change.
        let again = build_out(&p, &orders, Emcon::Burst, DoctrinePreset::Cautious, &o, &sea);
        assert_eq!(again.orders, None);
        // Land dead ahead: the route turns away from it.
        let land_east = |q: DVec2| if q.x > 500.0 && q.y.abs() < 3_000.0 { 0.0 } else { 100.0 };
        let out = build_out(&p, &ShipOrders::default(), Emcon::Burst, DoctrinePreset::Cautious, &o, &land_east);
        let w = out.orders.unwrap().route[0];
        assert!(ang(bearing(o.pos, w), deg(90.0)) >= deg(15.0));
    }

    #[test]
    fn hurt_rule() {
        let h = HullHealth { cells_lost: 0.2, weapons_total: 2, weapons_working: 1, propulsion_total: 2, propulsion_working: 2 };
        assert!(!wants_withdraw(&h, Behaviour::Combatant, false));
        assert!(wants_withdraw(&h, Behaviour::Picket, false));
        assert!(!wants_withdraw(&h, Behaviour::Combatant, true));
        assert!(wants_withdraw(&HullHealth { weapons_working: 0, ..h }, Behaviour::Combatant, false));
        assert!(wants_withdraw(&HullHealth { cells_lost: 0.0, propulsion_working: 0, ..h }, Behaviour::Escort, false));
        assert!(!wants_withdraw(&HullHealth { cells_lost: 0.9, ..h }, Behaviour::Static, true));
    }
}
