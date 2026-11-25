//! Unit `sys-sim-setup`. Building a `Sim` from a `SortieConfig`, and player lifecycle.
//! DESIGN.md L899 (same Sim everywhere), L832-838 (join between operations, drop and rejoin within 5
//! minutes, officers keep the hull), L959 (sandbox: join any time), L943-945 (server re-validates blueprints).
//!
//! # Choices where the spec is silent
//! - Inline blueprints are network data: every one is re-canonicalised, re-hashed and validated under the
//!   sortie ruleset (player-ship limits, never enemy-class limits) before anything is built, and vessel
//!   references are rewritten to the recomputed hash. Stock blueprints ship with the build and are trusted.
//! - `VesselInit::pos` is the hull's midships point on the surface; the grid origin is placed so the hull is
//!   centred on it.
//! - Captains: AI hulls get the behaviour of their class (`EnemyClassDef::behaviour`), else one from their
//!   role (prizes sail as `Proxy`, sites are `Static`, convoys and tenders `Merchant`); civilian and protected
//!   sides run `Merchant`; splash and trial targets run `Duelist`, except splash drones (`DRONE_TAG`), which
//!   run `Proxy` captains on the orbit their group's patrol markers lay out and hold fire. A player slot
//!   whose `PlayerInit::human` is false (a bot or an empty slot) gets a `Proxy` captain and officers in
//!   command. AI hulls take the difficulty's accuracy and reaction time with their own veterancy.
//! - Luau: scripts compile only where the ruleset allows them and never in PvP. The host runs its own
//!   scripts; other players' scripts need `SimSettings::guest_scripts` and the passed guest gate
//!   (`SimDeps::guest_gate_passed`). The config does not say who hosts, so slot 0 counts as the host only in
//!   modes that always run on a player's machine (campaign, splash, tutorial, trial); in skirmish and
//!   sandbox, which a dedicated server may run with a remote client in slot 0, every player is a guest
//!   (fail closed). AI hulls compile as the host.
//! - Carried ammunition per slot (`VesselInit::ammo`) is folded into one capacity-weighted fraction, the only
//!   load the world's spawn takes.
//! - A sandbox join without a position goes to the first free spot on the player spawn markers (the side's
//!   own marker first), stepping 600 m along the marker's line until no hull lies within 500 m.
//! - A live splash edit that changes only settings, rules, graphs, scripts or tuning updates the ship in
//!   place. One that changes parts, kits, networks, paint or markings with the same hull shell (hull lines
//!   and cells) replaces the vessel at the same pose and velocity, carrying its cell damage, part health
//!   (matched by part, cell and rotation), fuel and ammunition; the player's ship gets a new entity, sent in
//!   a `PlayerJoined` event. Water in compartments and fires burning are not carried over (the world takes
//!   persistent damage and loads at spawn, nothing else). A changed shell is refused.
use super::{PlayerState, Schedule, Sim, SimDeps, SimError};
use crate::ai::unit::{behaviour_from_key, new_captain, Behaviour, CaptainState};
use crate::datalink::{LinkPicture, PartCommand};
use crate::editor::splash::{SplashLog, DRONE_TAG, PATROL_BOAT_TAG, PATROL_STRIKE_HEALTH};
use crate::prelude::*;
use crate::protocol::config::{BlueprintRef, Faction, GroupOrders, ModeRules, PlayerInit, SortieConfig, VesselInit, VesselRole};
use crate::protocol::event::{Audience, Lane, SimEvent};
use crate::ship::{Proficiency, Ship, ShipDesign, ShipOrders};
use crate::tracks::{Pictures, SidePicture};
use content::blueprint::Blueprint;
use content::campaign::{MarkerKind, SurrenderRule, MAX_MAP_M};
use content::graph::NodeKind;
use content::part::PartFunction;
use content::ruleset::Ruleset;
use content::save::HullDamage;
use sim_core::parts::PartState;
use sim_core::vessel::{Fidelity, Vessel, VesselSpawn, VesselStatus};

/// A dropped player's slot is held this long (L838).
pub const REJOIN_HOLD_S: u64 = 300;
pub const REJOIN_HOLD_TICKS: u64 = REJOIN_HOLD_S * sim_core::time::TICK_HZ as u64;
/// Laps of the orbit a splash drone's route holds (about an hour at drone speed).
const DRONE_LAPS: usize = 8;
/// A sandbox spawn spot is free when no hull is closer than this.
const SPAWN_CLEAR_M: f64 = 500.0;
/// Step between candidate spawn spots along a marker's line.
const SPAWN_STEP_M: f64 = 600.0;
const SPAWN_TRIES: usize = 16;
/// The listen host's slot in player-hosted modes.
const HOST_SLOT: PlayerSlot = PlayerSlot(0);

/// Modes that only ever run on a player's own machine, where slot 0 is that player (campaign co-op is hosted
/// by a member; splash, tutorial and trials are local). Elsewhere a dedicated server may seat a remote client
/// in slot 0, so every player counts as a guest.
fn player_hosted(mode: GameMode) -> bool { matches!(mode, GameMode::Campaign | GameMode::Splash | GameMode::Tutorial | GameMode::Trial) }

/// Hash of a stock blueprint path seen before in this process (stock files are fixed per build).
fn stock_hash(path: &str) -> Option<ContentHash> { stock_hashes().lock().unwrap_or_else(|e| e.into_inner()).get(path).copied() }
fn remember_stock_hash(path: &str, hash: ContentHash) { stock_hashes().lock().unwrap_or_else(|e| e.into_inner()).insert(path.to_string(), hash); }
fn stock_hashes() -> &'static std::sync::Mutex<BTreeMap<String, ContentHash>> {
    static HASHES: std::sync::OnceLock<std::sync::Mutex<BTreeMap<String, ContentHash>>> = std::sync::OnceLock::new();
    HASHES.get_or_init(Default::default)
}
/// Most sides and player slots the masks carry.
const MAX_SIDES: usize = 8;

impl Sim {
    /// Create the simulation: check the protocol version; resolve and validate every blueprint
    /// (`content::validate::validate_blueprint` under the config's ruleset; stats recomputed, never trusted);
    /// build missing designs; create the `World` (seed, weather, map); spawn vessels with `arrives_s == 0` in
    /// config order, then bodies; create ships, captains for AI hulls, sensors, pictures per side, the ident
    /// library, AI state, objectives; emit `SortieStarted` and the first weather event.
    /// Unit: `sys-sim-setup`.
    pub fn new(config: SortieConfig, deps: SimDeps) -> Result<Sim, SimError> {
        if config.protocol != crate::protocol::PROTOCOL_VERSION {
            return Err(SimError::Protocol { found: config.protocol, expected: crate::protocol::PROTOCOL_VERSION });
        }
        validate_config(&config)?;
        let mut config = config;
        normalize_blueprints(&mut config, &deps.catalogue)?;
        let world = World::new(sim_core::WorldConfig { seed: config.seed, weather: config.weather, map: config.map.clone(), catalogue: deps.catalogue.clone() });
        let n_sides = side_slots(&config);
        let count = |n: usize| n.min(u16::MAX as usize) as u16;
        let schedule = Schedule {
            vessels: (0..count(config.vessels.len())).filter(|&i| config.vessels[i as usize].arrives_s > 0).collect(),
            bodies: (0..count(config.bodies.len())).filter(|&i| config.bodies[i as usize].arrives_s > 0).collect(),
            weather: (0..count(config.weather_changes.len())).collect(),
            spawned: BTreeMap::new(),
        };
        let mut players: Vec<PlayerState> = config.players.iter().map(|p| PlayerState { slot: p.slot, client_id: p.client_id, name: p.name.clone(), side: p.side, vessel: None, human: p.human, connected: p.human, dropped_tick: None }).collect();
        players.sort_by_key(|p| p.slot);
        let objectives = crate::objectives::init(&config);
        let splash_log = config.splash.map(|_| SplashLog::new(0));
        let pictures = Pictures { sides: (0..n_sides).map(|i| SidePicture { side: SideId(i as u8), tracks: Vec::new(), next_id: 1 }).collect() };
        let mut sim = Sim {
            config, deps, world, ships: Default::default(), players, schedule, sigs: Default::default(), sensors: Default::default(), detections: Vec::new(), pictures,
            links: vec![LinkPicture::default(); n_sides], ident: Default::default(), guidance: Default::default(), ai: Default::default(), specops: Default::default(),
            objectives, alerts: Default::default(), ladder: Default::default(), host_load: crate::lod::HostLoad::IDLE, time: Default::default(), history: Default::default(),
            shot_log: Vec::new(), interest: Default::default(), rewind: super::keyframe::RewindBuffer::new(), pending: Vec::new(), out: Vec::new(), work: Default::default(),
            result: None, splash_log, run: Default::default(),
        };

        // Every design the sortie can field, built now so scheduled arrivals spawn without a stall.
        let refs: BTreeSet<BlueprintRef> = sim.config.vessels.iter().map(|v| v.blueprint.clone()).collect();
        for r in &refs { sim.design_for(r)?; }

        let own_classes: Vec<ClassId> = {
            let players_side = |s: SideId| sim.config.sides.iter().any(|d| d.id == s && d.faction == Faction::Players);
            let mut v: Vec<ClassId> = sim.config.vessels.iter().filter(|v| players_side(v.side)).filter_map(|v| v.class.clone()).collect();
            v.sort();
            v.dedup();
            v
        };
        sim.ident = crate::ident::IdentLibrary::build(&sim.config.intel, &own_classes, &sim.deps.campaign, &sim.deps.catalogue);

        sim.emit(Audience::All, Lane::Reliable, SimEvent::SortieStarted { seed: sim.config.seed, tick_hz: sim_core::time::TICK_HZ });
        let weather = *sim.world.weather();
        let wave_seed = sim.world.waves().seed;
        sim.emit(Audience::All, Lane::Reliable, SimEvent::Phys(sim_core::PhysEvent::WeatherChanged { weather, wave_seed }));

        for i in 0..count(sim.config.vessels.len()) {
            if sim.config.vessels[i as usize].arrives_s == 0 { sim.spawn_configured(i)?; }
        }
        {
            let ctx = make_ctx(&sim.config, &sim.deps, &sim.world);
            sim.ai = crate::ai::init(&sim.config, &sim.world, &mut sim.ships, &ctx);
        }
        for i in 0..count(sim.config.bodies.len()) {
            if sim.config.bodies[i as usize].arrives_s == 0 { sim.spawn_configured_body(i)?; }
        }
        let joined: Vec<(PlayerSlot, String, SideId, EntityId)> = sim.players.iter().filter_map(|p| p.vessel.map(|v| (p.slot, p.name.clone(), p.side, v))).collect();
        for (player, name, side, vessel) in joined { sim.emit(Audience::All, Lane::Reliable, SimEvent::PlayerJoined { player, name, side, vessel }); }
        Ok(sim)
    }

    /// Spawn one configured vessel now and register its ship, sensors and group membership. Returns its
    /// entity. Unit: `sys-sim-setup`.
    ///
    /// Calling it again for an index already spawned returns that entity and changes nothing.
    pub(crate) fn spawn_configured(&mut self, index: u16) -> Result<EntityId, SimError> {
        if let Some(e) = self.schedule.spawned.get(&index) { return Ok(*e); }
        let vi = self.config.vessels.get(index as usize).cloned().ok_or_else(|| SimError::Config(format!("no vessel {index}")))?;
        let design = self.design_for(&vi.blueprint)?;
        let player = self.config.players.iter().find(|p| p.vessel == Some(index)).cloned();
        let cat = self.deps.catalogue.clone();
        let class_def = vi.class.as_ref().and_then(|c| self.deps.campaign.classes.get(c));
        let heading = if vi.heading_rad.is_finite() { vi.heading_rad } else { 0.0 };
        let pose = hull_pose(&design.prefab, DVec2::new(vi.pos.x, vi.pos.z), heading);
        let spawn = VesselSpawn {
            prefab: design.prefab.clone(), side: vi.side, player: player.as_ref().map(|p| p.slot), class: vi.class.clone(), pose,
            speed_mps: if vi.speed_kn.is_finite() { vi.speed_kn.max(0.0) * sim_core::time::KN } else { 0.0 },
            damage: vi.damage.clone(), fuel_fraction: fraction(vi.fuel_fraction), ammo_fraction: spawn_ammo_fraction(&vi, &design, &cat),
            fidelity: Fidelity::Full, submersible: class_def.is_some_and(|d| d.submersible),
        };
        let entity = self.world.spawn_vessel(&spawn);
        self.schedule.spawned.insert(index, entity);
        self.schedule.vessels.retain(|i| *i != index);
        let ship = self.make_ship(entity, &vi, design, player.as_ref());
        self.ships.insert(ship);
        self.load_scripts(entity);
        {
            let ctx = make_ctx(&self.config, &self.deps, &self.world);
            self.sensors.add_carrier(&self.world, entity, &ctx);
        }
        if let Some(g) = vi.group {
            if let Some(gs) = self.ai.groups.iter_mut().find(|gs| gs.id == g && gs.side == vi.side) {
                if let Err(i) = gs.members.binary_search(&entity) { gs.members.insert(i, entity); }
            }
        }
        crate::objectives::note_spawn(&mut self.objectives, entity, index, &self.world, &self.ships, &self.config);
        if let Some(p) = &player {
            if let Some(ps) = self.players.iter_mut().find(|s| s.slot == p.slot) { ps.vessel = Some(entity); }
        }
        Ok(entity)
    }

    /// Spawn one configured free body (`SortieConfig::bodies`) now: pre-laid mines, aircraft, survivors.
    /// Registers its sensors and, for aircraft, the air AI. Added by `sys-sim-setup` for the schedule phase.
    pub(crate) fn spawn_configured_body(&mut self, index: u16) -> Result<EntityId, SimError> {
        let b = self.config.bodies.get(index as usize).cloned().ok_or_else(|| SimError::Config(format!("no body {index}")))?;
        let cat = self.deps.catalogue.clone();
        let kind = cat.munition_idx(&b.munition).ok_or_else(|| SimError::Config(format!("body {index}: unknown munition {}", b.munition.0)))?;
        let def = cat.munition_at(kind);
        let entity = self.world.spawn_body(kind, b.side, b.pos, b.vel);
        self.schedule.bodies.retain(|i| *i != index);
        let ctx = make_ctx(&self.config, &self.deps, &self.world);
        if (!def.sensors.is_empty() || def.seeker != content::part::weapons::SeekerKind::None) && !self.sensors.sensors.iter().any(|s| s.id.carrier == entity) {
            self.sensors.add_carrier(&self.world, entity, &ctx);
        }
        if def.kind == content::part::weapons::MunitionKind::Aircraft {
            let (station, radius) = air_station(&self.config, b.group, def.range_m, DVec2::new(b.pos.x, b.pos.z));
            crate::ai::air::add(&mut self.ai.air, entity, b.side, b.group, station, radius, &cat, kind);
        }
        Ok(entity)
    }

    /// Sandbox join (any time) and co-op rejoin into a held slot: adds or reconnects a player. A new sandbox
    /// player's blueprint is validated and spawned at a free spawn marker. Unit: `sys-sim-setup`.
    ///
    /// Outside sandbox only a rejoin into a held slot is accepted (campaign joins happen between operations).
    /// Without a blueprint or vessel the player joins as a spectator of their side.
    pub fn add_player(&mut self, player: PlayerInit, vessel: Option<VesselInit>, blueprint: Option<Blueprint>) -> Result<(), SimError> {
        if self.players.iter().any(|p| p.client_id == player.client_id && p.human && !p.connected && p.dropped_tick.is_some()) {
            return self.rejoin_player(player.client_id).map(|_| ()).ok_or_else(|| SimError::Config("the held slot has been given to the officers".into()));
        }
        if self.config.mode != GameMode::Sandbox { return Err(SimError::Config("players join between operations".into())); }
        if player.slot.index() >= MAX_SIDES { return Err(SimError::Config(format!("player slot {} out of range", player.slot.0))); }
        if self.players.iter().any(|p| p.slot == player.slot) { return Err(SimError::Config(format!("player slot {} is taken", player.slot.0))); }
        if self.players.len() >= content::ruleset::max_players(self.config.mode, true) as usize { return Err(SimError::Config("the sandbox is full".into())); }
        if !self.config.sides.iter().any(|s| s.id == player.side) { return Err(SimError::Config(format!("no side {}", player.side.0))); }

        let mut index = None;
        if blueprint.is_some() || vessel.is_some() {
            let given_pos = vessel.as_ref().filter(|v| v.pos.x.is_finite() && v.pos.z.is_finite() && v.heading_rad.is_finite()).map(|v| (v.pos, v.heading_rad));
            let (pos, heading) = given_pos.unwrap_or_else(|| self.free_spawn(player.side));
            let mut vi = vessel.unwrap_or_else(|| default_vessel(BlueprintRef::Inline(ContentHash::EMPTY), player.side));
            if let Some(bp) = blueprint {
                let c = crate::editor::splash::canonical(&bp);
                check_blueprint(&c, &self.config.settings.ruleset, &self.deps.catalogue)?;
                vi.blueprint = BlueprintRef::Inline(c.header.hash);
                if !self.config.blueprints.iter().any(|b| b.header.hash == c.header.hash) { self.config.blueprints.push(c); }
            } else if let BlueprintRef::Inline(h) = vi.blueprint {
                if !self.config.blueprints.iter().any(|b| b.header.hash == h) { return Err(SimError::Blueprint("the vessel names a design that was not sent".into())); }
            }
            vi.side = player.side;
            vi.pos = pos;
            vi.heading_rad = heading;
            vi.arrives_s = 0;
            vi.group = None;
            vi.role = VesselRole::PlayerHull;
            self.design_for(&vi.blueprint)?;
            if self.config.vessels.len() >= u16::MAX as usize { return Err(SimError::Config("too many vessels".into())); }
            self.config.vessels.push(vi);
            index = Some((self.config.vessels.len() - 1) as u16);
        }
        let init = PlayerInit { vessel: index, ..player };
        let pos = self.config.players.partition_point(|p| p.slot < init.slot);
        self.config.players.insert(pos, init.clone());
        let ps = PlayerState { slot: init.slot, client_id: init.client_id, name: init.name.clone(), side: init.side, vessel: None, human: init.human, connected: true, dropped_tick: None };
        let pos = self.players.partition_point(|p| p.slot < ps.slot);
        self.players.insert(pos, ps);
        let entity = match index {
            Some(i) => match self.spawn_configured(i) {
                Ok(e) => e,
                Err(e) => {
                    self.config.players.retain(|p| p.slot != init.slot);
                    self.players.retain(|p| p.slot != init.slot);
                    self.config.vessels.pop();
                    return Err(e);
                }
            },
            None => EntityId::NONE,
        };
        self.emit(Audience::All, Lane::Reliable, SimEvent::PlayerJoined { player: init.slot, name: init.name, side: init.side, vessel: entity });
        Ok(())
    }

    /// A player dropped: officers (a `Proxy` captain) take the hull; the slot is held for 5 minutes, after
    /// which the officers keep it to the end of the operation. Unit: `sys-sim-setup`.
    pub fn drop_player(&mut self, player: PlayerSlot) {
        let tick = self.world.tick().0;
        let Some(p) = self.players.iter_mut().find(|p| p.slot == player) else { return };
        if !p.connected { return; }
        p.connected = false;
        p.dropped_tick = Some(tick);
        let vessel = p.vessel;
        let mut officers = false;
        let mut released: Vec<(EntityId, SideId, crate::protocol::command::ControlRole)> = Vec::new();
        if let Some(ship) = vessel.and_then(|e| self.ships.get_mut(e)) {
            let afloat = self.world.vessel(ship.vessel).is_some_and(|v| matches!(v.status, VesselStatus::Active | VesselStatus::Disabled));
            for role in core::mem::take(&mut ship.control.held) { released.push((ship.vessel, ship.side, role)); }
            ship.control.pending.clear();
            ship.control.officers_in_command = true;
            if ship.captain.is_none() { ship.captain = Some(proxy_captain(&ship.orders)); }
            officers = afloat;
        }
        for (vessel, side, role) in released { self.emit(Audience::side(side), Lane::Reliable, SimEvent::ControlChanged { vessel, role, holder: ControlHolder::Officer }); }
        self.emit(Audience::All, Lane::Reliable, SimEvent::PlayerDropped { player, officers_in_command: officers });
    }

    /// Rejoin by client id inside the hold window; the hull snaps back under player command. Returns the slot.
    /// Unit: `sys-sim-setup`.
    pub fn rejoin_player(&mut self, client_id: u64) -> Option<PlayerSlot> {
        let tick = self.world.tick().0;
        let p = self.players.iter_mut().find(|p| p.client_id == client_id && p.human && !p.connected && p.dropped_tick.is_some())?;
        if tick.saturating_sub(p.dropped_tick?) > REJOIN_HOLD_TICKS { return None; }
        p.connected = true;
        p.dropped_tick = None;
        let (slot, name, side, vessel) = (p.slot, p.name.clone(), p.side, p.vessel);
        if let Some(ship) = vessel.and_then(|e| self.ships.get_mut(e)) {
            ship.captain = None;
            ship.control.officers_in_command = false;
        }
        self.emit(Audience::All, Lane::Reliable, SimEvent::PlayerJoined { player: slot, name, side, vessel: vessel.unwrap_or(EntityId::NONE) });
        Some(slot)
    }

    /// Sandbox leave: remove the player's hull and slot. Unit: `sys-sim-setup`.
    ///
    /// Outside sandbox a leave is a drop: the officers keep the hull.
    pub fn remove_player(&mut self, player: PlayerSlot) {
        if self.config.mode != GameMode::Sandbox { self.drop_player(player); return; }
        let Some(i) = self.players.iter().position(|p| p.slot == player) else { return };
        let p = self.players.remove(i);
        self.config.players.retain(|q| q.slot != player);
        if let Some(e) = p.vessel { self.retire_vessel(e); }
        self.emit(Audience::All, Lane::Reliable, SimEvent::PlayerDropped { player, officers_in_command: false });
    }

    /// Live splash edit (L216): swap the player's design for a new blueprint with the same hull shell,
    /// keeping pose, damage and stores; recompiles the graph and re-arms scripts. A changed shell is refused
    /// (the client returns to dry dock). Unit: `sys-sim-setup`.
    pub(crate) fn apply_design(&mut self, player: PlayerSlot, blueprint: &Blueprint) -> Result<(), SimError> {
        let ship = self.ships.of_player(player).ok_or_else(|| SimError::Config("the player has no ship".into()))?;
        let vessel = ship.vessel;
        let old = ship.design.clone();
        let bp = crate::editor::splash::canonical(blueprint);
        if bp.hull != old.blueprint().hull || bp.cells != old.blueprint().cells { return Err(SimError::Config("the hull shell changed: return to dry dock".into())); }
        let v = self.world.vessel(vessel).ok_or_else(|| SimError::Config("the ship is gone".into()))?;
        if !matches!(v.status, VesselStatus::Active | VesselStatus::Disabled) { return Err(SimError::Config("the ship is lost".into())); }
        check_blueprint(&bp, &self.config.settings.ruleset, &self.deps.catalogue)?;
        let ruleset = self.compile_ruleset(Some(player));
        let cat = self.deps.catalogue.clone();
        let ob = old.blueprint();
        let in_place = bp.parts == ob.parts && bp.kits == ob.kits && bp.embedded_kits == ob.embedded_kits && bp.networks == ob.networks && bp.paint == ob.paint && bp.markings == ob.markings;
        let design = if in_place { crate::ship::design::rebuild(&bp, old.prefab.clone(), &cat, &ruleset, &old) } else {
            let prefab = Arc::new(sim_core::vessel::build_prefab(&bp, &cat));
            crate::ship::design::rebuild(&bp, prefab, &cat, &ruleset, &old)
        };
        let design = Arc::new(design);
        if !self.config.blueprints.iter().any(|b| b.header.hash == bp.header.hash) { self.config.blueprints.push(bp.clone()); }
        self.deps.designs.insert(design.clone());
        let index = self.schedule.spawned.iter().find(|(_, e)| **e == vessel).map(|(i, _)| *i);
        if in_place {
            let tuning = &cat.tuning;
            let Some(ship) = self.ships.get_mut(vessel) else { return Err(SimError::Config("the ship is gone".into())) };
            ship.settings = bp.officers.clone();
            ship.rules = bp.rules.clone();
            ship.graph = design.graph.clone();
            ship.graph_state = crate::graph::vm::init_state(&ship.graph, Some(&ship.graph_state));
            crate::graph::vm::rearm(&mut ship.graph_state);
            ship.doctrine = crate::doctrine::resolve(&ship.fleet_doctrine, &ship.settings, &design.mounts, tuning);
            ship.design = design;
            self.load_scripts(vessel);
            if let Some(i) = index { if let Some(vi) = self.config.vessels.get_mut(i as usize) { vi.blueprint = BlueprintRef::Inline(bp.header.hash); } }
            return Ok(());
        }

        // Physical change: replace the vessel at the same pose, carrying damage and stores.
        let kin = v.kin;
        let damage = carried_damage(v, ob, &bp);
        let (fuel, ammo) = stores(v, &cat);
        let (side, class) = (v.side, v.class.clone());
        let submersible = v.submersible;
        let Some(old_ship) = self.ships.remove(vessel) else { return Err(SimError::Config("the ship is gone".into())) };
        self.world.despawn(vessel);
        self.sensors.remove_carrier(vessel);
        self.deps.scripts.unload(vessel);
        self.interest.forget(vessel);
        let spawn = VesselSpawn { prefab: design.prefab.clone(), side, player: Some(player), class, pose: kin.pose, speed_mps: 0.0, damage: Some(damage), fuel_fraction: fuel, ammo_fraction: ammo, fidelity: Fidelity::Full, submersible };
        let entity = self.world.spawn_vessel(&spawn);
        self.world.teleport(entity, &kin);
        let mut ship = self.make_ship(entity, &VesselInit { tag: old_ship.tag.clone(), ..default_vessel(BlueprintRef::Inline(bp.header.hash), side) }, design, self.config.players.iter().find(|p| p.slot == player));
        ship.orders = old_ship.orders;
        ship.orders.launches.clear();
        ship.orders.decoys.clear();
        ship.control = old_ship.control;
        ship.control.held.retain(|r| matches!(r, crate::protocol::command::ControlRole::Helm | crate::protocol::command::ControlRole::Sensors));
        ship.control.pending.clear();
        ship.proficiency = old_ship.proficiency;
        ship.captain = old_ship.captain;
        ship.graph_state = crate::graph::vm::init_state(&ship.graph, Some(&old_ship.graph_state));
        crate::graph::vm::rearm(&mut ship.graph_state);
        self.ships.insert(ship);
        self.load_scripts(entity);
        {
            let ctx = make_ctx(&self.config, &self.deps, &self.world);
            self.sensors.add_carrier(&self.world, entity, &ctx);
        }
        for g in &mut self.ai.groups {
            if let Some(k) = g.members.iter().position(|m| *m == vessel) { g.members.remove(k); if let Err(i) = g.members.binary_search(&entity) { g.members.insert(i, entity); } }
            if g.leader == Some(vessel) { g.leader = Some(entity); }
        }
        if let Some(i) = index {
            self.schedule.spawned.insert(i, entity);
            if let Some(vi) = self.config.vessels.get_mut(i as usize) { vi.blueprint = BlueprintRef::Inline(bp.header.hash); }
            crate::objectives::note_spawn(&mut self.objectives, entity, i, &self.world, &self.ships, &self.config);
        }
        let mut joined = None;
        if let Some(ps) = self.players.iter_mut().find(|p| p.slot == player) { ps.vessel = Some(entity); joined = Some((ps.name.clone(), ps.side)); }
        if let Some((name, side)) = joined { self.emit(Audience::All, Lane::Reliable, SimEvent::PlayerJoined { player, name, side, vessel: entity }); }
        Ok(())
    }

    // ---- helpers ----

    /// The design of a blueprint reference, built and cached on first use. The cache is consulted before any
    /// blueprint is cloned or hashed, so scheduled arrivals of a known class cost a lookup.
    pub(crate) fn design_for(&mut self, r: &BlueprintRef) -> Result<Arc<ShipDesign>, SimError> {
        let (bp, hash) = match r {
            BlueprintRef::Inline(h) => {
                if let Some(d) = self.deps.designs.get(*h) { return Ok(d); }
                let bp = self.config.blueprints.iter().find(|b| b.header.hash == *h).cloned().ok_or_else(|| SimError::Blueprint("a vessel names a design that was not sent".into()))?;
                (bp, *h)
            }
            BlueprintRef::Stock(path) => {
                if let Some(d) = stock_hash(path).and_then(|h| self.deps.designs.get(h)) { return Ok(d); }
                // The loader canonicalises stock files and sets the header hash to the recomputed one.
                let bp = content::campaign::load::stock_blueprint(path).map_err(|e| SimError::Blueprint(format!("{path}: {e}")))?;
                remember_stock_hash(path, bp.header.hash);
                let h = bp.header.hash;
                (bp, h)
            }
        };
        if let Some(d) = self.deps.designs.get(hash) { return Ok(d); }
        let ruleset = self.compile_ruleset(None);
        let cat = self.deps.catalogue.clone();
        let prefab = Arc::new(sim_core::vessel::build_prefab(&bp, &cat));
        let d = Arc::new(ShipDesign::build(&bp, prefab, &cat, &ruleset));
        self.deps.designs.insert(d.clone());
        if d.hash != hash { self.deps.designs.designs.insert(hash, d.clone()); }
        Ok(d)
    }

    /// The ruleset a ship's graph compiles under: the sortie's, with Luau only where it may run (see the
    /// module doc). `player` is the commanding player; `None` for AI hulls.
    pub(crate) fn compile_ruleset(&self, player: Option<PlayerSlot>) -> Ruleset {
        let mut r = self.config.settings.ruleset.clone();
        let pvp = super::events::is_pvp(self.config.mode);
        let host = match player { None => true, Some(p) => p == HOST_SLOT && player_hosted(self.config.mode) };
        let guests = self.config.settings.guest_scripts && self.deps.guest_gate_passed;
        r.luau = r.luau && !pvp && (host || guests);
        r
    }

    fn make_ship(&self, entity: EntityId, vi: &VesselInit, design: Arc<ShipDesign>, player: Option<&PlayerInit>) -> Ship {
        let cat = &*self.deps.catalogue;
        let faction = side_faction(&self.config, vi.side);
        let class_def = vi.class.as_ref().and_then(|c| self.deps.campaign.classes.get(c));
        let ruleset = self.compile_ruleset(player.map(|p| p.slot));
        let mut settings = design.settings.clone();
        if let Some(e) = vi.emcon { settings.emcon = e; }
        let rules = design.rules.clone();
        let graph = if design.compiled_for(&ruleset) && settings == design.settings { design.graph.clone() } else { design.recompile(&settings, &rules, &design.overrides, cat, &ruleset) };
        let graph_state = crate::graph::vm::init_state(&graph, None);
        let doctrine = crate::doctrine::resolve(&[], &settings, &design.mounts, &cat.tuning);
        let drone = faction == Some(Faction::Target) && vi.tag == DRONE_TAG;
        let captain: Option<CaptainState> = match player {
            Some(p) if p.human => None,
            Some(_) => Some(new_captain(Behaviour::Proxy, None, None, SurrenderRule::Never)),
            None => {
                let behaviour = match faction {
                    Some(Faction::Civilian | Faction::Protected) => Behaviour::Merchant,
                    Some(Faction::Target) => if drone { Behaviour::Proxy } else { Behaviour::Duelist },
                    _ => match (class_def, vi.role) {
                        // A site (depot, radar station) is the objective's fixed point whatever hull class
                        // stands in for it: it holds where it was placed instead of sailing as a merchant.
                        (_, VesselRole::Site) => Behaviour::Static,
                        (Some(d), _) => behaviour_from_key(&d.behaviour),
                        (None, VesselRole::Prize | VesselRole::PlayerHull) => Behaviour::Proxy,
                        (None, VesselRole::Convoy | VesselRole::Tender) => Behaviour::Merchant,
                        (None, _) => Behaviour::Combatant,
                    },
                };
                // The Combine surrender rule (L537): Combine and civilian hulls strike by their class's rule (the
                // campaign's fraction unless the class sets its own or never strikes), a class-less one by the
                // campaign's. The splash patrol boat is one instance of the same rule at its own fraction, so a small
                // ship beats it in a minute or two of gunnery rather than by grinding its whole hull away. Other
                // target hulls (drones, trial duelists), protected hulls (a defector) and sites never strike.
                let default_strike = self.deps.campaign.rules.strike_cells_lost;
                // DECISION: a site (the depot or radar station a strike operation must destroy) has no colours to
                // strike; it is the objective's fixed point and counts only when disabled or lost by damage.
                let rule = match faction {
                    _ if vi.role == VesselRole::Site => SurrenderRule::Never,
                    Some(Faction::Target) if vi.tag == PATROL_BOAT_TAG => SurrenderRule::Strike { cells_lost: 1.0 - PATROL_STRIKE_HEALTH },
                    Some(Faction::Combine | Faction::Civilian) => class_def.map(|d| d.surrender).unwrap_or_default(),
                    _ => SurrenderRule::Never,
                };
                let surrender = match rule.cells_lost(default_strike) { Some(cells_lost) => SurrenderRule::Strike { cells_lost }, None => SurrenderRule::Never };
                let mut c = new_captain(behaviour, class_def.map(|d| d.role), vi.group, surrender);
                if c.behaviour == Behaviour::Proxy { c.route = proxy_route(&self.config, vi); }
                Some(c)
            }
        };
        let player_hull = player.is_some() || vi.role == VesselRole::PlayerHull;
        let speed = if vi.speed_kn.is_finite() { vi.speed_kn.max(0.0) } else { 0.0 };
        let orders = ShipOrders {
            speed_kn: if player_hull || speed > 0.0 { Some(speed) } else { None },
            weapons_tight: drone || matches!(faction, Some(Faction::Civilian | Faction::Protected)),
            ..Default::default()
        };
        let proficiency = if player.is_none() && captain.as_ref().is_some_and(|c| c.behaviour != Behaviour::Proxy) {
            let d = self.config.settings.difficulty.clamped();
            Proficiency { aim_error_mult: d.ai_accuracy, reaction_s: d.ai_reaction_s, veterancy: if vi.veterancy.is_finite() { vi.veterancy.clamp(0.0, 1.0) } else { 0.0 } }
        } else { Proficiency::default() };
        let slots = design.prefab.parts.iter().map(|p| p.slot.index() + 1).max().unwrap_or(0);
        Ship {
            vessel: entity, side: vi.side, player: player.map(|p| p.slot), design, settings, rules, graph, graph_state, fleet_doctrine: Vec::new(), doctrine, orders,
            control: crate::input::ControlState { officers_in_command: player.is_some_and(|p| !p.human), ..Default::default() },
            stages: Default::default(), offboard: Default::default(), proficiency, captain, sinks: Default::default(), part_command: vec![PartCommand::Networked; slots],
            board: Default::default(), detections: Vec::new(), tag: vi.tag.clone(),
        }
    }

    /// Load (or unload) a ship's scripts into the script host; sources that fail to load grey their nodes.
    pub(crate) fn load_scripts(&mut self, vessel: EntityId) {
        let Some(ship) = self.ships.get_mut(vessel) else { return };
        let graph = ship.graph.clone();
        if graph.scripts.is_empty() { self.deps.scripts.unload(vessel); return; }
        let limits = crate::script::ScriptLimits { interrupts_per_tick: self.deps.budgets.luau_interrupts, ..Default::default() };
        let results = self.deps.scripts.load(vessel, &graph.script_sources, limits);
        let mut greyed = Vec::new();
        for (i, r) in results.iter().enumerate() {
            let Err(fault) = r else { continue };
            let Some(node) = graph.nodes.iter().find(|n| n.kind == NodeKind::Script(i.min(u16::MAX as usize) as u16)) else { continue };
            if !ship.graph_state.greyed.iter().any(|(id, _)| *id == node.id) { ship.graph_state.greyed.push((node.id, *fault)); greyed.push((node.id, *fault)); }
        }
        let side = ship.side;
        for (node, fault) in greyed { self.emit(Audience::side(side), Lane::Reliable, SimEvent::ScriptGreyedOut { vessel, node, fault }); }
    }

    /// Remove a vessel from the sortie now (sandbox leave).
    fn retire_vessel(&mut self, vessel: EntityId) {
        self.ships.remove(vessel);
        if self.world.vessel(vessel).is_some() { self.world.despawn(vessel); }
        self.sensors.remove_carrier(vessel);
        self.deps.scripts.unload(vessel);
        self.interest.forget(vessel);
        for g in &mut self.ai.groups {
            g.members.retain(|m| *m != vessel);
            if g.leader == Some(vessel) { g.leader = None; }
        }
    }

    /// A free spawn spot for a sandbox join: (surface position, heading).
    fn free_spawn(&self, side: SideId) -> (DVec3, f64) {
        let mut markers: Vec<&content::campaign::Marker> = self.config.map.markers.iter().filter(|m| m.kind == MarkerKind::PlayerSpawn && m.pos_m.iter().all(|c| c.is_finite()) && m.heading_deg.is_finite()).collect();
        if let Some(own) = markers.get(side.index()).copied() { markers.retain(|m| !core::ptr::eq(*m, own)); markers.insert(0, own); }
        let hulls: Vec<DVec2> = self.world.vessels().filter(|(_, v)| !matches!(v.status, VesselStatus::Lost(_))).map(|(_, v)| {
            let c = v.to_world(DVec3::new(v.dims[0] * 0.5, 0.0, v.dims[2] * 0.5));
            DVec2::new(c.x, c.z)
        }).filter(|p| p.is_finite()).collect();
        let free = |p: DVec2| hulls.iter().all(|h| h.distance_squared(p) >= SPAWN_CLEAR_M * SPAWN_CLEAR_M);
        let half = DVec2::from(self.config.map.size_m) * 0.5;
        let on_map = |p: DVec2| p.x.abs() <= half.x && p.y.abs() <= half.y;
        let bases: Vec<(DVec2, f64)> = if markers.is_empty() { vec![(DVec2::ZERO, 0.0)] } else { markers.iter().map(|m| (DVec2::from(m.pos_m), m.heading_deg.to_radians())).collect() };
        for (base, heading) in &bases {
            let right = DVec2::new(libm::cos(*heading), libm::sin(*heading));
            for k in 0..SPAWN_TRIES {
                let step = (k as f64 / 2.0).ceil() * if k % 2 == 1 { 1.0 } else { -1.0 };
                let p = *base + right * (step * SPAWN_STEP_M);
                if on_map(p) && free(p) { return (DVec3::new(p.x, 0.0, p.y), *heading); }
            }
        }
        let (base, heading) = bases[0];
        let p = base + DVec2::new(libm::cos(heading), libm::sin(heading)) * (SPAWN_TRIES as f64 * SPAWN_STEP_M);
        (DVec3::new(p.x, 0.0, p.y), heading)
    }
}

/// Check a config before use: sides, players and vessel indices consistent; commitment not checked here
/// (the generator's gate does that). Unit: `sys-sim-setup`.
pub fn validate_config(config: &SortieConfig) -> Result<(), SimError> {
    fn bad<T>(s: String) -> Result<T, SimError> { Err(SimError::Config(s)) }
    let finite2 = |v: DVec3| v.x.is_finite() && v.z.is_finite();
    if config.sides.is_empty() { return bad("no sides".into()); }
    let mut ids = BTreeSet::new();
    for s in &config.sides {
        if s.id.index() >= MAX_SIDES { return bad(format!("side {} out of range", s.id.0)); }
        if !ids.insert(s.id) { return bad(format!("side {} defined twice", s.id.0)); }
        if s.pvp_team.is_some_and(|t| t > 1) { return bad(format!("side {}: PvP team must be 0 or 1", s.id.0)); }
    }
    let has_side = |s: SideId| ids.contains(&s);
    if config.vessels.len() > u16::MAX as usize { return bad("too many vessels".into()); }
    if config.bodies.len() > u16::MAX as usize { return bad("too many bodies".into()); }
    if config.weather_changes.len() > u16::MAX as usize { return bad("too many weather changes".into()); }
    let size_ok = config.map.size_m.iter().all(|s| s.is_finite() && *s > 0.0 && *s <= MAX_MAP_M);
    if !size_ok { return bad("map size out of range".into()); }
    if !config.map.depth_m.is_finite() || !config.layer_depth_m.is_finite() { return bad("map depth or layer depth is not a number".into()); }

    let stored: BTreeSet<ContentHash> = config.blueprints.iter().map(|b| b.header.hash).collect();
    let mut computed: Option<BTreeSet<ContentHash>> = None;
    for (i, v) in config.vessels.iter().enumerate() {
        if !has_side(v.side) { return bad(format!("vessel {i}: no side {}", v.side.0)); }
        if !finite2(v.pos) || !v.heading_rad.is_finite() { return bad(format!("vessel {i}: position or heading is not a number")); }
        match &v.blueprint {
            BlueprintRef::Inline(h) => {
                let known = stored.contains(h) || computed.get_or_insert_with(|| config.blueprints.iter().map(content::blueprint::io::compute_hash).collect()).contains(h);
                if !known { return bad(format!("vessel {i}: design not in the config")); }
            }
            BlueprintRef::Stock(p) => if p.trim().is_empty() { return bad(format!("vessel {i}: empty stock path")); },
        }
    }
    let mut slots = BTreeSet::new();
    let mut hulls = BTreeSet::new();
    if config.players.len() > MAX_SIDES { return bad("too many players".into()); }
    for p in &config.players {
        if p.slot.index() >= MAX_SIDES { return bad(format!("player slot {} out of range", p.slot.0)); }
        if !slots.insert(p.slot) { return bad(format!("player slot {} used twice", p.slot.0)); }
        if !has_side(p.side) { return bad(format!("player {}: no side {}", p.slot.0, p.side.0)); }
        if let Some(i) = p.vessel {
            let Some(v) = config.vessels.get(i as usize) else { return bad(format!("player {}: no vessel {i}", p.slot.0)) };
            if v.side != p.side { return bad(format!("player {}: vessel {i} is on another side", p.slot.0)); }
            if !hulls.insert(i) { return bad(format!("vessel {i} has two players")); }
        }
    }
    for (i, b) in config.bodies.iter().enumerate() {
        if !has_side(b.side) { return bad(format!("body {i}: no side {}", b.side.0)); }
        if !b.pos.is_finite() || !b.vel.is_finite() { return bad(format!("body {i}: position or velocity is not a number")); }
    }
    for g in &config.groups { if !has_side(g.side) { return bad(format!("group {}: no side {}", g.id.0, g.side.0)); } }
    if let Some(i) = config.intel.named.iter().find(|i| **i as usize >= config.vessels.len()) { return bad(format!("briefing names vessel {i}, which does not exist")); }
    if let ModeRules::Escort { convoy_side, required_fraction, .. } = &config.rules {
        if !has_side(*convoy_side) { return bad(format!("escort: no side {}", convoy_side.0)); }
        if !required_fraction.is_finite() { return bad("escort: required fraction is not a number".into()); }
    }
    Ok(())
}

// ------------------------------------------------------------------------------------------------------------
// Helpers
// ------------------------------------------------------------------------------------------------------------

/// The per-tick context, from disjoint fields so systems can be borrowed mutably beside it.
fn make_ctx<'a>(config: &'a SortieConfig, deps: &'a SimDeps, world: &'a World) -> SimCtx<'a> {
    SimCtx { seed: config.seed, tick: world.tick(), catalogue: &deps.catalogue, tuning: &deps.catalogue.tuning, weather: world.weather(), time_of_day: config.time_of_day, layer_depth_m: config.layer_depth_m, difficulty: &config.settings.difficulty }
}

/// Number of side slots pictures and links need: highest side id plus one.
fn side_slots(config: &SortieConfig) -> usize { config.sides.iter().map(|s| s.id.index() + 1).max().unwrap_or(1).min(MAX_SIDES) }

fn side_faction(config: &SortieConfig, side: SideId) -> Option<Faction> { config.sides.iter().find(|s| s.id == side).map(|s| s.faction) }

fn fraction(v: f64) -> f64 { if v.is_finite() { v.clamp(0.0, 1.0) } else { 1.0 } }

/// Validate one blueprint as a player ship under a ruleset.
fn check_blueprint(bp: &Blueprint, ruleset: &Ruleset, catalogue: &Catalogue) -> Result<(), SimError> {
    let ctx = content::validate::ValidationContext { ruleset, available: None, enemy_class: false, complexity: None };
    let report = content::validate::validate_blueprint(bp, catalogue, &ctx);
    if report.is_ok() { return Ok(()); }
    let keys: Vec<String> = report.issues.iter().take(8).map(|i| i.text_key().0).collect();
    let name: String = bp.header.name.chars().take(64).collect();
    Err(SimError::Blueprint(format!("{name}: {}", keys.join(", "))))
}

/// Re-canonicalise, re-hash and validate every inline blueprint; point vessel references at the recomputed
/// hashes. A reference matches the recomputed hash, the hash of the blueprint as sent, or the header's stored
/// hash, in that order.
fn normalize_blueprints(config: &mut SortieConfig, catalogue: &Catalogue) -> Result<(), SimError> {
    let sent = core::mem::take(&mut config.blueprints);
    let mut canon: Vec<ContentHash> = Vec::with_capacity(sent.len());
    let mut out: Vec<Blueprint> = Vec::new();
    for bp in &sent {
        let c = crate::editor::splash::canonical(bp);
        check_blueprint(&c, &config.settings.ruleset, catalogue)?;
        canon.push(c.header.hash);
        if !out.iter().any(|b| b.header.hash == c.header.hash) { out.push(c); }
    }
    // The hash of each blueprint as sent, computed only if some reference needs it.
    let mut raw: Option<Vec<ContentHash>> = None;
    for (i, v) in config.vessels.iter_mut().enumerate() {
        let BlueprintRef::Inline(h) = v.blueprint else { continue };
        let mut to = canon.iter().position(|c| *c == h);
        if to.is_none() {
            let r = raw.get_or_insert_with(|| sent.iter().map(content::blueprint::io::compute_hash).collect());
            to = r.iter().position(|x| *x == h);
        }
        let to = to.or_else(|| sent.iter().position(|b| b.header.hash == h));
        match to { Some(k) => v.blueprint = BlueprintRef::Inline(canon[k]), None => return Err(SimError::Blueprint(format!("vessel {i} names a design that was not sent"))) }
    }
    config.blueprints = out;
    Ok(())
}

/// Grid-origin pose that centres a hull on a surface point at a compass heading.
fn hull_pose(prefab: &sim_core::vessel::VesselPrefab, centre: DVec2, heading_rad: f64) -> Pose {
    let fin = |v: f64| if v.is_finite() { v } else { 0.0 };
    let rot = Pose::from_heading(DVec3::ZERO, heading_rad).rot;
    let half = DVec3::new(fin(prefab.dims[0]) * 0.5, 0.0, fin(prefab.dims[2]) * 0.5);
    let c = if centre.is_finite() { centre } else { DVec2::ZERO };
    Pose { pos: DVec3::new(c.x, 0.0, c.y) - rot * half, rot }
}

/// A blank player-hull init on a side.
fn default_vessel(blueprint: BlueprintRef, side: SideId) -> VesselInit {
    VesselInit {
        blueprint, side, class: None, group: None, pos: DVec3::ZERO, heading_rad: 0.0, speed_kn: 0.0, arrives_s: 0, damage: None, fuel_fraction: 1.0, ammo_fraction: 1.0,
        ammo: BTreeMap::new(), hull_record: None, veterancy: 0.0, tag: String::new(), declared: None, emcon: None, passengers: 0, role: VesselRole::PlayerHull,
    }
}

/// Officers running a player's hull: keep the player's orders, evade inbound weapons.
fn proxy_captain(orders: &ShipOrders) -> CaptainState {
    let mut c = new_captain(Behaviour::Proxy, None, None, SurrenderRule::Never);
    c.route = orders.route.clone();
    c
}

/// Route of an AI `Proxy` hull: its group's patrol markers as a closed loop, starting at the marker after the
/// nearest one and repeated [`DRONE_LAPS`] times. Empty when the group has no patrol orders.
fn proxy_route(config: &SortieConfig, vi: &VesselInit) -> Vec<DVec2> {
    let Some(g) = vi.group.and_then(|g| config.groups.iter().find(|gi| gi.id == g && gi.side == vi.side)) else { return Vec::new() };
    let GroupOrders::Patrol { markers } = &g.orders else { return Vec::new() };
    let pts: Vec<DVec2> = markers.iter().filter_map(|name| config.map.markers.iter().find(|m| &m.name == name)).map(|m| DVec2::from(m.pos_m)).filter(|p| p.is_finite()).collect();
    if pts.is_empty() { return Vec::new(); }
    let me = DVec2::new(vi.pos.x, vi.pos.z);
    let nearest = (0..pts.len()).min_by(|&a, &b| pts[a].distance_squared(me).total_cmp(&pts[b].distance_squared(me)).then(a.cmp(&b))).unwrap_or(0);
    let laps = if pts.len() > 1 { DRONE_LAPS } else { 1 };
    (0..pts.len() * laps).map(|k| pts[(nearest + 1 + k) % pts.len()]).collect()
}

/// Station and orbit radius of a configured aircraft: its group's air patrol or guard marker, else an orbit
/// around its spawn point at half its range (1 to 20 km).
fn air_station(config: &SortieConfig, group: Option<GroupId>, range_m: f64, pos: DVec2) -> (DVec2, f64) {
    let default_radius = if range_m.is_finite() { (0.5 * range_m).clamp(1_000.0, 20_000.0) } else { 10_000.0 };
    if let Some(g) = group.and_then(|g| config.groups.iter().find(|gi| gi.id == g)) {
        if let GroupOrders::AirPatrol { marker, radius_m } | GroupOrders::Guard { marker, radius_m } = &g.orders {
            if let Some(m) = config.map.markers.iter().find(|m| &m.name == marker) {
                let r = if radius_m.is_finite() { radius_m.max(500.0) } else { default_radius };
                return (DVec2::from(m.pos_m), r);
            }
        }
    }
    (if pos.is_finite() { pos } else { DVec2::ZERO }, default_radius)
}

/// Rounds each store slot holds when full: magazines, launcher cells, decoy rounds, mine rails.
fn store_capacity(f: &PartFunction) -> f64 {
    match f {
        PartFunction::Magazine { rounds, .. } => *rounds as f64,
        PartFunction::Launcher(l) => l.cells as f64,
        PartFunction::Decoy(d) => d.rounds as f64,
        PartFunction::MineRail { capacity, .. } => *capacity as f64,
        _ => 0.0,
    }
}

/// One load fraction for spawn: carried rounds per slot where given, `ammo_fraction` elsewhere, weighted by
/// capacity.
fn spawn_ammo_fraction(vi: &VesselInit, design: &ShipDesign, cat: &Catalogue) -> f64 {
    let base = fraction(vi.ammo_fraction);
    if vi.ammo.is_empty() { return base; }
    let (mut cap, mut have) = (0.0, 0.0);
    for p in &design.prefab.parts {
        let Some((_, def)) = cat.parts.get_index(p.def.index()) else { continue };
        let c = store_capacity(&def.function);
        if c <= 0.0 { continue; }
        cap += c;
        have += match vi.ammo.get(&p.slot) { Some(n) => (*n as f64).min(c), None => base * c };
    }
    if cap > 0.0 { (have / cap).clamp(0.0, 1.0) } else { base }
}

/// Fuel and ammunition fractions left aboard a live vessel.
fn stores(v: &Vessel, cat: &Catalogue) -> (f64, f64) {
    let (mut fuel_cap, mut fuel) = (0.0, 0.0);
    let (mut ammo_cap, mut ammo) = (0.0, 0.0);
    for p in &v.parts {
        let Some((_, def)) = cat.parts.get_index(p.def.index()) else { continue };
        match (&def.function, &p.state) {
            (PartFunction::FuelTank { litres: c }, PartState::FuelTank { litres }) if c.is_finite() && *c > 0.0 => { fuel_cap += c; fuel += litres.clamp(0.0, *c); }
            (f, s) => {
                let c = store_capacity(f);
                if c <= 0.0 { continue; }
                let n = match s { PartState::Magazine { rounds, .. } => *rounds as f64, PartState::Launcher { ready, .. } => *ready as f64, PartState::Decoy { rounds, .. } => *rounds as f64, PartState::MineRail { mines, .. } => *mines as f64, _ => c };
                ammo_cap += c;
                ammo += n.clamp(0.0, c);
            }
        }
    }
    (if fuel_cap > 0.0 { fuel / fuel_cap } else { 1.0 }, if ammo_cap > 0.0 { ammo / ammo_cap } else { 1.0 })
}

/// The damage a vessel carries, for respawning it from an edited blueprint with the same cells: every
/// damaged cell, and the health of each damaged part matched to the new blueprint's part with the same
/// definition, cell and rotation.
fn carried_damage(v: &Vessel, old: &Blueprint, new: &Blueprint) -> HullDamage {
    let cells = v.grid.damage.iter().enumerate().filter(|(_, b)| **b > 0).map(|(i, b)| content::cell::CellChange { cell: CellIndex(i as u32), bucket: (*b).min(content::cell::BUCKET_DESTROYED) }).collect();
    let mut parts = BTreeMap::new();
    for p in &v.parts {
        if !(p.health.is_finite() && p.health < 1.0) { continue; }
        let Some(place) = old.parts.get(p.slot.index()) else { continue };
        if let Some(j) = new.parts.iter().position(|q| q.part == place.part && q.pos == place.pos && content::kit::normalize_rot(q.rot) == content::kit::normalize_rot(place.rot)) {
            parts.insert(PartSlot(j as u16), p.health.clamp(0.0, 1.0));
        }
    }
    HullDamage { cells, parts, jury_rigged: 0 }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::config::{SideDef, SortieIntel};
    use content::campaign::{Marker, Objective};

    fn side(id: u8) -> SideDef { SideDef { id: SideId(id), faction: if id == 0 { Faction::Players } else { Faction::Combine }, name: TextKey::new("s"), hostile_to: SideMask::NONE, allied: SideMask::only(SideId(id)), pvp_team: None } }

    fn config() -> SortieConfig {
        let bp = content::blueprint::io::new_blueprint("Test", 30.0);
        let h = content::blueprint::io::compute_hash(&bp);
        let mut v = default_vessel(BlueprintRef::Inline(h), SideId(0));
        v.pos = DVec3::new(10.0, 0.0, 20.0);
        let mut e = default_vessel(BlueprintRef::Stock("examples/minimal.bp.ron".into()), SideId(1));
        e.role = VesselRole::Combatant;
        SortieConfig {
            protocol: crate::protocol::PROTOCOL_VERSION, seed: 1, mode: GameMode::Skirmish, rules: ModeRules::Objective, settings: Default::default(), act: Act::Act1,
            weather: Default::default(), weather_changes: vec![], time_of_day: TimeOfDay::Day, map: content::campaign::MapDef { size_m: [20_000.0, 20_000.0], depth_m: 100.0, islands: vec![], markers: vec![], seed: 0 },
            layer_depth_m: 50.0, sides: vec![side(0), side(1)], players: vec![PlayerInit { slot: PlayerSlot(0), client_id: 9, name: "a".into(), side: SideId(0), vessel: Some(0), human: true }],
            blueprints: vec![bp], vessels: vec![v, e], bodies: vec![], groups: vec![], objective: Objective::DestroyAll, ghosts: vec![], tags: vec![], actions: vec![],
            intel: SortieIntel::default(), cards: vec![], operation: None, region: None, theatre_hour: 0, splash: None, tutorial: None, time_limit_s: None,
        }
    }

    fn rejects(c: &SortieConfig, what: &str) { assert!(matches!(validate_config(c), Err(SimError::Config(_))), "{what} should be refused"); }

    #[test]
    fn a_consistent_config_passes() { assert!(validate_config(&config()).is_ok()); }

    #[test]
    fn inconsistent_configs_are_refused() {
        let mut c = config(); c.sides.clear(); rejects(&c, "no sides");
        let mut c = config(); c.sides.push(side(1)); rejects(&c, "duplicate side");
        let mut c = config(); c.sides.push(side(9)); rejects(&c, "side id 9");
        let mut c = config(); c.vessels[1].side = SideId(4); rejects(&c, "vessel on a missing side");
        let mut c = config(); c.vessels[0].pos.x = f64::NAN; rejects(&c, "NaN position");
        let mut c = config(); c.vessels[0].blueprint = BlueprintRef::Inline(ContentHash([7; 32])); rejects(&c, "unknown inline design");
        let mut c = config(); c.players[0].vessel = Some(5); rejects(&c, "player vessel index");
        let mut c = config(); c.players[0].vessel = Some(1); rejects(&c, "player on another side's vessel");
        let mut c = config(); let mut p = c.players[0].clone(); p.slot = PlayerSlot(1); c.players.push(p); rejects(&c, "two players on one hull");
        let mut c = config(); let p = c.players[0].clone(); c.players.push(p); rejects(&c, "slot used twice");
        let mut c = config(); c.intel.named.push(9); rejects(&c, "named vessel");
        let mut c = config(); c.map.size_m = [0.0, 1.0]; rejects(&c, "map size");
        let mut c = config(); c.map.size_m = [MAX_MAP_M * 2.0, 1_000.0]; rejects(&c, "map too large");
        let mut c = config(); c.rules = ModeRules::Escort { convoy_side: SideId(5), entry: "a".into(), exit: "b".into(), required_fraction: 0.5 }; rejects(&c, "escort side");
    }

    #[test]
    fn stored_hash_references_are_rewritten_to_the_recomputed_hash() {
        let mut c = config();
        let real = c.blueprints[0].header.hash;
        // The blueprint as sent carries a stale stored hash; the vessel names it by that hash.
        let stale = ContentHash([3; 32]);
        c.blueprints[0].header.hash = stale;
        c.vessels[0].blueprint = BlueprintRef::Inline(stale);
        let cat = content::catalogue::catalogue();
        // Validation of the empty test hull may refuse it; only the hash bookkeeping is under test here.
        let canonical = crate::editor::splash::canonical(&c.blueprints[0]);
        assert_ne!(canonical.header.hash, stale);
        match normalize_blueprints(&mut c, cat) {
            Ok(()) => {
                assert_eq!(c.blueprints[0].header.hash, canonical.header.hash);
                assert_eq!(c.vessels[0].blueprint, BlueprintRef::Inline(canonical.header.hash));
            }
            Err(SimError::Blueprint(_)) => {}
            Err(e) => panic!("unexpected {e}"),
        }
        let _ = real;
    }

    #[test]
    fn hull_is_centred_on_the_configured_point() {
        let mut prefab = sim_core::vessel::VesselPrefab {
            hash: ContentHash::EMPTY, name: String::new(), grid: test_grid(), parts: vec![], compartments: Default::default(), networks: Default::default(),
            dry_mass: Default::default(), cuboids: vec![], blocks: Default::default(), root_cell: CellIndex(0), berths: 0, waterline_y: 2.0, dims: [40.0, 6.0, 8.0], work_counts: [0; 4],
        };
        for heading in [0.0, 1.0, 2.5, 4.0] {
            let p = hull_pose(&prefab, DVec2::new(100.0, -50.0), heading);
            let c = p.to_world(DVec3::new(20.0, 0.0, 4.0));
            assert!((c - DVec3::new(100.0, 0.0, -50.0)).length() < 1e-9);
            assert!((p.heading_rad() - heading).abs() < 1e-9);
        }
        prefab.dims = [f64::NAN, 0.0, f64::INFINITY];
        assert!(hull_pose(&prefab, DVec2::ZERO, 0.0).pos.is_finite());
    }

    fn test_grid() -> sim_core::grid::VesselGrid { sim_core::grid::VesselGrid::build(Arc::new(content::cell::CellGrid::default()), &[], &|_| (false, content::part::Flammable::No)) }

    #[test]
    fn drone_route_loops_the_patrol_markers_from_the_next_marker() {
        let mut c = config();
        for (k, p) in [[0.0, -100.0], [100.0, 0.0], [0.0, 100.0], [-100.0, 0.0]].iter().enumerate() {
            c.map.markers.push(Marker { kind: MarkerKind::EnemySpawn, name: format!("m{k}"), pos_m: *p, heading_deg: 0.0 });
        }
        c.groups.push(crate::protocol::config::GroupInit { id: GroupId(0), side: SideId(1), kind: content::campaign::FleetKind::Patrol, formation: Default::default(), orders: GroupOrders::Patrol { markers: (0..4).map(|k| format!("m{k}")).collect() }, ace: None, theatre_group: None, tag: String::new() });
        let mut vi = c.vessels[1].clone();
        vi.group = Some(GroupId(0));
        vi.pos = DVec3::new(95.0, 0.0, 5.0);
        let r = proxy_route(&c, &vi);
        assert_eq!(r.len(), 4 * DRONE_LAPS);
        assert_eq!(r[0], DVec2::new(0.0, 100.0));
        assert_eq!(r[1], DVec2::new(-100.0, 0.0));
        assert_eq!(r[3], DVec2::new(100.0, 0.0));
        vi.group = None;
        assert!(proxy_route(&c, &vi).is_empty());
    }

    #[test]
    fn carried_ammunition_folds_into_one_fraction() {
        assert_eq!(fraction(f64::NAN), 1.0);
        assert_eq!(fraction(2.0), 1.0);
        assert_eq!(fraction(-1.0), 0.0);
    }

    fn stock(path: &str) -> Blueprint { crate::editor::splash::canonical(&content::campaign::load::stock_blueprint(path).unwrap()) }

    fn joined(sim: &Sim, player: PlayerSlot) -> Option<EntityId> {
        sim.out.iter().rev().find_map(|e| match e.event { SimEvent::PlayerJoined { player: p, vessel, .. } if p == player => Some(vessel), _ => None })
    }

    /// A sandbox with one human player on a 22 m craft and an enemy patrol boat.
    fn sandbox() -> Sim {
        let mut c = config();
        c.mode = GameMode::Sandbox;
        c.settings.ruleset = Ruleset::campaign(Act::Act1);
        let bp = stock("starters/fac22.bp.ron");
        c.vessels[0].blueprint = BlueprintRef::Inline(bp.header.hash);
        c.vessels[1].blueprint = BlueprintRef::Stock("starters/patrol15.bp.ron".into());
        c.vessels[1].pos = DVec3::new(3_000.0, 0.0, 0.0);
        c.blueprints = vec![bp];
        Sim::new(c, crate::editor::splash::tests::real_deps()).expect("the sandbox starts")
    }

    #[test]
    fn sandbox_players_join_drop_rejoin_and_leave() {
        let mut sim = sandbox();
        assert_eq!(sim.ships.ships.len(), 2);
        let first = sim.players[0].vessel.expect("slot 0 has its hull");
        assert_eq!(joined(&sim, PlayerSlot(0)), Some(first));
        let enemy = sim.ships.ships.iter().find(|s| s.vessel != first).unwrap();
        assert!(enemy.captain.is_some() && enemy.player.is_none());

        // A second player joins with a blueprint and no position: a free spot, away from every hull.
        let p = PlayerInit { slot: PlayerSlot(1), client_id: 77, name: "b".into(), side: SideId(0), vessel: None, human: true };
        sim.add_player(p.clone(), None, Some(stock("starters/patrol15.bp.ron"))).expect("sandbox join");
        let second = sim.players.iter().find(|q| q.slot == PlayerSlot(1)).and_then(|q| q.vessel).expect("the joiner has a hull");
        assert_eq!(joined(&sim, PlayerSlot(1)), Some(second));
        let ship = sim.ships.get(second).unwrap();
        assert_eq!(ship.player, Some(PlayerSlot(1)));
        assert!(ship.captain.is_none());
        let at = |e: EntityId| { let v = sim.world.vessel(e).unwrap(); let c = v.to_world(DVec3::new(v.dims[0] * 0.5, 0.0, v.dims[2] * 0.5)); DVec2::new(c.x, c.z) };
        assert!(at(second).distance(at(first)) >= SPAWN_CLEAR_M - 1.0, "joins clear of other hulls");
        // The slot is taken now; a joiner on a missing side is refused.
        assert!(sim.add_player(p.clone(), None, None).is_err());
        assert!(sim.add_player(PlayerInit { slot: PlayerSlot(2), side: SideId(6), client_id: 78, ..p.clone() }, None, None).is_err());

        // Drop: officers take the hull and keep the player's orders.
        sim.drop_player(PlayerSlot(1));
        let ship = sim.ships.get(second).unwrap();
        assert!(ship.control.officers_in_command);
        assert_eq!(ship.captain.as_ref().map(|c| c.behaviour), Some(Behaviour::Proxy));
        assert!(sim.out.iter().any(|e| matches!(e.event, SimEvent::PlayerDropped { player: PlayerSlot(1), officers_in_command: true })));
        // Rejoin within the hold: back under player command.
        assert_eq!(sim.rejoin_player(77), Some(PlayerSlot(1)));
        let ship = sim.ships.get(second).unwrap();
        assert!(!ship.control.officers_in_command && ship.captain.is_none());
        assert_eq!(sim.rejoin_player(77), None, "a connected player cannot rejoin");

        // Leave: the hull and the slot go.
        sim.remove_player(PlayerSlot(1));
        assert!(sim.ships.get(second).is_none() && sim.world.vessel(second).is_none_or(|v| v.id != second));
        assert!(sim.players.iter().all(|q| q.slot != PlayerSlot(1)));
        assert_eq!(sim.ships.ships.len(), 2);
    }

    #[test]
    fn joins_outside_sandbox_are_refused_and_bad_blueprints_never_spawn() {
        let mut sim = sandbox();
        sim.config.mode = GameMode::Skirmish;
        let p = PlayerInit { slot: PlayerSlot(1), client_id: 5, name: "c".into(), side: SideId(0), vessel: None, human: true };
        assert!(sim.add_player(p.clone(), None, None).is_err());
        sim.config.mode = GameMode::Sandbox;
        // A blueprint over the ruleset's length limit is refused and leaves no trace.
        let mut big = stock("starters/destroyer90.bp.ron");
        sim.config.settings.ruleset.max_length_m = 30.0;
        big.header.name = "Too long".into();
        let before = (sim.config.vessels.len(), sim.players.len(), sim.ships.ships.len());
        assert!(matches!(sim.add_player(p, None, Some(big)), Err(SimError::Blueprint(_))));
        assert_eq!(before, (sim.config.vessels.len(), sim.players.len(), sim.ships.ships.len()));
    }

    #[test]
    fn live_splash_edits_update_in_place_replace_the_vessel_or_refuse_a_new_shell() {
        let deps = crate::editor::splash::tests::real_deps();
        let camp = deps.campaign.clone();
        let bp = stock("starters/fac22.bp.ron");
        let options = content::settings::SplashOptions { water: content::weather::SeaState::CALM, target: content::settings::SplashTarget::PatrolBoat };
        let prepared = crate::editor::splash::prepare(&bp, options, 4, &camp, &deps.catalogue);
        let mut sim = crate::editor::splash::launch(&prepared, deps, crate::editor::splash::SplashStage::Full).expect("the basin starts");
        let me = PlayerSlot(0);
        let first = sim.ships.of_player(me).unwrap().vessel;

        // Settings only: the same vessel, new settings and design.
        let mut edit = bp.clone();
        edit.officers.sliders.insert("gunnery.aggression".into(), 0.8);
        sim.apply_design(me, &edit).expect("settings edit");
        let ship = sim.ships.of_player(me).unwrap();
        assert_eq!(ship.vessel, first);
        assert_eq!(ship.settings.sliders.get("gunnery.aggression"), Some(&0.8));
        assert_eq!(ship.design.hash, crate::editor::splash::canonical(&edit).header.hash);
        assert!(Arc::ptr_eq(&ship.design.prefab, &prepared.design.prefab), "no physical rebuild");

        // A physical edit with the same shell: a new vessel at the same place, announced to everyone.
        let mut repaint = edit.clone();
        repaint.paint[0].rgb[0] = (repaint.paint[0].rgb[0] + 0.25).fract();
        let pos = sim.world.vessel(first).unwrap().kin.pose.pos;
        sim.apply_design(me, &repaint).expect("paint edit");
        let second = sim.ships.of_player(me).unwrap().vessel;
        assert_ne!(second, first);
        // Columns are looked up by slot, so a reused slot answers for the old id: compare the stored id.
        assert!(sim.world.vessel(first).is_none_or(|v| v.id != first), "the old vessel is gone");
        assert!((sim.world.vessel(second).unwrap().kin.pose.pos - pos).length() < 1e-6);
        assert_eq!(sim.players[0].vessel, Some(second));
        assert_eq!(sim.schedule.spawned.get(&0), Some(&second));
        assert_eq!(joined(&sim, me), Some(second));
        assert_eq!(sim.ships.ships.len(), 2, "the patrol boat is untouched");

        // A new hull shell goes back to dry dock.
        let mut reshell = repaint.clone();
        reshell.hull.design_waterline_y += 1;
        assert!(sim.apply_design(me, &reshell).is_err());
        assert_eq!(sim.ships.of_player(me).unwrap().vessel, second);
    }

    #[test]
    fn air_station_uses_the_group_marker() {
        let mut c = config();
        c.map.markers.push(Marker { kind: MarkerKind::Objective, name: "cap".into(), pos_m: [5.0, 6.0], heading_deg: 0.0 });
        c.groups.push(crate::protocol::config::GroupInit { id: GroupId(2), side: SideId(1), kind: content::campaign::FleetKind::AirPatrol, formation: Default::default(), orders: GroupOrders::AirPatrol { marker: "cap".into(), radius_m: 100.0 }, ace: None, theatre_group: None, tag: String::new() });
        assert_eq!(air_station(&c, Some(GroupId(2)), 50_000.0, DVec2::ZERO), (DVec2::new(5.0, 6.0), 500.0));
        assert_eq!(air_station(&c, None, 50_000.0, DVec2::new(1.0, 2.0)), (DVec2::new(1.0, 2.0), 20_000.0));
        assert_eq!(air_station(&c, None, f64::NAN, DVec2::new(f64::NAN, 0.0)), (DVec2::ZERO, 10_000.0));
    }
}
