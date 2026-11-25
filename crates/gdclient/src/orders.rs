//! Unit `gd-orders`. `BpOrders` (autoload `Orders`): every player command of a sortie, and the continuous
//! input state. DESIGN.md L108 (orders are the same in both views), L949-953 (manual shots are designations;
//! taking a role turns its officer advisory), L494-514 (overrides), L396 and L648 (damage-control commands),
//! L786-797 (station actions), L1687 (only the player acknowledges alerts), L1719 (holds can be toggles).
//!
//! Each method builds one `sim_systems::protocol::Command` and sends it through the link; none of them
//! changes local state. The effect comes back in the next snapshot or as an event, and a refusal as
//! `Session.command_rejected`. Every method returns false, and sends nothing, when there is no sortie, the
//! viewer has no hull, or an argument is not a valid id or constant.
//! Commands are stamped with the client render tick by the link (L949).
//!
//! Continuous input (helm axes, trigger, aim) is sampled here and sent as `PlayerInput` at 30 Hz by
//! `BpSession`; it is only read by the server for roles the player holds.
//!
//! Decisions where the contract is silent:
//! - "A sortie" is `Bp.SESSION_IN_SORTIE` with a hull in the newest snapshot (pause included: orders given
//!   while paused apply on resume, L112). Loading, the after-action pause, replays and a lost hull send
//!   nothing.
//! - Only shape, range and constants are checked here. Whether a track is live, Friend or a role is held
//!   is the server's call (the sampled snapshot lags the newest tick, so a local check would refuse the
//!   click right after a takeover), and its refusal arrives as `Session.command_rejected`. The one
//!   exception is `fire_at_track` and `fire_at_reticle`, which need the role held: by the snapshot, or by
//!   this facade's own key state, which is current.
//! - The role-key machine runs on the wall clock (`Time.get_ticks_msec`), not sortie time, so it works while
//!   paused. A press while the role is latched releases it at once; the release of that press does
//!   nothing. A second press without a release (key repeat) does nothing. Switching `input.holds_toggle` on
//!   in the middle of a hold latches the role on release. A press of the key of a role held some other way
//!   (its button, the stick, the HUD's engine telegraph) releases it at once, as the hint beside the key
//!   promises; one press, not two.
//! - Key state is dropped when the role is released by `take_control`, when the server hands the role
//!   back to the officer (`ControlChanged` to the officer for a role not pressed in the last 0.5 s), when a
//!   latched or explicitly taken role has not shown up held in the snapshot 1.5 s after the press (the server
//!   refused it, or handed it back without an event), when the hull is lost and when the sortie ends.
//! - Helm axes take the helm when either leaves a 0.05 dead band. They let go 250 ms after both return
//!   inside it, unless the helm is held by its key or `take_control`; a key release while the rudder is
//!   still deflected leaves the helm with the stick. The throttle does not keep the helm on a key release:
//!   the sortie HUD sends it as an engine telegraph that stays where it was set, not as a sprung axis.
//! - `set_course` needs at least one waypoint and at most 64 (the server's route limit); `set_rules`
//!   refusals are `IssueDict`s with `orders.rules.*` text in `game/i18n/log.csv`.
use crate::convert::{d, get_f64, get_i64, get_text, msg_dict};
use crate::enums::{GdEnum, SessionState};
use crate::seams::ClientMsg;
use crate::services::services;
use content::campaign::StationAction;
use content::enums::{DoctrinePreset, Emcon, IdRung, OfficerRole, Stance};
use content::graph::{PortRef, RepairPriority};
use content::ids::{ClassId, CompartmentId, GraphNodeId, PartSlot};
use glam::{DVec2, DVec3};
use godot::classes::Time;
use godot::prelude::*;
use sim_core::parts::TeamOrder;
use sim_systems::ids::{ActionId, AlertId, TrackId};
use sim_systems::protocol::event::SimEvent;
use sim_systems::protocol::{ActionTarget, Command, ControlHolder, ControlRole, Msg, MsgArg, OffboardOrder, PrizeDecision, Query, ShotDesignation};

/// A press shorter than this toggles a takeover instead of holding it (L108).
pub const TOGGLE_PRESS_MS: u64 = 250;

/// Continuous input sampled from GDScript glue this frame.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct InputState { pub throttle: f32, pub rudder: f32, pub trigger: bool, pub aim_local: [f32; 3] }

/// Text keys this unit emits (rows in `game/i18n/log.csv`).
pub(crate) const TEXT_KEYS: &[&str] = &["orders.rules.no_sortie", "orders.rules.too_many", "orders.rules.invalid"];

/// Helm axes inside this band count as centred.
const AXIS_DEAD_BAND: f32 = 0.05;
/// A role pressed this recently ignores a handback event (it is the echo of an earlier release).
const HANDBACK_GRACE_S: f64 = 0.5;
/// Longest planned route the server accepts (`sim_systems::input::MAX_ROUTE`).
const MAX_ROUTE: usize = sim_systems::input::MAX_ROUTE;
/// A role this facade holds that the snapshot has not shown held for this long was refused or handed back.
const CONFIRM_S: f64 = 1.5;
/// Largest coordinate an order may name, metres (the server refuses beyond).
const MAX_COORD_M: f64 = 1.0e7;

/// What a role-key edge asks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum KeyAction { Take, Release, Nothing }

/// The hold-or-toggle machine over `BpOrders::role_presses`: each entry is (role, press time in seconds,
/// latched). An entry with a press time of minus infinity is a press that released a latched role.
pub(crate) fn role_key_edge(presses: &mut Vec<(ControlRole, f64, bool)>, role: ControlRole, pressed: bool, now_s: f64, toggle_all: bool) -> KeyAction {
    let i = presses.iter().position(|p| p.0 == role);
    let short = TOGGLE_PRESS_MS as f64 / 1_000.0;
    match (pressed, i) {
        (true, None) => { presses.push((role, now_s, toggle_all)); KeyAction::Take }
        (true, Some(i)) => {
            if presses[i].2 { presses[i] = (role, f64::NEG_INFINITY, false); KeyAction::Release } else { KeyAction::Nothing }
        }
        (false, None) => KeyAction::Nothing,
        (false, Some(i)) => {
            let (_, at, latched) = presses[i];
            if latched { return KeyAction::Nothing; }
            if at == f64::NEG_INFINITY { presses.remove(i); return KeyAction::Nothing; }
            let held_s = now_s - at;
            // A clock that went backwards (or NaN) reads as a short press.
            let long = held_s >= short;
            if toggle_all || !long { presses[i].2 = true; return KeyAction::Nothing; }
            presses.remove(i);
            KeyAction::Release
        }
    }
}

/// Whether the key machine holds a role (pressed down or latched on).
fn key_holds(presses: &[(ControlRole, f64, bool)], role: ControlRole) -> bool { presses.iter().any(|p| p.0 == role && (p.2 || p.1 > f64::NEG_INFINITY)) }

fn deflected(v: f32) -> bool { v.abs() > AXIS_DEAD_BAND }
fn axis(v: f64) -> f32 { if v.is_finite() { v.clamp(-1.0, 1.0) as f32 } else { 0.0 } }
fn coord_ok(v: Vector2) -> bool { v.x.is_finite() && v.y.is_finite() && (v.x as f64).abs() <= MAX_COORD_M && (v.y as f64).abs() <= MAX_COORD_M }
fn dvec2(v: Vector2) -> DVec2 { DVec2::new(v.x as f64, v.y as f64) }
/// A track number from GDScript: a positive 32-bit number, else `None`.
fn track_id(v: i64) -> Option<TrackId> { u32::try_from(v).ok().filter(|n| *n > 0).map(TrackId) }
fn slot(v: i64) -> Option<PartSlot> { u16::try_from(v).ok().map(PartSlot) }
fn compartment(v: i64) -> Option<CompartmentId> { u16::try_from(v).ok().map(CompartmentId) }
/// `Bp.NONE` (any negative) means "none": `Some(None)`; a valid value `Some(Some(v))`; garbage `None`.
fn optional<T>(v: i64, parse: impl Fn(i64) -> Option<T>) -> Option<Option<T>> { if v < 0 { Some(None) } else { parse(v).map(Some) } }
fn get_vec2(dict: &VarDictionary, key: &str) -> Option<Vector2> { dict.get(key).and_then(|v| v.try_to::<Vector2>().ok()).filter(|v| coord_ok(*v)) }

/// Team order from `Bp.TEAM_*` and its target.
pub(crate) fn team_order(kind: i64, target: i64) -> Option<Option<TeamOrder>> {
    Some(Some(match kind {
        k if k < 0 => return Some(None),
        0 => TeamOrder::Idle,
        1 => TeamOrder::FightFire(compartment(target)?),
        2 => TeamOrder::Patch { compartment: compartment(target)? },
        3 => TeamOrder::JuryRig { conduit: u16::try_from(target).ok()? },
        4 => TeamOrder::Repair(slot(target)?),
        _ => return None,
    }))
}

/// A full repair order: all six values, each once.
pub(crate) fn repair_order(values: &[i32]) -> Option<Vec<RepairPriority>> {
    let order: Vec<RepairPriority> = values.iter().map(|v| RepairPriority::from_gd(*v as i64)).collect::<Option<_>>()?;
    let mut seen = order.clone();
    seen.sort_unstable();
    seen.dedup();
    (order.len() == RepairPriority::VARIANTS.len() && seen.len() == order.len()).then_some(order)
}

/// Offboard order from its dictionary.
fn offboard_order(order: &VarDictionary) -> Option<OffboardOrder> {
    Some(match get_i64(order, "kind")? {
        0 => OffboardOrder::Launch,
        1 => OffboardOrder::Recall,
        2 => {
            let r = get_f64(order, "radius_m").filter(|r| *r > 0.0 && *r <= MAX_COORD_M)?;
            OffboardOrder::Orbit { centre: dvec2(get_vec2(order, "centre")?), radius_m: r }
        }
        3 => OffboardOrder::Shadow(track_id(get_i64(order, "track")?)?),
        4 => OffboardOrder::Waypoint(dvec2(get_vec2(order, "point")?)),
        _ => return None,
    })
}

/// Station action target from its dictionary.
fn action_target(target: &VarDictionary) -> Option<ActionTarget> {
    Some(match get_text(target, "kind").as_deref().unwrap_or("none") {
        "none" => ActionTarget::None,
        "track" => ActionTarget::Track(track_id(get_i64(target, "track")?)?),
        "marker" => ActionTarget::Marker(get_text(target, "marker").filter(|m| !m.is_empty() && m.len() <= 128)?),
        "line" => {
            let (a, b) = (get_vec2(target, "a")?, get_vec2(target, "b")?);
            if a == b { return None; }
            ActionTarget::Line { a: dvec2(a), b: dvec2(b) }
        }
        _ => return None,
    })
}

/// Extra per-facade state besides the contract fields of [`BpOrders`]. Plain data.
#[derive(Default)]
pub(crate) struct OrdersState {
    /// Roles taken with `take_control(.., true)`, with when.
    explicit: Vec<(ControlRole, f64)>,
    /// The helm is held because an axis is deflected.
    axis_helm: bool,
    /// When both axes came back inside the dead band.
    axis_zero_since: Option<f64>,
    /// Session state and hull presence last frame, to drop key state when either ends.
    live: bool,
    /// When the trigger last went down, and a release waiting for the shortest pull to pass.
    trigger_down_s: f64,
    trigger_release_pending: bool,
}

/// The trigger stays pulled at least this long, a little over one 30 Hz input period, so a quick click
/// or a trackpad tap still reaches the server as a shot.
const TRIGGER_MIN_S: f64 = 0.045;

/// Player commands. Autoload `Orders`.
#[derive(GodotClass)]
#[class(base = Node)]
pub struct BpOrders {
    pub(crate) input: InputState,
    /// Press time per role key, for the hold-or-toggle rule.
    pub(crate) role_presses: Vec<(sim_systems::protocol::ControlRole, f64, bool)>,
    /// Everything else (see [`OrdersState`]).
    pub(crate) state: OrdersState,
    base: Base<Node>,
}

/// Whether commands may go out now: in a sortie with a hull.
fn live() -> bool {
    let services = services();
    let feed = services.feed.borrow();
    feed.state == SessionState::InSortie && feed.snapshot.as_ref().is_some_and(|s| s.ship.is_some())
}

/// Send one command through the live link.
fn send(command: Command) -> bool {
    if !live() { return false; }
    let services = services();
    let mut c = services.connection.borrow_mut();
    match c.as_mut() { Some(conn) => { conn.link.send(ClientMsg::Command(command)); true } None => false }
}

fn now_s() -> f64 { Time::singleton().get_ticks_msec() as f64 / 1_000.0 }

/// Roles the newest snapshot says the player holds.
fn held_roles() -> Vec<ControlRole> {
    let services = services();
    let feed = services.feed.borrow();
    feed.snapshot.as_ref().and_then(|s| s.ship.as_ref()).map(|s| s.held.clone()).unwrap_or_default()
}

fn issue(text: Msg) -> VarDictionary { d! { "text" => msg_dict(&text), "fatal" => true, "part" => -1i64, "cell" => Variant::nil() } }

#[godot_api]
impl INode for BpOrders {
    fn init(base: Base<Node>) -> Self { Self { input: InputState::default(), role_presses: Vec::new(), state: OrdersState::default(), base } }
    fn ready(&mut self) {
        // After Session (-100) fills the feed, before the HUD (-90) and the 30 Hz input of the next frame.
        self.base_mut().set_process_priority(-95);
    }
    /// The helm axes' release timer, the aim mirror, and key state the server or the sortie ended.
    fn process(&mut self, _delta: f64) {
        let services = services();
        let now = now_s();
        self.input.aim_local = services.aim_local.get();
        if self.state.trigger_release_pending && now - self.state.trigger_down_s >= TRIGGER_MIN_S {
            self.input.trigger = false;
            self.state.trigger_release_pending = false;
        }
        let (live, handbacks, held) = {
            let feed = services.feed.borrow();
            let ship = feed.snapshot.as_ref().and_then(|s| s.ship.as_ref());
            let own = ship.map(|s| s.vessel);
            let handbacks: Vec<ControlRole> = feed.events.iter().filter_map(|e| match &e.event {
                SimEvent::ControlChanged { vessel, role, holder: ControlHolder::Officer } if Some(*vessel) == own => Some(*role),
                _ => None,
            }).collect();
            (feed.state == SessionState::InSortie && ship.is_some(), handbacks, ship.map(|s| s.held.clone()).unwrap_or_default())
        };
        if !live {
            if self.state.live || !self.role_presses.is_empty() || self.state.axis_helm {
                self.role_presses.clear();
                self.state = OrdersState::default();
                self.input = InputState { aim_local: self.input.aim_local, ..InputState::default() };
            }
            return;
        }
        self.state.live = true;
        for role in handbacks {
            self.role_presses.retain(|p| !(p.0 == role && (p.1 == f64::NEG_INFINITY || now - p.1 > HANDBACK_GRACE_S)));
            if role == ControlRole::Helm { self.state.axis_helm = self.state.axis_helm && (deflected(self.input.throttle) || deflected(self.input.rudder)); }
        }
        // Latches and explicit takes the server never confirmed (refused, or handed back since) are dropped.
        self.role_presses.retain(|p| !(p.2 && now - p.1 > CONFIRM_S && !held.contains(&p.0)));
        self.state.explicit.retain(|(r, t)| now - t <= CONFIRM_S || held.contains(r));
        if self.state.axis_helm {
            if let Some(t) = self.state.axis_zero_since {
                if now - t >= TOGGLE_PRESS_MS as f64 / 1_000.0 {
                    self.state.axis_helm = false;
                    self.state.axis_zero_since = None;
                    if !self.helm_held_explicitly() { send(Command::TakeControl { role: ControlRole::Helm, on: false }); }
                }
            }
        }
    }
}

impl BpOrders {
    fn explicit(&self, role: ControlRole) -> bool { self.state.explicit.iter().any(|(r, _)| *r == role) }
    fn helm_held_explicitly(&self) -> bool { self.explicit(ControlRole::Helm) || key_holds(&self.role_presses, ControlRole::Helm) }
    fn axes_deflected(&self) -> bool { deflected(self.input.throttle) || deflected(self.input.rudder) }
    /// The role is held: by the snapshot, or by this facade's own key or explicit state.
    fn holds(&self, role: ControlRole) -> bool {
        self.explicit(role) || key_holds(&self.role_presses, role) || held_roles().contains(&role)
            || (role == ControlRole::Helm && self.state.axis_helm)
    }
    fn weapon_role(kind: i64, index: i64) -> Option<ControlRole> {
        crate::convert::role_from_gd(kind, index).filter(|r| matches!(r, ControlRole::Mount(_) | ControlRole::WeaponGroup(_)))
    }
}

#[godot_api]
impl BpOrders {
    // ---- Helm and designation (L108) ----

    /// Set course through `waypoints` (map positions). `speed_kn < 0` keeps the ordered speed; `append` adds
    /// to the planned route instead of replacing it.
    #[func]
    fn set_course(&mut self, waypoints: PackedVector2Array, speed_kn: f64, append: bool) -> bool {
        let pts = waypoints.as_slice();
        if pts.is_empty() || pts.len() > MAX_ROUTE || !pts.iter().all(|p| coord_ok(*p)) || !speed_kn.is_finite() { return false; }
        send(Command::SetCourse { waypoints: pts.iter().map(|p| dvec2(*p)).collect(), speed_kn: (speed_kn >= 0.0).then_some(speed_kn), append })
    }
    #[func]
    fn set_speed(&mut self, speed_kn: f64) -> bool {
        if !(speed_kn.is_finite() && speed_kn >= 0.0) { return false; }
        send(Command::SetSpeed { speed_kn })
    }
    /// Hold station at a map position; `heading_deg < 0` leaves the heading to the Helm officer.
    #[func]
    fn hold_station(&mut self, at: Vector2, heading_deg: f64) -> bool {
        if !coord_ok(at) || !heading_deg.is_finite() { return false; }
        send(Command::HoldStation { at: dvec2(at), heading_rad: crate::convert::heading_rad(heading_deg.rem_euclid(360.0)).filter(|_| heading_deg >= 0.0) })
    }
    /// Designate a track for the officers; `Bp.NONE` clears.
    #[func]
    fn designate(&mut self, track: i64) -> bool {
        let Some(track) = optional(track, track_id) else { return false };
        send(Command::Designate { track })
    }
    /// Leave the sortie area; the sortie ends as withdrawn once clear of contact.
    #[func]
    fn withdraw(&mut self) -> bool { send(Command::Withdraw) }

    // ---- Roles and manual fire (L953, L1665) ----

    /// Take (`on` true) or release a role outright. `kind` is `Bp.ROLE_*`; `index` is the part slot for
    /// `Bp.ROLE_MOUNT`, the group number for `Bp.ROLE_WEAPON_GROUP`, else 0.
    #[func]
    fn take_control(&mut self, kind: i64, index: i64, on: bool) -> bool {
        let Some(role) = crate::convert::role_from_gd(kind, index) else { return false };
        if !send(Command::TakeControl { role, on }) { return false; }
        if on {
            let now = now_s();
            self.state.explicit.retain(|(r, _)| *r != role);
            self.state.explicit.push((role, now));
        } else {
            self.state.explicit.retain(|(r, _)| *r != role);
            self.role_presses.retain(|p| p.0 != role);
            if role == ControlRole::Helm { self.state.axis_helm = false; self.state.axis_zero_since = None; }
        }
        true
    }
    /// Feed a role key or button edge and let the rule decide: hold takes the role until release; a press
    /// shorter than 250 ms toggles it until pressed again; with the setting `input.holds_toggle` every press
    /// toggles (L108, L1719). Call on both press and release.
    #[func]
    fn role_key(&mut self, kind: i64, index: i64, pressed: bool) -> bool {
        let Some(role) = crate::convert::role_from_gd(kind, index) else { return false };
        if !live() { return false; }
        let toggle_all = services().settings.borrow().bool("input.holds_toggle");
        // A role the player holds some other way (its button, the stick, the engine telegraph) is let go
        // by its key: one press hands it back, as the hint beside the key says. The marker swallows the
        // matching release edge.
        if pressed && !self.role_presses.iter().any(|p| p.0 == role) && self.holds(role) {
            self.role_presses.push((role, f64::NEG_INFINITY, false));
            self.state.explicit.retain(|(r, _)| *r != role);
            if role == ControlRole::Helm { self.state.axis_helm = false; self.state.axis_zero_since = None; }
            return send(Command::TakeControl { role, on: false });
        }
        match role_key_edge(&mut self.role_presses, role, pressed, now_s(), toggle_all) {
            KeyAction::Take => send(Command::TakeControl { role, on: true }),
            KeyAction::Release => {
                self.state.explicit.retain(|(r, _)| *r != role);
                // The rudder keeps the helm while it is deflected. The throttle does not: under manual
                // helm it is a telegraph that stays where it was set, not a sprung axis.
                if role == ControlRole::Helm && self.state.axis_helm && deflected(self.input.rudder) { return true; }
                if role == ControlRole::Helm { self.state.axis_helm = false; }
                send(Command::TakeControl { role, on: false })
            }
            KeyAction::Nothing => true,
        }
    }
    /// Manual shot at a track with an aim offset in metres (track frame). The role must be held.
    #[func]
    fn fire_at_track(&mut self, kind: i64, index: i64, track: i64, offset: Vector3) -> bool {
        let (Some(role), Some(track)) = (Self::weapon_role(kind, index), track_id(track)) else { return false };
        if !(offset.x.is_finite() && offset.y.is_finite() && offset.z.is_finite()) || !self.holds(role) { return false; }
        send(Command::Fire { role, shot: ShotDesignation::Track { track, offset_m: DVec3::new(offset.x as f64, offset.y as f64, offset.z as f64) } })
    }
    /// Manual shot along the current reticle direction (`BpWorldView.set_reticle`). The role must be held.
    #[func]
    fn fire_at_reticle(&mut self, kind: i64, index: i64) -> bool {
        let Some(role) = Self::weapon_role(kind, index) else { return false };
        let a = services().aim_local.get();
        let dir = DVec3::new(a[0] as f64, a[1] as f64, a[2] as f64);
        if !dir.is_finite() || dir.length_squared() < 1e-6 || !self.holds(role) { return false; }
        send(Command::Fire { role, shot: ShotDesignation::Direction { dir_local: dir.normalize() } })
    }
    /// Launch a guided weapon by hand from a launcher slot. `track` may be `Bp.NONE` with a `bearing_deg`
    /// (bearing-only launch); `depth_m < 0` = default depth (torpedoes, depth charges).
    #[func]
    fn launch(&mut self, slot: i64, track: i64, bearing_deg: f64, depth_m: f64) -> bool {
        let (Some(slot), Some(track)) = (self::slot(slot), optional(track, track_id)) else { return false };
        if !bearing_deg.is_finite() || !depth_m.is_finite() { return false; }
        let bearing_rad = (bearing_deg >= 0.0).then(|| bearing_deg.rem_euclid(360.0).to_radians());
        let depth_m = (depth_m >= 0.0).then_some(depth_m);
        if depth_m.is_some_and(|d| d > 1_000.0) || (track.is_none() && bearing_rad.is_none() && depth_m.is_none()) { return false; }
        send(Command::Launch { slot, track, bearing_rad, depth_m })
    }
    #[func]
    fn fire_decoy(&mut self, slot: i64) -> bool {
        let Some(slot) = self::slot(slot) else { return false };
        send(Command::FireDecoy { slot })
    }

    // ---- Continuous input (sent at 30 Hz while a role is held) ----

    /// Helm axes: `throttle` -1.0 (full astern) to 1.0, `rudder` -1.0 (port) to 1.0. A non-zero axis takes the
    /// helm while it is deflected and releases it 250 ms after both return to zero, unless the helm role is
    /// held or toggled on explicitly. The sortie HUD sends its engine telegraph as `throttle` (it takes the
    /// helm explicitly first) and the sprung rudder keys or stick as `rudder`.
    #[func]
    fn set_helm_input(&mut self, throttle: f64, rudder: f64) {
        self.input.throttle = axis(throttle);
        self.input.rudder = axis(rudder);
        if self.axes_deflected() {
            self.state.axis_zero_since = None;
            if !self.state.axis_helm && live() {
                self.state.axis_helm = true;
                if !self.helm_held_explicitly() { send(Command::TakeControl { role: ControlRole::Helm, on: true }); }
            }
        } else if self.state.axis_helm && self.state.axis_zero_since.is_none() {
            self.state.axis_zero_since = Some(now_s());
        }
    }
    /// Trigger held: fire continuously along the reticle with the held mount or group.
    #[func]
    fn set_trigger(&mut self, held: bool) {
        let now = now_s();
        if held {
            if !self.input.trigger { self.state.trigger_down_s = now; }
            self.input.trigger = true;
            self.state.trigger_release_pending = false;
        } else if self.input.trigger && now - self.state.trigger_down_s < TRIGGER_MIN_S {
            self.state.trigger_release_pending = true;
        } else {
            self.input.trigger = false;
            self.state.trigger_release_pending = false;
        }
    }

    // ---- Sensors and tracks (L503, L507) ----

    /// `emcon` is `Bp.EMCON_*`.
    #[func]
    fn set_emcon(&mut self, emcon: i64) -> bool {
        let Some(e) = Emcon::from_gd(emcon) else { return false };
        send(Command::SetEmcon(e))
    }
    /// Manual stance per track; `Bp.NONE` reverts to the rules. Friend can never be engaged (L500).
    #[func]
    fn set_stance(&mut self, track: i64, stance: i64) -> bool {
        let (Some(track), Some(stance)) = (track_id(track), optional(stance, Stance::from_gd)) else { return false };
        send(Command::SetStance { track, stance })
    }
    /// Manual identification; `rung == Bp.NONE` reverts. `class_id` may be `&""`.
    #[func]
    fn set_identity(&mut self, track: i64, rung: i64, class_id: StringName) -> bool {
        let (Some(track), Some(rung)) = (track_id(track), optional(rung, IdRung::from_gd)) else { return false };
        let class = class_id.to_string();
        let class = if class.is_empty() || rung.is_none() { None } else {
            let id = ClassId::new(class);
            if !services().campaign().classes.contains_key(&id) { return false; }
            Some(id)
        };
        send(Command::SetIdentity { track, rung, class })
    }

    // ---- Control ladder L0 to L2 during a sortie (L322-336) ----

    /// Doctrine preset for the ship (`mount == Bp.NONE`) or one mount. `preset` is `Bp.PRESET_*`.
    #[func]
    fn set_preset(&mut self, mount: i64, preset: i64) -> bool {
        let (Some(mount), Some(preset)) = (optional(mount, slot), DoctrinePreset::from_gd(preset)) else { return false };
        send(Command::SetPreset { mount, preset })
    }
    /// An L1 slider by its `SliderDict.key`.
    #[func]
    fn set_slider(&mut self, key: GString, value: f64) -> bool {
        let key = key.to_string();
        if key.is_empty() || key.len() > 128 || !value.is_finite() { return false; }
        let sent = send(Command::SetSlider { key: key.clone(), value });
        // The tutorial's last lesson asks for the Aggression slider (`tutorial::Trigger::SliderMoved`).
        if sent { crate::services::services().tutorial_signal(sim_systems::tutorial::Signal::SliderMoved { key }); }
        sent
    }
    /// One doctrine row (`DoctrineEntryDict`) for the ship (`mount == Bp.NONE`) or one mount.
    #[func]
    fn set_doctrine(&mut self, mount: i64, entry: VarDictionary) -> bool {
        let (Some(mount), Some(entry)) = (optional(mount, slot), crate::convert::doctrine_entry_from_dict(&entry)) else { return false };
        send(Command::SetDoctrine { mount, entry })
    }
    /// Replace the rules table (at most 32 `RuleDict`s). Returns `Array[IssueDict]`; empty = sent.
    #[func]
    fn set_rules(&mut self, rules: Array<VarDictionary>) -> Array<VarDictionary> {
        let mut issues: Array<VarDictionary> = Array::new();
        if !live() {
            issues.push(&issue(Msg::new("orders.rules.no_sortie")));
            return issues;
        }
        let max = content::graph::MAX_RULES;
        if rules.len() > max {
            issues.push(&issue(Msg::new("orders.rules.too_many").arg(MsgArg::Int(rules.len() as i64)).arg(MsgArg::Int(max as i64))));
            return issues;
        }
        let mut parsed = Vec::with_capacity(rules.len());
        for (i, r) in rules.iter_shared().enumerate() {
            match crate::convert::rule_from_dict(&r) {
                Some(rule) => parsed.push(rule),
                None => issues.push(&issue(Msg::new("orders.rules.invalid").arg(MsgArg::Int(i as i64 + 1)))),
            }
        }
        if issues.is_empty() && !send(Command::SetRules(parsed)) { issues.push(&issue(Msg::new("orders.rules.no_sortie"))); }
        issues
    }
    /// Power priority 1 (shed last) to 5 for one consumer (L287-295).
    #[func]
    fn set_power_priority(&mut self, slot: i64, priority: i64) -> bool {
        let Some(slot) = self::slot(slot) else { return false };
        if !(1..=5).contains(&priority) { return false; }
        send(Command::SetPowerPriority { slot, priority: priority as u8 })
    }
    /// Revert one overridden value by its `OverrideDict.id` (one click, L351).
    #[func]
    fn revert(&mut self, id: GString) -> bool {
        let Some(key) = crate::hud::override_id::decode(&id.to_string()) else { return false };
        send(Command::Revert(key))
    }
    /// Revert every override of one officer (`Bp.OFFICER_*`).
    #[func]
    fn revert_all(&mut self, officer: i64) -> bool {
        let Some(role) = OfficerRole::from_gd(officer) else { return false };
        send(Command::RevertAll(role))
    }

    // ---- Damage control (L396, L639-648, L667, L716) ----

    /// New repair order: all six `Bp.REPAIR_*` values, most urgent first.
    #[func]
    fn reorder_repairs(&mut self, order: PackedInt32Array) -> bool {
        let Some(order) = repair_order(order.as_slice()) else { return false };
        send(Command::ReorderRepairs(order))
    }
    /// Pin team `team` to a job: `kind` is `Bp.TEAM_*` and `target` the compartment, conduit or part slot it
    /// needs. `kind == Bp.NONE` hands the team back to the officer.
    #[func]
    fn assign_team(&mut self, team: i64, kind: i64, target: i64) -> bool {
        let (Ok(team), Some(order)) = (u8::try_from(team), team_order(kind, target)) else { return false };
        if team >= 32 { return false; }
        send(Command::AssignTeam { team, order })
    }
    /// Run or stop a pump; `compartment == Bp.NONE` leaves the choice to the officer.
    #[func]
    fn set_pump(&mut self, slot: i64, running: bool, compartment: i64) -> bool {
        let (Some(slot), Some(target)) = (self::slot(slot), optional(compartment, self::compartment)) else { return false };
        send(Command::Pump { slot, running, target })
    }
    #[func]
    fn set_door(&mut self, slot: i64, closed: bool) -> bool {
        let Some(slot) = self::slot(slot) else { return false };
        send(Command::Door { slot, closed })
    }
    /// Counter-flood: open or close a compartment's flood valve.
    #[func]
    fn set_flood_valve(&mut self, compartment: i64, open: bool) -> bool {
        let Some(compartment) = self::compartment(compartment) else { return false };
        send(Command::FloodValve { compartment, open })
    }
    /// Flood a magazine to stop a cook-off. Cannot be undone during the sortie; the UI confirms first.
    #[func]
    fn flood_magazine(&mut self, slot: i64) -> bool {
        let Some(slot) = self::slot(slot) else { return false };
        send(Command::FloodMagazine { slot })
    }

    // ---- Offboard units, special operations (L786-797) ----

    /// Order a drone or helicopter. `order` keys: `kind: int` (`Bp.OFFBOARD_*`), and by kind `centre: Vector2`
    /// and `radius_m: float` (orbit), `track: int` (shadow), `point: Vector2` (waypoint).
    #[func]
    fn offboard(&mut self, slot: i64, order: VarDictionary) -> bool {
        let (Some(slot), Some(order)) = (self::slot(slot), offboard_order(&order)) else { return false };
        send(Command::Offboard { slot, order })
    }
    /// Start a station action (`Bp.STATION_*`). `target` keys: `kind: StringName` (`&"none"`, `&"track"`,
    /// `&"marker"`, `&"line"`), then `track: int`, `marker: String`, or `a: Vector2` and `b: Vector2`
    /// (mine line drawn on the plot).
    #[func]
    fn start_action(&mut self, action: i64, target: VarDictionary) -> bool {
        let (Some(action), Some(target)) = (StationAction::from_gd(action), action_target(&target)) else { return false };
        if action == StationAction::CarryPassengers { return false; }
        send(Command::StartAction { action, target })
    }
    #[func]
    fn cancel_action(&mut self, action_id: i64) -> bool {
        let Some(id) = u32::try_from(action_id).ok().filter(|i| *i > 0) else { return false };
        send(Command::CancelAction(ActionId(id)))
    }
    /// Decide a prize: `decision` is `Bp.PRIZE_*`; `prize` is the entity id from `ActionDict.actor`'s target.
    #[func]
    fn prize(&mut self, prize: i64, decision: i64) -> bool {
        let (Some(prize), Some(decision)) = (crate::convert::entity_from_gd(prize), PrizeDecision::from_gd(decision)) else { return false };
        send(Command::Prize { prize, decision })
    }

    // ---- Alerts (L1687) ----

    /// Acknowledge one alert, or with `Bp.NONE` the highest-priority, oldest one. There is no acknowledge-all.
    #[func]
    fn acknowledge(&mut self, alert_id: i64) -> bool {
        let Some(id) = optional(alert_id, |v| u32::try_from(v).ok().map(AlertId)) else { return false };
        send(Command::Acknowledge(id))
    }
    /// Silence all alert sounds for 30 s.
    #[func]
    fn silence_alerts(&mut self) -> bool { send(Command::SilenceAlerts) }

    // ---- Queries: replies arrive as `Hud` signals ----

    /// "Why?" on a part; the reply is `Hud.why_reply(WhyDict)`.
    #[func]
    fn query_why(&mut self, slot: i64) -> bool {
        let Some(slot) = self::slot(slot) else { return false };
        send(Command::Query(Query::Why(slot)))
    }
    /// Full candidate list and evidence of a track; the reply is `Hud.track_card_reply(TrackCardDict)`.
    #[func]
    fn query_track_card(&mut self, track: i64) -> bool {
        let Some(track) = track_id(track) else { return false };
        send(Command::Query(Query::TrackCard(track)))
    }
    /// Overridden values of one officer against defaults; the reply is `Hud.overrides_reply(officer, Array[OverrideDict])`.
    #[func]
    fn query_overrides(&mut self, officer: i64) -> bool {
        let Some(role) = OfficerRole::from_gd(officer) else { return false };
        send(Command::Query(Query::Overrides(role)))
    }
    /// Scope a graph port during a splash (L216): values stream as `Hud.scope_value(node, port, ValueDict)`
    /// until switched off.
    #[func]
    fn query_scope(&mut self, node: i64, port: i64, on: bool) -> bool {
        let (Ok(node), Ok(port)) = (u32::try_from(node), u8::try_from(port)) else { return false };
        send(Command::Query(Query::Scope { port: PortRef { node: GraphNodeId(node), port }, on }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const H: ControlRole = ControlRole::Helm;

    #[test]
    fn hold_then_release_gives_back() {
        let mut p = Vec::new();
        assert_eq!(role_key_edge(&mut p, H, true, 10.0, false), KeyAction::Take);
        assert_eq!(role_key_edge(&mut p, H, false, 10.6, false), KeyAction::Release);
        assert!(p.is_empty());
    }

    #[test]
    fn short_press_toggles_until_pressed_again() {
        let mut p = Vec::new();
        assert_eq!(role_key_edge(&mut p, H, true, 1.0, false), KeyAction::Take);
        assert_eq!(role_key_edge(&mut p, H, false, 1.1, false), KeyAction::Nothing);
        assert!(key_holds(&p, H));
        // Key repeat of the release, and a stray release, change nothing.
        assert_eq!(role_key_edge(&mut p, H, false, 1.2, false), KeyAction::Nothing);
        assert_eq!(role_key_edge(&mut p, H, true, 5.0, false), KeyAction::Release);
        assert!(!key_holds(&p, H));
        // The press that released waits for its release, which does nothing, however long it was.
        assert_eq!(role_key_edge(&mut p, H, true, 5.1, false), KeyAction::Nothing);
        assert_eq!(role_key_edge(&mut p, H, false, 9.0, false), KeyAction::Nothing);
        assert!(p.is_empty());
        // Exactly 250 ms counts as a hold.
        assert_eq!(role_key_edge(&mut p, H, true, 20.0, false), KeyAction::Take);
        assert_eq!(role_key_edge(&mut p, H, false, 20.25, false), KeyAction::Release);
    }

    #[test]
    fn echo_presses_and_orphan_releases() {
        let mut p = Vec::new();
        assert_eq!(role_key_edge(&mut p, H, false, 0.0, false), KeyAction::Nothing);
        assert_eq!(role_key_edge(&mut p, H, true, 0.0, false), KeyAction::Take);
        assert_eq!(role_key_edge(&mut p, H, true, 0.05, false), KeyAction::Nothing);
        assert_eq!(role_key_edge(&mut p, H, true, 0.4, false), KeyAction::Nothing);
        assert_eq!(role_key_edge(&mut p, H, false, 1.0, false), KeyAction::Release);
        // Roles are independent.
        let g = ControlRole::WeaponGroup(1);
        assert_eq!(role_key_edge(&mut p, g, true, 2.0, false), KeyAction::Take);
        assert_eq!(role_key_edge(&mut p, H, true, 2.0, false), KeyAction::Take);
        assert_eq!(p.len(), 2);
        // A clock that went backwards reads as a short press.
        assert_eq!(role_key_edge(&mut p, g, false, 1.0, false), KeyAction::Nothing);
        assert!(key_holds(&p, g));
    }

    #[test]
    fn holds_toggle_setting() {
        let mut p = Vec::new();
        assert_eq!(role_key_edge(&mut p, H, true, 0.0, true), KeyAction::Take);
        assert_eq!(role_key_edge(&mut p, H, false, 3.0, true), KeyAction::Nothing);
        assert!(key_holds(&p, H));
        assert_eq!(role_key_edge(&mut p, H, true, 4.0, true), KeyAction::Release);
        assert_eq!(role_key_edge(&mut p, H, false, 4.1, true), KeyAction::Nothing);
        assert!(p.is_empty());
        // Switched on in the middle of a long hold: the release latches.
        assert_eq!(role_key_edge(&mut p, H, true, 10.0, false), KeyAction::Take);
        assert_eq!(role_key_edge(&mut p, H, false, 12.0, true), KeyAction::Nothing);
        assert!(key_holds(&p, H));
        // Switched off while latched: the next press releases as usual.
        assert_eq!(role_key_edge(&mut p, H, true, 13.0, false), KeyAction::Release);
    }

    #[test]
    fn team_orders_and_repairs_validate() {
        assert_eq!(team_order(-1, 99), Some(None));
        assert_eq!(team_order(0, -5), Some(Some(TeamOrder::Idle)));
        assert_eq!(team_order(1, 3), Some(Some(TeamOrder::FightFire(CompartmentId(3)))));
        assert_eq!(team_order(3, 70_000), None);
        assert_eq!(team_order(4, 7), Some(Some(TeamOrder::Repair(PartSlot(7)))));
        assert_eq!(team_order(9, 0), None);
        assert_eq!(team_order(2, -1), None);
        assert_eq!(repair_order(&[5, 4, 3, 2, 1, 0]).map(|o| o.len()), Some(6));
        assert!(repair_order(&[0, 1, 2, 3, 4]).is_none());
        assert!(repair_order(&[0, 0, 2, 3, 4, 5]).is_none());
        assert!(repair_order(&[0, 1, 2, 3, 4, 6]).is_none());
        assert!(repair_order(&[0, 1, 2, 3, 4, -1]).is_none());
    }

    #[test]
    fn ids_narrow_without_wrapping() {
        assert_eq!(track_id(0), None);
        assert_eq!(track_id(-1), None);
        assert_eq!(track_id(1 << 33), None);
        assert_eq!(track_id(412), Some(TrackId(412)));
        assert_eq!(slot(65_536), None);
        assert_eq!(optional(-1, track_id), Some(None));
        assert_eq!(optional(1 << 40, track_id), None);
        assert!(!coord_ok(Vector2::new(f32::NAN, 0.0)));
        assert!(!coord_ok(Vector2::new(2.0e7, 0.0)));
    }
}
