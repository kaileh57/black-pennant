//! Unit `sys-sim-events`. Phase 12 of the tick: fold the physical event stream into every system that must
//! hear it and route events to audiences. DESIGN.md L920-926 (what replicates), L924 (interest), L945 (PvP
//! exterior-only damage), L501 (fires on your side), L686 (salvage on loss), L951 (shot log).
//!
//! # Order inside phase 12
//! 1. Fold every event into guidance, sensors, the ship table, group membership, stance, objectives, the shot
//!    log, captain percepts and ace lines.
//! 2. Refresh interest ([`Sim::refresh_interest`]: `InterestState::update`, then `EntityRevealed` for every
//!    entity a side may draw that it has not been told about), so a reveal always precedes the first event
//!    and pose of what it reveals.
//! 3. Route each event with [`route_phys`] in emission order.
//! 4. `EntityHidden` for what left a side's drawable set. Events of the hiding tick still reach the side.
//!
//! Interest refresh is idempotent within a tick, so a driver that also calls `InterestState::update` in the
//! same tick changes nothing.
//!
//! # Per-variant routing (anti-wallhack)
//! Interior-state events (`PartDamaged`, `Breach`, `ConduitCut`, `LinkChanged`, `FireStarted`, `FireOut`,
//! `CompartmentHalfFlooded`, `MagazineFlooded`, `PassengersLost`, `JobDone`) go to own and allied sides only:
//! another side sees fire through the snapshot's `BURNING` flag and damage through cell changes. A side that
//! fires the shell that sinks a hull it cannot draw still hears `VesselLost` (kill confirmation), with no
//! position. A side that saw a shell spawn but cannot draw the hull it strikes gets the `ShellImpact` with the
//! hull scrubbed, so its shell closes where the client already knows it flies.
use super::interest::{exterior_changes, ExteriorIndex, InterestState, KILL_CAM_TICKS};
use super::Sim;
use crate::prelude::*;
use crate::protocol::config::SortieConfig;
use crate::protocol::event::{Audience, Lane, OutEvent, SimEvent};
use crate::protocol::text::ControlHolder;
use content::part::weapons::{MunitionDef, MunitionKind, SeekerKind};
use content::graph::SinkKind;
use crate::graph::PortValue;
use sim_core::events::PhysEvent;
use sim_core::vessel::VesselStatus;

/// A splash within this horizontal distance of a hull counts as firing on that hull's side (L501).
pub const NEAR_MISS_M: f64 = 150.0;
/// Visual size of an air or underwater burst for point visibility, m^2, and its top above the sea, m.
const BURST_AREA_M2: f64 = 100.0;
const BURST_TOP_M: f64 = 15.0;

/// Water-column area a shell splash presents to a lookout, m^2: half a square metre per millimetre of
/// calibre, 10 to 200 m^2.
pub(crate) fn splash_area_m2(calibre_mm: u16) -> f64 { (0.5 * calibre_mm as f64).clamp(10.0, 200.0) }
/// Height of the water column: a tall, narrow plume twice as high as it is wide.
pub(crate) fn splash_top_m(area_m2: f64) -> f64 { 2.0 * area_m2.max(0.0).sqrt() }

/// The per-tick context, built from disjoint fields so the systems can be borrowed mutably next to it.
macro_rules! sim_ctx {
    ($s:expr) => {
        crate::ctx::SimCtx {
            seed: $s.config.seed,
            tick: $s.world.tick(),
            catalogue: &*$s.deps.catalogue,
            tuning: &$s.deps.catalogue.tuning,
            weather: $s.world.weather(),
            time_of_day: $s.config.time_of_day,
            layer_depth_m: $s.config.layer_depth_m,
            difficulty: &$s.config.settings.difficulty,
        }
    };
}

/// Allied mask per side id (each side includes itself), from the sortie config.
pub(crate) fn allied_masks(config: &SortieConfig) -> Vec<SideMask> { allied_masks_of(&config.sides) }

fn allied_masks_of(sides: &[crate::protocol::config::SideDef]) -> Vec<SideMask> {
    let n = sides.iter().map(|s| s.id.index() + 1).max().unwrap_or(0).min(8);
    let mut v: Vec<SideMask> = (0..n).map(|i| SideMask::only(SideId(i as u8))).collect();
    for s in sides {
        if s.id.index() < n { v[s.id.index()] = v[s.id.index()].union(s.allied).with(s.id); }
    }
    // Alliance is symmetric for interest: a datalink picture is shared both ways.
    for i in 0..n {
        for j in 0..n {
            if v[i].has(SideId(j as u8)) { v[j] = v[j].with(SideId(i as u8)); }
        }
    }
    v
}

pub(crate) fn is_pvp(mode: GameMode) -> bool { matches!(mode, GameMode::PvpElimination | GameMode::PvpEscort) }

fn is_guided(def: &MunitionDef) -> bool {
    match def.kind {
        MunitionKind::AntiShipMissile | MunitionKind::Sam | MunitionKind::PdMissile | MunitionKind::Torpedo => true,
        MunitionKind::Mine | MunitionKind::DepthCharge | MunitionKind::Drone | MunitionKind::Aircraft | MunitionKind::Helicopter | MunitionKind::Rhib => false,
    }
}

/// The track a launch from (launcher, slot) was aimed at: the graph's missile-target sink of this tick, the
/// mount's engagement, a manual launch request, then the designated track.
fn launch_track(ships: &crate::ship::ShipTable, launcher: EntityId, slot: PartSlot) -> TrackId {
    let Some(ship) = ships.get(launcher) else { return TrackId::NONE };
    if let Some(w) = ship.sinks.get(SinkKind::MissileTarget(slot)) {
        if let Some(t) = w.values.iter().find_map(|v| if let PortValue::Track(t) = v { (!t.is_none()).then_some(*t) } else { None }) { return t; }
    }
    if let Some(m) = ship.stages.engage.mounts.iter().find(|m| m.slot == slot) {
        match m.status { EngageStatus::Engaging { track, .. } if !track.is_none() => return track, _ => {} }
    }
    if let Some(l) = ship.orders.launches.iter().find(|l| l.slot == slot && !l.track.is_none()) { return l.track; }
    ship.orders.designated
}

fn has_carrier(sensors: &crate::sensors::SensorScheduler, carrier: EntityId) -> bool { sensors.sensors.iter().any(|s| s.id.carrier == carrier) }

impl Sim {
    /// Process the events `World::step` produced this tick, in emission order:
    /// - `BodySpawned`: register guided bodies with guidance (bound to the launch's track), sensors and, for
    ///   aircraft, the air AI; `BodyDestroyed`, `BodyBurst`, `BodyRecovered`: unregister;
    /// - `VesselSpawned`, `Split`, `VesselLost`, `VesselDespawned`, `StatusChanged`: ship table, sensor
    ///   carriers and group membership upkeep; a lost player hull ends that player's control;
    /// - `CellsDamaged`, `ShellImpact`, `BodyImpact`, `Splash` near own hulls: `ident::stance::note_fired_on`;
    /// - everything: `objectives::ingest`, `input::resolve_shots`, captain percepts, ace facts;
    /// - then route each event with [`route_phys`] into the out queue.
    /// Unit: `sys-sim-events`.
    pub(crate) fn process_phys_events(&mut self, events: Vec<PhysEvent>) {
        let tick = self.world.tick().0;
        let pvp = is_pvp(self.config.mode);
        let allied = allied_masks(&self.config);
        let allied_of = |s: SideId| allied.get(s.index()).copied().unwrap_or(SideMask::only(s));

        // Launchers of bodies that hit something, resolved before any body is unregistered.
        let mut impact_launcher: BTreeMap<EntityId, EntityId> = BTreeMap::new();
        for ev in &events {
            if let PhysEvent::BodyImpact { body, .. } = ev {
                let l = self.world.bodies().row_of(*body).map(|r| self.world.bodies().launcher[r])
                    .or_else(|| self.guidance.guided.iter().find(|g| g.body == *body).map(|g| g.launcher))
                    .or_else(|| events.iter().find_map(|e| if let PhysEvent::BodySpawned { body: b, launcher, .. } = e { (b == body).then_some(*launcher) } else { None }))
                    .unwrap_or(EntityId::NONE);
                impact_launcher.insert(*body, l);
            }
        }

        let mut under_fire: BTreeSet<EntityId> = BTreeSet::new();
        let mut fired_on: BTreeSet<(SideId, EntityId)> = BTreeSet::new();
        let mut lost_players: Vec<(PlayerSlot, EntityId, EntityId, Vec<crate::protocol::command::ControlRole>)> = Vec::new();
        let mut need_exterior: BTreeSet<EntityId> = BTreeSet::new();
        let mut shot_log_changed: Vec<crate::protocol::event::ShotLogEntry> = Vec::new();
        let mut ace_lines: Vec<(AceId, EntityId, SideId, content::campaign::AceCue, Msg)> = Vec::new();
        {
            let ctx = sim_ctx!(self);
            // Record who fired on whom: `victim` is a hull, `shooter` the entity that fired.
            let note = |world: &World, victim: EntityId, shooter: EntityId, fired_on: &mut BTreeSet<(SideId, EntityId)>| {
                if shooter.is_none() { return; }
                let Some(v) = world.vessel(victim) else { return };
                let Some((_, _, ss)) = world.locate(shooter) else { return };
                let mask = allied_of(v.side);
                if mask.has(ss) { return; }
                for s in mask.iter() { fired_on.insert((s, shooter)); }
            };
            for ev in &events {
                match ev {
                    PhysEvent::BodySpawned { body, kind, side, launcher, slot, pos, .. } => {
                        let Some((_, def)) = ctx.catalogue.munitions.get_index(kind.index()) else { continue };
                        if is_guided(def) && !self.guidance.guided.iter().any(|g| g.body == *body) {
                            let track = launch_track(&self.ships, *launcher, *slot);
                            self.guidance.launch(*body, *side, *launcher, *kind, track, None, &self.pictures, &ctx);
                        }
                        if (!def.sensors.is_empty() || def.seeker != SeekerKind::None) && !has_carrier(&self.sensors, *body) {
                            self.sensors.add_carrier(&self.world, *body, &ctx);
                        }
                        if def.kind == MunitionKind::Aircraft && !self.ai.air.aircraft.iter().any(|a| a.body == *body) {
                            let (group, station, radius) = air_station(&self.config, &self.ships, &self.world, def, *side, *launcher, *pos);
                            crate::ai::air::add(&mut self.ai.air, *body, *side, group, station, radius, ctx.catalogue, *kind);
                        }
                    }
                    PhysEvent::BodyDestroyed { body, .. } | PhysEvent::BodyBurst { body, .. } | PhysEvent::BodyRecovered { body, .. } => {
                        if self.guidance.guided.iter().any(|g| g.body == *body) { self.guidance.remove(*body); }
                        if has_carrier(&self.sensors, *body) { self.sensors.remove_carrier(*body); }
                        self.ai.air.aircraft.retain(|a| a.body != *body);
                    }
                    PhysEvent::VesselSpawned { vessel, .. } => {
                        if self.ships.get(*vessel).is_some() && !has_carrier(&self.sensors, *vessel) {
                            self.sensors.add_carrier(&self.world, *vessel, &ctx);
                        }
                        let group = self.schedule.spawned.iter().find(|(_, e)| **e == *vessel).and_then(|(i, _)| self.config.vessels.get(*i as usize)).and_then(|vi| vi.group);
                        if let Some(g) = group {
                            if let Some(gs) = self.ai.groups.iter_mut().find(|gs| gs.id == g) {
                                if let Err(i) = gs.members.binary_search(vessel) { gs.members.insert(i, *vessel); }
                            }
                        }
                        if pvp { need_exterior.insert(*vessel); }
                    }
                    PhysEvent::Split { vessel, piece, cells } => {
                        if pvp {
                            let piece_ix = self.interest.exterior.get(vessel).map(|parent| {
                                let mut member = sim_core::bits::BitSet::new(parent.solid.len);
                                for c in cells { member.set(c.index(), true); }
                                parent.restricted(&|i| member.get(i))
                            });
                            if let Some(ix) = piece_ix {
                                self.interest.exterior.insert(*piece, ix);
                            } else {
                                need_exterior.insert(*vessel);
                                need_exterior.insert(*piece);
                            }
                        }
                    }
                    PhysEvent::VesselLost { vessel, by, .. } => {
                        if has_carrier(&self.sensors, *vessel) { self.sensors.remove_carrier(*vessel); }
                        if let Some(ship) = self.ships.get_mut(*vessel) {
                            let held = core::mem::take(&mut ship.control.held);
                            ship.control.pending.clear();
                            if let Some(p) = ship.player { lost_players.push((p, *vessel, *by, held)); }
                        }
                        for g in &mut self.ai.groups {
                            g.members.retain(|m| m != vessel);
                            if g.leader == Some(*vessel) { g.leader = None; }
                        }
                    }
                    PhysEvent::VesselDespawned { vessel } | PhysEvent::WreckDespawned { wreck: vessel } => {
                        if has_carrier(&self.sensors, *vessel) { self.sensors.remove_carrier(*vessel); }
                        if self.ships.get(*vessel).map(|s| s.player.is_none()).unwrap_or(false) { self.ships.remove(*vessel); }
                        for g in &mut self.ai.groups {
                            g.members.retain(|m| m != vessel);
                            if g.leader == Some(*vessel) { g.leader = None; }
                        }
                    }
                    PhysEvent::StatusChanged { vessel, status } => {
                        if matches!(status, VesselStatus::Lost(_) | VesselStatus::Wreck) && has_carrier(&self.sensors, *vessel) { self.sensors.remove_carrier(*vessel); }
                    }
                    PhysEvent::CellsDamaged { vessel, attacker, cause, .. } => {
                        // Weapon damage puts a hull under fire; its own fires, a collision or a grounding do not
                        // (the surrender rule's "under fire", L537).
                        use sim_core::damage::DamageCause as D;
                        if matches!(cause, D::Shell | D::Missile | D::Torpedo | D::Mine | D::DepthCharge) { under_fire.insert(*vessel); }
                        note(&self.world, *vessel, *attacker, &mut fired_on);
                        if pvp && !self.interest.exterior.contains_key(vessel) { need_exterior.insert(*vessel); }
                    }
                    PhysEvent::ShellImpact { vessel, shooter, .. } => {
                        under_fire.insert(*vessel);
                        note(&self.world, *vessel, *shooter, &mut fired_on);
                    }
                    PhysEvent::BodyImpact { body, vessel, .. } => {
                        under_fire.insert(*vessel);
                        let l = impact_launcher.get(body).copied().unwrap_or(EntityId::NONE);
                        note(&self.world, *vessel, l, &mut fired_on);
                    }
                    PhysEvent::Splash { pos, shooter, .. } => {
                        let near: Vec<EntityId> = self.world.vessels().filter(|(_, v)| matches!(v.status, VesselStatus::Active | VesselStatus::Disabled) && {
                            let d = v.kin.pose.pos - *pos;
                            d.x * d.x + d.z * d.z <= NEAR_MISS_M * NEAR_MISS_M
                        }).map(|(_, v)| v.id).collect();
                        for v in near { under_fire.insert(v); note(&self.world, v, *shooter, &mut fired_on); }
                    }
                    PhysEvent::Debris { vessel, .. } => { if pvp && !self.interest.exterior.contains_key(vessel) { need_exterior.insert(*vessel); } }
                    _ => {}
                }
            }
            for (side, shooter) in &fired_on { crate::ident::stance::note_fired_on(&mut self.pictures, *side, *shooter); }

            crate::objectives::ingest(&mut self.objectives, &events, &self.world, &self.ships, &self.pictures, &self.config, &ctx);

            let open: Vec<bool> = self.shot_log.iter().map(|e| e.hit.is_none()).collect();
            crate::input::resolve_shots(&mut self.shot_log, &events);
            for (i, e) in self.shot_log.iter().enumerate() {
                if open.get(i).copied().unwrap_or(true) && e.hit.is_some() { shot_log_changed.push(*e); }
            }

            for v in &under_fire {
                if let Some(c) = self.ships.get_mut(*v).and_then(|s| s.captain.as_mut()) { crate::ai::unit::perceive(c, &[], true, tick); }
            }

            if let Some(op) = self.config.operation.clone() {
                for i in 0..self.ai.aces.len() {
                    let (ace_id, vessel) = (self.ai.aces[i].ace.clone(), self.ai.aces[i].vessel);
                    let Some(side) = self.world.locate(vessel).map(|l| l.2) else { continue };
                    let Some(picture) = self.pictures.side(side) else { continue };
                    let Some(def) = self.deps.campaign.aces.get(&ace_id) else { continue };
                    let facts = crate::ai::ace::facts(&self.ai.aces[i], &self.world, picture, &events);
                    for (cue, text) in crate::ai::ace::lines(&mut self.ai.aces[i], def, &op, &facts) { ace_lines.push((ace_id.clone(), vessel, side, cue, text)); }
                }
            }
        }

        for e in shot_log_changed { self.emit(Audience::Host, Lane::Log, SimEvent::ShotLogged(e)); }
        for (ace, vessel, side, cue, text) in ace_lines {
            let mask = allied_of(side);
            let mut groups: Vec<(Option<TrackId>, SideMask)> = Vec::new();
            for si in 0..allied.len() {
                let s = SideId(si as u8);
                if mask.has(s) { continue; }
                let from = self.pictures.side(s).and_then(|p| p.track_of(vessel)).map(|t| t.id);
                match groups.iter_mut().find(|g| g.0 == from) { Some(g) => g.1 = g.1.with(s), None => groups.push((from, SideMask::only(s))) }
            }
            for (from, m) in groups { self.emit(Audience::Sides(m), Lane::Reliable, SimEvent::AceMessage { ace: ace.clone(), cue: cue.clone(), text: text.clone(), from }); }
        }
        for (p, vessel, by, held) in lost_players {
            for role in held { self.emit(Audience::player(p), Lane::Reliable, SimEvent::ControlChanged { vessel, role, holder: ControlHolder::Officer }); }
            let killer = (!by.is_none()).then_some(by);
            self.emit(Audience::player(p), Lane::Reliable, SimEvent::KillCam { victim: vessel, killer, from_tick: tick.saturating_sub(KILL_CAM_TICKS), to_tick: tick });
        }

        for v in need_exterior { self.ensure_exterior(v); }
        let hides = self.refresh_interest();

        // Route in emission order, then fold what routing needs to remember.
        for ev in &events {
            let routed = route_phys(ev, &self.interest, &self.world, pvp, tick);
            match ev {
                PhysEvent::ProjectileSpawned(sp) => {
                    let mut m = SideMask::NONE;
                    for o in &routed { if let Audience::Sides(s) = o.audience { m = m.union(s); } }
                    self.interest.projectile_audience.insert(sp.id.0, m);
                }
                PhysEvent::ShellImpact { projectile, .. } | PhysEvent::Splash { projectile, .. } => { self.interest.projectile_audience.remove(&projectile.0); }
                PhysEvent::CellsDamaged { vessel, changes, .. } => { if let Some(ix) = self.interest.exterior.get_mut(vessel) { ix.absorb(changes); } }
                PhysEvent::Split { vessel, cells, .. } | PhysEvent::Debris { vessel, cells, .. } => { if let Some(ix) = self.interest.exterior.get_mut(vessel) { ix.absorb_removed(cells); } }
                PhysEvent::VesselDespawned { vessel } | PhysEvent::WreckDespawned { wreck: vessel } => { self.interest.forget(*vessel); }
                _ => {}
            }
            self.out.extend(routed);
        }
        self.emit_hides(&hides);
        self.interest.clear_just_hidden();

        if !self.interest.projectile_audience.is_empty() {
            let live: BTreeSet<u32> = self.world.projectiles().id.iter().map(|p| p.0).collect();
            self.interest.projectile_audience.retain(|id, _| live.contains(id));
        }
    }

    /// Queue an event. Real code.
    pub(crate) fn emit(&mut self, audience: Audience, lane: Lane, event: SimEvent) { let tick = self.world.tick().0; self.out.push(OutEvent { tick, audience, lane, event }); }

    /// Recompute the drawable sets and emit `EntityRevealed` for everything a side may now draw that it has not
    /// been told about (one event per distinct intro, ascending entity). Returns the pending hides, which the
    /// caller emits after this tick's routing. Idempotent within a tick.
    pub(crate) fn refresh_interest(&mut self) -> Vec<super::interest::InterestChange> {
        let allied = allied_masks(&self.config);
        {
            let ctx = sim_ctx!(self);
            let env = crate::sensors::SenseEnv { ctx: &ctx, world: &self.world, clutter: crate::sensors::clutter_for(self.world.weather(), &ctx.tuning.sensors), night: self.config.time_of_day == TimeOfDay::Night };
            self.interest.update(&self.world, &self.sensors, &self.sigs, &env, &allied);
        }
        let (reveals, hides) = self.interest.announce();
        if reveals.is_empty() { return hides; }
        let pvp = is_pvp(self.config.mode);
        let mut by_entity: BTreeMap<EntityId, Vec<SideId>> = BTreeMap::new();
        for r in &reveals { by_entity.entry(r.entity).or_default().push(r.side); }
        if pvp {
            let need: Vec<EntityId> = by_entity.keys().copied().filter(|e| self.world.vessel(*e).is_some() && !self.interest.exterior.contains_key(e)).collect();
            for e in need { self.ensure_exterior(e); }
        }
        for (entity, sides) in by_entity {
            let mut groups: Vec<(crate::protocol::config::EntityIntro, SideMask)> = Vec::new();
            for s in sides {
                let Some(intro) = super::snapshot::entity_intro(self, entity, s) else { continue };
                match groups.iter_mut().find(|g| g.0 == intro) { Some(g) => g.1 = g.1.with(s), None => groups.push((intro, SideMask::only(s))) }
            }
            for (intro, m) in groups { self.emit(Audience::Sides(m), Lane::Reliable, SimEvent::EntityRevealed(Box::new(intro))); }
        }
        hides
    }

    fn emit_hides(&mut self, hides: &[super::interest::InterestChange]) {
        let mut by_entity: BTreeMap<EntityId, SideMask> = BTreeMap::new();
        for h in hides { let m = by_entity.entry(h.entity).or_insert(SideMask::NONE); *m = m.with(h.side); }
        for (entity, m) in by_entity { self.emit(Audience::Sides(m), Lane::Reliable, SimEvent::EntityHidden { entity }); }
    }

    /// Make sure a vessel or wreck has a PvP exterior index, building it from its design and bringing it up to
    /// date with the damage and removals it has taken so far. Leaves none when the design cannot be found
    /// (routing then sends the other team no cell data for it: fail closed).
    pub(crate) fn ensure_exterior(&mut self, vessel: EntityId) {
        if self.interest.exterior.contains_key(&vessel) { return; }
        let Some(ix) = current_exterior(self, vessel) else { return };
        if let Some(v) = self.world.vessel(vessel) {
            self.interest.exterior_designs.entry(v.blueprint).or_insert_with(|| ix.1.clone());
        }
        self.interest.exterior.insert(vessel, ix.0);
    }
}

/// Station and orbit radius for a newly spawned aircraft: the matching `BodyInit`'s group orders (air patrol
/// or guard marker), else the launching ship's group around the launcher, else an orbit around the spawn
/// point at half the combat radius (capped at 20 km).
fn air_station(config: &SortieConfig, ships: &crate::ship::ShipTable, world: &World, def: &MunitionDef, side: SideId, launcher: EntityId, pos: DVec3) -> (Option<GroupId>, DVec2, f64) {
    let default_radius = (0.5 * def.range_m).clamp(1_000.0, 20_000.0);
    let marker = |name: &str| config.map.markers.iter().find(|m| m.name == name).map(|m| DVec2::new(m.pos_m[0], m.pos_m[1]));
    let init_group = config.bodies.iter().find(|b| b.munition == def.id && b.side == side && { let d = b.pos - pos; d.x * d.x + d.z * d.z < 2_500.0 }).and_then(|b| b.group);
    let group = init_group.or_else(|| ships.get(launcher).and_then(|s| s.captain.as_ref()).and_then(|c| c.group));
    if let Some(g) = group.and_then(|g| config.groups.iter().find(|gi| gi.id == g)) {
        use crate::protocol::config::GroupOrders;
        match &g.orders {
            GroupOrders::AirPatrol { marker: m, radius_m } | GroupOrders::Guard { marker: m, radius_m } => {
                if let Some(p) = marker(m) { return (Some(g.id), p, radius_m.max(500.0)); }
            }
            _ => {}
        }
    }
    let centre = world.locate(launcher).filter(|_| ships.get(launcher).is_some()).map(|l| l.0).unwrap_or(pos);
    (group, DVec2::new(centre.x, centre.z), default_radius)
}

/// The blueprint of a vessel: inline by hash, else through the config entry it spawned from (wreck pieces
/// through their parent).
fn find_blueprint(sim: &Sim, vessel: EntityId, hash: ContentHash) -> Option<content::blueprint::Blueprint> {
    if let Some(b) = sim.config.blueprints.iter().find(|b| b.header.hash == hash) { return Some(b.clone()); }
    let mut e = vessel;
    for _ in 0..4 {
        if let Some((i, _)) = sim.schedule.spawned.iter().find(|(_, x)| **x == e) {
            use crate::protocol::config::BlueprintRef;
            return match &sim.config.vessels.get(*i as usize)?.blueprint {
                BlueprintRef::Stock(path) => content::campaign::load::stock_blueprint(path).ok(),
                BlueprintRef::Inline(h) => sim.config.blueprints.iter().find(|b| b.header.hash == *h).cloned(),
            };
        }
        e = sim.world.vessel(e)?.parent?;
    }
    None
}

/// (current index of the vessel, pristine design index). Immutable so `entity_intro` can use it when an index
/// is missing (after a keyframe restore).
pub(crate) fn current_exterior(sim: &Sim, vessel: EntityId) -> Option<(ExteriorIndex, ExteriorIndex)> {
    let v = sim.world.vessel(vessel)?;
    let base = match sim.interest.exterior_designs.get(&v.blueprint) {
        Some(ix) => ix.clone(),
        None => {
            let prefab = sim.deps.designs.prefab(v.blueprint)
                .or_else(|| sim.ships.get(vessel).map(|s| s.design.prefab.clone()))
                .or_else(|| v.parent.and_then(|p| sim.ships.get(p)).map(|s| s.design.prefab.clone()))?;
            let bp = find_blueprint(sim, vessel, v.blueprint)?;
            super::interest::exterior_index(&bp, &prefab, &sim.deps.catalogue)
        }
    };
    let mut ix = base.clone();
    let n = ix.solid.len.min(v.grid.damage.len());
    for i in 0..n {
        let b = v.grid.damage[i];
        if ix.solid.get(i) && b > 0 && b < content::cell::BUCKET_DESTROYED && !ix.cells.get(i) { ix.hidden.insert(i as u32, b); }
    }
    let grid = &v.grid;
    let live = |i: usize| i < grid.damage.len() && grid.damage[i] < content::cell::BUCKET_DESTROYED && grid.is_solid(grid.pos(CellIndex(i as u32)));
    let ix = ix.restricted(&live);
    Some((ix, base))
}

/// Decide who may receive a physical event and in what form. Returns zero or more events with disjoint
/// audiences (more than one when content differs by audience). Rules, applied per recipient side:
/// - own-side and allied subjects: the event in full;
/// - any other subject: only while it is in the side's drawable set (`InterestState`);
/// - scrub: every entity id in the event that the recipient side cannot draw (`shooter`, `attacker`, `by`,
///   `launcher`, `owner`, `layer`) is replaced by `EntityId::NONE`;
/// - `CellsDamaged` is cut to exterior cells for the other team in PvP (`interest::exterior_changes`);
/// - `ProjectileSpawned` and `MountFired` go only to sides that can draw the shooter. A side that cannot
///   draw the shooter gets no spawn (its origin would reveal an undetected unit, L529); it still gets the
///   `ShellImpact` on any hull it can draw and a `Splash` inside an own-side unit's visual range, with the
///   shooter scrubbed, so the client shows the hit without the shell in flight;
/// - `BodySpawned` goes to the launching side, and to others only when the body itself is drawable;
/// - `WeatherChanged` goes to all.
/// OPEN QUESTION for the owner (recorded in the contract notes): L926 gives every shell a spawn event, L529
/// forbids revealing undetected objects. The rule above takes L529. The alternative is a late spawn (current
/// position and velocity) when a shell enters own-side visual range, which lets a client back-trace the arc.
/// Unit: `sys-sim-events`.
pub fn route_phys(event: &PhysEvent, interest: &super::interest::InterestState, world: &sim_core::World, pvp: bool, tick: u64) -> Vec<OutEvent> {
    let side_of = |e: EntityId| if e.is_none() { None } else { world.locate(e).map(|l| l.2) };
    let weather = world.weather();
    let tuning = &world.tuning().sensors;
    let point_visible = |s: SideId, pos: DVec3, area: f64, top: f64| interest.point_visible(s, pos, area, top, weather, tuning);
    route_core(event, &Router { interest, side_of: &side_of, point_visible: &point_visible, pvp }, tick)
}

/// What routing needs to know, as closures so the rules can be tested without a `World`.
struct Router<'a> {
    interest: &'a InterestState,
    side_of: &'a dyn Fn(EntityId) -> Option<SideId>,
    /// (side, position, area m^2, top m): a point event is inside an eye's visual range.
    point_visible: &'a dyn Fn(SideId, DVec3, f64, f64) -> bool,
    pvp: bool,
}

impl Router<'_> {
    fn own(&self, s: SideId, e: EntityId) -> bool { !e.is_none() && (self.side_of)(e).map(|x| self.interest.allied_of(s).has(x)).unwrap_or(false) }
    fn own_side(&self, s: SideId, side: SideId) -> bool { self.interest.allied_of(s).has(side) }
    fn sees(&self, s: SideId, e: EntityId) -> bool { !e.is_none() && (self.own(s, e) || self.interest.routable(s, e)) }
    fn scrub(&self, s: SideId, e: EntityId) -> EntityId { if self.sees(s, e) { e } else { EntityId::NONE } }
    /// The other team in PvP sees only exterior cells of this hull.
    fn exterior_cut(&self, s: SideId, vessel: EntityId) -> bool { self.pvp && !self.own(s, vessel) }
    fn spawn_audience(&self, s: SideId, projectile: u32) -> bool { self.interest.projectile_audience.get(&projectile).map(|m| m.has(s)).unwrap_or(false) }
    fn cut_cells(&self, s: SideId, vessel: EntityId, cells: &[CellIndex]) -> Vec<CellIndex> {
        if !self.exterior_cut(s, vessel) { return cells.to_vec(); }
        match self.interest.exterior.get(&vessel) { Some(ix) => cells.iter().copied().filter(|c| ix.knows(*c)).collect(), None => Vec::new() }
    }
    /// A part slot of a hull the side may not know the layout of.
    fn cut_slot(&self, s: SideId, vessel: EntityId, slot: PartSlot) -> PartSlot {
        if self.own(s, vessel) { return slot; }
        if !self.sees(s, vessel) { return PartSlot(u16::MAX); }
        if self.pvp && !self.interest.exterior.get(&vessel).map(|ix| ix.parts.binary_search(&slot).is_ok()).unwrap_or(false) { return PartSlot(u16::MAX); }
        slot
    }

    /// The form of `ev` side `s` receives, if any.
    fn view(&self, ev: &PhysEvent, s: SideId) -> Option<PhysEvent> {
        use PhysEvent as P;
        let keep = |b: bool| if b { Some(ev.clone()) } else { None };
        match ev {
            P::VesselSpawned { vessel, side } => keep(self.own_side(s, *side) || self.sees(s, *vessel)),
            P::VesselDespawned { vessel } => keep(self.sees(s, *vessel)),
            P::MountFired { vessel, slot, rounds } => self.sees(s, *vessel).then(|| P::MountFired { vessel: *vessel, slot: self.cut_slot(s, *vessel, *slot), rounds: *rounds }),
            P::ProjectileSpawned(sp) => {
                if !self.sees(s, sp.shooter) { return None; }
                let mut sp = *sp;
                sp.mount = self.cut_slot(s, sp.shooter, sp.mount);
                Some(P::ProjectileSpawned(sp))
            }
            P::BodySpawned { body, kind, side, launcher, slot, pos, vel } => (self.own_side(s, *side) || self.sees(s, *body))
                .then(|| P::BodySpawned { body: *body, kind: *kind, side: *side, launcher: self.scrub(s, *launcher), slot: self.cut_slot(s, *launcher, *slot), pos: *pos, vel: *vel }),
            P::DecoyDeployed { decoy, kind, owner, pos } => (self.own(s, *decoy) || self.own(s, *owner) || self.sees(s, *decoy))
                .then(|| P::DecoyDeployed { decoy: *decoy, kind: *kind, owner: self.scrub(s, *owner), pos: *pos }),
            P::DecoyExpired { decoy } => keep(self.sees(s, *decoy)),
            P::MineLaid { mine, layer, pos } => (self.own(s, *mine) || self.own(s, *layer) || self.sees(s, *mine))
                .then(|| P::MineLaid { mine: *mine, layer: self.scrub(s, *layer), pos: *pos }),
            P::Splash { projectile, pos, calibre_mm, shooter } => {
                let area = splash_area_m2(*calibre_mm);
                (self.spawn_audience(s, projectile.0) || self.own(s, *shooter) || (self.point_visible)(s, *pos, area, splash_top_m(area)))
                    .then(|| P::Splash { projectile: *projectile, pos: *pos, calibre_mm: *calibre_mm, shooter: self.scrub(s, *shooter) })
            }
            P::ShellImpact { projectile, vessel, cell, outcome, world_point, shooter, burst } => {
                let sv = self.sees(s, *vessel);
                (sv || self.spawn_audience(s, projectile.0) || self.own(s, *shooter)).then(|| P::ShellImpact {
                    projectile: *projectile,
                    vessel: if sv { *vessel } else { EntityId::NONE },
                    cell: if sv { *cell } else { CellIndex(0) },
                    outcome: *outcome,
                    world_point: *world_point,
                    shooter: self.scrub(s, *shooter),
                    burst: *burst,
                })
            }
            P::BodyImpact { body, vessel, cell, world_point, cause } => {
                let (sv, sb) = (self.sees(s, *vessel), self.sees(s, *body));
                (sv || sb).then(|| P::BodyImpact { body: if sb { *body } else { EntityId::NONE }, vessel: if sv { *vessel } else { EntityId::NONE }, cell: if sv { *cell } else { CellIndex(0) }, world_point: *world_point, cause: *cause })
            }
            P::BodyBurst { body, pos, underwater } => (self.sees(s, *body) || (self.point_visible)(s, *pos, BURST_AREA_M2, BURST_TOP_M))
                .then(|| P::BodyBurst { body: self.scrub(s, *body), pos: *pos, underwater: *underwater }),
            P::BodyDestroyed { body, pos, shot_down, by } => self.sees(s, *body).then(|| P::BodyDestroyed { body: *body, pos: *pos, shot_down: *shot_down, by: self.scrub(s, *by) }),
            P::BodyRecovered { body, vessel } => {
                let (sv, sb) = (self.sees(s, *vessel), self.sees(s, *body));
                (sv || sb).then(|| P::BodyRecovered { body: if sb { *body } else { EntityId::NONE }, vessel: if sv { *vessel } else { EntityId::NONE } })
            }
            P::CellsDamaged { vessel, changes, cause, attacker } => {
                if !self.sees(s, *vessel) { return None; }
                let changes = if self.exterior_cut(s, *vessel) {
                    match self.interest.exterior.get(vessel) { Some(ix) => exterior_changes(ix, changes), None => Vec::new() }
                } else { changes.clone() };
                (!changes.is_empty()).then(|| P::CellsDamaged { vessel: *vessel, changes, cause: *cause, attacker: self.scrub(s, *attacker) })
            }
            P::PartDamaged { vessel, .. } | P::Breach { vessel, .. } | P::ConduitCut { vessel, .. } | P::LinkChanged { vessel, .. }
            | P::FireStarted { vessel, .. } | P::FireOut { vessel, .. } | P::CompartmentHalfFlooded { vessel, .. }
            | P::MagazineFlooded { vessel, .. } | P::PassengersLost { vessel, .. } | P::JobDone { vessel, .. } => keep(self.own(s, *vessel)),
            P::CookOff { vessel, magazine, radius_cells, pos } => self.sees(s, *vessel)
                .then(|| P::CookOff { vessel: *vessel, magazine: if self.own(s, *vessel) { *magazine } else { PartSlot(u16::MAX) }, radius_cells: *radius_cells, pos: *pos }),
            P::Split { vessel, piece, cells } => (self.sees(s, *vessel) || self.sees(s, *piece))
                .then(|| P::Split { vessel: *vessel, piece: *piece, cells: self.cut_cells(s, *vessel, cells) }),
            P::Debris { vessel, cells, pos } => {
                if !self.sees(s, *vessel) { return None; }
                let cells = self.cut_cells(s, *vessel, cells);
                (!cells.is_empty()).then(|| P::Debris { vessel: *vessel, cells, pos: *pos })
            }
            P::StatusChanged { vessel, .. } => keep(self.sees(s, *vessel)),
            P::VesselLost { vessel, side, cause, by, tonnage_t } => (self.own_side(s, *side) || self.sees(s, *vessel) || self.own(s, *by))
                .then(|| P::VesselLost { vessel: *vessel, side: *side, cause: *cause, by: self.scrub(s, *by), tonnage_t: *tonnage_t }),
            P::WreckDespawned { wreck } => keep(self.sees(s, *wreck)),
            P::Collision { a, b, point, impulse } => (self.sees(s, *a) || self.sees(s, *b))
                .then(|| P::Collision { a: self.scrub(s, *a), b: self.scrub(s, *b), point: *point, impulse: *impulse }),
            P::Grounded { vessel, .. } => keep(self.sees(s, *vessel)),
            P::WeatherChanged { .. } => Some(ev.clone()),
        }
    }
}

fn lane_of(ev: &PhysEvent) -> Lane {
    use PhysEvent as P;
    match ev {
        P::MountFired { .. } | P::ProjectileSpawned(_) | P::BodySpawned { .. } | P::Splash { .. } | P::ShellImpact { .. }
        | P::BodyImpact { .. } | P::BodyBurst { .. } | P::BodyDestroyed { .. } => Lane::Projectile,
        P::CellsDamaged { .. } | P::Split { .. } | P::Debris { .. } => Lane::Damage,
        _ => Lane::Reliable,
    }
}

fn route_core(event: &PhysEvent, r: &Router<'_>, tick: u64) -> Vec<OutEvent> {
    let lane = lane_of(event);
    if let PhysEvent::WeatherChanged { .. } = event {
        return vec![OutEvent { tick, audience: Audience::All, lane, event: SimEvent::Phys(event.clone()) }];
    }
    let mut groups: Vec<(PhysEvent, SideMask)> = Vec::new();
    for si in 0..r.interest.side_count() {
        let s = SideId(si as u8);
        let Some(v) = r.view(event, s) else { continue };
        match groups.iter_mut().find(|g| g.0 == v) { Some(g) => g.1 = g.1.with(s), None => groups.push((v, SideMask::only(s))) }
    }
    if groups.is_empty() {
        // Nobody may see it; the host keeps the truth for the replay record.
        return vec![OutEvent { tick, audience: Audience::Host, lane, event: SimEvent::Phys(event.clone()) }];
    }
    groups.into_iter().map(|(v, m)| OutEvent { tick, audience: Audience::Sides(m), lane, event: SimEvent::Phys(v) }).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use content::cell::CellChange;
    use sim_core::damage::DamageCause;
    use sim_core::projectiles::{ProjectileId, ProjectileSpawn};

    fn e(i: u32) -> EntityId { EntityId { index: i, generation: 0 } }

    /// Two hostile sides: side 0 owns e1 and e2, side 1 owns e10 and e11. Side 0 can draw e10; side 1 draws
    /// nothing of side 0.
    fn setup() -> (InterestState, BTreeMap<EntityId, SideId>) {
        let st = InterestState {
            drawable: vec![vec![e(1), e(2), e(10)], vec![e(10), e(11)]],
            allied: vec![SideMask::only(SideId(0)), SideMask::only(SideId(1))],
            ..Default::default()
        };
        let sides: BTreeMap<EntityId, SideId> = [(e(1), SideId(0)), (e(2), SideId(0)), (e(10), SideId(1)), (e(11), SideId(1))].into_iter().collect();
        (st, sides)
    }
    fn run(ev: &PhysEvent, st: &InterestState, sides: &BTreeMap<EntityId, SideId>, pvp: bool, visible: bool) -> Vec<OutEvent> {
        let side_of = |x: EntityId| sides.get(&x).copied();
        let pv = |_: SideId, _: DVec3, _: f64, _: f64| visible;
        route_core(ev, &Router { interest: st, side_of: &side_of, point_visible: &pv, pvp }, 7)
    }
    fn spawn(shooter: EntityId) -> PhysEvent {
        PhysEvent::ProjectileSpawned(ProjectileSpawn { id: ProjectileId(5), tick: 7, origin: DVec3::ZERO, velocity: DVec3::X, calibre_mm: 76, band: content::part::weapons::CalibreBand::Auto, shooter, mount: PartSlot(3), rounds: 1, point_defence: false })
    }

    #[test]
    fn spawn_only_to_sides_that_draw_the_shooter() {
        let (st, sides) = setup();
        let out = run(&spawn(e(1)), &st, &sides, false, false);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].audience, Audience::Sides(SideMask::only(SideId(0))));
        assert_eq!(out[0].lane, Lane::Projectile);
        // Side 0 draws enemy e10: both sides get the spawn, identical content, one event.
        let out = run(&spawn(e(10)), &st, &sides, false, false);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].audience, Audience::Sides(SideMask(0b11)));
    }

    #[test]
    fn impact_on_drawn_hull_scrubs_hidden_shooter() {
        let (st, sides) = setup();
        let ev = PhysEvent::ShellImpact { projectile: ProjectileId(5), vessel: e(11), cell: CellIndex(4), outcome: sim_core::armour::HitOutcome::Penetrated, world_point: DVec3::ONE, shooter: e(2), burst: true };
        let out = run(&ev, &st, &sides, false, false);
        // Side 0 owns the shooter and sees the full event; side 1 owns the hull and gets the shooter scrubbed.
        assert_eq!(out.len(), 2);
        let s1 = out.iter().find(|o| o.audience == Audience::Sides(SideMask::only(SideId(1)))).unwrap();
        match &s1.event { SimEvent::Phys(PhysEvent::ShellImpact { shooter, vessel, .. }) => { assert!(shooter.is_none()); assert_eq!(*vessel, e(11)); } other => panic!("{other:?}") }
        let s0 = out.iter().find(|o| o.audience == Audience::Sides(SideMask::only(SideId(0)))).unwrap();
        match &s0.event { SimEvent::Phys(PhysEvent::ShellImpact { shooter, vessel, .. }) => { assert_eq!(*shooter, e(2)); assert!(vessel.is_none()); } other => panic!("{other:?}") }
    }

    #[test]
    fn splash_needs_spawn_audience_or_eyes() {
        let (mut st, sides) = setup();
        let ev = PhysEvent::Splash { projectile: ProjectileId(5), pos: DVec3::ZERO, calibre_mm: 127, shooter: e(1) };
        // Not visible to side 1 by eye and it never got the spawn.
        let out = run(&ev, &st, &sides, false, false);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].audience, Audience::Sides(SideMask::only(SideId(0))));
        // Visible by eye: side 1 also gets it, shooter scrubbed.
        let out = run(&ev, &st, &sides, false, true);
        assert_eq!(out.len(), 2);
        // Spawn audience alone also admits.
        st.projectile_audience.insert(5, SideMask::only(SideId(1)));
        assert_eq!(run(&ev, &st, &sides, false, false).len(), 2);
    }

    #[test]
    fn interior_events_stay_with_owner() {
        let (st, sides) = setup();
        let ev = PhysEvent::PartDamaged { vessel: e(10), slot: PartSlot(2), level: DamageLevel::Heavy };
        let out = run(&ev, &st, &sides, false, false);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].audience, Audience::Sides(SideMask::only(SideId(1))));
    }

    #[test]
    fn undrawable_subject_goes_to_host_only() {
        let (st, sides) = setup();
        let ev = PhysEvent::MountFired { vessel: e(99), slot: PartSlot(1), rounds: 1 };
        let out = run(&ev, &st, &sides, false, false);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].audience, Audience::Host);
    }

    #[test]
    fn pvp_damage_is_cut_and_fails_closed() {
        let (mut st, sides) = setup();
        let ev = PhysEvent::CellsDamaged { vessel: e(10), changes: vec![CellChange { cell: CellIndex(0), bucket: 3 }, CellChange { cell: CellIndex(1), bucket: 3 }], cause: DamageCause::Shell, attacker: e(1) };
        // No exterior index: the other team gets nothing.
        let out = run(&ev, &st, &sides, true, false);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].audience, Audience::Sides(SideMask::only(SideId(1))));
        // With an index where only cell 0 is exterior.
        let mut ix = ExteriorIndex { size: [2, 1, 1], ..Default::default() };
        ix.solid.set(0, true); ix.solid.set(1, true); ix.cells.set(0, true);
        st.exterior.insert(e(10), ix);
        let out = run(&ev, &st, &sides, true, false);
        assert_eq!(out.len(), 2);
        let s0 = out.iter().find(|o| o.audience == Audience::Sides(SideMask::only(SideId(0)))).unwrap();
        match &s0.event { SimEvent::Phys(PhysEvent::CellsDamaged { changes, attacker, .. }) => { assert_eq!(changes, &vec![CellChange { cell: CellIndex(0), bucket: 3 }]); assert_eq!(*attacker, e(1)); } other => panic!("{other:?}") }
        assert_eq!(out[0].lane, Lane::Damage);
    }

    #[test]
    fn kill_confirmation_without_position() {
        let (st, sides) = setup();
        // e2 (side 0) sinks a side-1 hull e12 that side 0 cannot draw.
        let mut sides = sides;
        sides.insert(e(12), SideId(1));
        let ev = PhysEvent::VesselLost { vessel: e(12), side: SideId(1), cause: Default::default(), by: e(2), tonnage_t: 300.0 };
        let out = run(&ev, &st, &sides, false, false);
        assert_eq!(out.len(), 2);
        let s1 = out.iter().find(|o| o.audience == Audience::Sides(SideMask::only(SideId(1)))).unwrap();
        match &s1.event { SimEvent::Phys(PhysEvent::VesselLost { by, .. }) => assert!(by.is_none()), other => panic!("{other:?}") }
    }

    #[test]
    fn weather_goes_to_all_and_masks_are_symmetric() {
        let (st, sides) = setup();
        let ev = PhysEvent::WeatherChanged { weather: WeatherState::default(), wave_seed: 3 };
        assert_eq!(run(&ev, &st, &sides, false, false)[0].audience, Audience::All);
        use crate::protocol::config::{Faction, SideDef};
        let mut cfg_sides = vec![
            SideDef { id: SideId(0), faction: Faction::Players, name: Default::default(), hostile_to: SideMask::only(SideId(1)), allied: SideMask::only(SideId(2)), pvp_team: None },
            SideDef { id: SideId(1), faction: Faction::Combine, name: Default::default(), hostile_to: SideMask::only(SideId(0)), allied: SideMask::NONE, pvp_team: None },
            SideDef { id: SideId(2), faction: Faction::Protected, name: Default::default(), hostile_to: SideMask::NONE, allied: SideMask::NONE, pvp_team: None },
        ];
        cfg_sides.rotate_left(1);
        let masks = allied_masks_of(&cfg_sides);
        assert_eq!(masks, vec![SideMask(0b101), SideMask(0b010), SideMask(0b101)]);
    }

    #[test]
    fn splash_area_is_bounded() {
        assert_eq!(splash_area_m2(20), 10.0);
        assert_eq!(splash_area_m2(127), 63.5);
        assert_eq!(splash_area_m2(1000), 200.0);
        assert!((splash_top_m(100.0) - 20.0).abs() < 1e-12);
    }
}
