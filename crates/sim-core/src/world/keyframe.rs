//! Unit `sim-core-keyframe` (hard: must restore every store exactly and rebuild Rapier in canonical order).
//! World keyframes for the 60 s operation checkpoint, suspend saves, co-op host migration and the 30 s splash
//! rewind. DESIGN.md L112, L843 (checkpoints), L216 (rewind), L1152 (canonical body insertion order).
//!
//! # Encoding
//! `"BPKF"`, the format number as a little-endian `u32`, then one bincode value (variable-length integers).
//! A build restores only its own format: there is no migration, because a keyframe never outlives the build
//! that wrote it (a suspend save from an older build is refused with [`KeyframeError::Format`]).
//!
//! What is stored, and what is rebuilt:
//! - The entity table, the flat stores (`Projectiles`, `Bodies`, `Decoys`, whole through serde, private
//!   columns included), split jobs, weather, the wave set, the queued events and every world counter go in
//!   as they are.
//! - Each vessel stores its blueprint hash and all mutable state. Its cell grid is the damage array (run
//!   lengths), scorch, the sealed-air bits and the dirty chunk marks; chunks, interned types and line extents
//!   are rebuilt from the prefab by removing the cells at bucket 8. Compartments, cuboids, buoyancy blocks
//!   and sealed-air bits that still equal the prefab's are stored as "as designed", so an undamaged hull
//!   costs little more than its parts.
//! - Not stored: Rapier (bodies are re-created in ascending slot order from pose, velocities, mass and
//!   cuboids), terrain (built from the config's map), the local remesh feeds (`VesselGrid::changes`,
//!   `dirty_render`: a client rebuilds its meshes after a restore) and derived caches.
//!
//! The sortie seed in the keyframe wins over the config's: the state continues the random streams it was
//! saved from. The map and catalogue come from the config.
//!
//! # Bad input
//! Keyframe bytes arrive from disk and, in host migration, from the network. Decoding is bounded by
//! [`MAX_KEYFRAME_BYTES`]; every array that is indexed by cell or slot is checked against its grid, and every
//! flat store (small bodies, decoys, projectiles) must have columns of one length, ids in order and finite
//! positions and velocities. A mismatch is [`KeyframeError::Corrupt`]. Nothing
//! here panics on any byte string, and a world that restores can be stepped.
use super::{refresh_bounds, KeyframeError, VesselAux, WorkCounters, World, WorldConfig, KEYFRAME_FORMAT};
use crate::bits::BitSet;
use crate::compartments::CompartmentSet;
use crate::crew::CrewState;
use crate::entity::{Column, EntityId, EntityKind, EntityTable, MAX_ENTITY_SLOTS};
use crate::events::PhysEvent;
use crate::hydro::BuoyancyBlocks;
use crate::mass::MassProperties;
use crate::math::Kinematics;
use crate::munitions::{Bodies, Decoys};
use crate::networks::Networks;
use crate::parts::{PartRuntime, VesselControls};
use crate::physics::{CuboidLocal, PhysicsWorld};
use crate::projectiles::{BodyGraze, Projectiles};
use crate::structure::SplitJob;
use crate::terrain::Terrain;
use crate::time::Tick;
use crate::vessel::{Fidelity, Vessel, VesselPrefab, VesselStatus};
use crate::waves::WaveSet;
use bincode::Options;
use content::cell::BUCKET_DESTROYED;
use content::ids::{CellIndex, ClassId, ContentHash, PlayerSlot, SideId};
use content::weather::WeatherState;
use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use std::sync::Arc;

/// First bytes of every keyframe.
pub const KEYFRAME_MAGIC: [u8; 4] = *b"BPKF";
/// Decoding refuses to read or allocate past this many bytes (256 MiB; a 44-hull battle is a few megabytes).
pub const MAX_KEYFRAME_BYTES: u64 = 256 << 20;
/// No sortie runs this long (2^40 ticks is about 580 years): a later tick is damage, and is refused before it
/// can overflow the counter.
const MAX_KEYFRAME_TICK: u64 = 1 << 40;

/// A vessel's cell grid as stored: everything that is not a function of the design.
#[derive(Serialize, Deserialize)]
struct GridFrame<'a> {
    /// Run lengths of the damage array: (cells, bucket).
    damage: Vec<(u32, u8)>,
    scorch: Cow<'a, [u8]>,
    /// `None` = as designed.
    sealed_air: Option<Cow<'a, [u64]>>,
    original_count: u32,
    dirty_chunks: Cow<'a, BitSet>,
}

#[derive(Serialize, Deserialize)]
struct VesselFrame<'a> {
    id: EntityId,
    side: SideId,
    player: Option<PlayerSlot>,
    class: Option<ClassId>,
    blueprint: ContentHash,
    name: Cow<'a, str>,
    status: VesselStatus,
    fidelity: Fidelity,
    grid: GridFrame<'a>,
    parts: Cow<'a, [PartRuntime]>,
    /// `None` = as designed (dry, cold, sealed).
    compartments: Option<Cow<'a, CompartmentSet>>,
    networks: Cow<'a, Networks>,
    crew: Cow<'a, CrewState>,
    controls: Cow<'a, VesselControls>,
    dry_mass: MassProperties,
    mass: MassProperties,
    cuboids: Option<Cow<'a, [CuboidLocal]>>,
    blocks: Option<Cow<'a, BuoyancyBlocks>>,
    kin: Kinematics,
    root_cell: CellIndex,
    dims: [f64; 3],
    waterline_y: f64,
    last_fired_tick: u64,
    capsize_ticks: u32,
    struck: bool,
    pending_destroyed: Cow<'a, [CellIndex]>,
    parent: Option<EntityId>,
    spawn_tick: u64,
    submersible: bool,
    tonnage_t: f64,
    aux: Cow<'a, VesselAux>,
}

#[derive(Serialize, Deserialize)]
struct Frame<'a> {
    seed: u64,
    tick: Tick,
    weather: WeatherState,
    waves: WaveSet,
    entities: Cow<'a, EntityTable>,
    /// Ascending slot.
    vessels: Vec<VesselFrame<'a>>,
    split_jobs: Cow<'a, Column<SplitJob>>,
    projectiles: Cow<'a, Projectiles>,
    bodies: Cow<'a, Bodies>,
    decoys: Cow<'a, Decoys>,
    events: Cow<'a, [PhysEvent]>,
    split_cursor: u32,
    projectile_cap: u64,
    work: WorkCounters,
    grazes: Cow<'a, [BodyGraze]>,
    doomed: Cow<'a, [EntityId]>,
    touching: Cow<'a, [(EntityId, EntityId)]>,
}

fn run_lengths(damage: &[u8]) -> Vec<(u32, u8)> {
    let mut runs: Vec<(u32, u8)> = Vec::new();
    for &b in damage {
        match runs.last_mut() {
            Some((n, v)) if *v == b && *n < u32::MAX => *n += 1,
            _ => runs.push((1, b)),
        }
    }
    runs
}

fn corrupt(what: impl Into<String>) -> KeyframeError { KeyframeError::Corrupt(what.into()) }

fn finite_mass(m: &MassProperties) -> bool {
    m.mass_kg.is_finite() && m.mass_kg > 0.0 && m.com.is_finite() && m.inertia.x_axis.is_finite() && m.inertia.y_axis.is_finite() && m.inertia.z_axis.is_finite()
}

fn finite_kin(k: &Kinematics) -> bool {
    k.pose.pos.is_finite() && k.pose.rot.is_finite() && (k.pose.rot.length_squared() - 1.0).abs() < 1e-3 && k.lin_vel.is_finite() && k.ang_vel.is_finite()
}

fn steer_finite(s: &crate::munitions::Steer) -> bool {
    use crate::munitions::Steer;
    match s {
        Steer::Hold | Steer::Detonate => true,
        Steer::Accel(a) => a.is_finite(),
        Steer::Waypoint(p) => p.is_finite(),
        Steer::Orbit { centre, radius_m } => centre.is_finite() && radius_m.is_finite(),
    }
}

/// The flat stores, the sea and the clock as decoded: every column of a store as long as its id column,
/// every position and velocity finite, ids ascending. The stores index their columns by row, so a ragged or
/// non-finite one is refused here and never reaches a tick. (A body of a kind the catalogue does not have is
/// not refused: the store gives it no hit points and it ends on its first step, as one spawned so does.)
fn check_stores(frame: &Frame<'_>) -> Result<(), KeyframeError> {
    let all3 = |v: &[glam::DVec3]| v.iter().all(|p| p.is_finite());
    let all1 = |v: &[f64]| v.iter().all(|x| x.is_finite());
    if frame.tick.0 > MAX_KEYFRAME_TICK { return Err(corrupt("tick is out of range")); }

    let b = &*frame.bodies;
    let n = b.entity.len();
    let lens = [b.kind.len(), b.side.len(), b.pos.len(), b.vel.len(), b.hp.len(), b.phase.len(), b.steer.len(), b.launcher.len(), b.launcher_slot.len(), b.spawn_tick.len(), b.remaining.len(), b.set_depth_m.len(), b.radiating.len()];
    // The stores pad their private flag columns on their next step, so a short one is not damage.
    if lens.iter().any(|&l| l != n) || b.cleared.len() > n { return Err(corrupt("small-body columns differ in length")); }
    if !(all3(&b.pos) && all3(&b.vel) && all1(&b.remaining) && all1(&b.set_depth_m) && b.steer.iter().all(steer_finite)) { return Err(corrupt("small-body state is not finite")); }

    let d = &*frame.decoys;
    let n = d.entity.len();
    if [d.kind.len(), d.owner.len(), d.pos.len(), d.age_s.len()].iter().any(|&l| l != n) || d.lost.len() > n { return Err(corrupt("decoy columns differ in length")); }
    if !(all3(&d.pos) && all1(&d.age_s)) { return Err(corrupt("decoy state is not finite")); }

    let p = &*frame.projectiles;
    let n = p.id.len();
    let lens = [p.pos.len(), p.vel.len(), p.band.len(), p.calibre_mm.len(), p.shooter.len(), p.mount.len(), p.spawn_tick.len(), p.v_1km.len(), p.path_m.len(), p.rounds.len(), p.point_defence.len()];
    if n > crate::projectiles::MAX_LIVE_HARD || lens.iter().any(|&l| l != n) { return Err(corrupt("projectile columns differ in length or pass the live cap")); }
    if !p.id.windows(2).all(|w| w[0] < w[1]) { return Err(corrupt("projectiles are not in spawn order")); }
    if !(all3(&p.pos) && all3(&p.vel) && all1(&p.v_1km) && all1(&p.path_m)) { return Err(corrupt("projectile state is not finite")); }
    if !p.sphere_prev.windows(2).all(|w| w[0].0 < w[1].0) || !p.sphere_prev.iter().all(|s| s.1.is_finite()) { return Err(corrupt("projectile sweep cache is inconsistent")); }

    let w = &frame.waves;
    if w.count as usize > crate::waves::MAX_WAVES { return Err(corrupt("wave set has too many components")); }
    if !w.waves.iter().all(|g| g.dir.is_finite() && g.k.is_finite() && g.omega.is_finite() && g.amplitude.is_finite() && g.q.is_finite() && g.phase.is_finite()) { return Err(corrupt("wave set is not finite")); }
    Ok(())
}

fn save_vessel<'a>(v: &'a Vessel, prefab: Option<&VesselPrefab>, aux: &'a VesselAux) -> VesselFrame<'a> {
    let g = &v.grid;
    VesselFrame {
        id: v.id,
        side: v.side,
        player: v.player,
        class: v.class.clone(),
        blueprint: v.blueprint,
        name: Cow::Borrowed(&v.name),
        status: v.status,
        fidelity: v.fidelity,
        grid: GridFrame {
            damage: run_lengths(&g.damage),
            scorch: Cow::Borrowed(&g.scorch),
            sealed_air: if prefab.is_some_and(|p| p.grid.sealed_air == g.sealed_air) { None } else { Some(Cow::Borrowed(&g.sealed_air)) },
            original_count: g.original_count,
            dirty_chunks: Cow::Borrowed(&g.dirty_chunks),
        },
        parts: Cow::Borrowed(&v.parts),
        compartments: if prefab.is_some_and(|p| p.compartments == v.compartments) { None } else { Some(Cow::Borrowed(&v.compartments)) },
        networks: Cow::Borrowed(&v.networks),
        crew: Cow::Borrowed(&v.crew),
        controls: Cow::Borrowed(&v.controls),
        dry_mass: v.dry_mass,
        mass: v.mass,
        cuboids: if prefab.is_some_and(|p| p.cuboids == v.cuboids) { None } else { Some(Cow::Borrowed(&v.cuboids)) },
        blocks: if prefab.is_some_and(|p| p.blocks == v.blocks) { None } else { Some(Cow::Borrowed(&v.blocks)) },
        kin: v.kin,
        root_cell: v.root_cell,
        dims: v.dims,
        waterline_y: v.waterline_y,
        last_fired_tick: v.last_fired_tick,
        capsize_ticks: v.capsize_ticks,
        struck: v.struck,
        pending_destroyed: Cow::Borrowed(&v.pending_destroyed),
        parent: v.parent,
        spawn_tick: v.spawn_tick,
        submersible: v.submersible,
        tonnage_t: v.tonnage_t,
        aux: Cow::Borrowed(aux),
    }
}

/// Rebuild one vessel from its frame and prefab, checking every cell- and slot-indexed array.
fn load_vessel(f: VesselFrame<'_>, prefab: &VesselPrefab, part_kinds: usize) -> Result<(Vessel, VesselAux), KeyframeError> {
    let who = f.id.index;
    let bad = |what: &str| corrupt(format!("vessel {who}: {what}"));
    let mut grid = prefab.grid.clone();
    let n = grid.design.cells.len();
    if grid.damage.len() != n { return Err(bad("its blueprint has no usable grid")); }
    // Damage: run lengths back to one bucket per cell.
    let mut damage: Vec<u8> = Vec::with_capacity(n);
    for (run, bucket) in f.grid.damage {
        if bucket > BUCKET_DESTROYED || run as usize > n - damage.len() { return Err(bad("damage does not fit its grid")); }
        damage.resize(damage.len() + run as usize, bucket);
    }
    if damage.len() != n { return Err(bad("damage does not fit its grid")); }
    let removed: Vec<CellIndex> = damage.iter().enumerate().filter(|(_, &b)| b == BUCKET_DESTROYED).map(|(i, _)| CellIndex(i as u32)).collect();
    grid.remove_cells(&removed);
    grid.damage = damage;
    let scorch = f.grid.scorch.into_owned();
    if !(scorch.is_empty() || scorch.len() == n) { return Err(bad("scorch does not fit its grid")); }
    grid.scorch = scorch;
    if let Some(bits) = f.grid.sealed_air {
        if !(bits.is_empty() || bits.len() == n.div_ceil(64)) { return Err(bad("sealed-air bits do not fit its grid")); }
        grid.sealed_air = bits.into_owned();
    }
    let dirty = f.grid.dirty_chunks.into_owned();
    if dirty.len != grid.dirty_chunks.len || dirty.words.len() != grid.dirty_chunks.words.len() { return Err(bad("chunk marks do not fit its grid")); }
    grid.dirty_chunks = dirty;
    grid.original_count = f.grid.original_count;
    grid.changes.clear();
    grid.dirty_render.clear();

    let in_grid = |c: &CellIndex| c.index() < n;
    let parts = f.parts.into_owned();
    // A hull's parts are indexed by slot. A wreck piece keeps the parts it carried under their original slots,
    // in that order.
    let slots_ok = if f.parent.is_none() { parts.iter().enumerate().all(|(i, p)| p.slot.index() == i) } else { parts.windows(2).all(|w| w[0].slot.0 < w[1].slot.0) };
    if parts.len() > u16::MAX as usize || !slots_ok || parts.iter().any(|p| p.def.index() >= part_kinds || !p.cells.iter().all(in_grid) || !p.health.is_finite() || !p.centre.is_finite()) {
        return Err(bad("part list is inconsistent"));
    }
    let compartments = match f.compartments {
        None => prefab.compartments.clone(),
        Some(c) => {
            let c = c.into_owned();
            let count = c.compartments.len();
            let ok = c.cell_to_compartment.len() == n
                && c.compartments.iter().enumerate().all(|(i, k)| k.id.index() == i && k.cells.iter().all(in_grid) && k.water_m3.is_finite() && k.volume_m3.is_finite() && k.fire.is_finite())
                && c.openings.iter().all(|o| in_grid(&o.cell) && o.a.index() < count && o.b.is_none_or(|b| b.index() < count) && o.area_m2.is_finite())
                && c.adjacency.iter().all(|a| a.a.index() < count && a.b.index() < count);
            if !ok { return Err(bad("compartments do not fit its grid")); }
            c
        }
    };
    if parts.iter().any(|p| p.compartment.is_some_and(|c| c.index() >= compartments.compartments.len())) { return Err(bad("a part sits in a compartment the hull does not have")); }
    let networks = f.networks.into_owned();
    if !networks.conduits.iter().all(|c| c.cells.iter().all(in_grid)) { return Err(bad("conduits leave its grid")); }
    let size = grid.size();
    let cuboids = match f.cuboids {
        None => prefab.cuboids.clone(),
        Some(c) => {
            if !c.iter().all(|c| (0..3).all(|k| c.min[k] as u32 + c.size[k] as u32 <= size[k] as u32)) { return Err(bad("collider leaves its grid")); }
            c.into_owned()
        }
    };
    let blocks = match f.blocks {
        None => prefab.blocks.clone(),
        Some(b) => {
            let ok = b.blocks.iter().all(|k| (k.column as usize) < b.columns.len() && k.centre.is_finite() && k.volume_m3.is_finite() && k.height_m.is_finite())
                && b.columns.iter().all(|c| c[0].is_finite() && c[1].is_finite())
                && (b.ext.is_empty() || b.ext.len() == b.blocks.len());
            if !ok { return Err(bad("buoyancy blocks are inconsistent")); }
            b.into_owned()
        }
    };
    if !finite_kin(&f.kin) || !finite_mass(&f.mass) || !finite_mass(&f.dry_mass) || !f.waterline_y.is_finite() || !f.tonnage_t.is_finite() || !f.dims.iter().all(|d| d.is_finite()) {
        return Err(bad("kinematics or mass are not finite"));
    }
    let pending = f.pending_destroyed.into_owned();
    if !pending.iter().all(in_grid) || (n > 0 && !in_grid(&f.root_cell)) { return Err(bad("cell lists leave its grid")); }
    let aux = f.aux.into_owned();
    if aux.extent.is_some_and(|e| !(e[0].is_finite() && e[1].is_finite())) { return Err(bad("extent is not finite")); }
    let mut vessel = Vessel {
        id: f.id,
        side: f.side,
        player: f.player,
        class: f.class,
        blueprint: f.blueprint,
        name: f.name.into_owned(),
        status: f.status,
        fidelity: f.fidelity,
        grid,
        parts,
        compartments,
        networks,
        crew: f.crew.into_owned(),
        controls: f.controls.into_owned(),
        dry_mass: f.dry_mass,
        mass: f.mass,
        body: crate::physics::BodyHandle::NONE,
        cuboids,
        blocks,
        // As saved, bit for bit (checked unit to 1e-3 above): normalising again can move the last bit, and the
        // restored world must hash as the saved one did.
        kin: f.kin,
        bounds: crate::math::Aabb::default(),
        root_cell: f.root_cell,
        dims: f.dims,
        waterline_y: f.waterline_y,
        last_fired_tick: f.last_fired_tick,
        capsize_ticks: f.capsize_ticks,
        struck: f.struck,
        pending_destroyed: pending,
        parent: f.parent,
        spawn_tick: f.spawn_tick,
        submersible: f.submersible,
        tonnage_t: f.tonnage_t,
    };
    refresh_bounds(&mut vessel, aux.extent);
    Ok((vessel, aux))
}

impl World {
    /// Serialise the whole world for the 60 s checkpoint, suspend saves, host migration and the 30 s splash
    /// rewind. DECISION: Rapier state is not serialised (its serde feature is off). The keyframe stores, per
    /// vessel in ascending slot order, the blueprint hash plus all mutable state (damage buckets, parts,
    /// compartments, networks, crew, controls, kinematics, split job) and all flat arrays.
    /// Unit: `sim-core-keyframe`.
    ///
    /// See the module docs for the encoding. Returns an empty vector only if encoding fails, which no world
    /// built through this crate's API can cause; an empty keyframe restores as [`KeyframeError::Corrupt`].
    pub fn save_keyframe(&self) -> Vec<u8> {
        let no_aux = VesselAux::default();
        let frame = Frame {
            seed: self.config.seed,
            tick: self.tick,
            weather: self.weather,
            waves: self.waves,
            entities: Cow::Borrowed(&self.entities),
            vessels: self.vessels.iter().map(|(_, v)| save_vessel(v, self.prefabs.get(v.id).map(|p| &**p), self.aux.get(v.id).unwrap_or(&no_aux))).collect(),
            split_jobs: Cow::Borrowed(&self.split_jobs),
            projectiles: Cow::Borrowed(&self.projectiles),
            bodies: Cow::Borrowed(&self.bodies),
            decoys: Cow::Borrowed(&self.decoys),
            events: Cow::Borrowed(&self.events),
            split_cursor: self.split_cursor,
            projectile_cap: self.projectile_cap as u64,
            work: self.work,
            grazes: Cow::Borrowed(&self.grazes),
            doomed: Cow::Borrowed(&self.doomed),
            touching: Cow::Borrowed(&self.touching),
        };
        let mut out = Vec::with_capacity(1 << 16);
        out.extend_from_slice(&KEYFRAME_MAGIC);
        out.extend_from_slice(&KEYFRAME_FORMAT.to_le_bytes());
        match bincode::DefaultOptions::new().serialize_into(&mut out, &frame) {
            Ok(()) => out,
            Err(_) => Vec::new(),
        }
    }

    /// Rebuild a world from a keyframe. Prefabs are looked up by blueprint hash; bodies are re-created in
    /// ascending slot order with saved pose and velocities. Solver warm-start caches are lost, so a restored
    /// world is not bit-identical to the uninterrupted one from that tick on; replays and netcode do not rely
    /// on that (L1160), and same-platform determinism tests never cross a restore. Unit: `sim-core-keyframe`.
    ///
    /// The restored world has the same state hash as the saved one. Every 30 Hz hull runs its next physics
    /// step as a fresh one (a tick it owed at the save is not made up).
    pub fn restore_keyframe(config: WorldConfig, bytes: &[u8], prefabs: &dyn Fn(ContentHash) -> Option<Arc<VesselPrefab>>) -> Result<World, KeyframeError> {
        if bytes.len() < 8 || bytes[..4] != KEYFRAME_MAGIC { return Err(corrupt("not a keyframe")); }
        let found = u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]);
        if found != KEYFRAME_FORMAT { return Err(KeyframeError::Format { found, expected: KEYFRAME_FORMAT }); }
        let frame: Frame<'_> = bincode::DefaultOptions::new().with_limit(MAX_KEYFRAME_BYTES).deserialize(&bytes[8..]).map_err(|e| corrupt(e.to_string()))?;

        check_stores(&frame)?;
        let entities = frame.entities.into_owned();
        let slots = entities.generations.len();
        if slots > MAX_ENTITY_SLOTS as usize || entities.kinds.len() > slots || entities.alive.len > slots || entities.alive.words.len() != entities.alive.len.div_ceil(64) || entities.free.len() > slots {
            return Err(corrupt("entity table is inconsistent"));
        }
        let mut world = World {
            tick: frame.tick,
            entities,
            vessels: Column::new(),
            split_jobs: Column::new(),
            projectiles: frame.projectiles.into_owned(),
            bodies: frame.bodies.into_owned(),
            decoys: frame.decoys.into_owned(),
            physics: PhysicsWorld::new(),
            waves: frame.waves,
            weather: frame.weather,
            terrain: Terrain::new(config.map.clone()),
            events: frame.events.into_owned(),
            split_cursor: frame.split_cursor,
            projectile_cap: (frame.projectile_cap as usize).clamp(1, crate::projectiles::MAX_LIVE_HARD),
            work: frame.work,
            prefabs: Column::new(),
            aux: Column::new(),
            grazes: frame.grazes.into_owned(),
            doomed: frame.doomed.into_owned(),
            touching: frame.touching.into_owned(),
            config: WorldConfig { seed: frame.seed, ..config },
        };
        // Bodies and decoys must be rows of live entities of the right kind, ascending.
        let rows_ok = |ids: &[EntityId], kind: EntityKind| ids.windows(2).all(|p| p[0] < p[1]) && ids.iter().all(|&id| world.entities.kind(id) == Some(kind));
        if !rows_ok(&world.bodies.entity, EntityKind::Body) || !rows_ok(&world.decoys.entity, EntityKind::Decoy) { return Err(corrupt("small-body rows do not match the entity table")); }

        // Vessels in ascending slot order: that is the order their bodies enter Rapier (L1152).
        let mut last: Option<u32> = None;
        for f in frame.vessels {
            let id = f.id;
            if last.is_some_and(|l| id.index <= l) { return Err(corrupt("vessels are not in ascending slot order")); }
            last = Some(id.index);
            if !matches!(world.entities.kind(id), Some(EntityKind::Vessel | EntityKind::Wreck)) { return Err(corrupt(format!("vessel {} is not a live vessel entity", id.index))); }
            let prefab = prefabs(f.blueprint).ok_or(KeyframeError::MissingPrefab(f.blueprint))?;
            let (mut vessel, mut aux) = load_vessel(f, &prefab, world.config.catalogue.parts.len())?;
            vessel.body = world.physics.add_vessel(vessel.kin.pose, vessel.kin.lin_vel, &vessel.mass, &vessel.cuboids);
            // Angular velocity, and the body type of a kinematic hull.
            world.physics.set_kinematics(vessel.body, &vessel.kin);
            if vessel.fidelity == Fidelity::Kinematic {
                world.physics.set_kinematic(vessel.body, true);
                world.physics.set_kinematics(vessel.body, &vessel.kin);
            }
            aux.pushed_mass = Some(vessel.mass);
            world.vessels.insert(id, vessel);
            world.prefabs.insert(id, prefab);
            world.aux.insert(id, aux);
        }
        // Every vessel entity must have come with a vessel.
        for kind in [EntityKind::Vessel, EntityKind::Wreck] {
            if world.entities.iter_kind(kind).any(|id| !world.vessels.contains(id)) { return Err(corrupt("a vessel entity has no vessel")); }
        }
        // Split jobs belong to live vessels; a job for anything else is dropped.
        let jobs = frame.split_jobs.into_owned();
        for (slot, job) in jobs.slots.into_iter().enumerate() {
            let (Some(job), Some(id)) = (job, world.entities.id_at(slot as u32)) else { continue };
            let Some(n) = world.vessels.get(id).map(|v| v.grid.design.cells.len()) else { continue };
            let fits = |cells: &[CellIndex]| cells.iter().all(|c| c.index() < n);
            if !fits(&job.destroyed) || !fits(&job.seeds) || job.visited.len > n + 64 || job.visited.words.len() != job.visited.len.div_ceil(64) || job.labels.len() > n {
                return Err(corrupt(format!("split job of vessel {slot} does not fit its grid")));
            }
            world.split_jobs.insert(id, job);
        }
        let alive = |id: &EntityId| world.entities.is_alive(*id);
        let mut doomed: Vec<EntityId> = world.doomed.iter().copied().filter(alive).collect();
        doomed.sort_unstable();
        doomed.dedup();
        world.doomed = doomed;
        world.touching.sort_unstable();
        world.touching.dedup();
        Ok(world)
    }
}
