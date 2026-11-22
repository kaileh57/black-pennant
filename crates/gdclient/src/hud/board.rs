//! Unit `gd-hud-board`. The Bridge Board on `BpHud`: one group per officer with task, reason, next action,
//! control state and override count, plus the in-sortie view of the control ladder (L0 to L2).
//! DESIGN.md L351-353 (Bridge Board, A chips, "Why?"), L1664-1665 (HUD rules 8 and 9), L322-336 (ladder),
//! L505-514 (emission control). No spinners, always an evidence-citing reason.
//!
//! # Where the live values come from
//! The snapshot carries the board itself (officer reports, radiating emitters, ESM warning, the EMCON in
//! force, held roles) but not the ship's live settings. Those start as the own design's stored settings
//! (`ShipDesign::settings`, `ShipDesign::rules`) and follow the server through [`BoardBook`]:
//! - `OverrideChanged` events say which value was overridden or reverted;
//! - `QueryReply::Overrides` lists, per officer, every value that differs from the design with its current
//!   value (preset, mount presets, effective sliders, EMCON, both ranges, which doctrine rows differ).
//!
//! The book asks for an officer's list itself when an `OverrideChanged` of that officer arrives (every
//! officer's for a ship preset, which moves sliders of three officers), at most every 0.2 s per officer, and
//! when the board's override count for an officer disagrees with the last list (a rejoin, or an override
//! the client missed), at most every 2 s per officer. A value changed again inside that gap shows the
//! previous reply's value until the next reply, one round trip later. The reply also reaches `overrides_reply`, so a diff panel that is
//! open stays current.
//!
//! Decisions where the contract is silent:
//! - An officer missing from the snapshot's board (before its first 10 Hz board) reports empty sentences
//!   and holder Officer, so the board always has five groups in `Bp.OFFICER_*` order.
//! - `takeable_roles` lists the helm, each weapons group of the design in group order, then sensors when
//!   the design has a sensor. Groups 1 to 4 name their `take_group_N` action; later groups name none
//!   (`&""`): they are reached with `group_next` and `group_prev`.
//! - `role_holder` of a mount is Player when the mount or its weapons group is held.
//! - `preset(mount)` of a mount without its own preset reports the ship preset it inherits, not overridden.
//! - `doctrine` resolves inheritance as `sim_systems::doctrine::resolve` does, from the live presets and the
//!   design's doctrine entries: the ship level lists one row per (weapon class on the ship, target
//!   category); a mount lists its own weapon's rows. Rows the server reports as differing from the design are
//!   marked overridden. A doctrine row or rules table changed in the sortie is not reported by the protocol
//!   (no event and no value); the rows show the design's entries until the protocol carries them.
//! - `rules` marks `active` from the newest snapshot with the operand definitions of `graph::library`
//!   (own ship facts, the side picture filtered as `TrackFilter` does, part health and ammunition); an
//!   operand the snapshot cannot give (ammunition of a decoy launcher or mine rail) makes its comparison
//!   unknown, and an unknown condition is not active, as the graph's Select treats it.
//! - `emcon.reason` is the Sensors officer's reason; `recommended` is the operation's recommendation, or
//!   Silent when one of its ghost conditions forbids emissions (the briefing's rule).
//! - `max_layer` is the ruleset's highest layer capped at L2 Rules: PvP sorties use the standard ruleset,
//!   every other mode the open one.
use super::{BpHud, Out, Section};
use crate::convert::{d, doctrine_entry_dict, empty_msg, msg_dict, role_dict, rule_dict, slider_dict};
use crate::enums::GdEnum;
use crate::services::services;
use content::enums::{ControlLayer, DoctrinePreset, Emcon, GameMode, OfficerRole, Stance};
use content::graph::{Cmp, Condition, DoctrineEntry, Operand, OfficerSettings, Rule, SinkKind, TargetCategory, TrackFilter, Value, WeaponClass};
use content::ids::{PartSlot, PlayerSlot};
use godot::prelude::*;
use sim_core::EntityId;
use sim_systems::protocol::event::{OverrideView, QueryReply, SimEvent};
use sim_systems::protocol::{Command, ControlHolder, ControlRole, Msg, MsgArg, OfficerReport, OverrideKey, PartReadout, Query, ShipStatus, Snapshot};
use std::collections::BTreeMap;

/// Text keys this unit emits (rows in `game/i18n/log.csv`).
pub(crate) const TEXT_KEYS: &[&str] = &["hud.role.helm", "hud.role.sensors"];

/// Least time between two override queries for one officer, seconds of wall time: after an override event,
/// and when only the board's count disagrees.
const EVENT_GAP_S: f64 = 0.2;
const POLL_GAP_S: f64 = 2.0;
/// A board count is compared with a reply only once the snapshot is this many ticks newer than the reply.
const SETTLE_TICKS: u64 = 30;
/// Weapons groups with their own take key (`take_group_1` to `take_group_4`).
const KEYED_GROUPS: u8 = 4;

fn role_index(r: OfficerRole) -> usize { OfficerRole::ALL.iter().position(|x| *x == r).unwrap_or(0) }

/// The officer whose overrides list holds a key (as `tuning::settings_diff` files them).
pub(crate) fn key_role(key: &OverrideKey) -> Option<OfficerRole> {
    Some(match key {
        OverrideKey::Preset | OverrideKey::MountPreset(_) | OverrideKey::Doctrine { .. } | OverrideKey::SelfDefenceRange => OfficerRole::Gunnery,
        OverrideKey::Slider(k) => sim_systems::officers::tuning::slider(k).map(|s| s.role).unwrap_or(match k.split('.').next().unwrap_or("") {
            "helm" => OfficerRole::Helm, "engineer" => OfficerRole::Engineer, "sensors" => OfficerRole::Sensors, "dc" => OfficerRole::DamageControl, _ => OfficerRole::Gunnery,
        }),
        OverrideKey::Emcon | OverrideKey::BurstTriggerRange => OfficerRole::Sensors,
        OverrideKey::PowerPriority(_) => OfficerRole::Engineer,
        OverrideKey::RepairOrder | OverrideKey::Sink(SinkKind::Pump(_) | SinkKind::Door(_) | SinkKind::FloodValve(_)) => OfficerRole::DamageControl,
        OverrideKey::Sink(_) | OverrideKey::TrackStance(_) | OverrideKey::TrackIdentity(_) => return None,
    })
}

// ---------------------------------------------------------------------------------------------------------
// The live-settings mirror (fed by the core's event walk)
// ---------------------------------------------------------------------------------------------------------

/// What the server told about the own hull's overridden values. Plain data; `Default` is empty.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct BoardBook {
    vessel: Option<EntityId>,
    /// Per officer (`OfficerRole::ALL` index): the newest overrides list and the tick of its reply.
    replies: [Option<(u64, Vec<OverrideView>)>; 5],
    /// Newest `OverrideChanged` per key (`override_id::encode`): (overridden, tick).
    flags: BTreeMap<String, (bool, u64)>,
    /// Officers whose list should be asked for (bit per `OfficerRole::ALL` index): after an override event,
    /// and after a count disagreement.
    pub(crate) dirty_event: u8,
    pub(crate) dirty_poll: u8,
    /// Wall time of the last automatic query per officer.
    asked_s: [Option<f64>; 5],
}

impl BoardBook {
    /// One walked event. Called by `HudCore::event` for every event, fresh or repeated.
    pub(crate) fn observe(&mut self, own: Option<EntityId>, own_slot: Option<PlayerSlot>, tick: u64, event: &SimEvent, fresh: bool, out: &mut Vec<Out>) {
        let Some(own) = own else { return };
        if self.vessel != Some(own) { *self = BoardBook { vessel: Some(own), ..BoardBook::default() }; }
        match event {
            SimEvent::OverrideChanged { vessel, key, overridden } if *vessel == own => {
                let id = super::override_id::encode(key);
                if self.flags.get(&id).is_some_and(|(_, t)| *t > tick) { return; }
                self.flags.insert(id, (*overridden, tick));
                // The ship preset moves values three officers list (preset sliders), so every list is asked for
                // again; any other key only its officer's.
                if fresh {
                    match (key, key_role(key)) {
                        (OverrideKey::Preset, _) => self.dirty_event = 0x1F,
                        (_, Some(r)) => self.dirty_event |= 1 << role_index(r),
                        (_, None) => {}
                    }
                }
                out.push(Out::Bump(Section::Board));
            }
            SimEvent::QueryReply { player, reply: QueryReply::Overrides { role, values } } if own_slot.is_none_or(|s| s == *player) => {
                let i = role_index(*role);
                if self.replies[i].as_ref().is_some_and(|(t, _)| *t > tick) { return; }
                self.replies[i] = Some((tick, values.clone()));
                out.push(Out::Bump(Section::Board));
            }
            _ => {}
        }
    }

    /// The live value of a key and whether it is overridden: `(Some(value), true)` when the server listed it,
    /// `(None, false)` when it is at the design's value, `(None, true)` when it is overridden but its value
    /// has not been reported yet.
    pub(crate) fn lookup(&self, key: &OverrideKey) -> (Option<Value>, bool) {
        let Some(role) = key_role(key) else { return (None, false) };
        let reply = self.replies[role_index(role)].as_ref();
        let listed = reply.and_then(|(_, v)| v.iter().find(|o| &o.key == key)).map(|o| o.current.clone());
        let reply_tick = reply.map(|(t, _)| *t);
        match self.flags.get(&super::override_id::encode(key)) {
            Some((on, t)) if reply_tick.is_none_or(|r| *t > r) => if *on { (listed, true) } else { (None, false) },
            _ => match listed { Some(v) => (Some(v), true), None => (None, false) },
        }
    }

    /// Doctrine row indices the server lists as differing from the design, for the ship (`None`) or a mount.
    pub(crate) fn doctrine_overrides(&self, mount: Option<PartSlot>) -> Vec<u8> {
        let mut v: Vec<u8> = self.replies[role_index(OfficerRole::Gunnery)].iter().flat_map(|(_, l)| l.iter()).filter_map(|o| match o.key {
            OverrideKey::Doctrine { mount: m, index } if m == mount => Some(index),
            _ => None,
        }).collect();
        v.sort_unstable();
        v.dedup();
        v
    }

    /// Officers to ask for now: dirty ones, and ones whose board count disagrees with a settled reply.
    pub(crate) fn due(&mut self, ship: Option<&ShipStatus>, tick: u64, now_s: f64) -> Vec<OfficerRole> {
        let Some(ship) = ship else { return Vec::new() };
        if self.vessel != Some(ship.vessel) { return Vec::new(); }
        for r in &ship.board.officers {
            let Some(role) = r.role else { continue };
            let i = role_index(role);
            let (known, at) = self.replies[i].as_ref().map_or((0, 0), |(t, v)| (v.len(), *t));
            if r.overrides as usize != known && tick >= at.saturating_add(SETTLE_TICKS) { self.dirty_poll |= 1 << i; }
        }
        let mut out = Vec::new();
        for (i, role) in OfficerRole::ALL.into_iter().enumerate() {
            let bit = 1u8 << i;
            let gap = if self.dirty_event & bit != 0 { EVENT_GAP_S } else if self.dirty_poll & bit != 0 { POLL_GAP_S } else { continue };
            if self.asked_s[i].is_some_and(|t| now_s - t < gap && now_s >= t) { continue; }
            self.dirty_event &= !bit;
            self.dirty_poll &= !bit;
            self.asked_s[i] = Some(now_s);
            out.push(role);
        }
        out
    }
}

// ---------------------------------------------------------------------------------------------------------
// Pure helpers (unit-tested without the engine)
// ---------------------------------------------------------------------------------------------------------

/// Doctrine rows of the ship level: for each weapon class, the preset's defaults overlaid by the ship's own
/// entries (later wins per target). Weapon classes in `weapons` order, targets in category order.
pub(crate) fn ship_doctrine(preset: DoctrinePreset, ship_entries: &[DoctrineEntry], weapons: &[WeaponClass]) -> Vec<DoctrineEntry> {
    let mut out = Vec::new();
    for &w in weapons {
        if w == WeaponClass::Decoy { continue; }
        let mut chosen: BTreeMap<TargetCategory, DoctrineEntry> = BTreeMap::new();
        for e in sim_systems::doctrine::default_entries(preset).iter().chain(ship_entries.iter()).filter(|e| e.weapon == w) { chosen.insert(e.target, *e); }
        out.extend(chosen.into_values());
    }
    out
}

/// Doctrine rows of one mount, as `doctrine::resolve` chains them (without the fleet entries a player hull
/// never has): ship preset, ship entries, mount preset, mount entries.
pub(crate) fn mount_doctrine(weapon: WeaponClass, settings: &OfficerSettings, slot: PartSlot) -> Vec<DoctrineEntry> {
    if weapon == WeaponClass::Decoy { return Vec::new(); }
    let mut chosen: BTreeMap<TargetCategory, DoctrineEntry> = BTreeMap::new();
    let mut apply = |list: &[DoctrineEntry], by_weapon: bool| for e in list.iter().filter(|e| !by_weapon || e.weapon == weapon) { chosen.insert(e.target, DoctrineEntry { weapon, ..*e }); };
    apply(&sim_systems::doctrine::default_entries(settings.preset), true);
    apply(&settings.doctrine, true);
    if let Some(p) = settings.mount_presets.get(&slot) { apply(&sim_systems::doctrine::default_entries(*p), true); }
    if let Some(list) = settings.mount_doctrine.get(&slot) { apply(list, false); }
    chosen.into_values().collect()
}

/// What a rule condition reads, taken from the newest snapshot.
pub(crate) struct RuleFacts<'a> {
    pub snap: &'a Snapshot,
    pub ship: &'a ShipStatus,
    pub own_pos: Option<glam::DVec2>,
    /// Ammunition fraction of a part, when the snapshot can tell.
    pub ammo: &'a dyn Fn(PartSlot) -> Option<f64>,
}

fn admits(f: &TrackFilter, t: &sim_systems::protocol::TrackView) -> bool {
    !t.last_known && t.pos.is_finite() && t.stance >= f.min_stance && f.domain.is_none_or(|d| t.domain == d) && t.grade >= f.min_grade
}

fn operand(facts: &RuleFacts<'_>, op: &Operand) -> Option<f64> {
    let s = facts.ship;
    let num = |v: f32| v.is_finite().then_some(v as f64);
    match op {
        Operand::Const(c) => c.is_finite().then_some(*c),
        Operand::OwnSpeedKn => num(s.speed_kn),
        Operand::OwnHeelDeg => num(s.heel_deg).map(f64::abs),
        Operand::FuelFraction => num(s.power.fuel_fraction),
        Operand::PowerMargin => num(s.power.generation_kw - s.power.demand_kw),
        Operand::HullHealth => num(s.cells_lost).map(|c| (1.0 - c).clamp(0.0, 1.0)),
        Operand::InboundWeapons => Some(facts.snap.tracks.iter().filter(|t| !t.last_known && t.inbound && t.domain == content::enums::Domain::Weapon).count() as f64),
        Operand::SortieTimeS => Some(super::tick_s(facts.snap.tick)),
        Operand::ContactCount(f) => Some(facts.snap.tracks.iter().filter(|t| admits(f, t)).count() as f64),
        Operand::NearestContactRangeM(f) => {
            let own = facts.own_pos?;
            facts.snap.tracks.iter().filter(|t| admits(f, t)).map(|t| glam::DVec2::new(t.pos.x, t.pos.z).distance(own)).filter(|r| r.is_finite()).min_by(|a, b| a.total_cmp(b))
        }
        Operand::PartHealth(p) => s.parts.iter().find(|x| x.slot == *p).map(|x| x.health as f64 / 255.0),
        Operand::AmmoFraction(p) => (facts.ammo)(*p),
    }
}

/// Three-valued truth of a condition (`None` = unknown), with the graph's comparison epsilon of 1e-6.
pub(crate) fn holds(facts: &RuleFacts<'_>, c: &Condition, depth: usize) -> Option<bool> {
    if depth > 16 { return None; }
    match c {
        Condition::Compare { lhs, cmp, rhs } => {
            let (a, b) = (operand(facts, lhs)?, operand(facts, rhs)?);
            let eq = (a - b).abs() <= 1e-6;
            Some(match cmp { Cmp::Eq => eq, Cmp::Ne => !eq, Cmp::Lt => !eq && a < b, Cmp::Le => eq || a < b, Cmp::Gt => !eq && a > b, Cmp::Ge => eq || a > b })
        }
        Condition::All(list) => {
            if list.is_empty() { return Some(true); }
            let v: Vec<Option<bool>> = list.iter().take(64).map(|x| holds(facts, x, depth + 1)).collect();
            if v.contains(&Some(false)) { Some(false) } else if v.contains(&None) { None } else { Some(true) }
        }
        Condition::Any(list) => {
            if list.is_empty() { return Some(false); }
            let v: Vec<Option<bool>> = list.iter().take(64).map(|x| holds(facts, x, depth + 1)).collect();
            if v.contains(&Some(true)) { Some(true) } else if v.contains(&None) { None } else { Some(false) }
        }
        Condition::Not(inner) => holds(facts, inner, depth + 1).map(|b| !b),
    }
}

/// Whether the sink a rule names exists on the design (the compiler skips a rule whose sink is missing).
pub(crate) fn sink_valid(design: &sim_systems::ship::ShipDesign, sink: &SinkKind) -> bool {
    let mount = |s: &PartSlot| design.mounts.iter().any(|m| m.slot == *s && m.weapon != WeaponClass::Decoy);
    match sink {
        SinkKind::MountAimPoint(s) | SinkKind::MountFirePermission(s) | SinkKind::MissileLaunch(s) | SinkKind::MissileTarget(s) => mount(s),
        SinkKind::EmissionState(s) => design.emitters.iter().any(|e| e.slot == *s),
        SinkKind::PowerPriority(s) => design.consumer_slots.contains(s),
        SinkKind::Pump(s) => design.pumps.contains(s),
        SinkKind::Door(s) => design.doors.contains(s),
        SinkKind::FloodValve(c) => design.prefab.compartments.compartments.iter().any(|x| x.id == *c && !x.is_absorbed()),
        SinkKind::HelmHeading | SinkKind::HelmSpeed | SinkKind::TrackStanceOverride | SinkKind::TrackIdOverride | SinkKind::ShipPreset | SinkKind::ShipEmcon => true,
    }
}

fn operands_valid(c: &Condition, parts: usize, depth: usize) -> bool {
    if depth > 16 { return false; }
    let op_ok = |o: &Operand| match o { Operand::PartHealth(s) | Operand::AmmoFraction(s) => s.index() < parts, _ => true };
    match c {
        Condition::Compare { lhs, rhs, .. } => op_ok(lhs) && op_ok(rhs),
        Condition::All(l) | Condition::Any(l) => l.iter().all(|x| operands_valid(x, parts, depth + 1)),
        Condition::Not(x) => operands_valid(x, parts, depth + 1),
    }
}

/// Recommended EMCON of an operation (the briefing rule of `theatre::opgen`).
pub(crate) fn recommended_emcon(op: &content::campaign::OperationDef) -> Option<Emcon> {
    use content::campaign::GhostKind;
    op.recommended_emcon.or_else(|| op.ghosts.iter().any(|g| matches!(g.kind, GhostKind::NoRadarEmissions | GhostKind::NeverDetected)).then_some(Emcon::Silent))
}

/// Highest layer a sortie of this mode lets the player use (graph edits never happen in a sortie, L112).
pub(crate) fn sortie_layer(mode: Option<GameMode>) -> ControlLayer {
    use content::ruleset::RulesetKind;
    let kind = match mode { Some(GameMode::PvpElimination | GameMode::PvpEscort) => RulesetKind::Standard, _ => RulesetKind::Open };
    kind.max_layer().min(ControlLayer::L2Rules)
}

fn msg_or_empty(m: Option<&Msg>) -> VarDictionary { m.filter(|m| !m.key.as_str().is_empty()).map_or_else(empty_msg, msg_dict) }
fn fin(v: f64) -> f64 { if v.is_finite() { v } else { 0.0 } }

/// Ordered speeds below this read as stopped on the board, in knots.
const STOPPED_KN: f64 = 0.05;

/// The Helm group's task, reason and next action while the player steers: the ship's real ordered course
/// and speed (`ShipStatus`) in the officer's own sentences, not the officer's idle plan (which says
/// "Stopped" while the player runs at 28 kn). A stopped ship under manual helm names no next action.
pub(crate) fn player_helm_lines(s: &ShipStatus) -> (Msg, Msg, Option<Msg>) {
    use sim_systems::protocol::Unit;
    let speed = fin(s.ordered_speed_kn as f64);
    let course = crate::convert::heading_deg(fin(s.ordered_heading_rad as f64));
    if speed.abs() < STOPPED_KN {
        return (Msg::new("officer.helm.task.stopped"), Msg::new("officer.helm.reason.stop"), None);
    }
    (
        Msg::new("officer.helm.task.hold_heading").num(course, Unit::Degrees).num(speed, Unit::Knots),
        Msg::new("officer.helm.reason.ordered").num(course, Unit::Degrees).num(speed, Unit::Knots),
        Some(Msg::new("officer.helm.next.hold").num(course, Unit::Degrees)),
    )
}

impl BpHud {
    /// Ask the server for the officers' override lists the book wants (see the module docs). Called once per
    /// frame by the core after the sections are diffed.
    pub(crate) fn board_sync(&mut self) {
        let services = services();
        let (state, now_s) = { let f = services.feed.borrow(); (f.state, f.now_s) };
        if state != crate::enums::SessionState::InSortie { return; }
        let tick = self.core.tick;
        let ship = self.snapshot.as_ref().and_then(|s| s.ship.as_ref());
        let due = self.core.board.due(ship, tick, now_s);
        if due.is_empty() { return; }
        let mut conn = services.connection.borrow_mut();
        let Some(conn) = conn.as_mut() else { return };
        for role in due { conn.link.send(crate::seams::ClientMsg::Command(Command::Query(Query::Overrides(role)))); }
    }

    /// The live settings: the design's, with the server's reported preset, mount presets, sliders, EMCON and
    /// ranges applied.
    fn live_settings(&self) -> OfficerSettings {
        let Some(design) = self.design() else { return OfficerSettings::default() };
        let mut s = design.settings.clone();
        let book = &self.core().board;
        if let (Some(Value::Preset(p)), _) = book.lookup(&OverrideKey::Preset) { s.preset = p; }
        for m in &design.mounts {
            match book.lookup(&OverrideKey::MountPreset(m.slot)) {
                (Some(Value::Preset(p)), _) => { s.mount_presets.insert(m.slot, p); }
                (Some(Value::None), true) => { s.mount_presets.remove(&m.slot); }
                _ => {}
            }
        }
        for def in sim_systems::officers::tuning::sliders() {
            if let (Some(Value::Scalar(v)), _) = book.lookup(&OverrideKey::Slider(def.key.to_string())) { if v.is_finite() { s.sliders.insert(def.key.to_string(), v); } }
        }
        if let (Some(Value::Emcon(e)), _) = book.lookup(&OverrideKey::Emcon) { s.emcon = e; }
        match book.lookup(&OverrideKey::BurstTriggerRange) { (Some(Value::Scalar(v)), _) => s.burst_trigger_range_m = Some(v), (Some(Value::None), true) => s.burst_trigger_range_m = None, _ => {} }
        match book.lookup(&OverrideKey::SelfDefenceRange) { (Some(Value::Scalar(v)), _) => s.self_defence_range_m = Some(v), (Some(Value::None), true) => s.self_defence_range_m = None, _ => {} }
        s
    }

    fn report(&self, role: OfficerRole) -> Option<&OfficerReport> { self.ship()?.board.officers.iter().find(|r| r.role == Some(role)) }

    fn officer_dict(&self, role: OfficerRole) -> VarDictionary {
        let r = self.report(role);
        let msg = |m: Option<&Msg>| msg_or_empty(m.map(|m| self.named_parts(m)).as_ref());
        let holder = r.map_or(ControlHolder::Officer, |r| r.holder);
        let manual = (role == OfficerRole::Helm && (holder == ControlHolder::Player || self.holder_of(ControlRole::Helm) == ControlHolder::Player))
            .then(|| self.ship().map(player_helm_lines)).flatten();
        let (task, reason, next) = match &manual {
            Some((t, why, n)) => (msg_or_empty(Some(t)), msg_or_empty(Some(why)), msg_or_empty(n.as_ref())),
            None => (msg(r.map(|r| &r.task)), msg(r.map(|r| &r.reason)), msg(r.map(|r| &r.next))),
        };
        let holder = if manual.is_some() { ControlHolder::Player } else { holder };
        d! {
            "role" => role.to_gd(),
            "name" => StringName::from(sim_systems::text::officer_word(role).as_str()),
            "task" => task,
            "reason" => reason,
            "next" => next,
            "holder" => holder.to_gd(),
            "layer" => r.map_or(ControlLayer::L0Doctrine, |r| r.layer).to_gd(),
            "overrides" => r.map_or(0, |r| r.overrides as i64),
            "advisory" => if holder == ControlHolder::Player { msg(r.and_then(|r| r.advisory.as_ref())) } else { empty_msg() },
        }
    }

    fn holder_of(&self, role: ControlRole) -> ControlHolder {
        let Some(ship) = self.ship() else { return ControlHolder::Officer };
        let group = match role { ControlRole::Mount(s) => self.design().and_then(|d| d.mounts.iter().find(|m| m.slot == s)).map(|m| m.group), _ => None };
        let held = ship.held.contains(&role) || group.is_some_and(|g| ship.held.contains(&ControlRole::WeaponGroup(g)));
        if held { ControlHolder::Player } else { ControlHolder::Officer }
    }

    /// Ammunition fraction of a part from its readout and catalogue capacity.
    fn ammo_fraction(&self, slot: PartSlot) -> Option<f64> {
        let ship = self.ship()?;
        let readout = ship.parts.iter().find(|p| p.slot == slot)?.readout;
        let design = self.design()?;
        let def = design.prefab.parts.get(slot.index()).and_then(|r| services().catalogue().parts.get_index(r.def.index()).map(|(_, d)| d.function.clone()))?;
        let ratio = |have: f64, cap: f64| (cap > 0.0 && have.is_finite()).then(|| (have / cap).clamp(0.0, 1.0));
        use content::part::PartFunction as F;
        match (readout, def) {
            (PartReadout::Rounds { magazine, .. }, F::Magazine { rounds, .. }) => ratio(magazine as f64, rounds as f64),
            (PartReadout::Rounds { ready, .. }, F::Gun(g)) => ratio(ready as f64, g.ready_rounds as f64),
            (PartReadout::Cells { ready, .. }, F::Launcher(l)) => ratio(ready as f64, l.cells as f64),
            _ => None,
        }
    }

    /// The design's rules as `RuleDict`s, cached with the design.
    fn rule_dicts(&self) -> Array<VarDictionary> {
        self.cached("rules_static", 0, &[Section::Static], |h| {
            let Some(design) = h.design() else { return Array::new() };
            let parts = design.prefab.parts.len();
            design.rules.iter().take(content::graph::MAX_RULES).enumerate().map(|(i, r)| rule_dict(i, r, sink_valid(design, &r.sink) && operands_valid(&r.when, parts, 0))).collect()
        })
    }

    fn rule_active(&self, rule: &Rule) -> bool {
        let (Some(snap), Some(ship)) = (self.snap(), self.ship()) else { return false };
        if !rule.enabled { return false; }
        let own_pos = self.own_view().map(|v| glam::DVec2::new(v.kin.pose.pos.x, v.kin.pose.pos.z)).filter(|p| p.is_finite());
        let ammo = |s: PartSlot| self.ammo_fraction(s);
        let facts = RuleFacts { snap, ship, own_pos, ammo: &ammo };
        // A nearest-contact rule needs a matching contact as well (the compiler adds that test).
        let contact = match &rule.value {
            content::graph::RuleValue::NearestContact(f) => {
                let mut f = *f;
                if f.min_stance < Stance::Suspect { f.min_stance = Stance::Suspect; }
                snap.tracks.iter().any(|t| admits(&f, t))
            }
            _ => true,
        };
        contact && holds(&facts, &rule.when, 0) == Some(true)
    }
}

#[godot_api(secondary)]
impl BpHud {
    /// Returns `BoardDict`. On change (at most 10 Hz), `board_changed`.
    #[func]
    fn board(&self) -> VarDictionary {
        self.cached("board", 0, &[Section::Board, Section::Static], |h| {
            let officers: Array<VarDictionary> = OfficerRole::ALL.into_iter().map(|r| h.officer_dict(r)).collect();
            let ship = h.ship();
            let radiating: Vec<i32> = ship.map(|s| s.board.radiating.iter().map(|p| p.0 as i32).collect()).unwrap_or_default();
            let held: Array<VarDictionary> = ship.map(|s| s.held.iter().map(|r| role_dict(*r)).collect()).unwrap_or_default();
            d! {
                "officers" => officers,
                "radiating" => PackedInt32Array::from(radiating.as_slice()),
                "esm_warning" => ship.is_some_and(|s| s.board.esm_warning),
                "emcon" => ship.map_or_else(|| h.live_settings().emcon, |s| s.board.emcon).to_gd(),
                "held" => held,
            }
        })
    }
    /// One `OfficerDict` by `Bp.OFFICER_*`.
    #[func]
    fn officer(&self, role: i64) -> VarDictionary {
        let Some(r) = OfficerRole::from_gd(role) else { return VarDictionary::new() };
        let board = self.board();
        board.get("officers").and_then(|v| v.try_to::<Array<VarDictionary>>().ok()).and_then(|a| a.get(role_index(r))).unwrap_or_else(|| self.officer_dict(r))
    }
    /// Who holds a role now (`Bp.HOLDER_*`). Arguments as in `Orders.take_control`.
    #[func]
    fn role_holder(&self, kind: i64, index: i64) -> i64 {
        match crate::convert::role_from_gd(kind, index) { Some(role) => self.holder_of(role).to_gd(), None => ControlHolder::Officer.to_gd() }
    }
    /// Roles the player can take, in display order: `Array[Dictionary]` with `role: RoleDict`,
    /// `name: MsgDict`, `holder: int`, `action: StringName` (input action that takes it, for the shortcut hint).
    #[func]
    fn takeable_roles(&self) -> Array<VarDictionary> {
        self.cached("takeable_roles", 0, &[Section::Board, Section::Mounts, Section::Static], |h| {
            if h.ship().is_none() { return Array::new(); }
            let mut roles: Vec<(ControlRole, Msg, String)> = vec![(ControlRole::Helm, Msg::new("hud.role.helm"), "take_helm".into())];
            let mut groups: Vec<u8> = h.design().map(|d| d.mounts.iter().map(|m| m.group).collect()).unwrap_or_default();
            groups.sort_unstable();
            groups.dedup();
            for g in groups {
                let action = if g < KEYED_GROUPS { format!("take_group_{}", g + 1) } else { String::new() };
                roles.push((ControlRole::WeaponGroup(g), Msg::new("hud.group.name").arg(MsgArg::Int(g as i64 + 1)), action));
            }
            if h.design().is_none_or(|d| !d.sensor_slots.is_empty()) { roles.push((ControlRole::Sensors, Msg::new("hud.role.sensors"), "take_sensors".into())); }
            roles.into_iter().map(|(role, name, action)| d! {
                "role" => role_dict(role),
                "name" => msg_dict(&name),
                "holder" => h.holder_of(role).to_gd(),
                "action" => StringName::from(action.as_str()),
            }).collect()
        })
    }
    /// Current doctrine preset of the ship (`mount == Bp.NONE`) or one mount: `{ "preset": int,
    /// "overridden": bool }`.
    #[func]
    fn preset(&self, mount: i64) -> VarDictionary {
        let live = self.live_settings();
        let book = &self.core().board;
        if mount < 0 {
            return d! { "preset" => live.preset.to_gd(), "overridden" => book.lookup(&OverrideKey::Preset).1 };
        }
        let Some(slot) = crate::convert::slot_from_gd(mount) else { return d! { "preset" => live.preset.to_gd(), "overridden" => false } };
        let p = live.mount_presets.get(&slot).copied().unwrap_or(live.preset);
        d! { "preset" => p.to_gd(), "overridden" => book.lookup(&OverrideKey::MountPreset(slot)).1 }
    }
    /// L1 sliders of one officer with live values: `Array[SliderDict]`, basic sliders first.
    #[func]
    fn sliders(&self, officer: i64) -> Array<VarDictionary> {
        let Some(role) = OfficerRole::from_gd(officer) else { return Array::new() };
        self.cached("sliders", officer, &[Section::Board, Section::Static], |h| {
            let live = h.live_settings();
            let book = &h.core().board;
            let defs = sim_systems::officers::tuning::sliders();
            let mut list: Vec<_> = defs.iter().filter(|s| s.role == role).collect();
            list.sort_by_key(|s| !s.primary);
            list.into_iter().map(|s| {
                let value = sim_systems::officers::tuning::slider_value(&live, s.key);
                let overridden = book.lookup(&OverrideKey::Slider(s.key.to_string())).1;
                slider_dict(s.key, value, overridden)
            }).collect()
        })
    }
    /// Doctrine rows in force for the ship (`mount == Bp.NONE`) or one mount: `Array[DoctrineEntryDict]`.
    #[func]
    fn doctrine(&self, mount: i64) -> Array<VarDictionary> {
        let slot = if mount < 0 { None } else { match crate::convert::slot_from_gd(mount) { Some(s) => Some(s), None => return Array::new() } };
        self.cached("doctrine", mount.max(-1), &[Section::Board, Section::Static], |h| {
            let Some(design) = h.design() else { return Array::new() };
            let live = h.live_settings();
            let differing = h.core().board.doctrine_overrides(slot);
            let stored: &[DoctrineEntry] = match slot { None => &design.settings.doctrine, Some(s) => design.settings.mount_doctrine.get(&s).map(|v| v.as_slice()).unwrap_or(&[]) };
            let marked = |e: &DoctrineEntry| differing.iter().any(|i| stored.get(*i as usize).is_some_and(|d| d.target == e.target && (slot.is_some() || d.weapon == e.weapon)));
            let rows = match slot {
                None => {
                    let mut weapons: Vec<WeaponClass> = design.mounts.iter().map(|m| m.weapon).collect();
                    weapons.sort_unstable();
                    weapons.dedup();
                    ship_doctrine(live.preset, &live.doctrine, &weapons)
                }
                Some(s) => match design.mounts.iter().find(|m| m.slot == s) { Some(m) => mount_doctrine(m.weapon, &live, s), None => Vec::new() },
            };
            rows.iter().map(|e| doctrine_entry_dict(e, marked(e))).collect()
        })
    }
    /// The rules table in force: `Array[RuleDict]`, top rule first. `active` (extra key, `bool`) marks rules
    /// whose condition holds this tick.
    #[func]
    fn rules(&self) -> Array<VarDictionary> {
        let Some(design) = self.design() else { return Array::new() };
        let dicts = self.rule_dicts();
        dicts.iter_shared().zip(design.rules.iter()).map(|(d, r)| {
            let mut d = d.duplicate_shallow();
            d.set("active", self.rule_active(r));
            d
        }).collect()
    }
    /// Emission control detail: `{ "emcon": int, "overridden": bool, "burst_trigger_range_m": float,
    /// "self_defence_range_m": float, "radiating": PackedInt32Array, "esm_warning": bool,
    /// "reason": MsgDict, "recommended": int (briefing recommendation or Bp.NONE) }`.
    #[func]
    fn emcon(&self) -> VarDictionary {
        self.cached("emcon", 0, &[Section::Board, Section::Static], |h| {
            let live = h.live_settings();
            let catalogue = services().catalogue();
            let tuning = &catalogue.tuning.doctrine;
            let ship = h.ship();
            let pos = |v: Option<f64>, fallback: f64| fin(v.filter(|x| x.is_finite() && *x > 0.0).unwrap_or(fallback));
            let radiating: Vec<i32> = ship.map(|s| s.board.radiating.iter().map(|p| p.0 as i32).collect()).unwrap_or_default();
            let op = &h.core().operation;
            let recommended = if op.is_empty() { None } else { services().campaign().operations.get(&content::ids::OperationId::new(op.as_str())).and_then(recommended_emcon) };
            d! {
                "emcon" => ship.map_or(live.emcon, |s| s.board.emcon).to_gd(),
                "overridden" => h.core().board.lookup(&OverrideKey::Emcon).1,
                "burst_trigger_range_m" => pos(live.burst_trigger_range_m, tuning.burst_trigger_m),
                "self_defence_range_m" => pos(live.self_defence_range_m, tuning.self_defence_range_m),
                "radiating" => PackedInt32Array::from(radiating.as_slice()),
                "esm_warning" => ship.is_some_and(|s| s.board.esm_warning),
                "reason" => msg_or_empty(h.report(OfficerRole::Sensors).map(|r| h.named_parts(&r.reason)).as_ref()),
                "recommended" => recommended.map_or(-1, |e| e.to_gd()),
            }
        })
    }
    /// Highest control layer this sortie's ruleset lets the player use (`Bp.LAYER_*`); graph edits are never
    /// possible in a sortie (L112).
    #[func]
    fn max_layer(&self) -> i64 {
        let mode = services().feed.borrow().intro.as_ref().map(|i| i.mode);
        sortie_layer(mode).to_gd()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use content::enums::{IdRung, TrackGrade};
    use sim_systems::protocol::{BridgeBoard, TrackView};

    fn own() -> EntityId { EntityId::from_u64(11) }
    fn ov(key: OverrideKey, current: Value) -> OverrideView { OverrideView { key, label: Msg::new("x"), default: Value::None, current, layer: ControlLayer::L1Tune } }

    #[test]
    fn book_follows_replies_and_flags() {
        let mut b = BoardBook::default();
        let mut out = Vec::new();
        let slider = OverrideKey::Slider("gunnery.aggression".into());
        assert_eq!(b.lookup(&slider), (None, false));
        b.observe(Some(own()), Some(PlayerSlot(0)), 100, &SimEvent::OverrideChanged { vessel: own(), key: slider.clone(), overridden: true }, true, &mut out);
        assert_eq!(b.dirty_event, 1 << role_index(OfficerRole::Gunnery));
        b.observe(Some(own()), Some(PlayerSlot(0)), 101, &SimEvent::OverrideChanged { vessel: own(), key: OverrideKey::Preset, overridden: true }, true, &mut out);
        assert_eq!(b.dirty_event, 0x1F);
        assert_eq!(b.lookup(&slider), (None, true));
        let reply = SimEvent::QueryReply { player: PlayerSlot(0), reply: QueryReply::Overrides { role: OfficerRole::Gunnery, values: vec![ov(slider.clone(), Value::Scalar(0.8))] } };
        b.observe(Some(own()), Some(PlayerSlot(0)), 103, &reply, true, &mut out);
        assert_eq!(b.lookup(&slider), (Some(Value::Scalar(0.8)), true));
        // Revert after the reply: back to the design value even though the old reply still lists it.
        b.observe(Some(own()), Some(PlayerSlot(0)), 200, &SimEvent::OverrideChanged { vessel: own(), key: slider.clone(), overridden: false }, true, &mut out);
        assert_eq!(b.lookup(&slider), (None, false));
        // A repeated older flag does not undo the newer one.
        b.observe(Some(own()), Some(PlayerSlot(0)), 100, &SimEvent::OverrideChanged { vessel: own(), key: slider.clone(), overridden: true }, false, &mut out);
        assert_eq!(b.lookup(&slider), (None, false));
        // Another player's reply is not ours.
        let other = SimEvent::QueryReply { player: PlayerSlot(1), reply: QueryReply::Overrides { role: OfficerRole::Helm, values: vec![ov(OverrideKey::Slider("helm.evasion".into()), Value::Scalar(0.1))] } };
        b.observe(Some(own()), Some(PlayerSlot(0)), 300, &other, true, &mut out);
        assert_eq!(b.lookup(&OverrideKey::Slider("helm.evasion".into())), (None, false));
        assert!(out.iter().all(|o| *o == Out::Bump(Section::Board)));
    }

    #[test]
    fn queries_are_rate_limited_and_follow_counts() {
        let mut b = BoardBook::default();
        let mut out = Vec::new();
        b.observe(Some(own()), None, 10, &SimEvent::OverrideChanged { vessel: own(), key: OverrideKey::Emcon, overridden: true }, true, &mut out);
        let mut ship = ShipStatus { vessel: own(), ..Default::default() };
        assert_eq!(b.due(Some(&ship), 20, 1.0), vec![OfficerRole::Sensors]);
        assert!(b.due(Some(&ship), 21, 1.05).is_empty());
        // A second change inside the event gap is asked for once the gap has passed.
        b.observe(Some(own()), None, 22, &SimEvent::OverrideChanged { vessel: own(), key: OverrideKey::Emcon, overridden: true }, true, &mut out);
        assert!(b.due(Some(&ship), 23, 1.1).is_empty());
        assert_eq!(b.due(Some(&ship), 24, 1.25), vec![OfficerRole::Sensors]);
        assert!(b.due(Some(&ship), 25, 1.5).is_empty());
        // The board says Sensors has one override but no reply came: asked again after the gap.
        ship.board = BridgeBoard { officers: OfficerRole::ALL.iter().map(|r| OfficerReport { role: Some(*r), overrides: if *r == OfficerRole::Sensors { 1 } else { 0 }, ..Default::default() }).collect(), ..Default::default() };
        assert!(b.due(Some(&ship), 60, 2.0).is_empty());
        assert_eq!(b.due(Some(&ship), 61, 3.3), vec![OfficerRole::Sensors]);
        // A settled reply that matches the count stops the asking.
        let reply = SimEvent::QueryReply { player: PlayerSlot(0), reply: QueryReply::Overrides { role: OfficerRole::Sensors, values: vec![ov(OverrideKey::Emcon, Value::Emcon(Emcon::Silent))] } };
        b.observe(Some(own()), None, 70, &reply, true, &mut out);
        assert!(b.due(Some(&ship), 200, 10.0).is_empty());
        assert_eq!(b.lookup(&OverrideKey::Emcon), (Some(Value::Emcon(Emcon::Silent)), true));
        // No ship, no asking; another hull's status neither.
        assert!(b.due(None, 300, 20.0).is_empty());
        assert!(b.due(Some(&ShipStatus { vessel: EntityId::from_u64(3), ..Default::default() }), 300, 20.0).is_empty());
    }

    #[test]
    fn doctrine_chains_like_resolve() {
        let weapons = [WeaponClass::Gun, WeaponClass::Decoy];
        let rows = ship_doctrine(DoctrinePreset::Cautious, &[], &weapons);
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().all(|r| r.weapon == WeaponClass::Gun));
        let custom = DoctrineEntry { weapon: WeaponClass::Gun, target: TargetCategory::Surface, min_stance: Stance::Suspect, min_id: IdRung::Unknown, min_grade: TrackGrade::Coarse, salvo: 1, effective_range_m: Some(3_000.0) };
        let rows = ship_doctrine(DoctrinePreset::Cautious, &[custom], &weapons);
        assert!(rows.contains(&custom));
        let mut s = OfficerSettings { preset: DoctrinePreset::Cautious, ..OfficerSettings::default() };
        s.mount_presets.insert(PartSlot(2), DoctrinePreset::SilentRunning);
        let held = mount_doctrine(WeaponClass::Gun, &s, PartSlot(2));
        assert!(!held.is_empty() && held.iter().all(|e| e.salvo == 0));
        assert!(mount_doctrine(WeaponClass::Gun, &s, PartSlot(3)).iter().all(|e| e.salvo > 0));
        assert!(mount_doctrine(WeaponClass::Decoy, &s, PartSlot(3)).is_empty());
    }

    fn track(id: u32, stance: Stance, x: f64) -> TrackView {
        TrackView {
            id: sim_systems::ids::TrackId(id), domain: content::enums::Domain::Surface, stance, stance_reason: Default::default(), rung: IdRung::Domain, rung_reason: Default::default(),
            grade: TrackGrade::Fine, pos: glam::DVec3::new(x, 0.0, 0.0), vel: glam::DVec3::ZERO, shape: sim_systems::protocol::UncertaintyShape::Circle { radius_m: 1.0 },
            last_update_tick: 0, last_known: false, class: None, candidates: 0, sources: Default::default(), trend: Default::default(), inbound: false, engaged_by: 0,
            envelope_m: 0.0, emitting: false, relevance: Default::default(), struck: false,
        }
    }

    #[test]
    fn rule_conditions_read_the_snapshot() {
        let ship = ShipStatus { speed_kn: 20.0, heel_deg: -12.0, ..Default::default() };
        let snap = Snapshot { tick: 600, tracks: vec![track(1, Stance::Hostile, 3_000.0), track(2, Stance::Neutral, 500.0)], ..Default::default() };
        let none = |_s: PartSlot| None;
        let facts = RuleFacts { snap: &snap, ship: &ship, own_pos: Some(glam::DVec2::ZERO), ammo: &none };
        let hostile = TrackFilter { min_stance: Stance::Suspect, domain: None, min_grade: TrackGrade::Bearing };
        let cmp = |lhs, cmp, rhs| Condition::Compare { lhs, cmp, rhs };
        assert_eq!(holds(&facts, &cmp(Operand::NearestContactRangeM(hostile), Cmp::Lt, Operand::Const(4_000.0)), 0), Some(true));
        assert_eq!(holds(&facts, &cmp(Operand::ContactCount(hostile), Cmp::Eq, Operand::Const(1.0)), 0), Some(true));
        assert_eq!(holds(&facts, &cmp(Operand::OwnHeelDeg, Cmp::Ge, Operand::Const(12.0)), 0), Some(true));
        assert_eq!(holds(&facts, &cmp(Operand::SortieTimeS, Cmp::Gt, Operand::Const(10.0)), 0), Some(false));
        // Unknown operands: comparisons are unknown; All with a false is false, Any with a true is true.
        let unknown = cmp(Operand::AmmoFraction(PartSlot(0)), Cmp::Lt, Operand::Const(0.5));
        assert_eq!(holds(&facts, &unknown, 0), None);
        assert_eq!(holds(&facts, &Condition::All(vec![unknown.clone(), cmp(Operand::OwnSpeedKn, Cmp::Gt, Operand::Const(30.0))]), 0), Some(false));
        assert_eq!(holds(&facts, &Condition::Any(vec![unknown.clone(), cmp(Operand::OwnSpeedKn, Cmp::Gt, Operand::Const(10.0))]), 0), Some(true));
        assert_eq!(holds(&facts, &Condition::Not(Box::new(unknown)), 0), None);
        assert_eq!(holds(&facts, &cmp(Operand::Const(f64::NAN), Cmp::Eq, Operand::Const(0.0)), 0), None);
    }

    #[test]
    fn manual_helm_reports_the_real_order() {
        // The splash basin: the player holds the helm at 28 kn ordered on 125 while the officer's own plan says Stopped.
        let ship = ShipStatus { ordered_speed_kn: 28.0, ordered_heading_rad: 125f32.to_radians(), held: vec![ControlRole::Helm], ..Default::default() };
        let (task, reason, next) = player_helm_lines(&ship);
        assert_eq!(task.key.as_str(), "officer.helm.task.hold_heading");
        assert_eq!(reason.key.as_str(), "officer.helm.reason.ordered");
        let num = |m: &Msg, i: usize| match m.args[i] { MsgArg::Num { value, .. } => value, _ => f64::NAN };
        assert!((num(&task, 0) - 125.0).abs() < 1e-3 && (num(&task, 1) - 28.0).abs() < 1e-6);
        assert!((num(&reason, 1) - 28.0).abs() < 1e-6);
        assert_eq!(next.map(|n| n.key.as_str().to_string()).as_deref(), Some("officer.helm.next.hold"));
        let stopped = ShipStatus { ordered_speed_kn: 0.0, ..ship };
        let (task, reason, next) = player_helm_lines(&stopped);
        assert_eq!((task.key.as_str(), reason.key.as_str(), next.is_none()), ("officer.helm.task.stopped", "officer.helm.reason.stop", true));
    }

    #[test]
    fn layers_and_roles() {
        assert_eq!(sortie_layer(Some(GameMode::PvpElimination)), ControlLayer::L2Rules);
        assert_eq!(sortie_layer(Some(GameMode::Campaign)), ControlLayer::L2Rules);
        assert_eq!(sortie_layer(None), ControlLayer::L2Rules);
        assert_eq!(key_role(&OverrideKey::Slider("helm.evasion".into())), Some(OfficerRole::Helm));
        assert_eq!(key_role(&OverrideKey::Slider("dc.unknown".into())), Some(OfficerRole::DamageControl));
        assert_eq!(key_role(&OverrideKey::Preset), Some(OfficerRole::Gunnery));
        assert_eq!(key_role(&OverrideKey::Emcon), Some(OfficerRole::Sensors));
        assert_eq!(key_role(&OverrideKey::TrackStance(sim_systems::ids::TrackId(3))), None);
    }
}
