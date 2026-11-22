//! Unit `sys-controls`. Turn one ship's graph output, takeovers and link state into the plain controls
//! sim-core obeys. DESIGN.md L347 (sinks), L394 (local control), L953 (player holds a role; the officer turns
//! advisory and resumes within one graph tick of release), L500 (Friend can never be engaged, even by hand).
use super::Ship;
use crate::graph::SinkWrites;
use crate::prelude::*;
use crate::tracks::Pictures;
use sim_core::parts::VesselControls;
use sim_core::vessel::Vessel;
use crate::doctrine::MountInfo;
use crate::graph::PortValue;
use crate::input::{self, AimTarget, ControlState, ManualIn};
use crate::protocol::command::ControlRole;
use crate::tracks::{SidePicture, Track};
use content::graph::{NodeKind, OfficerStage, SinkKind, WeaponClass};
use content::part::DecoyKind;
use sim_core::parts::{LaunchAim, MountOrder, PartControl, PartOrder, PartRuntime, PartState, TeamOrder};

/// Side-level effects of sinks that are not part controls.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SideEffects {
    pub stance_overrides: Vec<(TrackId, Option<Stance>)>,
    pub id_overrides: Vec<(TrackId, Option<IdRung>)>,
    pub preset: Option<DoctrinePreset>,
    pub emcon: Option<Emcon>,
    /// Guided launches ordered this tick: (launcher slot, track), so guidance can bind the spawned body.
    pub launches: Vec<(PartSlot, TrackId)>,
}

/// Build this tick's `VesselControls` for one ship, slots ascending:
/// - helm sinks to throttle and rudder (or the player's axes while the Helm role is held);
/// - mount aim and fire-permission sinks to `PartOrder::Mount` with aim error from
///   `guidance::gun::aim_error` (or the player's aim while that mount or its group is held; shots at Friend
///   tracks are refused);
/// - parts on local control use `tracks::local_track` and 3x error;
/// - launch, decoy, emission, pump, door, valve, magazine-flood, priority and team orders;
/// - parts whose command mode is `Dead` get `PartOrder::None`.
///
/// Returns the side-level effects for the caller to apply. Unit: `sys-controls`.
///
/// Details the list above leaves open:
/// - Helm: the helm sinks pass through `officers::helm::hold` once per graph tick (the only place the PID
///   integrators advance); a missing heading or speed holds the officer's last ordered value. While the
///   player holds the Helm, `input::manual_helm` drives and the integrators stay put.
/// - Gun mounts on the officers: the mount aim point is the solver's predicted impact point; when it is the
///   ballistic solver's own output for that mount, the solver's training and elevation are used as they are,
///   otherwise (a rule, pinned node or script supplied the point) the mount is laid on it with the range
///   table's elevation. Officer aim error is `gun::aim_error` with the 1 mrad laying sigma of the Pk model
///   (`gun::PLATFORM_SIGMA_B4_RAD`) scaled by the ship's `Proficiency::aim_error_mult`. Fire at a Friend
///   track, or with no target at a point within 100 m of a Friend, is refused.
/// - Held gun mounts are laid on the player's aim by `input::manual_core` over the side picture; the Sim then
///   replaces their orders with `input::manual_mount_order` (see the protocol in `crate::input`). Pending
///   shots whose mount fired since the last call are retired here, stale ones expire.
/// - Local control (L394): a gun mount whose Data link is lost follows a transient track from its director's
///   own detections (`tracks::local_track`) with the 3x error, firing when the officers' fire permission
///   holds. A launcher on local control holds: without the datalink there is no midcourse guidance.
/// - Launchers: a launch level is honoured at most once per track period (6 ticks) per launcher, and manual
///   launch requests (`ShipOrders::launches`) go first, one per ready launcher. Officer launches are
///   withheld while the player holds the launcher's mount or group.
/// - Decoys: player decoy orders fire first, then `engage::decoy_control` (called here: decoys have no sink).
/// - Emitters follow their `EmissionState` sinks; while the player holds Sensors, Active radiates every
///   emitter and Silent none, Burst keeps the officer's pattern.
/// - Damage control: team orders come from `officers::damage_control::planner`, run here at 10 Hz (teams have
///   no sink, and the stored plan omits team indices); pumps, doors and valves follow their sinks with the
///   player's manual settings winning; magazines flood when the player or the officer ordered it.
/// - One-shot orders (launch, decoy salvo) are given only when the part can act on the next physics tick;
///   a manual decoy order waits in `ShipOrders::decoys` through a reload.
/// - Stance and ID overrides that would move a Friend track are dropped (L500).
pub fn build_controls(ship: &mut Ship, vessel: &Vessel, sinks: &SinkWrites, pictures: &Pictures, ctx: &SimCtx<'_>) -> (VesselControls, SideEffects) {
    let tick = ctx.tick.0;
    let empty = SidePicture::default();
    let picture = pictures.side(ship.side).unwrap_or(&empty);
    let design = ship.design.clone();
    let mut fx = SideEffects::default();
    let n_parts = vessel.parts.len().min(MAX_PARTS);
    let mut c = VesselControls { parts: vec![PartControl::default(); n_parts], ..Default::default() };
    let part = |slot: PartSlot| vessel.parts.get(slot.index()).filter(|p| p.slot == slot);
    let part_command = ship.part_command.clone();
    let command = |slot: PartSlot| ship_part_command(&part_command, slot);
    let pose = vessel.kin.pose;

    retire_pending(&mut ship.control, &design.mounts, &vessel.parts, tick);

    // ---- Helm ----
    if !ship.control.officers_in_command && ship.control.holds(ControlRole::Helm) {
        let (t, r) = input::manual_helm(&ship.control);
        c.throttle = t;
        c.rudder = r;
    } else {
        let heading = scalar(sinks, SinkKind::HelmHeading, 0).unwrap_or(ship.stages.helm.ordered_heading_rad);
        let speed = scalar(sinks, SinkKind::HelmSpeed, 0).unwrap_or(ship.stages.helm.ordered_speed_kn);
        let hin = crate::officers::helm::HelmIn {
            pose, vel: vessel.kin.lin_vel, yaw_rate: -vessel.kin.ang_vel.y, orders: &ship.orders, turn: &design.turn, leader: None,
            threats: &[], sliders: &ship.settings.sliders, depth_ahead_m: f64::INFINITY, dt: 1.0 / sim_core::time::GRAPH_HZ as f64,
        };
        let (rudder, throttle) = crate::officers::helm::hold(heading, speed, &hin, &mut ship.stages.helm);
        c.throttle = finite_or(throttle, 0.0).clamp(-1.0, 1.0);
        c.rudder = finite_or(rudder, 0.0).clamp(-1.0, 1.0);
    }

    // ---- Weapons ----
    let picture_targets: Vec<AimTarget> = picture.tracks.iter().filter(|t| t.is_live() && t.domain != Domain::Weapon).map(|t| AimTarget {
        entity: t.truth, pos: t.predict(tick), vel: if t.vel.is_finite() { t.vel } else { DVec3::ZERO }, half_len_m: 30.0, friend: input::effective_stance(t) == Stance::Friend,
    }).collect();
    for m in &design.mounts {
        let i = m.slot.index();
        if i >= n_parts { continue; }
        let p = part(m.slot);
        let cmd = command(m.slot);
        if matches!(cmd, crate::datalink::PartCommand::Dead) || p.is_some_and(|p| !p.works()) { continue; }
        let local = matches!(cmd, crate::datalink::PartCommand::Local { .. });
        let held = input::holding_role(&ship.control, m);
        match m.weapon {
            WeaponClass::Decoy => {}
            WeaponClass::Gun | WeaponClass::PointDefenceGun => {
                let gun = input::gun_facts(m, &design.range_tables, ctx.catalogue);
                if held.is_some() {
                    let out = input::manual_core(&ManualIn { mount: m, part: p, gun, pose, own_vel: vessel.kin.lin_vel, control: &ship.control, picture, targets: &picture_targets, tick, tuning: ctx.tuning });
                    c.parts[i].order = PartOrder::Mount(out.order);
                    continue;
                }
                let order = if local {
                    local_gun_order(ship, vessel, m, p, &gun, sinks, picture, ctx)
                } else {
                    officer_gun_order(ship, vessel, m, p, &gun, sinks, picture, ctx)
                };
                if let Some(o) = order { c.parts[i].order = PartOrder::Mount(o); }
            }
            _ => {
                if let Some((aim, track)) = launch_order(ship, m, p, sinks, picture, held.is_some(), local, tick) {
                    c.parts[i].order = PartOrder::Launch(aim);
                    if !track.is_none() { fx.launches.push((m.slot, track)); }
                    note_launch(&mut ship.control, m.slot, tick);
                }
            }
        }
    }
    // Manual launch requests for launchers that are gone stop waiting.
    ship.orders.launches.retain(|r| design.mounts.iter().any(|m| m.slot == r.slot) && part(r.slot).is_none_or(|p| p.works() && !launcher_spent(p)));

    // ---- Decoys ----
    // A manual decoy order waits for its launcher's reload; one that can never fire is dropped.
    let manual_decoys = core::mem::take(&mut ship.orders.decoys);
    let mut decoy_set: BTreeSet<PartSlot> = BTreeSet::new();
    for slot in manual_decoys {
        let i = slot.index();
        if i >= n_parts || !design.decoys.iter().any(|d| d.0 == slot) { continue; }
        if matches!(command(slot), crate::datalink::PartCommand::Dead) || part(slot).is_some_and(|p| !p.works()) || decoy_spent(part(slot)) { continue; }
        decoy_set.insert(slot);
        if decoy_ready(part(slot)) { c.parts[i].order = PartOrder::FireDecoy; } else { ship.orders.decoys.push(slot); }
    }
    if !design.decoys.is_empty() {
        let own = crate::engage::OwnState { vessel: vessel.id, side: ship.side, pose, vel: vessel.kin.lin_vel, ang_vel: vessel.kin.ang_vel, heel_rad: heel_of(&pose) };
        let live: Vec<(PartSlot, DecoyKind)> = design.decoys.iter().copied().filter(|(s, _)| s.index() < n_parts && !matches!(command(*s), crate::datalink::PartCommand::Dead) && part(*s).is_none_or(|p| p.works())).collect();
        for (slot, order) in crate::engage::decoy_control(&own, &picture.tracks, &live, &mut ship.stages.engage, tick, ctx.tuning) {
            if decoy_set.contains(&slot) { continue; }
            let i = slot.index();
            if i >= n_parts { continue; }
            c.parts[i].order = match order { crate::engage::DecoyOrder::Fire => PartOrder::FireDecoy, crate::engage::DecoyOrder::StreamOn => PartOrder::StreamDecoy(true), crate::engage::DecoyOrder::StreamOff => PartOrder::StreamDecoy(false) };
        }
    }

    // ---- Emitters ----
    let sensors_held = !ship.control.officers_in_command && ship.control.holds(ControlRole::Sensors);
    for e in &design.emitters {
        let i = e.slot.index();
        if i >= n_parts || matches!(command(e.slot), crate::datalink::PartCommand::Dead) { continue; }
        let officer = sinks.get(SinkKind::EmissionState(e.slot)).and_then(|w| w.values.first()).and_then(as_bool);
        let radiate = match (sensors_held, ship.settings.emcon) { (true, Emcon::Active) => Some(true), (true, Emcon::Silent) => Some(false), _ => officer };
        let Some(radiating) = radiate else { continue };
        let train = match part(e.slot).map(|p| &p.state) { Some(PartState::Sensor { train_rad, .. }) if train_rad.is_finite() => *train_rad, _ => 0.0 };
        c.parts[i].order = PartOrder::Emit { radiating, train_rad: train };
    }

    // ---- Power priority ----
    for (slot, pr) in &ship.settings.power_priority { if let Some(pc) = c.parts.get_mut(slot.index()) { if (1..=5).contains(pr) { pc.priority = *pr; } } }
    for w in sinks.writes.iter() {
        if let SinkKind::PowerPriority(slot) = w.sink {
            // Outside 1..=5 is no command: the settings override or the part's default stands.
            if let (Some(pc), Some(v)) = (c.parts.get_mut(slot.index()), w.values.first().and_then(as_scalar)) { let r = libm::round(v); if (1.0..=5.0).contains(&r) { pc.priority = r as u8; } }
        }
    }

    // ---- Damage control ----
    // Team orders have no sink and the planner's stored plan drops team indices, so the planner runs here
    // at its 10 Hz rate; between runs the teams keep the orders they were given.
    let teams = if ctx.tick.is_due(sim_core::time::TRACK_HZ, 0) || vessel.controls.teams.len() != vessel.crew.teams.len() {
        crate::officers::damage_control::planner(vessel, &ship.settings.repair_order, &mut ship.stages.dc, ctx.catalogue, ctx.tuning).teams
    } else {
        vessel.controls.teams.clone()
    };
    let dc = &ship.stages.dc;
    apply_dc(&mut c, sinks, dc, &design.pumps, &design.doors, &|s| matches!(command(s), crate::datalink::PartCommand::Dead));
    c.teams = teams.into_iter().take(MAX_TEAMS).collect();

    // ---- Depth (Act 3 AI submarines) ----
    if vessel.submersible { c.depth_m = ship.orders.depth_m.filter(|d| d.is_finite() && *d > 0.0).map(|d| d.min(1_000.0)); }

    // ---- Side-level effects ----
    side_effects(sinks, picture, &mut fx);
    (c, fx)
}

/// Most parts a vessel may have (blueprint slot numbers are u16); guards the control vector against a corrupt
/// part list.
const MAX_PARTS: usize = 1 << 16;
/// A launch level is honoured at most once per this many ticks per launcher (one track period, 10 Hz).
const LAUNCH_MIN_TICKS: u64 = 6;
/// Fire with no target track is refused when the aim point lies this close to a Friend track, metres.
const FRIEND_CLEAR_M: f64 = 100.0;
/// Most damage-control teams a vessel orders.
const MAX_TEAMS: usize = 32;

fn finite_or(x: f64, d: f64) -> f64 { if x.is_finite() { x } else { d } }
fn as_bool(v: &PortValue) -> Option<bool> { match v { PortValue::Bool(b) => Some(*b), PortValue::Scalar(x) if x.is_finite() => Some(*x != 0.0), _ => None } }
fn as_scalar(v: &PortValue) -> Option<f64> { match v { PortValue::Scalar(x) if x.is_finite() => Some(*x), PortValue::Bool(b) => Some(if *b { 1.0 } else { 0.0 }), _ => None } }
fn as_track(v: Option<&PortValue>) -> TrackId { match v { Some(PortValue::Track(t)) => *t, _ => TrackId::NONE } }
fn as_vec(v: Option<&PortValue>) -> Option<DVec3> { match v { Some(PortValue::Vec3(x)) if x.is_finite() => Some(*x), _ => None } }
fn scalar(sinks: &SinkWrites, sink: SinkKind, port: usize) -> Option<f64> { sinks.get(sink).and_then(|w| w.values.get(port)).and_then(as_scalar) }
fn ship_part_command(cmds: &[crate::datalink::PartCommand], slot: PartSlot) -> crate::datalink::PartCommand { cmds.get(slot.index()).copied().unwrap_or_default() }

/// Signed heel from a pose: positive with the starboard side down.
fn heel_of(pose: &Pose) -> f64 {
    let stbd = pose.rot * DVec3::Z;
    if stbd.is_finite() { libm::asin((-stbd.y).clamp(-1.0, 1.0)) } else { 0.0 }
}

fn live(picture: &SidePicture, id: TrackId) -> Option<&Track> { if id.is_none() { None } else { picture.get(id).filter(|t| t.is_live()) } }
fn is_friend(t: &Track) -> bool { input::effective_stance(t) == Stance::Friend }
fn near_friend(picture: &SidePicture, p: DVec3, tick: u64) -> bool {
    picture.tracks.iter().any(|t| t.is_live() && is_friend(t) && (t.predict(tick) - p).length() <= FRIEND_CLEAR_M)
}

/// Retire pending shots whose mount fired since the last call (one per firing mount, oldest first), drop
/// shots that went stale or whose role was released, and remember every gun mount's fire counter.
fn retire_pending(control: &mut ControlState, mounts: &[MountInfo], parts: &[PartRuntime], tick: u64) {
    let fired = |slot: PartSlot| -> Option<u32> { match parts.get(slot.index()).filter(|p| p.slot == slot).map(|p| &p.state) { Some(PartState::Mount(ms)) => Some(ms.fired), _ => None } };
    if control.officers_in_command { control.pending.clear(); }
    let mut seen: Vec<(PartSlot, u32)> = Vec::with_capacity(control.fired_seen.len());
    for m in mounts.iter().filter(|m| matches!(m.weapon, WeaponClass::Gun | WeaponClass::PointDefenceGun)) {
        let Some(now) = fired(m.slot) else { continue };
        let before = control.fired_seen.iter().find(|f| f.0 == m.slot).map(|f| f.1);
        if before.is_some_and(|b| now > b) {
            let covers = |r: ControlRole| match r { ControlRole::Mount(s) => s == m.slot, ControlRole::WeaponGroup(g) => g == m.group, _ => false };
            if let Some(k) = control.pending.iter().position(|p| covers(p.role)) { control.pending.remove(k); }
        }
        seen.push((m.slot, now));
    }
    control.fired_seen = seen;
    let held = control.held.clone();
    let still_held = |r: ControlRole| held.contains(&r) || match r {
        ControlRole::Mount(s) => mounts.iter().any(|m| m.slot == s && held.contains(&ControlRole::WeaponGroup(m.group))),
        _ => false,
    };
    control.pending.retain(|p| tick.saturating_sub(p.issued_tick) <= input::PENDING_TTL_TICKS && still_held(p.role));
}

/// The ballistic solver node bound to a mount, if its aim point is the value the sink received: then its
/// training and elevation are the exact solution.
fn solver_angles(ship: &Ship, slot: PartSlot, aim: DVec3) -> Option<(f64, f64)> {
    let vals = &ship.graph_state.values;
    ship.graph.nodes.iter()
        .filter(|n| n.kind == NodeKind::Stage(OfficerStage::BallisticSolver) && n.part == Some(slot))
        .find_map(|n| {
            let b = n.out_base as usize;
            match (vals.get(b), vals.get(b + 2), vals.get(b + 3)) {
                (Some(PortValue::Vec3(p)), Some(t), Some(e)) if *p == aim => Some((as_scalar(t)?, as_scalar(e)?)),
                _ => None,
            }
        })
}

fn officer_error(ship: &Ship, vessel: &Vessel, slot: PartSlot, local: bool, ctx: &SimCtx<'_>) -> [f64; 2] {
    crate::guidance::gun::aim_error(vessel.id, slot, crate::guidance::gun::PLATFORM_SIGMA_B4_RAD, ship.proficiency.aim_error_mult, local, ctx)
}

fn officer_gun_order(ship: &Ship, vessel: &Vessel, m: &MountInfo, p: Option<&PartRuntime>, gun: &input::GunFacts<'_>, sinks: &SinkWrites, picture: &SidePicture, ctx: &SimCtx<'_>) -> Option<MountOrder> {
    let aim = as_vec(sinks.get(SinkKind::MountAimPoint(m.slot)).and_then(|w| w.values.first()))?;
    let pose = vessel.kin.pose;
    let (train, elev) = match solver_angles(ship, m.slot, aim) {
        Some((t, e)) => input::clamp_to_limits(t, e, &m.limits),
        None => {
            let muzzle = pose.to_world(if m.pos_local.is_finite() { m.pos_local } else { DVec3::ZERO });
            let dir = input::lay_toward(gun, m.muzzle_mps, muzzle, aim)?;
            input::angles_for(&pose, m, dir)
        }
    };
    let perm = sinks.get(SinkKind::MountFirePermission(m.slot));
    let wants = perm.and_then(|w| w.values.first()).and_then(as_bool).unwrap_or(false);
    let target = as_track(perm.and_then(|w| w.values.get(1)));
    let tick = ctx.tick.0;
    let allowed = match live(picture, target) { Some(t) => !is_friend(t), None => target.is_none() && !near_friend(picture, aim, tick) };
    let fire = wants && allowed;
    Some(MountOrder { train_rad: train, elevation_rad: elev, fire, max_rounds: 0, aim_error_rad: officer_error(ship, vessel, m.slot, false, ctx), laid_rot: Some(pose.rot) })
}

/// Local control of a gun (L394): its director's own track, 3x error, the officers' fire permission.
fn local_gun_order(ship: &mut Ship, vessel: &Vessel, m: &MountInfo, p: Option<&PartRuntime>, gun: &input::GunFacts<'_>, sinks: &SinkWrites, picture: &SidePicture, ctx: &SimCtx<'_>) -> Option<MountOrder> {
    let slot = m.slot;
    let Some(director) = m.director else { ship.control.local_tracks.retain(|t| t.0 != slot); return None };
    let sensor = SensorRef::part(vessel.id, director);
    let prev = ship.control.local_tracks.iter().find(|t| t.0 == slot).map(|t| t.1.clone());
    let track = crate::tracks::local_track(&ship.detections, sensor, prev.as_ref(), ctx);
    ship.control.local_tracks.retain(|t| t.0 != slot);
    let track = track?;
    ship.control.local_tracks.push((slot, track.clone()));
    ship.control.local_tracks.sort_by_key(|t| t.0);
    let tick = ctx.tick.0;
    let pose = vessel.kin.pose;
    let muzzle = pose.to_world(if m.pos_local.is_finite() { m.pos_local } else { DVec3::ZERO });
    let (cur_t, cur_e) = input::current_angles(p);
    let (tr, er) = gun.def.map(|d| (d.train_rate_dps.to_radians(), d.elevation_rate_dps.to_radians())).unwrap_or((0.0, 0.0));
    let g = &ctx.tuning.guidance;
    let sol = crate::guidance::gun::solve(&crate::guidance::gun::GunSolveIn {
        muzzle, own_vel: vessel.kin.lin_vel, pose, zero_rad: m.zero_rad, limits: m.limits,
        target_pos: track.gun_target(tick).0 + if track.domain == Domain::Surface { DVec3::Y * crate::guidance::gun::wave_lay_m(track.rcs_m2 as f64, ctx.weather.sea_state) } else { DVec3::ZERO },
        target_vel: track.gun_target(tick).1,
        muzzle_mps: m.muzzle_mps, drag_k: gun.drag_k, train_rad: cur_t, elevation_rad: cur_e, train_rate: tr, elevation_rate: er,
        iteration_cap: g.gun_iteration_cap.max(1), converge_m: g.gun_converge_m,
    });
    let wants = sinks.get(SinkKind::MountFirePermission(slot)).and_then(|w| w.values.first()).and_then(as_bool).unwrap_or(false);
    let friend = picture.track_of(track.truth).is_some_and(is_friend) || track.truth == vessel.id;
    let fire = wants && !friend && sol.in_range;
    Some(MountOrder { train_rad: sol.train_rad, elevation_rad: sol.elevation_rad, fire, max_rounds: 0, aim_error_rad: officer_error(ship, vessel, slot, true, ctx), laid_rot: Some(pose.rot) })
}

/// The launcher can launch on the next physics tick. sim-core keeps a waiting launch pending, but these
/// controls are rewritten every graph tick, so a launch is only ordered when it can act at once.
fn launcher_ready(p: Option<&PartRuntime>) -> bool {
    match p.map(|p| &p.state) { Some(PartState::Launcher { ready, interval_ticks, .. }) => *ready > 0 && *interval_ticks == 0, Some(PartState::MineRail { mines, .. }) => *mines > 0, None => true, _ => false }
}
fn decoy_ready(p: Option<&PartRuntime>) -> bool {
    match p.map(|p| &p.state) { Some(PartState::Decoy { rounds, reload_ticks, .. }) => *rounds > 0 && *reload_ticks == 0, None => true, _ => false }
}
fn decoy_spent(p: Option<&PartRuntime>) -> bool { matches!(p.map(|p| &p.state), Some(PartState::Decoy { rounds: 0, .. })) }
fn launcher_spent(p: &PartRuntime) -> bool { matches!(p.state, PartState::Launcher { ready: 0, reload_ticks: 0, .. }) }

fn launch_due(control: &ControlState, slot: PartSlot, tick: u64) -> bool {
    control.last_launch.iter().find(|l| l.0 == slot).is_none_or(|l| tick.saturating_sub(l.1) >= LAUNCH_MIN_TICKS || tick < l.1)
}
fn note_launch(control: &mut ControlState, slot: PartSlot, tick: u64) {
    match control.last_launch.binary_search_by_key(&slot, |l| l.0) { Ok(i) => control.last_launch[i].1 = tick, Err(i) => control.last_launch.insert(i, (slot, tick)) }
}

/// Initial aim of a launch at a track's estimate (or an explicit point) by weapon.
fn aim_for(weapon: WeaponClass, point: DVec3, depth: Option<f64>) -> LaunchAim {
    match weapon {
        WeaponClass::DepthCharge => LaunchAim::DepthM(depth.unwrap_or_else(|| (-point.y).max(DEFAULT_CHARGE_DEPTH_M)).clamp(1.0, 1_000.0)),
        _ => LaunchAim::Point(point),
    }
}
/// Depth charges set to this depth when the target's depth is unknown, metres.
const DEFAULT_CHARGE_DEPTH_M: f64 = 30.0;

/// One launch this tick for a launcher: a manual request first, then the officers' launch sink.
fn launch_order(ship: &mut Ship, m: &MountInfo, p: Option<&PartRuntime>, sinks: &SinkWrites, picture: &SidePicture, held: bool, local: bool, tick: u64) -> Option<(LaunchAim, TrackId)> {
    if !launcher_ready(p) || !launch_due(&ship.control, m.slot, tick) { return None; }
    if let Some(k) = ship.orders.launches.iter().position(|r| r.slot == m.slot) {
        let r = ship.orders.launches[k];
        let aim = if !r.track.is_none() {
            match live(picture, r.track) {
                Some(t) if !is_friend(t) => Some(aim_for(m.weapon, t.predict(tick), r.depth_m)),
                _ => None,
            }
        } else if let Some(b) = r.bearing_rad.filter(|b| b.is_finite()) {
            Some(LaunchAim::Bearing { heading_rad: b })
        } else {
            r.depth_m.filter(|d| d.is_finite()).map(|d| LaunchAim::DepthM(d.clamp(1.0, 1_000.0)))
        };
        ship.orders.launches.remove(k);
        if let Some(a) = aim { return Some((a, r.track)); }
        return None;
    }
    if held || local { return None; }
    let w = sinks.get(SinkKind::MissileLaunch(m.slot))?;
    if !w.values.first().and_then(as_bool).unwrap_or(false) { return None; }
    if w.values.get(1).and_then(as_scalar).is_some_and(|n| n < 1.0) { return None; }
    let tw = sinks.get(SinkKind::MissileTarget(m.slot));
    let track = as_track(tw.and_then(|w| w.values.first()));
    let t = live(picture, track)?;
    if is_friend(t) { return None; }
    let point = as_vec(tw.and_then(|w| w.values.get(1))).unwrap_or_else(|| t.predict(tick));
    Some((aim_for(m.weapon, point, None), track))
}

/// Pumps, doors, valves and magazine floods: sinks first, the player's manual settings on top.
fn apply_dc(c: &mut VesselControls, sinks: &SinkWrites, dc: &crate::officers::damage_control::DcState, pumps: &[PartSlot], doors: &[PartSlot], dead: &dyn Fn(PartSlot) -> bool) {
    let n = c.parts.len();
    let mut valves: BTreeSet<CompartmentId> = BTreeSet::new();
    for w in &sinks.writes {
        match w.sink {
            SinkKind::Pump(slot) if pumps.contains(&slot) && slot.index() < n && !dead(slot) => {
                if let Some(on) = w.values.first().and_then(as_bool) {
                    let target = w.values.get(1).and_then(as_scalar).filter(|x| *x >= 0.0 && *x < u16::MAX as f64).map(|x| CompartmentId(x as u16));
                    c.parts[slot.index()].order = PartOrder::Pump { running: on, target };
                }
            }
            SinkKind::Door(slot) if doors.contains(&slot) && slot.index() < n && !dead(slot) => {
                if let Some(closed) = w.values.first().and_then(as_bool) { c.parts[slot.index()].order = PartOrder::Door { closed }; }
            }
            SinkKind::FloodValve(comp) => {
                if let Some(open) = w.values.first().and_then(as_bool) { if open { valves.insert(comp); } else { valves.remove(&comp); } }
            }
            _ => {}
        }
    }
    for (slot, running, target) in &dc.manual_pumps { if slot.index() < n && !dead(*slot) { c.parts[slot.index()].order = PartOrder::Pump { running: *running, target: *target }; } }
    for (slot, closed) in &dc.manual_doors { if slot.index() < n && !dead(*slot) { c.parts[slot.index()].order = PartOrder::Door { closed: *closed }; } }
    for (comp, open) in &dc.manual_valves { if *open { valves.insert(*comp); } else { valves.remove(comp); } }
    c.open_valves = valves.into_iter().collect();
    let mut floods: BTreeSet<PartSlot> = dc.manual_magazine_floods.iter().copied().collect();
    floods.extend(dc.auto_flood.iter().copied());
    c.flood_magazines = floods.into_iter().filter(|s| s.index() < n).collect();
}

/// Stance and ID overrides (never moving a Friend), preset and EMCON switches.
fn side_effects(sinks: &SinkWrites, picture: &SidePicture, fx: &mut SideEffects) {
    if let Some(w) = sinks.get(SinkKind::TrackStanceOverride) {
        let track = as_track(w.values.first());
        if let (Some(t), Some(PortValue::Stance(s))) = (live(picture, track), w.values.get(1)) {
            if t.stance != Stance::Friend || *s == Stance::Friend { fx.stance_overrides.push((track, Some(*s))); }
        }
    }
    if let Some(w) = sinks.get(SinkKind::TrackIdOverride) {
        let track = as_track(w.values.first());
        if let (Some(_), Some(PortValue::Rung(r))) = (live(picture, track), w.values.get(1)) { fx.id_overrides.push((track, Some(*r))); }
    }
    if let Some(PortValue::Preset(p)) = sinks.get(SinkKind::ShipPreset).and_then(|w| w.values.first()) { fx.preset = Some(*p); }
    if let Some(PortValue::Emcon(e)) = sinks.get(SinkKind::ShipEmcon).and_then(|w| w.values.first()) { fx.emcon = Some(*e); }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::{SinkWrite, WriteOrigin};
    use crate::input::tests::{picture, ship, track, vessel, Env, OWN};
    use crate::input::PendingShot;
    use crate::protocol::command::ShotDesignation;
    use content::enums::OfficerRole;
    use sim_core::parts::MountState;

    fn w(sink: SinkKind, values: Vec<PortValue>) -> SinkWrite { SinkWrite { sink, values, origin: WriteOrigin::Officer(OfficerRole::Gunnery) } }
    fn pics(p: SidePicture) -> Pictures { Pictures { sides: vec![p] } }

    #[test]
    fn helm_sinks_drive_the_pid_and_the_player_overrides() {
        let env = Env::new();
        let mut s = ship();
        let v = vessel();
        let sinks = SinkWrites { writes: vec![w(SinkKind::HelmHeading, vec![PortValue::Scalar(1.0)]), w(SinkKind::HelmSpeed, vec![PortValue::Scalar(20.0)])] };
        let (c, _) = build_controls(&mut s, &v, &sinks, &pics(picture(vec![])), &env.ctx(10));
        assert!(c.throttle > 0.5 && c.throttle <= 1.0, "{}", c.throttle);
        assert!(c.rudder != 0.0 && c.rudder.abs() <= 1.0);
        assert!((s.stages.helm.ordered_speed_kn - 20.0).abs() < 1e-9);
        s.control.held = vec![ControlRole::Helm];
        s.control.input.throttle = -0.25;
        s.control.input.rudder = 3.0;
        let integ = s.stages.helm.speed_integral;
        let (c, _) = build_controls(&mut s, &v, &sinks, &pics(picture(vec![])), &env.ctx(12));
        assert_eq!((c.throttle, c.rudder), (-0.25, 1.0));
        assert_eq!(s.stages.helm.speed_integral, integ, "the officer is advisory while the Helm is held");
    }

    #[test]
    fn officer_fire_never_reaches_a_friend() {
        let env = Env::new();
        let mut s = ship();
        let v = vessel();
        let p = picture(vec![track(3, DVec3::new(3_000.0, 0.0, 0.0), Stance::Hostile, Domain::Surface), track(4, DVec3::new(2_000.0, 0.0, 0.0), Stance::Friend, Domain::Surface)]);
        let at = |t: u32, aim: DVec3| SinkWrites { writes: vec![w(SinkKind::MountAimPoint(PartSlot(1)), vec![PortValue::Vec3(aim)]), w(SinkKind::MountFirePermission(PartSlot(1)), vec![PortValue::Bool(true), PortValue::Track(TrackId(t))])] };
        let order = |c: &VesselControls| match c.parts[1].order { PartOrder::Mount(m) => m, ref o => panic!("{o:?}") };
        let (c, _) = build_controls(&mut s, &v, &at(3, DVec3::new(3_000.0, 0.0, 0.0)), &pics(p.clone()), &env.ctx(10));
        let m = order(&c);
        assert!(m.fire && m.max_rounds == 0);
        assert!(m.train_rad.abs() < 1e-6, "dead ahead");
        assert!(m.aim_error_rad.iter().all(|e| e.is_finite() && e.abs() < 0.01));
        let (c, _) = build_controls(&mut s, &v, &at(4, DVec3::new(2_000.0, 0.0, 0.0)), &pics(p.clone()), &env.ctx(12));
        assert!(!order(&c).fire);
        // No target, aimed at a point beside a Friend: refused.
        let (c, _) = build_controls(&mut s, &v, &at(0, DVec3::new(2_050.0, 0.0, 0.0)), &pics(p.clone()), &env.ctx(14));
        assert!(!order(&c).fire);
        // A dead part gets no order at all.
        s.part_command[1] = crate::datalink::PartCommand::Dead;
        let (c, _) = build_controls(&mut s, &v, &at(3, DVec3::new(3_000.0, 0.0, 0.0)), &pics(p), &env.ctx(16));
        assert_eq!(c.parts[1].order, PartOrder::None);
    }

    #[test]
    fn launches_are_rate_limited_and_bound_for_guidance() {
        let env = Env::new();
        let mut s = ship();
        let v = vessel();
        let p = picture(vec![track(3, DVec3::new(20_000.0, 0.0, 0.0), Stance::Hostile, Domain::Surface)]);
        let sinks = SinkWrites { writes: vec![w(SinkKind::MissileLaunch(PartSlot(2)), vec![PortValue::Bool(true), PortValue::Scalar(1.0)]), w(SinkKind::MissileTarget(PartSlot(2)), vec![PortValue::Track(TrackId(3)), PortValue::None])] };
        let (c, fx) = build_controls(&mut s, &v, &sinks, &pics(p.clone()), &env.ctx(100));
        assert_eq!(c.parts[2].order, PartOrder::Launch(LaunchAim::Point(DVec3::new(20_000.0, 0.0, 0.0))));
        assert_eq!(fx.launches, vec![(PartSlot(2), TrackId(3))]);
        let (c, fx) = build_controls(&mut s, &v, &sinks, &pics(p.clone()), &env.ctx(102));
        assert_eq!(c.parts[2].order, PartOrder::None, "a held launch level is not a second launch");
        assert!(fx.launches.is_empty());
        // A manual request goes first once the launcher may launch again; a hand-held group stops the officers.
        s.orders.launches.push(crate::ship::LaunchRequest { slot: PartSlot(2), track: TrackId::NONE, bearing_rad: Some(1.0), depth_m: None });
        let (c, fx) = build_controls(&mut s, &v, &sinks, &pics(p.clone()), &env.ctx(106));
        assert_eq!(c.parts[2].order, PartOrder::Launch(LaunchAim::Bearing { heading_rad: 1.0 }));
        assert!(fx.launches.is_empty() && s.orders.launches.is_empty());
        s.control.held = vec![ControlRole::WeaponGroup(1)];
        let (c, _) = build_controls(&mut s, &v, &sinks, &pics(p), &env.ctx(120));
        assert_eq!(c.parts[2].order, PartOrder::None);
    }

    #[test]
    fn damage_control_decoys_and_overrides() {
        let env = Env::new();
        let mut s = ship();
        let v = vessel();
        let p = picture(vec![track(4, DVec3::ZERO, Stance::Friend, Domain::Surface), track(5, DVec3::ZERO, Stance::Suspect, Domain::Surface)]);
        s.orders.decoys.push(PartSlot(3));
        s.stages.dc.manual_doors.push((PartSlot(5), true));
        s.stages.dc.manual_valves.push((CompartmentId(2), false));
        s.stages.dc.manual_magazine_floods.push(PartSlot(1));
        let sinks = SinkWrites { writes: vec![
            w(SinkKind::Pump(PartSlot(4)), vec![PortValue::Bool(true), PortValue::Scalar(3.0)]),
            w(SinkKind::Door(PartSlot(5)), vec![PortValue::Bool(false)]),
            w(SinkKind::FloodValve(CompartmentId(2)), vec![PortValue::Bool(true)]),
            w(SinkKind::FloodValve(CompartmentId(7)), vec![PortValue::Bool(true)]),
            w(SinkKind::PowerPriority(PartSlot(1)), vec![PortValue::Scalar(9.0)]),
            w(SinkKind::PowerPriority(PartSlot(2)), vec![PortValue::Scalar(2.4)]),
            w(SinkKind::TrackStanceOverride, vec![PortValue::Track(TrackId(4)), PortValue::Stance(Stance::Hostile)]),
            w(SinkKind::ShipEmcon, vec![PortValue::Emcon(Emcon::Silent)]),
        ] };
        let (c, fx) = build_controls(&mut s, &v, &sinks, &pics(p.clone()), &env.ctx(10));
        assert_eq!(c.parts[3].order, PartOrder::FireDecoy);
        assert!(s.orders.decoys.is_empty());
        assert_eq!(c.parts[4].order, PartOrder::Pump { running: true, target: Some(CompartmentId(3)) });
        assert_eq!(c.parts[5].order, PartOrder::Door { closed: true }, "the player's door setting wins");
        assert_eq!(c.open_valves, vec![CompartmentId(7)]);
        assert_eq!(c.flood_magazines, vec![PartSlot(1)]);
        assert_eq!(c.teams.len(), v.crew.teams.len());
        assert_eq!((c.parts[1].priority, c.parts[2].priority), (0, 2), "out-of-range priority is no command");
        // A launcher still reloading keeps the player's decoy order waiting.
        let mut v2 = vessel();
        v2.parts[3].state = PartState::Decoy { rounds: 5, reload_ticks: 40, streamed: false };
        s.orders.decoys.push(PartSlot(3));
        let (c, _) = build_controls(&mut s, &v2, &SinkWrites::default(), &pics(picture(vec![])), &env.ctx(11));
        assert_eq!(c.parts[3].order, PartOrder::None);
        assert_eq!(s.orders.decoys, vec![PartSlot(3)]);
        s.orders.decoys.clear();
        assert!(fx.stance_overrides.is_empty(), "a Friend keeps its stance");
        assert_eq!(fx.emcon, Some(Emcon::Silent));
        let sinks = SinkWrites { writes: vec![w(SinkKind::TrackStanceOverride, vec![PortValue::Track(TrackId(5)), PortValue::Stance(Stance::Hostile)])] };
        let (_, fx) = build_controls(&mut s, &v, &sinks, &pics(p), &env.ctx(12));
        assert_eq!(fx.stance_overrides, vec![(TrackId(5), Some(Stance::Hostile))]);
    }

    #[test]
    fn pending_shots_retire_when_their_mount_fires() {
        let env = Env::new();
        let mut s = ship();
        let mut v = vessel();
        s.control.held = vec![ControlRole::WeaponGroup(0)];
        let shot = |tick: u64| PendingShot { role: ControlRole::Mount(PartSlot(1)), shot: ShotDesignation::Direction { dir_local: DVec3::X }, client_render_tick: tick, rtt_ms: 30, issued_tick: tick };
        s.control.pending = vec![shot(10), shot(11)];
        let p = pics(picture(vec![]));
        let (c, _) = build_controls(&mut s, &v, &SinkWrites::default(), &p, &env.ctx(12));
        assert!(matches!(c.parts[1].order, PartOrder::Mount(MountOrder { fire: true, max_rounds: 1, .. })), "a held mount is laid and fires its pending shot");
        assert_eq!(s.control.pending.len(), 2);
        v.parts[1].state = PartState::Mount(MountState { ready_rounds: 9, fired: 1, on_target: true, cooldown_ticks: 30, ..Default::default() });
        build_controls(&mut s, &v, &SinkWrites::default(), &p, &env.ctx(14));
        assert_eq!(s.control.pending, vec![shot(11)], "one shot retired per firing");
        build_controls(&mut s, &v, &SinkWrites::default(), &p, &env.ctx(11 + crate::input::PENDING_TTL_TICKS + 1));
        assert!(s.control.pending.is_empty(), "stale shots expire");
        // Pending shots of a released role are dropped.
        s.control.pending = vec![shot(500)];
        s.control.held.clear();
        build_controls(&mut s, &v, &SinkWrites::default(), &p, &env.ctx(501));
        assert!(s.control.pending.is_empty());
        let _ = OWN;
    }
}
