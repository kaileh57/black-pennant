//! Unit `hm-parts-sensors`. Masts, radars, directors, ESM, sonar domes, cameras and jammers.
//! DESIGN.md L1233 (masts built procedurally), L412 (rotating antennas at 24 rpm, staring sensors), L421-456
//! (sensor families), L796 (camera mast), L1811 (G3: a 5 degree list must read on a 30 m mast at 1 km).
//!
//! Rotating radars are an `Antenna` piece with `Joint::Spin { axis: +Y }` (the client spins it at the
//! sensor's rpm); fire-control dishes and directors are `Turret`/`Cradle` pieces driven like gun mounts.
//! Lattice masts use `MeshBuilder::beam` members so they read as lattice at range without alpha. Sonar domes
//! hang below y = 0 (underwater placement).
//!
//! # Model notes
//! - Masts stand at the footprint centre and rise above it to their `height`. Pole and tripod legs are sized
//!   from height (a 30 m pole is about 1 m thick at the foot, 1 px at 1 km) so G3's list cue survives range;
//!   every mast carries a masthead `Light` anchor at its truck.
//! - The search radar is a curved, slightly up-tilted reflector of 6 x 2 flat facets with a feed horn, or a
//!   planar array face with a rear equipment box (`planar`); both sit on a fixed pedestal, the spinning part is
//!   the `Antenna` piece. The navigation radar is a slotted bar.
//! - Fire-control radars are a dish on a yoke (`Turret` Train, `Cradle` Elevate) with a concave front, a
//!   three-strut feed and a rear electronics box. Directors are a faceted house with glazing and rangefinder
//!   ears that trains, and an elevating sensor head with two lenses. Camera masts are a pole with the same
//!   train-and-elevate head arrangement, as the camera mast is a trainable mount (L795).
//! - Sonar domes are rubber (`Tone::Black`, no ship paint below the waterline) and hang from a faired strut
//!   whose top is at y = 0.
use super::{AnchorKind, Generator, Joint, PartModel, PieceRole, Tone};
use crate::prim::kit::{builder, disc_x, ellipse_ring, foot_m, footprint_block, ring_x, sanitize, set_tone, Kit};
use crate::prim::{add, cross, mul, normalize_or, sub, MeshBuilder, Xform};

const DEG: f32 = core::f32::consts::PI / 180.0;
const SPIN_Y: Joint = Joint::Spin { axis: [0.0, 1.0, 0.0] };

/// Build `g` (a sensors-family generator) from resolved parameters. Unit: `hm-parts-sensors`.
pub fn build(g: Generator, params: &[f32], footprint: [u8; 3]) -> PartModel {
    let p = sanitize(g, params);
    let f = foot_m(footprint);
    match g {
        Generator::MastPole => mast_pole(p[0], p[1] as usize, f),
        Generator::MastLattice => mast_lattice(p[0], p[1], p[2] as usize, f),
        Generator::MastTripod => mast_tripod(p[0], p[1], f),
        Generator::RadarSearch => radar_search(p[0], p[1], p[2] >= 0.5, f),
        Generator::RadarNav => radar_nav(p[0], f),
        Generator::RadarFireControl => radar_fire_control(p[0], f),
        Generator::Director => director(p[0], f),
        Generator::Esm => esm(p[0] as usize, f),
        Generator::SonarDome => sonar_dome(p[0], f),
        Generator::Camera => camera(p[0], p[1], f),
        Generator::Jammer => jammer(p[0] as usize, f),
        _ => footprint_block(footprint),
    }
}

/// One fixed base piece and a masthead light at `top`.
fn mast_model(mesh: crate::types::MeshData, top: [f32; 3]) -> PartModel {
    let mut kit = Kit::new();
    let base = kit.piece(PieceRole::Base, None, [0.0; 3], Joint::Fixed, mesh);
    kit.anchor(AnchorKind::Light, base, top, [1.0, 0.0, 0.0]);
    kit.finish()
}

/// A yard across Z at height `y`.
fn yard(b: &mut MeshBuilder, cx: f32, cz: f32, y: f32, half: f32, hw: f32) {
    b.beam([cx, y, cz - half], [cx, y, cz + half], hw);
}

// ---------------------------------------------------------------------------------------------------------
// Masts

fn mast_pole(height: f32, yards: usize, f: [f32; 3]) -> PartModel {
    let (cx, cz) = (f[0] * 0.5, f[2] * 0.5);
    let r0 = (0.05 + 0.016 * height).clamp(0.08, 0.6);
    let r1 = r0 * 0.45;
    let mut b = builder(Tone::Paint);
    b.prism_y(cx, cz, (r0 * 1.8).min(f[0].min(f[2]) * 0.5).max(r0), 0.0, 0.12, 8, true);
    b.frustum([cx, 0.0, cz], [cx, height, cz], r0, r1, 8, false);
    b.prism_y(cx, cz, r1 * 1.6, height, height + r1 * 1.2, 8, true);
    for i in 0..yards {
        let y = height * (0.8 - 0.12 * i as f32);
        let r_at = r0 + (r1 - r0) * 0.8;
        yard(&mut b, cx, cz, y, (height * 0.11).clamp(0.5, 3.0), (r_at * 0.5).max(0.04));
    }
    mast_model(b.finish(), [cx, height + r1 * 1.2, cz])
}

fn mast_lattice(height: f32, base: f32, platforms: usize, f: [f32; 3]) -> PartModel {
    let (cx, cz) = (f[0] * 0.5, f[2] * 0.5);
    let b0 = (base.min(f[0]).min(f[2]) * 0.5).max(0.2);
    let h_l = height * 0.86;
    let b1 = (b0 * 0.3).clamp(0.12, b0);
    let leg = (height * 0.006).clamp(0.04, 0.16);
    let brace = leg * 0.6;
    let bays = (libm::ceilf(h_l / (b0 + b1)) as usize).clamp(2, 12);
    let half = |y: f32| b0 + (b1 - b0) * (y / h_l);
    const SIGNS: [(f32, f32); 4] = [(1.0, 1.0), (1.0, -1.0), (-1.0, -1.0), (-1.0, 1.0)];
    let corner = |k: usize, y: f32| { let (sx, sz) = SIGNS[k % 4]; let s = half(y); [cx + sx * s, y, cz + sz * s] };
    let mut b = builder(Tone::Paint);
    // Raked legs start one member-width up, inside a foot shoe, so their slanted ends stay above the deck.
    for k in 0..4 {
        let c0 = corner(k, 0.0);
        b.cuboid([c0[0] - leg * 2.0, 0.0, c0[2] - leg * 2.0], [c0[0] + leg * 2.0, leg * 2.5, c0[2] + leg * 2.0]);
        b.beam(corner(k, leg * 1.5), corner(k, h_l), leg);
    }
    for j in 0..bays {
        let (y0, y1) = ((h_l * j as f32 / bays as f32).max(leg * 2.0), h_l * (j + 1) as f32 / bays as f32);
        for k in 0..4 {
            b.strut(corner(k, y1), corner(k + 1, y1), brace);
            if (j + k) % 2 == 0 { b.strut(corner(k, y0), corner(k + 1, y1), brace); } else { b.strut(corner(k + 1, y0), corner(k, y1), brace); }
        }
    }
    // Cap plate and topmast.
    b.cuboid([cx - b1 - leg, h_l - 0.04, cz - b1 - leg], [cx + b1 + leg, h_l + 0.04, cz + b1 + leg]);
    b.frustum([cx, h_l, cz], [cx, height, cz], (b1 * 0.5).max(0.05), (b1 * 0.2).max(0.03), 6, true);
    for i in 0..platforms {
        let y = h_l * (1.0 - 0.28 * i as f32);
        let s = half(y) + 0.35;
        b.cuboid([cx - s, y - 0.06, cz - s], [cx + s, y + 0.02, cz + s]);
    }
    mast_model(b.finish(), [cx, height, cz])
}

fn mast_tripod(height: f32, spread: f32, f: [f32; 3]) -> PartModel {
    let (cx, cz) = (f[0] * 0.5, f[2] * 0.5);
    let r0 = (0.06 + 0.018 * height).clamp(0.1, 0.6);
    let r_top = r0 * 0.6;
    let spread = spread.min(f[2] * 0.95).max(0.3);
    let back = (spread * 0.8).min(cx * 0.95).max(0.2);
    let y_star = height * 0.72;
    let mut b = builder(Tone::Paint);
    b.frustum([cx, 0.0, cz], [cx, y_star, cz], r0, r_top, 8, true);
    for s in [-1.0f32, 1.0] {
        let foot = [cx - back, 0.0, cz + s * spread * 0.5];
        b.prism_y(foot[0], foot[2], r0, 0.0, r0 * 1.2, 8, true);
        b.frustum([foot[0], r0 * 0.8, foot[2]], [cx, height * 0.7, cz], r0 * 0.7, r_top * 0.8, 8, true);
    }
    let side = (r0 * 3.0 + 0.3).max(0.6);
    b.cuboid([cx - side, y_star, cz - side], [cx + side, y_star + 0.12, cz + side]);
    b.frustum([cx, y_star + 0.12, cz], [cx, height, cz], r_top * 0.7, r_top * 0.35, 6, true);
    yard(&mut b, cx, cz, height * 0.86, (height * 0.1).clamp(0.5, 3.0), (r_top * 0.4).max(0.04));
    mast_model(b.finish(), [cx, height, cz])
}

// ---------------------------------------------------------------------------------------------------------
// Rotating radars

fn radar_search(width: f32, height: f32, planar: bool, f: [f32; 3]) -> PartModel {
    let [fx, fy, fz] = f;
    let (cx, cz) = (fx * 0.5, fz * 0.5);
    let y_p = (fy * 0.4).clamp(0.2, 0.8);
    let r_ped = (width * 0.07).clamp(0.12, 0.45);
    let mut b = builder(Tone::Paint);
    b.prism_y(cx, cz, r_ped, 0.0, y_p, 8, true);
    b.cuboid([cx - r_ped * 1.3, y_p * 0.55, cz - r_ped], [cx + r_ped * 1.3, y_p, cz + r_ped]);
    let mut kit = Kit::new();
    let base = kit.piece(PieceRole::Base, None, [0.0; 3], Joint::Fixed, b.finish());

    let mut a = builder(Tone::Paint);
    a.prism_y(0.0, 0.0, r_ped * 0.5, 0.0, 0.15, 8, true);
    if planar {
        let t = (height * 0.12).clamp(0.08, 0.3);
        let yc = 0.25 + height * 0.5;
        a.prism_y(0.0, 0.0, r_ped * 0.4, 0.15, yc, 8, false);
        a.with(Xform::translation([0.0, yc, 0.0]).then(&Xform::rot_z(15.0 * DEG)), |a| {
            a.cuboid([-t * 0.5, -height * 0.5, -width * 0.5], [t * 0.5, height * 0.5, width * 0.5]);
            a.cuboid([-t * 0.5 - 0.3 * height - 0.1, -height * 0.3, -width * 0.2], [-t * 0.5, height * 0.3, width * 0.2]);
            set_tone(a, Tone::Pale);
            a.cuboid([t * 0.5, -height * 0.47, -width * 0.48], [t * 0.5 + 0.02, height * 0.47, width * 0.48]);
        });
    } else {
        reflector(&mut a, width, height);
    }
    kit.piece(PieceRole::Antenna, Some(base), [cx, y_p, cz], SPIN_Y, a.finish());
    kit.finish()
}

/// Curved search-radar reflector (concave toward +X), tilted up 10 degrees, with a boom and feed horn, in the
/// antenna frame (spin axis through the origin).
fn reflector(a: &mut MeshBuilder, width: f32, height: f32) {
    const NS: usize = 6;
    let depth = width * 0.12;
    let t = (width * 0.015).clamp(0.03, 0.08);
    let x_off = depth * 0.6;
    let y_bot = 0.25;
    let y_mid = y_bot + height * 0.5;
    let pt = |i: usize, j: usize| -> [f32; 3] {
        let u = -1.0 + 2.0 * i as f32 / NS as f32;
        let v = j as f32 - 1.0;
        let x = x_off - depth * (1.0 - u * u) - 0.3 * depth * (1.0 - v * v);
        [x, y_mid + v * height * 0.5, u * width * 0.5]
    };
    let back = |p: [f32; 3]| [p[0] - t, p[1], p[2]];
    let tilt = Xform::translation([0.0, y_mid, 0.0]).then(&Xform::rot_z(10.0 * DEG)).then(&Xform::translation([0.0, -y_mid, 0.0]));
    a.with(tilt, |a| {
        for j in 0..2 {
            for i in 0..NS {
                let q = [pt(i, j), pt(i + 1, j), pt(i + 1, j + 1), pt(i, j + 1)];
                a.face(&q, [1.0, 0.0, 0.0]);
                a.face(&[back(q[0]), back(q[1]), back(q[2]), back(q[3])], [-1.0, 0.0, 0.0]);
            }
        }
        for i in 0..NS {
            let (p0, p1) = (pt(i, 0), pt(i + 1, 0));
            a.face(&[p0, p1, back(p1), back(p0)], [0.0, -1.0, 0.0]);
            let (p0, p1) = (pt(i, 2), pt(i + 1, 2));
            a.face(&[p0, p1, back(p1), back(p0)], [0.0, 1.0, 0.0]);
        }
        for (i, s) in [(0usize, -1.0f32), (NS, 1.0)] {
            for j in 0..2 {
                let (p0, p1) = (pt(i, j), pt(i, j + 1));
                a.face(&[p0, p1, back(p1), back(p0)], [0.0, 0.0, s]);
            }
        }
        // Boom from the spindle to the reflector back, and the feed horn on its arm at the focus.
        let back_centre = back(pt(NS / 2, 1));
        a.beam([0.0, 0.12, 0.0], [back_centre[0] - 0.02, back_centre[1], 0.0], (width * 0.02).clamp(0.03, 0.08));
        let focus = [x_off + width * 0.22, y_bot + height * 0.4, 0.0];
        set_tone(a, Tone::Metal);
        a.beam(pt(NS / 2, 0), focus, (width * 0.008).clamp(0.015, 0.04));
        let s = (width * 0.03).clamp(0.04, 0.15);
        a.cuboid([focus[0] - s, focus[1] - s, -s], [focus[0] + s, focus[1] + s, s]);
        set_tone(a, Tone::Paint);
    });
}

fn radar_nav(length: f32, f: [f32; 3]) -> PartModel {
    let [fx, fy, fz] = f;
    let (cx, cz) = (fx * 0.5, fz * 0.5);
    let y_p = (fy * 0.3).clamp(0.15, 0.5);
    let (gx, gz) = (0.18 + length * 0.04, 0.15 + length * 0.03);
    let mut b = builder(Tone::Paint);
    b.cuboid([cx - gx, 0.0, cz - gz], [cx + gx, y_p, cz + gz]);
    let mut kit = Kit::new();
    let base = kit.piece(PieceRole::Base, None, [0.0; 3], Joint::Fixed, b.finish());

    let d = 0.08 + length * 0.02;
    let hb = 0.12 + length * 0.04;
    let mut a = builder(Tone::Paint);
    a.prism_y(0.0, 0.0, 0.05, 0.0, 0.08, 8, true);
    set_tone(&mut a, Tone::Pale);
    let profile = [[-d * 0.5, 0.08], [d * 0.5, 0.08 + hb * 0.2], [d * 0.5, 0.08 + hb * 0.8], [-d * 0.5, 0.08 + hb]];
    a.with(Xform::translation([0.0, 0.0, -length * 0.5]), |a| a.extrude_z(&profile, length));
    set_tone(&mut a, Tone::Black);
    a.cuboid([d * 0.5, 0.08 + hb * 0.35, -length * 0.47], [d * 0.5 + 0.01, 0.08 + hb * 0.65, length * 0.47]);
    kit.piece(PieceRole::Antenna, Some(base), [cx, y_p, cz], SPIN_Y, a.finish());
    kit.finish()
}

// ---------------------------------------------------------------------------------------------------------
// Trainable sensors: fire-control radar, director, camera

/// Yoke for a trainable sensor head: turntable and two arms either side of a head `half_w` wide, trunnion at
/// `t`. Built in the turret frame.
fn yoke(b: &mut MeshBuilder, r_table: f32, half_w: f32, t: f32, arm: f32) {
    b.prism_y(0.0, 0.0, r_table, 0.0, 0.1, 10, true);
    b.cuboid([-arm * 1.5, 0.05, -(half_w + arm)], [arm * 1.5, 0.12, half_w + arm]);
    for s in [-1.0f32, 1.0] {
        let (z0, z1) = (s * half_w, s * (half_w + arm));
        b.cuboid([-arm * 1.5, 0.1, z0.min(z1)], [arm * 1.5, t + arm, z0.max(z1)]);
    }
}

fn radar_fire_control(dish: f32, f: [f32; 3]) -> PartModel {
    let [fx, fy, fz] = f;
    let (cx, cz) = (fx * 0.5, fz * 0.5);
    let y_p = (fy * 0.35).clamp(0.2, 0.8);
    let r_ped = (dish * 0.22).max(0.12);
    let mut b = builder(Tone::Paint);
    b.prism_y(cx, cz, r_ped, 0.0, y_p, 10, true);
    b.prism_y(cx, cz, r_ped * 1.25, y_p - 0.05, y_p, 10, true);
    let mut kit = Kit::new();
    let base = kit.piece(PieceRole::Base, None, [0.0; 3], Joint::Fixed, b.finish());

    let rr = dish * 0.5;
    let t = dish * 0.55 + 0.15;
    let mut y = builder(Tone::Paint);
    yoke(&mut y, r_ped * 1.2, rr + 0.06, t, (0.04 + dish * 0.03).min(0.12));
    let turret = kit.piece(PieceRole::Turret, Some(base), [cx, y_p, cz], Joint::Train, y.finish());

    let dd = dish * 0.18;
    let n = 12;
    let hub = ring_x(-dd * 0.5, 0.0, 0.0, dish * 0.12, dish * 0.12, n);
    let rim = ring_x(dd * 0.5, 0.0, 0.0, rr, rr, n);
    let mut c = builder(Tone::Paint);
    c.loft(&[hub, rim.clone()], true, false);
    c.cuboid([-dd * 0.5 - dish * 0.25, -dish * 0.15, -dish * 0.15], [-dd * 0.5, dish * 0.15, dish * 0.15]);
    // Concave front: a shallow cone pointing back into the dish.
    set_tone(&mut c, Tone::Pale);
    let apex = [dd * 0.5 - dd * 0.7, 0.0, 0.0];
    for i in 0..n { c.face(&[rim[i], rim[(i + 1) % n], apex], [1.0, 0.0, 0.0]); }
    // Three-strut feed and horn at the focus.
    set_tone(&mut c, Tone::Metal);
    let focus = [dd * 0.5 + dish * 0.35, 0.0, 0.0];
    for k in [0usize, n / 3, 2 * n / 3] { c.strut(rim[k], focus, (dish * 0.012).max(0.01)); }
    c.frustum([focus[0] + 0.06, 0.0, 0.0], [focus[0] - 0.04, 0.0, 0.0], dish * 0.03 + 0.01, dish * 0.06 + 0.02, 6, true);
    kit.piece(PieceRole::Cradle, Some(turret), [0.0, t, 0.0], Joint::Elevate, c.finish());
    kit.finish()
}

fn director(size: f32, f: [f32; 3]) -> PartModel {
    let [fx, fy, fz] = f;
    let (cx, cz) = (fx * 0.5, fz * 0.5);
    let s = size;
    let y_p = (fy * 0.3).clamp(0.15, 0.6);
    let mut b = builder(Tone::Paint);
    b.prism_y(cx, cz, s * 0.3, 0.0, y_p, 8, true);
    let mut kit = Kit::new();
    let base = kit.piece(PieceRole::Base, None, [0.0; 3], Joint::Fixed, b.finish());

    let hh = s * 0.5;
    let bot = [(0.45, -0.28), (0.2, -0.45), (-0.45, -0.45), (-0.45, 0.45), (0.2, 0.45), (0.45, 0.28)];
    let top = [(0.3, -0.22), (0.12, -0.38), (-0.42, -0.38), (-0.42, 0.38), (0.12, 0.38), (0.3, 0.22)];
    let ring = |pts: &[(f32, f32)], y: f32| -> Vec<[f32; 3]> { pts.iter().map(|&(x, z)| [x * s, y, z * s]).collect() };
    let mut t = builder(Tone::Paint);
    t.loft(&[ring(&bot, 0.0), ring(&top, hh)], false, true);
    // Glazing band on the sloped front face, just proud of it.
    let fb = |z: f32| [bot[0].0 * s, 0.0, z * s];
    let ft = |z: f32| [top[0].0 * s, hh, z * s];
    let n = normalize_or(cross(sub(ft(-0.22), fb(-0.28)), sub(fb(0.28), fb(-0.28))), [1.0, 0.0, 0.0]);
    let n = if n[0] < 0.0 { mul(n, -1.0) } else { n };
    let on_face = |u: f32, v: f32| {
        let p0 = add(fb(-0.28 * u), mul(sub(ft(-0.22 * u), fb(-0.28 * u)), v));
        add(p0, mul(n, 0.006))
    };
    set_tone(&mut t, Tone::Glass);
    t.face(&[on_face(0.8, 0.45), on_face(-0.8, 0.45), on_face(-0.8, 0.75), on_face(0.8, 0.75)], n);
    // Rangefinder ears across the house.
    set_tone(&mut t, Tone::Paint);
    let ry = hh * 0.8;
    t.beam([-s * 0.1, ry, -s * 0.72], [-s * 0.1, ry, s * 0.72], s * 0.07);
    for side in [-1.0f32, 1.0] {
        let z = side * s * 0.72;
        t.cuboid([-s * 0.18, ry - s * 0.08, z - s * 0.08], [-s * 0.02, ry + s * 0.08, z + s * 0.08]);
        set_tone(&mut t, Tone::Black);
        t.cuboid([-s * 0.02, ry - s * 0.04, z - s * 0.04], [-s * 0.01, ry + s * 0.04, z + s * 0.04]);
        set_tone(&mut t, Tone::Paint);
    }
    // Roof yoke for the elevating head.
    let trunnion = hh + s * 0.22;
    for side in [-1.0f32, 1.0] {
        let (z0, z1) = (side * s * 0.27, side * s * 0.32);
        t.cuboid([-s * 0.1, hh, z0.min(z1)], [s * 0.1, trunnion + s * 0.06, z0.max(z1)]);
    }
    let turret = kit.piece(PieceRole::Turret, Some(base), [cx, y_p, cz], Joint::Train, t.finish());

    let mut c = builder(Tone::Paint);
    c.cuboid([-s * 0.2, -s * 0.14, -s * 0.24], [s * 0.2, s * 0.14, s * 0.24]);
    set_tone(&mut c, Tone::Glass);
    disc_x(&mut c, [s * 0.2 + 0.003, 0.0, -s * 0.1], s * 0.08, 8);
    disc_x(&mut c, [s * 0.2 + 0.003, 0.0, s * 0.11], s * 0.05, 8);
    kit.piece(PieceRole::Cradle, Some(turret), [0.0, trunnion, 0.0], Joint::Elevate, c.finish());
    kit.finish()
}

fn camera(head: f32, mast: f32, f: [f32; 3]) -> PartModel {
    let [fx, _fy, fz] = f;
    let (cx, cz) = (fx * 0.5, fz * 0.5);
    let r_m = (0.05 + 0.012 * mast).clamp(0.06, 0.25);
    let y_top = mast.max(0.1);
    let mut b = builder(Tone::Paint);
    b.prism_y(cx, cz, (r_m * 2.0).min(fx.min(fz) * 0.5).max(r_m), 0.0, 0.06, 8, true);
    b.frustum([cx, 0.0, cz], [cx, y_top, cz], r_m, r_m * 0.8, 8, true);
    let mut kit = Kit::new();
    let base = kit.piece(PieceRole::Base, None, [0.0; 3], Joint::Fixed, b.finish());

    let h = head;
    let t = h * 0.55;
    let mut y = builder(Tone::Paint);
    yoke(&mut y, h * 0.4, h * 0.45, t, (h * 0.06).max(0.02));
    let turret = kit.piece(PieceRole::Turret, Some(base), [cx, y_top, cz], Joint::Train, y.finish());

    let mut c = builder(Tone::Paint);
    c.cuboid([-h * 0.45, -h * 0.3, -h * 0.42], [h * 0.4, h * 0.3, h * 0.42]);
    c.cuboid([-h * 0.45, h * 0.3, -h * 0.44], [h * 0.55, h * 0.34, h * 0.44]);
    for (z, r) in [(-h * 0.16, h * 0.17), (h * 0.22, h * 0.09)] {
        set_tone(&mut c, Tone::Black);
        c.frustum([h * 0.4, 0.0, z], [h * 0.47, 0.0, z], r, r, 8, true);
        set_tone(&mut c, Tone::Glass);
        disc_x(&mut c, [h * 0.47 + 0.002, 0.0, z], r * 0.85, 8);
    }
    kit.piece(PieceRole::Cradle, Some(turret), [0.0, t, 0.0], Joint::Elevate, c.finish());
    kit.finish()
}

// ---------------------------------------------------------------------------------------------------------
// Fixed sensors: ESM, sonar dome, jammer

fn esm(domes: usize, f: [f32; 3]) -> PartModel {
    let [fx, fy, fz] = f;
    let (cx, cz) = (fx * 0.5, fz * 0.5);
    let n = domes.max(1);
    let r_p = (fx.min(fz) * 0.4).max(0.3);
    let h0 = (fy * 0.3).clamp(0.2, 0.8);
    let mut b = builder(Tone::Paint);
    b.prism_y(cx, cz, 0.1 + r_p * 0.1, 0.0, h0, 8, true);
    b.prism_y(cx, cz, r_p, h0, h0 + 0.05, 8, true);
    let rd = (r_p * 0.3).clamp(0.08, 0.3);
    let start = match n { 2 => 90.0 * DEG, 4 => 45.0 * DEG, _ => 0.0 };
    let ring_r = if n == 1 { 0.0 } else { r_p - rd * 1.1 };
    set_tone(&mut b, Tone::Pale);
    let sides = 8;
    let st = core::f32::consts::PI / sides as f32;
    for k in 0..n {
        let a = start + core::f32::consts::TAU * k as f32 / n as f32;
        let (px, pz) = (cx + ring_r * libm::cosf(a), cz + ring_r * libm::sinf(a));
        let y0 = h0 + 0.05;
        let y1 = y0 + rd * 1.4;
        let rings = vec![
            ellipse_ring(px, y0, pz, rd, rd, sides, st),
            ellipse_ring(px, y1, pz, rd, rd, sides, st),
            ellipse_ring(px, y1 + rd * 0.4, pz, rd * 0.8, rd * 0.8, sides, st),
            ellipse_ring(px, y1 + rd * 0.7, pz, rd * 0.4, rd * 0.4, sides, st),
            ellipse_ring(px, y1 + rd * 0.8, pz, rd * 0.08, rd * 0.08, sides, st),
        ];
        b.loft(&rings, false, true);
    }
    let mut kit = Kit::new();
    kit.piece(PieceRole::Base, None, [0.0; 3], Joint::Fixed, b.finish());
    kit.finish()
}

fn sonar_dome(length: f32, f: [f32; 3]) -> PartModel {
    let (cx, cz) = (f[0] * 0.5, f[2] * 0.5);
    let r_max = length * 0.16;
    let h_s = 0.1 + length * 0.08;
    let mut b = builder(Tone::Black);
    let st = core::f32::consts::PI / 8.0;
    b.loft(&[ellipse_ring(cx, 0.0, cz, length * 0.35, r_max * 0.35, 8, st), ellipse_ring(cx, -h_s - r_max * 0.5, cz, length * 0.3, r_max * 0.3, 8, st)], true, true);
    const PROFILE: [(f32, f32); 7] = [(0.0, 0.0), (0.05, 0.55), (0.15, 0.85), (0.3, 1.0), (0.55, 0.95), (0.8, 0.6), (1.0, 0.1)];
    let yc = -h_s - r_max;
    let rings: Vec<Vec<[f32; 3]>> = PROFILE.iter().map(|&(t, s)| ring_x(cx + length * 0.5 - t * length, yc, cz, r_max * s, r_max * s, 10)).collect();
    b.loft(&rings, false, true);
    let mut kit = Kit::new();
    kit.piece(PieceRole::Base, None, [0.0; 3], Joint::Fixed, b.finish());
    kit.finish()
}

fn jammer(panels: usize, f: [f32; 3]) -> PartModel {
    let [fx, fy, fz] = f;
    let (cx, cz) = (fx * 0.5, fz * 0.5);
    let w = (fx.min(fz) * 0.3).max(0.25);
    let h_b = (fy * 0.3).clamp(0.3, 1.2);
    let oct = core::f32::consts::PI / 8.0;
    let mut b = builder(Tone::Paint);
    b.prism_y(cx, cz, w / libm::cosf(oct), 0.0, h_b, 8, true);
    let bearings: &[f32] = match panels { 0 | 1 => &[0.0], 2 => &[90.0, 270.0], 3 => &[0.0, 135.0, 225.0], _ => &[45.0, 135.0, 225.0, 315.0] };
    let tp = 0.1;
    let pw = 2.0 * w * libm::tanf(oct) * 1.1;
    let ph = h_b * 0.8;
    set_tone(&mut b, Tone::Pale);
    for &brg in bearings {
        let place = Xform::translation([cx, h_b * 0.5, cz]).then(&Xform::rot_y(brg * DEG)).then(&Xform::translation([w + tp * 0.5, 0.0, 0.0])).then(&Xform::rot_z(8.0 * DEG));
        b.with(place, |b| b.cuboid([-tp * 0.5, -ph * 0.5, -pw * 0.5], [tp * 0.5, ph * 0.5, pw * 0.5]));
    }
    b.icosphere([cx, h_b, cz], w * 0.4, 1);
    let mut kit = Kit::new();
    kit.piece(PieceRole::Base, None, [0.0; 3], Joint::Fixed, b.finish());
    kit.finish()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parts::{Family, GENERATORS};
    use crate::prim::tests::check_consistent;

    fn sensors() -> Vec<Generator> { GENERATORS.iter().filter(|s| s.family == Family::Sensors).map(|s| s.generator).collect() }

    fn param_sets(g: Generator) -> Vec<Vec<f32>> {
        let ps = g.spec().params;
        vec![ps.iter().map(|p| p.min).collect(), ps.iter().map(|p| p.default).collect(), ps.iter().map(|p| p.max).collect()]
    }

    fn check_model(m: &PartModel) {
        assert!(!m.pieces.is_empty());
        for (i, p) in m.pieces.iter().enumerate() {
            if let Some(q) = p.parent { assert!((q as usize) < i); } else { assert_eq!(i, 0); }
            assert!(!p.mesh.is_empty(), "empty piece {i}");
            check_consistent(&p.mesh);
        }
        for a in &m.anchors { assert!((a.piece as usize) < m.pieces.len()); }
        let tris: usize = m.pieces.iter().map(|p| p.mesh.triangle_count()).sum();
        assert!(tris < 2000, "{tris} triangles");
    }

    #[test]
    fn every_sensor_builds_within_budget_at_range_extremes() {
        for g in sensors() {
            for params in param_sets(g) {
                for fp in [[1, 1, 1], [2, 2, 2], [6, 4, 6], [16, 8, 16], [0, 0, 0], [255, 255, 255]] {
                    check_model(&build(g, &params, fp));
                }
            }
        }
    }

    #[test]
    fn rotating_radars_spin_about_y() {
        for (g, params) in [(Generator::RadarSearch, vec![3.0, 1.0, 0.0]), (Generator::RadarSearch, vec![4.0, 2.0, 1.0]), (Generator::RadarNav, vec![2.0])] {
            let m = build(g, &params, [4, 2, 4]);
            let ant: Vec<_> = m.pieces.iter().filter(|p| p.role == PieceRole::Antenna).collect();
            assert_eq!(ant.len(), 1);
            assert_eq!(ant[0].joint, Joint::Spin { axis: [0.0, 1.0, 0.0] });
            assert_eq!(ant[0].parent, Some(0));
            // The antenna is about as wide as asked.
            let bb = ant[0].mesh.aabb();
            let want = params[0];
            assert!((bb.max[2] - bb.min[2] - want).abs() < want * 0.1 + 0.05, "{g:?} width {}", bb.max[2] - bb.min[2]);
        }
    }

    #[test]
    fn trainable_sensors_have_turret_and_cradle() {
        for (g, params) in [(Generator::RadarFireControl, vec![1.2]), (Generator::Director, vec![2.0]), (Generator::Camera, vec![0.5, 3.0])] {
            let m = build(g, &params, [4, 4, 4]);
            let t = m.pieces.iter().position(|p| p.role == PieceRole::Turret).unwrap();
            let c = m.pieces.iter().position(|p| p.role == PieceRole::Cradle).unwrap();
            assert_eq!(m.pieces[t].joint, Joint::Train);
            assert_eq!(m.pieces[c].joint, Joint::Elevate);
            assert_eq!(m.pieces[c].parent, Some(t as u8));
        }
        // Directors and cameras carry glazing (glass flag in custom0.y).
        for (g, params) in [(Generator::Director, vec![2.0]), (Generator::Camera, vec![0.5, 3.0])] {
            let m = build(g, &params, [4, 4, 4]);
            assert!(m.pieces.iter().any(|p| p.mesh.custom0.iter().any(|c| c[1] == 1.0)));
        }
    }

    #[test]
    fn masts_reach_their_height_and_carry_a_masthead_light() {
        for (g, h) in [(Generator::MastPole, 30.0), (Generator::MastLattice, 40.0), (Generator::MastTripod, 12.0), (Generator::MastPole, 2.0)] {
            let m = build(g, &[h], [4, 2, 4]);
            assert!(m.aabb.max[1] >= h - 1e-3 && m.aabb.max[1] < h + 0.5, "{g:?} top {}", m.aabb.max[1]);
            let lights: Vec<_> = m.anchors.iter().filter(|a| a.kind == AnchorKind::Light).collect();
            assert_eq!(lights.len(), 1);
            assert!(lights[0].pos[1] >= h - 1e-3);
            // Feet stay inside the footprint.
            assert!(m.aabb.min[1] >= -1e-3);
        }
        // G3: a 30 m pole is at least 1 m thick at the foot (about 1 px at 1 km).
        let m = build(Generator::MastPole, &[30.0, 0.0], [4, 2, 4]);
        let bb = m.pieces[0].mesh.aabb();
        assert!(bb.max[0] - bb.min[0] >= 1.0);
    }

    #[test]
    fn lattice_masts_are_built_from_members() {
        let m = build(Generator::MastLattice, &[15.0, 3.0, 1.0], [6, 2, 6]);
        let tris = m.pieces[0].mesh.triangle_count();
        assert!(tris > 300, "lattice has {tris} triangles");
        assert!(m.aabb.max[0] - m.aabb.min[0] <= 3.0 + 0.8);
    }

    #[test]
    fn sonar_domes_hang_below_the_waterline_plane() {
        for l in [1.0, 3.0, 6.0] {
            let m = build(Generator::SonarDome, &[l], [6, 2, 4]);
            assert!(m.aabb.max[1] <= 1e-4, "dome top {}", m.aabb.max[1]);
            assert!(m.aabb.min[1] < -l * 0.3);
            assert!((m.aabb.max[0] - m.aabb.min[0] - l).abs() < 0.05);
            assert!(m.pieces[0].mesh.colors.iter().all(|c| c[3] == 0.0), "no ship paint below the waterline");
        }
    }

    #[test]
    fn esm_and_jammer_counts() {
        for n in 1..=4 {
            let m = build(Generator::Esm, &[n as f32], [3, 2, 3]);
            check_model(&m);
            let m = build(Generator::Jammer, &[n as f32], [3, 2, 3]);
            check_model(&m);
        }
        // More domes, more geometry.
        let t = |n: f32| build(Generator::Esm, &[n], [3, 2, 3]).pieces[0].mesh.triangle_count();
        assert!(t(1.0) < t(2.0) && t(2.0) < t(4.0));
    }

    #[test]
    fn foreign_generators_and_junk_never_panic() {
        check_model(&build(Generator::GunTurret, &[], [2, 2, 2]));
        check_model(&build(Generator::RadarSearch, &[f32::NAN, f32::INFINITY, -3.0], [2, 2, 2]));
    }

    #[test]
    fn models_are_deterministic() {
        for g in sensors() { assert_eq!(build(g, &[], [4, 4, 4]), build(g, &[], [4, 4, 4])); }
    }
}
