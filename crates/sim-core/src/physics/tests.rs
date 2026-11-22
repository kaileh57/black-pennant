//! Tests of the Rapier seam: cuboid merging, mass and force handling, skipped ticks, contacts, terrain.
use super::*;
use crate::compartments::testkit::{box_hull, build};
use crate::grid::VesselGrid;
use crate::time::DT;
use content::campaign::{Island, MapDef};
use content::cell::CELL_M;
use content::ids::CellIndex;
use glam::{DMat3, DQuat};
use rayon::prelude::*;
use std::sync::Arc;

fn grid(size: [u16; 3]) -> VesselGrid { build(box_hull(size, &[], &[]), &[]) }

fn cube_mass(kg: f64, com: DVec3, side: f64) -> MassProperties {
    MassProperties { mass_kg: kg, com, inertia: DMat3::from_diagonal(DVec3::splat(kg * side * side / 6.0)) }
}

/// A solid 2 m cube of cells with its mass at the centre.
fn cube() -> (Vec<CuboidLocal>, MassProperties) {
    (vec![CuboidLocal { min: [0, 0, 0], size: [4, 4, 4] }], cube_mass(4_000.0, DVec3::splat(1.0), 2.0))
}

fn covered(cuboids: &[CuboidLocal], g: &VesselGrid) -> bool {
    let mut seen = vec![false; g.design.cells.len()];
    let mut total = 0u32;
    for c in cuboids {
        for y in c.min[1]..c.min[1] + c.size[1] { for z in c.min[2]..c.min[2] + c.size[2] { for x in c.min[0]..c.min[0] + c.size[0] {
            let p = content::cell::CellPos::new(x as i16, y as i16, z as i16);
            let Some(i) = g.index(p) else { return false };
            if !g.is_solid(p) || seen[i.index()] { return false; }
            seen[i.index()] = true;
            total += 1;
        } } }
    }
    total == g.solid_count
}

#[test]
fn greedy_covers_every_solid_cell_once() {
    let g = grid([40, 10, 12]);
    let c = greedy_cuboids(&g);
    assert!(covered(&c, &g));
    // A shell of six plates over 3 x 1 x 1 chunks merges to a few dozen boxes, not thousands of cells.
    assert!(c.len() < 60, "{}", c.len());
    for q in &c { for k in 0..3 { assert_eq!(q.min[k] as usize / 16, (q.min[k] + q.size[k] - 1) as usize / 16, "crosses a chunk"); } }
    assert_eq!(c, greedy_cuboids(&g), "deterministic");
    assert!(greedy_cuboids(&VesselGrid::build(std::sync::Arc::new(content::cell::CellGrid::new([0, 0, 0])), &[], &|_| (false, content::part::Flammable::No))).is_empty());
}

#[test]
fn regreedy_matches_full_merge_after_damage() {
    let mut g = grid([40, 10, 12]);
    let mut cached = greedy_cuboids(&g);
    g.drain_dirty_chunks();
    let hit: Vec<CellIndex> = [(3, 0, 3), (4, 0, 3), (20, 9, 6), (39, 5, 5), (17, 0, 0)].iter().map(|&(x, y, z)| g.index(content::cell::CellPos::new(x, y, z)).unwrap()).collect();
    for c in &hit { g.add_damage(*c, 8); }
    regreedy_dirty(&g, &mut cached);
    assert_eq!(cached, greedy_cuboids(&g));
    assert!(covered(&cached, &g));
    // Nothing dirty: untouched.
    g.drain_dirty_chunks();
    let before = cached.clone();
    regreedy_dirty(&g, &mut cached);
    assert_eq!(before, cached);
}

#[test]
fn principal_axes_rebuild_the_tensor() {
    let m = DMat3::from_cols(DVec3::new(9.0, 1.5, -2.0), DVec3::new(1.5, 6.0, 0.7), DVec3::new(-2.0, 0.7, 4.0));
    let (d, q) = principal_inertia(&m);
    let r = DMat3::from_quat(q);
    let back = r * DMat3::from_diagonal(d) * r.transpose();
    for c in 0..3 { assert!((back.col(c) - m.col(c)).length() < 1e-9, "{back:?}"); }
    assert!((q.length() - 1.0).abs() < 1e-12);
    // Degenerate input still gives usable moments.
    let (d, _) = principal_inertia(&DMat3::ZERO);
    assert!(d.min_element() > 0.0);
}

#[test]
fn free_fall_matches_substepped_euler() {
    let mut w = PhysicsWorld::new();
    let (c, m) = cube();
    let b = w.add_vessel(Pose::new(DVec3::new(10.0, 100.0, -5.0), DQuat::IDENTITY), DVec3::new(2.0, 0.0, 0.0), &m, &c);
    let mut contacts = Vec::new();
    for _ in 0..60 { w.step(DT, &mut contacts); }
    let k = w.kinematics(b);
    // Rapier integrates in `num_solver_iterations` substeps per step.
    let sub = w.params.num_solver_iterations as f64;
    let (n, h) = (60.0 * sub, DT / sub);
    assert!((k.pose.pos.y - (100.0 - GRAVITY * h * h * n * (n + 1.0) / 2.0)).abs() < 1e-9, "{}", k.pose.pos.y);
    assert!((k.pose.pos.x - 12.0).abs() < 1e-9);
    assert!((k.lin_vel.y + GRAVITY).abs() < 1e-9);
    assert!(contacts.is_empty());
    assert_eq!(w.counts(), (1, 1));
}

#[test]
fn force_through_new_centre_of_mass_does_not_turn_the_body() {
    let mut w = PhysicsWorld::new();
    let (c, m) = cube();
    let pose = Pose::new(DVec3::new(0.0, 50.0, 0.0), DQuat::from_rotation_y(0.7));
    let b = w.add_vessel(pose, DVec3::ZERO, &m, &c);
    // Shift the centre of mass, then push through the new one in the same tick.
    let shifted = cube_mass(4_000.0, DVec3::new(1.6, 0.4, 1.2), 2.0);
    w.set_mass(b, &shifted);
    w.add_force_at(b, DVec3::new(30_000.0, 4_000.0 * GRAVITY, -12_000.0), pose.to_world(shifted.com));
    let mut contacts = Vec::new();
    w.step(DT, &mut contacts);
    let k = w.kinematics(b);
    assert!(k.ang_vel.length() < 1e-12, "{:?}", k.ang_vel);
    assert!((k.lin_vel - DVec3::new(30_000.0, 0.0, -12_000.0) / 4_000.0 * DT).length() < 1e-12);
    // An off-centre push does turn it, and forces do not persist into the next step.
    w.add_force_at(b, DVec3::new(0.0, 0.0, 5_000.0), w.kinematics(b).pose.to_world(shifted.com + DVec3::X));
    w.step(DT, &mut contacts);
    let spun = w.kinematics(b);
    assert!(spun.ang_vel.length() > 1e-4);
    w.step(DT, &mut contacts);
    let after = w.kinematics(b);
    assert!((after.ang_vel - spun.ang_vel).length() < 1e-9);
    assert!((after.lin_vel - spun.lin_vel - DVec3::new(0.0, -GRAVITY * DT, 0.0)).length() < 1e-9);
}

#[test]
fn skipped_ticks_are_made_up() {
    // One body steps every tick, the other every second tick; the same force is offered on the ticks each runs.
    let mut w = PhysicsWorld::new();
    let (c, m) = cube();
    let fast = w.add_vessel(Pose::new(DVec3::new(0.0, 0.0, 0.0), DQuat::IDENTITY), DVec3::ZERO, &m, &c);
    let slow = w.add_vessel(Pose::new(DVec3::new(0.0, 0.0, 500.0), DQuat::IDENTITY), DVec3::ZERO, &m, &c);
    let push = DVec3::new(8_000.0, 4_000.0 * GRAVITY, 0.0);
    let mut contacts = Vec::new();
    for tick in 0..120 {
        let on = tick % 2 == 1;
        w.set_enabled(slow, on);
        for b in [fast, slow] { let at = w.kinematics(b).pose.to_world(m.com); w.add_force_at(b, push, at); }
        w.step(DT, &mut contacts);
    }
    let (f, s) = (w.kinematics(fast), w.kinematics(slow));
    // 2 m/s^2 for 2 s: both are near 4 m, the slow body within a step's worth of the fast one.
    assert!((f.pose.pos.x - 4.0).abs() < 0.05, "{}", f.pose.pos.x);
    assert!(s.pose.pos.y.abs() < 1e-9 && s.lin_vel.y.abs() < 1e-9, "gravity is made up too: {:?}", s);
    assert!((s.lin_vel.x - f.lin_vel.x).abs() < 1e-9);
    assert!((s.pose.pos.x - f.pose.pos.x).abs() / f.pose.pos.x < 0.02, "{} vs {}", s.pose.pos.x, f.pose.pos.x);
}

#[test]
fn head_on_collision_reports_one_ordered_contact() {
    let mut w = PhysicsWorld::new();
    let (c, m) = cube();
    w.gravity = DVec3::ZERO;
    let a = w.add_vessel(Pose::new(DVec3::new(-3.0, 0.0, 0.0), DQuat::IDENTITY), DVec3::new(3.0, 0.0, 0.0), &m, &c);
    let b = w.add_vessel(Pose::new(DVec3::new(1.5, 0.0, 0.0), DQuat::IDENTITY), DVec3::new(-3.0, 0.0, 0.0), &m, &c);
    let mut all = Vec::new();
    for _ in 0..120 { w.step(DT, &mut all); }
    assert!(!all.is_empty());
    let total: f64 = all.iter().map(|c| c.impulse).sum();
    for c in &all {
        assert_eq!((c.a, c.b), (a.min(b), Some(a.max(b))));
        assert!(c.impulse > 0.0 && c.point.is_finite());
        assert!(c.point.x > -1.5 && c.point.x < 2.0 && (0.0..=2.0).contains(&c.point.y), "{:?}", c.point);
    }
    // Each body arrives with 12,000 N s. Stopping them takes that much, and the 5% restitution a little more:
    // the reported impulse is the whole step's, not one solver substep's.
    assert!(total > 11_500.0 && total < 13_500.0, "{total}");
    let (ka, kb) = (w.kinematics(a), w.kinematics(b));
    assert!(ka.lin_vel.x <= 0.2 && kb.lin_vel.x >= -0.2, "they no longer close: {} {}", ka.lin_vel.x, kb.lin_vel.x);
    assert!(kb.pose.pos.x - ka.pose.pos.x >= 2.0 - 0.05, "no tunnelling");
}

fn shoal_map() -> MapDef {
    MapDef { size_m: [20_000.0, 20_000.0], depth_m: 150.0, islands: vec![Island { centre_m: [700.0, -300.0], radius_m: 400.0, height_m: 40.0, seed: 3 }], markers: vec![], seed: 11 }
}

#[test]
fn terrain_tiles_match_the_height_field() {
    let t = crate::terrain::Terrain::new(shoal_map());
    let mut w = PhysicsWorld::new();
    // Deep water only: tiles are remembered as deep and no collider is made.
    w.set_terrain(&t, &[DVec3::new(-8_000.0, 0.0, 8_000.0)]);
    assert!(w.terrain.is_none() && !w.tiles.is_empty());
    assert_eq!(w.counts().1, 0);
    // Near the island: tiles appear, and their samples sit on the analytic terrain with x and z the right way round.
    let near = DVec3::new(1_030.0, 0.0, -420.0);
    w.set_terrain(&t, &[near]);
    assert!(w.terrain.is_some());
    assert!(w.tiles.keys().all(|k| k.0 <= 0), "the deep tiles far away were dropped");
    let mut checked = 0;
    for (key, tile) in &w.tiles {
        let Some(h) = tile else { continue };
        let col = w.colliders.get(*h).unwrap();
        let hf = col.shape().as_heightfield().expect("heightfield");
        let centre = col.translation();
        assert_eq!(centre, DVec3::new((key.1 as f64 + 0.5) * TERRAIN_TILE_M, 0.0, (key.0 as f64 + 0.5) * TERRAIN_TILE_M));
        for (i, j) in [(0usize, 0usize), (3, 17), (32, 5), (20, 32)] {
            let (x, z) = (centre.x + hf.x_at(j), centre.z + hf.z_at(i));
            let got = hf.heights().data()[hf.heights().flat_index(i, j)] * hf.scale().y;
            assert!((got - t.height_at(x, z)).abs() < 1e-9, "tile {key:?} sample ({i}, {j}): {got} vs {}", t.height_at(x, z));
            checked += 1;
        }
    }
    assert!(checked >= 4);
    // Calling again changes nothing.
    let before = w.counts();
    w.set_terrain(&t, &[near]);
    assert_eq!(before, w.counts());
}

#[test]
fn a_body_grounds_on_the_shore_and_reports_terrain_contact() {
    let t = crate::terrain::Terrain::new(shoal_map());
    let mut w = PhysicsWorld::new();
    let (c, m) = cube();
    // Over land near the island centre.
    let (x, z) = (700.0, -300.0);
    let ground = t.height_at(x, z);
    assert!(ground > 10.0);
    let b = w.add_vessel(Pose::new(DVec3::new(x - 1.0, ground + 3.0, z - 1.0), DQuat::IDENTITY), DVec3::ZERO, &m, &c);
    let mut contacts = Vec::new();
    for _ in 0..240 {
        let at = w.kinematics(b).pose.pos;
        w.set_terrain(&t, &[at]);
        w.step(DT, &mut contacts);
    }
    let k = w.kinematics(b);
    assert!(k.pose.pos.y > ground - 3.0 && k.pose.pos.y < ground + 3.0, "rests on the ground: {} vs {ground}", k.pose.pos.y);
    assert!(k.lin_vel.length() < 2.0);
    assert!(contacts.iter().any(|c| c.a == b && c.b.is_none() && c.impulse > 0.0));
}

#[test]
fn kinematic_bodies_follow_orders_and_resume() {
    let mut w = PhysicsWorld::new();
    let (c, m) = cube();
    let b = w.add_vessel(Pose::default(), DVec3::new(1.0, 0.0, 0.0), &m, &c);
    w.set_kinematic(b, true);
    let mut contacts = Vec::new();
    let target = Kinematics { pose: Pose::new(DVec3::new(0.5, 0.0, 0.25), DQuat::from_rotation_y(0.1)), lin_vel: DVec3::new(6.0, 0.0, 3.0), ang_vel: DVec3::new(0.0, 0.2, 0.0) };
    w.set_kinematics(b, &target);
    w.add_force_at(b, DVec3::splat(1e9), DVec3::ZERO);
    w.step(DT, &mut contacts);
    let k = w.kinematics(b);
    assert!((k.pose.pos - target.pose.pos).length() < 1e-12 && k.pose.rot.angle_between(target.pose.rot) < 1e-9);
    assert_eq!((k.lin_vel, k.ang_vel), (target.lin_vel, target.ang_vel));
    // Back to dynamic: it carries the reported velocity on and falls again.
    w.set_kinematic(b, false);
    w.step(DT, &mut contacts);
    let k = w.kinematics(b);
    assert!((k.lin_vel - DVec3::new(6.0, -GRAVITY * DT, 3.0)).length() < 1e-9, "{:?}", k.lin_vel);
    // Teleport of a dynamic body.
    let there = Kinematics { pose: Pose::new(DVec3::new(900.0, 2.0, -40.0), DQuat::IDENTITY), lin_vel: DVec3::ZERO, ang_vel: DVec3::ZERO };
    w.set_kinematics(b, &there);
    assert_eq!(w.kinematics(b), there);
}

#[test]
fn bad_input_is_ignored() {
    let mut w = PhysicsWorld::new();
    let (c, m) = cube();
    let mut contacts = Vec::new();
    // Stale and NONE handles.
    for h in [BodyHandle::NONE, BodyHandle { index: 3, generation: 9 }] {
        w.add_force_at(h, DVec3::ONE, DVec3::ZERO);
        w.add_torque(h, DVec3::ONE);
        w.set_mass(h, &m);
        w.set_collider(h, &c);
        w.set_kinematic(h, true);
        w.set_enabled(h, false);
        w.remove(h);
        assert_eq!(w.kinematics(h), Kinematics::default());
    }
    // Unusable pose, velocity and mass still give a live body; an empty compound gives none.
    let bad = MassProperties { mass_kg: f64::NAN, com: DVec3::ZERO, inertia: DMat3::ZERO };
    let b = w.add_vessel(Pose::new(DVec3::splat(f64::NAN), DQuat::IDENTITY), DVec3::splat(f64::INFINITY), &bad, &[]);
    assert_eq!(w.counts(), (1, 0));
    w.add_force_at(b, DVec3::splat(f64::NAN), DVec3::ZERO);
    w.add_torque(b, DVec3::splat(f64::INFINITY));
    w.set_mass(b, &MassProperties { mass_kg: -5.0, ..m });
    w.set_kinematics(b, &Kinematics { pose: Pose::new(DVec3::splat(f64::NAN), DQuat::IDENTITY), ..Default::default() });
    w.step(f64::NAN, &mut contacts);
    w.step(DT, &mut contacts);
    let k = w.kinematics(b);
    assert!(k.pose.pos.is_finite() && k.lin_vel.is_finite());
    assert!((k.lin_vel.y + GRAVITY * DT).abs() < 1e-12);
    // Collider swap and removal; the freed slot's old handle goes stale.
    w.set_collider(b, &c);
    assert_eq!(w.counts(), (1, 1));
    w.set_collider(b, &[CuboidLocal { min: [0, 0, 0], size: [0, 3, 3] }]);
    assert_eq!(w.counts(), (1, 0));
    w.remove(b);
    assert_eq!(w.counts(), (0, 0));
    let b2 = w.add_vessel(Pose::default(), DVec3::ZERO, &m, &c);
    assert_ne!(b, b2);
    assert_eq!(w.kinematics(b), Kinematics::default());
    let _ = CELL_M;
}

fn scripted_run() -> Vec<Kinematics> {
    let mut w = PhysicsWorld::new();
    let (c, m) = cube();
    let mut bodies = Vec::new();
    for i in 0..6 {
        let p = DVec3::new(i as f64 * 1.9 - 5.0, (i % 3) as f64 * 2.4, (i % 2) as f64 * 0.7);
        bodies.push(w.add_vessel(Pose::new(p, DQuat::from_rotation_z(0.2 * i as f64)), DVec3::new(1.0 - i as f64 * 0.4, 0.0, 0.0), &m, &c));
    }
    w.remove(bodies[2]);
    bodies[2] = w.add_vessel(Pose::new(DVec3::new(0.0, 9.0, 0.0), DQuat::IDENTITY), DVec3::ZERO, &m, &c);
    let mut contacts = Vec::new();
    for tick in 0..90 {
        for (i, b) in bodies.iter().enumerate() {
            let k = w.kinematics(*b);
            w.add_force_at(*b, DVec3::new(0.0, 4_000.0 * GRAVITY, 300.0 * i as f64), k.pose.to_world(DVec3::new(1.0, 1.0, 1.0 + 0.01 * tick as f64)));
        }
        w.step(DT, &mut contacts);
    }
    bodies.iter().map(|b| w.kinematics(*b)).collect()
}

#[test]
fn reruns_are_bitwise_identical_on_any_pool_size() {
    let one = rayon::ThreadPoolBuilder::new().num_threads(1).build().unwrap().install(scripted_run);
    let three = rayon::ThreadPoolBuilder::new().num_threads(3).build().unwrap().install(scripted_run);
    let again = rayon::ThreadPoolBuilder::new().num_threads(3).build().unwrap().install(scripted_run);
    let bits = |v: &[Kinematics]| v.iter().flat_map(|k| [k.pose.pos.x, k.pose.pos.y, k.pose.pos.z, k.pose.rot.x, k.pose.rot.w, k.lin_vel.x, k.ang_vel.z].map(f64::to_bits)).collect::<Vec<_>>();
    assert_eq!(bits(&three), bits(&again));
    assert_eq!(bits(&one), bits(&three));
}

// ---- Rapier's parallel step and rayon pools ----------------------------------------------------------------

/// Run `f` on a detached thread and wait at most `limit`: `None` means it is still running (hung). A hung
/// thread is left behind; the tests below either release it or fail.
fn within<T: Send + 'static>(limit: std::time::Duration, f: impl FnOnce() -> T + Send + 'static) -> Option<T> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || { let _ = tx.send(f()); });
    rx.recv_timeout(limit).ok()
}

/// Generous for debug builds on a shared machine; a deadlock never finishes at all.
const HANG: std::time::Duration = std::time::Duration::from_secs(300);

fn kin_bits(v: &[Kinematics]) -> Vec<u64> {
    v.iter().flat_map(|k| [k.pose.pos.x, k.pose.pos.y, k.pose.pos.z, k.pose.rot.x, k.pose.rot.y, k.pose.rot.z, k.pose.rot.w, k.lin_vel.x, k.lin_vel.y, k.lin_vel.z, k.ang_vel.x, k.ang_vel.y, k.ang_vel.z].map(f64::to_bits)).collect()
}

/// `scripted_run` for `ticks` ticks, also counting how each Rapier step ran.
fn scripted_ticks(ticks: u32) -> (Vec<Kinematics>, [u32; 3]) {
    let mut w = PhysicsWorld::new();
    let (c, m) = cube();
    let bodies: Vec<BodyHandle> = (0..6).map(|i| {
        let p = DVec3::new(i as f64 * 1.9 - 5.0, (i % 3) as f64 * 2.4, (i % 2) as f64 * 0.7);
        w.add_vessel(Pose::new(p, DQuat::from_rotation_z(0.2 * i as f64)), DVec3::new(1.0 - i as f64 * 0.4, 0.0, 0.0), &m, &c)
    }).collect();
    let mut contacts = Vec::new();
    let mut modes = [0u32; 3];
    for tick in 0..ticks {
        for (i, b) in bodies.iter().enumerate() {
            let k = w.kinematics(*b);
            w.add_force_at(*b, DVec3::new(0.0, 4_000.0 * GRAVITY, 300.0 * i as f64), k.pose.to_world(DVec3::new(1.0, 1.0, 1.0 + 0.01 * tick as f64)));
        }
        contacts.clear();
        modes[w.step_mode(DT, &mut contacts) as usize] += 1;
    }
    (bodies.iter().map(|b| w.kinematics(*b)).collect(), modes)
}

/// The upstream mechanism, on Rapier alone: a parallel step `rayon::spawn`s its deferred BVH pass onto the
/// calling worker's own deque (rapier `pipeline/physics_pipeline/solve.rs`, guarded by
/// `rayon::current_num_threads() > 1`) and later blocks in a plain channel `recv` for it
/// (`physics_pipeline/mod.rs`, `join_deferred_bvh_optimize`). Only another worker of the same pool can run
/// it, so with every other worker blocked the step never returns. Here the other worker of a 2-worker pool is
/// parked; the step hangs until it is released, then finishes. If a Rapier upgrade makes this test fail,
/// re-read the seam notes on stepping in `physics/mod.rs`.
#[test]
fn rapier_step_starves_when_no_other_worker_is_free() {
    let pool = Arc::new(rayon::ThreadPoolBuilder::new().num_threads(2).build().unwrap());
    let (parked_tx, parked_rx) = std::sync::mpsc::channel::<()>();
    let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
    pool.spawn(move || { let _ = parked_tx.send(()); let _ = release_rx.recv(); });
    parked_rx.recv().unwrap();
    let (done_tx, done_rx) = std::sync::mpsc::channel::<u32>();
    pool.spawn(move || {
        let mut bodies = rp::RigidBodySet::new();
        let mut colliders = rp::ColliderSet::new();
        for i in 0..12 {
            let rb = bodies.insert(rp::RigidBodyBuilder::dynamic().translation(DVec3::new(i as f64 * 10.0, 0.0, 0.0)).linvel(DVec3::new(0.0, 0.0, 20.0)).build());
            colliders.insert_with_parent(rp::ColliderBuilder::cuboid(1.0, 1.0, 1.0).build(), rb, &mut bodies);
        }
        let (mut pipeline, mut islands, mut broad, mut narrow) = (rp::PhysicsPipeline::new(), rp::IslandManager::new(), rp::DefaultBroadPhase::default(), rp::NarrowPhase::new());
        let (mut ij, mut mj, mut soft, mut ccd) = (rp::ImpulseJointSet::new(), rp::MultibodyJointSet::new(), rp::SoftBodySet::new(), rp::CCDSolver::new());
        let params = rp::IntegrationParameters { dt: DT, ..Default::default() };
        let mut steps = 0;
        for _ in 0..30 {
            pipeline.step(DVec3::new(0.0, -GRAVITY, 0.0), &params, &mut islands, &mut broad, &mut narrow, &mut bodies, &mut colliders, &mut ij, &mut mj, &mut soft, &mut ccd, &(), &());
            steps += 1;
        }
        let _ = done_tx.send(steps);
    });
    assert!(done_rx.recv_timeout(std::time::Duration::from_secs(2)).is_err(), "Rapier finished its steps with the only other worker parked: the deferred BVH pass no longer needs a second worker");
    release_tx.send(()).unwrap();
    assert_eq!(done_rx.recv_timeout(HANG).expect("the step finishes once a worker is free"), 30);
}

/// The reported hang: as many worlds stepping at once as the pool has workers. Each world steps on its own
/// worker of one 3-worker pool, started together by a barrier.
#[test]
fn a_full_pool_of_worlds_steps_without_hanging() {
    let reference = kin_bits(&rayon::ThreadPoolBuilder::new().num_threads(3).build().unwrap().install(|| scripted_ticks(300)).0);
    let got = within(HANG, || {
        let pool = rayon::ThreadPoolBuilder::new().num_threads(3).build().unwrap();
        let barrier = Arc::new(std::sync::Barrier::new(3));
        let (tx, rx) = std::sync::mpsc::channel();
        for _ in 0..3 {
            let (tx, barrier) = (tx.clone(), barrier.clone());
            pool.spawn(move || { barrier.wait(); let _ = tx.send(scripted_ticks(300)); });
        }
        drop(tx);
        rx.iter().collect::<Vec<_>>()
    }).expect("three worlds stepping on a 3-worker pool hung");
    assert_eq!(got.len(), 3);
    for (k, _) in &got { assert_eq!(kin_bits(k), reference, "a world stepped beside others ends bit-identical"); }
}

/// Eight worlds on three workers for a few hundred ticks, the way background jobs and nested parallel work
/// pile onto one pool. All finish, bit-identical to a world stepped alone.
#[test]
fn eight_worlds_on_three_workers_finish_bit_identical() {
    let reference = kin_bits(&rayon::ThreadPoolBuilder::new().num_threads(1).build().unwrap().install(|| scripted_ticks(300)).0);
    let got = within(HANG, || {
        let pool = rayon::ThreadPoolBuilder::new().num_threads(3).build().unwrap();
        pool.install(|| (0..8).into_par_iter().map(|_| scripted_ticks(300)).collect::<Vec<_>>())
    }).expect("eight worlds on a 3-worker pool hung");
    assert_eq!(got.len(), 8);
    let mut modes = [0u32; 3];
    for (k, m) in &got {
        assert_eq!(kin_bits(k), reference);
        for i in 0..3 { modes[i] += m[i]; }
    }
    assert_eq!(modes[StepMode::Inline as usize], 0, "a 3-worker pool never steps inline: {modes:?}");
    assert_eq!(modes.iter().sum::<u32>(), 8 * 300);
}

/// The fallback taken under contention gives the same bits as the parallel step: with the parallel slot held
/// elsewhere, a world on a 3-worker pool steps serially and ends where a world stepped alone does.
#[test]
fn a_contended_step_runs_serially_and_matches_the_parallel_one() {
    let pool = rayon::ThreadPoolBuilder::new().num_threads(3).build().unwrap();
    let alone = kin_bits(&pool.install(|| scripted_ticks(120)).0);
    // Another test may hold the slot for a moment; wait for it on this (non-pool) thread.
    let claim = loop { if let Some(c) = ParallelClaim::try_take() { break c; } std::thread::yield_now(); };
    let (k, modes) = pool.install(|| scripted_ticks(120));
    drop(claim);
    assert_eq!(modes, [0, 0, 120], "every step took the serial fallback");
    assert_eq!(kin_bits(&k), alone);
    let one = rayon::ThreadPoolBuilder::new().num_threads(1).build().unwrap();
    assert_eq!(one.install(|| scripted_ticks(5)).1, [5, 0, 0], "a 1-worker pool steps inline");
}
