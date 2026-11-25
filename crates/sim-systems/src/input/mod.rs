//! Unit `sys-input`. Player commands and continuous input applied to ships: orders, role takeover, manual
//! aiming and helm, and the shot log. DESIGN.md L108 (orders are identical in both views), L112 (orders given
//! while paused apply on resume), L949-953 (designations, no world rewind, role control), L951 (shot log and
//! the rewind trigger).
//!
//! Manual shots are designations stamped with the client render tick. The server fires at its current tick:
//! a track designation converts through the server's current track estimate; a direction-only shot is
//! corrected by extrapolating the aimed-at target from the client render tick to the server tick. There is no
//! world-state rewind at launch. [`PoseHistory`] is the seam for the conditional rewind (only if the trigger
//! at L951 fires: at least 200 shots per RTT bucket and the 100-200 ms bucket's hit rate 25% below the
//! under-30 ms bucket): the server keeps 200 ms of target poses and tests direction-only shots against the
//! pose at the client render tick. It stays unused until then.
//!
//! # Call protocol (for `sys-sim`)
//! 1. Phase 1, per command of a player with a ship: [`apply_command`]. `Applied::NotShip` goes to the Sim's
//!    own handling (`SetStance` and `SetIdentity` arrive there already validated). After an `Ok`, when
//!    [`ControlState::settings_dirty`] is set, the Sim recompiles the ship's graph with `ShipDesign::recompile`
//!    and clears the flag.
//! 2. Phase 8, per ship after its graph tick: `ship::controls::build_controls` (it retires pending shots that
//!    fired, expires stale ones, and lays and fires held mounts from the side picture alone, so a held mount
//!    works even without the next step), then for every slot in [`held_mounts`] call [`manual_mount_order`],
//!    which lays the same shot with the server's truth for the lag correction: put its order into that slot's
//!    `PartOrder::Mount`, replacing the picture-based one, and append its rows to the shot log (emitting
//!    `ShotLogged`). Only `manual_mount_order` writes shot-log rows.
//! 3. Phase 12: [`resolve_shots`] over the tick's physical events. Phase 13: [`record_poses`].
//!
//! # Manual aim
//! The player aims along a line of sight; the server supplies the gun elevation for the range, and the
//! player supplies the lead (the lead the officers compute is what a hand-laid gun gives up). The aimed-at
//! point is the vessel nearest the aim ray inside a small angular gate, moved on by its velocity from the
//! client render tick to the server tick (at most 1 s); else the point where the ray meets the sea; else
//! (aimed above the horizon) the ray itself with no elevation added. A track shot is laid by the ballistic
//! solver on the server's current track estimate plus the offset, with full lead. Fire is refused whenever the
//! aimed-at contact is a Friend (L500). A hand-fired shot is one round from every barrel; trigger fire runs at
//! the mount's full cyclic rate.
use crate::prelude::*;
use crate::protocol::command::{Command, CommandEnvelope, ControlRole, PlayerInput, ShotDesignation};
use crate::protocol::event::{RejectReason, ShotLogEntry};
use crate::ship::Ship;
use crate::tracks::SidePicture;
use sim_core::parts::MountOrder;
use crate::doctrine::MountInfo;
use crate::graph::PortValue;
use crate::guidance::gun::{self, RangeRow};
use crate::tracks::Track;
use content::graph::{MountLimits, WeaponClass};
use content::part::{GunDef, PartFunction};
use sim_core::parts::{PartRuntime, PartState};

/// A manual shot waiting for its mount to be ready.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct PendingShot { pub role: ControlRole, pub shot: ShotDesignation, pub client_render_tick: u64, pub rtt_ms: u16, pub issued_tick: u64 }

/// Takeover and input state of one ship.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ControlState {
    /// Roles the player holds, ascending.
    pub held: Vec<ControlRole>,
    pub input: PlayerInput,
    pub pending: Vec<PendingShot>,
    /// Highest command sequence number applied (duplicates are dropped).
    pub last_seq: u32,
    /// True while the player is disconnected and the officers run the hull (L838).
    pub officers_in_command: bool,
    /// Set by [`apply_command`] when an L0-L2 settings or rules change needs the graph recompiled; the Sim
    /// recompiles and clears it. Added by `sys-input`.
    #[serde(default)]
    pub settings_dirty: bool,
    /// Round-trip time of the player's latest command, for shot-log rows of trigger fire.
    #[serde(default)]
    pub(crate) last_rtt_ms: u16,
    /// `MountState::fired` per held mount as last seen by `build_controls`, to retire pending shots that fired.
    #[serde(default)]
    pub(crate) fired_seen: Vec<(PartSlot, u32)>,
    /// Transient local-control tracks per mount (L394), kept between graph ticks for `tracks::local_track`.
    #[serde(default)]
    pub(crate) local_tracks: Vec<(PartSlot, Track)>,
    /// Tick of the last launch order per launcher (guards against a held launch level).
    #[serde(default)]
    pub(crate) last_launch: Vec<(PartSlot, u64)>,
}
impl ControlState { pub fn holds(&self, role: ControlRole) -> bool { self.held.contains(&role) } }

/// 200 ms of poses per entity (12 ticks), newest last. Seam for the conditional rewind; see module docs.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct PoseHistory { pub ticks: Vec<u64>, pub poses: BTreeMap<EntityId, Vec<Pose>> }
pub const POSE_HISTORY_TICKS: usize = 12;

/// Outcome of applying one command to a ship.
#[derive(Clone, Debug, PartialEq)]
pub enum Applied { Ok, Rejected(RejectReason), /// Not a ship command: the Sim handles it (pause, time rate, alerts, queries, splash, actions).
    NotShip }

// ---------------------------------------------------------------------------------------------------------
// Limits on player data (a hostile or buggy client must not grow server state without bound).

/// Longest planned route, waypoints.
pub const MAX_ROUTE: usize = 64;
/// Manual shots queued per ship; the oldest is dropped beyond this.
pub const MAX_PENDING: usize = 8;
/// Manual launch requests queued per ship.
pub const MAX_LAUNCHES: usize = 16;
/// A pending shot that has not fired within this many ticks (2 s) is dropped: a shot fired long after the
/// click would surprise the player.
pub const PENDING_TTL_TICKS: u64 = 120;
/// Largest coordinate accepted in an order, metres.
const MAX_COORD_M: f64 = 1.0e7;
/// Fastest ordered speed accepted, knots.
const MAX_SPEED_KN: f64 = 200.0;
/// Largest offset of a track shot from the track estimate, metres.
const MAX_SHOT_OFFSET_M: f64 = 200.0;
/// Deepest ordered weapon depth, metres.
const MAX_DEPTH_M: f64 = 1_000.0;
/// Doctrine entries per list.
const MAX_DOCTRINE: usize = 64;
/// Repair list entries.
const MAX_REPAIR_ORDER: usize = 32;
/// Damage-control teams a command may name.
const MAX_TEAMS: u8 = 32;
/// Direction-only correction never extrapolates further than this, seconds.
const MAX_CORRECTION_S: f64 = 1.0;
/// Smallest angular gate around the aim ray for picking the aimed-at vessel, radians (about 1.1 degrees),
/// widened to the target's half-length plus 20 m at its range.
const AIM_GATE_RAD: f64 = 0.02;
const AIM_GATE_PAD_M: f64 = 20.0;
/// Farthest a hand-aimed shot is laid, metres.
const MAX_MANUAL_RANGE_M: f64 = 60_000.0;

fn is_pvp(mode: GameMode) -> bool { matches!(mode, GameMode::PvpElimination | GameMode::PvpEscort) }
fn finite2(v: DVec2) -> bool { v.is_finite() && v.x.abs() <= MAX_COORD_M && v.y.abs() <= MAX_COORD_M }

/// Effective stance of a track: a datalink Friend stays Friend (rule 1 cannot be overridden), otherwise a manual
/// or graph override wins.
pub(crate) fn effective_stance(t: &Track) -> Stance { if t.stance == Stance::Friend { Stance::Friend } else { t.stance_override.unwrap_or(t.stance) } }

fn live_track(picture: &SidePicture, id: TrackId) -> Option<&Track> { if id.is_none() { None } else { picture.get(id).filter(|t| t.is_live()) } }

/// The role that puts a mount under the player's hand, if any: the mount itself, else its weapon group.
pub(crate) fn holding_role(control: &ControlState, m: &MountInfo) -> Option<ControlRole> {
    if control.officers_in_command { return None; }
    if control.holds(ControlRole::Mount(m.slot)) { return Some(ControlRole::Mount(m.slot)); }
    if control.holds(ControlRole::WeaponGroup(m.group)) { return Some(ControlRole::WeaponGroup(m.group)); }
    None
}

fn role_covers(role: ControlRole, m: &MountInfo) -> bool { match role { ControlRole::Mount(s) => s == m.slot, ControlRole::WeaponGroup(g) => g == m.group, _ => false } }

fn hand_aimable(w: WeaponClass) -> bool { matches!(w, WeaponClass::Gun | WeaponClass::PointDefenceGun) }
fn guided(w: WeaponClass) -> bool { matches!(w, WeaponClass::AntiShipMissile | WeaponClass::Sam | WeaponClass::PointDefenceMissile | WeaponClass::Torpedo | WeaponClass::DepthCharge) }

/// Gun mounts the player holds (directly or by weapon group), ascending slot: the slots the Sim passes to
/// [`manual_mount_order`]. Unit: `sys-input`.
pub fn held_mounts(ship: &Ship) -> Vec<PartSlot> {
    ship.design.mounts.iter().filter(|m| hand_aimable(m.weapon) && holding_role(&ship.control, m).is_some()).map(|m| m.slot).collect()
}

/// Whether a command, once applied, changes settings or rules the compiled graph bakes in (the Sim then
/// recompiles). Unit: `sys-input`.
pub fn needs_recompile(cmd: &Command) -> bool {
    match cmd {
        Command::SetPreset { .. } | Command::SetSlider { .. } | Command::SetDoctrine { .. } | Command::SetRules(_) | Command::SetEmcon(_)
        | Command::SetPowerPriority { .. } | Command::ReorderRepairs(_) | Command::RevertAll(_) => true,
        Command::Revert(k) => !matches!(k, crate::protocol::command::OverrideKey::TrackStance(_) | crate::protocol::command::OverrideKey::TrackIdentity(_)),
        _ => false,
    }
}

/// Apply one ship-level command: orders (course, speed, station, designation), takeover, manual fire and
/// launch requests, emission setting, L0-L2 settings, damage-control commands, offboard orders. Checks the
/// mode and ruleset (L3 and L4 edits are refused outside dry dock and splash; Friend tracks cannot be
/// designated for fire). Unit: `sys-input`.
///
/// - Sequence numbers start at 1 per player: a command whose `seq` is not above `last_seq` is a duplicate.
///   `seq` 0 marks an unsequenced command from an in-process source (trials, bots, tutorials) and is never
///   deduplicated. A rejected command still consumes its number, so a resend is not applied twice.
/// - `SetStance` and `SetIdentity` change the side picture, which this function cannot write: they are
///   validated here (the track must exist; a Friend cannot be given another stance) and returned as
///   `NotShip` for the Sim to apply through `ident::stance::set_override`. So are track-override reverts.
/// - Splash commands and `RearmScripts` are refused outside the splash basin; elsewhere they are `NotShip`.
/// - Values from the wire are checked: non-finite or out-of-range numbers are `InvalidValue`, unknown parts
///   `NoSuchPart`, unknown or dropped tracks `NoSuchTrack`.
pub fn apply_command(ship: &mut Ship, cmd: &CommandEnvelope, picture: &SidePicture, mode: GameMode, ruleset: &content::ruleset::Ruleset, tick: u64) -> Applied {
    if cmd.seq != 0 {
        if cmd.seq <= ship.control.last_seq { return Applied::Rejected(RejectReason::Duplicate); }
        ship.control.last_seq = cmd.seq;
    }
    ship.control.last_rtt_ms = cmd.rtt_ms;
    let out = apply_inner(ship, cmd, picture, mode, ruleset, tick);
    if out == Applied::Ok && needs_recompile(&cmd.command) { ship.control.settings_dirty = true; }
    out
}

fn apply_inner(ship: &mut Ship, cmd: &CommandEnvelope, picture: &SidePicture, mode: GameMode, ruleset: &content::ruleset::Ruleset, tick: u64) -> Applied {
    use Applied::{NotShip, Ok as Done, Rejected as No};
    use RejectReason as R;
    let has_mount = |s: PartSlot| ship.design.mounts.iter().any(|m| m.slot == s);
    match &cmd.command {
        Command::SetCourse { waypoints, speed_kn, append } => {
            if waypoints.iter().any(|w| !finite2(*w)) { return No(R::InvalidValue); }
            let speed = match speed_kn { Some(v) if !(v.is_finite() && *v >= 0.0) => return No(R::InvalidValue), Some(v) => Some(v.min(MAX_SPEED_KN)), None => None };
            let o = &mut ship.orders;
            if *append { if o.route.len() + waypoints.len() > MAX_ROUTE { return No(R::InvalidValue); } } else if waypoints.len() > MAX_ROUTE { return No(R::InvalidValue); }
            if !*append { o.route.clear(); }
            o.route.extend_from_slice(waypoints);
            if speed.is_some() { o.speed_kn = speed; }
            o.station = None;
            o.formation = None;
            Done
        }
        Command::SetSpeed { speed_kn } => {
            if !(speed_kn.is_finite() && *speed_kn >= 0.0) { return No(R::InvalidValue); }
            ship.orders.speed_kn = Some(speed_kn.min(MAX_SPEED_KN));
            Done
        }
        Command::HoldStation { at, heading_rad } => {
            if !finite2(*at) || heading_rad.is_some_and(|h| !h.is_finite()) { return No(R::InvalidValue); }
            let h = heading_rad.map(|h| h.rem_euclid(core::f64::consts::TAU));
            ship.orders.station = Some((*at, h));
            ship.orders.route.clear();
            ship.orders.formation = None;
            Done
        }
        Command::Designate { track } => {
            match track {
                None => { ship.orders.designated = TrackId::NONE; Done }
                Some(id) => {
                    let Some(t) = live_track(picture, *id) else { return No(R::NoSuchTrack) };
                    if effective_stance(t) == Stance::Friend { return No(R::FriendCannotBeEngaged); }
                    ship.orders.designated = *id;
                    Done
                }
            }
        }
        Command::TakeControl { role, on } => {
            match role {
                ControlRole::Mount(s) => if !has_mount(*s) { return No(R::NoSuchPart) },
                ControlRole::WeaponGroup(g) => if !ship.design.mounts.iter().any(|m| m.group == *g) { return No(R::NoSuchPart) },
                ControlRole::Helm | ControlRole::Sensors => {}
            }
            let c = &mut ship.control;
            match c.held.binary_search(role) {
                Ok(i) => if !*on { c.held.remove(i); },
                Err(i) => if *on { c.held.insert(i, *role); },
            }
            if !*on { c.pending.retain(|p| p.role != *role); }
            Done
        }
        Command::Fire { role, shot } => {
            let held = match role {
                ControlRole::Mount(s) => {
                    let Some(m) = ship.design.mounts.iter().find(|m| m.slot == *s) else { return No(R::NoSuchPart) };
                    if !hand_aimable(m.weapon) { return No(R::InvalidValue); }
                    ship.control.holds(*role) || ship.control.holds(ControlRole::WeaponGroup(m.group))
                }
                ControlRole::WeaponGroup(g) => {
                    if !ship.design.mounts.iter().any(|m| m.group == *g && hand_aimable(m.weapon)) { return No(R::NoSuchPart); }
                    ship.control.holds(*role)
                }
                ControlRole::Helm | ControlRole::Sensors => return No(R::InvalidValue),
            };
            if !held || ship.control.officers_in_command { return No(R::RoleNotHeld); }
            let shot = match *shot {
                ShotDesignation::Track { track, offset_m } => {
                    let Some(t) = live_track(picture, track) else { return No(R::NoSuchTrack) };
                    if effective_stance(t) == Stance::Friend { return No(R::FriendCannotBeEngaged); }
                    if !offset_m.is_finite() { return No(R::InvalidValue); }
                    ShotDesignation::Track { track, offset_m: offset_m.clamp_length_max(MAX_SHOT_OFFSET_M) }
                }
                ShotDesignation::Direction { dir_local } => {
                    if !dir_local.is_finite() || dir_local.length_squared() < 1e-12 { return No(R::InvalidValue); }
                    ShotDesignation::Direction { dir_local: dir_local.normalize() }
                }
            };
            let p = &mut ship.control.pending;
            if p.len() >= MAX_PENDING { p.remove(0); }
            p.push(PendingShot { role: *role, shot, client_render_tick: cmd.client_render_tick.min(tick), rtt_ms: cmd.rtt_ms, issued_tick: tick });
            Done
        }
        Command::Launch { slot, track, bearing_rad, depth_m } => {
            let Some(m) = ship.design.mounts.iter().find(|m| m.slot == *slot) else { return No(R::NoSuchPart) };
            if !guided(m.weapon) { return No(R::NoSuchPart); }
            if bearing_rad.is_some_and(|b| !b.is_finite()) || depth_m.is_some_and(|d| !(d.is_finite() && (0.0..=MAX_DEPTH_M).contains(&d))) { return No(R::InvalidValue); }
            let track = match track {
                Some(id) => {
                    let Some(t) = live_track(picture, *id) else { return No(R::NoSuchTrack) };
                    if effective_stance(t) == Stance::Friend { return No(R::FriendCannotBeEngaged); }
                    *id
                }
                None => {
                    if bearing_rad.is_none() && depth_m.is_none() { return No(R::InvalidValue); }
                    TrackId::NONE
                }
            };
            let l = &mut ship.orders.launches;
            if l.len() >= MAX_LAUNCHES { return No(R::ConditionsNotMet); }
            l.push(crate::ship::LaunchRequest { slot: *slot, track, bearing_rad: bearing_rad.map(|b| b.rem_euclid(core::f64::consts::TAU)), depth_m: *depth_m });
            Done
        }
        Command::FireDecoy { slot } => {
            if !ship.design.decoys.iter().any(|d| d.0 == *slot) { return No(R::NoSuchPart); }
            if !ship.orders.decoys.contains(slot) { ship.orders.decoys.push(*slot); }
            Done
        }
        Command::SetEmcon(e) => { ship.settings.emcon = *e; Done }
        Command::SetStance { track, stance } => {
            let Some(t) = live_track(picture, *track) else { return No(R::NoSuchTrack) };
            if t.stance == Stance::Friend && stance.is_some_and(|s| s != Stance::Friend) { return No(R::FriendCannotBeEngaged); }
            NotShip
        }
        Command::SetIdentity { track, .. } => {
            if live_track(picture, *track).is_none() { return No(R::NoSuchTrack); }
            NotShip
        }
        Command::SetPreset { mount, preset } => {
            match mount {
                Some(s) => { if !has_mount(*s) { return No(R::NoSuchPart); } ship.settings.mount_presets.insert(*s, *preset); }
                None => ship.settings.preset = *preset,
            }
            Done
        }
        Command::SetSlider { key, value } => {
            let Some(def) = crate::officers::tuning::slider(key) else { return No(R::InvalidValue) };
            if !value.is_finite() { return No(R::InvalidValue); }
            let (lo, hi) = if def.min <= def.max { (def.min, def.max) } else { (def.max, def.min) };
            ship.settings.sliders.insert(def.key.to_string(), value.clamp(lo, hi));
            Done
        }
        Command::SetDoctrine { mount, entry } => {
            if entry.effective_range_m.is_some_and(|r| !(r.is_finite() && r >= 0.0)) { return No(R::InvalidValue); }
            let list = match mount {
                Some(s) => { if !has_mount(*s) { return No(R::NoSuchPart); } ship.settings.mount_doctrine.entry(*s).or_default() }
                None => &mut ship.settings.doctrine,
            };
            match list.iter_mut().find(|e| e.weapon == entry.weapon && e.target == entry.target) {
                Some(e) => *e = *entry,
                None => { if list.len() >= MAX_DOCTRINE { return No(R::InvalidValue); } list.push(*entry); }
            }
            Done
        }
        Command::SetRules(rules) => {
            if ruleset.kind.max_layer() < ControlLayer::L2Rules { return No(R::NotAllowedByRuleset); }
            if rules.len() > content::graph::MAX_RULES { return No(R::TooManyRules); }
            ship.rules = rules.clone();
            Done
        }
        Command::SetPowerPriority { slot, priority } => {
            if !(1..=5).contains(priority) { return No(R::InvalidValue); }
            if !ship.design.consumer_slots.contains(slot) { return No(R::NoSuchPart); }
            ship.settings.power_priority.insert(*slot, *priority);
            Done
        }
        Command::Revert(key) => {
            use crate::protocol::command::OverrideKey;
            if matches!(key, OverrideKey::TrackStance(_) | OverrideKey::TrackIdentity(_)) { return NotShip; }
            crate::officers::tuning::revert(ship, key);
            Done
        }
        Command::RevertAll(role) => {
            let keys: Vec<_> = crate::officers::tuning::overrides(ship, Some(*role)).into_iter().map(|o| o.key).collect();
            for k in &keys { crate::officers::tuning::revert(ship, k); }
            Done
        }
        Command::ReorderRepairs(order) => {
            if order.len() > MAX_REPAIR_ORDER { return No(R::InvalidValue); }
            let mut seen = order.clone();
            seen.sort_unstable();
            if seen.windows(2).any(|w| w[0] == w[1]) { return No(R::InvalidValue); }
            ship.settings.repair_order = order.clone();
            Done
        }
        Command::AssignTeam { team, order } => {
            if *team >= MAX_TEAMS { return No(R::InvalidValue); }
            let pins = &mut ship.stages.dc.pinned;
            pins.retain(|p| p.0 != *team);
            if let Some(o) = order { let i = pins.partition_point(|p| p.0 < *team); pins.insert(i, (*team, *o)); }
            Done
        }
        Command::Pump { slot, running, target } => {
            if !ship.design.pumps.contains(slot) { return No(R::NoSuchPart); }
            let m = &mut ship.stages.dc.manual_pumps;
            m.retain(|p| p.0 != *slot);
            let i = m.partition_point(|p| p.0 < *slot);
            m.insert(i, (*slot, *running, *target));
            Done
        }
        Command::Door { slot, closed } => {
            if !ship.design.doors.contains(slot) { return No(R::NoSuchPart); }
            let m = &mut ship.stages.dc.manual_doors;
            m.retain(|d| d.0 != *slot);
            let i = m.partition_point(|d| d.0 < *slot);
            m.insert(i, (*slot, *closed));
            Done
        }
        Command::FloodValve { compartment, open } => {
            let m = &mut ship.stages.dc.manual_valves;
            m.retain(|v| v.0 != *compartment);
            let i = m.partition_point(|v| v.0 < *compartment);
            m.insert(i, (*compartment, *open));
            Done
        }
        Command::FloodMagazine { slot } => {
            if slot.index() >= ship.design.prefab.parts.len() { return No(R::NoSuchPart); }
            let m = &mut ship.stages.dc.manual_magazine_floods;
            if let Err(i) = m.binary_search(slot) { m.insert(i, *slot); }
            Done
        }
        Command::Offboard { slot, order } => {
            use crate::protocol::command::OffboardOrder as O;
            if !ship.design.bays.contains(slot) { return No(R::NoSuchPart); }
            let valid = match order {
                O::Launch | O::Recall | O::Shadow(_) => true,
                O::Orbit { centre, radius_m } => finite2(*centre) && radius_m.is_finite() && *radius_m > 0.0 && *radius_m <= MAX_COORD_M,
                O::Waypoint(p) => finite2(*p),
            };
            if !valid { return No(R::InvalidValue); }
            if let O::Shadow(t) = order { if live_track(picture, *t).is_none() { return No(R::NoSuchTrack); } }
            if crate::offboard::order(&mut ship.offboard, *slot, *order) { Done } else { No(R::ConditionsNotMet) }
        }
        Command::SplashApplyDesign(_) | Command::RearmScripts => if mode == GameMode::Splash { NotShip } else { No(R::OnlyInDryDockOrSplash) },
        Command::SplashRewind { .. } | Command::SplashRestart => if mode == GameMode::Splash { NotShip } else { No(R::NotAllowedInMode) },
        Command::Pause(_) | Command::RequestTimeRate(_) => if is_pvp(mode) { No(R::NotAllowedInMode) } else { NotShip },
        Command::StartAction { .. } | Command::CancelAction(_) | Command::Prize { .. } | Command::Acknowledge(_) | Command::SilenceAlerts
        | Command::Query(_) | Command::Withdraw => NotShip,
    }
}

/// Store the newest continuous input for a ship; older `input_tick`s are ignored. Real code.
pub fn set_input(control: &mut ControlState, input: PlayerInput) { if input.input_tick >= control.input.input_tick { control.input = input; } }

// ---------------------------------------------------------------------------------------------------------
// Manual aim (shared with `ship::controls`)

/// A vessel the player may be aiming at: position now, velocity, half-length, and whether firing at it is
/// refused (a Friend).
#[derive(Clone, Copy, Debug)]
pub(crate) struct AimTarget { pub entity: EntityId, pub pos: DVec3, pub vel: DVec3, pub half_len_m: f64, pub friend: bool }

/// Gun facts for laying a mount by hand or by the officers.
#[derive(Clone, Copy)]
pub(crate) struct GunFacts<'a> { pub table: Option<&'a [RangeRow]>, pub def: Option<&'a GunDef>, pub drag_k: f64 }

pub(crate) fn gun_facts<'a>(m: &MountInfo, tables: &'a BTreeMap<PartSlot, Arc<Vec<RangeRow>>>, catalogue: &'a Catalogue) -> GunFacts<'a> {
    let def = catalogue.parts.get_index(m.part.index()).and_then(|(_, d)| match &d.function { PartFunction::Gun(g) => Some(g), _ => None });
    let drag_k = def.and_then(|g| catalogue.bands.iter().find(|b| b.band == g.band)).map(|b| b.drag_k).filter(|k| k.is_finite() && *k >= 0.0).unwrap_or(0.0);
    GunFacts { table: tables.get(&m.slot).map(|t| t.as_slice()), def, drag_k }
}

/// `MountOrder::max_rounds` for one hand-fired shot: one round from every barrel (a salvo of the mount).
/// Continuous fire uses 0, sim-core's "no cap", so the mount keeps its full cyclic rate.
pub(crate) fn salvo_rounds(def: Option<&GunDef>) -> u8 { def.map(|g| g.barrels).unwrap_or(1).max(1) }

/// Low-angle gun elevation above the horizontal to reach a point `range_m` away and `dy_m` higher, from the
/// mount's range table (target at muzzle height) plus the angle of sight; without a table, by
/// `gun::elevation_for`. Beyond the gun's reach the table's last row (its longest range) is used.
pub(crate) fn gun_elevation(g: &GunFacts<'_>, muzzle_mps: f64, range_m: f64, dy_m: f64) -> f64 {
    let sight = libm::atan2(dy_m, range_m.max(1e-6));
    if let Some(t) = g.table.filter(|t| !t.is_empty()) {
        let e = if range_m <= t[0].range_m {
            if t[0].range_m > 0.0 { t[0].elevation_rad * (range_m / t[0].range_m).max(0.0) } else { t[0].elevation_rad }
        } else {
            let i = t.partition_point(|r| r.range_m < range_m);
            if i >= t.len() { t[t.len() - 1].elevation_rad } else {
                let (a, b) = (&t[i - 1], &t[i]);
                let w = if b.range_m > a.range_m { (range_m - a.range_m) / (b.range_m - a.range_m) } else { 0.0 };
                a.elevation_rad + (b.elevation_rad - a.elevation_rad) * w
            }
        };
        let e = e + sight;
        return if e.is_finite() { e } else { sight };
    }
    if muzzle_mps.is_finite() && muzzle_mps > 0.0 {
        if let Some((e, _)) = gun::elevation_for(range_m, dy_m, muzzle_mps, g.drag_k) { if e.is_finite() { return e; } }
    }
    sight
}

/// Clamp mount angles to the limits in force. Training keeps the limits' representation: inside
/// `[train_min, train_max]` for a limited arc (choosing the nearer end across the dead arc), `(-pi, pi]` for a
/// full circle.
pub(crate) fn clamp_to_limits(train: f64, elev: f64, lim: &MountLimits) -> (f64, f64) {
    use core::f64::consts::{PI, TAU};
    let d2r = |d: f64| if d.is_finite() { d.to_radians() } else { 0.0 };
    let (tmin, tmax) = (d2r(lim.train_min_deg), d2r(lim.train_max_deg));
    let (emin, emax) = (d2r(lim.elevation_min_deg), d2r(lim.elevation_max_deg));
    let train = if train.is_finite() { train } else { 0.0 };
    let elev = if elev.is_finite() { elev } else { 0.0 };
    let elev = if emin <= emax { elev.clamp(emin, emax) } else { elev };
    let t = if tmax - tmin >= TAU - 1e-9 || tmax <= tmin {
        let w = train.rem_euclid(TAU);
        if w > PI { w - TAU } else { w }
    } else {
        let w = tmin + (train - tmin).rem_euclid(TAU);
        if w <= tmax { w } else {
            let past = w - tmax;
            let before = tmin + TAU - w;
            if past <= before { tmax } else { tmin }
        }
    };
    (t, elev)
}

/// World fire direction toward `point` from `muzzle` with gun elevation for its range.
pub(crate) fn lay_toward(g: &GunFacts<'_>, muzzle_mps: f64, muzzle: DVec3, point: DVec3) -> Option<DVec3> {
    let d = point - muzzle;
    if !d.is_finite() { return None; }
    let r = libm::sqrt(d.x * d.x + d.z * d.z);
    if r < 1.0 { return if d.length_squared() > 1e-12 { Some(d.normalize()) } else { None }; }
    let e = gun_elevation(g, muzzle_mps, r, d.y).clamp(-core::f64::consts::FRAC_PI_2, core::f64::consts::FRAC_PI_2);
    let (s, c) = (libm::sin(e), libm::cos(e));
    Some(DVec3::new(d.x / r * c, s, d.z / r * c))
}

/// Mount angles (clamped) for a world fire direction.
pub(crate) fn angles_for(pose: &Pose, m: &MountInfo, dir: DVec3) -> (f64, f64) {
    let (t, e) = gun::world_dir_to_mount(pose, m.zero_rad, dir);
    clamp_to_limits(t, e, &m.limits)
}

/// Current mount angles from part state, or (0, 0).
pub(crate) fn current_angles(part: Option<&PartRuntime>) -> (f64, f64) {
    match part.map(|p| &p.state) { Some(PartState::Mount(ms)) => (ms.train_rad, ms.elevation_rad), _ => (0.0, 0.0) }
}

/// The oldest pending shot for this mount that is still fresh.
pub(crate) fn pending_for<'a>(control: &'a ControlState, m: &MountInfo, tick: u64) -> Option<&'a PendingShot> {
    control.pending.iter().find(|p| role_covers(p.role, m) && tick.saturating_sub(p.issued_tick) <= PENDING_TTL_TICKS)
}

/// Inputs of one manual laying.
pub(crate) struct ManualIn<'a> {
    pub mount: &'a MountInfo,
    pub part: Option<&'a PartRuntime>,
    pub gun: GunFacts<'a>,
    pub pose: Pose,
    pub own_vel: DVec3,
    pub control: &'a ControlState,
    pub picture: &'a SidePicture,
    pub targets: &'a [AimTarget],
    pub tick: u64,
    pub tuning: &'a TuningTable,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct ManualOut {
    pub order: MountOrder,
    /// The shot being fired (pending shot, or trigger fire along the input aim) with its stamps, when the
    /// order fires.
    pub shot: Option<(ShotDesignation, u64, u16)>,
    pub target_speed_mps: f32,
}

/// Lay one hand-held mount. Fire is ordered only for a fresh pending shot or a held trigger, never at a Friend.
pub(crate) fn manual_core(i: &ManualIn<'_>) -> ManualOut {
    let m = i.mount;
    let hold = current_angles(i.part);
    let mut out = ManualOut { order: MountOrder { laid_rot: None, train_rad: hold.0, elevation_rad: hold.1, fire: false, max_rounds: 0, aim_error_rad: [0.0, 0.0] }, shot: None, target_speed_mps: 0.0 };
    if !i.pose.pos.is_finite() { return out; }
    let muzzle = i.pose.to_world(if m.pos_local.is_finite() { m.pos_local } else { DVec3::ZERO });
    let pending = pending_for(i.control, m, i.tick);
    let (shot, render_tick, rtt, single) = match pending {
        Some(p) => (Some(p.shot), p.client_render_tick, p.rtt_ms, true),
        None => {
            let a = i.control.input.aim_local;
            let v = DVec3::new(a[0] as f64, a[1] as f64, a[2] as f64);
            if !v.is_finite() || v.length_squared() < 1e-12 { return out; }
            (Some(ShotDesignation::Direction { dir_local: v.normalize() }), i.control.input.client_render_tick, i.control.last_rtt_ms, false)
        }
    };
    let Some(shot) = shot else { return out };
    let mut friend = false;
    let dir = match shot {
        ShotDesignation::Track { track, offset_m } => {
            let Some(t) = live_track(i.picture, track) else { return out };
            friend = effective_stance(t) == Stance::Friend;
            out.target_speed_mps = if t.vel.is_finite() { t.vel.length() as f32 } else { 0.0 };
            let g = &i.tuning.guidance;
            let (tr, er) = i.gun.def.map(|d| (d.train_rate_dps.to_radians(), d.elevation_rate_dps.to_radians())).unwrap_or((0.0, 0.0));
            let off = if offset_m.is_finite() { offset_m.clamp_length_max(MAX_SHOT_OFFSET_M) } else { DVec3::ZERO };
            let sol = gun::solve(&gun::GunSolveIn {
                muzzle, own_vel: i.own_vel, pose: i.pose, zero_rad: m.zero_rad, limits: m.limits,
                target_pos: t.predict(i.tick) + off, target_vel: t.vel, muzzle_mps: m.muzzle_mps, drag_k: i.gun.drag_k,
                train_rad: hold.0, elevation_rad: hold.1, train_rate: tr, elevation_rate: er,
                iteration_cap: g.gun_iteration_cap.max(1), converge_m: g.gun_converge_m,
            });
            out.order.train_rad = sol.train_rad;
            out.order.elevation_rad = sol.elevation_rad;
            None
        }
        ShotDesignation::Direction { dir_local } => {
            let d = i.pose.rot * dir_local;
            if !d.is_finite() || d.length_squared() < 1e-12 { return out; }
            Some(d.normalize())
        }
    };
    if let Some(ray) = dir {
        let delay = (i.tick.saturating_sub(render_tick) as f64 * sim_core::time::DT).clamp(0.0, MAX_CORRECTION_S);
        let picked = pick_target(muzzle, ray, i.targets, delay);
        let point = match picked {
            Some((tgt, t_ray)) => {
                friend = tgt.friend;
                out.target_speed_mps = if tgt.vel.is_finite() { tgt.vel.length() as f32 } else { 0.0 };
                Some(muzzle + ray * t_ray + tgt.vel * delay)
            }
            None if ray.y < -1e-4 => {
                let t = (muzzle.y / -ray.y).min(MAX_MANUAL_RANGE_M);
                Some(muzzle + ray * t)
            }
            None => None,
        };
        let fire_dir = point.and_then(|p| lay_toward(&i.gun, m.muzzle_mps, muzzle, p)).unwrap_or(ray);
        let (t, e) = angles_for(&i.pose, m, fire_dir);
        out.order.train_rad = t;
        out.order.elevation_rad = e;
    }
    let wants_fire = single || i.control.input.trigger;
    let works = i.part.is_none_or(|p| p.works());
    if wants_fire && works && !friend {
        out.order.fire = true;
        out.order.max_rounds = if single { salvo_rounds(i.gun.def) } else { 0 };
        out.shot = Some((shot, render_tick, rtt));
    }
    out
}

/// The vessel nearest the aim ray inside the gate, judged at where the client saw it (`delay` seconds ago),
/// and the distance along the ray to it.
fn pick_target(origin: DVec3, ray: DVec3, targets: &[AimTarget], delay: f64) -> Option<(AimTarget, f64)> {
    let mut best: Option<(f64, AimTarget, f64)> = None;
    for t in targets {
        if !(t.pos.is_finite() && t.vel.is_finite()) { continue; }
        let seen = t.pos - t.vel * delay;
        let v = seen - origin;
        let along = v.dot(ray);
        if !(along > 1.0 && along <= MAX_MANUAL_RANGE_M) { continue; }
        let perp = (v - ray * along).length();
        let ang = libm::atan2(perp, along);
        let half = if t.half_len_m.is_finite() { t.half_len_m.max(0.0) } else { 0.0 };
        let gate = AIM_GATE_RAD.max(libm::atan2(half + AIM_GATE_PAD_M, along));
        if ang > gate { continue; }
        if best.is_none_or(|b| ang < b.0 || (ang == b.0 && t.entity < b.1.entity)) { best = Some((ang, *t, along)); }
    }
    best.map(|b| (b.1, b.2))
}

/// Mount order for a mount the player holds: slew to the aim direction, fire while the trigger is held or a
/// pending shot exists, with the direction-only correction described above. Returns the order and a shot-log
/// row for each round group fired. Unit: `sys-input`.
///
/// Aimed-at vessels are judged by the server's truth (the client drew them where it saw them, which the
/// correction undoes), and a vessel on the ship's own side or held as a Friend track refuses fire. A row is
/// written only when the mount will fire this tick (loaded, not cooling, on target); its `binding` names the
/// shooter so [`resolve_shots`] can follow the rounds. Mounts not held, not guns or not found give a hold
/// order and no rows.
pub fn manual_mount_order(ship: &Ship, slot: PartSlot, picture: &SidePicture, history: &PoseHistory, world: &World, ctx: &SimCtx<'_>) -> (MountOrder, Vec<ShotLogEntry>) {
    let _ = history; // Conditional-rewind seam (L951); unused until the trigger fires.
    let Some(v) = world.vessel(ship.vessel) else { return (MountOrder::default(), Vec::new()) };
    let part = v.parts.get(slot.index()).filter(|p| p.slot == slot);
    let Some(m) = ship.design.mounts.iter().find(|m| m.slot == slot) else { return (hold_order(part), Vec::new()) };
    if !hand_aimable(m.weapon) || holding_role(&ship.control, m).is_none() { return (hold_order(part), Vec::new()); }
    let targets: Vec<AimTarget> = world.vessels().filter(|(_, o)| o.id != ship.vessel).map(|(_, o)| AimTarget {
        entity: o.id, pos: o.kin.pose.pos, vel: o.kin.lin_vel, half_len_m: 0.5 * o.dims[0],
        friend: o.side == ship.side || picture.track_of(o.id).is_some_and(|t| effective_stance(t) == Stance::Friend),
    }).collect();
    let gun = gun_facts(m, &ship.design.range_tables, ctx.catalogue);
    let out = manual_core(&ManualIn { mount: m, part, gun, pose: v.kin.pose, own_vel: v.kin.lin_vel, control: &ship.control, picture, targets: &targets, tick: ctx.tick.0, tuning: ctx.tuning });
    let mut rows = Vec::new();
    if let (true, Some((shot, render_tick, rtt))) = (out.order.fire, out.shot) {
        if mount_ready(part) {
            rows.push(ShotLogEntry {
                tick: ctx.tick.0, player: ship.player.unwrap_or_default(), mount: slot, shot, rtt_ms: rtt, client_render_tick: render_tick,
                target_speed_mps: out.target_speed_mps, hit: None,
                binding: crate::protocol::event::ShotBinding { shooter: ship.vessel, ..Default::default() },
            });
        }
    }
    (out.order, rows)
}

fn hold_order(part: Option<&PartRuntime>) -> MountOrder { let (t, e) = current_angles(part); MountOrder { laid_rot: None, train_rad: t, elevation_rad: e, ..Default::default() } }

/// The mount fires on the next physics tick if ordered: loaded, cooled and on target.
fn mount_ready(part: Option<&PartRuntime>) -> bool {
    match part { Some(p) => p.works() && matches!(&p.state, PartState::Mount(ms) if ms.cooldown_ticks == 0 && ms.ready_rounds > 0 && ms.on_target), None => false }
}

/// Throttle and rudder while the player holds the Helm. Real code.
pub fn manual_helm(control: &ControlState) -> (f64, f64) { (control.input.throttle.clamp(-1.0, 1.0) as f64, control.input.rudder.clamp(-1.0, 1.0) as f64) }

/// Record this tick's poses, dropping those older than [`POSE_HISTORY_TICKS`]. Unit: `sys-input`.
///
/// Each entity's list stays aligned with the newest end of `ticks`: an entity seen for fewer ticks has a
/// shorter list, and an entity no longer in the world is forgotten. Recording the same tick twice replaces it.
pub fn record_poses(history: &mut PoseHistory, world: &World) {
    let poses: Vec<(EntityId, Pose)> = world.vessels().map(|(_, v)| (v.id, v.kin.pose)).collect();
    record_core(history, world.tick().0, &poses);
}

fn record_core(history: &mut PoseHistory, tick: u64, poses: &[(EntityId, Pose)]) {
    let again = history.ticks.last() == Some(&tick);
    if !again { history.ticks.push(tick); }
    let drop = history.ticks.len().saturating_sub(POSE_HISTORY_TICKS);
    if drop > 0 { history.ticks.drain(..drop); }
    let present: BTreeSet<EntityId> = poses.iter().map(|p| p.0).collect();
    history.poses.retain(|e, _| present.contains(e));
    for (e, pose) in poses {
        let list = history.poses.entry(*e).or_default();
        if again && !list.is_empty() { list.pop(); }
        list.push(*pose);
        let n = list.len().saturating_sub(history.ticks.len());
        if n > 0 { list.drain(..n); }
    }
}

/// Fill in `hit` on shot-log rows whose rounds resolved this tick (impact or splash events).
/// Unit: `sys-input`.
///
/// A row is bound to its rounds by the `ProjectileSpawned` events of its shooter and mount in the two ticks
/// its order stands (ids of one group are contiguous). A row hits when any of its rounds strikes a vessel
/// other than the shooter, and misses once every round has splashed. Rounds that leave no closing event (a
/// point-defence round self-destructing) leave the row unresolved, and a row whose mount did not fire stays
/// unbound and unresolved; unresolved rows do not count toward the rewind trigger.
pub fn resolve_shots(log: &mut Vec<ShotLogEntry>, events: &[sim_core::events::PhysEvent]) {
    use sim_core::events::PhysEvent;
    for ev in events {
        match ev {
            PhysEvent::ProjectileSpawned(sp) => {
                let id = sp.id.0;
                // Extend a group bound this tick, else bind the oldest waiting row.
                if let Some(r) = log.iter_mut().find(|r| r.hit.is_none() && r.binding.shooter == sp.shooter && r.mount == sp.mount && r.binding.projectiles > 0
                    && r.binding.projectiles < u8::MAX && sp.tick >= r.tick && sp.tick <= r.tick + 2 && r.binding.first_projectile.checked_add(r.binding.projectiles as u32) == Some(id)) {
                    r.binding.projectiles += 1;
                    r.binding.open = r.binding.open.saturating_add(1);
                    continue;
                }
                if let Some(r) = log.iter_mut().find(|r| r.hit.is_none() && r.binding.shooter == sp.shooter && r.mount == sp.mount && r.binding.projectiles == 0 && sp.tick >= r.tick && sp.tick <= r.tick + 2) {
                    r.binding.first_projectile = id;
                    r.binding.projectiles = 1;
                    r.binding.open = 1;
                }
            }
            PhysEvent::ShellImpact { projectile, vessel, shooter, .. } => {
                if let Some(r) = owner(log, *shooter, projectile.0) {
                    r.binding.open = r.binding.open.saturating_sub(1);
                    if *vessel != *shooter { r.hit = Some(true); } else if r.binding.open == 0 { r.hit = Some(false); }
                }
            }
            PhysEvent::Splash { projectile, shooter, .. } => {
                if let Some(r) = owner(log, *shooter, projectile.0) {
                    r.binding.open = r.binding.open.saturating_sub(1);
                    if r.binding.open == 0 { r.hit = Some(false); }
                }
            }
            _ => {}
        }
    }
}

fn owner(log: &mut [ShotLogEntry], shooter: EntityId, id: u32) -> Option<&mut ShotLogEntry> {
    log.iter_mut().find(|r| r.hit.is_none() && r.binding.shooter == shooter && r.binding.open > 0 && id >= r.binding.first_projectile && id - r.binding.first_projectile < r.binding.projectiles as u32)
}

/// Shots per RTT bucket before the trigger can fire (L951).
pub const REWIND_MIN_SHOTS: usize = 200;
/// Targets moving this fast or faster count, m/s (20 kn).
pub const REWIND_MIN_TARGET_MPS: f32 = 20.0 * 0.514_444;

/// The rewind trigger test over an accumulated shot log (L951). Unit: `sys-input`.
///
/// Counts resolved direction-only shots at targets moving 20 kn or more in two buckets, RTT under 30 ms and
/// RTT 100 to 200 ms. With at least 200 shots in each, the trigger fires when the high bucket's hit rate is
/// 25% or more below the low bucket's (relative: rate_high <= 0.75 x rate_low).
pub fn rewind_triggered(log: &[ShotLogEntry]) -> bool {
    let (mut lo, mut lo_hit, mut hi, mut hi_hit) = (0usize, 0usize, 0usize, 0usize);
    for r in log {
        let Some(hit) = r.hit else { continue };
        if !matches!(r.shot, ShotDesignation::Direction { .. }) || !(r.target_speed_mps >= REWIND_MIN_TARGET_MPS) { continue; }
        if r.rtt_ms < 30 { lo += 1; lo_hit += hit as usize; } else if (100..=200).contains(&r.rtt_ms) { hi += 1; hi_hit += hit as usize; }
    }
    if lo < REWIND_MIN_SHOTS || hi < REWIND_MIN_SHOTS || lo_hit == 0 { return false; }
    // hi_hit / hi <= 0.75 lo_hit / lo, in integers.
    4 * hi_hit * lo <= 3 * lo_hit * hi
}

#[cfg(test)]
pub(crate) mod tests;
