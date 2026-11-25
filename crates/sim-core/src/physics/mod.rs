//! Unit `sim-core-physics` (hard: the Rapier seam and determinism of body order).
//! The only module that names Rapier types. DESIGN.md L1100-1105 (Rapier use, compounds vs Voxels),
//! L1131-1146 (f64 world), L1152 (canonical body insertion order).
//!
//! Rules of the seam:
//! - rapier3d-f64 0.36.1 with `parallel` and `enhanced-determinism`; never `simd8`.
//! - A vessel collider is a greedy-merged compound of cuboids, cached and rebuilt when cells change. The spec's
//!   default stands (Milestone 0 spikes are skipped); FALLBACK noted for later: the Voxels collider may replace
//!   compounds only if it is at least 20% faster over a damage soak and keeps identical state hashes (L1102).
//! - Mass properties are always supplied by `mass`; Rapier's automatic mass is disabled.
//! - Bodies are inserted and removed in ascending entity slot order at fixed points of the tick, so handle
//!   allocation is reproducible.
//! - f64 world is the default; FALLBACK noted for later: per-cluster f32 worlds with 1 km snapped origins
//!   (L1143-1146). Nothing outside this module may depend on which is in use.
//! - Keyframes do not serialise Rapier: `World::restore_keyframe` rebuilds every body here in ascending slot
//!   order from saved pose, velocities, mass and cuboids.
//!
//! Implementation notes (binding for callers):
//! - The Rapier body frame is the vessel grid frame: the body pose is the pose of the grid origin and the
//!   compound's cuboids sit at their cell positions. Colliders carry no mass (density 0); the body's mass,
//!   centre of mass and inertia are exactly what `mass` supplies, diagonalised here with a sqrt-only Jacobi
//!   sweep so no platform transcendental enters the state.
//! - Forces are accumulated in this module, not in Rapier, with the torque taken about the centre of mass
//!   last supplied through [`PhysicsWorld::set_mass`]. `set_mass` followed by `add_force_at` in one tick is
//!   therefore exact. [`PhysicsWorld::step`] hands the sums to Rapier and clears them.
//! - 30 Hz hulls: a body skipped with `set_enabled(false)` owes that tick. On its next enabled step the
//!   forces added for that step (and gravity) act for the owed time as well, and after the step its pose
//!   advances by the owed time at the new velocity. One step after one skipped tick is thus a 30 Hz
//!   semi-implicit Euler step to first order, so distant hulls keep the same velocities and cover the same
//!   distance as near ones. Callers must not rescale forces themselves. Forces added for a skipped tick are
//!   dropped.
//! - Kinematic bodies are position-driven: `set_kinematics` gives the pose to reach in the next step and
//!   the velocities to report; switching back to dynamic resumes with those velocities.
//! - Terrain is a set of 256 m heightfield tiles sampled every 8 m, built only where the seabed is shallower
//!   than [`TERRAIN_MAX_DEPTH_M`] within reach of a listed position, cached, and dropped when no listed
//!   position is near. Tiles are created in ascending (z, x) tile order so collider handles are reproducible.
//!   `PhysicsWorld::terrain` holds the lowest-keyed live tile, or `None` when no tile is installed.
//! - No call panics on a stale or `NONE` handle, an empty cuboid list, or non-finite input: such calls do
//!   nothing, and `kinematics` returns the default.
//!
//! Stepping and rayon (binding; the one place that decides how a world's physics meets the thread pools):
//! - Rapier 0.36's parallel step defers a BVH optimisation pass with `rayon::spawn`, which lands on the
//!   calling worker's own deque (or the global pool's queue off-pool), and later waits for it with a plain
//!   channel `recv` that does not help the pool (rapier `pipeline/physics_pipeline/solve.rs`, the spawn under
//!   `rayon::current_num_threads() > 1`; `physics_pipeline/mod.rs`, `join_deferred_bvh_optimize`). Only
//!   another free worker of that pool can run the pass. When as many worlds step at once as their pool has
//!   workers, every worker sits in `recv` and the process deadlocks.
//! - So [`PhysicsWorld::step`] lets at most one Rapier step in the process run in parallel: a step on a pool
//!   of more than one worker takes the process-wide parallel slot without waiting (never a blocking lock:
//!   a worker waiting on it could be the only thread able to run the holder's deferred pass). If the slot is
//!   taken, the step runs on a one-worker pool leased from a small cache, where Rapier runs the pass inline
//!   and its parallel loops serially. A step already on a one-worker pool runs inline as before. The slot
//!   holder's pool then has at most one blocked worker, and its other workers are free or waiting inside
//!   rayon (which steals), so the deferred pass always gets a thread.
//! - Rapier's results do not depend on the pool size (enhanced-determinism, L1097), so which way a step ran
//!   never changes a bit; `physics::tests` checks the serial fallback against the parallel step.
//! - The live sortie, alone on the sim pool, always gets the slot and steps exactly as before. Background
//!   work stepping beside it may take the slot first; the sortie's step then runs serially for that tick.
//! - Nothing else may call `PhysicsPipeline::step`.
use crate::math::{Kinematics, Pose};
use crate::mass::MassProperties;
use glam::DVec3;
use rapier3d_f64::prelude as rp;
use serde::{Deserialize, Serialize};
use std::cell::Cell;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

#[cfg(test)]
mod tests;

/// Opaque body handle (wraps Rapier's rigid-body handle parts).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct BodyHandle { pub index: u32, pub generation: u32 }
impl BodyHandle { pub const NONE: BodyHandle = BodyHandle { index: u32::MAX, generation: 0 }; }

/// One cuboid of a vessel's compound, in whole cells of the vessel-local frame (min corner and extent).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CuboidLocal { pub min: [u16; 3], pub size: [u16; 3] }

/// A contact reported by the step, for collision damage and grounding events.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Contact { pub a: BodyHandle, pub b: Option<BodyHandle>, pub point: DVec3, pub impulse: f64 }

/// Conversions proving glam and Rapier share types: `rapier3d_f64::math::Vector` is `glam::DVec3` and a Rapier
/// pose is (translation `DVec3`, rotation `DQuat`).
pub fn to_rapier_vec(v: DVec3) -> rapier3d_f64::math::Vector { v }
pub fn to_rapier_pose(p: Pose) -> rapier3d_f64::math::Pose { rapier3d_f64::math::Pose::from_parts(p.pos, p.rot) }
pub fn from_rapier_pose(p: &rapier3d_f64::math::Pose) -> Pose { Pose { pos: p.translation, rot: p.rotation } }


/// Standard gravity in m/s^2, acting along -Y. Hydrostatics in `hydro` use the same value.
pub const GRAVITY: f64 = 9.81;
/// Edge of one terrain heightfield tile in metres.
pub const TERRAIN_TILE_M: f64 = 256.0;
/// Sample spacing of a terrain tile in metres.
pub const TERRAIN_STEP_M: f64 = 8.0;
/// Seabed deeper than this everywhere in a tile gets no collider: no hull reaches it afloat, and a wreck
/// sinking in deep water is despawned by the world's wreck rules, not stopped by the bottom.
pub const TERRAIN_MAX_DEPTH_M: f64 = 60.0;
/// Tiles within this distance of a listed position are installed. A position may be any point of its hull,
/// the grid origin included: the longest hull is 150 m, and the rest is margin.
pub const TERRAIN_REACH_M: f64 = 192.0;
/// Tiles farther than this from every listed position are dropped.
const TERRAIN_KEEP_M: f64 = 448.0;
/// Coarse pre-test spacing: a tile whose 5 x 5 coarse samples are all this much deeper than the cut-off is
/// deep without the full sampling (island shelves are at least 300 m wide, so nothing hides between them).
const TERRAIN_COARSE_SLACK_M: f64 = 60.0;
/// Most ticks a skipped body can owe; beyond it the body simply loses time instead of taking one huge step.
const MAX_OWED_TICKS: u32 = 8;
/// Steel on steel and steel on seabed. Restitution is near zero: hulls crunch, they do not bounce.
const HULL_FRICTION: f64 = 0.35;
const HULL_RESTITUTION: f64 = 0.05;
const TERRAIN_FRICTION: f64 = 0.6;

/// How one Rapier step ran (see the module notes on stepping and rayon).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum StepMode {
    /// On a one-worker pool: Rapier runs its deferred BVH pass inline.
    Inline = 0,
    /// On the caller's pool, holding the process-wide parallel slot.
    Parallel = 1,
    /// The slot was taken: on a leased one-worker pool.
    Serial = 2,
}

/// The process-wide parallel slot: true while one Rapier step runs on a pool of more than one worker.
static PARALLEL_STEP: AtomicBool = AtomicBool::new(false);
/// One-worker pools for steps that find the slot taken, returned after each step. The cache grows to the
/// most steps ever contended at once; its threads sleep while unused.
static SERIAL_POOLS: Mutex<Vec<rayon::ThreadPool>> = Mutex::new(Vec::new());
thread_local! {
    /// True on the thread whose step holds the parallel slot, for the debug check in `run_rapier_step`.
    static HOLDS_SLOT: Cell<bool> = const { Cell::new(false) };
}

/// The parallel slot, held for one step and released on drop (a panicking step releases it too).
pub(crate) struct ParallelClaim(());
impl ParallelClaim {
    /// Take the slot if it is free. Never waits.
    pub(crate) fn try_take() -> Option<ParallelClaim> {
        PARALLEL_STEP.compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire).ok().map(|_| ParallelClaim(()))
    }
}
impl Drop for ParallelClaim { fn drop(&mut self) { PARALLEL_STEP.store(false, Ordering::Release); } }

/// Run one Rapier step (`step`) so that its deferred BVH pass always finds a free worker. Unit:
/// `sim-core-physics`.
fn run_rapier_step(step: impl FnOnce() + Send) -> StepMode {
    if rayon::current_num_threads() <= 1 {
        step();
        return StepMode::Inline;
    }
    if let Some(claim) = ParallelClaim::try_take() {
        let outer = HOLDS_SLOT.with(|h| h.replace(true));
        debug_assert!(!outer, "the parallel slot is taken twice on one thread");
        struct Restore(bool);
        impl Drop for Restore { fn drop(&mut self) { HOLDS_SLOT.with(|h| h.set(self.0)); } }
        let _restore = Restore(outer);
        guarded(step);
        drop(claim);
        return StepMode::Parallel;
    }
    let leased = SERIAL_POOLS.lock().unwrap_or_else(|e| e.into_inner()).pop();
    let pool = match leased {
        Some(p) => p,
        None => rayon::ThreadPoolBuilder::new().num_threads(1).thread_name(|_| "bp-physics-serial".into()).build().expect("a one-worker pool for a contended physics step"),
    };
    pool.install(|| guarded(step));
    SERIAL_POOLS.lock().unwrap_or_else(|e| e.into_inner()).push(pool);
    StepMode::Serial
}

/// The step itself. Debug builds check the invariant the module notes rely on: Rapier only takes its
/// spawn-and-block path (a pool of more than one worker) in the step holding the parallel slot.
fn guarded(step: impl FnOnce()) {
    debug_assert!(rayon::current_num_threads() <= 1 || HOLDS_SLOT.with(Cell::get), "a Rapier step would spawn into a {}-worker pool without the parallel slot", rayon::current_num_threads());
    step();
}

/// Per-body bookkeeping kept beside Rapier, indexed by the body's arena index.
#[derive(Clone, Copy, Debug)]
pub(crate) struct BodyMeta {
    pub(crate) generation: u32,
    pub(crate) live: bool,
    pub(crate) collider: Option<rp::ColliderHandle>,
    /// Centre of mass in the body (grid) frame and mass, as last supplied.
    pub(crate) com: DVec3,
    pub(crate) mass: f64,
    /// Force and torque about the centre of mass accumulated for the next step, world frame.
    pub(crate) force: DVec3,
    pub(crate) torque: DVec3,
    pub(crate) enabled: bool,
    /// Skipped ticks not yet made up (see the module docs).
    pub(crate) owed: u32,
    pub(crate) kinematic: bool,
    /// Velocities reported while kinematic.
    pub(crate) kin_lin: DVec3,
    pub(crate) kin_ang: DVec3,
}
impl BodyMeta {
    const DEAD: BodyMeta = BodyMeta { generation: 0, live: false, collider: None, com: DVec3::ZERO, mass: 1.0, force: DVec3::ZERO, torque: DVec3::ZERO, enabled: true, owed: 0, kinematic: false, kin_lin: DVec3::ZERO, kin_ang: DVec3::ZERO };
}

/// Rapier state for the one physics world of a sortie (L1133).
pub struct PhysicsWorld {
    pub(crate) bodies: rp::RigidBodySet,
    pub(crate) colliders: rp::ColliderSet,
    pub(crate) pipeline: rp::PhysicsPipeline,
    pub(crate) islands: rp::IslandManager,
    pub(crate) broad_phase: rp::DefaultBroadPhase,
    pub(crate) narrow_phase: rp::NarrowPhase,
    pub(crate) impulse_joints: rp::ImpulseJointSet,
    pub(crate) multibody_joints: rp::MultibodyJointSet,
    pub(crate) ccd: rp::CCDSolver,
    pub(crate) params: rp::IntegrationParameters,
    pub(crate) terrain: Option<rp::ColliderHandle>,
    /// Rapier 0.36 threads a soft-body set through the step and the removal calls; it stays empty.
    pub(crate) soft_bodies: rp::SoftBodySet,
    pub(crate) gravity: DVec3,
    pub(crate) meta: Vec<BodyMeta>,
    /// Terrain tiles by (z, x) tile index: `Some` holds the tile's collider, `None` marks a tile known to be deep.
    pub(crate) tiles: BTreeMap<(i32, i32), Option<rp::ColliderHandle>>,
}

impl Default for PhysicsWorld { fn default() -> Self { Self::new() } }

fn finite_pose(p: &Pose) -> bool { p.pos.is_finite() && p.rot.is_finite() && p.rot.length_squared() > 1e-12 }

fn raw(body: BodyHandle) -> rp::RigidBodyHandle { rp::RigidBodyHandle::from_raw_parts(body.index, body.generation) }

fn handle_of(h: rp::RigidBodyHandle) -> BodyHandle { let (index, generation) = h.into_raw_parts(); BodyHandle { index, generation } }

/// Rotation by the rotation vector `v` (axis times angle), through libm.
fn quat_from_rotation_vector(v: DVec3) -> glam::DQuat {
    let angle = v.length();
    if !(angle.is_finite() && angle > 1e-12) { return glam::DQuat::IDENTITY; }
    let (s, c) = libm::sincos(angle * 0.5);
    let a = v * (s / angle);
    glam::DQuat::from_xyzw(a.x, a.y, a.z, c)
}

/// Principal moments and frame of a symmetric inertia tensor by cyclic Jacobi rotations. Uses only
/// `+ - * /` and `sqrt`, so the result is the same on every platform. Returns the moments and the rotation
/// whose columns are the principal axes in the body frame.
pub(crate) fn principal_inertia(inertia: &glam::DMat3) -> (DVec3, glam::DQuat) {
    let mut a = [[0.0f64; 3]; 3];
    for (r, row) in a.iter_mut().enumerate() { for (c, v) in row.iter_mut().enumerate() { *v = 0.5 * (inertia.col(c)[r] + inertia.col(r)[c]); } }
    let mut v = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
    let scale = a.iter().flatten().fold(0.0f64, |m, x| m.max(x.abs()));
    if !(scale.is_finite() && scale > 0.0) { return (DVec3::ONE, glam::DQuat::IDENTITY); }
    for _sweep in 0..12 {
        let off = a[0][1].abs() + a[0][2].abs() + a[1][2].abs();
        if off <= scale * 1e-15 { break; }
        for (p, q) in [(0usize, 1usize), (0, 2), (1, 2)] {
            let apq = a[p][q];
            if apq.abs() <= scale * 1e-18 { continue; }
            let theta = (a[q][q] - a[p][p]) / (2.0 * apq);
            let t = if theta >= 0.0 { 1.0 / (theta + (theta * theta + 1.0).sqrt()) } else { -1.0 / (-theta + (theta * theta + 1.0).sqrt()) };
            let c = 1.0 / (t * t + 1.0).sqrt();
            let s = t * c;
            let (app, aqq) = (a[p][p], a[q][q]);
            a[p][p] = app - t * apq;
            a[q][q] = aqq + t * apq;
            a[p][q] = 0.0;
            a[q][p] = 0.0;
            let r = 3 - p - q;
            let (arp, arq) = (a[r][p], a[r][q]);
            a[r][p] = c * arp - s * arq;
            a[p][r] = a[r][p];
            a[r][q] = s * arp + c * arq;
            a[q][r] = a[r][q];
            for row in v.iter_mut() {
                let (vp, vq) = (row[p], row[q]);
                row[p] = c * vp - s * vq;
                row[q] = s * vp + c * vq;
            }
        }
    }
    let mut cols = [DVec3::new(v[0][0], v[1][0], v[2][0]), DVec3::new(v[0][1], v[1][1], v[2][1]), DVec3::new(v[0][2], v[1][2], v[2][2])];
    // Right-handed frame, re-orthogonalised against rounding.
    cols[0] = cols[0].normalize_or(DVec3::X);
    cols[1] = (cols[1] - cols[0] * cols[0].dot(cols[1])).normalize_or(DVec3::Y);
    cols[2] = cols[0].cross(cols[1]);
    let frame = glam::DQuat::from_mat3(&glam::DMat3::from_cols(cols[0], cols[1], cols[2])).normalize();
    let floor = scale * 1e-9;
    let moments = DVec3::new(a[0][0].max(floor), a[1][1].max(floor), a[2][2].max(floor));
    if !(moments.is_finite() && frame.is_finite()) { return (DVec3::splat(scale), glam::DQuat::IDENTITY); }
    (moments, frame)
}

/// Rapier mass properties from the game's, or `None` when they are unusable.
fn rapier_mass(mass: &MassProperties) -> Option<rp::MassProperties> {
    if !(mass.mass_kg.is_finite() && mass.mass_kg > 0.0 && mass.com.is_finite()) { return None; }
    let (moments, frame) = principal_inertia(&mass.inertia);
    Some(rp::MassProperties::with_principal_inertia_frame(mass.com, mass.mass_kg, moments, frame))
}

/// Compound collider of a cuboid list in the grid frame; `None` when no cuboid has volume.
fn compound_of(cuboids: &[CuboidLocal]) -> Option<rp::ColliderBuilder> {
    let cell = content::cell::CELL_M;
    let mut shapes = Vec::with_capacity(cuboids.len());
    for c in cuboids {
        if c.size.contains(&0) { continue; }
        let half = DVec3::new(c.size[0] as f64, c.size[1] as f64, c.size[2] as f64) * (0.5 * cell);
        let centre = DVec3::new(c.min[0] as f64, c.min[1] as f64, c.min[2] as f64) * cell + half;
        shapes.push((rapier3d_f64::math::Pose::from_parts(centre, glam::DQuat::IDENTITY), rp::SharedShape::cuboid(half.x, half.y, half.z)));
    }
    if shapes.is_empty() { return None; }
    Some(rp::ColliderBuilder::compound(shapes).density(0.0).friction(HULL_FRICTION).restitution(HULL_RESTITUTION))
}

impl PhysicsWorld {
    /// Empty world with dt = 1/60, gravity (0, -9.81, 0), and the rayon pool the caller installs (3 workers).
    /// Unit: `sim-core-physics`.
    pub fn new() -> PhysicsWorld {
        let params = rp::IntegrationParameters { dt: crate::time::DT, ..Default::default() };
        PhysicsWorld {
            bodies: rp::RigidBodySet::new(),
            colliders: rp::ColliderSet::new(),
            pipeline: rp::PhysicsPipeline::new(),
            islands: rp::IslandManager::new(),
            broad_phase: rp::DefaultBroadPhase::default(),
            narrow_phase: rp::NarrowPhase::new(),
            impulse_joints: rp::ImpulseJointSet::new(),
            multibody_joints: rp::MultibodyJointSet::new(),
            ccd: rp::CCDSolver::new(),
            params,
            terrain: None,
            soft_bodies: rp::SoftBodySet::new(),
            gravity: DVec3::new(0.0, -GRAVITY, 0.0),
            meta: Vec::new(),
            tiles: BTreeMap::new(),
        }
    }

    fn meta_of(&self, body: BodyHandle) -> Option<&BodyMeta> {
        self.meta.get(body.index as usize).filter(|m| m.live && m.generation == body.generation)
    }
    fn meta_mut(&mut self, body: BodyHandle) -> Option<&mut BodyMeta> {
        self.meta.get_mut(body.index as usize).filter(|m| m.live && m.generation == body.generation)
    }

    /// Insert a dynamic body with the given mass properties and compound collider. `origin_pose` is the pose of
    /// the vessel grid origin. Unit: `sim-core-physics`.
    ///
    /// `lin_vel` is the velocity of the centre of mass. A non-finite pose or velocity is replaced by the
    /// identity pose or rest, and unusable mass properties by 1 kg at the origin, so the handle is always valid.
    pub fn add_vessel(&mut self, origin_pose: Pose, lin_vel: DVec3, mass: &MassProperties, cuboids: &[CuboidLocal]) -> BodyHandle {
        let pose = if finite_pose(&origin_pose) { Pose { pos: origin_pose.pos, rot: origin_pose.rot.normalize() } } else { Pose::default() };
        let vel = if lin_vel.is_finite() { lin_vel } else { DVec3::ZERO };
        let fallback = MassProperties::default();
        let (props, used) = match rapier_mass(mass) { Some(p) => (p, mass), None => (rapier_mass(&fallback).unwrap_or_default(), &fallback) };
        let body = rp::RigidBodyBuilder::dynamic().pose(to_rapier_pose(pose)).linvel(vel).additional_mass_properties(props).can_sleep(false).build();
        let h = self.bodies.insert(body);
        let collider = compound_of(cuboids).map(|b| self.colliders.insert_with_parent(b, h, &mut self.bodies));
        if let Some(rb) = self.bodies.get_mut(h) { rb.recompute_mass_properties_from_colliders(&self.colliders); }
        let out = handle_of(h);
        let i = out.index as usize;
        if self.meta.len() <= i { self.meta.resize(i + 1, BodyMeta::DEAD); }
        self.meta[i] = BodyMeta { generation: out.generation, live: true, collider, com: used.com, mass: used.mass_kg, ..BodyMeta::DEAD };
        out
    }

    pub fn remove(&mut self, body: BodyHandle) {
        if self.meta_of(body).is_none() { return; }
        self.bodies.remove(raw(body), &mut self.islands, &mut self.colliders, &mut self.impulse_joints, &mut self.multibody_joints, &mut self.soft_bodies, true);
        self.meta[body.index as usize] = BodyMeta::DEAD;
    }

    /// Replace a body's compound after cells changed. Unit: `sim-core-physics`.
    ///
    /// An empty list leaves the body without a collider (it still moves and takes forces).
    pub fn set_collider(&mut self, body: BodyHandle, cuboids: &[CuboidLocal]) {
        let Some(old) = self.meta_of(body).map(|m| m.collider) else { return };
        if let Some(c) = old { self.colliders.remove(c, &mut self.islands, &mut self.bodies, &mut self.soft_bodies, true); }
        let new = compound_of(cuboids).map(|b| self.colliders.insert_with_parent(b, raw(body), &mut self.bodies));
        if let Some(m) = self.meta_mut(body) { m.collider = new; }
    }

    /// Replace a body's mass properties (fluids move every tick). Unit: `sim-core-physics`.
    ///
    /// Takes effect at once: forces added afterwards in the same tick lever about the new centre of mass.
    /// Unusable properties (mass not positive, non-finite values) are ignored.
    pub fn set_mass(&mut self, body: BodyHandle, mass: &MassProperties) {
        if self.meta_of(body).is_none() { return; }
        let Some(props) = rapier_mass(mass) else { return };
        if let Some(rb) = self.bodies.get_mut(raw(body)) {
            rb.set_additional_mass_properties(props, true);
            rb.recompute_mass_properties_from_colliders(&self.colliders);
        }
        if let Some(m) = self.meta_mut(body) { m.com = mass.com; m.mass = mass.mass_kg; }
    }

    /// World position of a body's centre of mass, if the body exists.
    fn com_world(&self, body: BodyHandle) -> Option<DVec3> {
        let m = self.meta_of(body)?;
        let rb = self.bodies.get(raw(body))?;
        let p = rb.position();
        Some(p.translation + p.rotation * m.com)
    }

    /// Accumulate a force at a world point for this step. Unit: `sim-core-physics`.
    pub fn add_force_at(&mut self, body: BodyHandle, force: DVec3, world_point: DVec3) {
        if !(force.is_finite() && world_point.is_finite()) { return; }
        let Some(com) = self.com_world(body) else { return };
        if let Some(m) = self.meta_mut(body) {
            m.force += force;
            m.torque += (world_point - com).cross(force);
        }
    }
    pub fn add_torque(&mut self, body: BodyHandle, torque: DVec3) {
        if !torque.is_finite() { return; }
        if let Some(m) = self.meta_mut(body) { m.torque += torque; }
    }

    /// Switch a body between dynamic and kinematic (fidelity changes). Unit: `sim-core-physics`.
    ///
    /// Pose and velocities carry over both ways.
    pub fn set_kinematic(&mut self, body: BodyHandle, kinematic: bool) {
        let Some(was) = self.meta_of(body).map(|m| (m.kinematic, m.kin_lin, m.kin_ang)) else { return };
        if was.0 == kinematic { return; }
        let Some(rb) = self.bodies.get_mut(raw(body)) else { return };
        let (lin, ang) = (rb.linvel(), rb.angvel());
        if kinematic {
            rb.set_body_type(rp::RigidBodyType::KinematicPositionBased, true);
            let pose = *rb.position();
            rb.set_next_kinematic_position(pose);
        } else {
            rb.set_body_type(rp::RigidBodyType::Dynamic, true);
            rb.set_linvel(was.1, true);
            rb.set_angvel(was.2, true);
        }
        if let Some(m) = self.meta_mut(body) {
            m.kinematic = kinematic;
            m.owed = 0;
            m.force = DVec3::ZERO;
            m.torque = DVec3::ZERO;
            if kinematic { m.kin_lin = lin; m.kin_ang = ang; }
        }
    }

    /// Drive a kinematic body or teleport a dynamic one (rejoin, restore). Unit: `sim-core-physics`.
    ///
    /// A kinematic body reaches `kin.pose` during the next step, pushing dynamic bodies on the way, and
    /// reports `kin`'s velocities. A dynamic body moves there now with those velocities. Non-finite input is
    /// ignored.
    pub fn set_kinematics(&mut self, body: BodyHandle, kin: &Kinematics) {
        if !(finite_pose(&kin.pose) && kin.lin_vel.is_finite() && kin.ang_vel.is_finite()) { return; }
        let Some(kinematic) = self.meta_of(body).map(|m| m.kinematic) else { return };
        let Some(rb) = self.bodies.get_mut(raw(body)) else { return };
        let pose = to_rapier_pose(Pose { pos: kin.pose.pos, rot: kin.pose.rot.normalize() });
        if kinematic {
            rb.set_next_kinematic_position(pose);
        } else {
            rb.set_position(pose, true);
            rb.set_linvel(kin.lin_vel, true);
            rb.set_angvel(kin.ang_vel, true);
        }
        if let Some(m) = self.meta_mut(body) {
            m.kin_lin = kin.lin_vel;
            m.kin_ang = kin.ang_vel;
            if !kinematic { m.owed = 0; }
        }
    }

    /// Pose of the grid origin and velocities at the centre of mass. Unit: `sim-core-physics`.
    pub fn kinematics(&self, body: BodyHandle) -> Kinematics {
        let (Some(m), Some(rb)) = (self.meta_of(body), self.bodies.get(raw(body))) else { return Kinematics::default() };
        let pose = from_rapier_pose(rb.position());
        if m.kinematic { Kinematics { pose, lin_vel: m.kin_lin, ang_vel: m.kin_ang } } else { Kinematics { pose, lin_vel: rb.linvel(), ang_vel: rb.angvel() } }
    }

    /// Skip a body in this step (30 Hz distant hulls on off ticks). Unit: `sim-core-physics`.
    ///
    /// The skipped time is made up on the body's next enabled step (see the module docs).
    pub fn set_enabled(&mut self, body: BodyHandle, enabled: bool) {
        let Some(m) = self.meta_mut(body) else { return };
        if m.enabled == enabled { return; }
        m.enabled = enabled;
        if let Some(rb) = self.bodies.get_mut(raw(body)) { rb.set_enabled(enabled); }
    }

    /// Install or refresh the terrain collider near the bodies that could touch land. Unit: `sim-core-physics`.
    ///
    /// `around` holds one position per such body (any point of the hull will do). Cheap when nothing moved to
    /// a new tile: every tile is sampled once and remembered, deep ones included.
    pub fn set_terrain(&mut self, terrain: &crate::terrain::Terrain, around: &[DVec3]) {
        let tiles_near = |reach: f64| -> std::collections::BTreeSet<(i32, i32)> {
            let mut set = std::collections::BTreeSet::new();
            let idx = |v: f64| (v / TERRAIN_TILE_M).floor().clamp(-1.0e6, 1.0e6) as i32;
            for p in around {
                if !(p.x.is_finite() && p.z.is_finite()) { continue; }
                for tz in idx(p.z - reach)..=idx(p.z + reach) { for tx in idx(p.x - reach)..=idx(p.x + reach) { set.insert((tz, tx)); } }
            }
            set
        };
        let keep = tiles_near(TERRAIN_KEEP_M);
        let stale: Vec<(i32, i32)> = self.tiles.keys().filter(|k| !keep.contains(k)).copied().collect();
        for k in stale {
            if let Some(Some(c)) = self.tiles.remove(&k) { self.colliders.remove(c, &mut self.islands, &mut self.bodies, &mut self.soft_bodies, false); }
        }
        for k in tiles_near(TERRAIN_REACH_M) {
            if self.tiles.contains_key(&k) { continue; }
            let tile = terrain_tile(terrain, k).map(|b| self.colliders.insert(b));
            self.tiles.insert(k, tile);
        }
        self.terrain = self.tiles.values().find_map(|t| *t);
    }

    /// Advance one tick and clear accumulated forces. Contacts are appended in ascending (a, b) handle order.
    /// Unit: `sim-core-physics`.
    ///
    /// One contact is reported per touching pair of bodies, and one per body touching terrain (`b == None`),
    /// with the summed normal impulse of the step in N s and the impulse-weighted contact point. A dt that is
    /// not positive and finite advances nothing and only clears the forces.
    ///
    /// Safe to call from any thread or rayon worker, with any number of other worlds stepping at once on the
    /// same pool (see the module notes on stepping and rayon).
    pub fn step(&mut self, dt: f64, contacts: &mut Vec<Contact>) { self.step_mode(dt, contacts); }

    /// [`Self::step`], reporting how Rapier ran (`Inline` also for a step that advanced nothing).
    pub(crate) fn step_mode(&mut self, dt: f64, contacts: &mut Vec<Contact>) -> StepMode {
        if !(dt.is_finite() && dt > 0.0) {
            for m in &mut self.meta { m.force = DVec3::ZERO; m.torque = DVec3::ZERO; }
            return StepMode::Inline;
        }
        self.params.dt = dt;
        let gravity = self.gravity;
        for (i, m) in self.meta.iter_mut().enumerate() {
            if !m.live { continue; }
            let (force, torque) = (m.force, m.torque);
            m.force = DVec3::ZERO;
            m.torque = DVec3::ZERO;
            if !m.enabled { m.owed = (m.owed + 1).min(MAX_OWED_TICKS); continue; }
            if m.kinematic { m.owed = 0; continue; }
            let Some(rb) = self.bodies.get_mut(rp::RigidBodyHandle::from_raw_parts(i as u32, m.generation)) else { continue };
            rb.reset_forces(false);
            rb.reset_torques(false);
            let span = (m.owed + 1) as f64;
            let f = force * span + gravity * (m.mass * m.owed as f64);
            let t = torque * span;
            if f.is_finite() { rb.add_force(f, true); }
            if t.is_finite() { rb.add_torque(t, true); }
        }
        let (pipeline, params, islands, broad_phase, narrow_phase) = (&mut self.pipeline, &self.params, &mut self.islands, &mut self.broad_phase, &mut self.narrow_phase);
        let (bodies, colliders, impulse_joints, multibody_joints, soft_bodies, ccd) = (&mut self.bodies, &mut self.colliders, &mut self.impulse_joints, &mut self.multibody_joints, &mut self.soft_bodies, &mut self.ccd);
        let mode = run_rapier_step(move || {
            pipeline.step(gravity, params, islands, broad_phase, narrow_phase, bodies, colliders, impulse_joints, multibody_joints, soft_bodies, ccd, &(), &());
        });
        for (i, m) in self.meta.iter_mut().enumerate() {
            if !m.live || !m.enabled || m.kinematic { continue; }
            let Some(rb) = self.bodies.get_mut(rp::RigidBodyHandle::from_raw_parts(i as u32, m.generation)) else { continue };
            rb.reset_forces(false);
            rb.reset_torques(false);
            if m.owed > 0 {
                // Make up the skipped ticks at the new velocity, turning about the centre of mass.
                let extra = m.owed as f64 * dt;
                let pose = *rb.position();
                let (v, w) = (rb.linvel(), rb.angvel());
                if v.is_finite() && w.is_finite() {
                    let com = pose.translation + pose.rotation * m.com;
                    let rot = (quat_from_rotation_vector(w * extra) * pose.rotation).normalize();
                    let pos = com + v * extra - rot * m.com;
                    rb.set_position(rapier3d_f64::math::Pose::from_parts(pos, rot), true);
                }
                m.owed = 0;
            }
        }
        self.collect_contacts(contacts);
        mode
    }

    fn collect_contacts(&self, contacts: &mut Vec<Contact>) {
        let start = contacts.len();
        for pair in self.narrow_phase.contact_pairs() {
            let body_of = |c: rp::ColliderHandle| self.colliders.get(c).and_then(|c| c.parent()).map(handle_of);
            let (b1, b2) = (body_of(pair.collider1), body_of(pair.collider2));
            let (a, b) = match (b1, b2) {
                (Some(x), Some(y)) if x != y => (x.min(y), Some(x.max(y))),
                (Some(x), None) | (None, Some(x)) => (x, None),
                _ => continue,
            };
            let mut impulse = 0.0;
            let mut weighted = DVec3::ZERO;
            for m in pair.solver_manifolds() {
                let mi: f64 = m.points.iter().map(|pt| pt.data.impulse).sum();
                if !(mi.is_finite() && mi > 0.0) || m.data.solver_contacts.is_empty() { continue; }
                let mut mid = DVec3::ZERO;
                for sc in &m.data.solver_contacts {
                    let (p1, p2) = m.data.solver_contact_world_points(sc, &self.bodies);
                    mid += (p1 + p2) * 0.5;
                }
                mid /= m.data.solver_contacts.len() as f64;
                if !mid.is_finite() { continue; }
                impulse += mi;
                weighted += mid * mi;
            }
            if impulse > 0.0 { contacts.push(Contact { a, b, point: weighted / impulse, impulse }); }
        }
        // Canonical order, then one entry per pair (several terrain tiles can touch one hull).
        contacts[start..].sort_by(|x, y| (x.a, x.b).cmp(&(y.a, y.b)).then(x.point.x.total_cmp(&y.point.x)).then(x.point.y.total_cmp(&y.point.y)).then(x.point.z.total_cmp(&y.point.z)).then(x.impulse.total_cmp(&y.impulse)));
        let mut w = start;
        for r in start..contacts.len() {
            let c = contacts[r];
            if w > start && contacts[w - 1].a == c.a && contacts[w - 1].b == c.b {
                let p = &mut contacts[w - 1];
                let total = p.impulse + c.impulse;
                p.point = (p.point * p.impulse + c.point * c.impulse) / total;
                p.impulse = total;
            } else {
                contacts[w] = c;
                w += 1;
            }
        }
        contacts.truncate(w);
    }

    /// Number of live bodies and collider shapes, for tracing and Complexity.
    pub fn counts(&self) -> (usize, usize) { (self.bodies.len(), self.colliders.len()) }
}

/// Heightfield collider of one terrain tile, or `None` when the whole tile is deeper than
/// [`TERRAIN_MAX_DEPTH_M`].
fn terrain_tile(terrain: &crate::terrain::Terrain, key: (i32, i32)) -> Option<rp::ColliderBuilder> {
    let (z0, x0) = (key.0 as f64 * TERRAIN_TILE_M, key.1 as f64 * TERRAIN_TILE_M);
    let coarse = 4usize;
    let mut top = f64::NEG_INFINITY;
    for i in 0..=coarse { for j in 0..=coarse {
        top = top.max(terrain.height_at(x0 + TERRAIN_TILE_M * j as f64 / coarse as f64, z0 + TERRAIN_TILE_M * i as f64 / coarse as f64));
    } }
    if !(top > -(TERRAIN_MAX_DEPTH_M + TERRAIN_COARSE_SLACK_M)) { return None; }
    let n = (TERRAIN_TILE_M / TERRAIN_STEP_M) as usize + 1;
    let mut heights = Vec::with_capacity(n * n);
    let mut top = f64::NEG_INFINITY;
    // Column-major: rows advance along z, columns along x.
    for j in 0..n { for i in 0..n {
        let h = terrain.height_at(x0 + j as f64 * TERRAIN_STEP_M, z0 + i as f64 * TERRAIN_STEP_M);
        let h = if h.is_finite() { h } else { -crate::terrain::DEFAULT_DEPTH_M };
        top = top.max(h);
        heights.push(h);
    } }
    if !(top > -TERRAIN_MAX_DEPTH_M) { return None; }
    let field = rapier3d_f64::parry::utils::Array2::new(n, n, heights);
    let centre = DVec3::new(x0 + 0.5 * TERRAIN_TILE_M, 0.0, z0 + 0.5 * TERRAIN_TILE_M);
    Some(rp::ColliderBuilder::heightfield(field, DVec3::new(TERRAIN_TILE_M, 1.0, TERRAIN_TILE_M)).translation(centre).friction(TERRAIN_FRICTION).restitution(0.0))
}

/// Sort key of a cuboid: its sim chunk, then its minimum corner in scan order (y, z, x).
fn cuboid_key(c: &CuboidLocal, chunk_dims: [u8; 3]) -> (usize, u16, u16, u16) {
    let n = crate::grid::CHUNK;
    let (cx, cy, cz) = (c.min[0] as usize / n, c.min[1] as usize / n, c.min[2] as usize / n);
    (cx + chunk_dims[0] as usize * (cz + chunk_dims[2] as usize * cy), c.min[1], c.min[2], c.min[0])
}

/// Greedy-merge the solid cells of one sim chunk, appending cuboids in scan order.
fn merge_chunk(grid: &crate::grid::VesselGrid, ci: usize, out: &mut Vec<CuboidLocal>) {
    use crate::grid::{CHUNK, CHUNK_CELLS, CHUNK_WORDS};
    let Some(Some(chunk)) = grid.chunks.get(ci) else { return };
    if chunk.solid_count == 0 || chunk.solid.len() < CHUNK_WORDS { return; }
    let (cdx, cdz) = (grid.chunk_dims[0] as usize, grid.chunk_dims[2] as usize);
    if cdx == 0 || cdz == 0 { return; }
    let origin = [(ci % cdx) * CHUNK, (ci / (cdx * cdz)) * CHUNK, ((ci / cdx) % cdz) * CHUNK];
    // Bits still to cover.
    let mut left = [0u64; CHUNK_WORDS];
    left.copy_from_slice(&chunk.solid[..CHUNK_WORDS]);
    let at = |x: usize, y: usize, z: usize| x + CHUNK * (z + CHUNK * y);
    let has = |left: &[u64; CHUNK_WORDS], i: usize| (left[i / 64] >> (i % 64)) & 1 == 1;
    for start in 0..CHUNK_CELLS {
        if !has(&left, start) { continue; }
        let (x, z, y) = (start % CHUNK, (start / CHUNK) % CHUNK, start / (CHUNK * CHUNK));
        let mut sx = 1;
        while x + sx < CHUNK && has(&left, at(x + sx, y, z)) { sx += 1; }
        let row_free = |left: &[u64; CHUNK_WORDS], yy: usize, zz: usize| (x..x + sx).all(|xx| has(left, at(xx, yy, zz)));
        let mut sz = 1;
        while z + sz < CHUNK && row_free(&left, y, z + sz) { sz += 1; }
        let mut sy = 1;
        while y + sy < CHUNK && (z..z + sz).all(|zz| row_free(&left, y + sy, zz)) { sy += 1; }
        for yy in y..y + sy { for zz in z..z + sz { for xx in x..x + sx { let i = at(xx, yy, zz); left[i / 64] &= !(1 << (i % 64)); } } }
        out.push(CuboidLocal { min: [(origin[0] + x) as u16, (origin[1] + y) as u16, (origin[2] + z) as u16], size: [sx as u16, sy as u16, sz as u16] });
    }
}

/// Greedy-merge live solid cells into cuboids, deterministic (ascending scan, largest-X-run first).
/// Per-chunk results are cached by the caller; only dirty chunks are remerged. Unit: `sim-core-physics`.
///
/// A cuboid never crosses a 16-cell sim chunk boundary, which is what lets [`regreedy_dirty`] replace one
/// chunk's cuboids without touching its neighbours. The list is ordered by chunk index, then by the scan
/// order (y, z, x) of each cuboid's minimum corner.
pub fn greedy_cuboids(grid: &crate::grid::VesselGrid) -> Vec<CuboidLocal> {
    let mut out = Vec::new();
    for ci in 0..grid.chunks.len() { merge_chunk(grid, ci, &mut out); }
    out
}
/// Remerge only the dirty chunks of `grid` into an existing cuboid list. Unit: `sim-core-physics`.
///
/// Reads `grid.dirty_chunks` without clearing it (the caller drains the marks once every consumer has
/// rebuilt). The result equals [`greedy_cuboids`] on the same grid, order included, provided the list came
/// from it and every chunk changed since is marked.
pub fn regreedy_dirty(grid: &crate::grid::VesselGrid, cuboids: &mut Vec<CuboidLocal>) {
    let dims = grid.chunk_dims;
    let dirty: Vec<usize> = grid.dirty_chunks.iter_ones().filter(|&c| c < grid.chunks.len()).collect();
    if dirty.is_empty() { return; }
    cuboids.retain(|c| dirty.binary_search(&cuboid_key(c, dims).0).is_err());
    for &ci in &dirty { merge_chunk(grid, ci, cuboids); }
    cuboids.sort_by_key(|c| cuboid_key(c, dims));
}
