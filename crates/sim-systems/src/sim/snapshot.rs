//! Unit `sys-snapshot`. Building the per-viewer snapshot and static entity introductions.
//! DESIGN.md L529, L920-926, L1423 (distant track rates), L640 (compartment water at 2 Hz), L416 (0.5 ms
//! share for replication and snapshot encoding; this unit's part is `SystemsWork::snapshot_items`).
//!
//! Notes on what a viewer gets:
//! - Vessels, bodies and decoys come from the viewer side's drawable set; ids a side may not draw (a decoy's
//!   unseen owner) are scrubbed. `Viewer::Omniscient` draws everything and reads the plot of the lowest
//!   player-faction side (host views, bots, golden captures).
//! - Relevance for tracks uses the track estimate, never truth, so update rates leak nothing.
//! - Slow lists (parts, compartments) are filled every snapshot; net sends them on change at most at 2 Hz.
//! - The expert gun solution is the fixed-point lead iteration (L518: under 0.25 m, at most 6 iterations)
//!   run against the mount's design-time range table and the engaged track's estimate. It reads the same
//!   track and table the officer uses, without re-stepping the ballistic model, to fit the snapshot budget.
//! - An entity intro carries the full damage state at the time of the reveal (one change per damaged cell,
//!   ascending cell), so a side that re-acquires a hull sees damage dealt while it was hidden. In PvP the
//!   other team's intro carries the exterior-only blueprint hash and only cells it may know.
use super::Sim;
use crate::prelude::*;
use crate::protocol::command::ControlRole;
use crate::protocol::config::{EntityIntro, Faction};
use crate::protocol::event::{Audience, Lane, OutEvent, SimEvent};
use crate::protocol::snapshot::*;
use crate::tracks::Track;
use content::cell::{CellChange, BUCKET_DESTROYED};
use content::graph::WeaponClass;
use content::part::weapons::MunitionKind;
use content::part::{DecoyKind, PartFunction};
use sim_core::parts::PartState;
use sim_core::time::KN;
use sim_core::vessel::{Fidelity, Vessel, VesselStatus};

/// Beyond this range from the viewer's hull an item is distant (L1423).
pub const DISTANT_M: f64 = sim_core::time::DISTANT_M;
/// Distant tracks closing faster than 30 kn update at 2 Hz (L1423).
pub const FAST_CLOSING_MPS: f64 = 30.0 * KN;
/// "Fired in the last 10 s" (L436).
const FIRED_RECENTLY_TICKS: u64 = 600;
const RAD2DEG: f64 = 180.0 / core::f64::consts::PI;

fn u8_frac(x: f64) -> u8 { (x.clamp(0.0, 1.0) * 255.0).round() as u8 }

/// Who the viewer is: (side, own hull).
fn resolve_viewer(sim: &Sim, viewer: Viewer) -> Option<(Option<SideId>, Option<EntityId>)> {
    match viewer {
        Viewer::Player(p) => {
            let ps = sim.players.iter().find(|x| x.slot == p)?;
            Some((Some(ps.side), ps.vessel.filter(|v| sim.world.vessel(*v).is_some())))
        }
        Viewer::Side(s) => Some((Some(s), None)),
        Viewer::Omniscient => Some((None, None)),
    }
}

/// Side whose plot the omniscient view shows: the lowest player-faction side, else side 0.
fn omniscient_plot_side(sim: &Sim) -> SideId {
    sim.config.sides.iter().filter(|s| s.faction == Faction::Players).map(|s| s.id).min().unwrap_or(SideId(0))
}

/// Allied mask of a side straight from the config (valid before the first interest update).
fn allied_of(sim: &Sim, side: SideId) -> SideMask { super::events::allied_masks(&sim.config).get(side.index()).copied().unwrap_or(SideMask::only(side)) }

fn is_weapon(kind: MunitionKind) -> bool { matches!(kind, MunitionKind::AntiShipMissile | MunitionKind::Sam | MunitionKind::PdMissile | MunitionKind::Torpedo | MunitionKind::DepthCharge) }

/// Build a snapshot for one viewer from the Sim's state as of the last completed tick. Vessels, bodies and
/// decoys come from `InterestState::drawable` of the viewer's side (everything for `Viewer::Omniscient`);
/// tracks from the viewer side's picture; `ship` for `Viewer::Player` with a live hull. Lists ascend by id.
/// Unit: `sys-snapshot`.
pub fn build(sim: &Sim, viewer: Viewer) -> Snapshot {
    let tick = sim.world.tick().0;
    let mut snap = Snapshot { tick, pacing: sim.pacing(), ..Default::default() };
    let Some((side, own)) = resolve_viewer(sim, viewer) else { return snap };
    let world = &sim.world;
    let pvp = super::events::is_pvp(sim.config.mode);
    let mask = side.map(|s| allied_of(sim, s));
    let is_allied = |x: SideId| mask.map(|m| m.has(x)).unwrap_or(true);
    let own_vessel = own.and_then(|o| world.vessel(o));
    let own_pos = own_vessel.map(|v| v.kin.pose.pos);
    let plot_side = side.unwrap_or_else(|| omniscient_plot_side(sim));
    let picture = sim.pictures.side(plot_side);
    let designated = own.and_then(|o| sim.ships.get(o)).map(|s| s.orders.designated).unwrap_or(TrackId::NONE);
    let track_of = |e: EntityId| picture.and_then(|p| p.track_of(e));

    // Entities the viewer may draw, ascending.
    let drawable: Vec<EntityId> = match side {
        Some(s) => sim.interest.drawable.get(s.index()).cloned().unwrap_or_default(),
        None => { let mut v: Vec<EntityId> = world.entities().iter().collect(); v.sort(); v }
    };
    let can_draw = |e: EntityId| side.is_none() || drawable.binary_search(&e).is_ok();
    let engaged_track = |t: Option<&Track>| t.map(|t| t.engaged_by > 0 || t.inbound || t.fired_on_us || (!designated.is_none() && t.id == designated)).unwrap_or(false);

    for &e in &drawable {
        let Some(v) = world.vessel(e) else { continue };
        let t = track_of(e);
        let relevance = if Some(e) == own { Relevance::Own }
            else if side.is_some() && is_allied(v.side) { Relevance::OwnSide }
            else { relevance(own_pos, v.kin.pose.pos, v.kin.lin_vel, side.is_none(), engaged_track(t), false) };
        let exterior = if pvp && side.is_some() && !is_allied(v.side) { Some(sim.interest.exterior.get(&e)) } else { None };
        snap.vessels.push(VesselView {
            entity: e,
            kin: v.kin,
            status: v.status,
            flags: vessel_flags(sim, v),
            turrets: turrets(v, exterior),
            rudder: v.controls.rudder.clamp(-1.0, 1.0) as f32,
            power: shaft_fraction(v, &sim.deps.catalogue) as f32,
            track: t.map(|t| t.id).unwrap_or(TrackId::NONE),
            relevance,
        });
    }

    let bodies = world.bodies();
    for row in 0..bodies.len() {
        let e = bodies.entity[row];
        if !can_draw(e) { continue; }
        let s = bodies.side[row];
        let allied = side.is_some() && is_allied(s);
        let kind = bodies.kind[row];
        let weapon = sim.deps.catalogue.munitions.get_index(kind.index()).map(|(_, d)| is_weapon(d.kind)).unwrap_or(false);
        let t = track_of(e);
        let relevance = if allied { Relevance::OwnSide } else { relevance(own_pos, bodies.pos[row], bodies.vel[row], side.is_none(), engaged_track(t), weapon) };
        let seeker = if allied || side.is_none() { sim.guidance.guided.iter().find(|g| g.body == e).map(|g| g.phase).unwrap_or_default() } else { SeekerPhase::Off };
        snap.bodies.push(BodyView { entity: e, kind, side: s, pos: bodies.pos[row], vel: bodies.vel[row], phase: bodies.phase[row], seeker, track: t.map(|t| t.id).unwrap_or(TrackId::NONE), relevance });
    }
    snap.bodies.sort_by_key(|b| b.entity);

    let decoys = world.decoys();
    for row in 0..decoys.entity.len() {
        let e = decoys.entity[row];
        if !can_draw(e) { continue; }
        let owner = decoys.owner[row];
        snap.decoys.push(DecoyView { entity: e, kind: decoys.kind[row], owner: if can_draw(owner) { owner } else { EntityId::NONE }, pos: decoys.pos[row], age_s: decoys.age_s[row] as f32 });
    }
    snap.decoys.sort_by_key(|d| d.entity);

    if let Some(p) = picture {
        let tuning = &sim.deps.catalogue.tuning.tracks;
        let mut envelopes: BTreeMap<ClassId, f32> = BTreeMap::new();
        for t in &p.tracks {
            let mut tv = track_view(t, own_pos, tick, tuning);
            if !designated.is_none() && t.id == designated && tv.relevance < Relevance::Engaged { tv.relevance = Relevance::Engaged; }
            if tv.stance != Stance::Friend {
                if let Some(c) = &tv.class {
                    let env = *envelopes.entry(c.clone()).or_insert_with(|| class_envelope_m(sim, c));
                    tv.envelope_m = env;
                }
            }
            snap.tracks.push(tv);
        }
    }

    if let (Some(o), Some(v)) = (own, own_vessel) {
        if !matches!(v.status, VesselStatus::Lost(_)) { snap.ship = ship_status(sim, o); }
        snap.alerts = sim.alerts.for_vessel(o);
    }
    if let Some(m) = mask {
        for ship in &sim.ships.ships {
            if Some(ship.vessel) == own || !m.has(ship.side) { continue; }
            let Some(v) = world.vessel(ship.vessel) else { continue };
            if !matches!(v.status, VesselStatus::Active | VesselStatus::Disabled) { continue; }
            snap.allies.push(AllyStatus {
                vessel: ship.vessel,
                player: ship.player,
                speed_kn: (v.forward_speed() / KN) as f32,
                cells_lost: v.cells_lost_fraction() as f32,
                burning: v.is_burning(),
                flooding: v.compartments.compartments.iter().any(|c| c.volume_m3 > 0.0 && c.water_m3 > 0.01 * c.volume_m3),
                emcon: ship.settings.emcon,
                officers_in_command: ship.player.is_none() || ship.control.officers_in_command,
            });
        }
    }
    snap.objectives = crate::objectives::views(&sim.objectives, &sim.config);
    snap
}

/// Items in a snapshot: the deterministic work count for `SystemsWork::snapshot_items` (the driver adds it
/// after building each viewer's snapshot, since `build` borrows the Sim immutably).
pub fn snapshot_items(s: &Snapshot) -> u32 {
    let ship = s.ship.as_ref().map(|sh| 1 + sh.parts.len() + sh.mounts.len() + sh.compartments.len()).unwrap_or(0);
    (s.vessels.len() + s.bodies.len() + s.decoys.len() + s.tracks.len() + s.allies.len() + s.alerts.len() + s.objectives.len() + ship) as u32
}

fn vessel_flags(sim: &Sim, v: &Vessel) -> VesselFlags {
    let mut f = VesselFlags::empty();
    let sig = sim.sigs.get(v.id);
    let tick = sim.world.tick().0;
    if v.kin.lin_vel.length_squared() < 0.05 * 0.05 && v.kin.ang_vel.length_squared() < 0.01 * 0.01 { f |= VesselFlags::REST; }
    if v.is_burning() { f |= VesselFlags::BURNING; }
    if sig.map(|s| s.lights).unwrap_or(false) { f |= VesselFlags::LIGHTS; }
    let fired = sig.map(|s| s.fired_recently).unwrap_or(false) || (v.last_fired_tick > 0 && tick.saturating_sub(v.last_fired_tick) <= FIRED_RECENTLY_TICKS);
    if fired { f |= VesselFlags::FIRED_RECENTLY; }
    if v.fidelity == Fidelity::Kinematic { f |= VesselFlags::KINEMATIC; }
    if v.struck { f |= VesselFlags::STRUCK_COLOURS; }
    let d = sim.world.decoys();
    if (0..d.entity.len()).any(|r| d.owner[r] == v.id && d.kind[r] == DecoyKind::TowedDecoy) { f |= VesselFlags::TOWING_DECOY; }
    if v.submersible && sig.map(|s| s.top_m < 0.0).unwrap_or(false) { f |= VesselFlags::SUBMERGED; }
    if v.status == VesselStatus::Wreck || v.parent.is_some() { f |= VesselFlags::WRECK; }
    f
}

/// Trainable parts. For the other PvP team only the exterior blueprint's parts (none without an index).
fn turrets(v: &Vessel, exterior: Option<Option<&super::interest::ExteriorIndex>>) -> Vec<TurretView> {
    let allowed = |slot: PartSlot| match exterior { None => true, Some(Some(ix)) => ix.parts.binary_search(&slot).is_ok(), Some(None) => false };
    v.parts.iter().filter(|p| allowed(p.slot)).filter_map(|p| match &p.state {
        PartState::Mount(m) => Some(TurretView { slot: p.slot, train_rad: m.train_rad as f32, elevation_rad: m.elevation_rad as f32 }),
        PartState::Launcher { train_rad, .. } => Some(TurretView { slot: p.slot, train_rad: *train_rad as f32, elevation_rad: 0.0 }),
        PartState::Sensor { train_rad, .. } => Some(TurretView { slot: p.slot, train_rad: *train_rad as f32, elevation_rad: 0.0 }),
        _ => None,
    }).collect()
}

/// Shaft power actually absorbed over the propulsors' rating, 0..=1.
fn shaft_fraction(v: &Vessel, catalogue: &Catalogue) -> f64 {
    let (mut kw, mut max) = (0.0, 0.0);
    for p in &v.parts {
        if let PartState::Propulsor { shaft_kw, .. } = p.state {
            if let Some(def) = catalogue.parts.get_index(p.def.index()).map(|(_, d)| d) {
                if let PartFunction::Propulsor(pd) = &def.function { max += pd.max_shaft_kw; kw += shaft_kw.abs(); }
            }
        }
    }
    if max > 0.0 { (kw / max).clamp(0.0, 1.0) } else { 0.0 }
}

/// Longest weapon envelope of an identified class: the longest effective range of any mount on a hull of
/// that class in this sortie (designs are public once the class is known), else its doctrine engagement
/// range from the campaign data.
fn class_envelope_m(sim: &Sim, class: &ClassId) -> f32 {
    let from_designs = sim.ships.ships.iter()
        .filter(|s| sim.world.vessel(s.vessel).and_then(|v| v.class.as_ref()) == Some(class))
        .flat_map(|s| s.design.mounts.iter().filter(|m| m.weapon != WeaponClass::Decoy).map(|m| m.effective_range_m))
        .fold(0.0f64, f64::max);
    if from_designs > 0.0 { return from_designs as f32; }
    sim.deps.campaign.classes.get(class).and_then(|c| c.engagement_range_m).unwrap_or(0.0) as f32
}

/// Relevance of something at `pos` moving at `vel` for a viewer hull at `own_pos` (L1423, L924).
/// Unit: `sys-snapshot`.
pub fn relevance(own_pos: Option<DVec3>, pos: DVec3, vel: DVec3, own_side: bool, engaged: bool, weapon: bool) -> Relevance {
    if own_side { return Relevance::OwnSide; }
    if engaged { return Relevance::Engaged; }
    let Some(o) = own_pos else { return if weapon { Relevance::DistantFast } else { Relevance::Distant } };
    let d = pos - o;
    let r = d.length();
    if !r.is_finite() { return Relevance::Distant; }
    if r <= DISTANT_M { return Relevance::Near; }
    let closing = if r > 0.0 { -vel.dot(d) / r } else { 0.0 };
    if weapon || closing > FAST_CLOSING_MPS { Relevance::DistantFast } else { Relevance::Distant }
}

/// One track as the viewer sees it. Unit: `sys-snapshot`.
pub fn track_view(track: &Track, own_pos: Option<DVec3>, tick: u64, tuning: &content::tuning::TrackTuning) -> TrackView {
    let pos = if track.is_live() { track.predict(tick) } else { track.pos };
    let vel = if track.is_live() { track.vel } else { DVec3::ZERO };
    let engaged = track.engaged_by > 0 || track.inbound || track.fired_on_us;
    TrackView {
        id: track.id,
        domain: track.domain,
        stance: track.stance,
        stance_reason: track.stance_reason,
        rung: track.rung,
        rung_reason: track.rung_reason,
        grade: track.grade,
        pos,
        vel,
        shape: track.shape(tick, tuning),
        last_update_tick: track.last_update_tick,
        last_known: !track.is_live(),
        class: if track.rung >= IdRung::Class { track.class.clone() } else { None },
        candidates: track.candidates.remaining.len().min(u16::MAX as usize) as u16,
        sources: track.sources,
        trend: track.trend,
        inbound: track.inbound,
        engaged_by: track.engaged_by,
        envelope_m: 0.0,
        emitting: track.emitting,
        relevance: relevance(own_pos, pos, vel, track.stance == Stance::Friend, track.is_live() && engaged, track.domain == Domain::Weapon),
        struck: track.struck,
    }
}

/// Expert solution readout: fixed-point lead against the range table (see module docs).
fn solution_view(muzzle: DVec3, own_vel: DVec3, target_pos: DVec3, target_vel: DVec3, table: &[crate::guidance::gun::RangeRow], tuning: &content::tuning::GuidanceTuning) -> Option<SolutionView> {
    if table.is_empty() { return None; }
    let tof_at = |range: f64| -> Option<f64> {
        let last = table.last()?;
        if range > last.range_m { return None; }
        let i = table.partition_point(|r| r.range_m < range);
        if i == 0 { let r0 = &table[0]; return Some(if r0.range_m > 0.0 { r0.time_of_flight_s * range / r0.range_m } else { r0.time_of_flight_s }); }
        let (a, b) = (&table[i - 1], &table[i]);
        let span = b.range_m - a.range_m;
        let f = if span > 0.0 { (range - a.range_m) / span } else { 0.0 };
        Some(a.time_of_flight_s + f * (b.time_of_flight_s - a.time_of_flight_s))
    };
    let cap = tuning.gun_iteration_cap.max(1);
    let base = target_pos;
    let mut tof = 0.0;
    let mut aim = base;
    let (mut iterations, mut converged, mut err) = (0u8, false, f64::MAX);
    for _ in 0..cap {
        iterations += 1;
        // Lead relative to the firing platform: the shell inherits the ship's velocity.
        let next = base + (target_vel - own_vel) * tof;
        let d = next - muzzle;
        let range = (d.x * d.x + d.z * d.z).sqrt();
        let Some(t) = tof_at(range) else { return Some(SolutionView { aim_point: next, time_of_flight_s: tof as f32, iterations, converged: false, error_m: f32::MAX }) };
        err = next.distance(aim);
        aim = next;
        tof = t;
        if iterations > 1 && err < tuning.gun_converge_m { converged = true; break; }
    }
    Some(SolutionView { aim_point: aim, time_of_flight_s: tof as f32, iterations, converged, error_m: err.min(f32::MAX as f64) as f32 })
}

fn readout(p: &sim_core::parts::PartRuntime, v: &Vessel, catalogue: &Catalogue) -> PartReadout {
    let def = catalogue.parts.get_index(p.def.index()).map(|(_, d)| d);
    match &p.state {
        PartState::Mount(m) => {
            let calibre = def.and_then(|d| if let PartFunction::Gun(g) = &d.function { Some(g.calibre_mm) } else { None });
            let magazine = v.parts.iter().filter_map(|q| match (&q.state, catalogue.parts.get_index(q.def.index()).map(|(_, d)| &d.function)) {
                (PartState::Magazine { rounds, .. }, Some(PartFunction::Magazine { calibre_mm, .. })) if Some(*calibre_mm) == calibre => Some(*rounds),
                _ => None,
            }).sum();
            PartReadout::Rounds { ready: m.ready_rounds, magazine }
        }
        PartState::Launcher { ready, reload_ticks, .. } => PartReadout::Cells { ready: *ready, reload_s: *reload_ticks as f32 / sim_core::TICK_HZ as f32 },
        PartState::Engine { output_kw, .. } | PartState::Generator { output_kw } => PartReadout::Output { kw: *output_kw as f32 },
        PartState::Propulsor { shaft_kw, .. } => PartReadout::Output { kw: *shaft_kw as f32 },
        PartState::FuelTank { litres } => PartReadout::Fuel { litres: *litres as f32 },
        PartState::Magazine { rounds, .. } => PartReadout::Rounds { ready: 0, magazine: *rounds },
        PartState::MineRail { mines, .. } => PartReadout::Rounds { ready: *mines as u32, magazine: *mines as u32 },
        PartState::Decoy { rounds, reload_ticks, .. } => PartReadout::Cells { ready: (*rounds).min(255) as u8, reload_s: *reload_ticks as f32 / sim_core::TICK_HZ as f32 },
        PartState::Sensor { radiating, .. } | PartState::Jammer { radiating } => PartReadout::Radiating(*radiating),
        PartState::Pump { running, .. } => PartReadout::Running(*running),
        PartState::Door { closed } => PartReadout::Closed(*closed),
        PartState::DroneBay { aboard, .. } => PartReadout::Aboard(*aboard),
        PartState::HeliDeck { aboard } => PartReadout::Aboard(*aboard as u8),
        PartState::Davit { rhib_aboard } => PartReadout::Aboard(*rhib_aboard as u8),
        PartState::Inert | PartState::Battery { .. } | PartState::Rudder { .. } | PartState::Ballast { .. } => PartReadout::None,
    }
}

/// The viewer's own hull in detail. Unit: `sys-snapshot`.
pub fn ship_status(sim: &Sim, vessel: EntityId) -> Option<ShipStatus> {
    let v = sim.world.vessel(vessel)?;
    let ship = sim.ships.get(vessel)?;
    let catalogue = &*sim.deps.catalogue;
    let tick = sim.world.tick().0;
    let picture = sim.pictures.side(ship.side);
    let heading = v.kin.pose.heading_rad();
    let (heel, trim) = v.heel_trim();
    let manual_helm = ship.control.holds(ControlRole::Helm);
    let (ordered_speed_kn, ordered_heading_rad) = if manual_helm {
        ((ship.control.input.throttle.clamp(-1.0, 1.0) as f64 * ship.design.turn.top_speed_mps / KN) as f32, heading as f32)
    } else {
        (ship.stages.helm.ordered_speed_kn as f32, ship.stages.helm.ordered_heading_rad as f32)
    };

    let capacity: f64 = v.parts.iter().filter_map(|p| catalogue.parts.get_index(p.def.index()).map(|(_, d)| d)).filter_map(|d| if let PartFunction::FuelTank { litres } = d.function { Some(litres) } else { None }).sum();
    let n = &v.networks;
    let power = PowerView { generation_kw: n.generation_kw as f32, demand_kw: n.demand_kw as f32, supplied_kw: n.supplied_kw as f32, fuel_l: n.fuel_l as f32, fuel_fraction: if capacity > 0.0 { (n.fuel_l / capacity).clamp(0.0, 1.0) as f32 } else { 0.0 }, fuel_flow_lps: n.fuel_flow_lps as f32 };

    let parts = v.parts.iter().map(|p| PartView {
        slot: p.slot, level: p.level, health: u8_frac(p.health), power: p.links.power, fuel: p.links.fuel, data: p.links.data, shed: p.links.shed, priority: p.priority, readout: readout(p, v, catalogue),
    }).collect();

    let mount_range = |m: &crate::doctrine::MountInfo, t: Option<&Track>| -> f64 {
        if let Some(e) = t.and_then(|t| ship.doctrine.get(m.slot, crate::doctrine::target_category(t))) { return e.effective_range_m; }
        let best = ship.doctrine.entries.iter().filter(|((s, _), _)| *s == m.slot).map(|(_, e)| e.effective_range_m).fold(0.0f64, f64::max);
        if best > 0.0 { best } else { m.effective_range_m }
    };
    let mut longest = 0.0f64;
    let mounts = ship.design.mounts.iter().map(|m| {
        let status = ship.stages.engage.mounts.iter().find(|x| x.slot == m.slot).map(|x| x.status).unwrap_or_default();
        let track_id = match status { EngageStatus::Engaging { track, .. } | EngageStatus::Holding { track, .. } => track, EngageStatus::Idle => TrackId::NONE };
        let track = picture.and_then(|p| p.get(track_id)).filter(|t| t.is_live());
        let held = ship.control.holds(ControlRole::Mount(m.slot)) || ship.control.holds(ControlRole::WeaponGroup(m.group));
        let solution = match (ship.design.range_tables.get(&m.slot), track) {
            (Some(table), Some(t)) => {
                let (mut p, vel) = t.gun_target(tick);
                if t.domain == Domain::Surface { p.y += crate::guidance::gun::wave_lay_m(t.rcs_m2 as f64, sim.config().weather.sea_state); }
                solution_view(v.to_world(m.pos_local), v.kin.lin_vel, p, vel, table, &catalogue.tuning.guidance)
            }
            _ => None,
        };
        let range = mount_range(m, track);
        if m.weapon != WeaponClass::Decoy { longest = longest.max(range); }
        MountView {
            slot: m.slot,
            status,
            holder: if held { ControlHolder::Player } else { ControlHolder::Officer },
            solution,
            effective_range_m: range as f32,
            local_control: matches!(ship.part_command.get(m.slot.index()), Some(crate::datalink::PartCommand::Local { .. })),
        }
    }).collect();

    let compartments = v.compartments.compartments.iter().map(|c| CompartmentView {
        id: c.id,
        fill: if c.volume_m3 > 0.0 { u8_frac(c.water_m3 / c.volume_m3) } else { 0 },
        fire: u8_frac(c.fire / 100.0),
        breached: v.compartments.openings.iter().any(|o| o.a == c.id && o.b.is_none() && o.kind == sim_core::compartments::OpeningKind::Breach && !o.patched),
        valve_open: c.flood_valve_open,
        sealed: c.sealed,
    }).collect();

    let crew_t = &catalogue.tuning.crew;
    let teams = v.crew.teams.iter().enumerate().map(|(i, t)| {
        let needed = match t.order {
            sim_core::parts::TeamOrder::Idle => 0.0,
            sim_core::parts::TeamOrder::FightFire(cid) => {
                // Progress of firefighting is how far the fire has been knocked down.
                let fire = v.compartments.compartments.iter().find(|c| c.id == cid).map(|c| c.fire).unwrap_or(0.0);
                let p = 1.0 - (fire / 100.0).clamp(0.0, 1.0);
                return (t.order, u8_frac(p), ship.stages.dc.pinned.iter().any(|(ti, _)| *ti as usize == i));
            }
            sim_core::parts::TeamOrder::Patch { .. } => crew_t.patch_face_s,
            sim_core::parts::TeamOrder::JuryRig { .. } => crew_t.jury_rig_s,
            sim_core::parts::TeamOrder::Repair(_) => crew_t.repair_job_s,
        };
        let p = if needed > 0.0 { t.progress_s / needed } else { 0.0 };
        (t.order, u8_frac(p), ship.stages.dc.pinned.iter().any(|(ti, _)| *ti as usize == i))
    }).collect();
    let crew = CrewView {
        original_berths: v.crew.original_berths,
        surviving_berths: v.crew.surviving_berths,
        passengers: v.crew.passengers,
        passengers_lost: v.crew.passengers_lost,
        teams,
        repair_order: if ship.settings.repair_order.is_empty() { content::graph::RepairPriority::DEFAULT_ORDER.to_vec() } else { ship.settings.repair_order.clone() },
    };

    let offboard = ship.design.bays.iter().map(|&slot| {
        let unit = ship.offboard.units.iter().find(|u| u.bay == slot);
        let (aboard, recharging_s) = match v.parts.get(slot.index()).map(|p| &p.state) {
            Some(PartState::DroneBay { aboard, recharge_ticks }) => (*aboard, *recharge_ticks as f32 / sim_core::TICK_HZ as f32),
            Some(PartState::HeliDeck { aboard }) => (*aboard as u8, 0.0),
            _ => (0, 0.0),
        };
        OffboardView { slot, body: unit.and_then(|u| u.body), aboard, endurance_s: unit.map(|u| u.endurance_left_s as f32).unwrap_or(0.0), recharging_s }
    }).collect();

    let actions = sim.specops.actions.iter().filter(|a| a.actor == vessel).map(|a| ActionView {
        id: a.id,
        action: a.action.clone(),
        actor: a.actor,
        state: a.state,
        progress: if a.needed > 0.0 { (a.progress / a.needed).clamp(0.0, 1.0) as f32 } else { 0.0 },
        block: a.block,
        at: match &a.target {
            crate::protocol::command::ActionTarget::None => None,
            crate::protocol::command::ActionTarget::Track(t) => picture.and_then(|p| p.get(*t)).map(|t| { let p = t.predict(tick); DVec2::new(p.x, p.z) }),
            crate::protocol::command::ActionTarget::Marker(m) => sim.config.map.markers.iter().find(|x| &x.name == m).map(|x| DVec2::new(x.pos_m[0], x.pos_m[1])),
            crate::protocol::command::ActionTarget::Line { a, b } => Some((*a + *b) * 0.5),
        },
    }).collect();

    Some(ShipStatus {
        vessel,
        speed_kn: (v.forward_speed() / KN) as f32,
        heading_rad: heading as f32,
        heel_deg: (heel * RAD2DEG) as f32,
        trim_deg: (trim * RAD2DEG) as f32,
        ordered_speed_kn,
        ordered_heading_rad,
        route: ship.orders.route.clone(),
        designated: ship.orders.designated,
        power,
        parts,
        mounts,
        compartments,
        crew,
        board: ship.board.clone(),
        held: ship.control.held.clone(),
        offboard,
        actions,
        longest_envelope_m: longest as f32,
        cells_lost: v.cells_lost_fraction() as f32,
    })
}

/// Static facts about an entity for a side, with damage so far (full for own side; exterior-only form in PvP
/// for the other team). Unit: `sys-snapshot`.
pub fn entity_intro(sim: &Sim, entity: EntityId, for_side: SideId) -> Option<EntityIntro> {
    let world = &sim.world;
    let mask = allied_of(sim, for_side);
    let track = sim.pictures.side(for_side).and_then(|p| p.track_of(entity));
    let track_id = track.map(|t| t.id).unwrap_or(TrackId::NONE);
    let Some(v) = world.vessel(entity) else {
        // Small bodies and decoys: no design; the snapshot carries their kind and pose.
        let (_, _, side) = world.locate(entity)?;
        return Some(EntityIntro { entity, side, blueprint: ContentHash::EMPTY, exterior_only: false, class: None, name: String::new(), player: None, status: VesselStatus::Active, damage: Vec::new(), removed: Vec::new(), parent: None, track: track_id });
    };
    let allied = mask.has(v.side);
    let exterior_only = super::events::is_pvp(sim.config.mode) && !allied;
    let built;
    let ext = if exterior_only {
        match sim.interest.exterior.get(&entity) {
            Some(ix) => Some(ix),
            None => { built = super::events::current_exterior(sim, entity).map(|x| x.0); built.as_ref() }
        }
    } else { None };
    // Fail closed: without an exterior index the other team gets the hull with no design and no cells.
    if exterior_only && ext.is_none() {
        return Some(EntityIntro { entity, side: v.side, blueprint: ContentHash::EMPTY, exterior_only: true, class: None, name: String::new(), player: v.player, status: v.status, damage: Vec::new(), removed: Vec::new(), parent: v.parent, track: track_id });
    }
    let knows = |i: usize| ext.map(|ix| ix.knows(CellIndex(i as u32))).unwrap_or(true);
    let grid = &v.grid;
    let n = grid.damage.len().min(grid.design.cells.len());
    let mut damage = Vec::new();
    let mut removed = Vec::new();
    for i in 0..n {
        if grid.design.cells[i].is_empty() || !knows(i) { continue; }
        let b = grid.damage[i];
        if b > 0 { damage.push(CellChange { cell: CellIndex(i as u32), bucket: b }); }
        if b < BUCKET_DESTROYED && !grid.is_solid(grid.pos(CellIndex(i as u32))) { removed.push(CellIndex(i as u32)); }
    }
    let rung = track.map(|t| t.rung).unwrap_or_default();
    Some(EntityIntro {
        entity,
        side: v.side,
        blueprint: ext.map(|ix| ix.exterior_hash).unwrap_or(v.blueprint),
        exterior_only,
        class: if allied || rung >= IdRung::Class { v.class.clone() } else { None },
        name: if allied || rung >= IdRung::Exact { v.name.clone() } else { String::new() },
        player: v.player,
        status: v.status,
        damage,
        removed,
        parent: v.parent,
        track: track_id,
    })
}

/// Catch-up for a client that joins or rejoins while the sortie runs (co-op rejoin, sandbox join): in order,
/// `SortieStarted`, the current weather and wave seed as `Phys(WeatherChanged)`, `PacingChanged`, then an
/// `EntityRevealed` (via [`entity_intro`]) for every entity the viewer's side may draw now, ascending, each
/// addressed to that viewer only. Added by the net contract; net sends these before the first snapshot frame.
/// Unit: `sys-snapshot`.
pub fn catch_up(sim: &Sim, viewer: Viewer) -> Vec<OutEvent> {
    let tick = sim.world.tick().0;
    let (audience, side) = match viewer {
        Viewer::Player(p) => match sim.players.iter().find(|x| x.slot == p) { Some(ps) => (Audience::player(p), Some(ps.side)), None => return Vec::new() },
        Viewer::Side(s) => (Audience::side(s), Some(s)),
        Viewer::Omniscient => (Audience::Host, None),
    };
    let ev = |event: SimEvent| OutEvent { tick, audience, lane: Lane::Reliable, event };
    let mut out = vec![
        ev(SimEvent::SortieStarted { seed: sim.config.seed, tick_hz: sim_core::TICK_HZ }),
        ev(SimEvent::Phys(sim_core::events::PhysEvent::WeatherChanged { weather: sim.world.weather().clone(), wave_seed: sim.world.waves().seed })),
        ev(SimEvent::PacingChanged(sim.pacing())),
    ];
    let entities: Vec<EntityId> = match side {
        Some(s) => sim.interest.drawable.get(s.index()).cloned().unwrap_or_default(),
        None => { let mut v: Vec<EntityId> = sim.world.entities().iter().collect(); v.sort(); v }
    };
    for e in entities {
        let for_side = match side { Some(s) => s, None => match sim.world.locate(e) { Some(l) => l.2, None => continue } };
        if let Some(intro) = entity_intro(sim, e, for_side) { out.push(ev(SimEvent::EntityRevealed(Box::new(intro)))); }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::guidance::gun::RangeRow;

    #[test]
    fn relevance_bands() {
        let o = Some(DVec3::ZERO);
        assert_eq!(relevance(o, DVec3::new(9_000.0, 0.0, 0.0), DVec3::ZERO, false, false, false), Relevance::Near);
        assert_eq!(relevance(o, DVec3::new(12_000.0, 0.0, 0.0), DVec3::ZERO, false, false, false), Relevance::Distant);
        // Closing at 31 kn from 12 km.
        assert_eq!(relevance(o, DVec3::new(12_000.0, 0.0, 0.0), DVec3::new(-31.0 * KN, 0.0, 0.0), false, false, false), Relevance::DistantFast);
        // Opening at 40 kn stays distant.
        assert_eq!(relevance(o, DVec3::new(12_000.0, 0.0, 0.0), DVec3::new(40.0 * KN, 0.0, 0.0), false, false, false), Relevance::Distant);
        assert_eq!(relevance(o, DVec3::new(50_000.0, 0.0, 0.0), DVec3::ZERO, false, false, true), Relevance::DistantFast);
        assert_eq!(relevance(o, DVec3::new(50_000.0, 0.0, 0.0), DVec3::ZERO, false, true, false), Relevance::Engaged);
        assert_eq!(relevance(o, DVec3::new(50_000.0, 0.0, 0.0), DVec3::ZERO, true, true, false), Relevance::OwnSide);
        assert_eq!(relevance(None, DVec3::ZERO, DVec3::ZERO, false, false, false), Relevance::Distant);
        assert_eq!(relevance(o, DVec3::ZERO, DVec3::X, false, false, false), Relevance::Near);
        assert_eq!(relevance(o, DVec3::new(f64::NAN, 0.0, 0.0), DVec3::ZERO, false, false, false), Relevance::Distant);
    }

    fn table() -> Vec<RangeRow> {
        (0..=40).map(|i| { let r = i as f64 * 250.0; RangeRow { range_m: r, elevation_rad: 0.0, time_of_flight_s: r / 700.0, impact_mps: 700.0, fall_angle_rad: 0.0 } }).collect()
    }
    #[test]
    fn lead_iteration_converges_and_respects_cap() {
        let tuning = content::tuning::GuidanceTuning::default();
        // Stationary target 7 km away: time of flight 10 s, converged on the second pass.
        let s = solution_view(DVec3::ZERO, DVec3::ZERO, DVec3::new(7_000.0, 0.0, 0.0), DVec3::ZERO, &table(), &tuning).unwrap();
        assert!(s.converged);
        assert!((s.time_of_flight_s - 10.0).abs() < 1e-3);
        assert_eq!(s.iterations, 2);
        // A crossing target at 10 m/s: the aim point leads it by about v * tof.
        let s = solution_view(DVec3::ZERO, DVec3::ZERO, DVec3::new(5_000.0, 0.0, 0.0), DVec3::new(0.0, 0.0, 10.0), &table(), &tuning).unwrap();
        assert!(s.converged);
        assert!((s.aim_point.z - 10.0 * s.time_of_flight_s as f64).abs() < 0.5);
        assert!(s.iterations <= tuning.gun_iteration_cap);
        // Beyond the table: unconverged.
        let s = solution_view(DVec3::ZERO, DVec3::ZERO, DVec3::new(20_000.0, 0.0, 0.0), DVec3::ZERO, &table(), &tuning).unwrap();
        assert!(!s.converged);
        assert!(solution_view(DVec3::ZERO, DVec3::ZERO, DVec3::X, DVec3::ZERO, &[], &tuning).is_none());
    }

    #[test]
    fn snapshot_item_count() {
        let mut s = Snapshot::default();
        assert_eq!(snapshot_items(&s), 0);
        s.ship = Some(ShipStatus::default());
        assert_eq!(snapshot_items(&s), 1);
    }
}
