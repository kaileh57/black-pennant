//! Unit `hm-parts-bodies`. Small-body models: missiles, torpedoes, mines, depth charges (Act 3), drones,
//! enemy strike aircraft and the officer-flown helicopter (Act 2), RHIBs and towed decoys.
//! DESIGN.md L586-597 (munitions and small bodies; bounding radius missile 0.5 m, drone 1 m, aircraft 5 m),
//! L1949 (Layer A MultiMesh per type), L1966 (missile trail starts at the tail).
//!
//! Dimensions derive from the munition row: length from `radius_m` and kind, so a body's mesh fits its hit
//! sphere. Drone rotors and helicopter rotors are `Rotor` pieces with `Joint::Spin`. Missiles carry an
//! `Exhaust` anchor at the tail for the trail. Body-local frame: origin at the centre, nose +X, up +Y.
//!
//! # Fitting the hit sphere
//! Each shape is drawn at its own proportions scaled by `radius_m`, then the whole model (meshes, pivots,
//! anchors) is scaled down uniformly until every rest-pose vertex, and every point a `Spin` piece sweeps, lies
//! within 0.98 `radius_m` of the origin. What the player sees is never larger than what can be hit (L597).
//! A non-finite or non-positive radius takes the generator's typical size (missile 0.5 m, drone 1 m, aircraft
//! 5 m, from L597; the rest this unit's choice); radii clamp to 0.05..=50 m.
//!
//! # Shape notes
//! The generator picks the shape; the munition row only sizes it, with small proportion changes by kind
//! (point-defence and surface-to-air missiles are slimmer than anti-ship missiles) and seeker (an infrared
//! seeker shows a glass nose, active radar a pale radome, none a metal cap). Missiles and torpedoes stay under
//! 200 triangles because salvos draw many at once. Torpedoes and fixed-wing drones carry a `Propeller` piece
//! (`Joint::Spin` about +X); quadcopters four `Rotor` pieces about +Y; the helicopter a main rotor about +Y and
//! a tail rotor about +Z. The strike aircraft and helicopter carry an `Exhaust` anchor, torpedoes and RHIBs a
//! `Wash` anchor at the propulsor. Aircraft, drones, helicopters and RHIB hulls wear paint (paint mask 1);
//! missiles, torpedoes, mines, charges and decoys are fixed tones.
use super::{AnchorKind, BodyGenerator, Joint, PartModel, PieceRole, Tone};
use crate::prim::kit::{builder, disc_x, ring_x, set_tone, Kit};
use crate::prim::{MeshBuilder, Xform};
use crate::types::{Aabb, MeshData};
use content::part::weapons::{MunitionDef, MunitionKind, SeekerKind};

const DEG: f32 = core::f32::consts::PI / 180.0;
/// Fraction of the hit radius the drawn model may fill.
const FILL: f32 = 0.98;

/// Build a small-body model. Unit: `hm-parts-bodies`.
pub fn build(g: BodyGenerator, def: &MunitionDef) -> PartModel {
    let r = body_radius(g, def.radius_m);
    let model = match g {
        BodyGenerator::Missile => missile(r, def.kind, def.seeker),
        BodyGenerator::Torpedo => torpedo(r),
        BodyGenerator::Mine => mine(r),
        BodyGenerator::DepthCharge => depth_charge(r),
        BodyGenerator::Drone => drone(r),
        BodyGenerator::DroneRotor => drone_rotor(r),
        BodyGenerator::Aircraft => aircraft(r),
        BodyGenerator::Helicopter => helicopter(r),
        BodyGenerator::Rhib => rhib_body(r),
        BodyGenerator::TowedDecoy => towed_decoy(r),
    };
    fit_to_sphere(model, r * FILL)
}

/// Typical hit radius per generator, metres.
fn default_radius(g: BodyGenerator) -> f32 {
    match g {
        BodyGenerator::Missile | BodyGenerator::Torpedo => 0.5,
        BodyGenerator::Mine | BodyGenerator::TowedDecoy | BodyGenerator::DroneRotor => 0.6,
        BodyGenerator::DepthCharge => 0.4,
        BodyGenerator::Drone => 1.0,
        BodyGenerator::Aircraft | BodyGenerator::Helicopter => 5.0,
        BodyGenerator::Rhib => 3.5,
    }
}

fn body_radius(g: BodyGenerator, radius_m: f64) -> f32 {
    if radius_m.is_finite() && radius_m > 0.0 { (radius_m as f32).clamp(0.05, 50.0) } else { default_radius(g) }
}

// ---------------------------------------------------------------------------------------------------------
// Fitting

/// Largest distance from the origin any rest-pose vertex of the model reaches, counting the full circle a
/// `Spin` piece sweeps about its axis.
pub(crate) fn reach(m: &PartModel) -> f32 {
    let mut best = 0.0f32;
    for (i, p) in m.pieces.iter().enumerate() {
        let o = Kit::offset(&m.pieces, i);
        let spin_axis: Option<[f32; 3]> = match p.joint { Joint::Spin { axis } => Some(crate::prim::normalize_or(axis, [0.0, 1.0, 0.0])), _ => None };
        for v in &p.mesh.positions {
            let d = match spin_axis {
                None => { let q = [v[0] + o[0], v[1] + o[1], v[2] + o[2]]; crate::prim::length(q) }
                Some(a) => {
                    // |o + v_a a + r u| is largest when u lies along o's part perpendicular to the axis.
                    let (va, oa) = (crate::prim::dot(*v, a), crate::prim::dot(o, a));
                    let r = crate::prim::length(crate::prim::sub(*v, crate::prim::mul(a, va)));
                    let q = crate::prim::length(crate::prim::sub(o, crate::prim::mul(a, oa)));
                    let (s, t) = (oa + va, q + r);
                    libm::sqrtf(s * s + t * t)
                }
            };
            if d.is_finite() { best = best.max(d); }
        }
    }
    best
}

/// Scale the whole model about the origin so its reach is at most `limit`.
fn fit_to_sphere(mut m: PartModel, limit: f32) -> PartModel {
    let r = reach(&m);
    if r.is_nan() || r <= limit || !limit.is_finite() || limit <= 0.0 { return m; }
    let s = limit / r;
    let sc = |p: &mut [f32; 3]| { p[0] *= s; p[1] *= s; p[2] *= s; };
    for p in &mut m.pieces {
        sc(&mut p.pivot);
        for v in &mut p.mesh.positions { sc(v); }
        if let Joint::Recoil { travel_m } = &mut p.joint { *travel_m *= s; }
    }
    for a in &mut m.anchors { sc(&mut a.pos); }
    if !m.aabb.is_empty() { sc(&mut m.aabb.min); sc(&mut m.aabb.max); } else { m.aabb = Aabb::default(); }
    m
}

// ---------------------------------------------------------------------------------------------------------
// Shared shapes

/// Loft a body of revolution along X from a nose profile: `(t, s)` pairs, t = 0 at the nose (x = `x_nose`)
/// to 1 at the tail (x = `x_nose - len`), s = radius fraction of `r`.
fn revolve_x(b: &mut MeshBuilder, x_nose: f32, len: f32, r: f32, profile: &[(f32, f32)], sides: usize, cap_tail: bool) {
    let rings: Vec<Vec<[f32; 3]>> = profile.iter().map(|&(t, s)| ring_x(x_nose - t * len, 0.0, 0.0, r * s, r * s, sides)).collect();
    b.loft(&rings, false, cap_tail);
}

/// A thin fin in its own XY plane (leading edge at `x_le`, root on the body at radius `r0`, span outward along
/// +Y), rolled about +X by `roll`.
#[allow(clippy::too_many_arguments)]
fn fin(b: &mut MeshBuilder, x_le: f32, root: f32, tip: f32, sweep: f32, r0: f32, span: f32, t: f32, roll: f32) {
    let outline = [[x_le, r0], [x_le - root, r0], [x_le - sweep - tip, r0 + span], [x_le - sweep, r0 + span]];
    b.with(Xform::rot_x(roll).then(&Xform::translation([0.0, 0.0, -t * 0.5])), |b| b.extrude_z(&outline, t));
}

/// A flat plate whose plan outline (x, z) is extruded down from `y_top` by `t` (wings, tailplanes, decks).
fn plate_xz(b: &mut MeshBuilder, outline: &[[f32; 2]], y_top: f32, t: f32) {
    // rot_x(90 deg) maps local (x, y, z) to (x, -z, y): the outline lands in XZ, the extrusion goes down.
    b.with(Xform::translation([0.0, y_top, 0.0]).then(&Xform::rot_x(90.0 * DEG)), |b| b.extrude_z(outline, t));
}

/// Mirror of `f` across the XY plane (port copy of a starboard shape).
fn mirrored_z<F: Fn(&mut MeshBuilder)>(b: &mut MeshBuilder, f: F) {
    f(b);
    b.with(Xform::scale([1.0, 1.0, -1.0]), |b| f(b));
}

/// A propeller hub along X centred on the piece origin with `blades` twisted paddle blades of radius `rad`.
pub(crate) fn prop_mesh(rad: f32, blades: usize, hub_r: f32, hub_len: f32, pitch_deg: f32, tone: Tone) -> MeshData {
    let mut b = builder(tone);
    b.frustum([hub_len * 0.5, 0.0, 0.0], [-hub_len * 0.5, 0.0, 0.0], hub_r * 0.6, hub_r, 6, true);
    let n = blades.clamp(2, 7);
    let chord = (rad * 0.45).min(hub_len * 1.6).max(hub_r);
    for k in 0..n {
        let roll = core::f32::consts::TAU * k as f32 / n as f32;
        // Blade: a flat paddle in the XY plane along +Y, pitched about its radial axis, faced both ways (4
        // triangles; salvos of torpedoes draw many of these).
        let outline = [[chord * 0.35, hub_r * 0.8, 0.0], [-chord * 0.35, hub_r * 0.8, 0.0], [-chord * 0.25, rad, 0.0], [chord * 0.2, rad, 0.0]];
        b.with(Xform::rot_x(roll).then(&Xform::rot_y(pitch_deg * DEG)), |b| { b.face(&outline, [0.0, 0.0, 1.0]); b.face(&outline, [0.0, 0.0, -1.0]); });
    }
    b.finish()
}

/// A rotor disc about +Y centred on the piece origin: a hub and `blades` flat blades of radius `rad`.
fn rotor_mesh(rad: f32, blades: usize, chord: f32, tone: Tone) -> MeshData {
    let mut b = builder(Tone::Black);
    let hub = (rad * 0.08).max(chord * 0.6);
    b.prism_y(0.0, 0.0, hub, -hub * 0.4, hub * 0.4, 6, true);
    set_tone(&mut b, tone);
    let n = blades.clamp(2, 6);
    let t = (chord * 0.12).max(0.004);
    for k in 0..n {
        let a = core::f32::consts::TAU * k as f32 / n as f32;
        b.with(Xform::rot_y(a), |b| b.cuboid([hub * 0.6, -t * 0.5, -chord * 0.5], [rad, t * 0.5, chord * 0.5]));
    }
    b.finish()
}

/// Rotate a finished mesh in place (rotation only, so normals stay unit and winding is kept).
fn rotate_mesh(m: &mut MeshData, x: Xform) {
    for p in &mut m.positions { *p = x.apply(*p); }
    for n in &mut m.normals { *n = x.apply(*n); }
}

// ---------------------------------------------------------------------------------------------------------
// Missiles, torpedoes, mines, charges, decoys

fn missile(r: f32, kind: MunitionKind, seeker: SeekerKind) -> PartModel {
    // Fineness by role: slim interceptors, stockier sea-skimmers.
    let ld = match kind { MunitionKind::Sam => 15.0, MunitionKind::PdMissile => 17.0, _ => 11.0 };
    let len = 1.9 * r;
    let rr = len / ld * 0.5;
    let x_n = len * 0.5;
    let mut b = builder(Tone::Pale);
    // Nose: glass for infrared, pale radome for radar, metal cap otherwise.
    let nose_tone = match seeker { SeekerKind::Infrared => Tone::Glass, SeekerKind::ActiveRadar | SeekerKind::ActiveAcoustic => Tone::Pale, SeekerKind::None => Tone::Metal };
    set_tone(&mut b, nose_tone);
    let ogive: &[(f32, f32)] = if seeker == SeekerKind::Infrared { &[(0.0, 0.0), (0.012, 0.6), (0.035, 1.0)] } else { &[(0.0, 0.0), (0.05, 0.5), (0.11, 0.85), (0.16, 1.0)] };
    revolve_x(&mut b, x_n, len, rr, ogive, 8, false);
    let t0 = ogive[ogive.len() - 1].0;
    set_tone(&mut b, Tone::Pale);
    revolve_x(&mut b, x_n, len, rr, &[(t0, 1.0), (0.93, 1.0), (1.0, 0.8)], 8, false);
    set_tone(&mut b, Tone::Black);
    // Nozzle disc facing aft (rot_y(180 deg) turns disc_x's +X face round).
    b.with(Xform::rot_y(180.0 * DEG), |b| disc_x(b, [-(x_n - len) + 0.0005, 0.0, 0.0], rr * 0.8, 8));
    // Cruciform tail fins (x form); anti-ship missiles add mid wings, interceptors long strakes.
    set_tone(&mut b, Tone::Pale);
    let t = (rr * 0.12).max(0.003);
    let tail_x = x_n - len * 0.86;
    for k in 0..4 {
        let roll = (45.0 + 90.0 * k as f32) * DEG;
        fin(&mut b, tail_x, len * 0.12, len * 0.05, len * 0.06, rr * 0.9, rr * 2.2, t, roll);
        match kind {
            MunitionKind::AntiShipMissile => fin(&mut b, x_n - len * 0.42, len * 0.1, len * 0.05, len * 0.05, rr * 0.9, rr * 1.6, t, roll),
            _ => fin(&mut b, x_n - len * 0.3, len * 0.3, len * 0.26, len * 0.02, rr * 0.9, rr * 0.5, t, roll),
        }
    }
    let mut kit = Kit::new();
    let body = kit.piece(PieceRole::Body, None, [0.0; 3], Joint::Fixed, b.finish());
    kit.anchor(AnchorKind::Exhaust, body, [x_n - len, 0.0, 0.0], [-1.0, 0.0, 0.0]);
    kit.finish()
}

fn torpedo(r: f32) -> PartModel {
    let len = 1.85 * r;
    let rr = len / 12.0 * 0.5;
    let x_n = len * 0.5;
    let mut b = builder(Tone::Metal);
    revolve_x(&mut b, x_n, len, rr, &[(0.0, 0.0), (0.03, 0.75), (0.1, 1.0), (0.78, 1.0), (0.94, 0.5), (0.97, 0.35)], 8, true);
    // Black seeker window band near the nose.
    set_tone(&mut b, Tone::Black);
    revolve_x(&mut b, x_n, len, rr * 1.01, &[(0.12, 1.0), (0.15, 1.0)], 8, false);
    set_tone(&mut b, Tone::Metal);
    let t = (rr * 0.1).max(0.003);
    for k in 0..4 {
        fin(&mut b, x_n - len * 0.82, len * 0.1, len * 0.06, len * 0.03, rr * 0.55, rr * 0.75, t, 90.0 * k as f32 * DEG);
    }
    let mut kit = Kit::new();
    let body = kit.piece(PieceRole::Body, None, [0.0; 3], Joint::Fixed, b.finish());
    let px = x_n - len * 0.985;
    kit.piece(PieceRole::Propeller, Some(body), [px, 0.0, 0.0], Joint::Spin { axis: [1.0, 0.0, 0.0] }, prop_mesh(rr * 0.85, 5, rr * 0.3, len * 0.03, 30.0, Tone::Black));
    kit.anchor(AnchorKind::Wash, body, [px - len * 0.02, 0.0, 0.0], [-1.0, 0.0, 0.0]);
    kit.finish()
}

/// Upper-hemisphere horn directions of a contact mine: one on top and five round it.
fn horn_dirs() -> [[f32; 3]; 6] {
    let mut d = [[0.0, 1.0, 0.0]; 6];
    for (k, item) in d.iter_mut().enumerate().skip(1) {
        let a = core::f32::consts::TAU * (k - 1) as f32 / 5.0;
        let (e, s) = (libm::cosf(50.0 * DEG), libm::sinf(50.0 * DEG));
        *item = [s * libm::cosf(a), e, s * libm::sinf(a)];
    }
    d
}

fn mine(r: f32) -> PartModel {
    let rs = r * 0.68;
    let mut b = builder(Tone::Black);
    b.icosphere([0.0; 3], rs, 1);
    // Horns: short metal studs with dark caps.
    for d in horn_dirs() {
        let a = [d[0] * rs * 0.9, d[1] * rs * 0.9, d[2] * rs * 0.9];
        let tip = [d[0] * r * 0.93, d[1] * r * 0.93, d[2] * r * 0.93];
        set_tone(&mut b, Tone::Metal);
        b.frustum(a, tip, rs * 0.1, rs * 0.06, 6, true);
    }
    // Equator band and mooring lug.
    set_tone(&mut b, Tone::Metal);
    b.prism_y(0.0, 0.0, rs * 1.01, -rs * 0.06, rs * 0.06, 12, false);
    b.cuboid([-rs * 0.12, -rs * 1.12, -rs * 0.05], [rs * 0.12, -rs * 0.92, rs * 0.05]);
    let mut kit = Kit::new();
    kit.piece(PieceRole::Body, None, [0.0; 3], Joint::Fixed, b.finish());
    kit.finish()
}

fn depth_charge(r: f32) -> PartModel {
    // A drum lying across the rack (axis along Z, so it rolls off over the stern): length 1.3 x diameter.
    let rd = r * 0.55;
    let half = rd * 1.3;
    let mut b = builder(Tone::Black);
    b.frustum([0.0, 0.0, -half], [0.0, 0.0, half], rd, rd, 10, true);
    set_tone(&mut b, Tone::Metal);
    for z in [-half * 0.7, half * 0.7] { b.frustum([0.0, 0.0, z - half * 0.06], [0.0, 0.0, z + half * 0.06], rd * 1.05, rd * 1.05, 10, false); }
    // Pistol boss on one end.
    b.frustum([0.0, 0.0, half], [0.0, 0.0, half + rd * 0.12], rd * 0.25, rd * 0.22, 6, true);
    let mut kit = Kit::new();
    kit.piece(PieceRole::Body, None, [0.0; 3], Joint::Fixed, b.finish());
    kit.finish()
}

fn towed_decoy(r: f32) -> PartModel {
    let len = 1.9 * r;
    let rr = len / 6.5 * 0.5;
    let x_n = len * 0.5;
    let mut b = builder(Tone::Pale);
    revolve_x(&mut b, x_n, len, rr, &[(0.0, 0.0), (0.04, 0.7), (0.12, 1.0), (0.8, 1.0), (0.95, 0.6)], 8, true);
    // Ring tail: a short open duct around four stub fins.
    set_tone(&mut b, Tone::Black);
    let xr = x_n - len * 0.9;
    b.frustum([xr + len * 0.08, 0.0, 0.0], [xr - len * 0.04, 0.0, 0.0], rr * 1.5, rr * 1.5, 8, false);
    // Inner wall facing the axis, so the duct reads from behind too.
    let (fa, fb) = (ring_x(xr + len * 0.08, 0.0, 0.0, rr * 1.5, rr * 1.5, 8), ring_x(xr - len * 0.04, 0.0, 0.0, rr * 1.5, rr * 1.5, 8));
    for i in 0..8 {
        let j = (i + 1) % 8;
        let mid = [0.0, (fa[i][1] + fa[j][1]) * 0.5, (fa[i][2] + fa[j][2]) * 0.5];
        b.face(&[fa[i], fa[j], fb[j], fb[i]], [0.0, -mid[1], -mid[2]]);
    }
    for k in 0..4 { fin(&mut b, xr + len * 0.08, len * 0.12, len * 0.12, 0.0, rr * 0.7, rr * 0.75, (rr * 0.08).max(0.003), (45.0 + 90.0 * k as f32) * DEG); }
    // Tow lug on top near the nose.
    set_tone(&mut b, Tone::Metal);
    b.cuboid([x_n - len * 0.2, rr * 0.9, -rr * 0.1], [x_n - len * 0.12, rr * 1.25, rr * 0.1]);
    let mut kit = Kit::new();
    kit.piece(PieceRole::Body, None, [0.0; 3], Joint::Fixed, b.finish());
    kit.finish()
}

// ---------------------------------------------------------------------------------------------------------
// Drones and aircraft

fn drone(r: f32) -> PartModel {
    // Pusher fixed-wing drone: high straight wing, inverted-V tail, nose camera ball.
    let span = 1.9 * r;
    let len = span * 0.62;
    let fr = len * 0.07;
    let x_n = len * 0.5;
    let mut b = builder(Tone::Paint);
    revolve_x(&mut b, x_n, len, fr, &[(0.0, 0.2), (0.06, 0.8), (0.15, 1.0), (0.6, 1.0), (0.92, 0.45)], 8, true);
    let chord = span * 0.09;
    let xw = x_n - len * 0.32;
    mirrored_z(&mut b, |b| plate_xz(b, &[[xw, 0.0], [xw - chord, 0.0], [xw - chord * 0.85, span * 0.5], [xw - chord * 0.15, span * 0.5]], fr * 1.15, chord * 0.1));
    // Inverted V tail.
    let xt = x_n - len * 0.82;
    for roll in [180.0 + 50.0, 180.0 - 50.0] {
        fin(&mut b, xt, len * 0.14, len * 0.08, len * 0.05, fr * 0.6, span * 0.13, chord * 0.08, roll * DEG);
    }
    set_tone(&mut b, Tone::Glass);
    b.icosphere([x_n - len * 0.1, -fr * 0.9, 0.0], fr * 0.6, 0);
    let mut kit = Kit::new();
    let body = kit.piece(PieceRole::Body, None, [0.0; 3], Joint::Fixed, b.finish());
    let px = x_n - len * 0.95;
    kit.piece(PieceRole::Propeller, Some(body), [px, 0.0, 0.0], Joint::Spin { axis: [1.0, 0.0, 0.0] }, prop_mesh(span * 0.11, 2, fr * 0.35, fr * 0.6, 20.0, Tone::Black));
    kit.finish()
}

fn drone_rotor(r: f32) -> PartModel {
    // Quadcopter: centre pod, four diagonal arms with motor pods, four rotors, chin camera ball.
    let arm = r * 0.55;
    let rot_r = r * 0.36;
    let pod = r * 0.18;
    let mut b = builder(Tone::Paint);
    b.cuboid([-pod, -pod * 0.45, -pod * 0.7], [pod, pod * 0.45, pod * 0.7]);
    let d = arm * core::f32::consts::FRAC_1_SQRT_2;
    let corners = [[d, d], [d, -d], [-d, d], [-d, -d]];
    for c in corners {
        b.beam([0.0, 0.0, 0.0], [c[0], 0.0, c[1]], pod * 0.12);
        set_tone(&mut b, Tone::Black);
        b.prism_y(c[0], c[1], pod * 0.22, -pod * 0.2, pod * 0.35, 6, true);
        set_tone(&mut b, Tone::Paint);
    }
    set_tone(&mut b, Tone::Glass);
    b.icosphere([pod * 0.6, -pod * 0.6, 0.0], pod * 0.35, 0);
    // Landing legs.
    set_tone(&mut b, Tone::Black);
    for z in [-pod * 0.6, pod * 0.6] { b.beam([-pod * 0.5, -pod * 0.45, z], [-pod * 0.5, -pod * 1.1, z * 1.4], pod * 0.05); b.beam([pod * 0.5, -pod * 0.45, z], [pod * 0.5, -pod * 1.1, z * 1.4], pod * 0.05); }
    let mut kit = Kit::new();
    let body = kit.piece(PieceRole::Body, None, [0.0; 3], Joint::Fixed, b.finish());
    for c in corners {
        kit.piece(PieceRole::Rotor, Some(body), [c[0], pod * 0.42, c[1]], Joint::Spin { axis: [0.0, 1.0, 0.0] }, rotor_mesh(rot_r, 2, rot_r * 0.14, Tone::Black));
    }
    kit.finish()
}

fn aircraft(r: f32) -> PartModel {
    // Combine strike aircraft: single-engine, shoulder delta with canards, twin canted fins, two pale
    // missiles on wing pylons. Length 1.9 r, span 0.62 of length.
    let len = 1.9 * r;
    let span = len * 0.62;
    let fr = len * 0.055;
    let x_n = len * 0.5;
    let mut b = builder(Tone::Paint);
    let prof = [(0.0, 0.0), (0.08, 0.55), (0.2, 0.95), (0.3, 1.0), (0.85, 1.0), (0.97, 0.8)];
    let rings: Vec<Vec<[f32; 3]>> = prof.iter().map(|&(t, s)| ring_x(x_n - t * len, 0.0, 0.0, fr * s * 0.85, fr * s * 1.1, 8)).collect();
    b.loft(&rings, false, false);
    // Engine nozzle.
    set_tone(&mut b, Tone::Black);
    b.frustum([x_n - len * 0.97, 0.0, 0.0], [x_n - len, 0.0, 0.0], fr * 0.75, fr * 0.65, 8, true);
    // Canopy.
    set_tone(&mut b, Tone::Glass);
    let cprof = [(0.14, 0.0), (0.2, 0.7), (0.28, 0.85), (0.38, 0.0)];
    let crings: Vec<Vec<[f32; 3]>> = cprof.iter().map(|&(t, s)| ring_x(x_n - t * len, fr * 0.75, 0.0, fr * s * 0.6, fr * s * 0.55, 6)).collect();
    b.loft(&crings, false, false);
    // Delta wing, canards, tail fins.
    set_tone(&mut b, Tone::Paint);
    let xl = x_n - len * 0.42;
    let root = len * 0.48;
    let wing = [[xl, 0.0], [xl - root, 0.0], [xl - root, span * 0.5], [xl - root * 0.82, span * 0.5]];
    mirrored_z(&mut b, |b| plate_xz(b, &wing, fr * 0.2, fr * 0.16));
    let xc = x_n - len * 0.25;
    mirrored_z(&mut b, |b| plate_xz(b, &[[xc, fr * 0.8], [xc - len * 0.08, fr * 0.8], [xc - len * 0.1, span * 0.2], [xc - len * 0.07, span * 0.2]], fr * 0.35, fr * 0.08));
    for roll in [-15.0f32, 15.0] {
        fin(&mut b, x_n - len * 0.74, len * 0.2, len * 0.08, len * 0.1, fr * 0.7, span * 0.2, fr * 0.1, roll * DEG);
    }
    // Pylons and stores.
    for z in [-span * 0.26, span * 0.26] {
        set_tone(&mut b, Tone::Metal);
        b.cuboid([xl - root * 0.65, -fr * 0.45, z - fr * 0.06], [xl - root * 0.35, fr * 0.05, z + fr * 0.06]);
        set_tone(&mut b, Tone::Pale);
        b.frustum([xl - root * 0.15, -fr * 0.7, z], [xl - root * 0.85, -fr * 0.7, z], fr * 0.18, fr * 0.22, 6, true);
    }
    let mut kit = Kit::new();
    let body = kit.piece(PieceRole::Body, None, [0.0; 3], Joint::Fixed, b.finish());
    kit.anchor(AnchorKind::Exhaust, body, [x_n - len, 0.0, 0.0], [-1.0, 0.0, 0.0]);
    kit.finish()
}

fn helicopter(r: f32) -> PartModel {
    // Light naval helicopter: pod and boom, four-blade main rotor, three-blade tail rotor on the port side of
    // the fin, skids. The rotor mast stands over the centre of mass (the origin); the fuselage runs from 0.25 of
    // its length ahead of the mast to 0.72 behind, and the rotor disc is 1.04 fuselage lengths across (Lynx
    // proportions), so the tail tip sets the reach: fuselage length 1.36 r.
    let len = 1.36 * r;
    let pod_len = len * 0.5;
    let x_n = pod_len * 0.5;
    let ph = len * 0.2;
    let mut b = builder(Tone::Paint);
    let prof = [(0.0, 0.25), (0.08, 0.75), (0.25, 1.0), (0.7, 1.0), (1.0, 0.45)];
    let rings: Vec<Vec<[f32; 3]>> = prof.iter().map(|&(t, s)| ring_x(x_n - t * pod_len, 0.0, 0.0, ph * s * 0.55, ph * s * 0.42, 8)).collect();
    b.loft(&rings, true, false);
    // Tail boom, fin and stabiliser.
    let xb = x_n - pod_len * 0.95;
    let xt = x_n - len * 0.97;
    b.frustum([xb, ph * 0.12, 0.0], [xt, ph * 0.22, 0.0], ph * 0.2, ph * 0.08, 6, true);
    fin(&mut b, xt + len * 0.08, len * 0.09, len * 0.06, len * 0.03, ph * 0.2, ph * 0.5, ph * 0.05, 0.0);
    mirrored_z(&mut b, |b| plate_xz(b, &[[xt + len * 0.16, 0.0], [xt + len * 0.11, 0.0], [xt + len * 0.115, ph * 0.5], [xt + len * 0.15, ph * 0.5]], ph * 0.22, ph * 0.04));
    // Engine fairing and rotor mast.
    let mast_x = x_n - pod_len * 0.5;
    let mast_top = ph * 0.75;
    b.cuboid([mast_x - pod_len * 0.25, ph * 0.3, -ph * 0.2], [mast_x + pod_len * 0.15, ph * 0.52, ph * 0.2]);
    set_tone(&mut b, Tone::Black);
    b.frustum([mast_x, ph * 0.5, 0.0], [mast_x, mast_top, 0.0], ph * 0.07, ph * 0.05, 6, false);
    // Canopy glazing on the nose.
    set_tone(&mut b, Tone::Glass);
    let crings: Vec<Vec<[f32; 3]>> = [(0.04, 0.6), (0.12, 0.92), (0.24, 0.0)].iter().map(|&(t, s)| ring_x(x_n - t * pod_len, ph * 0.12, 0.0, ph * s * 0.48, ph * s * 0.44, 8)).collect();
    b.loft(&crings, true, false);
    // Skids.
    set_tone(&mut b, Tone::Black);
    for z in [-ph * 0.5, ph * 0.5] {
        b.beam([x_n - pod_len * 0.05, -ph * 0.72, z], [x_n - pod_len * 0.85, -ph * 0.72, z], ph * 0.03);
        for xs in [x_n - pod_len * 0.25, x_n - pod_len * 0.65] { b.beam([xs, -ph * 0.3, z * 0.6], [xs, -ph * 0.72, z], ph * 0.025); }
    }
    let mut kit = Kit::new();
    let body = kit.piece(PieceRole::Body, None, [0.0; 3], Joint::Fixed, b.finish());
    kit.piece(PieceRole::Rotor, Some(body), [mast_x, mast_top, 0.0], Joint::Spin { axis: [0.0, 1.0, 0.0] }, rotor_mesh(len * 0.52, 4, len * 0.03, Tone::Black));
    // Tail rotor in the XY plane, spinning about +Z, on the port (-Z) face of the fin.
    let mut tail = rotor_mesh(len * 0.075, 3, len * 0.012, Tone::Black);
    rotate_mesh(&mut tail, Xform::rot_x(90.0 * DEG));
    kit.piece(PieceRole::Rotor, Some(body), [xt + len * 0.05, ph * 0.55, -ph * 0.08], Joint::Spin { axis: [0.0, 0.0, 1.0] }, tail);
    kit.anchor(AnchorKind::Exhaust, body, [mast_x - pod_len * 0.25, ph * 0.45, 0.0], [-1.0, 0.4, 0.0]);
    kit.finish()
}

// ---------------------------------------------------------------------------------------------------------
// RHIB (also drawn stowed on davits)

/// A RHIB of overall length `len` centred on the origin, nose +X, the tube line at y = 0: deep-V painted hull,
/// black tube collar, console with glass windscreen and a black outboard. Shared with `fit.davit`.
pub(crate) fn rhib_mesh(len: f32) -> MeshData {
    let l = len.abs().max(0.5);
    let w = l * 0.19; // half beam at the tube
    let d = l * 0.09; // keel depth below the tube line
    let mut b = builder(Tone::Paint);
    // Hull cross-section: keel, chines and gunwales (5 corners), tapering into the bow.
    let sec = |x: f32, s: f32, lift: f32| -> Vec<[f32; 3]> {
        let (ww, dd) = (w * s, d * s);
        vec![[x, -dd + lift, 0.0], [x, -dd * 0.35 + lift, ww * 0.85], [x, lift * 0.5, ww * 0.8], [x, lift * 0.5, -ww * 0.8], [x, -dd * 0.35 + lift, -ww * 0.85]]
    };
    let rings = vec![sec(l * 0.5, 0.02, d * 0.8), sec(l * 0.4, 0.55, d * 0.35), sec(l * 0.2, 0.95, 0.0), sec(-l * 0.5, 1.0, 0.0)];
    b.loft(&rings, false, true);
    // Tube collar: segments round the gunwale, converging at the bow.
    set_tone(&mut b, Tone::Black);
    let tr = l * 0.045;
    let path = [[-l * 0.5, w * 0.92], [l * 0.15, w * 0.92], [l * 0.35, w * 0.7], [l * 0.48, w * 0.15]];
    for side in [1.0f32, -1.0] {
        for s in path.windows(2) {
            b.frustum([s[0][0], 0.0, s[0][1] * side], [s[1][0], 0.0, s[1][1] * side], tr, tr, 6, true);
        }
    }
    // Console and windscreen.
    set_tone(&mut b, Tone::Paint);
    b.cuboid([-l * 0.05, -d * 0.2, -w * 0.3], [l * 0.08, l * 0.12, w * 0.3]);
    set_tone(&mut b, Tone::Glass);
    b.face(&[[l * 0.08, l * 0.12, -w * 0.3], [l * 0.08, l * 0.12, w * 0.3], [l * 0.04, l * 0.19, w * 0.28], [l * 0.04, l * 0.19, -w * 0.28]], [1.0, 0.6, 0.0]);
    // Outboard: cowl above the transom, leg into the water.
    set_tone(&mut b, Tone::Black);
    b.cuboid([-l * 0.56, -d * 0.1, -w * 0.15], [-l * 0.47, l * 0.07, w * 0.15]);
    b.cuboid([-l * 0.54, -d * 1.5, -w * 0.05], [-l * 0.5, -d * 0.1, w * 0.05]);
    b.finish()
}

fn rhib_body(r: f32) -> PartModel {
    let len = 1.9 * r;
    let mut kit = Kit::new();
    let body = kit.piece(PieceRole::Body, None, [0.0; 3], Joint::Fixed, rhib_mesh(len));
    kit.anchor(AnchorKind::Wash, body, [-len * 0.52, -len * 0.09 * 1.4, 0.0], [-1.0, 0.0, 0.0]);
    kit.finish()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::prim::tests::check_consistent;
    use content::enums::{Domain, Phase};
    use content::ids::{IconId, ModelId, MunitionId, TextKey};
    use content::part::weapons::Fuse;

    fn def(kind: MunitionKind, radius_m: f64, seeker: SeekerKind) -> MunitionDef {
        MunitionDef {
            id: MunitionId::default(), name: TextKey::default(), kind, phase: Phase::default(), domain: Domain::Air,
            speed_mps: 270.0, range_m: 30_000.0, max_accel: 100.0, cruise_alt_m: 5.0, fuse: Fuse::Contact, he_b: 8, he_r_cells: 4,
            seeker, seeker_r_ref_m: 12_000.0, seeker_fov_half_deg: 30.0, pn_n: 3.0, wire_guided: false, radius_m, hp: 1, rcs_m2: 0.1, noise: 0.0,
            mass_kg: 500.0, cost: 1, sensors: vec![], payload: vec![], endurance_s: 0.0, model: ModelId::default(), icon: IconId::default(),
        }
    }

    fn check(m: &PartModel, r: f32) {
        assert!(!m.pieces.is_empty());
        for (i, p) in m.pieces.iter().enumerate() {
            if let Some(q) = p.parent { assert!((q as usize) < i); } else { assert_eq!(i, 0); }
            assert!(!p.mesh.is_empty(), "empty piece {i}");
            check_consistent(&p.mesh);
        }
        for a in &m.anchors { assert!((a.piece as usize) < m.pieces.len()); assert!(a.pos.iter().all(|v| v.is_finite())); }
        let reach = reach(m);
        assert!(reach <= r * FILL * 1.0001, "reach {reach} > radius {r}");
        assert!(reach >= r * 0.6, "reach {reach} too small for radius {r}");
    }

    fn kind_for(g: BodyGenerator) -> MunitionKind {
        match g {
            BodyGenerator::Missile => MunitionKind::AntiShipMissile, BodyGenerator::Torpedo => MunitionKind::Torpedo,
            BodyGenerator::Mine => MunitionKind::Mine, BodyGenerator::DepthCharge => MunitionKind::DepthCharge,
            BodyGenerator::Drone | BodyGenerator::DroneRotor => MunitionKind::Drone, BodyGenerator::Aircraft => MunitionKind::Aircraft,
            BodyGenerator::Helicopter => MunitionKind::Helicopter, BodyGenerator::Rhib => MunitionKind::Rhib, BodyGenerator::TowedDecoy => MunitionKind::Torpedo,
        }
    }

    #[test]
    fn every_body_fits_its_hit_sphere() {
        for g in BodyGenerator::ALL {
            for r in [0.5, 1.0, 5.0, 3.5] {
                let m = build(g, &def(kind_for(g), r, SeekerKind::ActiveRadar));
                check(&m, r as f32);
            }
        }
    }

    #[test]
    fn junk_radii_take_typical_sizes() {
        for g in BodyGenerator::ALL {
            for r in [f64::NAN, 0.0, -2.0, f64::INFINITY] {
                check(&build(g, &def(kind_for(g), r, SeekerKind::None)), default_radius(g));
            }
            check(&build(g, &def(kind_for(g), 1e9, SeekerKind::None)), 50.0);
        }
    }

    #[test]
    fn missiles_trail_from_the_tail_and_stay_cheap() {
        for (kind, seeker) in [(MunitionKind::AntiShipMissile, SeekerKind::ActiveRadar), (MunitionKind::Sam, SeekerKind::ActiveRadar), (MunitionKind::PdMissile, SeekerKind::Infrared)] {
            let m = build(BodyGenerator::Missile, &def(kind, 0.5, seeker));
            let ex: Vec<_> = m.anchors.iter().filter(|a| a.kind == AnchorKind::Exhaust).collect();
            assert_eq!(ex.len(), 1);
            assert_eq!(ex[0].dir, [-1.0, 0.0, 0.0]);
            assert!((ex[0].pos[0] - m.aabb.min[0]).abs() < 0.02, "exhaust at {:?}, tail at {}", ex[0].pos, m.aabb.min[0]);
            let tris: usize = m.pieces.iter().map(|p| p.mesh.triangle_count()).sum();
            assert!(tris < 200, "{kind:?} {tris} triangles");
            // Long axis is X.
            assert!(m.aabb.max[0] - m.aabb.min[0] > 4.0 * (m.aabb.max[1] - m.aabb.min[1]));
        }
        // Infrared seekers show glass.
        let ir = build(BodyGenerator::Missile, &def(MunitionKind::PdMissile, 0.5, SeekerKind::Infrared));
        assert!(ir.pieces[0].mesh.custom0.iter().any(|c| c[1] == 1.0));
        let tris: usize = build(BodyGenerator::Torpedo, &def(MunitionKind::Torpedo, 0.5, SeekerKind::ActiveAcoustic)).pieces.iter().map(|p| p.mesh.triangle_count()).sum();
        assert!(tris < 200, "torpedo {tris}");
    }

    #[test]
    fn rotors_and_propellers_spin() {
        let spins = |g: BodyGenerator| -> Vec<[f32; 3]> {
            build(g, &def(kind_for(g), default_radius(g) as f64, SeekerKind::None)).pieces.iter().filter_map(|p| match p.joint { Joint::Spin { axis } => Some(axis), _ => None }).collect()
        };
        assert_eq!(spins(BodyGenerator::DroneRotor), vec![[0.0, 1.0, 0.0]; 4]);
        assert_eq!(spins(BodyGenerator::Helicopter), vec![[0.0, 1.0, 0.0], [0.0, 0.0, 1.0]]);
        assert_eq!(spins(BodyGenerator::Torpedo), vec![[1.0, 0.0, 0.0]]);
        assert_eq!(spins(BodyGenerator::Drone), vec![[1.0, 0.0, 0.0]]);
        let h = build(BodyGenerator::Helicopter, &def(MunitionKind::Helicopter, 5.0, SeekerKind::None));
        assert!(h.pieces.iter().filter(|p| p.role == PieceRole::Rotor).count() == 2);
        // Spun meshes are centred on their axis (no wobble).
        for g in [BodyGenerator::DroneRotor, BodyGenerator::Helicopter, BodyGenerator::Torpedo] {
            for p in build(g, &def(kind_for(g), 1.0, SeekerKind::None)).pieces.iter().filter(|p| matches!(p.joint, Joint::Spin { .. })) {
                let bb = p.mesh.aabb();
                let Joint::Spin { axis } = p.joint else { unreachable!() };
                let n = p.mesh.positions.len() as f32;
                for k in 0..3 {
                    if axis[k] != 0.0 { continue; }
                    let mean = p.mesh.positions.iter().map(|v| v[k]).sum::<f32>() / n;
                    assert!(mean.abs() < 0.05 * (bb.max[k] - bb.min[k]) + 1e-4, "{g:?} off-axis mean {mean}");
                }
            }
        }
        assert!(build(BodyGenerator::Aircraft, &def(MunitionKind::Aircraft, 5.0, SeekerKind::None)).anchors.iter().any(|a| a.kind == AnchorKind::Exhaust));
        assert!(build(BodyGenerator::Rhib, &def(MunitionKind::Rhib, 3.5, SeekerKind::None)).anchors.iter().any(|a| a.kind == AnchorKind::Wash));
    }

    #[test]
    fn bodies_are_deterministic_and_silhouettes_differ() {
        let d = |g| def(kind_for(g), 1.0, SeekerKind::None);
        for g in BodyGenerator::ALL { assert_eq!(build(g, &d(g)), build(g, &d(g))); }
        // Proportions: aircraft span wide, helicopter disc wide, missile and torpedo slender.
        let ext = |g| { let m = build(g, &d(g)); let b = m.aabb; [b.max[0] - b.min[0], b.max[1] - b.min[1], b.max[2] - b.min[2]] };
        let a = ext(BodyGenerator::Aircraft);
        assert!(a[2] > 0.5 * a[0] && a[1] < 0.35 * a[0]);
        let t = ext(BodyGenerator::Torpedo);
        assert!(t[0] > 6.0 * t[1]);
        let mi = ext(BodyGenerator::Mine);
        assert!((mi[0] - mi[2]).abs() < 0.2 * mi[0]);
    }
}
