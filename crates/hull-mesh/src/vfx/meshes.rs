//! Unit `hm-vfx-meshes`. The reference mesh of every [`VfxKind`], built with `prim::MeshBuilder`.
//! DESIGN.md L1953 (faceted flat-normal meshes of 20 to 80 triangles, continuous motion, ramps posterised to 4
//! to 6 bands, peak in the first 25% of life then ease-out, ending by scaling to 0), L1959-1969 (per-effect
//! shapes), L1915 (no volumetrics; faceted opaque masses).
//!
//! Meshes are unit-sized (about 1 m) and centred so instance transforms scale them: splash columns are unit
//! hex prisms with their base at y = 0; tracers are unit boxes along +X; wake and trail segments span x 0..1 so a
//! transform stretches them between two trail points. `custom0` carries a per-vertex value the shader uses for
//! continuous motion (height fraction for columns, ring radius fraction for foam). Colours are white; the ramp
//! and instance colour supply albedo and emission.
//!
//! # Frames and `custom0` per kind
//! Every mesh is white, flat shaded, clockwise from outside. `custom0.x` is the kind's main motion coordinate
//! (0..1), `custom0.y` a secondary one, `custom0.z` a per-vertex noise value 0..1 (deterministic), `custom0.w`
//! is 0. The frames match the instance builders in `vfx::runtime`.
//!
//! | Kind | Shape (local frame) | Triangles | `custom0.x` | `custom0.y` |
//! | --- | --- | --- | --- | --- |
//! | Tracer | box -0.5..0.5, length along +X | 12 | 0 tail .. 1 head | 0 |
//! | SplashColumn | tapering hex column, base y = 0, top y = 1, radius 0.5 at the base | 40 | height fraction | radius fraction |
//! | SplashFoam | flat double annulus r 0.3..0.5 at y = 0, facing up | 64 | radius fraction (r / 0.5) | 0 |
//! | HitFlash | core icosphere r 0.25 and six spikes to r 0.5 | 56 | radius fraction | 0 |
//! | HitPuff, Fireball, MuzzleHaze | jittered icosphere r 0.5 | 80 | radius fraction | 0 |
//! | SmokePuff | four jittered 20-triangle lobes within r 0.5 (a clump, not a ball) | 80 | radius fraction | 0 |
//! | Debris | unit cube -0.5..0.5 | 12 | 0 | 0 |
//! | FlamePrism | leaning pentagonal flame, base r 0.45 at y = 0, tip at y = 1 | 28 | height fraction | 0 |
//! | DefenceSmoke | three jittered icospheres within r 0.5 | 60 | radius fraction | 0 |
//! | FlakBurst | two jittered icospheres within r 0.5 | 40 | radius fraction | 0 |
//! | MissileTrail | three crossed double-sided quads, x 0..1, half-size 0.5 | 24 | along fraction | 0 |
//! | WakeSegment | flat ribbon x 0..1, z -0.5..0.5 | 24 | along fraction | across fraction 0..1 |
//! | KelvinArm | flat-topped ridge strip x 0..1, z -0.5..0.5, crest y 0.12 | 24 | along fraction | across fraction |
//! | BowWave | two outward-leaning double-sided sheets from the stem (x 0) aft to x -1, foot to z +-0.5, crest to +-0.62 and y 0.5 | 40 | aft fraction | height fraction |
//! | MuzzleStar | core icosphere r 0.18, forward spike to x 1, four side spikes | 50 | radius fraction | 0 |
//! | PumpArc | square tube along a falling arc from (0, 0, 0) out along +X to (0.9, -0.85, 0) | 48 | arc fraction | 0 |
//! | BoilRing | flat annulus r 0.3..0.5 and a low cone dome inside | 32 | radius fraction | 0 |
//! | FoamBand | flat band x 0..1 (along the hull), z 0..1 (outward), y = 0 | 24 | along fraction | outward fraction |
//! | ChaffCloud | 14 small double-sided facets within r 0.45 | 28 | radius fraction | 0 |
//! | FlareBurn | core icosphere r 0.2 and four spikes to r 0.5 | 44 | radius fraction | 0 |
use super::VfxKind;
use crate::prim::{cross, length, normalize_or, sub, MeshBuilder};
use crate::types::MeshData;
use core::f32::consts::{PI, TAU};

/// Reference mesh of one kind (20 to 80 triangles; tracers and debris cubes 12). Deterministic. Unit: `hm-vfx-meshes`.
pub fn base_mesh(kind: VfxKind) -> MeshData {
    let mut m = match kind {
        VfxKind::Tracer | VfxKind::Debris => { let mut b = MeshBuilder::new(); b.cuboid([-0.5; 3], [0.5; 3]); b.finish() }
        VfxKind::SplashColumn => column(),
        VfxKind::SplashFoam => { let mut b = MeshBuilder::new(); b.ring_y(0.0, 0.0, 0.3, 0.42, 0.0, 16); b.ring_y(0.0, 0.0, 0.42, 0.5, 0.0, 16); b.finish() }
        VfxKind::HitFlash => star(0.25, &[([1.0, 0.0, 0.0], 0.5), ([-1.0, 0.0, 0.0], 0.5), ([0.0, 1.0, 0.0], 0.5), ([0.0, -1.0, 0.0], 0.5), ([0.0, 0.0, 1.0], 0.5), ([0.0, 0.0, -1.0], 0.5)], 0.1),
        VfxKind::HitPuff => puff(&[([0.0; 3], 0.5)], 1, 0.08, 11),
        VfxKind::Fireball => puff(&[([0.0; 3], 0.5)], 1, 0.12, 12),
        VfxKind::SmokePuff => puff(&[([-0.16, -0.08, 0.04], 0.3), ([0.17, -0.05, -0.06], 0.28), ([0.0, 0.14, 0.08], 0.3), ([0.02, 0.0, -0.17], 0.26)], 0, 0.14, 13),
        VfxKind::MuzzleHaze => puff(&[([0.0; 3], 0.5)], 1, 0.06, 14),
        VfxKind::DefenceSmoke => puff(&[([-0.18, -0.06, 0.0], 0.3), ([0.16, -0.08, 0.1], 0.3), ([0.0, 0.16, -0.08], 0.32)], 0, 0.12, 15),
        VfxKind::FlakBurst => puff(&[([-0.08, 0.0, 0.0], 0.4), ([0.22, 0.08, 0.05], 0.26)], 0, 0.15, 16),
        VfxKind::FlamePrism => flame(),
        VfxKind::MissileTrail => trail(),
        VfxKind::WakeSegment => grid_strip(2, 6, |_, _| 0.0),
        VfxKind::KelvinArm => ridge(),
        VfxKind::BowWave => bow_wave(),
        VfxKind::MuzzleStar => star(0.18, &[([1.0, 0.0, 0.0], 1.0), ([0.0, 1.0, 0.0], 0.45), ([0.0, -1.0, 0.0], 0.45), ([0.0, 0.0, 1.0], 0.45), ([0.0, 0.0, -1.0], 0.45)], 0.12),
        VfxKind::PumpArc => pump_arc(),
        VfxKind::BoilRing => { let mut b = MeshBuilder::new(); b.ring_y(0.0, 0.0, 0.3, 0.5, 0.0, 12); b.frustum([0.0; 3], [0.0, 0.12, 0.0], 0.3, 0.0, 8, false); b.finish() }
        VfxKind::FoamBand => foam_band(),
        VfxKind::ChaffCloud => chaff(),
        VfxKind::FlareBurn => star(0.2, &[([0.0, 1.0, 0.0], 0.5), ([0.0, -1.0, 0.0], 0.5), ([1.0, 0.0, 0.0], 0.5), ([-1.0, 0.0, 0.0], 0.5)], 0.09),
    };
    fill_custom(&mut m, kind);
    m
}

/// Deterministic 32-bit hash (a murmur3-style finaliser over two words).
fn hash(a: u32, b: u32) -> u32 {
    let mut h = a.wrapping_mul(0x9E37_79B9) ^ b.wrapping_add(0x7F4A_7C15).rotate_left(13);
    h ^= h >> 16;
    h = h.wrapping_mul(0x85EB_CA6B);
    h ^= h >> 13;
    h = h.wrapping_mul(0xC2B2_AE35);
    h ^ (h >> 16)
}
fn unit(h: u32) -> f32 { (h >> 8) as f32 / (1u32 << 24) as f32 }
/// Noise 0..1 of a position, identical for coincident vertices of neighbouring faces.
fn pos_noise(p: [f32; 3], salt: u32) -> f32 {
    let q = |v: f32| (v * 4096.0).round() as i32 as u32;
    unit(hash(hash(q(p[0]), q(p[1])), q(p[2]) ^ salt))
}

/// Per-vertex `custom0` from the table in the module docs.
fn fill_custom(m: &mut MeshData, kind: VfxKind) {
    let salt = kind as u32;
    m.custom0 = m.positions.iter().map(|&p| {
        let r = length(p);
        let radial = (r / 0.5).min(1.0);
        let (x, y) = match kind {
            VfxKind::Tracer => (p[0] + 0.5, 0.0),
            VfxKind::Debris => (0.0, 0.0),
            VfxKind::SplashColumn => (p[1], (libm::sqrtf(p[0] * p[0] + p[2] * p[2]) / 0.5).min(1.0)),
            VfxKind::FlamePrism => (p[1], 0.0),
            VfxKind::MissileTrail => (p[0].clamp(0.0, 1.0), 0.0),
            VfxKind::PumpArc => ((p[0] / 0.9).clamp(0.0, 1.0), 0.0),
            VfxKind::WakeSegment | VfxKind::KelvinArm => (p[0], p[2] + 0.5),
            VfxKind::BowWave => (-p[0], (p[1] / 0.5).clamp(0.0, 1.0)),
            VfxKind::FoamBand => (p[0], p[2]),
            VfxKind::SplashFoam | VfxKind::BoilRing => ((libm::sqrtf(p[0] * p[0] + p[2] * p[2]) / 0.5).min(1.0), 0.0),
            _ => (radial, 0.0),
        };
        [x, y, pos_noise(p, salt), 0.0]
    }).collect();
}

/// Append `src` to `dst`, re-basing indices.
fn append(dst: &mut MeshData, src: MeshData) {
    let base = dst.positions.len() as u32;
    dst.positions.extend(src.positions);
    dst.normals.extend(src.normals);
    dst.colors.extend(src.colors);
    dst.indices.extend(src.indices.into_iter().map(|i| i + base));
}

/// Recompute every triangle's flat normal from its winding (all three vertices of a triangle are its own).
fn renormal(m: &mut MeshData) {
    for t in m.indices.chunks(3) {
        let (a, b, c) = (m.positions[t[0] as usize], m.positions[t[1] as usize], m.positions[t[2] as usize]);
        let n = normalize_or(cross(sub(c, a), sub(b, a)), m.normals[t[0] as usize]);
        for &i in t { m.normals[i as usize] = n; }
    }
}

/// Faceted puff mass: icospheres (centre, radius) of `subdiv`, each vertex pushed radially by up to `jitter`
/// of the radius so the silhouette is irregular. Icosphere faces are single triangles, so faces stay planar.
fn puff(spheres: &[([f32; 3], f32)], subdiv: u8, jitter: f32, salt: u32) -> MeshData {
    let mut out = MeshData::new();
    for (k, &(c, r)) in spheres.iter().enumerate() {
        let mut b = MeshBuilder::new();
        b.icosphere(c, r, subdiv);
        let mut m = b.finish();
        for p in &mut m.positions {
            let d = sub(*p, c);
            let f = 1.0 + jitter * (2.0 * pos_noise(d, salt.wrapping_mul(31).wrapping_add(k as u32)) - 1.0);
            *p = [c[0] + d[0] * f, c[1] + d[1] * f, c[2] + d[2] * f];
        }
        renormal(&mut m);
        append(&mut out, m);
    }
    out
}

/// Icosphere core of radius `core` (20 triangles) with four-sided spikes (direction, tip radius), each 6
/// triangles, of base radius `spike_r` starting inside the core.
fn star(core: f32, spikes: &[([f32; 3], f32)], spike_r: f32) -> MeshData {
    let mut b = MeshBuilder::new();
    b.icosphere([0.0; 3], core, 0);
    for &(d, tip) in spikes {
        let d = normalize_or(d, [1.0, 0.0, 0.0]);
        let s = core * 0.6;
        b.frustum([d[0] * s, d[1] * s, d[2] * s], [d[0] * tip, d[1] * tip, d[2] * tip], spike_r, 0.0, 4, true);
    }
    b.finish()
}

/// Regular `sides`-gon ring at height `y`, radius `r`, centred on (cx, cz), first corner on +X.
fn ring(sides: usize, cx: f32, cz: f32, r: f32, y: f32) -> Vec<[f32; 3]> {
    (0..sides).map(|i| {
        let a = TAU * i as f32 / sides as f32;
        [cx + r * libm::cosf(a), y, cz + r * libm::sinf(a)]
    }).collect()
}

/// Side quads between two rings of equal length, each facing away from the lower ring's centre.
fn band(b: &mut MeshBuilder, lower: &[[f32; 3]], upper: &[[f32; 3]]) {
    let n = lower.len();
    let (cx, cz) = (lower.iter().map(|p| p[0]).sum::<f32>() / n as f32, lower.iter().map(|p| p[2]).sum::<f32>() / n as f32);
    for i in 0..n {
        let j = (i + 1) % n;
        let out = [(lower[i][0] + lower[j][0]) * 0.5 - cx, 0.0, (lower[i][2] + lower[j][2]) * 0.5 - cz];
        b.face(&[lower[i], lower[j], upper[j], upper[i]], out);
    }
}

/// Splash column: three tapering hex sections and a top cap (40 triangles), base open below the water.
fn column() -> MeshData {
    let mut b = MeshBuilder::new();
    let rings = [ring(6, 0.0, 0.0, 0.5, 0.0), ring(6, 0.0, 0.0, 0.46, 0.45), ring(6, 0.0, 0.0, 0.38, 0.8), ring(6, 0.0, 0.0, 0.22, 1.0)];
    for k in 0..3 { band(&mut b, &rings[k], &rings[k + 1]); }
    b.face(&rings[3], [0.0, 1.0, 0.0]);
    b.finish()
}

/// Flame tongue: two leaning pentagonal sections and a tip cone over a base cap (28 triangles).
fn flame() -> MeshData {
    let mut b = MeshBuilder::new();
    let r0 = ring(5, 0.0, 0.0, 0.45, 0.0);
    let r1 = ring(5, 0.03, 0.0, 0.36, 0.4);
    let r2 = ring(5, 0.07, 0.0, 0.2, 0.75);
    let tip = [0.1, 1.0, 0.0];
    band(&mut b, &r0, &r1);
    band(&mut b, &r1, &r2);
    for i in 0..5 {
        let j = (i + 1) % 5;
        let out = [(r2[i][0] + r2[j][0]) * 0.5 - 0.07, 0.2, (r2[i][2] + r2[j][2]) * 0.5];
        b.face(&[r2[i], r2[j], tip], out);
    }
    b.face(&r0, [0.0, -1.0, 0.0]);
    b.finish()
}

/// Missile trail: three planes through the X axis at 60 degree steps, two lengthwise sections each, both sides
/// (24 triangles).
fn trail() -> MeshData {
    let mut b = MeshBuilder::new();
    for k in 0..3 {
        let a = PI / 3.0 * k as f32;
        let (c, s) = (libm::cosf(a) * 0.5, libm::sinf(a) * 0.5);
        let n = [0.0, -s, c]; // plane normal (perpendicular to the in-plane direction (0, c, s))
        for sec in 0..2 {
            let (x0, x1) = (sec as f32 * 0.5, sec as f32 * 0.5 + 0.5);
            let quad = [[x0, -c, -s], [x1, -c, -s], [x1, c, s], [x0, c, s]];
            b.face(&quad, n);
            b.face(&quad, [0.0, s, -c]);
        }
    }
    b.finish()
}

/// Flat up-facing strip x 0..1, z -0.5..0.5, `along` x `across` quads, height by `y(x, z)`.
fn grid_strip(along: usize, across: usize, y: impl Fn(f32, f32) -> f32) -> MeshData {
    let mut b = MeshBuilder::new();
    for i in 0..along {
        for j in 0..across {
            let (x0, x1) = (i as f32 / along as f32, (i + 1) as f32 / along as f32);
            let (z0, z1) = (j as f32 / across as f32 - 0.5, (j + 1) as f32 / across as f32 - 0.5);
            b.face(&[[x0, y(x0, z0), z0], [x1, y(x1, z0), z0], [x1, y(x1, z1), z1], [x0, y(x0, z1), z1]], [0.0, 1.0, 0.0]);
        }
    }
    b.finish()
}

/// Kelvin arm: a flat-topped ridge across z (-0.5, -0.15, 0.15, 0.5) with crest y 0.12, four sections along x.
fn ridge() -> MeshData {
    let zs = [-0.5f32, -0.15, 0.15, 0.5];
    let ys = [0.0f32, 0.12, 0.12, 0.0];
    let mut b = MeshBuilder::new();
    for i in 0..4 {
        let (x0, x1) = (i as f32 * 0.25, (i + 1) as f32 * 0.25);
        for j in 0..3 {
            let up = [0.0, 1.0, if j == 0 { -0.5 } else if j == 2 { 0.5 } else { 0.0 }];
            b.face(&[[x0, ys[j], zs[j]], [x1, ys[j], zs[j]], [x1, ys[j + 1], zs[j + 1]], [x0, ys[j + 1], zs[j + 1]]], up);
        }
    }
    b.finish()
}

/// Bow wave: per side, five sections of an outward-leaning sheet from the waterline to the crest, emitted on
/// both faces so the inside reads against the hull (40 triangles). The foot runs from the stem out to the
/// half-width at the aft end, along the entry; the crest leans out over it and falls away aft.
fn bow_wave() -> MeshData {
    let mut b = MeshBuilder::new();
    let pt = |t: f32, side: f32, top: bool| -> [f32; 3] {
        let z = side * (0.03 + 0.47 * t);
        if top { [-t, 0.5 * (1.0 - 0.85 * t), z + side * (0.06 + 0.06 * t)] } else { [-t, 0.0, z] }
    };
    for side in [-1.0f32, 1.0] {
        for k in 0..5 {
            let (t0, t1) = (k as f32 / 5.0, (k + 1) as f32 / 5.0);
            let quad = [pt(t0, side, false), pt(t1, side, false), pt(t1, side, true), pt(t0, side, true)];
            b.face(&quad, [0.0, 0.3, side]);
            b.face(&quad, [0.0, 0.3, -side]);
        }
    }
    b.finish()
}

/// Pump discharge: a square tube along the falling arc (0.9 u, 0.45 u - 1.3 u^2), six segments, open ends (48
/// triangles).
fn pump_arc() -> MeshData {
    let mut b = MeshBuilder::new();
    let at = |u: f32| [0.9 * u, 0.45 * u - 1.3 * u * u, 0.0];
    for k in 0..6 {
        let (u0, u1) = (k as f32 / 6.0, (k + 1) as f32 / 6.0);
        let (r0, r1) = (0.06 + 0.05 * u0, 0.06 + 0.05 * u1);
        b.frustum(at(u0), at(u1), r0, r1, 4, false);
    }
    b.finish()
}

/// Foam band: flat strip x 0..1 along the hull, z 0..1 outward, 4 x 3 quads (24 triangles).
fn foam_band() -> MeshData {
    let mut b = MeshBuilder::new();
    for i in 0..4 {
        for j in 0..3 {
            let (x0, x1) = (i as f32 / 4.0, (i + 1) as f32 / 4.0);
            let (z0, z1) = (j as f32 / 3.0, (j + 1) as f32 / 3.0);
            b.face(&[[x0, 0.0, z0], [x1, 0.0, z0], [x1, 0.0, z1], [x0, 0.0, z1]], [0.0, 1.0, 0.0]);
        }
    }
    b.finish()
}

/// Chaff: 14 small facets at hashed positions and orientations, each emitted on both sides (28 triangles).
fn chaff() -> MeshData {
    let mut b = MeshBuilder::new();
    for k in 0..14u32 {
        let h = |i: u32| unit(hash(k, i));
        // Position inside a sphere of radius 0.45 (cube-root radius for an even fill).
        let (u, v) = (h(1) * TAU, 2.0 * h(2) - 1.0);
        let r = 0.45 * libm::cbrtf(0.15 + 0.85 * h(3));
        let sv = libm::sqrtf((1.0 - v * v).max(0.0));
        let c = [r * sv * libm::cosf(u), r * v, r * sv * libm::sinf(u)];
        // Facet plane from two hashed directions.
        let d1 = normalize_or([2.0 * h(4) - 1.0, 2.0 * h(5) - 1.0, 2.0 * h(6) - 1.0], [1.0, 0.0, 0.0]);
        let d2 = normalize_or(cross(d1, [2.0 * h(7) - 1.0, 2.0 * h(8) - 1.0, 2.0 * h(9) - 1.0]), [0.0, 1.0, 0.0]);
        let s = 0.06;
        let p = |a: [f32; 3], w: f32| [c[0] + a[0] * w, c[1] + a[1] * w, c[2] + a[2] * w];
        let tri = [p(d1, s), p(d2, s), p([-d1[0] - d2[0], -d1[1] - d2[1], -d1[2] - d2[2]], s * 0.7)];
        let n = cross(d1, d2);
        b.face(&tri, n);
        b.face(&tri, [-n[0], -n[1], -n[2]]);
    }
    b.finish()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::prim::tests::check_consistent;

    #[test]
    fn every_kind_is_in_budget_and_consistent() {
        for kind in VfxKind::ALL {
            let m = base_mesh(kind);
            let t = m.triangle_count();
            match kind {
                VfxKind::Tracer | VfxKind::Debris => assert_eq!(t, 12, "{kind:?}"),
                _ => assert!((20..=80).contains(&t), "{kind:?} has {t} triangles"),
            }
            check_consistent(&m);
            assert_eq!(m.custom0.len(), m.positions.len(), "{kind:?}");
            assert!(m.colors.iter().all(|c| *c == [1.0; 4]), "{kind:?} not white");
            assert!(m.custom0.iter().flatten().all(|v| v.is_finite() && (0.0..=1.0).contains(v)), "{kind:?} custom0 out of 0..1");
            let bb = m.aabb();
            assert!(bb.min.iter().chain(bb.max.iter()).all(|v| v.abs() <= 1.2), "{kind:?} not unit-sized: {bb:?}");
            assert_eq!(m, base_mesh(kind), "{kind:?} not deterministic");
        }
    }

    #[test]
    fn frames_match_the_builders() {
        // Columns stand on y = 0 and top out at 1; the column base is radius 0.5.
        let c = base_mesh(VfxKind::SplashColumn).aabb();
        assert!((c.min[1]).abs() < 1e-6 && (c.max[1] - 1.0).abs() < 1e-6 && (c.max[0] - 0.5).abs() < 0.01);
        // Segments span x 0..1 so a transform maps x = 0 to a and x = 1 to b.
        for k in [VfxKind::MissileTrail, VfxKind::WakeSegment, VfxKind::KelvinArm, VfxKind::FoamBand] {
            let b = base_mesh(k).aabb();
            assert!(b.min[0].abs() < 1e-6 && (b.max[0] - 1.0).abs() < 1e-6, "{k:?} {b:?}");
        }
        // Tracer box is centred, length 1 along X.
        let t = base_mesh(VfxKind::Tracer).aabb();
        assert_eq!((t.min, t.max), ([-0.5; 3], [0.5; 3]));
        // Flat effects face up.
        for k in [VfxKind::SplashFoam, VfxKind::WakeSegment, VfxKind::FoamBand] {
            assert!(base_mesh(k).normals.iter().all(|n| n[1] > 0.99), "{k:?}");
        }
        // Column motion coordinate is the height fraction.
        let m = base_mesh(VfxKind::SplashColumn);
        assert!(m.positions.iter().zip(&m.custom0).all(|(p, c)| (p[1] - c[0]).abs() < 1e-6));
    }
}
