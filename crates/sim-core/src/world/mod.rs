//! Unit `sim-core-world` (hard: system order, parallel fan-out and serial merge). `query.rs` is unit
//! `sim-core-world-query` and `keyframe.rs` is unit `sim-core-keyframe`; the `World` struct below is the
//! contract all three share. One sortie's physical world: owns every store, spawns and despawns, steps the systems
//! in a fixed order, queues physical events, and gives read access to sim-systems and rendering.
//! DESIGN.md L1098-1129 (simulation core), L1150-1158 (determinism), L1133 (one world per sortie),
//! L416 (tick split), L112 and L843 (checkpoints), L216 (rewind).
//!
//! # Tick order (contract; golden hashes depend on it)
//! `World::step` runs these phases in this order. Parallel phases fan out per vessel on the caller's rayon pool
//! and merge serially in ascending vessel slot order. No phase reads wall-clock time; budgets are work units.
//!  1. Apply queued spawns and despawns in ascending entity slot order (bodies enter Rapier in that order).
//!  2. Advance time; refresh the wave set if weather changed.
//!  3. Parts and networks, per vessel: `networks::solve_tick`, then `parts::step_parts` (collects spawn requests).
//!  4. Spawns from step 3 in (vessel id, mount id) order: shells into `projectiles`, munitions and decoys
//!     into `munitions` (new entities), with `ProjectileSpawned` / `BodySpawned` events.
//!  5. Forces, per vessel in parallel: `mass::with_fluids`, `hydro::buoyancy`, `hydro::drag`,
//!     `hydro::propulsion`; applied to the physics world serially. Kinematic hulls use `hydro::kinematic_step`.
//!  6. `physics::PhysicsWorld::step`; copy kinematics and bounds back; contacts become collision damage.
//!  7. `munitions::Bodies::step` (uses last tick's shell grazes), outcomes resolved with `armour::resolve_warhead`.
//!  8. `projectiles::Projectiles::step`; hits sorted by (tick, projectile id).
//!  9. `armour::resolve_shell` per hit, then `damage::apply_hit` in that order; cook-off blasts via
//!     `damage::apply_cells`.
//! 10. `crew::step_crew`, `flooding::step_flooding`, `fire::step_fire`, per vessel.
//! 11. Structure under the shared [`crate::time::SPLIT_WORK_BUDGET`]: `structure::step_split` and
//!     `CompartmentSet::update_after_damage` in ascending vessel id, resuming from the saved cursor; new wreck
//!     entities are allocated here; colliders, blocks and dry mass rebuild for dirty vessels.
//! 12. `Vessel::evaluate_status` (disabled, capsized, sunk); wreck despawn rules; `VesselLost` events.
//! 13. Tick counter increments.
//!
//! `step.rs` implements the tick and lists, in its header, the decisions this order leaves open: rates of
//! 30 Hz and kinematic hulls, when ended entities are freed, where cook-off blasts land (the serial merge of
//! phase 10), how the structure cursor moves, when colliders rebuild, and which contacts and bucket changes
//! become events. The compartment recompute of phase 11 runs inside `structure::step_split`.
pub mod keyframe;
pub mod query;
mod step;
#[cfg(test)]
mod tests;

use crate::entity::{Column, EntityId, EntityTable};
use crate::events::PhysEvent;
use crate::math::Kinematics;
use crate::munitions::{Bodies, Decoys, Steer};
use crate::parts::VesselControls;
use crate::physics::PhysicsWorld;
use crate::projectiles::Projectiles;
use crate::structure::SplitJob;
use crate::terrain::Terrain;
use crate::time::Tick;
use crate::vessel::{Fidelity, Vessel, VesselPrefab, VesselSpawn};
use crate::waves::WaveSet;
use content::campaign::MapDef;
use content::catalogue::Catalogue;
use content::cell::{CellChange, RenderChunkCoord};
use content::ids::{CellIndex, ContentHash, MunitionIdx, PartSlot, SideId};
use content::part::DecoyKind;
use content::tuning::TuningTable;
use content::weather::WeatherState;
use glam::DVec3;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// Version of the keyframe encoding; a build restores only its own version.
pub const KEYFRAME_FORMAT: u32 = 1;

/// Everything needed to create an empty world.
#[derive(Clone, Debug)]
pub struct WorldConfig {
    pub seed: u64,
    pub weather: WeatherState,
    pub map: MapDef,
    pub catalogue: Arc<Catalogue>,
}

/// Read-only per-tick context handed to every system function.
#[derive(Clone, Copy)]
pub struct StepContext<'a> {
    pub seed: u64,
    pub tick: Tick,
    pub dt: f64,
    /// Simulation time in seconds at the start of this tick.
    pub time_s: f64,
    pub catalogue: &'a Catalogue,
    pub tuning: &'a TuningTable,
    pub waves: &'a WaveSet,
    pub weather: &'a WeatherState,
    pub terrain: &'a Terrain,
}

/// Deterministic work counts of the last tick per subsystem, for tracing spans, the nightly work-unit
/// regression check (L418) and Complexity points (L973).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkCounters {
    pub bodies: u32,
    pub cuboids: u32,
    pub collider_rebuilds: u32,
    pub buoyancy_samples: u32,
    pub projectile_cells: u32,
    pub live_projectiles: u32,
    pub damage_cells: u32,
    pub flood_openings: u32,
    pub split_cells: u32,
    pub conduit_segments: u32,
    pub compartments: u32,
}

/// Result of a pick ray (UI picking is done by Rust queries, never Godot physics, L1097).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PickHit { pub entity: EntityId, pub cell: Option<CellIndex>, pub part: Option<PartSlot>, pub point: DVec3, pub distance: f64 }

#[derive(Debug, thiserror::Error)]
pub enum KeyframeError {
    #[error("keyframe format {found} does not match this build's {expected}")]
    Format { found: u32, expected: u32 },
    #[error("keyframe is corrupt: {0}")]
    Corrupt(String),
    #[error("keyframe needs blueprint {0:?} which was not supplied")]
    MissingPrefab(ContentHash),
}

/// Bookkeeping the world keeps beside each [`Vessel`]: who hit it last, when it was lost, when its collider
/// was last rebuilt. Saved in keyframes and hashed with the vessel; the two cached fields are rebuilt on demand.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct VesselAux {
    /// Last entity that damaged this hull from outside ([`EntityId::NONE`] when nothing has): the `by` of
    /// `VesselLost` and the attacker credited for fire and cook-off damage.
    pub(crate) last_attacker: EntityId,
    /// Tick of the hull's last magazine cook-off, for the loss cause.
    pub(crate) cookoff_tick: Option<u64>,
    /// Tick the hull was lost; the wreck despawn rules count from it.
    pub(crate) lost_tick: Option<u64>,
    /// Tick of the last collider, buoyancy block and dry mass rebuild.
    pub(crate) rebuilt_tick: u64,
    /// `CrewState::passengers_lost` already reported through `PassengersLost`.
    pub(crate) passengers_reported: u16,
    /// Extent of the live cells in vessel-local metres for wreck pieces (their bounds are tight; a hull's
    /// bounds are its whole grid box).
    pub(crate) extent: Option<[DVec3; 2]>,
    /// Coarse blocks for the far-hull buoyancy fallback (L658); built on first use, dropped when cells change.
    #[serde(skip)]
    pub(crate) far_blocks: Option<Arc<crate::hydro::BuoyancyBlocks>>,
    /// Mass last pushed to the physics body, so an unchanged mass is not pushed again.
    #[serde(skip)]
    pub(crate) pushed_mass: Option<crate::mass::MassProperties>,
}
impl Default for VesselAux {
    fn default() -> Self {
        Self { last_attacker: EntityId::NONE, cookoff_tick: None, lost_tick: None, rebuilt_tick: 0, passengers_reported: 0, extent: None, far_blocks: None, pushed_mass: None }
    }
}

pub struct World {
    pub(crate) config: WorldConfig,
    pub(crate) tick: Tick,
    pub(crate) entities: EntityTable,
    pub(crate) vessels: Column<Vessel>,
    pub(crate) split_jobs: Column<SplitJob>,
    pub(crate) projectiles: Projectiles,
    pub(crate) bodies: Bodies,
    pub(crate) decoys: Decoys,
    pub(crate) physics: PhysicsWorld,
    pub(crate) waves: WaveSet,
    pub(crate) weather: WeatherState,
    pub(crate) terrain: Terrain,
    pub(crate) events: Vec<PhysEvent>,
    /// Vessel slot at which structure work resumes next tick (L688).
    pub(crate) split_cursor: u32,
    /// Live cap in force for projectiles (rung 3 of the overload ladder).
    pub(crate) projectile_cap: usize,
    pub(crate) work: WorkCounters,
    /// The prefab each vessel was built from (a wreck piece shares its parent's): `restore_vessel` and
    /// keyframes rebuild from it.
    pub(crate) prefabs: Column<Arc<VesselPrefab>>,
    pub(crate) aux: Column<VesselAux>,
    /// Shell grazes of the last projectile step; the next munitions step consumes them (L597).
    pub(crate) grazes: Vec<crate::projectiles::BodyGraze>,
    /// Entities that ended last tick (bodies, decoys, wrecks past their rules), ascending. Their rows stay
    /// readable until the start of the next tick, when phase 1 frees them, so the layers above can still
    /// resolve ids named in this tick's events.
    pub(crate) doomed: Vec<EntityId>,
    /// Pairs in contact at the end of last tick (`b` is `NONE` for the seabed), ascending: `Collision` and
    /// `Grounded` are queued when a contact starts or does damage, not on every tick it lasts.
    pub(crate) touching: Vec<(EntityId, EntityId)>,
}

/// Ticks between collider, buoyancy block and dry mass rebuilds of one hull under sustained fire. Shells read
/// the cell grid itself, so only rigid contact and displacement lag by this much. A split rebuilds at once.
pub(crate) const REBUILD_TICKS: u64 = 6;
/// A magazine cook-off this recent is the cause of a loss (L666).
pub(crate) const COOKOFF_LOSS_TICKS: u64 = 60 * crate::time::TICK_HZ as u64;
/// A vessel's undrained damage feed is dropped past this many entries (no local client is reading it; the
/// `CellsDamaged` events already carried every change).
pub(crate) const CHANGE_FEED_CAP: usize = 1 << 16;
/// A vessel's dirty render chunk list is deduplicated in place past this length.
pub(crate) const DIRTY_RENDER_COMPACT: usize = 1 << 10;

/// Wave seed of a sortie: one value per sortie seed, so a change of weather reshapes the same wave family.
pub(crate) fn wave_seed(seed: u64) -> u64 { crate::rng::bits(crate::rng::Key::new(seed, 0, crate::rng::Stream::WaveSet, 0, 0)) }

/// Live projectile cap for a ladder rung (L557); a zero or missing row falls back to the spec's values.
pub(crate) fn live_cap(tuning: &TuningTable, rung: u8) -> usize {
    let r = (rung as usize).min(2);
    match tuning.projectiles.live_caps[r] as usize { 0 => [crate::time::MAX_LIVE_PROJECTILES, 1_500, 1_000][r], c => c }
}

/// World box of the local box `[lo, hi]` under a pose.
pub(crate) fn world_box(pose: &crate::math::Pose, lo: DVec3, hi: DVec3) -> crate::math::Aabb {
    let centre = pose.to_world((lo + hi) * 0.5);
    let m = glam::DMat3::from_quat(pose.rot);
    let half = glam::DMat3::from_cols(m.x_axis.abs(), m.y_axis.abs(), m.z_axis.abs()) * ((hi - lo) * 0.5);
    if centre.is_finite() && half.is_finite() { crate::math::Aabb { min: centre - half, max: centre + half } } else { crate::math::Aabb { min: pose.pos, max: pose.pos } }
}

/// Refresh a vessel's world bounds from its pose: the whole grid box, or a wreck piece's live extent.
pub(crate) fn refresh_bounds(v: &mut Vessel, extent: Option<[DVec3; 2]>) {
    let (lo, hi) = match extent {
        Some([lo, hi]) => (lo, hi),
        None => { let s = v.grid.size(); (DVec3::ZERO, DVec3::new(s[0] as f64, s[1] as f64, s[2] as f64) * content::cell::CELL_M) }
    };
    v.bounds = world_box(&v.kin.pose, lo, hi);
}

/// Local extent of a cuboid list in metres, or `None` when it is empty.
pub(crate) fn cuboid_extent(cuboids: &[crate::physics::CuboidLocal]) -> Option<[DVec3; 2]> {
    let mut lo = [u32::MAX; 3];
    let mut hi = [0u32; 3];
    for c in cuboids {
        for k in 0..3 { lo[k] = lo[k].min(c.min[k] as u32); hi[k] = hi[k].max(c.min[k] as u32 + c.size[k] as u32); }
    }
    if cuboids.is_empty() { return None; }
    let m = content::cell::CELL_M;
    Some([DVec3::new(lo[0] as f64, lo[1] as f64, lo[2] as f64) * m, DVec3::new(hi[0] as f64, hi[1] as f64, hi[2] as f64) * m])
}

fn fraction(v: f64) -> f64 { if v.is_finite() { v.clamp(0.0, 1.0) } else { 1.0 } }

/// Scale the fuel and ammunition a prefab's parts carry by the loads brought in from the theatre.
fn apply_loads(parts: &mut [crate::parts::PartRuntime], fuel: f64, ammo: f64) {
    use crate::parts::PartState;
    let (fuel, ammo) = (fraction(fuel), fraction(ammo));
    let scale = |n: u32| libm::round(n as f64 * ammo) as u32;
    for p in parts {
        match &mut p.state {
            PartState::FuelTank { litres } if fuel < 1.0 => *litres *= fuel,
            PartState::Magazine { rounds, ammo_kg, .. } if ammo < 1.0 => {
                let kept = scale(*rounds);
                *ammo_kg = if *rounds > 0 { *ammo_kg * kept as f64 / *rounds as f64 } else { *ammo_kg * ammo };
                *rounds = kept;
            }
            PartState::Mount(m) if ammo < 1.0 => m.ready_rounds = scale(m.ready_rounds),
            PartState::Launcher { ready, .. } if ammo < 1.0 => *ready = scale(*ready as u32) as u8,
            PartState::MineRail { mines, .. } if ammo < 1.0 => *mines = scale(*mines as u32) as u16,
            PartState::Decoy { rounds, .. } if ammo < 1.0 => *rounds = scale(*rounds as u32) as u16,
            _ => {}
        }
    }
}

impl World {
    /// Create an empty world: terrain from the map, wave set from (seed, weather), empty physics.
    /// Unit: `sim-core-world`.
    pub fn new(config: WorldConfig) -> World {
        let tuning = &config.catalogue.tuning;
        let waves = WaveSet::generate(wave_seed(config.seed), &config.weather, &tuning.waves);
        let projectile_cap = live_cap(tuning, 0);
        World {
            tick: Tick(0),
            entities: EntityTable::new(),
            vessels: Column::new(),
            split_jobs: Column::new(),
            projectiles: Projectiles::default(),
            bodies: Bodies::default(),
            decoys: Decoys::default(),
            physics: PhysicsWorld::new(),
            waves,
            weather: config.weather,
            terrain: Terrain::new(config.map.clone()),
            events: Vec::new(),
            split_cursor: 0,
            projectile_cap,
            work: WorkCounters::default(),
            prefabs: Column::new(),
            aux: Column::new(),
            grazes: Vec::new(),
            doomed: Vec::new(),
            touching: Vec::new(),
            config,
        }
    }

    /// The live vessel behind an id, checked against the entity table (columns are indexed by slot only).
    pub(crate) fn vessel_mut(&mut self, id: EntityId) -> Option<&mut Vessel> {
        if !self.entities.is_alive(id) { return None; }
        self.vessels.get_mut(id).filter(|v| v.id == id)
    }

    // ---- Spawning ----
    /// Spawn a vessel from a prefab at its floating equilibrium; applies persistent damage and loads.
    /// Takes effect at once (callers spawn between ticks, in a fixed order). Unit: `sim-core-world`.
    ///
    /// Loads scale what the blueprint placed: fuel in tanks by `fuel_fraction`; magazine rounds, ready rounds,
    /// launcher cells, mines and decoy rounds by `ammo_fraction`. Persistent damage goes through the damage
    /// unit on the intact compartments (so breached compartments flood through their breaches), then the
    /// structure pass, with no `CellsDamaged` event: a spawned hull's damage is part of its starting state,
    /// and its damage and render feeds start empty. The hull floats at the calm-water equilibrium of its
    /// loaded mass, lifted by the wave height under its centre of mass. Returns [`EntityId::NONE`] when the
    /// entity table is full.
    pub fn spawn_vessel(&mut self, spawn: &VesselSpawn) -> EntityId {
        let id = self.entities.spawn(crate::entity::EntityKind::Vessel);
        if id.is_none() { return id; }
        let cat = self.config.catalogue.clone();
        let tuning = &cat.tuning;
        let prefab = &spawn.prefab;
        let mut parts = prefab.parts.clone();
        apply_loads(&mut parts, spawn.fuel_fraction, spawn.ammo_fraction);
        let mass = crate::mass::with_fluids(&prefab.dry_mass, &parts, &prefab.compartments, &cat);
        let heading = if spawn.pose.rot.is_finite() && spawn.pose.rot.length_squared() > 1e-12 { crate::math::Pose::new(DVec3::ZERO, spawn.pose.rot.normalize()).heading_rad() } else { 0.0 };
        let (x, z) = (if spawn.pose.pos.x.is_finite() { spawn.pose.pos.x } else { 0.0 }, if spawn.pose.pos.z.is_finite() { spawn.pose.pos.z } else { 0.0 });
        let mut vessel = Vessel {
            id,
            side: spawn.side,
            player: spawn.player,
            class: spawn.class.clone(),
            blueprint: prefab.hash,
            name: prefab.name.clone(),
            status: crate::vessel::VesselStatus::Active,
            fidelity: spawn.fidelity,
            grid: prefab.grid.clone(),
            parts,
            compartments: prefab.compartments.clone(),
            networks: prefab.networks.clone(),
            crew: crate::crew::build_crew(prefab, &tuning.crew),
            controls: VesselControls::default(),
            dry_mass: prefab.dry_mass,
            mass,
            body: crate::physics::BodyHandle::NONE,
            cuboids: prefab.cuboids.clone(),
            blocks: prefab.blocks.clone(),
            kin: Kinematics { pose: crate::math::Pose::from_heading(DVec3::new(x, 0.0, z), heading), ..Default::default() },
            bounds: crate::math::Aabb::default(),
            root_cell: prefab.root_cell,
            dims: prefab.dims,
            waterline_y: prefab.waterline_y,
            last_fired_tick: 0,
            capsize_ticks: 0,
            struck: false,
            pending_destroyed: Vec::new(),
            parent: None,
            spawn_tick: self.tick.0,
            submersible: spawn.submersible,
            tonnage_t: 0.0,
        };
        let damaged = match &spawn.damage {
            Some(d) => self.apply_persistent_damage(&mut vessel, d),
            None => false,
        };
        vessel.tonnage_t = vessel.mass.mass_kg / 1000.0;
        // Float it: calm-water equilibrium of the loaded mass, then ride the sea at the spawn point.
        let mut pose = crate::hydro::floating_pose_of(&vessel.blocks, &vessel.mass, x, z, heading, &tuning.buoyancy);
        let com = pose.to_world(vessel.mass.com);
        let lift = self.waves.height_at(com.x, com.z, self.time_s());
        if lift.is_finite() { pose.pos.y += lift; }
        let speed = if spawn.speed_mps.is_finite() { spawn.speed_mps } else { 0.0 };
        let bow = pose.rot * DVec3::X;
        let lin_vel = DVec3::new(bow.x, 0.0, bow.z).normalize_or(DVec3::X) * speed;
        vessel.kin = Kinematics { pose, lin_vel, ang_vel: DVec3::ZERO };
        vessel.body = self.physics.add_vessel(pose, lin_vel, &vessel.mass, &vessel.cuboids);
        if vessel.fidelity == Fidelity::Kinematic {
            self.physics.set_kinematic(vessel.body, true);
            self.physics.set_kinematics(vessel.body, &vessel.kin);
        }
        refresh_bounds(&mut vessel, None);
        // A spawned hull starts with empty feeds: its damage is part of the state the client builds from.
        vessel.grid.changes.clear();
        vessel.grid.dirty_render.clear();
        let side = vessel.side;
        let aux = VesselAux { rebuilt_tick: self.tick.0, pushed_mass: Some(vessel.mass), ..Default::default() };
        self.vessels.insert(id, vessel);
        self.prefabs.insert(id, prefab.clone());
        self.aux.insert(id, aux);
        self.split_jobs.remove(id);
        self.events.push(PhysEvent::VesselSpawned { vessel: id, side });
        if damaged {
            // Pieces the saved damage has cut loose leave now, so the hull enters its first tick as one body.
            // Only a piece that becomes a wreck is news (it is a new entity); debris from a saved hole is not.
            let from = self.events.len();
            let mut budget = u32::MAX;
            step::structure_for(self, id.index, &mut budget, usize::MAX);
            step::rebuild_geometry(self, id.index, true);
            let mut k = 0;
            self.events.retain(|e| { k += 1; k <= from || matches!(e, PhysEvent::Split { .. }) });
            if let Some(v) = self.vessels.get_mut(id) { v.grid.changes.clear(); v.grid.dirty_render.clear(); }
        }
        id
    }

    /// Persistent damage and part health carried in from the theatre (L714, L396). Returns whether any cell
    /// was removed.
    fn apply_persistent_damage(&self, vessel: &mut Vessel, damage: &content::save::HullDamage) -> bool {
        let cat = &*self.config.catalogue;
        let ctx = StepContext { seed: self.config.seed, tick: self.tick, dt: crate::time::DT, time_s: self.time_s(), catalogue: cat, tuning: &cat.tuning, waves: &self.waves, weather: &self.weather, terrain: &self.terrain };
        let cells: Vec<crate::armour::CellDamage> = damage.cells.iter().map(|c| crate::armour::CellDamage { cell: c.cell, buckets: c.bucket.min(content::cell::BUCKET_DESTROYED) }).collect();
        let outcome = crate::damage::apply_cells(vessel, &cells, crate::damage::DamageCause::Collision, &ctx);
        for (slot, health) in &damage.parts {
            let Some(p) = vessel.parts.get_mut(slot.index()).filter(|p| p.slot == *slot) else { continue };
            let h = if health.is_finite() { health.clamp(0.0, 1.0) } else { 1.0 };
            if h < p.health { p.health = h; p.level = crate::crew::level_from_health(h); }
        }
        // Conduits left jury-rigged: the cuts follow from the cells; the lowest-numbered ones were rigged.
        let mut left = damage.jury_rigged;
        for i in 0..vessel.networks.conduits.len().min(u16::MAX as usize) {
            if left == 0 { break; }
            if crate::networks::jury_rig(&mut vessel.networks, i as u16) { left -= 1; }
        }
        if !outcome.destroyed.is_empty() {
            crate::physics::regreedy_dirty(&vessel.grid, &mut vessel.cuboids);
            crate::hydro::rebuild_dirty_blocks(&mut vessel.blocks, &vessel.grid, &vessel.compartments);
            vessel.grid.drain_dirty_chunks();
            vessel.dry_mass = crate::mass::dry_mass(&vessel.grid, &vessel.parts, cat);
        }
        vessel.mass = crate::mass::with_fluids(&vessel.dry_mass, &vessel.parts, &vessel.compartments, cat);
        !outcome.destroyed.is_empty()
    }

    /// Spawn a free small body (enemy aircraft arriving, a pre-laid mine). Launches from parts
    /// happen inside `step`. Unit: `sim-core-world`.
    ///
    /// The body has no launcher. A non-finite position or velocity, or a full entity table, spawns nothing
    /// and returns [`EntityId::NONE`].
    pub fn spawn_body(&mut self, kind: MunitionIdx, side: SideId, pos: DVec3, vel: DVec3) -> EntityId {
        if !(pos.is_finite() && vel.is_finite()) { return EntityId::NONE; }
        let id = self.entities.spawn(crate::entity::EntityKind::Body);
        if id.is_none() { return id; }
        self.bodies.insert(id, kind, side, pos, vel, EntityId::NONE, PartSlot(0), self.tick.0, &self.config.catalogue);
        self.events.push(PhysEvent::BodySpawned { body: id, kind, side, launcher: EntityId::NONE, slot: PartSlot(0), pos, vel });
        id
    }
    /// Fire one shell now from no mount (scripted scenes and the weapon calibration gates of L606-L620, which
    /// fire a band's rounds at a hull from a set range). Mount fire happens inside `step`. Unit: `sim-core-world`.
    ///
    /// The round flies and hits exactly as a mount's does from the next step, with `shooter` credited (may be
    /// [`EntityId::NONE`]). Returns `None` for a non-finite origin or velocity or a full projectile store.
    pub fn fire_shell(&mut self, origin: DVec3, velocity: DVec3, calibre_mm: u16, band: content::part::weapons::CalibreBand, shooter: EntityId) -> Option<crate::projectiles::ProjectileId> {
        let s = self.projectiles.spawn(self.tick.0, origin, velocity, calibre_mm, band, shooter, PartSlot(0), false, self.projectile_cap)?;
        self.events.push(PhysEvent::ProjectileSpawned(s));
        Some(s.id)
    }
    /// Deploy a decoy object now (scripted scenes; launcher salvos happen inside `step`).
    pub fn spawn_decoy(&mut self, kind: DecoyKind, owner: EntityId, pos: DVec3) -> EntityId {
        if !pos.is_finite() { return EntityId::NONE; }
        let id = self.entities.spawn(crate::entity::EntityKind::Decoy);
        if id.is_none() { return id; }
        self.decoys.insert(id, kind, owner, pos);
        self.events.push(PhysEvent::DecoyDeployed { decoy: id, kind, owner, pos });
        id
    }
    /// Remove any entity now (vessel leaving the map, scripted removal). Unit: `sim-core-world`.
    ///
    /// Queues `VesselDespawned`, `WreckDespawned`, `BodyDestroyed` (not shot down) or `DecoyExpired`. A stale
    /// id does nothing.
    pub fn despawn(&mut self, id: EntityId) {
        use crate::entity::EntityKind;
        let Some(kind) = self.entities.kind(id) else { return };
        // An entity already waiting in the despawn queue has reported its end.
        let reported = self.doomed.contains(&id);
        match kind {
            _ if reported => {}
            EntityKind::Vessel => self.events.push(PhysEvent::VesselDespawned { vessel: id }),
            EntityKind::Wreck => self.events.push(PhysEvent::WreckDespawned { wreck: id }),
            EntityKind::Body => {
                let pos = self.bodies.row_of(id).and_then(|r| self.bodies.pos.get(r).copied()).unwrap_or(DVec3::ZERO);
                self.events.push(PhysEvent::BodyDestroyed { body: id, pos, shot_down: false, by: EntityId::NONE });
            }
            EntityKind::Decoy => self.events.push(PhysEvent::DecoyExpired { decoy: id }),
        }
        self.free_entity(id);
        self.doomed.retain(|d| *d != id);
    }

    /// Drop every store's rows of an entity and free its slot. No event.
    pub(crate) fn free_entity(&mut self, id: EntityId) {
        use crate::entity::EntityKind;
        match self.entities.kind(id) {
            Some(EntityKind::Vessel | EntityKind::Wreck) => {
                if let Some(v) = self.vessels.remove(id) { self.physics.remove(v.body); }
                self.split_jobs.remove(id);
                self.prefabs.remove(id);
                self.aux.remove(id);
                self.touching.retain(|(a, b)| *a != id && *b != id);
            }
            Some(EntityKind::Body) => self.bodies.remove(id),
            Some(EntityKind::Decoy) => self.decoys.remove(id),
            None => return,
        }
        self.entities.despawn(id);
    }

    // ---- Inputs from sim-systems (plain data) ----
    /// Replace a vessel's controls; they persist until replaced. Unit: `sim-core-world`.
    pub fn set_controls(&mut self, vessel: EntityId, controls: VesselControls) {
        if let Some(v) = self.vessel_mut(vessel) { v.controls = controls; }
    }
    pub fn controls_mut(&mut self, vessel: EntityId) -> Option<&mut VesselControls> { self.vessel_mut(vessel).map(|v| &mut v.controls) }
    /// Set a hull's fidelity (L1542-1549). Switching to or from kinematic keeps pose and velocity continuous.
    pub fn set_fidelity(&mut self, vessel: EntityId, fidelity: Fidelity) {
        let Some(v) = self.vessel_mut(vessel) else { return };
        if v.fidelity == fidelity { return; }
        let (was, now) = (v.fidelity == Fidelity::Kinematic, fidelity == Fidelity::Kinematic);
        v.fidelity = fidelity;
        let (body, kin) = (v.body, v.kin);
        // A 30 Hz hull may be sitting out this tick: wake it before its type or rate changes.
        self.physics.set_enabled(body, true);
        if was != now {
            self.physics.set_kinematic(body, now);
            if now { self.physics.set_kinematics(body, &kin); }
        }
    }
    /// Mark a hull as having struck its colours (counts as Disabled, L537).
    pub fn set_struck(&mut self, vessel: EntityId, struck: bool) {
        if let Some(v) = self.vessel_mut(vessel) { v.struck = struck; }
    }
    /// Change side (prize captured by boarding, defector at rendezvous).
    ///
    /// A vessel or a body changes side itself; a decoy follows its owner and has no side of its own.
    pub fn set_side(&mut self, entity: EntityId, side: SideId) {
        if let Some(v) = self.vessel_mut(entity) { v.side = side; return; }
        if !self.entities.is_alive(entity) { return; }
        if let Some(r) = self.bodies.row_of(entity) { if let Some(s) = self.bodies.side.get_mut(r) { *s = side; } }
    }
    /// Guidance output for one body; persists until replaced.
    pub fn steer_body(&mut self, body: EntityId, steer: Steer) {
        if !self.entities.is_alive(body) { return; }
        if let Some(r) = self.bodies.row_of(body) { if let Some(s) = self.bodies.steer.get_mut(r) { *s = steer; } }
    }
    pub fn set_body_radiating(&mut self, body: EntityId, radiating: bool) {
        if !self.entities.is_alive(body) { return; }
        if let Some(r) = self.bodies.row_of(body) { if let Some(s) = self.bodies.radiating.get_mut(r) { *s = radiating; } }
    }
    /// Projectile live-cap rung 0..=2 selecting 2,000 / 1,500 / 1,000 (L557).
    pub fn set_projectile_cap_rung(&mut self, rung: u8) { self.projectile_cap = live_cap(&self.config.catalogue.tuning, rung); }
    /// Change weather; regenerates the wave set and queues `WeatherChanged`.
    ///
    /// The wave seed stays the sortie's, so the sea changes state without becoming a different sea. Setting
    /// the weather already in force does nothing.
    pub fn set_weather(&mut self, weather: WeatherState) {
        if weather == self.weather { return; }
        self.weather = weather;
        let seed = wave_seed(self.config.seed);
        self.waves = WaveSet::generate(seed, &self.weather, &self.config.catalogue.tuning.waves);
        self.events.push(PhysEvent::WeatherChanged { weather, wave_seed: seed });
    }
    /// Move a vessel (rejoin teleport, scripted placement). Unit: `sim-core-world`.
    ///
    /// `kin.pose` is the pose of the grid origin and the velocities are the centre of mass's. Non-finite
    /// input is ignored.
    pub fn teleport(&mut self, vessel: EntityId, kin: &Kinematics) {
        if !(kin.pose.pos.is_finite() && kin.pose.rot.is_finite() && kin.pose.rot.length_squared() > 1e-12 && kin.lin_vel.is_finite() && kin.ang_vel.is_finite()) { return; }
        let extent = self.aux.get(vessel).and_then(|a| a.extent);
        let Some(v) = self.vessel_mut(vessel) else { return };
        v.kin = Kinematics { pose: crate::math::Pose::new(kin.pose.pos, kin.pose.rot.normalize()), lin_vel: kin.lin_vel, ang_vel: kin.ang_vel };
        refresh_bounds(v, extent);
        let (body, kin) = (v.body, v.kin);
        self.physics.set_kinematics(body, &kin);
    }
    /// Repair or rearm in place (splash rewind edits, tender scenes): reset damage, fires, water and loads.
    ///
    /// The hull returns to its prefab: every cell, part, compartment, conduit and load as designed, crew at
    /// full strength, pending structure work dropped, status re-evaluated from scratch. Pose, velocity,
    /// controls, side and fidelity are kept. Wreck pieces that split from it are removed (their cells are back
    /// on the hull), each with `WreckDespawned`. A wreck piece itself cannot be restored.
    pub fn restore_vessel(&mut self, vessel: EntityId) {
        let Some(prefab) = self.prefabs.get(vessel).cloned() else { return };
        let cat = self.config.catalogue.clone();
        let tick = self.tick.0;
        let Some(v) = self.vessel_mut(vessel) else { return };
        if v.parent.is_some() || v.status == crate::vessel::VesselStatus::Wreck { return; }
        v.grid = prefab.grid.clone();
        v.parts = prefab.parts.clone();
        v.compartments = prefab.compartments.clone();
        v.networks = prefab.networks.clone();
        v.crew = crate::crew::CrewState { passengers: v.crew.passengers, veterancy: v.crew.veterancy, ..crate::crew::build_crew(&prefab, &cat.tuning.crew) };
        v.dry_mass = prefab.dry_mass;
        v.mass = crate::mass::with_fluids(&v.dry_mass, &v.parts, &v.compartments, &cat);
        v.cuboids = prefab.cuboids.clone();
        v.blocks = prefab.blocks.clone();
        v.root_cell = prefab.root_cell;
        v.dims = prefab.dims;
        v.waterline_y = prefab.waterline_y;
        v.status = crate::vessel::VesselStatus::Active;
        v.capsize_ticks = 0;
        v.struck = false;
        v.pending_destroyed.clear();
        // The client rebuilds this hull's meshes from the restored grid; the feeds restart empty.
        v.grid.changes.clear();
        v.grid.dirty_render.clear();
        let (body, mass) = (v.body, v.mass);
        self.physics.set_collider(body, &prefab.cuboids);
        self.physics.set_mass(body, &mass);
        self.split_jobs.remove(vessel);
        // A lost hull the wreck rules had already named stays: it is a ship again.
        self.doomed.retain(|d| *d != vessel);
        self.aux.insert(vessel, VesselAux { rebuilt_tick: tick, pushed_mass: Some(mass), ..Default::default() });
        let pieces: Vec<EntityId> = self.vessels.iter().filter(|(_, w)| w.parent == Some(vessel)).map(|(_, w)| w.id).collect();
        for p in pieces { self.despawn(p); }
    }

    // ---- Stepping ----
    /// Advance one 60 Hz tick in the documented order. Must be called from inside the sim rayon pool
    /// (3 workers); never spawns threads itself. Unit: `sim-core-world`.
    pub fn step(&mut self) { step::step(self) }
    /// Take the events queued since the last drain, in emission order.
    pub fn drain_events(&mut self) -> Vec<PhysEvent> { core::mem::take(&mut self.events) }

    // ---- Read access ----
    pub fn tick(&self) -> Tick { self.tick }
    pub fn time_s(&self) -> f64 { self.tick.seconds() }
    pub fn seed(&self) -> u64 { self.config.seed }
    pub fn catalogue(&self) -> &Catalogue { &self.config.catalogue }
    pub fn tuning(&self) -> &TuningTable { &self.config.catalogue.tuning }
    pub fn entities(&self) -> &EntityTable { &self.entities }
    /// The live vessel or wreck behind an id. Checked against the entity table, so a stale id whose slot
    /// was reused after a despawn returns `None` rather than the new occupant.
    pub fn vessel(&self, id: EntityId) -> Option<&Vessel> {
        if !self.entities.is_alive(id) { return None; }
        self.vessels.get(id).filter(|v| v.id == id)
    }
    /// Vessels (including wrecks) in ascending slot order.
    pub fn vessels(&self) -> crate::entity::ColumnIter<'_, Vessel> { self.vessels.iter() }
    pub fn projectiles(&self) -> &Projectiles { &self.projectiles }
    pub fn bodies(&self) -> &Bodies { &self.bodies }
    pub fn decoys(&self) -> &Decoys { &self.decoys }
    pub fn waves(&self) -> &WaveSet { &self.waves }
    pub fn weather(&self) -> &WeatherState { &self.weather }
    pub fn terrain(&self) -> &Terrain { &self.terrain }
    pub fn work(&self) -> &WorkCounters { &self.work }
    /// Sea surface height at world (x, z) now.
    pub fn water_height(&self, x: f64, z: f64) -> f64 { self.waves.height_at(x, z, self.time_s()) }
}

// Compile-time contract: the world moves to the sim thread and prefabs are shared across workers.
const _: fn() = || {
    fn assert_send<T: Send>() {}
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send::<World>();
    assert_send_sync::<VesselPrefab>();
    assert_send_sync::<Catalogue>();
};
