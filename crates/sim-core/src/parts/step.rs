//! The per-tick mechanical step (see the module docs in `mod.rs` for the rules).
use super::build::{def_of, fin, gun_elev_limits, nonneg};
use super::{mount_dir_local, mount_zero_rad, HandArc, LaunchAim, MountState, PartControl, PartOrder, PartRuntime, PartState, SpawnRequest, VesselControls};
use crate::compartments::CompartmentSet;
use crate::math::{DQuat, DVec3, Kinematics};
use crate::rng::{self, Key, Stream};
use crate::time::DT;
use content::catalogue::Catalogue;
use content::ids::{CompartmentId, MunitionIdx, PartSlot};
use content::kit::part_rot_matrix;
use content::part::weapons::MunitionKind;
use content::part::{DecoyKind, EngineKind, GunDef, LauncherDef, LauncherKind, PartFunction, SensorKind};
use core::f64::consts::{PI, TAU};

/// Everything of one vessel the step reads or writes.
pub(crate) struct PartsIo<'a> {
    pub parts: &'a mut [PartRuntime],
    pub controls: &'a mut VesselControls,
    pub kin: Kinematics,
    pub compartments: &'a CompartmentSet,
    /// False for lost hulls and wrecks: everything shuts down.
    pub active: bool,
    pub last_fired_tick: &'a mut u64,
}

/// RNG key parts.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Keys { pub seed: u64, pub tick: u64, pub vessel: u64 }

/// Smallest on-target tolerance, radians.
const MIN_TOLERANCE_RAD: f64 = 1.0e-3;
/// Slack on the firing clock so 45 summed ticks make 0.75 s.
const CYCLE_EPS: f64 = 1e-9;
/// Hard bound on rounds one mount fires in one tick.
const MAX_ROUNDS_PER_TICK: u32 = 32;
/// Trainable launchers launch within this of their aim bearing.
const LAUNCH_TRAIN_TOLERANCE_RAD: f64 = 2.0 * PI / 180.0;
/// Launch kinematics (module docs).
const MISSILE_EJECT_MPS: f64 = 25.0;
const MISSILE_LAUNCH_ELEV_RAD: f64 = 30.0 * PI / 180.0;
const TORPEDO_EJECT_MPS: f64 = 12.0;
const RACK_ROLL_MPS: f64 = 3.0;
const MINE_DROP_MPS: f64 = 1.0;
const OFFBOARD_LIFT_MPS: f64 = 5.0;
/// Rotating antenna rate when the part names none (L412).
const DEFAULT_RPM: f64 = 24.0;
/// Ballast transfer: the large pump's flow (L638), m^3/s.
const BALLAST_M3PS: f64 = 0.15;
/// Waterjet nozzles cross their full range (2 x steer_max) in this many seconds.
const NOZZLE_FULL_TRAVEL_S: f64 = 2.0;
/// Decoy salvo pattern: fan step, and (distance out, height) per kind.
const DECOY_FAN_RAD: f64 = 15.0 * PI / 180.0;

fn decoy_offset(kind: DecoyKind) -> (f64, Option<f64>) {
    match kind {
        DecoyKind::Chaff => (40.0, Some(15.0)),
        DecoyKind::Flare => (30.0, Some(10.0)),
        // Smoke sits on the sea surface.
        DecoyKind::Smoke => (20.0, None),
        DecoyKind::TowedDecoy => (0.0, Some(0.0)),
    }
}

fn clamp01(v: f64) -> f64 { if v.is_finite() { v.clamp(0.0, 1.0) } else { 0.0 } }

fn wrap_pi(a: f64) -> f64 {
    if !a.is_finite() { return 0.0; }
    let mut r = a % TAU;
    if r > PI { r -= TAU; } else if r <= -PI { r += TAU; }
    r
}
fn wrap_tau(a: f64) -> f64 {
    if !a.is_finite() { return 0.0; }
    let r = a - TAU * libm::floor(a / TAU);
    if (0.0..TAU).contains(&r) { r } else { 0.0 }
}

fn unit_rot(q: DQuat) -> DQuat {
    let n = q.length_squared();
    if !n.is_finite() || n < 1e-24 { DQuat::IDENTITY } else { q.normalize() }
}

/// Base ticks covered by one call: `round(dt / DT)`.
fn base_ticks(dt: f64) -> u32 { if dt.is_finite() && dt > 0.0 { libm::round(dt / DT).clamp(0.0, 3_600.0) as u32 } else { 0 } }

/// Countdown steps this call at fractional rate `f` (0..=1) over base ticks `tick .. tick + n`, by a stateless
/// dither on the tick counter: the steps over any run of ticks total floor(run * f) within one.
fn dither(tick: u64, n: u32, f: f64) -> u32 {
    let f = clamp01(f);
    if n == 0 || f <= 0.0 { return 0; }
    if f >= 1.0 { return n; }
    // Reduce the tick so the products stay exact in f64.
    let t = (tick % (1u64 << 40)) as f64;
    (libm::floor((t + n as f64) * f) - libm::floor(t * f)).clamp(0.0, n as f64) as u32
}

fn count_down(v: &mut u32, steps: u32) { *v = v.saturating_sub(steps); }

/// Base ticks for a duration in seconds (at least one for a positive duration).
fn ticks_for(seconds: f64) -> u32 { if seconds.is_finite() && seconds > 0.0 { libm::ceil(seconds / DT - 1e-9).clamp(1.0, u32::MAX as f64) as u32 } else { 0 } }

/// Vessel geometry for this tick.
struct Frame { pos: DVec3, rot: DQuat, vel: DVec3 }
impl Frame {
    fn new(kin: &Kinematics) -> Self {
        let pos = if kin.pose.pos.is_finite() { kin.pose.pos } else { DVec3::ZERO };
        let vel = if kin.lin_vel.is_finite() { kin.lin_vel } else { DVec3::ZERO };
        Self { pos, rot: unit_rot(kin.pose.rot), vel }
    }
    fn world(&self, local: DVec3) -> DVec3 { self.pos + self.rot * local }
    fn dir(&self, local: DVec3) -> DVec3 { self.rot * local }
    /// Local direction of the part's +X axis.
    fn facing_local(rot: content::kit::PartRot) -> DVec3 {
        let m = part_rot_matrix(rot);
        DVec3::new(m[0][0] as f64, m[0][1] as f64, m[0][2] as f64)
    }
    /// Horizontal world unit vector of a local direction, falling back to the bow, then north.
    fn flat(&self, local: DVec3) -> DVec3 {
        for d in [self.dir(local), self.dir(DVec3::X)] {
            let f = DVec3::new(d.x, 0.0, d.z);
            if f.length_squared() > 1e-12 { return f.normalize(); }
        }
        DVec3::NEG_Z
    }
    /// Mount-frame training of a world direction about a zero bearing (inverse of [`mount_dir_local`]).
    fn train_of(&self, zero_rad: f64, dir_world: DVec3) -> Option<f64> {
        let local = self.rot.inverse() * dir_world;
        let (s, c) = (libm::sin(zero_rad), libm::cos(zero_rad));
        let mx = local.x * c - local.z * s;
        let mz = local.x * s + local.z * c;
        if !(mx.is_finite() && mz.is_finite()) || mx * mx + mz * mz < 1e-18 { return None; }
        Some(libm::atan2(-mz, mx))
    }
}

/// Per-part view used while one part is processed.
#[derive(Clone, Copy)]
struct Drive {
    /// Working and enabled.
    on: bool,
    /// Damage output factor times power supply, 0 when off.
    power: f64,
}

fn control_of(controls: &VesselControls, slot: PartSlot) -> PartControl { controls.parts.get(slot.index()).copied().unwrap_or_default() }

fn clear_order(controls: &mut VesselControls, slot: PartSlot) {
    if let Some(c) = controls.parts.get_mut(slot.index()) { c.order = PartOrder::None; }
}

fn is_one_shot(o: &PartOrder) -> bool { matches!(o, PartOrder::Launch(_) | PartOrder::DropMine | PartOrder::FireDecoy | PartOrder::LaunchOffboard | PartOrder::RecoverOffboard) }

/// Index of a slot in the part list (normally slot == index).
fn index_of(parts: &[PartRuntime], slot: PartSlot) -> Option<usize> {
    if parts.get(slot.index()).is_some_and(|p| p.slot == slot) { return Some(slot.index()); }
    parts.iter().position(|p| p.slot == slot)
}

pub(crate) fn step(io: PartsIo<'_>, keys: Keys, cat: &Catalogue, dt: f64, out: &mut Vec<SpawnRequest>) {
    let PartsIo { parts, controls, kin, compartments, active, last_fired_tick } = io;
    if !active { shut_down(parts); return; }
    let dt = nonneg(dt);
    let n_ticks = base_ticks(dt);
    let frame = Frame::new(&kin);
    // Process in ascending slot (the list is normally already in slot order).
    let mut order: Vec<usize> = (0..parts.len()).collect();
    if !parts.windows(2).all(|w| w[0].slot < w[1].slot) { order.sort_by_key(|&i| parts[i].slot); }

    // Priorities for the next network solve.
    for p in parts.iter_mut() {
        let Some(def) = def_of(cat, p) else { continue };
        let base = super::build::default_priority(def);
        let ctl = control_of(controls, p.slot);
        p.priority = if base == 0 { 0 } else if (1..=5).contains(&ctl.priority) { ctl.priority } else { base };
    }

    // Engines spool; their signed output is pooled for the propulsors.
    let throttle_cmd = fin(controls.throttle, 0.0).clamp(-1.0, 1.0);
    let mut net_kw = 0.0;
    for &i in &order {
        let p = &mut parts[i];
        let Some(PartFunction::Engine(e)) = def_of(cat, p).map(|d| &d.function) else { continue };
        let on = p.works() && control_of(controls, p.slot).enabled;
        let supply = clamp01(if e.kind == EngineKind::ElectricMotor { p.links.power_supplied } else { p.links.fuel_supplied });
        let of = p.output_factor();
        let PartState::Engine { throttle, output_kw } = &mut p.state else { continue };
        let target = if on { throttle_cmd } else { 0.0 };
        let cur = fin(*throttle, 0.0).clamp(-1.0, 1.0);
        let spool = fin(e.spool_s, 0.0);
        let rate = if spool > 0.0 { dt / spool } else { f64::INFINITY };
        let next = cur + (target - cur).clamp(-rate, rate);
        *throttle = next.clamp(-1.0, 1.0);
        *output_kw = if on { throttle.abs() * nonneg(e.shaft_kw) * of * supply } else { 0.0 };
        net_kw += if *throttle < 0.0 { -*output_kw } else { *output_kw };
    }
    let mut prop_cap = 0.0;
    for &i in &order {
        if let Some(PartFunction::Propulsor(pd)) = def_of(cat, &parts[i]).map(|d| &d.function) { prop_cap += absorb(&parts[i], controls, pd.max_shaft_kw); }
    }

    // Hoist capacity this call in rounds, shared by the guns they serve in slot order.
    let mut hoist_budget = vec![0.0f64; parts.len()];
    for (i, p) in parts.iter().enumerate() {
        if let Some(PartFunction::Hoist { rounds_per_min }) = def_of(cat, p).map(|d| &d.function) {
            let d = drive(p, controls);
            hoist_budget[i] = nonneg(*rounds_per_min) / 60.0 * dt * d.power;
        }
    }
    let rudder_cmd = fin(controls.rudder, 0.0).clamp(-1.0, 1.0);
    let ballast_rate_kg_s = BALLAST_M3PS * nonneg(cat.tuning.buoyancy.water_density);

    for &i in &order {
        let Some(def) = def_of(cat, &parts[i]) else { continue };
        let slot = parts[i].slot;
        let ctl = control_of(controls, slot);
        let d = drive(&parts[i], controls);
        let mut consumed = false;
        match &def.function {
            PartFunction::Propulsor(pd) => {
                let cap = absorb(&parts[i], controls, pd.max_shaft_kw);
                let share = if prop_cap > 0.0 { (net_kw.abs() * cap / prop_cap).min(cap) } else { 0.0 };
                let sign = if net_kw < 0.0 { -1.0 } else { 1.0 };
                let steer_max = nonneg(pd.steer_max_deg).to_radians();
                let p = &mut parts[i];
                let steer_power = clamp01(p.links.power_supplied) * p.output_factor();
                if let PartState::Propulsor { shaft_kw, thrust_n, steer_rad } = &mut p.state {
                    *shaft_kw = share;
                    *thrust_n = sign * share * nonneg(pd.thrust_n_per_kw);
                    let cur = fin(*steer_rad, 0.0);
                    if steer_max > 0.0 {
                        let target = if d.on { rudder_cmd * steer_max } else { cur };
                        let rate = 2.0 * steer_max / NOZZLE_FULL_TRAVEL_S * dt * if d.on { steer_power } else { 0.0 };
                        *steer_rad = (cur + (target - cur).clamp(-rate, rate)).clamp(-steer_max, steer_max);
                    } else {
                        *steer_rad = 0.0;
                    }
                }
            }
            PartFunction::Rudder { max_deg, rate_dps, .. } => {
                let max = nonneg(*max_deg).to_radians();
                let rate = nonneg(*rate_dps).to_radians() * dt * d.power;
                if let PartState::Rudder { angle_rad } = &mut parts[i].state {
                    let cur = fin(*angle_rad, 0.0).clamp(-max, max);
                    let target = if d.on { rudder_cmd * max } else { cur };
                    *angle_rad = (cur + (target - cur).clamp(-rate, rate)).clamp(-max, max);
                }
            }
            PartFunction::Ballast { max_kg } => {
                let max = nonneg(*max_kg);
                let rate = ballast_rate_kg_s * dt * d.power;
                if let PartState::Ballast { kg } = &mut parts[i].state {
                    let cur = fin(*kg, 0.0).clamp(0.0, max);
                    let target = match ctl.order { PartOrder::Ballast { kg: t } if d.on && t.is_finite() => t.clamp(0.0, max), _ => cur };
                    *kg = cur + (target - cur).clamp(-rate, rate);
                }
            }
            PartFunction::Gun(g) => {
                if step_gun(parts, i, g, &ctl, d, &mut hoist_budget, &frame, keys, dt, out) { *last_fired_tick = keys.tick; }
            }
            PartFunction::Launcher(l) => {
                let (launched, done) = step_launcher(parts, i, l, &ctl, d, &frame, keys, n_ticks, dt, cat, out);
                if launched { *last_fired_tick = keys.tick; }
                consumed = done;
            }
            PartFunction::MineRail { mine, .. } => {
                let mine = cat.munition_idx(mine);
                let p = &mut parts[i];
                let origin = frame.world(p.centre);
                // Mines roll off astern of the direction of travel (astern of the bow when stopped).
                let ground = DVec3::new(frame.vel.x, 0.0, frame.vel.z);
                let astern = if ground.length_squared() > 0.01 { -ground.normalize() } else { -frame.flat(DVec3::X) };
                if let PartState::MineRail { mines, since_last_m } = &mut p.state {
                    let run = DVec3::new(frame.vel.x, 0.0, frame.vel.z).length() * dt;
                    if *since_last_m < f64::MAX { *since_last_m = (fin(*since_last_m, 0.0).max(0.0) + run).min(f64::MAX); }
                    if ctl.order == PartOrder::DropMine {
                        consumed = true;
                        if let (true, true, Some(m)) = (d.on && d.power > 0.0, *mines > 0, mine) {
                            if origin.is_finite() {
                                out.push(SpawnRequest::Munition { slot, munition: m, origin, velocity: frame.vel + astern * MINE_DROP_MPS, aim: LaunchAim::Point(origin) });
                                *mines -= 1;
                                *since_last_m = 0.0;
                            }
                        }
                    }
                }
            }
            PartFunction::Decoy(dd) => {
                consumed = step_decoy(&mut parts[i], dd, &ctl, d, &frame, keys, n_ticks, out);
            }
            PartFunction::Sensor(s) => {
                let rotate = matches!(s.kind, SensorKind::SearchRadar | SensorKind::NavRadar).then(|| match s.rpm { Some(r) if r.is_finite() && r > 0.0 => r, _ => DEFAULT_RPM });
                let emitter = matches!(s.kind, SensorKind::SearchRadar | SensorKind::NavRadar | SensorKind::FireControlRadar | SensorKind::ActiveSonar);
                step_sensor(&mut parts[i], &ctl, d, rotate, emitter, dt);
            }
            PartFunction::Director | PartFunction::CameraMast { .. } => step_sensor(&mut parts[i], &ctl, d, None, false, dt),
            PartFunction::Jammer { .. } => {
                if let PartState::Jammer { radiating } = &mut parts[i].state {
                    let want = match ctl.order { PartOrder::Emit { radiating: r, .. } => r, _ => *radiating };
                    *radiating = want && d.on && d.power > 0.0;
                }
            }
            PartFunction::Pump { .. } => {
                let own = parts[i].compartment;
                if let PartState::Pump { running, target } = &mut parts[i].state {
                    let (want, tgt) = match ctl.order { PartOrder::Pump { running: r, target: t } => (r, t), _ => (*running, *target) };
                    *target = tgt.filter(|&t| valid_pump_target(compartments, own, t));
                    *running = want && d.on && d.power > 0.0;
                }
            }
            PartFunction::Door => {
                if let (PartOrder::Door { closed: c }, PartState::Door { closed }) = (ctl.order, &mut parts[i].state) { *closed = c; }
            }
            PartFunction::DroneBay { drone, .. } => {
                let m = cat.munition_idx(drone);
                let p = &mut parts[i];
                let origin = frame.world(p.centre);
                if let PartState::DroneBay { aboard, recharge_ticks } = &mut p.state {
                    count_down(recharge_ticks, dither(keys.tick, n_ticks, d.power));
                    consumed = offboard_order(&ctl.order, d, *aboard > 0, *recharge_ticks == 0, m, origin, OFFBOARD_LIFT_MPS, &frame, slot, out, || *aboard -= 1);
                }
            }
            PartFunction::HeliDeck { helicopter } => {
                let m = cat.munition_idx(helicopter);
                let p = &mut parts[i];
                let origin = frame.world(p.centre);
                if let PartState::HeliDeck { aboard } = &mut p.state {
                    consumed = offboard_order(&ctl.order, d, *aboard, true, m, origin, OFFBOARD_LIFT_MPS, &frame, slot, out, || *aboard = false);
                }
            }
            PartFunction::RhibDavit | PartFunction::BoardingKit { .. } => {
                let m = rhib_munition(cat);
                let p = &mut parts[i];
                let origin = frame.world(p.centre);
                if let PartState::Davit { rhib_aboard } = &mut p.state {
                    consumed = offboard_order(&ctl.order, d, *rhib_aboard, true, m, origin, 0.0, &frame, slot, out, || *rhib_aboard = false);
                }
            }
            // Engines ran above; generators, batteries and tanks belong to the network solve; magazines are
            // drawn by their guns and launchers, and fire and flooding own their cook-off and flood state;
            // hoists act through the guns they serve.
            PartFunction::Engine(_) | PartFunction::Generator { .. } | PartFunction::Battery { .. } | PartFunction::FuelTank { .. }
            | PartFunction::Magazine { .. } | PartFunction::Hoist { .. } | PartFunction::Structure | PartFunction::Console
            | PartFunction::Berths { .. } | PartFunction::PassengerBerths { .. } | PartFunction::RecoveryNet | PartFunction::Crane => {}
        }
        // A one-shot order sent to a part that has no use for it is dropped too.
        if consumed || (is_one_shot(&ctl.order) && !understands(&def.function, &ctl.order)) { clear_order(controls, slot); }
    }
}

/// True when the part acts on this one-shot order (and so decides itself when it is used up).
fn understands(f: &PartFunction, o: &PartOrder) -> bool {
    match o {
        PartOrder::Launch(_) => matches!(f, PartFunction::Launcher(_)),
        PartOrder::DropMine => matches!(f, PartFunction::MineRail { .. }),
        PartOrder::FireDecoy => matches!(f, PartFunction::Decoy(_)),
        PartOrder::LaunchOffboard | PartOrder::RecoverOffboard => matches!(f, PartFunction::DroneBay { .. } | PartFunction::HeliDeck { .. } | PartFunction::RhibDavit | PartFunction::BoardingKit { .. }),
        _ => false,
    }
}

/// Shaft power a propulsor can absorb now, kW.
fn absorb(p: &PartRuntime, controls: &VesselControls, max_kw: f64) -> f64 {
    if p.works() && control_of(controls, p.slot).enabled { nonneg(max_kw) * p.output_factor() } else { 0.0 }
}

fn drive(p: &PartRuntime, controls: &VesselControls) -> Drive {
    let on = p.works() && control_of(controls, p.slot).enabled;
    Drive { on, power: if on { p.output_factor() * clamp01(p.links.power_supplied) } else { 0.0 } }
}

fn valid_pump_target(c: &CompartmentSet, own: Option<CompartmentId>, t: CompartmentId) -> bool {
    let Some(own) = own else { return false };
    if t == own { return true; }
    if c.get(t).is_none() { return false; }
    c.adjacency.iter().any(|j| (j.a == own && j.b == t) || (j.a == t && j.b == own))
}

fn rhib_munition(cat: &Catalogue) -> Option<MunitionIdx> {
    cat.munitions.values().position(|m| m.kind == MunitionKind::Rhib).map(|i| MunitionIdx(i as u16))
}

/// Shared launch-offboard and recover handling for bays, decks and davits. Returns whether the order is used up.
#[allow(clippy::too_many_arguments)]
fn offboard_order(order: &PartOrder, d: Drive, aboard: bool, ready: bool, munition: Option<MunitionIdx>, origin: DVec3, lift_mps: f64, frame: &Frame, slot: PartSlot, out: &mut Vec<SpawnRequest>, take: impl FnOnce()) -> bool {
    match order {
        PartOrder::LaunchOffboard => {
            let Some(m) = munition else { return true };
            if !d.on || !aboard || !origin.is_finite() { return true; }
            if d.power <= 0.0 || !ready { return false; }
            out.push(SpawnRequest::Munition { slot, munition: m, origin, velocity: frame.vel + DVec3::Y * lift_mps, aim: LaunchAim::Point(origin) });
            take();
            true
        }
        // Recovery completes when the body arrives (`recover_offboard`); the order itself is acknowledged.
        PartOrder::RecoverOffboard => true,
        _ => false,
    }
}

fn step_sensor(p: &mut PartRuntime, ctl: &PartControl, d: Drive, rotate_rpm: Option<f64>, emitter: bool, dt: f64) {
    let PartState::Sensor { bearing_rad, swept_from_rad, radiating, train_rad } = &mut p.state else { return };
    let alive = d.on && d.power > 0.0;
    let want = match ctl.order {
        PartOrder::Emit { radiating: r, train_rad: t } => {
            if alive && t.is_finite() { *train_rad = wrap_pi(t); }
            r
        }
        _ => *radiating,
    };
    *radiating = emitter && want && alive;
    *train_rad = wrap_pi(*train_rad);
    match rotate_rpm {
        Some(rpm) => {
            let b = wrap_tau(*bearing_rad);
            *swept_from_rad = b;
            *bearing_rad = if alive { wrap_tau(b + rpm * TAU / 60.0 * dt) } else { b };
        }
        None => {
            *bearing_rad = wrap_tau(*train_rad);
            *swept_from_rad = *bearing_rad;
        }
    }
}

/// Take up to `want` rounds from the magazines in draw order. Returns rounds taken.
fn take_rounds(parts: &mut [PartRuntime], mags: &[PartSlot], want: u32) -> u32 {
    let mut got = 0;
    for &m in mags {
        if got >= want { break; }
        let Some(mi) = index_of(parts, m) else { continue };
        let mp = &mut parts[mi];
        if !mp.works() { continue; }
        if let PartState::Magazine { rounds, ammo_kg, flooded: false, .. } = &mut mp.state {
            let k = (*rounds).min(want - got);
            if k == 0 { continue; }
            let per = if *rounds > 0 { nonneg(*ammo_kg) / *rounds as f64 } else { 0.0 };
            *rounds -= k;
            *ammo_kg = if *rounds == 0 { 0.0 } else { (nonneg(*ammo_kg) - per * k as f64).max(0.0) };
            got += k;
        }
    }
    got
}

fn rounds_available(parts: &[PartRuntime], mags: &[PartSlot]) -> bool {
    mags.iter().any(|&m| index_of(parts, m).is_some_and(|i| parts[i].works() && matches!(parts[i].state, PartState::Magazine { rounds, flooded: false, .. } if rounds > 0)))
}

/// Target and order validity in the mount's limits: (train target, elevation target, order within limits).
pub(super) fn mount_targets(o_train: f64, o_elev: f64, g_lo: f64, g_hi: f64, arc: Option<HandArc>) -> (f64, f64, bool) {
    let (mut lo, mut hi) = (g_lo, g_hi);
    if let Some(a) = arc { lo = lo.max(a.e_min); hi = hi.min(a.e_max); if lo > hi { hi = lo; } }
    let e = o_elev.clamp(lo, hi);
    let e_ok = o_elev >= lo - 1e-12 && o_elev <= hi + 1e-12;
    match arc {
        Some(a) if !a.full => {
            let t = a.t_min + (o_train - a.t_min).rem_euclid(TAU);
            if t <= a.t_max + 1e-12 { (t.min(a.t_max), e, e_ok) }
            else if t - a.t_max <= a.t_min + TAU - t { (a.t_max, e, false) }
            else { (a.t_min, e, false) }
        }
        _ => (wrap_pi(o_train), e, e_ok),
    }
}

/// One gun mount. Returns true when it fired.
#[allow(clippy::too_many_arguments)]
fn step_gun(parts: &mut [PartRuntime], i: usize, g: &GunDef, ctl: &PartControl, d: Drive, hoist_budget: &mut [f64], frame: &Frame, keys: Keys, dt: f64, out: &mut Vec<SpawnRequest>) -> bool {
    let PartState::Mount(mut m) = parts[i].state else { return false };
    let mut mech = parts[i].links.mech;
    let feed = mech.feed;
    let slot = parts[i].slot;
    let barrels = g.barrels.max(1) as u32;
    let rof = nonneg(g.rof_rpm);
    let capacity = g.ready_rounds;

    // Feed: hoists (or a touching magazine) lift rounds from the kit's magazines.
    let mags: Vec<PartSlot> = feed.magazines().collect();
    if m.ready_rounds < capacity && parts[i].works() && rounds_available(parts, &mags) {
        let need = (capacity - m.ready_rounds) as f64 - nonneg(mech.feed_progress);
        let hoists: Vec<usize> = feed.hoists().filter_map(|h| index_of(parts, h)).collect();
        let direct = if feed.direct { rof * barrels as f64 / 60.0 * dt * d.power } else { 0.0 };
        let lifted: f64 = hoists.iter().map(|&h| hoist_budget[h]).sum();
        let take = (direct + lifted).min(need.max(0.0));
        // Direct feed is used first, then the hoists in slot order.
        let mut owed = (take - direct).max(0.0);
        for &h in &hoists { let t = owed.min(hoist_budget[h]); hoist_budget[h] -= t; owed -= t; }
        mech.feed_progress = nonneg(mech.feed_progress) + take;
        let whole = libm::floor(mech.feed_progress + 1e-9).clamp(0.0, (capacity - m.ready_rounds) as f64) as u32;
        if whole > 0 {
            let got = take_rounds(parts, &mags, whole);
            m.ready_rounds += got;
            mech.feed_progress = if got < whole { 0.0 } else { (mech.feed_progress - got as f64).max(0.0) };
        }
    }
    if m.ready_rounds >= capacity || !rounds_available(parts, &mags) { mech.feed_progress = 0.0; }

    // Slew.
    let (g_lo, g_hi) = gun_elev_limits(g);
    let sigma = nonneg(g.dispersion_mrad) * 1e-3;
    let tol = sigma.max(MIN_TOLERANCE_RAD);
    let order = match ctl.order { PartOrder::Mount(o) if d.on && o.train_rad.is_finite() && o.elevation_rad.is_finite() => Some(stabilised(o, mount_zero_rad(parts[i].rot), frame.rot)), _ => None };
    let full = mech.arc.is_none_or(|a| a.full);
    let mut cur_t = fin(m.train_rad, 0.0);
    let mut cur_e = fin(m.elevation_rad, 0.0).clamp(g_lo, g_hi);
    if full { cur_t = wrap_pi(cur_t); } else if let Some(a) = mech.arc.filter(|a| !(a.t_min..=a.t_max).contains(&cur_t)) { cur_t = mount_targets(cur_t, cur_e, g_lo, g_hi, Some(a)).0; }
    let mut on_target = false;
    if let Some(o) = order {
        let (tt, te, ok) = mount_targets(o.train_rad, o.elevation_rad, g_lo, g_hi, mech.arc);
        let rt = nonneg(g.train_rate_dps).to_radians() * dt * d.power;
        let re = nonneg(g.elevation_rate_dps).to_radians() * dt * d.power;
        let dtr = if full { wrap_pi(tt - cur_t) } else { tt - cur_t };
        cur_t += dtr.clamp(-rt, rt);
        if full { cur_t = wrap_pi(cur_t); }
        cur_e = (cur_e + (te - cur_e).clamp(-re, re)).clamp(g_lo, g_hi);
        let et = if full { wrap_pi(tt - cur_t) } else { tt - cur_t };
        on_target = ok && et.abs() <= tol && (te - cur_e).abs() <= tol;
    }
    m.train_rad = cur_t;
    m.elevation_rad = cur_e;
    m.on_target = on_target;

    // Fire on a continuous cycle clock.
    let rate = rof * d.power;
    let interval = if rate > 0.0 { 60.0 / rate } else { f64::INFINITY };
    // A clock already at a full cycle is a loaded gun waiting: it fires this tick without waiting a tick more.
    mech.cycle_s = nonneg(mech.cycle_s);
    if mech.cycle_s + CYCLE_EPS < interval { mech.cycle_s += dt; }
    let mut fired = 0u32;
    if let Some(o) = order.filter(|o| o.fire && on_target) {
        let cap = if o.max_rounds == 0 { MAX_ROUNDS_PER_TICK } else { (o.max_rounds as u32).min(MAX_ROUNDS_PER_TICK) };
        let origin = frame.world(parts[i].centre);
        let zero = mount_zero_rad(parts[i].rot);
        let err_t = fin(o.aim_error_rad[0], 0.0);
        let err_e = fin(o.aim_error_rad[1], 0.0);
        let barrel = frame.dir(mount_dir_local(zero, cur_t + err_t, cur_e + err_e));
        let muzzle = nonneg(g.muzzle_mps);
        let fine = origin.is_finite() && barrel.is_finite() && muzzle > 0.0;
        while fine && mech.cycle_s + CYCLE_EPS >= interval && fired < cap && m.ready_rounds > 0 {
            let volley = barrels.min(m.ready_rounds).min(cap - fired);
            for _ in 0..volley {
                let k = m.fired.wrapping_add(fired) as u64;
                let dir = disperse(barrel, sigma, keys, slot, k);
                out.push(SpawnRequest::Shell { slot, origin, velocity: dir * muzzle + frame.vel, rounds: 1 });
                fired += 1;
            }
            m.ready_rounds -= volley;
            mech.cycle_s = (mech.cycle_s - interval).max(0.0);
        }
    }
    m.fired = m.fired.saturating_add(fired);
    if interval.is_finite() { mech.cycle_s = mech.cycle_s.min(interval); } else { mech.cycle_s = 0.0; }
    m.cooldown_ticks = if !interval.is_finite() { u32::MAX } else if mech.cycle_s + CYCLE_EPS >= interval { 0 } else { libm::ceil((interval - mech.cycle_s) / DT - 1e-6).clamp(0.0, u32::MAX as f64) as u32 };
    parts[i].state = PartState::Mount(m);
    parts[i].links.mech = mech;
    fired > 0
}

/// An order laid against an earlier hull attitude, carried to this tick's: the same line in the world frame,
/// as training and elevation in the mount frame now (see [`super::MountOrder::laid_rot`]).
pub(super) fn stabilised(o: super::MountOrder, zero: f64, rot: DQuat) -> super::MountOrder {
    let Some(r0) = o.laid_rot.filter(|r| r.is_finite() && r.length_squared() > 1e-12) else { return o };
    if !(rot.is_finite() && rot.length_squared() > 1e-12) { return o; }
    let l = rot.normalize().inverse() * (r0.normalize() * super::mount_dir_local(zero, o.train_rad, o.elevation_rad));
    let (s, c) = (libm::sin(zero), libm::cos(zero));
    let (mx, mz) = (l.x * c - l.z * s, l.x * s + l.z * c);
    let horiz = libm::sqrt(mx * mx + mz * mz);
    let (train, elev) = (if horiz < 1e-15 { o.train_rad } else { libm::atan2(-mz, mx) }, libm::atan2(l.y, horiz));
    if !(train.is_finite() && elev.is_finite()) { return o; }
    super::MountOrder { train_rad: train, elevation_rad: elev, ..o }
}

/// Barrel direction with isotropic angular dispersion for round `k` (draws 4k and 4k + 2).
fn disperse(dir: DVec3, sigma: f64, keys: Keys, slot: PartSlot, k: u64) -> DVec3 {
    if sigma <= 0.0 { return dir; }
    let key = Key::new(keys.seed, keys.tick, Stream::GunDispersion, keys.vessel, slot.0 as u64);
    let draw = (k as u32).wrapping_mul(4);
    let (a, b) = (rng::normal(key.draw(draw)), rng::normal(key.draw(draw.wrapping_add(2))));
    let mut right = dir.cross(DVec3::Y);
    if right.length_squared() < 1e-12 { right = dir.cross(DVec3::X); }
    let right = right.normalize();
    let up = right.cross(dir).normalize();
    let out = dir + (right * a + up * b) * sigma;
    let l = out.length();
    if l.is_finite() && l > 0.0 { out / l } else { dir }
}

/// One launcher, tube or rack. Returns (launched, order used up).
#[allow(clippy::too_many_arguments)]
fn step_launcher(parts: &mut [PartRuntime], i: usize, l: &LauncherDef, ctl: &PartControl, d: Drive, frame: &Frame, keys: Keys, n_ticks: u32, dt: f64, cat: &Catalogue, out: &mut Vec<SpawnRequest>) -> (bool, bool) {
    let PartState::Launcher { mut ready, mut reload_ticks, mut interval_ticks, mut train_rad } = parts[i].state else {
        return (false, matches!(ctl.order, PartOrder::Launch(_)));
    };
    let slot = parts[i].slot;
    let mags: Vec<PartSlot> = parts[i].links.mech.feed.magazines().collect();
    let rot = parts[i].rot;
    let centre = parts[i].centre;
    let works = parts[i].works();
    let steps = dither(keys.tick, n_ticks, d.power);
    count_down(&mut interval_ticks, steps);
    // At-sea reload from the kit's magazines.
    let reloads = l.reload_s.is_finite() && l.reload_s > 0.0;
    if reloads && works && ready < l.cells {
        if reload_ticks == 0 && rounds_available(parts, &mags) { reload_ticks = ticks_for(l.reload_s); }
        else if reload_ticks > 0 {
            count_down(&mut reload_ticks, steps);
            if reload_ticks == 0 && take_rounds(parts, &mags, 1) == 1 { ready += 1; }
        }
    } else if ready >= l.cells || !works {
        reload_ticks = 0;
    }
    let zero = mount_zero_rad(rot);
    let origin = frame.world(centre);
    // Training toward the aim of a pending launch.
    let mut trained = true;
    if let Some(rate_dps) = l.train_rate_dps {
        train_rad = wrap_pi(fin(train_rad, 0.0));
        let want = match ctl.order {
            PartOrder::Launch(LaunchAim::Point(p)) if p.is_finite() => frame.train_of(zero, p - origin),
            PartOrder::Launch(LaunchAim::Bearing { heading_rad: h }) if h.is_finite() => frame.train_of(zero, DVec3::new(libm::sin(h), 0.0, -libm::cos(h))),
            _ => None,
        };
        if let (Some(t), true) = (want, d.on) {
            let rate = nonneg(rate_dps).to_radians() * dt * d.power;
            let e = wrap_pi(t - train_rad);
            train_rad = wrap_pi(train_rad + e.clamp(-rate, rate));
            trained = wrap_pi(t - train_rad).abs() <= LAUNCH_TRAIN_TOLERANCE_RAD;
        }
    }
    let mut launched = false;
    let mut done = false;
    if let PartOrder::Launch(aim) = ctl.order {
        let munition = cat.munition_idx(&l.munition);
        let can_ever = ready > 0 || (reloads && (reload_ticks > 0 || rounds_available(parts, &mags)));
        if !d.on || munition.is_none() || !can_ever || !origin.is_finite() {
            done = true;
        } else if ready > 0 && interval_ticks == 0 && d.power > 0.0 && trained {
            let dir = launch_dir(l, rot, train_rad, zero, frame);
            let speed = match l.kind { LauncherKind::Missile => MISSILE_EJECT_MPS, LauncherKind::TorpedoTube => TORPEDO_EJECT_MPS, LauncherKind::DepthChargeRack => RACK_ROLL_MPS };
            if let Some(m) = munition {
                out.push(SpawnRequest::Munition { slot, munition: m, origin, velocity: frame.vel + dir * speed, aim });
                ready -= 1;
                interval_ticks = ticks_for(l.interval_s);
                launched = true;
                done = true;
            }
        }
    }
    parts[i].state = PartState::Launcher { ready, reload_ticks, interval_ticks, train_rad };
    (launched, done)
}

/// World launch direction (module docs).
fn launch_dir(l: &LauncherDef, rot: content::kit::PartRot, train_rad: f64, zero: f64, frame: &Frame) -> DVec3 {
    let facing = Frame::facing_local(rot);
    let (flat_local, vertical) = match l.train_rate_dps {
        Some(_) => (mount_dir_local(zero, train_rad, 0.0), false),
        None => (DVec3::new(facing.x, 0.0, facing.z), facing.y.abs() > 0.7),
    };
    let flat = frame.flat(flat_local);
    match l.kind {
        LauncherKind::Missile if vertical => {
            let d = frame.dir(facing);
            if d.is_finite() && d.length_squared() > 1e-12 { d.normalize() } else { DVec3::Y }
        }
        LauncherKind::Missile => (flat * libm::cos(MISSILE_LAUNCH_ELEV_RAD) + DVec3::Y * libm::sin(MISSILE_LAUNCH_ELEV_RAD)).normalize(),
        LauncherKind::TorpedoTube | LauncherKind::DepthChargeRack => flat,
    }
}

/// One decoy launcher. Returns whether the order is used up.
#[allow(clippy::too_many_arguments)]
fn step_decoy(p: &mut PartRuntime, dd: &content::part::DecoyLauncherDef, ctl: &PartControl, d: Drive, frame: &Frame, keys: Keys, n_ticks: u32, out: &mut Vec<SpawnRequest>) -> bool {
    let slot = p.slot;
    let origin = frame.world(p.centre);
    let facing = frame.flat(Frame::facing_local(p.rot));
    let PartState::Decoy { rounds, reload_ticks, streamed } = &mut p.state else { return is_one_shot(&ctl.order) };
    count_down(reload_ticks, dither(keys.tick, n_ticks, d.power));
    if dd.kind == DecoyKind::TowedDecoy {
        if !d.on { *streamed = false; }
        if let PartOrder::StreamDecoy(want) = ctl.order {
            if !want { *streamed = false; }
            else if !*streamed && d.on && d.power > 0.0 && origin.is_finite() {
                *streamed = true;
                out.push(SpawnRequest::Decoy { slot, kind: DecoyKind::TowedDecoy, origin });
            }
        }
        // Towed decoys stream; they have no salvo to fire.
        return matches!(ctl.order, PartOrder::FireDecoy);
    }
    if ctl.order != PartOrder::FireDecoy { return false; }
    if !d.on || *rounds == 0 || !origin.is_finite() { return true; }
    if d.power <= 0.0 || *reload_ticks > 0 { return false; }
    let k = (dd.salvo.max(1) as u16).min(*rounds);
    let (dist, height) = decoy_offset(dd.kind);
    for j in 0..k {
        let a = (j as f64 - (k as f64 - 1.0) * 0.5) * DECOY_FAN_RAD;
        let (s, c) = (libm::sin(a), libm::cos(a));
        // Turn the facing about +Y by `a`.
        let dir = DVec3::new(facing.x * c + facing.z * s, 0.0, -facing.x * s + facing.z * c);
        let mut at = origin + dir * dist;
        at.y = match height { Some(h) => origin.y + h, None => 0.0 };
        out.push(SpawnRequest::Decoy { slot, kind: dd.kind, origin: at });
    }
    *rounds -= k;
    *reload_ticks = ticks_for(fin(dd.reload_s, 10.0).max(0.0));
    true
}

fn shut_down(parts: &mut [PartRuntime]) {
    for p in parts.iter_mut() {
        match &mut p.state {
            PartState::Engine { throttle, output_kw } => { *throttle = 0.0; *output_kw = 0.0; }
            PartState::Propulsor { shaft_kw, thrust_n, .. } => { *shaft_kw = 0.0; *thrust_n = 0.0; }
            PartState::Mount(m) => m.on_target = false,
            PartState::Sensor { radiating, .. } | PartState::Jammer { radiating } => *radiating = false,
            PartState::Pump { running, .. } => *running = false,
            PartState::Decoy { streamed, .. } => *streamed = false,
            _ => {}
        }
    }
}

pub(crate) fn recover_offboard(parts: &mut [PartRuntime], slot: PartSlot, munition: MunitionIdx, cat: &Catalogue) -> Option<PartSlot> {
    let rhib = cat.munitions.get_index(munition.index()).is_some_and(|(_, m)| m.kind == MunitionKind::Rhib);
    let mut order: Vec<usize> = Vec::with_capacity(parts.len());
    if let Some(i) = index_of(parts, slot) { order.push(i); }
    let mut rest: Vec<usize> = (0..parts.len()).filter(|&i| Some(i) != order.first().copied()).collect();
    rest.sort_by_key(|&i| parts[i].slot);
    order.extend(rest);
    for i in order {
        let Some(def) = def_of(cat, &parts[i]) else { continue };
        if !parts[i].works() { continue; }
        let took = match (&def.function, &mut parts[i].state) {
            (PartFunction::DroneBay { drone, count, recharge_s }, PartState::DroneBay { aboard, recharge_ticks }) if cat.munition_idx(drone) == Some(munition) && *aboard < *count => {
                *aboard += 1;
                *recharge_ticks = ticks_for(nonneg(*recharge_s));
                true
            }
            (PartFunction::HeliDeck { helicopter }, PartState::HeliDeck { aboard }) if cat.munition_idx(helicopter) == Some(munition) && !*aboard => { *aboard = true; true }
            (PartFunction::RhibDavit | PartFunction::BoardingKit { .. }, PartState::Davit { rhib_aboard }) if rhib && !*rhib_aboard => { *rhib_aboard = true; true }
            _ => false,
        };
        if took { return Some(parts[i].slot); }
    }
    None
}
