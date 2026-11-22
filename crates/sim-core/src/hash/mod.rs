//! Unit `sim-core-hash`. State hashing for golden tests, replays' desync detection and the nightly
//! cross-platform comparison. DESIGN.md L1150-1168, L445 (NaN canonicalisation).
//!
//! The hash covers simulation state only, visits every store in ascending slot or row order, and feeds floats
//! as their IEEE bit patterns with NaN canonicalised to one pattern (a debug assertion flags any NaN reaching
//! state). It is a per-subsystem hash every tick, so a divergence names the first diverging tick and subsystem.
//!
//! What each subsystem hash covers:
//! - `entities`: the entity table, the despawn queue, and per vessel its identity (side, player, class,
//!   blueprint, status, fidelity, parent) and the world's loss bookkeeping.
//! - `bodies`: per vessel kinematics, mass, dry mass, bounds and the capsize timer; pairs in contact.
//! - `cells`: per vessel damage buckets, scorch, sealed-air bits, the chunk bitsets, cuboids, the cells
//!   waiting for the structure pass and its split job; the structure cursor.
//! - `compartments`: per vessel the compartment set and the buoyancy blocks.
//! - `networks_parts`: per vessel networks, parts and controls.
//! - `projectiles`: the projectile store (private columns included) and the live cap.
//! - `munitions`: bodies, decoys and the shell grazes owed to the next munitions step.
//! - `crew`: per vessel crew state.
//! - `environment`: tick, seed, weather, the wave set and the map.
//!
//! Left out on purpose: Rapier's internals (the kinematics copied out each tick stand for them), the event
//! queue, work counters, the local remesh feeds and derived caches. Stores that implement `Serialize` are
//! hashed through their serde form ([`StateHasher::write_serde`]), so a field added to one is covered without
//! a change here.
use crate::world::World;
use serde::{Deserialize, Serialize};

mod ser;
#[cfg(test)]
mod tests;

/// 64-bit state hash (first 8 bytes of BLAKE3 over the canonical byte stream).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct StateHash(pub u64);

/// One hash per subsystem, matching the tick-split rows (L416).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubsystemHashes {
    pub entities: StateHash,
    /// Vessel kinematics and mass.
    pub bodies: StateHash,
    /// Cell damage buckets and chunk bitsets.
    pub cells: StateHash,
    pub compartments: StateHash,
    pub networks_parts: StateHash,
    pub projectiles: StateHash,
    pub munitions: StateHash,
    pub crew: StateHash,
    pub environment: StateHash,
}
impl SubsystemHashes {
    /// Names in field order, for divergence reports.
    pub const NAMES: [&'static str; 9] = ["entities", "bodies", "cells", "compartments", "networks_parts", "projectiles", "munitions", "crew", "environment"];
}

/// Incremental hasher with the canonical float rule. sim-systems uses the same type for its own state so
/// the combined hash is uniform.
pub struct StateHasher { pub(crate) inner: blake3::Hasher }
impl StateHasher {
    pub fn new() -> Self { Self { inner: blake3::Hasher::new() } }
    pub fn write_u64(&mut self, v: u64) { self.inner.update(&v.to_le_bytes()); }
    pub fn write_u32(&mut self, v: u32) { self.inner.update(&v.to_le_bytes()); }
    pub fn write_bytes(&mut self, v: &[u8]) { self.inner.update(v); }
    /// NaN canonicalised to `f64::NAN` bits; -0.0 kept distinct from 0.0.
    pub fn write_f64(&mut self, v: f64) { debug_assert!(!v.is_nan(), "NaN reached simulation state"); let bits = if v.is_nan() { f64::NAN.to_bits() } else { v.to_bits() }; self.write_u64(bits); }
    pub fn write_vec3(&mut self, v: glam::DVec3) { self.write_f64(v.x); self.write_f64(v.y); self.write_f64(v.z); }
    pub fn write_quat(&mut self, q: glam::DQuat) { self.write_f64(q.x); self.write_f64(q.y); self.write_f64(q.z); self.write_f64(q.w); }
    pub fn finish(&self) -> StateHash { let h = self.inner.finalize(); let b = h.as_bytes(); StateHash(u64::from_le_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]])) }
}
impl Default for StateHasher { fn default() -> Self { Self::new() } }
impl StateHasher {
    /// Feed any serialisable value in canonical form: integers little-endian at their own width, floats by
    /// the NaN rule above, sequences and strings prefixed with their length, options and enum variants
    /// tagged. Fields a type marks `serde(skip)` (derived caches) stay out.
    pub fn write_serde<T: Serialize + ?Sized>(&mut self, value: &T) { ser::feed(self, value); }
}

/// Per-subsystem hashes of the world at its current tick. Unit: `sim-core-hash`.
pub fn subsystem_hashes(world: &World) -> SubsystemHashes {
    let mut h: [StateHasher; 9] = core::array::from_fn(|_| StateHasher::new());
    let [entities, bodies, cells, compartments, networks_parts, projectiles, munitions, crew, environment] = &mut h;

    entities.write_serde(&world.entities);
    entities.write_serde(&world.doomed);
    for (slot, v) in world.vessels.iter() {
        let aux = world.aux.get(v.id);
        entities.write_u32(slot);
        entities.write_serde(&(v.id, v.side, v.player, &v.class, v.blueprint, &v.name, v.status, v.fidelity, v.parent, v.spawn_tick, v.struck, v.submersible, v.last_fired_tick));
        entities.write_serde(&aux.map(|a| (a.last_attacker, a.cookoff_tick, a.lost_tick, a.passengers_reported)));

        bodies.write_u32(slot);
        bodies.write_serde(&(v.kin, v.mass, v.dry_mass, v.bounds, v.capsize_ticks, v.tonnage_t, v.dims, v.waterline_y));
        bodies.write_serde(&aux.map(|a| a.extent));

        cells.write_u32(slot);
        hash_grid(cells, &v.grid);
        cells.write_serde(&(&v.cuboids, &v.pending_destroyed, v.root_cell));
        cells.write_serde(&aux.map(|a| a.rebuilt_tick));
        cells.write_serde(&world.split_jobs.get(v.id));

        compartments.write_u32(slot);
        compartments.write_serde(&v.compartments);
        compartments.write_serde(&v.blocks);

        networks_parts.write_u32(slot);
        networks_parts.write_serde(&v.networks);
        networks_parts.write_serde(&v.parts);
        networks_parts.write_serde(&v.controls);

        crew.write_u32(slot);
        crew.write_serde(&v.crew);
    }
    bodies.write_serde(&world.touching);
    cells.write_u32(world.split_cursor);

    projectiles.write_serde(&world.projectiles);
    projectiles.write_u64(world.projectile_cap as u64);

    munitions.write_serde(&world.bodies);
    munitions.write_serde(&world.decoys);
    munitions.write_serde(&world.grazes);

    environment.write_u64(world.tick.0);
    environment.write_u64(world.config.seed);
    environment.write_serde(&world.weather);
    environment.write_serde(&world.waves);
    environment.write_serde(&world.terrain.map);

    let [e, b, c, k, n, p, m, w, v] = h.map(|x| x.finish());
    SubsystemHashes { entities: e, bodies: b, cells: c, compartments: k, networks_parts: n, projectiles: p, munitions: m, crew: w, environment: v }
}

/// A cell grid is not `Serialize` (it shares its design); this feeds everything damage can change.
fn hash_grid(h: &mut StateHasher, g: &crate::grid::VesselGrid) {
    h.write_u64(g.damage.len() as u64);
    h.write_bytes(&g.damage);
    h.write_u64(g.scorch.len() as u64);
    h.write_bytes(&g.scorch);
    h.write_serde(&g.sealed_air);
    h.write_u32(g.solid_count);
    h.write_u32(g.original_count);
    h.write_u64(g.chunks.len() as u64);
    for chunk in &g.chunks {
        match chunk {
            None => h.write_bytes(&[0]),
            Some(c) => {
                h.write_bytes(&[1]);
                h.write_serde(&(&c.solid, &c.watertight, &c.air, c.solid_count));
            }
        }
    }
    h.write_serde(&g.dirty_chunks);
}

/// One hash of the whole world: the hash of the subsystem hashes in field order. Golden tests commit this
/// value; a simulation change that alters results updates it with the reason in the PR. Unit: `sim-core-hash`.
pub fn state_hash(world: &World) -> StateHash {
    let s = subsystem_hashes(world);
    let mut h = StateHasher::new();
    for part in [s.entities, s.bodies, s.cells, s.compartments, s.networks_parts, s.projectiles, s.munitions, s.crew, s.environment] { h.write_u64(part.0); }
    h.finish()
}

/// Inputs per function of the libm golden vector (L447).
pub const LIBM_GOLDEN_INPUTS: u64 = 100_000;
/// The committed value of [`libm_golden_hash`] for the pinned libm (0.2.16). The per-platform CI test and the
/// nightly cross-platform comparison check against it; it changes only with a deliberate libm upgrade.
pub const LIBM_GOLDEN: StateHash = StateHash(0x6a97_b589_48e3_ea87);
/// Seed of the golden vector's inputs. Changing it changes the committed hash.
const LIBM_GOLDEN_SEED: u64 = 0x4C49_424D_5F47_4F4C;

/// Golden-vector hash of every libm function the simulation uses over a fixed set of 10^5 inputs (L447).
/// Unit: `sim-core-hash`.
///
/// Each function gets [`LIBM_GOLDEN_INPUTS`] inputs of its own, drawn from the counter RNG and assembled
/// with exact integer and bit operations only, so the inputs themselves cannot differ between platforms.
/// Magnitudes are spread over the binary exponents the simulation meets (angles to 10^6 rad, distances to
/// 10^9 m, ratios near 1). Results go in by bit pattern with NaN canonicalised. The functions are the ones
/// sim-core, sim-systems and content call, plus `acos`, `trunc` and `fmod`, which glam's `libm` backend
/// reaches for quaternion and angle maths.
pub fn libm_golden_hash() -> StateHash {
    use crate::rng::{bits, Key, Stream};
    // Exact helpers: no libm, no rounding.
    let raw = |f: u64, i: u64, k: u32| bits(Key::new(LIBM_GOLDEN_SEED, i, Stream::Misc, f, 0).draw(k));
    // Uniform in [-1, 1).
    let unit = |r: u64| ((r >> 11) as f64) * (1.0 / 9_007_199_254_740_992.0) * 2.0 - 1.0;
    // Positive value with a uniform binary exponent in [lo, hi] and a random mantissa.
    let spread = |r: u64, lo: i32, hi: i32| {
        let e = lo + ((r >> 53) % (hi - lo + 1) as u64) as i32;
        f64::from_bits((((1023 + e) as u64) << 52) | (r & 0x000F_FFFF_FFFF_FFFF))
    };
    let signed = |r: u64, v: f64| if r >> 63 == 1 { -v } else { v };
    let canon = |v: f64| if v.is_nan() { f64::NAN.to_bits() } else { v.to_bits() };

    let mut h = StateHasher::new();
    let mut buf: Vec<u8> = Vec::with_capacity(LIBM_GOLDEN_INPUTS as usize * 16);
    let mut run = |f: u64, eval: &dyn Fn(u64, u64) -> [f64; 2], outputs: usize| {
        buf.clear();
        for i in 0..LIBM_GOLDEN_INPUTS {
            let out = eval(raw(f, i, 0), raw(f, i, 1));
            for v in &out[..outputs] { buf.extend_from_slice(&canon(*v).to_le_bytes()); }
        }
        h.write_u64(f);
        h.write_bytes(&buf);
    };
    // Angles: exponents -20..=20 (to about 10^6 rad).
    let angle = |r: u64| signed(r, spread(r, -20, 20));
    run(1, &|a, _| [libm::sin(angle(a)), 0.0], 1);
    run(2, &|a, _| [libm::cos(angle(a)), 0.0], 1);
    run(3, &|a, _| [libm::tan(angle(a)), 0.0], 1);
    run(4, &|a, _| { let (s, c) = libm::sincos(angle(a)); [s, c] }, 2);
    run(5, &|a, _| [libm::asin(unit(a)), 0.0], 1);
    run(6, &|a, _| [libm::acos(unit(a)), 0.0], 1);
    run(7, &|a, _| [libm::atan(signed(a, spread(a, -30, 30))), 0.0], 1);
    run(8, &|a, b| [libm::atan2(signed(a, spread(a, -30, 30)), signed(b, spread(b, -30, 30))), 0.0], 1);
    run(9, &|a, _| [libm::exp(unit(a) * 40.0), 0.0], 1);
    run(10, &|a, _| [libm::exp2(unit(a) * 60.0), 0.0], 1);
    run(11, &|a, _| [libm::log(spread(a, -40, 40)), 0.0], 1);
    run(12, &|a, _| { let x = spread(a, -40, 30); [libm::log1p(if a >> 63 == 1 && x < 1.0 { -x } else { x }), 0.0] }, 1);
    run(13, &|a, _| [libm::log10(spread(a, -40, 40)), 0.0], 1);
    run(14, &|a, b| [libm::pow(spread(a, -10, 10), unit(b) * 8.0), 0.0], 1);
    run(15, &|a, _| [libm::sqrt(spread(a, -60, 60)), 0.0], 1);
    run(16, &|a, _| [libm::cbrt(signed(a, spread(a, -30, 60))), 0.0], 1);
    run(17, &|a, b| [libm::hypot(signed(a, spread(a, -20, 30)), signed(b, spread(b, -20, 30))), 0.0], 1);
    run(18, &|a, _| [libm::erf(unit(a) * 6.0), 0.0], 1);
    run(19, &|a, _| [libm::floor(signed(a, spread(a, -5, 55))), 0.0], 1);
    run(20, &|a, _| [libm::ceil(signed(a, spread(a, -5, 55))), 0.0], 1);
    run(21, &|a, _| [libm::round(signed(a, spread(a, -5, 55))), 0.0], 1);
    run(22, &|a, _| [libm::trunc(signed(a, spread(a, -5, 55))), 0.0], 1);
    run(23, &|a, _| [libm::fabs(signed(a, spread(a, -60, 60))), 0.0], 1);
    run(24, &|a, b| [libm::fmod(signed(a, spread(a, -10, 40)), spread(b, -10, 10)), 0.0], 1);
    // Single precision (the wave shader port): the f64 input narrows exactly once, by IEEE rounding.
    run(25, &|a, _| [libm::sinf(angle(a) as f32) as f64, 0.0], 1);
    run(26, &|a, _| [libm::cosf(angle(a) as f32) as f64, 0.0], 1);
    run(27, &|a, _| [libm::floorf(signed(a, spread(a, -5, 22)) as f32) as f64, 0.0], 1);
    h.finish()
}
