//! Unit `gd-hud-ship`. Own-ship state on `BpHud`: motion, power, parts, mounts and fire control, offboard
//! units, station actions, objectives, allies. DESIGN.md L1668 (gauges: value with units against band and
//! limits), L407-408 and L518 (engagement state and solution per mount), L353 (every hold and shot has a
//! reason), L786-797 (station actions), L920 (what replicates).
//! Source: `protocol::ShipStatus` of the newest snapshot plus the own design for static facts.
//!
//! Decisions where the contract is silent:
//! - `astern_speed_kn` is six tenths of the top speed, the figure `sim_core::hydro::forces` documents for
//!   its astern thrust and stern-first drag.
//! - Heel limits: the limit is the heel at which the simulation reports capsize imminent
//!   (`capsize_heel_deg` x `alerts::CAPSIZE_WARN_FRACTION`, 50 degrees by default); the caution band starts
//!   at half of it.
//! - `mounts()` lists the snapshot's mounts and launchers, then any decoy launcher of the design the snapshot
//!   does not list (status Idle, reason "no target"). A mount's `name` is the HUD part name ("Gun 2").
//! - `weapon_groups()` names a group "Weapons group N" (N = takeover key, 1-based); its status is the first
//!   engaging mount's, else the first holding mount's, else Idle.
//! - `available_actions()` lists every station action the hull has a part for (Carry passengers is passive and
//!   never listed). It is enabled while that part works, the hull is afloat, the sortie offers it (the
//!   operation's `actions`; a sortie without an operation offers all of them, and the server still has the
//!   last word), no copy of it runs, and no other RHIB action holds the boat. Speed, range and target
//!   conditions are not checked here: the server starts the action paused with its reason, so the player
//!   can start it on the approach. Which parts serve which action mirrors `sim_systems::specops::serves`
//!   (crate-private there): keep the two in step.
//! - Hidden objectives are left out of `objectives()`.
use super::{part_name_msg, BpHud, Section};
use crate::convert::{d, empty_msg, msg_dict};
use crate::enums::GdEnum;
use content::campaign::StationAction;
use content::part::{PartDef, PartFunction, PartTag};
use godot::prelude::*;
use sim_core::vessel::VesselStatus;
use sim_systems::protocol::{ActionState, ControlHolder, ControlRole, EngageStatus, MsgArg, PartReadout};

pub(crate) const TEXT_KEYS: &[&str] = &["hud.group.name", "hud.action.part_lost", "hud.action.running", "hud.action.rhib_busy", "hud.action.not_offered", "hud.action.no_hull"];

/// Station actions in `Bp.STATION_*` order that a player starts by hand.
const STARTABLE: [StationAction; 5] = [StationAction::BoardAndCapture, StationAction::InsertOrExtract, StationAction::RecoverSurvivors, StationAction::LayMines, StationAction::Photograph];

/// `Bp.VESSEL_*` of a vessel status.
pub(crate) fn vessel_status_gd(s: VesselStatus) -> i64 { match s { VesselStatus::Active => 0, VesselStatus::Disabled => 1, VesselStatus::Lost(_) => 2, VesselStatus::Wreck => 3 } }

/// Whether a part type performs a station action (mirror of `sim_systems::specops::serves`).
pub(crate) fn serves(action: StationAction, def: &PartDef) -> bool {
    let tag = |t: PartTag| def.tags.contains(&t);
    match action {
        StationAction::BoardAndCapture => matches!(def.function, PartFunction::BoardingKit { .. }) || tag(PartTag::BoardingKit),
        StationAction::InsertOrExtract => matches!(def.function, PartFunction::RhibDavit | PartFunction::BoardingKit { .. }) || tag(PartTag::RhibDavit) || tag(PartTag::BoardingKit),
        StationAction::RecoverSurvivors => matches!(def.function, PartFunction::RhibDavit | PartFunction::BoardingKit { .. } | PartFunction::RecoveryNet) || tag(PartTag::RhibDavit) || tag(PartTag::BoardingKit) || tag(PartTag::RecoveryNet),
        StationAction::LayMines => matches!(def.function, PartFunction::MineRail { .. }) || tag(PartTag::MineRail),
        StationAction::Photograph => matches!(def.function, PartFunction::CameraMast { .. }) || tag(PartTag::CameraMast),
        StationAction::CarryPassengers => false,
    }
}
/// The action puts the RHIB in the water (a recovery net alone does not).
fn uses_rhib(action: StationAction) -> bool { matches!(action, StationAction::BoardAndCapture | StationAction::InsertOrExtract | StationAction::RecoverSurvivors) }

/// What the player must pick for an action.
fn needs(action: StationAction) -> &'static str {
    match action {
        StationAction::BoardAndCapture | StationAction::Photograph => "track",
        StationAction::InsertOrExtract => "marker",
        StationAction::LayMines => "line",
        StationAction::RecoverSurvivors | StationAction::CarryPassengers => "none",
    }
}

/// One row of `available_actions` before conversion: (action, enabled, reason key).
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Availability { pub action: StationAction, pub enabled: bool, pub reason: Option<&'static str> }

/// The pure rule of `available_actions`. `parts` gives, per action, whether the hull has a serving part and
/// whether one of them works; `running` lists actions in progress (running or paused) on the own hull.
pub(crate) fn availability(has: impl Fn(StationAction) -> (bool, bool), running: &[StationAction], offered: Option<&[StationAction]>, afloat: bool) -> Vec<Availability> {
    let rhib_busy = running.iter().any(|a| uses_rhib(*a));
    STARTABLE.iter().filter_map(|&action| {
        let (present, works) = has(action);
        if !present { return None; }
        let reason = if !afloat { Some("hud.action.no_hull") }
            else if offered.is_some_and(|o| !o.contains(&action)) { Some("hud.action.not_offered") }
            else if !works { Some("hud.action.part_lost") }
            else if running.contains(&action) { Some("hud.action.running") }
            else if uses_rhib(action) && rhib_busy { Some("hud.action.rhib_busy") }
            else { None };
        Some(Availability { action, enabled: reason.is_none(), reason })
    }).collect()
}

/// The engagement status of a group: the first engaging mount's, else the first holding one's, else Idle.
pub(crate) fn group_status(statuses: &[EngageStatus]) -> i64 {
    if statuses.iter().any(|s| matches!(s, EngageStatus::Engaging { .. })) { 1 } else if statuses.iter().any(|s| matches!(s, EngageStatus::Holding { .. })) { 2 } else { 0 }
}

fn engage_parts(s: &EngageStatus) -> (i64, i64, sim_systems::protocol::Msg) {
    match s {
        EngageStatus::Idle => (0, -1, sim_systems::text::hold_reason(&sim_systems::protocol::HoldReason::NoTarget)),
        EngageStatus::Engaging { track, reason } => (1, crate::convert::track_to_gd(*track), sim_systems::text::fire_reason(reason)),
        EngageStatus::Holding { track, reason } => (2, crate::convert::track_to_gd(*track), sim_systems::text::hold_reason(reason)),
    }
}

/// (ready, magazine, reload seconds) of a mount's readout.
fn readout_rounds(r: Option<PartReadout>) -> (i64, i64, f64) {
    match r {
        Some(PartReadout::Rounds { ready, magazine }) => (ready as i64, magazine as i64, 0.0),
        Some(PartReadout::Cells { ready, reload_s }) => (ready as i64, ready as i64, if reload_s.is_finite() { reload_s.max(0.0) as f64 } else { 0.0 }),
        _ => (0, 0, 0.0),
    }
}

fn fin(v: f32) -> f64 { if v.is_finite() { v as f64 } else { 0.0 } }

impl BpHud {
    fn mount_dict(&self, slot: content::ids::PartSlot) -> Option<VarDictionary> {
        let ship = self.ship()?;
        let design = self.design();
        let view = ship.mounts.iter().find(|m| m.slot == slot);
        let is_decoy = design.is_some_and(|d| d.decoys.iter().any(|(s, _)| *s == slot));
        if view.is_none() && !is_decoy { return None; }
        let group = design.and_then(|d| d.mounts.iter().find(|m| m.slot == slot)).map_or(-1, |m| m.group as i64);
        let status = view.map(|v| v.status).unwrap_or_default();
        let (status_gd, track, reason) = engage_parts(&status);
        let turret = self.own_view().and_then(|v| v.turrets.iter().find(|t| t.slot == slot));
        let readout = ship.parts.iter().find(|p| p.slot == slot).map(|p| p.readout);
        let (ready, magazine, reload_s) = readout_rounds(readout);
        let held = ship.held.contains(&ControlRole::Mount(slot)) || (group >= 0 && ship.held.contains(&ControlRole::WeaponGroup(group as u8)));
        let holder = view.map(|v| v.holder).unwrap_or(if held { ControlHolder::Player } else { ControlHolder::Officer });
        let solution = view.and_then(|v| v.solution).map_or(Variant::nil(), |s| d! {
            "aim" => crate::convert::map_from_world(s.aim_point),
            "aim_alt_m" => s.aim_point.y,
            "time_of_flight_s" => fin(s.time_of_flight_s),
            "iterations" => s.iterations as i64,
            "converged" => s.converged,
            "error_m" => fin(s.error_m),
        }.to_variant());
        Some(d! {
            "slot" => crate::convert::slot_to_gd(slot),
            "name" => msg_dict(&part_name_msg(design, slot)),
            "group" => group,
            "status" => status_gd,
            "track" => track,
            "reason" => msg_dict(&reason),
            "holder" => holder.to_gd(),
            "train_deg" => turret.map_or(0.0, |t| fin(t.train_rad).to_degrees()),
            "elevation_deg" => turret.map_or(0.0, |t| fin(t.elevation_rad).to_degrees()),
            "effective_range_m" => view.map_or(0.0, |v| fin(v.effective_range_m)),
            "local_control" => view.is_some_and(|v| v.local_control),
            "solution" => solution,
            "ready" => ready,
            "magazine" => magazine,
            "reload_s" => reload_s,
        })
    }

    /// Slots `mounts()` lists, ascending.
    fn mount_slots(&self) -> Vec<content::ids::PartSlot> {
        let Some(ship) = self.ship() else { return Vec::new() };
        let mut slots: Vec<_> = ship.mounts.iter().map(|m| m.slot).collect();
        if let Some(d) = self.design() { slots.extend(d.decoys.iter().map(|(s, _)| *s)); }
        slots.sort_unstable();
        slots.dedup();
        slots
    }

    /// Station actions the sortie offers; `None` when it names no operation (all of them).
    fn offered_actions(&self) -> Option<Vec<StationAction>> {
        let op = &self.core().operation;
        if op.is_empty() { return None; }
        crate::services::services().campaign().operations.get(&content::ids::OperationId::new(op.as_str())).map(|o| o.actions.clone())
    }
}

#[godot_api(secondary)]
impl BpHud {
    /// Returns `MotionDict`; empty Dictionary without a hull. 30 Hz, `motion_changed`.
    #[func]
    fn motion(&self) -> VarDictionary {
        self.cached("motion", 0, &[Section::Motion], |h| {
            let Some(s) = h.ship() else { return VarDictionary::new() };
            let view = h.own_view();
            let route: PackedVector2Array = s.route.iter().map(|p| crate::convert::map_from_dvec2(*p)).collect();
            d! {
                "vessel" => crate::convert::entity_to_gd(s.vessel),
                "pos" => view.map_or(Vector2::ZERO, |v| crate::convert::map_from_world(v.kin.pose.pos)),
                "speed_kn" => fin(s.speed_kn),
                "heading_deg" => crate::convert::heading_deg(fin(s.heading_rad)),
                "heel_deg" => fin(s.heel_deg),
                "trim_deg" => fin(s.trim_deg),
                "ordered_speed_kn" => fin(s.ordered_speed_kn),
                "ordered_heading_deg" => crate::convert::heading_deg(fin(s.ordered_heading_rad)),
                "rudder" => view.map_or(0.0, |v| fin(v.rudder).clamp(-1.0, 1.0)),
                "power" => view.map_or(0.0, |v| fin(v.power).clamp(0.0, 1.0)),
                "route" => route,
                "designated" => crate::convert::track_to_gd(s.designated),
                "longest_envelope_m" => fin(s.longest_envelope_m),
                "cells_lost" => fin(s.cells_lost).clamp(0.0, 1.0),
                "status" => view.map_or(0, |v| vessel_status_gd(v.status)),
            }
        })
    }
    /// Gauge limits for the helm cluster, constant per design: `{ "max_speed_kn": float,
    /// "astern_speed_kn": float, "heel_caution_deg": float, "heel_limit_deg": float, "draft_m": float,
    /// "length_m": float, "beam_m": float }`.
    #[func]
    fn motion_limits(&self) -> VarDictionary {
        self.cached("motion_limits", 0, &[Section::Static], |h| {
            let Some(design) = h.design() else { return VarDictionary::new() };
            let t = &design.turn;
            let max_kn = crate::convert::mps_to_kn(if t.top_speed_mps.is_finite() { t.top_speed_mps.max(0.0) } else { 0.0 });
            let tuning = &crate::services::services().catalogue().tuning.stability;
            let capsize = if tuning.capsize_heel_deg.is_finite() && tuning.capsize_heel_deg > 0.0 { tuning.capsize_heel_deg.min(180.0) } else { 90.0 };
            let limit = capsize * sim_systems::alerts::CAPSIZE_WARN_FRACTION;
            let dims = design.prefab.dims;
            let pick = |a: f64, b: f64| if a.is_finite() && a > 0.0 { a } else if b.is_finite() { b.max(0.0) } else { 0.0 };
            d! {
                "max_speed_kn" => max_kn,
                "astern_speed_kn" => max_kn * 0.6,
                "heel_caution_deg" => limit * 0.5,
                "heel_limit_deg" => limit,
                "draft_m" => pick(t.draft_m, 0.0),
                "length_m" => pick(t.length_m, dims[0]),
                "beam_m" => pick(t.beam_m, dims[2]),
            }
        })
    }
    /// Returns `PowerDict`. 2 Hz, `power_changed`.
    #[func]
    fn power(&self) -> VarDictionary {
        self.cached("power", 0, &[Section::Power], |h| {
            let Some(s) = h.ship() else { return VarDictionary::new() };
            let p = &s.power;
            d! {
                "generation_kw" => fin(p.generation_kw).max(0.0),
                "demand_kw" => fin(p.demand_kw).max(0.0),
                "supplied_kw" => fin(p.supplied_kw).max(0.0),
                "fuel_l" => fin(p.fuel_l).max(0.0),
                "fuel" => fin(p.fuel_fraction).clamp(0.0, 1.0),
                "fuel_flow_lps" => fin(p.fuel_flow_lps).max(0.0),
                "shed_count" => s.parts.iter().filter(|x| x.shed).count() as i64,
            }
        })
    }
    /// Returns `Array[PartStaticDict]`, ascending slot. Changes only with `ship_static_changed`.
    #[func]
    fn parts_static(&self) -> Array<VarDictionary> {
        self.cached("parts_static", 0, &[Section::Static], |h| {
            let Some(design) = h.design() else { return Array::new() };
            let catalogue = crate::services::services().catalogue();
            let bp = design.blueprint();
            design.prefab.parts.iter().map(|rt| {
                let placement = bp.parts.get(rt.slot.index());
                let def = placement.and_then(|p| catalogue.part(&p.part));
                let size = def.map_or([1, 1, 1], |d| content::kit::rotated_size(d.size, rt.rot));
                let category = def.map_or(0, |d| d.category.to_gd());
                let icon = def.map(|d| d.icon.as_str().to_string()).filter(|i| !i.is_empty())
                    .unwrap_or_else(|| format!("part_{}", def.map_or("hull", |d| category_stem(d.category))));
                let group = design.mounts.iter().find(|m| m.slot == rt.slot).map_or(-1, |m| m.group as i64);
                d! {
                    "slot" => crate::convert::slot_to_gd(rt.slot),
                    "part" => StringName::from(placement.map_or("", |p| p.part.as_str())),
                    "name" => msg_dict(&part_name_msg(Some(design), rt.slot)),
                    "category" => category,
                    "compartment" => rt.compartment.map_or(-1, |c| c.0 as i64),
                    "cell" => Vector3i::new(rt.pos.x as i32, rt.pos.y as i32, rt.pos.z as i32),
                    "size" => Vector3i::new(size[0] as i32, size[1] as i32, size[2] as i32),
                    "kit" => placement.and_then(|p| p.kit).map_or(-1, |k| k.0 as i64),
                    "is_mount" => design.mounts.iter().any(|m| m.slot == rt.slot) || design.decoys.iter().any(|(s, _)| *s == rt.slot),
                    "is_emitter" => design.emitters.iter().any(|e| e.slot == rt.slot),
                    "weapon_group" => group,
                    "icon" => StringName::from(icon.as_str()),
                }
            }).collect()
        })
    }
    /// Returns `Array[PartDict]`, ascending slot, same length and order as `parts_static()`. 2 Hz.
    #[func]
    fn parts(&self) -> Array<VarDictionary> {
        self.cached("parts", 0, &[Section::Parts, Section::Static], |h| {
            let (Some(ship), Some(design)) = (h.ship(), h.design()) else { return Array::new() };
            design.prefab.parts.iter().map(|rt| {
                let view = ship.parts.binary_search_by_key(&rt.slot, |p| p.slot).ok().map(|i| &ship.parts[i]).or_else(|| ship.parts.iter().find(|p| p.slot == rt.slot));
                let (kind, a, b) = match view.map(|v| v.readout).unwrap_or_default() {
                    PartReadout::None => (0, 0.0, 0.0),
                    PartReadout::Rounds { ready, magazine } => (1, ready as f64, magazine as f64),
                    PartReadout::Cells { ready, reload_s } => (2, ready as f64, fin(reload_s)),
                    PartReadout::Output { kw } => (3, fin(kw), 0.0),
                    PartReadout::Fuel { litres } => (4, fin(litres), 0.0),
                    PartReadout::Radiating(on) => (5, on as u8 as f64, 0.0),
                    PartReadout::Running(on) => (6, on as u8 as f64, 0.0),
                    PartReadout::Closed(on) => (7, on as u8 as f64, 0.0),
                    PartReadout::Aboard(n) => (8, n as f64, 0.0),
                };
                match view {
                    Some(v) => d! {
                        "slot" => crate::convert::slot_to_gd(rt.slot),
                        "level" => v.level.to_gd(),
                        "health" => v.health as f64 / 255.0,
                        "power" => v.power.to_gd(),
                        "fuel" => v.fuel.to_gd(),
                        "data" => v.data.to_gd(),
                        "shed" => v.shed,
                        "priority" => (v.priority as i64).clamp(1, 5),
                        "readout" => d! { "kind" => kind as i64, "a" => a, "b" => b },
                    },
                    // A part the snapshot does not list yet: shown as the design built it.
                    None => d! {
                        "slot" => crate::convert::slot_to_gd(rt.slot),
                        "level" => rt.level.to_gd(),
                        "health" => rt.health.clamp(0.0, 1.0),
                        "power" => 0i64, "fuel" => 0i64, "data" => 0i64,
                        "shed" => false,
                        "priority" => (rt.priority as i64).clamp(1, 5),
                        "readout" => d! { "kind" => 0i64, "a" => 0.0, "b" => 0.0 },
                    },
                }
            }).collect()
        })
    }
    /// Returns `Array[MountDict]`, ascending slot: weapon mounts, launchers and decoy launchers. 10 Hz.
    #[func]
    fn mounts(&self) -> Array<VarDictionary> {
        self.cached("mounts", 0, &[Section::Mounts, Section::Static], |h| h.mount_slots().into_iter().filter_map(|s| h.mount_dict(s)).collect())
    }
    /// One `MountDict`, or `null` when the slot is not a mount.
    #[func]
    fn mount(&self, slot: i64) -> Variant {
        let Some(slot) = crate::convert::slot_from_gd(slot) else { return Variant::nil() };
        let all = self.mounts();
        all.iter_shared().find(|d| d.get("slot").and_then(|v| v.try_to::<i64>().ok()) == Some(slot.0 as i64)).map_or(Variant::nil(), |d| d.to_variant())
    }
    /// Weapon groups the player can take as one role (L1665): `Array[Dictionary]` with `group: int`,
    /// `name: MsgDict`, `mounts: PackedInt32Array`, `holder: int` (`Bp.HOLDER_*`), `status: int`
    /// (`Bp.ENGAGE_*` of the group's first engaged mount), `key: int` (1-based takeover key).
    #[func]
    fn weapon_groups(&self) -> Array<VarDictionary> {
        self.cached("weapon_groups", 0, &[Section::Mounts, Section::Static], |h| {
            let (Some(ship), Some(design)) = (h.ship(), h.design()) else { return Array::new() };
            let mut groups: Vec<u8> = design.mounts.iter().map(|m| m.group).collect();
            groups.sort_unstable();
            groups.dedup();
            groups.into_iter().map(|g| {
                let slots: Vec<_> = design.mounts.iter().filter(|m| m.group == g).map(|m| m.slot).collect();
                let statuses: Vec<EngageStatus> = slots.iter().filter_map(|s| ship.mounts.iter().find(|m| m.slot == *s).map(|m| m.status)).collect();
                let held = ship.held.contains(&ControlRole::WeaponGroup(g));
                d! {
                    "group" => g as i64,
                    "name" => msg_dict(&sim_systems::protocol::Msg::new("hud.group.name").arg(MsgArg::Int(g as i64 + 1))),
                    "mounts" => slots.iter().map(|s| s.0 as i32).collect::<PackedInt32Array>(),
                    "holder" => if held { 1i64 } else { 0i64 },
                    "status" => group_status(&statuses),
                    "key" => g as i64 + 1,
                }
            }).collect()
        })
    }
    /// Returns `Array[OffboardDict]`, ascending slot.
    #[func]
    fn offboard(&self) -> Array<VarDictionary> {
        self.cached("offboard", 0, &[Section::Offboard, Section::Static], |h| {
            let Some(ship) = h.ship() else { return Array::new() };
            let mut rows: Vec<_> = ship.offboard.iter().collect();
            rows.sort_by_key(|o| o.slot);
            rows.into_iter().map(|o| d! {
                "slot" => crate::convert::slot_to_gd(o.slot),
                "body" => o.body.map_or(-1, crate::convert::entity_to_gd),
                "aboard" => o.aboard as i64,
                "endurance_s" => fin(o.endurance_s).max(0.0),
                "recharging_s" => fin(o.recharging_s).max(0.0),
                "name" => msg_dict(&part_name_msg(h.design(), o.slot)),
            }).collect()
        })
    }
    /// Station actions in progress: `Array[ActionDict]`.
    #[func]
    fn actions(&self) -> Array<VarDictionary> {
        self.cached("actions", 0, &[Section::Actions], |h| {
            let Some(ship) = h.ship() else { return Array::new() };
            ship.actions.iter().map(|a| d! {
                "id" => a.id.0 as i64,
                "action" => a.action.to_gd(),
                "actor" => crate::convert::entity_to_gd(a.actor),
                "state" => a.state.to_gd(),
                "progress" => fin(a.progress).clamp(0.0, 1.0),
                "block" => a.block.map_or_else(empty_msg, |b| msg_dict(&sim_systems::text::action_block(b))),
                "at" => a.at.filter(|p| p.is_finite()).map_or(Variant::nil(), |p| crate::convert::map_from_dvec2(p).to_variant()),
            }).collect()
        })
    }
    /// Station actions this sortie and hull allow right now: `Array[Dictionary]` with `action: int`
    /// (`Bp.STATION_*`), `enabled: bool`, `reason: MsgDict` (why not, empty when enabled),
    /// `needs: StringName` (`&"none"`, `&"track"`, `&"marker"`, `&"line"`).
    #[func]
    fn available_actions(&self) -> Array<VarDictionary> {
        self.cached("available_actions", 0, &[Section::Actions, Section::Static], |h| {
            let (Some(ship), Some(design)) = (h.ship(), h.design()) else { return Array::new() };
            let catalogue = crate::services::services().catalogue();
            let bp = design.blueprint();
            let has = |action: StationAction| {
                let mut present = false;
                let mut works = false;
                for (i, p) in bp.parts.iter().enumerate() {
                    let Some(def) = catalogue.part(&p.part) else { continue };
                    if !serves(action, def) { continue; }
                    present = true;
                    let level = ship.parts.iter().find(|v| v.slot.index() == i).map(|v| v.level);
                    if level.is_none_or(|l| l != content::enums::DamageLevel::Destroyed) { works = true; }
                }
                (present, works)
            };
            let running: Vec<StationAction> = ship.actions.iter().filter(|a| matches!(a.state, ActionState::Running | ActionState::Paused)).map(|a| a.action).collect();
            let afloat = !h.own_view().is_some_and(|v| matches!(v.status, VesselStatus::Lost(_) | VesselStatus::Wreck));
            let offered = h.offered_actions();
            availability(has, &running, offered.as_deref(), afloat).into_iter().map(|a| d! {
                "action" => a.action.to_gd(),
                "enabled" => a.enabled,
                "reason" => a.reason.map_or_else(empty_msg, |k| msg_dict(&sim_systems::protocol::Msg::new(k))),
                "needs" => StringName::from(needs(a.action)),
            }).collect()
        })
    }
    /// Returns `Array[ObjectiveDict]`: the primary first, then ghost conditions in briefing order.
    #[func]
    fn objectives(&self) -> Array<VarDictionary> {
        self.cached("objectives", 0, &[Section::Objectives], |h| {
            let Some(s) = h.snap() else { return Array::new() };
            let mut list: Vec<_> = s.objectives.iter().filter(|o| o.state != sim_systems::protocol::ObjectiveState::Hidden).collect();
            list.sort_by_key(|o| (o.ghost, o.id));
            list.into_iter().map(|o| d! {
                "id" => o.id.0 as i64,
                "ghost" => o.ghost,
                "state" => o.state.to_gd(),
                "text" => msg_dict(&o.text),
                "progress" => fin(o.progress).clamp(0.0, 1.0),
                "marker" => o.marker.filter(|m| m.is_finite()).map_or(Variant::nil(), |m| crate::convert::map_from_dvec2(m).to_variant()),
            }).collect()
        })
    }
    /// Returns `Array[AllyDict]`: own-side hulls other than the viewer's (co-op partners, SEXTANT, prizes).
    #[func]
    fn allies(&self) -> Array<VarDictionary> {
        self.cached("allies", 0, &[Section::Allies], |h| {
            let Some(s) = h.snap() else { return Array::new() };
            let own = h.ship().map(|x| x.vessel);
            let core = h.core();
            s.allies.iter().filter(|a| Some(a.vessel) != own).map(|a| {
                let view = s.vessels.iter().find(|v| v.entity == a.vessel);
                let name = a.player.and_then(|p| core.players.get(&p).cloned()).or_else(|| core.intros.get(&a.vessel).map(|f| f.name.clone())).unwrap_or_default();
                d! {
                    "vessel" => crate::convert::entity_to_gd(a.vessel),
                    "player" => a.player.map_or(-1, crate::convert::player_to_gd),
                    "name" => GString::from(name.as_str()),
                    "speed_kn" => fin(a.speed_kn).max(0.0),
                    "cells_lost" => fin(a.cells_lost).clamp(0.0, 1.0),
                    "burning" => a.burning,
                    "flooding" => a.flooding,
                    "emcon" => a.emcon.to_gd(),
                    "officers_in_command" => a.officers_in_command,
                    "pos" => view.map_or(Vector2::ZERO, |v| crate::convert::map_from_world(v.kin.pose.pos)),
                    "heading_deg" => view.map_or(0.0, |v| crate::convert::heading_deg(v.kin.pose.heading_rad())),
                }
            }).collect()
        })
    }
}

/// Icon stem of a part category (`res://ui/icons/part_<stem>.svg`).
fn category_stem(c: content::part::PartCategory) -> &'static str {
    use content::part::PartCategory as C;
    match c {
        C::Hull => "hull", C::Propulsion => "propulsion", C::Power => "power", C::Fuel => "fuel", C::Control => "control", C::Gun => "gun",
        C::Missile => "missile", C::Underwater => "underwater", C::Defence => "defence", C::Offboard => "offboard", C::Sensor => "sensor",
        C::Crew => "crew", C::DamageControl => "damage_control", C::SpecialOps => "special_ops",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn availability_follows_parts_offer_and_rhib() {
        let has = |a: StationAction| match a { StationAction::BoardAndCapture => (true, true), StationAction::RecoverSurvivors => (true, true), StationAction::Photograph => (true, false), _ => (false, false) };
        let rows = availability(has, &[], None, true);
        assert_eq!(rows.iter().map(|r| r.action).collect::<Vec<_>>(), vec![StationAction::BoardAndCapture, StationAction::RecoverSurvivors, StationAction::Photograph]);
        assert!(rows[0].enabled && rows[1].enabled);
        assert_eq!(rows[2].reason, Some("hud.action.part_lost"));
        // A boarding party out holds the RHIB.
        let rows = availability(has, &[StationAction::BoardAndCapture], None, true);
        assert_eq!(rows[0].reason, Some("hud.action.running"));
        assert_eq!(rows[1].reason, Some("hud.action.rhib_busy"));
        // The operation offers only photography; a sunk hull offers nothing.
        let rows = availability(has, &[], Some(&[StationAction::Photograph]), true);
        assert_eq!(rows[0].reason, Some("hud.action.not_offered"));
        assert!(availability(has, &[], None, false).iter().all(|r| r.reason == Some("hud.action.no_hull")));
    }

    #[test]
    fn group_status_prefers_engaging() {
        use sim_systems::ids::TrackId;
        use sim_systems::protocol::{FireReason, HoldReason};
        let hold = EngageStatus::Holding { track: TrackId(3), reason: HoldReason::Slewing };
        let fire = EngageStatus::Engaging { track: TrackId(3), reason: FireReason::Doctrine };
        assert_eq!(group_status(&[]), 0);
        assert_eq!(group_status(&[EngageStatus::Idle, hold]), 2);
        assert_eq!(group_status(&[hold, fire]), 1);
    }

    #[test]
    fn every_category_has_an_icon() {
        let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../../game/ui/icons");
        for c in content::part::PartCategory::VARIANTS {
            let path = format!("{dir}/part_{}.svg", category_stem(*c));
            assert!(std::path::Path::new(&path).exists(), "{path}");
        }
    }
}
