//! Unit `sys-officer-board`. The Bridge Board, officer radio messages and "Why?" chains.
//! DESIGN.md L351-353, L1664 (Bridge Board per officer: task, evidence-citing reason, next action, control
//! state, override count, radiating emitters, ESM warning; no spinners), L1706 (officers speak as text: two
//! 40-character lines, speaker named at changes), L1666 (message traffic stays inside the alert budget).
//!
//! Decisions where the spec is silent:
//! - Gunnery's control state is Player while the player holds any mount or weapon group; Engineer and Damage
//!   Control have no takeover role and always show Officer;
//! - the Sensors reason is derived from the emission phase, the ship's setting and the picture, because the
//!   emission controller's own reason is not kept in its state;
//! - officers radio at most once per `RADIO_GAP_S` each; events inside the gap go to the log only. Radio
//!   events: new engagement target, load shed starting or ending, emitters coming on, ESM warning, fire, breach
//!   or magazine flooding taken on by Damage Control, evasion starting, last waypoint reached.
use crate::emcon::{EmconPhase, EmconReason};
use crate::graph::{CompiledGraph, WriteOrigin};
use crate::input::ControlState;
use crate::officers::helm::HelmMode;
use crate::prelude::*;
use crate::protocol::command::ControlRole;
use crate::protocol::event::WhyLink;
use crate::protocol::snapshot::{BridgeBoard, OfficerReport};
use crate::ship::{Ship, ShipOrders, ShipView, StageStates};
use crate::tracks::Track;
use content::graph::{NodeKind, OfficerStage, OfficerSettings, RepairPriority, SinkKind};
use content::part::PartFunction;
use sim_core::networks::{LinkState, Networks};
use sim_core::parts::{PartRuntime, PartState, TeamOrder};
use sim_core::time::TICK_HZ;


/// Fewest seconds between two radio messages of one officer.
pub const RADIO_GAP_S: u64 = 4;
/// DECISION (q5): routine repeats go to the log, not the radio, unless this long has passed since the
/// officer last spoke: Sensors' "radiating" for each Burst sweep (every 30 s on the approach), and Gunnery's
/// "engaging" when a mount resumes fire on the track it was holding on (a friend in the line, a refreshing
/// lock). A first engagement of a track after a long hold is still news.
pub const ROUTINE_RADIO_GAP_S: u64 = 90;

fn m(key: &str) -> Msg { Msg::new(key) }
fn finite(v: f64) -> f64 { if v.is_finite() { v } else { 0.0 } }
fn deg(rad: f64) -> f64 { let d = finite(rad) * 180.0 / core::f64::consts::PI; let d = d - 360.0 * libm::floor(d / 360.0); if d >= 360.0 { 0.0 } else { d } }
fn role_index(role: OfficerRole) -> usize { OfficerRole::ALL.iter().position(|&r| r == role).unwrap_or(0) }

/// Everything the board reads, gathered from a ship and its view.
pub(crate) struct BoardIn<'a> {
    pub tick: u64,
    pub stages: &'a StageStates,
    pub control: &'a ControlState,
    pub orders: &'a ShipOrders,
    pub settings: &'a OfficerSettings,
    pub tracks: &'a [Track],
    pub own_pos: DVec3,
    pub parts: &'a [PartRuntime],
    pub networks: &'a Networks,
    pub teams: usize,
    pub can_work: bool,
    pub trigger_range_m: f64,
    /// Control layer and override count per officer, `OfficerRole::ALL` order.
    pub layers: [ControlLayer; 5],
    pub overrides: [u16; 5],
    pub previous: &'a BridgeBoard,
}

/// Rebuild a ship's Bridge Board from its stage states (10 Hz is enough). Every line cites evidence, for
/// example "Holding fire: Suspect track, not inside self-defence range". When the player holds a role the
/// report carries the officer's suggested solution as advisory. Unit: `sys-officer-board`.
pub fn update_board(ship: &Ship, view: &ShipView<'_>) -> BridgeBoard {
    let mut layers = [ControlLayer::default(); 5];
    let mut overrides = [0u16; 5];
    for (i, role) in OfficerRole::ALL.into_iter().enumerate() {
        let ov = crate::officers::tuning::overrides(ship, Some(role));
        let graph_layer = ship.graph.layer_by_role.get(&role).copied().unwrap_or_default();
        layers[i] = ov.iter().map(|o| o.layer).fold(graph_layer, |a, b| a.max(b));
        overrides[i] = ov.len().min(u16::MAX as usize) as u16;
    }
    let input = BoardIn {
        tick: view.ctx.tick.0,
        stages: &ship.stages,
        control: view.control,
        orders: view.orders,
        settings: view.settings,
        tracks: &view.picture.tracks,
        own_pos: view.vessel.kin.pose.pos,
        parts: &view.vessel.parts,
        networks: &view.vessel.networks,
        teams: view.vessel.crew.teams.len(),
        can_work: view.vessel.crew.berth_factor().is_some(),
        trigger_range_m: view.settings.burst_trigger_range_m.unwrap_or(view.ctx.tuning.doctrine.burst_trigger_m),
        layers,
        overrides,
        previous: &ship.board,
    };
    build(&input)
}

pub(crate) fn build(b: &BoardIn<'_>) -> BridgeBoard {
    let held_gun = b.control.held.iter().any(|r| matches!(r, ControlRole::Mount(_) | ControlRole::WeaponGroup(_)));
    let officers_rule = b.control.officers_in_command;
    let holder = |role: OfficerRole| -> ControlHolder {
        let held = match role { OfficerRole::Helm => b.control.holds(ControlRole::Helm), OfficerRole::Sensors => b.control.holds(ControlRole::Sensors), OfficerRole::Gunnery => held_gun, _ => false };
        if held && !officers_rule { ControlHolder::Player } else { ControlHolder::Officer }
    };
    let mut officers = Vec::with_capacity(5);
    for (i, role) in OfficerRole::ALL.into_iter().enumerate() {
        let h = holder(role);
        let (task, reason, next, advisory) = match role {
            OfficerRole::Helm => helm(b),
            OfficerRole::Engineer => engineer(b),
            OfficerRole::Sensors => sensors(b),
            OfficerRole::Gunnery => gunnery(b),
            OfficerRole::DamageControl => damage_control(b),
        };
        officers.push(OfficerReport { role: Some(role), task, reason, next, holder: h, layer: b.layers[i], overrides: b.overrides[i], advisory: if h == ControlHolder::Player { advisory } else { None } });
    }
    let mut radiating: Vec<PartSlot> = b.parts.iter().filter(|p| matches!(p.state, PartState::Sensor { radiating: true, .. } | PartState::Jammer { radiating: true })).map(|p| p.slot).collect();
    radiating.sort();
    let mut board = BridgeBoard { officers, radiating, esm_warning: b.stages.emcon.esm_warning, emcon: b.settings.emcon, radio: b.previous.radio };
    // Stamp the officers that radio this board, so `messages` knows which events to send.
    for (i, role) in OfficerRole::ALL.into_iter().enumerate() {
        let last = b.previous.radio[i];
        let gap = if routine_radio(b.previous, &board, role) { ROUTINE_RADIO_GAP_S } else { RADIO_GAP_S } * TICK_HZ as u64;
        let free = last == 0 || b.tick + 1 >= last + gap;
        if free && radio_event(b.previous, &board, role).is_some() { board.radio[i] = b.tick + 1; }
    }
    board
}

/// A radio event that only repeats what the player already heard (see [`ROUTINE_RADIO_GAP_S`]).
fn routine_radio(prev: &BridgeBoard, cur: &BridgeBoard, role: OfficerRole) -> bool {
    match role {
        OfficerRole::Sensors => !(cur.esm_warning && !prev.esm_warning) && cur.emcon == prev.emcon,
        OfficerRole::Gunnery => {
            let (Some(p), Some(c)) = (report(prev, role), report(cur, role)) else { return false };
            p.task.key.0 == "officer.gunnery.task.holding" && c.task.key.0 == "officer.gunnery.task.engaging" && p.task.args.first() == c.task.args.first()
        }
        _ => false,
    }
}

// ---- Helm ----

fn inbound_threat<'a>(tracks: &'a [Track], own: DVec3) -> Option<&'a Track> {
    tracks.iter().filter(|t| t.is_live() && t.inbound && t.domain == Domain::Weapon)
        .min_by(|a, b| (a.pos - own).length_squared().total_cmp(&(b.pos - own).length_squared()).then(a.id.cmp(&b.id)))
}

fn helm(b: &BoardIn<'_>) -> (Msg, Msg, Msg, Option<Msg>) {
    let h = &b.stages.helm;
    let course = deg(h.ordered_heading_rad);
    let speed = finite(h.ordered_speed_kn);
    let own2 = DVec2::new(b.own_pos.x, b.own_pos.z);
    let route = &b.orders.route;
    let (task, reason, next) = match h.mode {
        HelmMode::FollowRoute if !route.is_empty() => {
            let wp = (h.waypoint as usize).min(route.len() - 1);
            let dist = finite((route[wp] - own2).length());
            let next = if wp + 1 < route.len() { m("officer.helm.next.waypoint").arg(MsgArg::Int(wp as i64 + 2)) } else { m("officer.helm.next.last") };
            (m("officer.helm.task.route").arg(MsgArg::Int(wp as i64 + 1)).arg(MsgArg::Int(route.len() as i64)), m("officer.helm.reason.waypoint").arg(MsgArg::Int(wp as i64 + 1)).num(dist, Unit::Metres), next)
        }
        HelmMode::Station => {
            let dist = b.orders.station.map(|(p, _)| finite((p - own2).length())).unwrap_or(0.0);
            (m("officer.helm.task.station"), m("officer.helm.reason.station").num(dist, Unit::Metres), m("officer.helm.next.station"))
        }
        HelmMode::Formation => (m("officer.helm.task.formation"), m("officer.helm.reason.formation"), m("officer.helm.next.station")),
        HelmMode::Evading => {
            let reason = match inbound_threat(b.tracks, b.own_pos) {
                Some(t) if t.pos.y < -1.0 => m("officer.helm.reason.torpedo").arg(MsgArg::Track(t.id)),
                Some(t) => m("officer.helm.reason.threat").arg(MsgArg::Track(t.id)),
                None => m("officer.helm.reason.evade_ordered"),
            };
            (m("officer.helm.task.evading"), reason, m("officer.helm.next.resume"))
        }
        HelmMode::Stopped => (m("officer.helm.task.stopped"), m("officer.helm.reason.stop"), m("officer.helm.next.station")),
        HelmMode::HoldHeading | HelmMode::FollowRoute => (
            m("officer.helm.task.hold_heading").num(course, Unit::Degrees).num(speed, Unit::Knots),
            m("officer.helm.reason.ordered").num(course, Unit::Degrees).num(speed, Unit::Knots),
            m("officer.helm.next.hold").num(course, Unit::Degrees),
        ),
    };
    (task, reason, next, Some(m("officer.helm.advisory").num(course, Unit::Degrees).num(speed, Unit::Knots)))
}

// ---- Engineer ----

fn engineer(b: &BoardIn<'_>) -> (Msg, Msg, Msg, Option<Msg>) {
    let e = &b.stages.engineer;
    let (gen, demand) = (finite(e.generation_kw).max(0.0), finite(e.demand_kw).max(0.0));
    let shed = e.shed.len();
    let (task, reason) = if shed > 0 {
        (m("officer.engineer.task.shedding").arg(MsgArg::Int(shed as i64)), m("officer.engineer.reason.short").num(demand, Unit::Kilowatts).num(gen, Unit::Kilowatts))
    } else {
        (m("officer.engineer.task.supplying"), m("officer.engineer.reason.balance").num(gen, Unit::Kilowatts).num(demand, Unit::Kilowatts))
    };
    let fuel = finite(e.fuel_fraction).clamp(0.0, 1.0);
    let flow = finite(b.networks.fuel_flow_lps);
    let next = if shed > 0 {
        m("officer.engineer.next.restore").num(gen, Unit::Kilowatts)
    } else if fuel < crate::alerts::FUEL_LOW_FRACTION {
        m("officer.engineer.next.fuel_low").num(fuel * 100.0, Unit::Percent)
    } else if flow > 1e-6 && b.networks.fuel_l > 0.0 {
        m("officer.engineer.next.endurance").num(finite(b.networks.fuel_l / flow / 3600.0), Unit::Hours)
    } else {
        m("officer.engineer.next.standby")
    };
    (task, reason, next, None)
}

// ---- Sensors ----

/// The hull is inside the emission controller's under-fire window (`crate::emcon`: shellfire is a weapon
/// inbound, for [`crate::emcon::SELF_DEFENCE_S`] after the last shell).
fn under_fire(e: &crate::emcon::EmconState, tick: u64) -> bool {
    e.under_fire_tick.is_some_and(|t| t <= tick && (tick - t) as f64 <= crate::emcon::SELF_DEFENCE_S * sim_core::TICK_HZ as f64)
}

/// The emission reason, rebuilt from the phase, the setting and the picture.
fn emcon_reason(b: &BoardIn<'_>) -> EmconReason {
    let e = &b.stages.emcon;
    match (b.settings.emcon, e.phase) {
        (Emcon::Silent, EmconPhase::PointDefenceOnly) if e.self_defence_only && under_fire(e, b.tick) => EmconReason::SelfDefenceUnderFire,
        (_, EmconPhase::PointDefenceOnly) => EmconReason::PointDefenceException,
        (Emcon::Silent, _) => EmconReason::SilentOrdered,
        (Emcon::Active, _) => EmconReason::ActiveOrdered,
        (Emcon::Burst, EmconPhase::Quiet) => EmconReason::PassiveNoTrigger,
        (Emcon::Burst, EmconPhase::Continuous) => EmconReason::HoldingLockWeaponsInFlight,
        (Emcon::Burst, EmconPhase::Bursting) => {
            if inbound_threat(b.tracks, b.own_pos).is_some() || under_fire(e, b.tick) { return EmconReason::BurstWeaponInbound; }
            let near = b.tracks.iter().filter(|t| t.is_live() && t.domain != Domain::Weapon && t.stance >= Stance::Suspect)
                .map(|t| (t, finite((t.pos - b.own_pos).length()))).filter(|(_, r)| *r <= b.trigger_range_m)
                .min_by(|a, b| a.1.total_cmp(&b.1).then(a.0.id.cmp(&b.0.id)));
            match near {
                Some((t, r)) => EmconReason::BurstTrackInside { track: t.id, range_m: r },
                None if b.stages.engage.weapons_in_flight > 0 => EmconReason::HoldingLockWeaponsInFlight,
                None => EmconReason::BurstRefreshForWeapon,
            }
        }
    }
}

fn sensors(b: &BoardIn<'_>) -> (Msg, Msg, Msg, Option<Msg>) {
    let live = b.tracks.iter().filter(|t| t.is_live() && t.stance != Stance::Friend);
    let (n, hostile) = live.fold((0i64, 0i64), |(n, h), t| (n + 1, h + (t.stance == Stance::Hostile) as i64));
    let task = if n == 0 { m("officer.sensors.task.no_contacts") } else { m("officer.sensors.task.tracking").arg(MsgArg::Int(n)).arg(MsgArg::Int(hostile)) };
    let reason = crate::text::emcon_reason(&emcon_reason(b));
    let e = &b.stages.emcon;
    let next = match (b.settings.emcon, e.phase) {
        (Emcon::Silent, _) => m("officer.sensors.next.silent"),
        (Emcon::Active, _) => m("officer.sensors.next.radiate"),
        (Emcon::Burst, EmconPhase::Continuous) => m("officer.sensors.next.hold_lock"),
        (Emcon::Burst, EmconPhase::Bursting | EmconPhase::PointDefenceOnly) if e.next_burst_tick > b.tick => m("officer.sensors.next.burst_in").num(e.next_burst_tick.saturating_sub(b.tick) as f64 / TICK_HZ as f64, Unit::Seconds),
        (Emcon::Burst, _) => m("officer.sensors.next.burst_trigger").num(finite(b.trigger_range_m), Unit::Metres),
    };
    (task, reason, next, Some(m("officer.sensors.advisory").arg(MsgArg::Emcon(b.settings.emcon))))
}

// ---- Gunnery ----

fn gunnery(b: &BoardIn<'_>) -> (Msg, Msg, Msg, Option<Msg>) {
    let g = &b.stages.engage;
    let top = g.ranked.first();
    let advisory = Some(match top { Some(t) => m("officer.gunnery.advisory").arg(MsgArg::Track(t.track)).num(finite(t.range_m), Unit::Metres), None => m("officer.gunnery.advisory_none") });
    let next_target = |not: TrackId| g.ranked.iter().find(|r| r.track != not).map(|r| m("officer.gunnery.next.next_target").arg(MsgArg::Track(r.track)));
    if g.mounts.is_empty() {
        return (m("officer.gunnery.task.no_mounts"), m("officer.gunnery.reason.no_mounts"), m("officer.gunnery.next.none"), None);
    }
    let mut engaged: Vec<(TrackId, FireReason)> = g.mounts.iter().filter_map(|me| match me.status { EngageStatus::Engaging { track, reason } => Some((track, reason)), _ => None }).collect();
    engaged.sort_by_key(|e| e.0);
    if let Some(&(track, reason)) = engaged.first() {
        let distinct = { let mut t: Vec<TrackId> = engaged.iter().map(|e| e.0).collect(); t.dedup(); t.len() };
        let task = if distinct > 1 { m("officer.gunnery.task.engaging_many").arg(MsgArg::Int(distinct as i64)) } else { m("officer.gunnery.task.engaging").arg(MsgArg::Track(track)) };
        // The most urgent reason: point defence, then the rest in mount order.
        let reason = engaged.iter().find(|e| e.1 == FireReason::PointDefence).map(|e| e.1).unwrap_or(reason);
        return (task, crate::text::fire_reason(&reason), next_target(track).unwrap_or_else(|| m("officer.gunnery.next.continue")), advisory);
    }
    if b.orders.weapons_tight {
        return (m("officer.gunnery.task.weapons_tight"), m("officer.gunnery.reason.weapons_tight"), m("officer.gunnery.next.wait"), advisory);
    }
    let holding = g.mounts.iter().find_map(|me| match me.status { EngageStatus::Holding { track, reason } if !track.is_none() => Some((track, reason)), _ => None });
    if let Some((track, reason)) = holding {
        let next = match reason {
            HoldReason::OutOfRange { limit_m, .. } | HoldReason::SuspectOutsideSelfDefence { limit_m, .. } => m("officer.gunnery.next.in_range").num(finite(limit_m), Unit::Metres),
            _ => m("officer.gunnery.next.wait"),
        };
        return (m("officer.gunnery.task.holding").arg(MsgArg::Track(track)), crate::text::hold_reason(&reason), next, advisory);
    }
    let contacts = b.tracks.iter().filter(|t| t.is_live() && t.stance != Stance::Friend).count() as i64;
    let reason = if contacts == 0 { m("officer.gunnery.reason.none_ranked") } else { m("officer.gunnery.reason.none_hostile").arg(MsgArg::Int(contacts)) };
    (m("officer.gunnery.task.no_target"), reason, m("officer.gunnery.next.engage_first"), advisory)
}

// ---- Damage Control ----

fn dc_job(o: TeamOrder, cat: RepairPriority) -> Msg {
    match o {
        TeamOrder::FightFire(c) if cat == RepairPriority::MagazineFires => m("officer.dc.reason.fire_magazine").arg(MsgArg::Compartment(c)),
        TeamOrder::FightFire(c) => m("officer.dc.reason.fire").arg(MsgArg::Compartment(c)),
        TeamOrder::Patch { compartment } => m("officer.dc.reason.breach").arg(MsgArg::Compartment(compartment)),
        TeamOrder::JuryRig { conduit } if cat == RepairPriority::ConduitsToCritical => m("officer.dc.reason.conduit_critical").arg(MsgArg::Int(conduit as i64 + 1)),
        TeamOrder::JuryRig { conduit } => m("officer.dc.reason.conduit").arg(MsgArg::Int(conduit as i64 + 1)),
        TeamOrder::Repair(s) => m("officer.dc.reason.repair").arg(MsgArg::Part(s)),
        TeamOrder::Idle => m("officer.dc.reason.no_damage"),
    }
}

fn damage_control(b: &BoardIn<'_>) -> (Msg, Msg, Msg, Option<Msg>) {
    let dc = &b.stages.dc;
    let teams = b.teams.max(dc.plan.len());
    let flooding = b.parts.iter().find(|p| dc.auto_flood.contains(&p.slot) && !matches!(p.state, PartState::Magazine { flooded: true, .. }));
    let task = if !b.can_work { m("officer.dc.task.no_teams") } else if dc.plan.is_empty() { m("officer.dc.task.standing_by").arg(MsgArg::Int(teams as i64)) } else { m("officer.dc.task.working").arg(MsgArg::Int(dc.plan.len() as i64)).arg(MsgArg::Int(teams as i64)) };
    let reason = match (flooding, dc.plan.first()) {
        (Some(p), _) => m("officer.dc.reason.flooding_magazine").arg(MsgArg::Part(p.slot)),
        (None, Some(&(o, cat))) => dc_job(o, cat),
        (None, None) => m("officer.dc.reason.no_damage"),
    };
    let pumping = b.parts.iter().find_map(|p| match p.state { PartState::Pump { running: true, target } => Some(target.or(p.compartment)), _ => None }).flatten();
    let next = match pumping {
        Some(c) => m("officer.dc.next.pumping").arg(MsgArg::Compartment(c)),
        None if dc.plan.is_empty() => m("officer.dc.next.stand_by"),
        None => m("officer.dc.next.list"),
    };
    (task, reason, next, None)
}

// ---- Radio ----

fn report(board: &BridgeBoard, role: OfficerRole) -> Option<&OfficerReport> { board.officers.iter().find(|r| r.role == Some(role)) }

/// The radio line an officer sends for the change from `prev` to `cur`, if the change is one the player should
/// hear. Pure: the same pair always gives the same answer.
fn radio_event(prev: &BridgeBoard, cur: &BridgeBoard, role: OfficerRole) -> Option<Msg> {
    let (p, c) = (report(prev, role), report(cur, role)?);
    let key = |r: Option<&OfficerReport>, f: fn(&OfficerReport) -> &Msg| r.map(|r| f(r).key.0.clone()).unwrap_or_default();
    fn task(r: &OfficerReport) -> &Msg { &r.task }
    fn reason(r: &OfficerReport) -> &Msg { &r.reason }
    let first = |msg: &Msg| msg.args.first().cloned();
    match role {
        OfficerRole::Gunnery => {
            let ck = c.task.key.0.as_str();
            if ck == "officer.gunnery.task.engaging" && (key(p, task) != ck || p.map(|p| first(&p.task)) != Some(first(&c.task))) {
                return Some(m("officer.radio.gunnery.engaging").arg(first(&c.task)?));
            }
            None
        }
        OfficerRole::Engineer => {
            let (was, is) = (key(p, task) == "officer.engineer.task.shedding", c.task.key.0 == "officer.engineer.task.shedding");
            match (was, is) {
                (false, true) => Some(m("officer.radio.engineer.shedding").arg(first(&c.task)?)),
                (true, false) => Some(m("officer.radio.engineer.restored")),
                _ => None,
            }
        }
        OfficerRole::Sensors => {
            if cur.esm_warning && !prev.esm_warning { return Some(m("officer.radio.sensors.esm")); }
            if !cur.radiating.is_empty() && prev.radiating.is_empty() { return Some(m("officer.radio.sensors.radiating").arg(MsgArg::Int(cur.radiating.len() as i64))); }
            None
        }
        OfficerRole::DamageControl => {
            let changed = p.map(|p| p.reason != c.reason).unwrap_or(true);
            if !changed { return None; }
            match c.reason.key.0.as_str() {
                "officer.dc.reason.fire_magazine" | "officer.dc.reason.fire" => Some(m("officer.radio.dc.fire").arg(first(&c.reason)?)),
                "officer.dc.reason.breach" => Some(m("officer.radio.dc.breach").arg(first(&c.reason)?)),
                "officer.dc.reason.flooding_magazine" => Some(m("officer.radio.dc.flood_magazine").arg(first(&c.reason)?)),
                _ => None,
            }
        }
        OfficerRole::Helm => {
            let ck = c.task.key.0.as_str();
            if ck == "officer.helm.task.evading" && key(p, task) != ck {
                return Some(match c.reason.key.0.as_str() {
                    "officer.helm.reason.threat" | "officer.helm.reason.torpedo" => m("officer.radio.helm.evading").arg(first(&c.reason)?),
                    _ => m("officer.radio.helm.evading_ordered"),
                });
            }
            if key(p, task) == "officer.helm.task.route" && key(p, reason) == "officer.helm.reason.waypoint" && ck == "officer.helm.task.hold_heading"
                && p.map(|p| p.next.key.0 == "officer.helm.next.last").unwrap_or(false) {
                return Some(m("officer.radio.helm.route_done"));
            }
            None
        }
    }
}

/// Radio messages an officer sends when its task changes in a way the player should hear (new engagement,
/// load shed, fire reported, emitters on). Rate-limited per officer; routine changes go to the log only.
/// Unit: `sys-officer-board`.
///
/// `current` must come from [`update_board`] with `previous` as the ship's board at that time: the board
/// stamps which officers may radio, so the limit holds without other state.
pub fn messages(previous: &BridgeBoard, current: &BridgeBoard, ship: &Ship, tick: u64) -> Vec<(OfficerRole, Msg)> {
    let _ = ship;
    radio(previous, current, tick)
}

pub(crate) fn radio(previous: &BridgeBoard, current: &BridgeBoard, tick: u64) -> Vec<(OfficerRole, Msg)> {
    let mut out = Vec::new();
    for (i, role) in OfficerRole::ALL.into_iter().enumerate() {
        let stamp = current.radio[i];
        if stamp == 0 || stamp == previous.radio[i] || stamp > tick + 1 { continue; }
        if let Some(msg) = radio_event(previous, current, role) { out.push((role, msg)); }
    }
    out
}

// ---- Why? ----

fn node_for(graph: &CompiledGraph, stage: OfficerStage, part: Option<PartSlot>) -> Option<GraphNodeId> {
    let is = |n: &&crate::graph::CompiledNode| matches!(n.kind, NodeKind::Stage(s) if s == stage);
    graph.nodes.iter().filter(is).find(|n| part.is_some() && n.part == part).or_else(|| graph.nodes.iter().filter(is).find(|n| n.part.is_none())).or_else(|| graph.nodes.iter().find(is)).map(|n| n.id)
}

fn link(text: Msg, node: Option<GraphNodeId>, part: Option<PartSlot>) -> WhyLink { WhyLink { text, node, part } }

/// "Why?" on a part: the chain of causes from graph state, each link naming the responsible node, for example
/// "Gun 2 idle: solution needs +74 degrees, mount maximum is +70." Unit: `sys-officer-board`.
pub fn why(ship: &Ship, view: &ShipView<'_>, slot: PartSlot) -> Vec<WhyLink> {
    let func = view.vessel.parts.iter().find(|p| p.slot == slot).and_then(|p| view.ctx.catalogue.parts.get_index(p.def.index())).map(|(_, d)| d.function.clone());
    let w = WhyIn { graph: &ship.graph, sinks: &ship.sinks, stages: &ship.stages, control: view.control, parts: &view.vessel.parts, tracks: &view.picture.tracks, settings: view.settings, function: func, own_pos: view.vessel.kin.pose.pos, tick: view.ctx.tick.0, trigger_range_m: view.settings.burst_trigger_range_m.unwrap_or(view.ctx.tuning.doctrine.burst_trigger_m) };
    why_chain(&w, slot)
}

pub(crate) struct WhyIn<'a> {
    pub graph: &'a CompiledGraph,
    pub sinks: &'a crate::graph::SinkWrites,
    pub stages: &'a StageStates,
    pub control: &'a ControlState,
    pub parts: &'a [PartRuntime],
    pub tracks: &'a [Track],
    pub settings: &'a OfficerSettings,
    pub function: Option<PartFunction>,
    pub own_pos: DVec3,
    pub tick: u64,
    pub trigger_range_m: f64,
}

fn origin_link(origin: WriteOrigin, slot: PartSlot) -> Option<WhyLink> {
    match origin {
        WriteOrigin::Officer(_) => None,
        WriteOrigin::Rule(i) => Some(link(m("officer.why.rule").arg(MsgArg::Int(i as i64 + 1)), None, Some(slot))),
        WriteOrigin::Pinned(n) => Some(link(m("officer.why.pinned"), Some(n), Some(slot))),
        WriteOrigin::Replacement(_) => Some(link(m("officer.why.replacement"), None, Some(slot))),
        WriteOrigin::Script(n) => Some(link(m("officer.why.script"), Some(n), Some(slot))),
    }
}

pub(crate) fn why_chain(w: &WhyIn<'_>, slot: PartSlot) -> Vec<WhyLink> {
    let mut chain = Vec::new();
    let Some(p) = w.parts.iter().find(|p| p.slot == slot) else {
        return vec![link(m("officer.why.no_part").arg(MsgArg::Int(slot.0 as i64)), None, None)];
    };
    let at = Some(slot);
    let part = MsgArg::Part(slot);
    let shedder = node_for(w.graph, OfficerStage::LoadShedder, at);
    // Physical state first: it overrides everything the graph wants.
    if !p.works() {
        chain.push(link(m("officer.why.destroyed").arg(part), None, at));
        return chain;
    }
    if p.level == DamageLevel::Heavy { chain.push(link(m("officer.why.heavy").arg(part.clone()), None, at)); }
    if p.links.shed {
        chain.push(link(m("officer.why.shed").arg(part.clone()).arg(MsgArg::Int(p.priority as i64)), shedder, at));
        let e = &w.stages.engineer;
        chain.push(link(m("officer.why.power_short").num(finite(e.generation_kw), Unit::Kilowatts).num(finite(e.demand_kw), Unit::Kilowatts), shedder, None));
        if let Some(l) = w.sinks.get(SinkKind::PowerPriority(slot)).and_then(|s| origin_link(s.origin, slot)) { chain.push(l); }
    } else if p.links.power == LinkState::Dead && p.priority > 0 {
        chain.push(link(m("officer.why.no_power").arg(part.clone()), None, at));
    } else if p.links.power == LinkState::Buffer {
        chain.push(link(m("officer.why.buffer").arg(part.clone()).num(finite(p.links.battery_s).max(0.0), Unit::Seconds), None, at));
    }
    if p.links.fuel == LinkState::Dead && matches!(w.function, Some(PartFunction::Engine(_) | PartFunction::Generator { .. })) {
        chain.push(link(m("officer.why.no_fuel").arg(part.clone()), None, at));
    }
    if p.links.data == LinkState::LocalControl { chain.push(link(m("officer.why.local_control").arg(part.clone()), None, at)); }

    // Weapon mounts: the engagement status and its cause.
    if let Some(me) = w.stages.engage.mounts.iter().find(|me| me.slot == slot) {
        let assign = node_for(w.graph, OfficerStage::MountAssignment, at);
        let solver = node_for(w.graph, OfficerStage::BallisticSolver, at);
        let held = w.control.held.iter().any(|r| *r == ControlRole::Mount(slot));
        if held { chain.push(link(m("officer.why.player_holds").arg(part.clone()), None, at)); }
        match me.status {
            EngageStatus::Engaging { track, reason } => {
                chain.push(link(m("officer.why.engaging").arg(part.clone()).arg(MsgArg::Track(track)), assign, at));
                chain.push(link(crate::text::fire_reason(&reason), assign, at));
            }
            EngageStatus::Holding { track, reason } => {
                let head = if track.is_none() { m("officer.why.idle").arg(part.clone()) } else { m("officer.why.holding").arg(part.clone()).arg(MsgArg::Track(track)) };
                chain.push(link(head, assign, at));
                let node = match reason { HoldReason::TrainLimit { .. } | HoldReason::ElevationLimit { .. } | HoldReason::NoSolution | HoldReason::Masked => solver.or(assign), _ => assign };
                chain.push(link(crate::text::hold_reason(&reason), node, at));
                let t = w.tracks.iter().find(|t| t.id == track);
                let fusion = node_for(w.graph, OfficerStage::TrackFusion, None);
                match (reason, t) {
                    (HoldReason::Stance { .. }, Some(t)) => chain.push(link(crate::text::stance_reason(t.stance_reason), fusion, None)),
                    (HoldReason::Id { .. }, Some(t)) => chain.push(link(crate::text::rung_reason(t.rung_reason), fusion, None)),
                    (HoldReason::EmconSilent, _) => {
                        let b = BoardIn { tick: w.tick, stages: w.stages, control: w.control, orders: &ShipOrders::default(), settings: w.settings, tracks: w.tracks, own_pos: w.own_pos, parts: w.parts, networks: &Networks::default(), teams: 0, can_work: true, trigger_range_m: w.trigger_range_m, layers: [ControlLayer::default(); 5], overrides: [0; 5], previous: &BridgeBoard::default() };
                        chain.push(link(crate::text::emcon_reason(&emcon_reason(&b)), node_for(w.graph, OfficerStage::EmissionController, None), None));
                    }
                    (HoldReason::PermissionWithheld, _) => {
                        if let Some(l) = w.sinks.get(SinkKind::MountFirePermission(slot)).and_then(|s| origin_link(s.origin, slot)) { chain.push(l); }
                    }
                    _ => {}
                }
            }
            EngageStatus::Idle => chain.push(link(m("officer.why.idle").arg(part.clone()), assign, at)),
        }
        return chain;
    }

    // Other parts by function.
    match (&w.function, &p.state) {
        (_, PartState::Sensor { radiating, .. }) | (_, PartState::Jammer { radiating }) => {
            let emitter = node_for(w.graph, OfficerStage::EmissionController, None);
            chain.push(link(m(if *radiating { "officer.why.radiating" } else { "officer.why.silent" }).arg(part.clone()), emitter, at));
            if let Some(l) = w.sinks.get(SinkKind::EmissionState(slot)).and_then(|s| origin_link(s.origin, slot)) { chain.push(l); } else {
                let b = BoardIn { tick: w.tick, stages: w.stages, control: w.control, orders: &ShipOrders::default(), settings: w.settings, tracks: w.tracks, own_pos: w.own_pos, parts: w.parts, networks: &Networks::default(), teams: 0, can_work: true, trigger_range_m: w.trigger_range_m, layers: [ControlLayer::default(); 5], overrides: [0; 5], previous: &BridgeBoard::default() };
                chain.push(link(crate::text::emcon_reason(&emcon_reason(&b)), emitter, None));
            }
        }
        (_, PartState::Pump { running, target }) => {
            let planner = node_for(w.graph, OfficerStage::DamageControlPlanner, None);
            let manual = w.stages.dc.manual_pumps.iter().any(|&(s, _, _)| s == slot);
            let text = match (running, target.or(p.compartment)) {
                (true, Some(c)) => m("officer.why.pump_running").arg(part.clone()).arg(MsgArg::Compartment(c)),
                _ => m("officer.why.pump_idle").arg(part.clone()),
            };
            chain.push(link(text, planner, at));
            if manual { chain.push(link(m("officer.why.manual"), None, at)); } else if let Some(l) = w.sinks.get(SinkKind::Pump(slot)).and_then(|s| origin_link(s.origin, slot)) { chain.push(l); }
        }
        (_, PartState::Door { closed }) => {
            let planner = node_for(w.graph, OfficerStage::DamageControlPlanner, None);
            chain.push(link(m(if *closed { "officer.why.door_closed" } else { "officer.why.door_open" }).arg(part.clone()), planner, at));
            if w.stages.dc.manual_doors.iter().any(|&(s, _)| s == slot) { chain.push(link(m("officer.why.manual"), None, at)); }
        }
        (_, PartState::Magazine { flooded: true, .. }) => {
            let planner = node_for(w.graph, OfficerStage::DamageControlPlanner, None);
            let by = if w.stages.dc.manual_magazine_floods.contains(&slot) { m("officer.why.manual") } else { m("officer.why.magazine_autoflood") };
            chain.push(link(m("officer.why.magazine_flooded").arg(part.clone()), planner, at));
            chain.push(link(by, planner, at));
        }
        _ => {}
    }
    // A team at work on the part.
    if let Some(&(o, _)) = w.stages.dc.plan.iter().find(|(o, _)| *o == TeamOrder::Repair(slot)) {
        chain.push(link(dc_job(o, RepairPriority::PartRepairs), node_for(w.graph, OfficerStage::DamageControlPlanner, None), at));
    }
    if chain.is_empty() { chain.push(link(m("officer.why.working").arg(part), None, at)); }
    chain
}

/// Every `officer.*` key this unit can emit (`game/i18n/officers.csv`). Unit: `sys-officer-board`.
pub fn text_keys() -> Vec<&'static str> { KEYS.to_vec() }

const KEYS: &[&str] = &[
    "officer.helm.task.hold_heading", "officer.helm.task.route", "officer.helm.task.station", "officer.helm.task.formation",
    "officer.helm.task.evading", "officer.helm.task.stopped", "officer.helm.reason.ordered", "officer.helm.reason.waypoint",
    "officer.helm.reason.station", "officer.helm.reason.formation", "officer.helm.reason.threat", "officer.helm.reason.torpedo",
    "officer.helm.reason.evade_ordered", "officer.helm.reason.stop", "officer.helm.next.waypoint", "officer.helm.next.last",
    "officer.helm.next.hold", "officer.helm.next.resume", "officer.helm.next.station", "officer.helm.advisory",
    "officer.engineer.task.supplying", "officer.engineer.task.shedding", "officer.engineer.reason.balance",
    "officer.engineer.reason.short", "officer.engineer.next.endurance", "officer.engineer.next.restore",
    "officer.engineer.next.fuel_low", "officer.engineer.next.standby",
    "officer.sensors.task.tracking", "officer.sensors.task.no_contacts", "officer.sensors.next.burst_in",
    "officer.sensors.next.burst_trigger", "officer.sensors.next.silent", "officer.sensors.next.radiate",
    "officer.sensors.next.hold_lock", "officer.sensors.advisory",
    "officer.gunnery.task.engaging", "officer.gunnery.task.engaging_many", "officer.gunnery.task.holding",
    "officer.gunnery.task.weapons_tight", "officer.gunnery.task.no_target", "officer.gunnery.task.no_mounts",
    "officer.gunnery.reason.weapons_tight", "officer.gunnery.reason.none_ranked", "officer.gunnery.reason.none_hostile",
    "officer.gunnery.reason.no_mounts", "officer.gunnery.next.next_target", "officer.gunnery.next.continue",
    "officer.gunnery.next.in_range", "officer.gunnery.next.wait", "officer.gunnery.next.engage_first", "officer.gunnery.next.none",
    "officer.gunnery.advisory", "officer.gunnery.advisory_none",
    "officer.dc.task.working", "officer.dc.task.standing_by", "officer.dc.task.no_teams", "officer.dc.reason.fire_magazine",
    "officer.dc.reason.fire", "officer.dc.reason.breach", "officer.dc.reason.conduit_critical", "officer.dc.reason.conduit",
    "officer.dc.reason.repair", "officer.dc.reason.flooding_magazine", "officer.dc.reason.no_damage", "officer.dc.next.pumping",
    "officer.dc.next.stand_by", "officer.dc.next.list",
    "officer.radio.helm.evading", "officer.radio.helm.evading_ordered", "officer.radio.helm.route_done",
    "officer.radio.engineer.shedding", "officer.radio.engineer.restored", "officer.radio.sensors.radiating",
    "officer.radio.sensors.esm", "officer.radio.gunnery.engaging", "officer.radio.gunnery.struck", "officer.radio.dc.fire", "officer.radio.dc.breach",
    "officer.radio.dc.flood_magazine",
    "officer.why.no_part", "officer.why.destroyed", "officer.why.heavy", "officer.why.shed", "officer.why.power_short",
    "officer.why.no_power", "officer.why.buffer", "officer.why.no_fuel", "officer.why.local_control", "officer.why.player_holds",
    "officer.why.engaging", "officer.why.holding", "officer.why.idle", "officer.why.rule", "officer.why.pinned",
    "officer.why.replacement", "officer.why.script", "officer.why.radiating", "officer.why.silent", "officer.why.pump_running",
    "officer.why.pump_idle", "officer.why.manual", "officer.why.door_closed", "officer.why.door_open",
    "officer.why.magazine_flooded", "officer.why.magazine_autoflood", "officer.why.working",
];

#[cfg(test)]
#[path = "board_tests.rs"]
mod tests;
