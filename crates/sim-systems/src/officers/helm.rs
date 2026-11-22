//! Unit `sys-officer-helm`. Helm officer: heading and speed by PID, station keeping, evasion patterns.
//! Deliberate limit: no terrain-aware pathing beyond the route it is given (L307). DESIGN.md L307, L108
//! (right-click sets course), L317 (stock Helm follows the briefing route in the winnability gate).
//!
//! Implementation notes:
//! - The three stages choose a heading and a speed. Each also reports the rudder and throttle the PID would
//!   give for them, but without advancing the PID integrators: [`hold`] is the only place they advance. Call
//!   `hold` once per graph tick on the final heading and speed (the helm sinks, or the player's ordered
//!   heading). Run each stage once per graph tick, autopilot then station keeping then evasion: the zigzag
//!   leg timer advances per `evasion` call, and `HelmState::mode` and the Bridge Board fields keep the last
//!   stage's values.
//! - A route is followed by turning-circle-aware pure pursuit (`ai::path`); when the ship passes the last
//!   waypoint it holds the last leg's course at the ordered speed (a course, not a stop). With no route the
//!   officer holds the heading it had when the route, station or formation order ended.
//! - Speed: throttle is a power fraction, so the feed-forward is (speed / top)^3 with PI on the error. The
//!   officer backs the engines only to stop or to shed more than a tenth of top speed.
//! - Without an ordered speed the officer cruises at `helm.cruise` (fraction of top speed, default 0.7).
//! - Grounding guard: with less than draft + 2 m of water one stop distance ahead the officer stops if it
//!   is still heading that way, or creeps at 20% of top speed while it turns away; under draft + 5 m it
//!   slows to 30%.
//! - Evasion (`helm.evasion`, 0..=1, default 0.5): 0 never evades on its own; above 0 it evades inbound
//!   weapons whose time to go is under 20 s + 100 s x slider. An ordered evasion evades every inbound weapon
//!   and zigzags when none is inbound. Torpedoes are combed (turn parallel to the torpedo track, the smaller
//!   turn, at full speed); missiles are beamed (bearing 90 degrees off the bow, the nearer side, at full
//!   speed). The zigzag swings 30 degrees either side of the base course on legs of 40 s scaled by a fixed
//!   irregular cycle, the first leg half length so the mean track stays on the base course.
//! - Formation offsets are in the leader's frame, x ahead and y starboard (`ai::group::Directive::slot`).
use crate::ai::path::{bearing, dubins_points, heading_dir, posrot_heading, proj_t, pursue, wrap_2pi, wrap_pi, ARC_STEP_RAD};
use dubins_paths::f64::DubinsPath;
use crate::prelude::*;
use crate::protocol::snapshot::SourceMask;
use crate::ship::{FormationSlot, ShipOrders, TurnModel};
use crate::tracks::Track;
use core::f64::consts::{FRAC_PI_2, PI};
use sim_core::time::KN;

/// L1 slider key: evasion readiness, 0..=1.
pub const SLIDER_EVASION: &str = "helm.evasion";
/// L1 slider key: cruise speed as a fraction of top speed, used when no speed is ordered.
pub const SLIDER_CRUISE: &str = "helm.cruise";
const DEFAULT_EVASION: f64 = 0.5;
const DEFAULT_CRUISE: f64 = 0.7;

/// Heading loop: time constant from heading error to ordered yaw rate, rate-feedback gain, integral gain and
/// limits (integral only inside the band, anti-windup on saturation).
const HEADING_TAU_S: f64 = 4.0;
const RATE_GAIN: f64 = 1.0;
const HEADING_KI: f64 = 0.02;
const HEADING_I_MAX: f64 = 10.0;
const HEADING_I_BAND_RAD: f64 = 0.35;
/// Speed loop gains on speed error as a fraction of top speed.
const SPEED_KP: f64 = 2.0;
const SPEED_KI: f64 = 0.1;
const SPEED_I_MAX: f64 = 5.0;
/// Formation: along-track time constant, join distance floor.
const FORMATION_TAU_S: f64 = 15.0;
const FORMATION_JOIN_M: f64 = 600.0;
/// Zigzag amplitude and base leg.
const ZIGZAG_RAD: f64 = PI / 6.0;
const ZIGZAG_LEG_S: f64 = 40.0;
const ZIGZAG_CYCLE: [f64; 5] = [1.0, 0.7, 1.3, 0.85, 1.15];
const PATTERN_NONE: u8 = 0;
const PATTERN_ZIGZAG: u8 = 1;
const PATTERN_COMB: u8 = 2;
const PATTERN_BEAM: u8 = 3;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum HelmMode { #[default] HoldHeading, FollowRoute, Station, Formation, Evading, Stopped }

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct HelmState {
    pub mode: HelmMode,
    pub waypoint: u16,
    pub heading_integral: f64,
    pub heading_prev_error: f64,
    pub speed_integral: f64,
    /// Evasion pattern phase (zigzag leg, time on leg).
    pub evade_leg: u8,
    pub evade_leg_ticks: u32,
    /// Heading and speed the officer is steering for now (Bridge Board, advisory while the player holds Helm).
    pub ordered_heading_rad: f64,
    pub ordered_speed_kn: f64,
    /// Fingerprint of the route being followed; a new route restarts at waypoint 0.
    #[serde(default)]
    pub(crate) route_sig: u64,
    /// The ship has passed the last waypoint and holds `hold_heading_rad`.
    #[serde(default)]
    pub(crate) route_done: bool,
    /// Course held with no route (and after a finished route).
    #[serde(default)]
    pub(crate) hold_heading_rad: f64,
    #[serde(default)]
    pub(crate) latched: bool,
    /// Station keeping: inside the arrival circle (with hysteresis), and the heading on arrival.
    #[serde(default)]
    pub(crate) on_station: bool,
    #[serde(default)]
    pub(crate) station_heading_rad: f64,
    /// Dubins approach in progress: start pose (x, z, heading) and target pose, and the pursuit index on it.
    #[serde(default)]
    pub(crate) approach: Option<[f64; 6]>,
    #[serde(default)]
    pub(crate) approach_wp: u16,
    /// Evasion: active pattern, the threat it answers and the side chosen (+1 / -1).
    #[serde(default)]
    pub(crate) evade_pattern: u8,
    #[serde(default)]
    pub(crate) evade_track: u32,
    #[serde(default)]
    pub(crate) evade_side: i8,
}

pub struct HelmIn<'a> {
    pub pose: Pose,
    pub vel: DVec3,
    /// Rate of change of the compass heading, rad/s, positive turning to starboard (clockwise seen from
    /// above, which is `-ang_vel.y` in the world frame).
    pub yaw_rate: f64,
    pub orders: &'a ShipOrders,
    pub turn: &'a TurnModel,
    /// Leader kinematics when keeping formation.
    pub leader: Option<Kinematics>,
    /// Inbound torpedo or missile tracks to comb or beam, for the evasion stage.
    pub threats: &'a [Track],
    pub sliders: &'a BTreeMap<String, f64>,
    /// Water depth ahead along the current heading at one stop distance, metres (grounding guard only).
    pub depth_ahead_m: f64,
    pub dt: f64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct HelmOut { pub heading_rad: f64, pub speed_kn: f64, pub throttle: f64, pub rudder: f64, pub mode: HelmMode }

// ---------------------------------------------------------------------------------------------------------
// Sanitised views of the input

struct Own { h: f64, pos: DVec2, vel: DVec2, u: f64, top: f64, r: f64, length: f64, draft: f64 }

fn finite_or(v: f64, d: f64) -> f64 { if v.is_finite() { v } else { d } }

fn own(input: &HelmIn<'_>) -> Own {
    let h = wrap_2pi(input.pose.heading_rad());
    let p = input.pose.pos;
    let pos = if p.is_finite() { DVec2::new(p.x, p.z) } else { DVec2::ZERO };
    let vel = if input.vel.is_finite() { DVec2::new(input.vel.x, input.vel.z) } else { DVec2::ZERO };
    let u = vel.dot(heading_dir(h));
    let t = input.turn;
    let top = if t.top_speed_mps.is_finite() && t.top_speed_mps > 0.1 { t.top_speed_mps.min(100.0) } else { 10.0 };
    let length = if t.length_m.is_finite() && t.length_m > 0.0 { t.length_m } else { 0.0 };
    let r = if t.turn_radius_m.is_finite() && t.turn_radius_m >= 1.0 { t.turn_radius_m.min(20_000.0) } else { (3.0 * length).max(100.0) };
    let draft = if t.draft_m.is_finite() && t.draft_m > 0.0 { t.draft_m } else { 0.0 };
    Own { h, pos, vel, u, top, r, length, draft }
}

fn slider(input: &HelmIn<'_>, key: &str, default: f64) -> f64 {
    input.sliders.get(key).copied().filter(|v| v.is_finite()).map_or(default, |v| v.clamp(0.0, 1.0))
}

fn sane_dt(dt: f64) -> f64 { if dt.is_finite() && dt > 0.0 { dt.min(1.0) } else { 1.0 / 30.0 } }

fn top_kn(o: &Own) -> f64 { o.top / KN }

/// Ordered speed in knots, or cruise.
fn ordered_kn(input: &HelmIn<'_>, o: &Own) -> f64 {
    match input.orders.speed_kn.filter(|v| v.is_finite()) {
        Some(v) => v.clamp(0.0, top_kn(o)),
        None => slider(input, SLIDER_CRUISE, DEFAULT_CRUISE) * top_kn(o),
    }
}

/// Grounding guard on the chosen speed (knots).
fn ground_guard(input: &HelmIn<'_>, o: &Own, heading: f64, speed_kn: f64) -> f64 {
    let depth = input.depth_ahead_m;
    if !depth.is_finite() { return speed_kn; }
    let margin = depth - o.draft;
    if margin < crate::ai::path::KEEL_CLEARANCE_M {
        if wrap_pi(heading - o.h).abs() < PI / 6.0 { 0.0 } else { speed_kn.min(0.2 * top_kn(o)) }
    } else if margin < 5.0 {
        speed_kn.min(0.3 * top_kn(o))
    } else {
        speed_kn
    }
}

fn route_sig(route: &[DVec2]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    let mut mix = |v: u64| { for b in v.to_le_bytes() { h ^= b as u64; h = h.wrapping_mul(0x0100_0000_01b3); } };
    mix(route.len() as u64);
    for p in route { mix(p.x.to_bits()); mix(p.y.to_bits()); }
    h
}

// ---------------------------------------------------------------------------------------------------------
// PID

struct Pid { rudder: f64, throttle: f64, heading_integral: f64, speed_integral: f64, error: f64 }

fn pid_eval(heading_rad: f64, speed_kn: f64, input: &HelmIn<'_>, state: &HelmState) -> Pid {
    let o = own(input);
    let dt = sane_dt(input.dt);
    let target = if heading_rad.is_finite() { wrap_2pi(heading_rad) } else { o.h };
    let err = wrap_pi(target - o.h);
    // Heading: error to an ordered yaw rate, limited by what full rudder gives at this speed, then rate
    // feedback; feed-forward makes P alone adequate, the integral trims steady bias.
    let r_max = o.u.abs().max(0.5) / o.r;
    let desired = (err / HEADING_TAU_S).clamp(-r_max, r_max);
    let rate = finite_or(input.yaw_rate, 0.0);
    let hi_old = finite_or(state.heading_integral, 0.0).clamp(-HEADING_I_MAX, HEADING_I_MAX);
    let base = desired / r_max + RATE_GAIN * (desired - rate) / r_max;
    let mut hi = hi_old;
    if err.abs() < HEADING_I_BAND_RAD {
        hi = (hi_old + err * dt).clamp(-HEADING_I_MAX, HEADING_I_MAX);
        let unsat = base + HEADING_KI * hi;
        if unsat.abs() > 1.0 && unsat.signum() == err.signum() { hi = hi_old; }
    }
    let mut rudder = base + HEADING_KI * hi;
    if o.u < -0.3 { rudder = -rudder; }
    let rudder = finite_or(rudder, 0.0).clamp(-1.0, 1.0);
    // Speed: cube-law feed-forward (throttle is a power fraction) plus PI on the error.
    let vt = if speed_kn.is_finite() { (speed_kn * KN).clamp(0.0, o.top) } else { 0.0 };
    let e = (vt - o.u) / o.top;
    let ff = { let f = vt / o.top; f * f * f };
    let stop = vt <= 0.01;
    // Astern only to stop or to shed more than a tenth of top speed; small overspeed just coasts down.
    let floor = if stop || o.u - vt > 0.1 * o.top { -1.0 } else { 0.0 };
    let si_old = finite_or(state.speed_integral, 0.0).clamp(-SPEED_I_MAX, SPEED_I_MAX);
    let mut si = (si_old + e * dt).clamp(-SPEED_I_MAX, SPEED_I_MAX);
    let raw = ff + SPEED_KP * e + SPEED_KI * si;
    if (raw > 1.0 && e > 0.0) || (raw < floor && e < 0.0) { si = si_old; }
    let raw = ff + SPEED_KP * e + SPEED_KI * si;
    let throttle = if stop && o.u.abs() < 0.05 { si = 0.0; 0.0 } else { finite_or(raw, 0.0).clamp(floor, 1.0) };
    Pid { rudder, throttle, heading_integral: hi, speed_integral: si, error: err }
}

fn out(heading: f64, speed_kn: f64, mode: HelmMode, input: &HelmIn<'_>, state: &mut HelmState) -> HelmOut {
    let heading = wrap_2pi(heading);
    let speed_kn = finite_or(speed_kn, 0.0).max(0.0);
    let pid = pid_eval(heading, speed_kn, input, state);
    state.mode = mode;
    state.ordered_heading_rad = heading;
    state.ordered_speed_kn = speed_kn;
    HelmOut { heading_rad: heading, speed_kn, throttle: pid.throttle, rudder: pid.rudder, mode }
}

// ---------------------------------------------------------------------------------------------------------
// Stages

/// The `HelmAutopilot` stage: choose the heading and speed for the standing orders (route following by
/// turning-circle-aware pure pursuit with a lookahead of at least one turning radius, L766) and hold them by
/// PID. Unit: `sys-officer-helm`.
pub fn autopilot(input: &HelmIn<'_>, state: &mut HelmState) -> HelmOut {
    let o = own(input);
    let route = &input.orders.route;
    let sig = route_sig(route);
    if sig != state.route_sig {
        state.route_sig = sig;
        state.waypoint = 0;
        state.route_done = false;
    }
    let mut speed = ordered_kn(input, &o);
    let (heading, mut mode);
    if !route.is_empty() {
        if state.route_done {
            heading = state.hold_heading_rad;
        } else {
            let p = pursue(route, o.pos, o.h, o.u, o.r, state.waypoint as usize);
            state.waypoint = p.waypoint.min(u16::MAX as usize) as u16;
            if p.finished {
                state.route_done = true;
                state.hold_heading_rad = p.final_heading_rad;
                heading = p.final_heading_rad;
            } else {
                // A route cleared mid-leg leaves the officer holding the heading it has now.
                state.hold_heading_rad = o.h;
                heading = p.heading_rad;
                speed *= p.speed_factor;
            }
        }
        state.latched = true;
        mode = HelmMode::FollowRoute;
    } else {
        let o_ = input.orders;
        if !state.latched || o_.station.is_some() || o_.formation.is_some() || speed <= 0.0 {
            state.hold_heading_rad = o.h;
            state.latched = true;
        }
        heading = state.hold_heading_rad;
        mode = HelmMode::HoldHeading;
    }
    if speed <= 0.0 { mode = HelmMode::Stopped; }
    let speed = ground_guard(input, &o, heading, speed);
    out(heading, speed, mode, input, state)
}

/// The `StationKeeping` stage: hold a point and heading, or a formation offset from a leader.
/// Unit: `sys-officer-helm`.
pub fn station_keeping(input: &HelmIn<'_>, state: &mut HelmState) -> Option<HelmOut> {
    let o = own(input);
    let orders = input.orders;
    if let (Some(slot), Some(leader)) = (orders.formation.as_ref(), input.leader.as_ref()) {
        if leader.pose.pos.is_finite() && leader.pose.rot.is_finite() {
            state.on_station = false;
            return Some(formation(input, state, &o, slot, leader));
        }
    }
    let Some((p, hdg)) = orders.station else { state.on_station = false; state.approach = None; return None };
    if !p.is_finite() { state.on_station = false; state.approach = None; return None; }
    let hdg = hdg.filter(|h| h.is_finite()).map(wrap_2pi);
    let cruise = ordered_kn(input, &o) * KN;
    let a_brake = if input.turn.stop_distance_m.is_finite() && input.turn.stop_distance_m > 1.0 {
        o.top * o.top / (2.0 * input.turn.stop_distance_m)
    } else if input.turn.accel_mps2.is_finite() && input.turn.accel_mps2 > 0.0 {
        input.turn.accel_mps2
    } else {
        0.2
    }
    .clamp(0.02, 5.0);
    let e = p - o.pos;
    let d = e.length();
    // While a route is being followed, take over only on its last leg once inside the approach distance.
    if !orders.route.is_empty() && !state.route_done {
        let on_last_leg = state.waypoint as usize + 1 >= orders.route.len();
        let approach = o.u.max(0.0) * o.u.max(0.0) / (2.0 * a_brake) + 2.0 * o.r;
        if !(on_last_leg && d < approach) { state.on_station = false; return None; }
    }
    let arrive = (0.5 * o.length).max(0.1 * o.r).max(15.0);
    if state.on_station {
        if d > 3.0 * arrive { state.on_station = false; }
    } else if d < arrive {
        state.on_station = true;
        state.station_heading_rad = o.h;
    }
    let (heading, speed_mps);
    if state.on_station {
        state.approach = None;
        heading = hdg.unwrap_or(state.station_heading_rad);
        speed_mps = 0.0;
    } else {
        let min_steer = (0.1 * o.top).max(0.5).min(cruise.max(0.5));
        let brake = |remaining: f64| libm::sqrt(2.0 * a_brake * (remaining - arrive).max(0.0)).min(cruise).max(min_steer);
        // With a heading to finish on, follow a Dubins approach (L766) by pure pursuit; without one, chase
        // the point.
        let approach = match hdg { Some(hf) => dubins_approach(state, &o, p, hf), None => { state.approach = None; None } };
        if let Some((h, remaining, finished)) = approach {
            if finished && d < 3.0 * arrive {
                state.on_station = true;
                state.station_heading_rad = o.h;
                state.approach = None;
                heading = hdg.unwrap_or(o.h);
                speed_mps = 0.0;
            } else {
                heading = h;
                speed_mps = brake(remaining);
            }
        } else {
            // A point inside the turning circle on its side cannot be reached by turning: stand on to open it.
            let stbd = DVec2::new(libm::cos(o.h), libm::sin(o.h));
            let side = if e.dot(stbd) >= 0.0 { 1.0 } else { -1.0 };
            let centre = o.pos + stbd * (o.r * side);
            if p.distance(centre) < 0.95 * o.r && heading_dir(o.h).dot(e) < d * 0.9 {
                heading = o.h;
                speed_mps = min_steer;
            } else {
                heading = bearing(e);
                speed_mps = brake(d);
            }
        }
    }
    let speed = ground_guard(input, &o, heading, speed_mps / KN);
    Some(out(heading, speed, HelmMode::Station, input, state))
}

/// Follow the Dubins approach to the pose (p, hf). The path is planned once from the pose where the approach
/// began and replanned from the current pose only when the ship strays more than half a turning radius from
/// it (replanning every tick would flip to a full loop on any small miss). Returns (heading, path length
/// remaining, passed the end).
fn dubins_approach(state: &mut HelmState, o: &Own, p: DVec2, hf: f64) -> Option<(f64, f64, bool)> {
    for attempt in 0..2 {
        let same = matches!(state.approach, Some(a) if a[3] == p.x && a[4] == p.y && a[5] == hf);
        if !same || attempt == 1 {
            state.approach = Some([o.pos.x, o.pos.y, o.h, p.x, p.y, hf]);
            state.approach_wp = 1;
        }
        let a = state.approach?;
        let start = DVec2::new(a[0], a[1]);
        let Some(path) = DubinsPath::shortest_from(posrot_heading(start, a[2]), posrot_heading(p, hf), o.r).ok().filter(|q| q.length().is_finite()) else {
            state.approach = None;
            return None;
        };
        let mut pts = vec![start];
        dubins_points(&path, o.r * ARC_STEP_RAD, &mut pts);
        if pts.len() < 2 { state.approach = None; return None; }
        let pp = pursue(&pts, o.pos, o.h, o.u, o.r, (state.approach_wp as usize).max(1));
        let w = pp.waypoint.clamp(1, pts.len() - 1);
        let proj = pts[w - 1].lerp(pts[w], proj_t(pts[w - 1], pts[w], o.pos).clamp(0.0, 1.0));
        if attempt == 0 && same && proj.distance(o.pos) > 0.5 * o.r { continue; }
        state.approach_wp = w.min(u16::MAX as usize) as u16;
        let remaining = proj.distance(pts[w]) + pts[w..].windows(2).map(|s| s[0].distance(s[1])).fold(0.0, |x, y| x + y);
        return Some((pp.heading_rad, remaining, pp.finished));
    }
    None
}

fn formation(input: &HelmIn<'_>, state: &mut HelmState, o: &Own, slot: &FormationSlot, leader: &Kinematics) -> HelmOut {
    let lh = wrap_2pi(leader.pose.heading_rad());
    let ahead = heading_dir(lh);
    let stbd = DVec2::new(libm::cos(lh), libm::sin(lh));
    let lp = DVec2::new(leader.pose.pos.x, leader.pose.pos.z);
    let off = if slot.offset_m.is_finite() { slot.offset_m } else { DVec2::ZERO };
    let slot_p = lp + ahead * off.x + stbd * off.y;
    let vl = if leader.lin_vel.is_finite() { DVec2::new(leader.lin_vel.x, leader.lin_vel.z) } else { DVec2::ZERO };
    let ul = vl.dot(ahead).max(0.0);
    let e = slot_p - o.pos;
    let along = e.dot(ahead);
    let dist = e.length();
    let join = (4.0 * o.r).max(FORMATION_JOIN_M);
    let (heading, speed_mps);
    if dist > join {
        if along < 0.0 && ul > 1.0 {
            // Well ahead of a moving leader's slot: stand on along its course and let it catch up.
            heading = bearing(slot_p + ahead * (2.0 * join) - o.pos);
            speed_mps = 0.5 * ul;
        } else {
            let vj = 0.95 * o.top;
            let pred = slot_p + vl * (dist / vj.max(1.0));
            heading = bearing(pred - o.pos);
            speed_mps = vj;
        }
    } else {
        let l = o.r.max(2.0 * o.length).max(50.0);
        heading = bearing(slot_p + ahead * l - o.pos);
        speed_mps = (ul + along / FORMATION_TAU_S).clamp(0.0, o.top);
    }
    let speed = ground_guard(input, o, heading, speed_mps / KN);
    out(heading, speed, HelmMode::Formation, input, state)
}

/// Torpedo or missile: a weapon track below the surface, heard only on sonar, or slower than a missile.
fn is_torpedo(t: &Track) -> bool {
    let sonar = SourceMask::PASSIVE_SONAR | SourceMask::ACTIVE_SONAR;
    let v = DVec2::new(t.vel.x, t.vel.z).length();
    t.pos.y < -0.5 || (!t.sources.is_empty() && sonar.contains(t.sources)) || (v.is_finite() && v < 40.0)
}

/// The `Evasion` stage: when orders or the `helm.evasion` slider ask for it and a threat is inbound, override
/// heading and speed with a pattern (zigzag, comb the torpedo track, beam the missile). `None` = no override.
/// Unit: `sys-officer-helm`.
pub fn evasion(input: &HelmIn<'_>, state: &mut HelmState) -> Option<HelmOut> {
    let o = own(input);
    let readiness = slider(input, SLIDER_EVASION, DEFAULT_EVASION);
    let ordered = input.orders.evade;
    let ttg_max = if ordered { f64::INFINITY } else { 20.0 + 100.0 * readiness };
    let threat = if ordered || readiness > 0.0 {
        let mut best: Option<(f64, u32, &Track)> = None;
        for t in input.threats {
            if !t.is_live() || !t.inbound || t.domain != Domain::Weapon || !t.pos.is_finite() || !t.vel.is_finite() { continue; }
            let rel = DVec2::new(t.pos.x, t.pos.z) - o.pos;
            let rv = DVec2::new(t.vel.x, t.vel.z) - o.vel;
            let range = rel.length();
            let closing = if range > 1e-6 { -rel.dot(rv) / range } else { f64::INFINITY };
            if !(closing > 0.1) { continue; }
            let ttg = range / closing;
            if ttg > ttg_max { continue; }
            if best.is_none_or(|(bt, bid, _)| ttg < bt || (ttg == bt && t.id.0 < bid)) { best = Some((ttg, t.id.0, t)); }
        }
        best.map(|b| b.2)
    } else {
        None
    };
    let full = top_kn(&o);
    let (heading, speed, pattern, track) = match threat {
        Some(t) => {
            let new_choice = |state: &HelmState, pat: u8| state.evade_pattern != pat || state.evade_track != t.id.0;
            if is_torpedo(t) {
                let tv = DVec2::new(t.vel.x, t.vel.z);
                let course = if tv.length_squared() > 0.25 { bearing(tv) } else { bearing(o.pos - DVec2::new(t.pos.x, t.pos.z)) };
                if new_choice(state, PATTERN_COMB) {
                    let away = wrap_pi(course - o.h).abs();
                    let toward = wrap_pi(course + PI - o.h).abs();
                    state.evade_side = if away <= toward { 1 } else { -1 };
                }
                let h = if state.evade_side >= 0 { course } else { course + PI };
                (h, full, PATTERN_COMB, t.id.0)
            } else {
                let b = bearing(DVec2::new(t.pos.x, t.pos.z) - o.pos);
                if new_choice(state, PATTERN_BEAM) {
                    let s = wrap_pi(b + FRAC_PI_2 - o.h).abs();
                    let p = wrap_pi(b - FRAC_PI_2 - o.h).abs();
                    state.evade_side = if s <= p { 1 } else { -1 };
                }
                (b + FRAC_PI_2 * state.evade_side.signum() as f64, full, PATTERN_BEAM, t.id.0)
            }
        }
        None if ordered => {
            let base = base_course(input, state, &o);
            if state.evade_pattern != PATTERN_ZIGZAG {
                state.evade_leg = 0;
                state.evade_leg_ticks = 0;
            }
            let dt = sane_dt(input.dt);
            let leg_s = |leg: u8| {
                let f = ZIGZAG_CYCLE[leg as usize % ZIGZAG_CYCLE.len()] * ZIGZAG_LEG_S;
                if leg == 0 { 0.5 * f } else { f }
            };
            state.evade_leg_ticks = state.evade_leg_ticks.saturating_add(1);
            if state.evade_leg_ticks as f64 * dt >= leg_s(state.evade_leg) {
                // Leg 0 is the half leg; the cycle then runs 1, 2, ... and wraps to 1, never back to 0.
                state.evade_leg = if state.evade_leg == u8::MAX { 1 } else { state.evade_leg + 1 };
                state.evade_leg_ticks = 0;
            }
            let sign = if state.evade_leg % 2 == 0 { 1.0 } else { -1.0 };
            (base + sign * ZIGZAG_RAD, ordered_kn(input, &o), PATTERN_ZIGZAG, 0)
        }
        None => {
            state.evade_pattern = PATTERN_NONE;
            state.evade_track = 0;
            state.evade_leg = 0;
            state.evade_leg_ticks = 0;
            return None;
        }
    };
    if pattern != PATTERN_ZIGZAG {
        state.evade_leg = 0;
        state.evade_leg_ticks = 0;
    }
    state.evade_pattern = pattern;
    state.evade_track = track;
    let heading = wrap_2pi(heading);
    let speed = ground_guard(input, &o, heading, speed);
    Some(out(heading, speed, HelmMode::Evading, input, state))
}

/// Course the zigzag swings about: the route's pursuit heading, else the held course.
fn base_course(input: &HelmIn<'_>, state: &HelmState, o: &Own) -> f64 {
    let route = &input.orders.route;
    if !route.is_empty() && !state.route_done && route_sig(route) == state.route_sig {
        return pursue(route, o.pos, o.h, o.u, o.r, state.waypoint as usize).heading_rad;
    }
    if state.latched { state.hold_heading_rad } else { o.h }
}

/// PID from heading and speed error to rudder and throttle. Shared with manual "ordered heading" input.
/// Returns `(rudder, throttle)`: rudder -1 (port) ..= 1 (starboard), throttle -1 (full astern) ..= 1. This
/// is the only function that advances the PID integrators; call it once per graph tick.
/// Unit: `sys-officer-helm`.
pub fn hold(heading_rad: f64, speed_kn: f64, input: &HelmIn<'_>, state: &mut HelmState) -> (f64, f64) {
    let pid = pid_eval(heading_rad, speed_kn, input, state);
    state.heading_integral = pid.heading_integral;
    state.speed_integral = pid.speed_integral;
    state.heading_prev_error = pid.error;
    if heading_rad.is_finite() { state.ordered_heading_rad = wrap_2pi(heading_rad); }
    state.ordered_speed_kn = finite_or(speed_kn, 0.0).max(0.0);
    (pid.rudder, pid.throttle)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ship::FormationSlot;

    fn turn() -> TurnModel { TurnModel { top_speed_mps: 15.0, turn_radius_m: 150.0, accel_mps2: 0.5, stop_distance_m: 300.0, beam_m: 8.0, draft_m: 3.0, length_m: 50.0 } }

    /// A plain kinematic hull: rudder sets the turn rate at the model radius, throttle is a power fraction.
    struct Hull { pos: DVec2, h: f64, u: f64, rate: f64 }
    impl Hull {
        fn new(pos: DVec2, h: f64, u: f64) -> Self { Self { pos, h, u, rate: 0.0 } }
        fn input<'a>(&self, orders: &'a ShipOrders, tm: &'a TurnModel, sliders: &'a BTreeMap<String, f64>, threats: &'a [Track], leader: Option<Kinematics>) -> HelmIn<'a> {
            let d = heading_dir(self.h);
            HelmIn {
                pose: Pose::from_heading(DVec3::new(self.pos.x, 0.0, self.pos.y), self.h),
                vel: DVec3::new(d.x * self.u, 0.0, d.y * self.u),
                yaw_rate: self.rate,
                orders, turn: tm, leader, threats, sliders, depth_ahead_m: 100.0, dt: 1.0 / 30.0,
            }
        }
        fn step(&mut self, rudder: f64, throttle: f64, tm: &TurnModel, dt: f64) {
            let target_rate = rudder * self.u / tm.turn_radius_m;
            self.rate += (target_rate - self.rate) * (dt / 2.0).min(1.0);
            self.h = wrap_2pi(self.h + self.rate * dt);
            let f = self.u / tm.top_speed_mps;
            self.u += tm.accel_mps2 * (throttle - f * f * f.abs()) * dt * 2.0;
            self.pos += heading_dir(self.h) * self.u * dt;
        }
    }

    /// One graph tick: stages in priority order (evasion over station keeping over autopilot), then hold.
    fn tick(hull: &mut Hull, orders: &ShipOrders, state: &mut HelmState, leader: Option<Kinematics>, threats: &[Track]) -> HelmOut {
        let tm = turn();
        let sliders = BTreeMap::new();
        let input = hull.input(orders, &tm, &sliders, threats, leader);
        let a = autopilot(&input, state);
        let s = station_keeping(&input, state);
        let e = evasion(&input, state);
        let o = e.or(s).unwrap_or(a);
        let (rudder, throttle) = hold(o.heading_rad, o.speed_kn, &input, state);
        hull.step(rudder, throttle, &tm, input.dt);
        o
    }

    fn track(id: u32, pos: DVec3, vel: DVec3) -> Track {
        Track {
            id: TrackId(id), side: Default::default(), domain: Domain::Weapon, truth: EntityId::NONE, pos, vel, r0_m: 10.0, sigma_v: 1.0,
            bearing_only: None, created_tick: 0, last_update_tick: 0, last_lock_tick: None, dropped_tick: None, grade: Default::default(),
            sources: SourceMask::SEARCH_RADAR, trend: Default::default(), rung: Default::default(), rung_reason: Default::default(),
            candidates: Default::default(), class: None, history: Vec::new(), rcs_m2: 0.1, consistent_ticks: 0, emitters: Vec::new(),
            id_override: None, stance: Stance::Hostile, stance_reason: Default::default(), stance_override: None, fired_on_us: false,
            inbound: true, engaged_by: 0, emitting: false, fit: None, out_of_action: false, struck: false,
        }
    }

    #[test]
    fn hold_signs_and_wrap() {
        let tm = turn();
        let orders = ShipOrders::default();
        let sliders = BTreeMap::new();
        let hull = Hull::new(DVec2::ZERO, 350f64.to_radians(), 8.0);
        let input = hull.input(&orders, &tm, &sliders, &[], None);
        let mut st = HelmState::default();
        let (r, t) = hold(10f64.to_radians(), 20.0, &input, &mut st);
        assert!(r > 0.0, "10 deg is to starboard of 350 deg");
        assert!(t > 0.0, "8 m/s is below 20 kn");
        assert!(st.speed_integral > 0.0);
        assert_eq!(st.heading_integral, 0.0, "full rudder: anti-windup holds the integral");
        hold(352f64.to_radians(), 20.0, &input, &mut st);
        assert!(st.heading_integral > 0.0);
        let (r, t) = hold(300f64.to_radians(), 0.0, &input, &mut st);
        assert!(r < 0.0 && t < 0.0, "turn to port and back down to stop");
        // Stages do not advance the integrators.
        let before = st.clone();
        let _ = autopilot(&input, &mut st);
        assert_eq!((st.heading_integral, st.speed_integral), (before.heading_integral, before.speed_integral));
        // Garbage input stays finite.
        let mut bad = hull.input(&orders, &tm, &sliders, &[], None);
        bad.vel = DVec3::splat(f64::NAN);
        bad.yaw_rate = f64::INFINITY;
        bad.dt = -1.0;
        let zero = TurnModel::default();
        bad.turn = &zero;
        let (r, t) = hold(f64::NAN, f64::NAN, &bad, &mut st);
        assert!(r.is_finite() && t.is_finite());
    }

    #[test]
    fn follows_a_route_and_holds_the_last_course() {
        let orders = ShipOrders { route: vec![DVec2::new(0.0, -1500.0), DVec2::new(1500.0, -1500.0), DVec2::new(1500.0, 0.0)], speed_kn: Some(20.0), ..Default::default() };
        let mut hull = Hull::new(DVec2::ZERO, 0.0, 5.0);
        let mut st = HelmState::default();
        let mut closest = [f64::INFINITY; 3];
        for _ in 0..30 * 600 {
            let o = tick(&mut hull, &orders, &mut st, None, &[]);
            assert_eq!(o.mode, HelmMode::FollowRoute);
            for (c, w) in closest.iter_mut().zip(&orders.route) { *c = c.min(hull.pos.distance(*w)); }
            if st.route_done && hull.pos.y > 300.0 { break; }
        }
        assert!(st.route_done, "route finished");
        assert!(closest[0] < 150.0 && closest[1] < 150.0, "corners cut by under one turning radius: {closest:?}");
        assert!(closest[2] < 20.0, "last waypoint passed closely: {closest:?}");
        assert!(wrap_pi(hull.h - PI).abs() < 0.05, "holds the last leg's course (south)");
        assert!((hull.u - 20.0 * KN).abs() < 1.0, "holds ordered speed: {}", hull.u);
        // A new route restarts at its first waypoint.
        let orders2 = ShipOrders { route: vec![DVec2::new(1500.0, 3000.0)], ..orders.clone() };
        tick(&mut hull, &orders2, &mut st, None, &[]);
        assert!(!st.route_done && st.waypoint == 0);
    }

    #[test]
    fn holds_heading_without_a_route() {
        let orders = ShipOrders { speed_kn: Some(15.0), ..Default::default() };
        let mut hull = Hull::new(DVec2::ZERO, 1.0, 7.0);
        let mut st = HelmState::default();
        for _ in 0..30 * 60 { tick(&mut hull, &orders, &mut st, None, &[]); }
        assert!((hull.h - 1.0).abs() < 0.02, "latched the starting heading, not north");
        let stop = ShipOrders { speed_kn: Some(0.0), ..Default::default() };
        let mut o = HelmOut::default();
        for _ in 0..30 * 120 { o = tick(&mut hull, &stop, &mut st, None, &[]); }
        assert_eq!(o.mode, HelmMode::Stopped);
        assert!(hull.u.abs() < 0.3, "stopped: {}", hull.u);
    }

    #[test]
    fn station_keeping_arrives_and_stops() {
        let target = DVec2::new(800.0, -1200.0);
        let orders = ShipOrders { station: Some((target, Some(FRAC_PI_2))), speed_kn: Some(20.0), ..Default::default() };
        let mut hull = Hull::new(DVec2::ZERO, PI, 6.0);
        let mut st = HelmState::default();
        for _ in 0..30 * 600 { tick(&mut hull, &orders, &mut st, None, &[]); }
        assert!(hull.pos.distance(target) < 75.0, "on station: {}", hull.pos.distance(target));
        assert!(hull.u.abs() < 0.5, "stopped: {}", hull.u);
        assert_eq!(st.mode, HelmMode::Station);
    }

    #[test]
    fn keeps_formation_on_a_moving_leader() {
        let slot = FormationSlot { leader: EntityId { index: 9, generation: 0 }, offset_m: DVec2::new(-300.0, 200.0) };
        let orders = ShipOrders { formation: Some(slot), ..Default::default() };
        let mut hull = Hull::new(DVec2::new(-900.0, 400.0), 0.5, 5.0);
        let mut st = HelmState::default();
        let mut lp = DVec2::ZERO;
        let lv = 7.0;
        for _ in 0..30 * 600 {
            lp.y -= lv / 30.0;
            let leader = Kinematics { pose: Pose::from_heading(DVec3::new(lp.x, 0.0, lp.y), 0.0), lin_vel: DVec3::new(0.0, 0.0, -lv), ang_vel: DVec3::ZERO };
            tick(&mut hull, &orders, &mut st, Some(leader), &[]);
        }
        // Leader heads north; the slot is 300 m astern and 200 m to starboard (east).
        let slot_p = lp + DVec2::new(200.0, 300.0);
        assert!(hull.pos.distance(slot_p) < 40.0, "in slot: {}", hull.pos.distance(slot_p));
        assert!(wrap_pi(hull.h).abs() < 0.1 && (hull.u - lv).abs() < 0.5);
        assert_eq!(st.mode, HelmMode::Formation);
    }

    #[test]
    fn evasion_beams_missiles_combs_torpedoes_and_zigzags() {
        let tm = turn();
        let sliders = BTreeMap::new();
        let orders = ShipOrders::default();
        let hull = Hull::new(DVec2::ZERO, 20f64.to_radians(), 8.0);
        // Missile 10 km north closing at 280 m/s: beam it on the nearer side (090).
        let missile = [track(4, DVec3::new(0.0, 5.0, -10_000.0), DVec3::new(0.0, 0.0, 280.0))];
        let mut st = HelmState::default();
        let o = evasion(&hull.input(&orders, &tm, &sliders, &missile, None), &mut st).unwrap();
        assert!((o.heading_rad - FRAC_PI_2).abs() < 1e-6 && o.mode == HelmMode::Evading);
        assert!((o.speed_kn - 15.0 / KN).abs() < 1e-6, "full speed");
        // Torpedo from the east running west: comb it bow-on with the smaller turn (from 080 to 090).
        let hull = Hull::new(DVec2::ZERO, 80f64.to_radians(), 8.0);
        let torp = [track(5, DVec3::new(1_500.0, -5.0, 0.0), DVec3::new(-25.0, 0.0, 0.0))];
        let mut st = HelmState::default();
        let o = evasion(&hull.input(&orders, &tm, &sliders, &torp, None), &mut st).unwrap();
        assert!((o.heading_rad - FRAC_PI_2).abs() < 1e-6);
        // Too far off at the default readiness, and nothing at readiness 0, unless evasion is ordered.
        let far = [track(6, DVec3::new(0.0, 5.0, -60_000.0), DVec3::new(0.0, 0.0, 280.0))];
        assert!(evasion(&hull.input(&orders, &tm, &sliders, &far, None), &mut HelmState::default()).is_none());
        let off: BTreeMap<String, f64> = [(SLIDER_EVASION.to_string(), 0.0)].into_iter().collect();
        assert!(evasion(&hull.input(&orders, &tm, &off, &missile, None), &mut HelmState::default()).is_none());
        let evade = ShipOrders { evade: true, ..Default::default() };
        assert!(evasion(&hull.input(&evade, &tm, &off, &far, None), &mut HelmState::default()).is_some());
        // Ordered zigzag with no threat: swings either side of the held course.
        let mut hull = Hull::new(DVec2::ZERO, 0.0, 8.0);
        let mut st = HelmState::default();
        let evade = ShipOrders { evade: true, speed_kn: Some(16.0), ..Default::default() };
        let mut seen = BTreeSet::new();
        for _ in 0..30 * 120 {
            let o = tick(&mut hull, &evade, &mut st, None, &[]);
            assert_eq!(o.mode, HelmMode::Evading);
            seen.insert((wrap_pi(o.heading_rad).to_degrees().round()) as i64);
        }
        assert_eq!(seen.into_iter().collect::<Vec<_>>(), vec![-30, 30]);
        assert!(st.evade_leg >= 2);
        // Threat gone and no order: no override and the pattern resets.
        let input = hull.input(&orders, &tm, &sliders, &[], None);
        assert!(evasion(&input, &mut st).is_none());
        assert_eq!((st.evade_leg, st.evade_pattern), (0, PATTERN_NONE));
    }

    #[test]
    fn grounding_guard_stops_ahead_of_shoal() {
        let tm = turn();
        let sliders = BTreeMap::new();
        let orders = ShipOrders { speed_kn: Some(20.0), ..Default::default() };
        let hull = Hull::new(DVec2::ZERO, 0.0, 8.0);
        let mut input = hull.input(&orders, &tm, &sliders, &[], None);
        input.depth_ahead_m = 4.0;
        let o = autopilot(&input, &mut HelmState::default());
        assert_eq!(o.speed_kn, 0.0);
        input.depth_ahead_m = 7.0;
        let o = autopilot(&input, &mut HelmState::default());
        assert!((o.speed_kn - 0.3 * 15.0 / KN).abs() < 1e-9);
    }
}
