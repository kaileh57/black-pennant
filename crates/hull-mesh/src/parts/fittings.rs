//! Unit `hm-parts-fittings`. Funnels, propulsors, rudders, davits, cranes, heli deck (Act 2), drone bay, recovery
//! net, deck details (searchlight, railing, bollards, liferafts, vents, anchor, doors), internal equipment blocks
//! for the editor cutaway, and the fallback box.
//! DESIGN.md L1233 (a small set of details), L786-797 (davits, RHIB, recovery net, crane, camera), L551 (heli
//! deck, Act 2), L1969 (pump outlets for flooding arcs), L1941 (primer on exposed internals).
//!
//! Propellers are a `Propeller` piece with `Joint::Spin { axis: +X }` and a `Wash` anchor; rudders are an
//! `Arm` piece with `Joint::Train` (the client drives it from `VesselView.rudder`); funnels carry an `Exhaust`
//! anchor; searchlights a `Light` anchor and a `Lamp` lens. `fit.internal` fills the footprint with a chamfered
//! equipment block whose tone parameter picks machinery (0), tanks (1), magazine (2) or accommodation (3).
//! `fit.box` is the fallback for unknown generators: the footprint as one painted block.
//!
//! # Fitting the footprint
//! Parameter sizes are clamped so every model stays inside the footprint in X and Z. The exceptions are the
//! fittings whose job is to reach past the hull: the crane jib (overhangs like a barrel), the recovery net
//! (hangs over the outboard side) and the waterjet nozzle (stands aft of the transom). Underwater fittings
//! (propeller, waterjet intake, rudder) hang below y = 0 and use fixed tones only (paint mask 0), like the sonar
//! dome. Funnels, the searchlight and the davit arms keep their parameter heights even when the footprint is
//! shallower, as masts do.
//!
//! # Conventions this unit chose
//! - The outboard side of deck-edge fittings (davit, recovery net, anchor) is +Z at rotation 0; the part's
//!   rotation turns it to the side it is placed on.
//! - The waterjet nozzle is an `Arm` piece with `Joint::Train` like a rudder: waterjets steer by vectoring
//!   (`PropulsorDef`), so the client drives it from `VesselView.rudder`. Its `Wash` anchor rides on the nozzle.
//! - The crane, searchlight, doors and hatches have no replicated state, so their pieces are `Joint::Fixed`.
//!   Door and hatch panels are separate `Door` pieces pivoted on their hinge line, and the davit's boat is a
//!   separate `Body` piece, so the client can hide or swing them later without new meshes.
//! - Machinery blocks (`fit.internal` tone 0; pumps, engines, generators and batteries resolve here) carry a
//!   `PumpOutlet` anchor at the top centre pointing up (L1969). Anchors are per part slot, so the VFX unit draws
//!   pump arcs only for slots whose function is a pump, carried out to the nearest hull side.
//! - Internal blocks never wear ship paint: machinery is gunmetal, tanks primer, magazines deck-dark with a pale
//!   band, accommodation pale, so the four read apart by value in the cutaway (value before hue, L1779).
use super::{AnchorKind, Generator, Joint, PartModel, PieceRole, Tone};
use crate::parts::bodies::{prop_mesh, rhib_mesh};
use crate::prim::kit::{builder, disc_x, ellipse_ring, foot_m, footprint_block, sanitize, set_tone, vplate, Kit};
use crate::prim::{MeshBuilder, Xform};
use crate::types::MeshData;

const DEG: f32 = core::f32::consts::PI / 180.0;

/// Build `g` (a fittings-family generator) from resolved parameters. Unit: `hm-parts-fittings`.
pub fn build(g: Generator, params: &[f32], footprint: [u8; 3]) -> PartModel {
    let p = sanitize(g, params);
    let f = foot_m(footprint);
    match g {
        Generator::Funnel => funnel(p[0], p[1], p[2], p[3], p[4] >= 0.5, f),
        Generator::Propeller => propeller(p[0], p[1] as usize, f),
        Generator::Waterjet => waterjet(p[0], f),
        Generator::Rudder => rudder(p[0], p[1], f),
        Generator::Davit => davit(p[0], f),
        Generator::Crane => crane(p[0], f),
        Generator::HeliDeck => heli_deck(p[0], p[1], f),
        Generator::DroneBay => drone_bay(p[0] as usize, f),
        Generator::RecoveryNet => recovery_net(p[0], f),
        Generator::Searchlight => searchlight(p[0], f),
        Generator::Railing => railing(p[0], f),
        Generator::Bollard => bollard(p[0] as usize, f),
        Generator::Liferaft => liferaft(p[0] as usize, f),
        Generator::Vent => vent(p[0], f),
        Generator::Anchor => anchor(p[0], f),
        Generator::Door => door(p[0] >= 0.5, f),
        Generator::Internal => internal(p[0] as usize, f),
        _ => footprint_block(footprint),
    }
}

/// The model drawn when a part's generator is unknown or fails validation: a painted block filling the
/// footprint. Unit: `hm-parts-fittings`.
pub fn fallback_box(footprint: [u8; 3]) -> PartModel { footprint_block(footprint) }

fn single(role: PieceRole, mesh: MeshData) -> Kit {
    let mut kit = Kit::new();
    kit.piece(role, None, [0.0; 3], Joint::Fixed, mesh);
    kit
}

/// Box with every edge chamfered by `c` (clamped to a quarter of the smallest side): an octagonal loft.
fn chamfer_block(b: &mut MeshBuilder, min: [f32; 3], max: [f32; 3], c: f32) {
    let (x0, x1) = (min[0].min(max[0]), min[0].max(max[0]));
    let (y0, y1) = (min[1].min(max[1]), min[1].max(max[1]));
    let (z0, z1) = (min[2].min(max[2]), min[2].max(max[2]));
    let c = c.min((x1 - x0) * 0.25).min((y1 - y0) * 0.25).min((z1 - z0) * 0.25).max(0.0);
    if c <= 1e-4 { b.cuboid([x0, y0, z0], [x1, y1, z1]); return; }
    let ring = |y: f32, i: f32| {
        let (a0, a1, b0, b1) = (x0 + i, x1 - i, z0 + i, z1 - i);
        vec![[a0 + c, y, b0], [a1 - c, y, b0], [a1, y, b0 + c], [a1, y, b1 - c], [a1 - c, y, b1], [a0 + c, y, b1], [a0, y, b1 - c], [a0, y, b0 + c]]
    };
    b.loft(&[ring(y0, c), ring(y0 + c, 0.0), ring(y1 - c, 0.0), ring(y1, c)], true, true);
}

// ---------------------------------------------------------------------------------------------------------
// Funnel, propulsors, rudder

fn funnel(h: f32, w: f32, l: f32, rake_deg: f32, cap: bool, f: [f32; 3]) -> PartModel {
    let [fx, _fy, fz] = f;
    let w = w.min(fz).max(0.2);
    let l = l.min(fx).max(0.2);
    let h = h.max(0.3);
    let rake = rake_deg * DEG;
    // The top section is 0.85 of the base length and slides aft by the rake; fit both inside the footprint.
    let shift = (h * libm::tanf(rake)).min((fx - 0.925 * l).max(0.0));
    let fore = (l * 0.5).max(shift + 0.425 * l);
    let span = l * 0.5 + fore;
    let xb = (fx - span) * 0.5 + fore;
    let cz = fz * 0.5;
    let h_body = if cap { h * 0.9 } else { h };
    let st = core::f32::consts::PI / 8.0;
    let at = |t: f32, s: f32| ellipse_ring(xb - shift * t, h_body * t, cz, l * 0.5 * (1.0 - 0.15 * t) * s, w * 0.5 * (1.0 - 0.15 * t) * s, 8, st);
    let mut b = builder(Tone::Paint);
    b.loft(&[at(0.0, 1.0), at(0.86, 1.0)], false, false);
    // Soot band and the dark mouth.
    set_tone(&mut b, Tone::Black);
    b.loft(&[at(0.86, 1.0), at(1.0, 1.0)], false, false);
    // The mouth: a flat lip facing up, a shallow well whose wall faces the funnel axis, and a floor facing up,
    // so the top reads as an opening from above.
    let top = [xb - shift, h_body, cz];
    let (outer, inner) = (at(1.0, 1.0), at(1.0, 0.85));
    let depth = h_body * 0.03;
    let floor: Vec<[f32; 3]> = inner.iter().map(|p| [p[0], p[1] - depth, p[2]]).collect();
    for i in 0..outer.len() {
        let j = (i + 1) % outer.len();
        b.face(&[outer[i], outer[j], inner[j], inner[i]], [0.0, 1.0, 0.0]);
        let mid = [(inner[i][0] + inner[j][0]) * 0.5, 0.0, (inner[i][2] + inner[j][2]) * 0.5];
        b.face(&[inner[i], inner[j], floor[j], floor[i]], [top[0] - mid[0], 0.0, top[2] - mid[2]]);
    }
    b.face(&floor, [0.0, 1.0, 0.0]);
    if cap {
        // Raked smoke cap on two struts above the mouth.
        set_tone(&mut b, Tone::Paint);
        let yc = h_body + (h - h_body) * 0.7;
        b.strut([top[0], h_body, cz - w * 0.3], [top[0], yc, cz - w * 0.3], 0.02);
        b.strut([top[0], h_body, cz + w * 0.3], [top[0], yc, cz + w * 0.3], 0.02);
        let tilt = Xform::translation([top[0], yc, cz]).then(&Xform::rot_z(-(rake + 8.0 * DEG)));
        b.with(tilt, |b| b.cuboid([-l * 0.38, 0.0, -w * 0.38], [l * 0.38, (h - h_body) * 0.25, w * 0.38]));
    }
    let mut kit = single(PieceRole::Base, b.finish());
    kit.anchor(AnchorKind::Exhaust, 0, [top[0], h_body - h_body * 0.03, cz], [-libm::sinf(rake), libm::cosf(rake), 0.0]);
    kit.finish()
}

fn propeller(d: f32, blades: usize, f: [f32; 3]) -> PartModel {
    let [fx, _fy, fz] = f;
    let r = d.min(fz).max(0.3) * 0.5;
    let cz = fz * 0.5;
    let hub_r = r * 0.18;
    let hub_len = (r * 0.4).min(fx * 0.3);
    let hx = (hub_len * 0.6).max(fx * 0.25).min(fx * 0.5);
    let yh = -(r + 0.1);
    let mut b = builder(Tone::Metal);
    // Shaft from the hull bottom to the hub, and an A-bracket strut ahead of the hub.
    let sr = hub_r * 0.45;
    b.frustum([fx - sr * 1.5, -sr * 1.5, cz], [hx + hub_len * 0.5, yh, cz], sr, sr, 6, true);
    let xs = (hx + hub_len * 0.5 + r * 0.25).min(fx - 0.03);
    let hw = (r * 0.03).max(0.015);
    for side in [-1.0f32, 1.0] {
        let zt = cz + side * r * 0.35;
        b.beam([xs, -hw * 1.5, zt.clamp(0.03, fz - 0.03)], [xs, yh, cz + side * hub_r * 0.5], hw);
    }
    b.frustum([xs + 0.06, yh, cz], [xs - 0.06, yh, cz], hub_r * 0.7, hub_r * 0.7, 6, true);
    let mut kit = single(PieceRole::Base, b.finish());
    kit.piece(PieceRole::Propeller, Some(0), [hx, yh, cz], Joint::Spin { axis: [1.0, 0.0, 0.0] }, prop_mesh(r, blades, hub_r, hub_len, 28.0, Tone::Metal));
    kit.anchor(AnchorKind::Wash, 0, [hx - hub_len, yh, cz], [-1.0, 0.0, 0.0]);
    kit.finish()
}

fn waterjet(d: f32, f: [f32; 3]) -> PartModel {
    let [fx, fy, fz] = f;
    let d = d.min(fz * 0.9).min(fy * 0.9).max(0.2);
    let cz = fz * 0.5;
    let yc = d * 0.55;
    let mut b = builder(Tone::Metal);
    // Pump housing inside the hull, at the transom (x = 0) end of the footprint.
    let hl = (d * 2.0).min(fx * 0.9);
    chamfer_block(&mut b, [0.0, yc - d * 0.5, cz - d * 0.55], [hl * 0.45, yc + d * 0.5, cz + d * 0.55], d * 0.08);
    b.frustum([hl * 0.45, yc, cz], [hl, yc * 0.4, cz], d * 0.45, d * 0.35, 8, true);
    // Intake grille flush under the hull.
    set_tone(&mut b, Tone::Black);
    b.cuboid([hl * 0.55, -0.02, cz - d * 0.4], [hl, 0.0, cz + d * 0.4]);
    let mut kit = single(PieceRole::Base, b.finish());
    // Steering nozzle and reversing bucket aft of the transom.
    let mut n = builder(Tone::Metal);
    n.frustum([0.0, 0.0, 0.0], [-d * 0.6, 0.0, 0.0], d * 0.4, d * 0.28, 8, false);
    set_tone(&mut n, Tone::Black);
    n.with(Xform::rot_y(180.0 * DEG), |b| disc_x(b, [d * 0.6 - 0.001, 0.0, 0.0], d * 0.26, 8));
    set_tone(&mut n, Tone::Metal);
    n.with(Xform::translation([-d * 0.72, d * 0.36, 0.0]).then(&Xform::rot_z(-30.0 * DEG)), |b| b.cuboid([-d * 0.05, -d * 0.04, -d * 0.36], [d * 0.05, d * 0.04, d * 0.36]));
    let nozzle = kit.piece(PieceRole::Arm, Some(0), [0.0, yc, cz], Joint::Train, n.finish());
    kit.anchor(AnchorKind::Wash, nozzle, [-d * 0.65, 0.0, 0.0], [-1.0, 0.0, 0.0]);
    kit.finish()
}

/// Hexagonal foil section of chord `c` and thickness `t` at height `y`, leading edge at +X (`0.25 c` ahead of
/// the stock).
fn foil(y: f32, c: f32, t: f32) -> Vec<[f32; 3]> {
    vec![[0.25 * c, y, 0.0], [0.05 * c, y, t * 0.5], [-0.45 * c, y, t * 0.42], [-0.75 * c, y, 0.0], [-0.45 * c, y, -t * 0.42], [0.05 * c, y, -t * 0.5]]
}

fn rudder(h: f32, chord: f32, f: [f32; 3]) -> PartModel {
    let [fx, _fy, fz] = f;
    let c = chord.min(fx).max(0.2);
    let t = (c * 0.15).min(fz * 0.9);
    let px = 0.75 * c + (fx - c) * 0.5;
    let cz = fz * 0.5;
    let mut b = builder(Tone::Black);
    b.frustum([px, 0.0, cz], [px, -0.06, cz], (c * 0.08).min(fz * 0.45), (c * 0.08).min(fz * 0.45), 6, true);
    let mut kit = single(PieceRole::Base, b.finish());
    let mut a = builder(Tone::Black);
    a.frustum([0.0, -0.04, 0.0], [0.0, -0.12, 0.0], c * 0.05, c * 0.05, 6, false);
    a.loft(&[foil(-0.1, c, t), foil(-0.1 - h * 0.5, c * 0.95, t * 0.95), foil(-0.1 - h, c * 0.85, t * 0.8)], true, true);
    kit.piece(PieceRole::Arm, Some(0), [px, 0.0, cz], Joint::Train, a.finish());
    kit.finish()
}

// ---------------------------------------------------------------------------------------------------------
// Boats, cranes, decks

fn davit(boat: f32, f: [f32; 3]) -> PartModel {
    let [fx, _fy, fz] = f;
    // The boat sits in chocks inside the footprint; two arms stand on the outboard (+Z) edge and lean in over
    // it, with falls down to its lifting points.
    // The RHIB mesh spans -0.56 to +0.5 of its length (outboard aft of the transom) and 0.46 across.
    let bl = boat.min(fx * 0.88).min(fz / 0.46);
    let has_boat = bl >= 0.8;
    let bl = if has_boat { bl } else { (fx * 0.8).min(4.0) };
    let zb = fz * 0.5;
    let keel = bl * 0.135;
    let yb = keel + 0.12;
    let arm_h = (bl * 0.35).clamp(1.2, 3.0) + yb;
    let z_post = fz - 0.08;
    let z_head = zb;
    let half = (bl * 0.32).min(fx * 0.45);
    let mut b = builder(Tone::Paint);
    for x in [fx * 0.5 - half, fx * 0.5 + half] {
        b.prism_y(x, z_post, 0.08, 0.0, 0.08, 6, true);
        b.beam([x, 0.0, z_post], [x, arm_h, z_post], 0.05);
        b.beam([x, arm_h, z_post], [x, arm_h + 0.1, z_head], 0.045);
        set_tone(&mut b, Tone::Black);
        b.strut([x, arm_h + 0.05, z_head], [x, yb + bl * 0.06, z_head], 0.008);
        set_tone(&mut b, Tone::Metal);
        if has_boat { b.cuboid([x - 0.08, 0.0, zb - bl * 0.12], [x + 0.08, yb - keel * 0.6, zb + bl * 0.12]); }
        set_tone(&mut b, Tone::Paint);
    }
    let mut kit = single(PieceRole::Base, b.finish());
    if has_boat { kit.piece(PieceRole::Body, Some(0), [fx * 0.5 + bl * 0.03, yb, zb], Joint::Fixed, rhib_mesh(bl)); }
    kit.finish()
}

fn crane(reach: f32, f: [f32; 3]) -> PartModel {
    let [fx, fy, fz] = f;
    let (cx, cz) = (fx * 0.5, fz * 0.5);
    let pr = (fx.min(fz) * 0.3).clamp(0.2, 0.8);
    let ped = (fy * 0.6).clamp(0.4, 1.5);
    let hh = (pr * 1.2).clamp(0.4, 1.2);
    let mut b = builder(Tone::Paint);
    b.prism_y(cx, cz, pr, 0.0, ped, 8, true);
    // Slewing house with the operator's cab on the port side.
    chamfer_block(&mut b, [cx - pr * 1.1, ped, cz - pr * 0.9], [cx + pr * 0.9, ped + hh, cz + pr * 0.9], pr * 0.1);
    b.cuboid([cx - pr * 0.5, ped + hh * 0.2, cz - pr * 1.0 - 0.02], [cx + pr * 0.6, ped + hh * 0.95, cz - pr * 0.9]);
    set_tone(&mut b, Tone::Glass);
    b.face(&[[cx + pr * 0.6 + 0.005, ped + hh * 0.5, cz - pr * 1.0], [cx + pr * 0.6 + 0.005, ped + hh * 0.9, cz - pr * 1.0], [cx + pr * 0.6 + 0.005, ped + hh * 0.9, cz - pr * 0.9], [cx + pr * 0.6 + 0.005, ped + hh * 0.5, cz - pr * 0.9]], [1.0, 0.0, 0.0]);
    let mut kit = single(PieceRole::Base, b.finish());
    // Jib, raised 25 degrees, reaching `reach` horizontally from the heel; hook block hanging from the tip.
    let ang = 25.0 * DEG;
    let len = reach / libm::cosf(ang);
    let tip = [len * libm::cosf(ang), len * libm::sinf(ang), 0.0];
    let bw = (len * 0.02).clamp(0.06, 0.25);
    let mut j = builder(Tone::Paint);
    j.frustum([-bw, 0.0, 0.0], tip, bw * 1.4, bw * 0.8, 4, true);
    set_tone(&mut j, Tone::Black);
    j.beam([len * 0.35 * libm::cosf(ang), len * 0.35 * libm::sinf(ang) - bw, 0.0], [pr * 0.2, -hh * 0.8, 0.0], bw * 0.5);
    j.strut([tip[0], tip[1] - bw, 0.0], [tip[0], tip[1] * 0.4, 0.0], 0.01);
    j.cuboid([tip[0] - bw, tip[1] * 0.4 - bw * 1.6, -bw * 0.6], [tip[0] + bw, tip[1] * 0.4, bw * 0.6]);
    kit.piece(PieceRole::Arm, Some(0), [cx + pr * 0.5, ped + hh * 0.8, cz], Joint::Fixed, j.finish());
    kit.finish()
}

fn heli_deck(length: f32, width: f32, f: [f32; 3]) -> PartModel {
    let [fx, fy, fz] = f;
    let l = length.min(fx);
    let w = width.min(fz);
    let (x0, z0) = ((fx - l) * 0.5, (fz - w) * 0.5);
    let t = (fy * 0.5).clamp(0.08, 0.2);
    let edge = (l.min(w) * 0.04).clamp(0.1, 0.5);
    let mut b = builder(Tone::Deck);
    b.cuboid([x0 + edge, 0.0, z0 + edge], [x0 + l - edge, t, z0 + w - edge]);
    // Safety netting: a black strip round the edge, sloping down outward.
    set_tone(&mut b, Tone::Black);
    let (xa, xb, za, zb) = (x0 + edge, x0 + l - edge, z0 + edge, z0 + w - edge);
    let rim = [[xa, za], [xb, za], [xb, zb], [xa, zb]];
    for k in 0..4 {
        let (p, q) = (rim[k], rim[(k + 1) % 4]);
        let out = [[x0, z0], [x0 + l, z0], [x0 + l, z0 + w], [x0, z0 + w]];
        let (po, qo) = (out[k], out[(k + 1) % 4]);
        b.face(&[[p[0], t, p[1]], [q[0], t, q[1]], [qo[0], t * 0.4, qo[1]], [po[0], t * 0.4, po[1]]], [0.0, 1.0, 0.0]);
        b.face(&[[p[0], t * 0.6, p[1]], [q[0], t * 0.6, q[1]], [qo[0], t * 0.2, qo[1]], [po[0], t * 0.2, po[1]]], [0.0, -1.0, 0.0]);
    }
    // Markings: touchdown circle, perimeter line and the fore-aft line-up line, flush on the deck.
    set_tone(&mut b, Tone::Pale);
    let (cx, cz) = (x0 + l * 0.5, z0 + w * 0.5);
    let rc = (l.min(w) - 2.0 * edge) * 0.32;
    let lw = (rc * 0.08).max(0.05);
    let y = t + 0.002;
    b.ring_y(cx, cz, rc - lw, rc, y, 24);
    let flat = |b: &mut MeshBuilder, a: [f32; 2], c: [f32; 2]| b.face(&[[a[0], y, a[1]], [c[0], y, a[1]], [c[0], y, c[1]], [a[0], y, c[1]]], [0.0, 1.0, 0.0]);
    let i = edge + lw;
    flat(&mut b, [x0 + i, z0 + i], [x0 + l - i, z0 + i + lw]);
    flat(&mut b, [x0 + i, z0 + w - i - lw], [x0 + l - i, z0 + w - i]);
    flat(&mut b, [x0 + i, z0 + i + lw], [x0 + i + lw, z0 + w - i - lw]);
    flat(&mut b, [x0 + l - i - lw, z0 + i + lw], [x0 + l - i, z0 + w - i - lw]);
    flat(&mut b, [x0 + i + lw * 2.0, cz - lw * 0.5], [cx - rc - lw, cz + lw * 0.5]);
    // Perimeter lamps at the corners and mid-sides.
    set_tone(&mut b, Tone::Lamp);
    for (lx, lz) in [(xa, za), (xb, za), (xb, zb), (xa, zb), (cx, za), (cx, zb)] {
        b.prism_y(lx.clamp(x0 + 0.05, x0 + l - 0.05), lz.clamp(z0 + 0.05, z0 + w - 0.05), 0.05, t, t + 0.06, 6, true);
    }
    single(PieceRole::Base, b.finish()).finish()
}

fn drone_bay(doors: usize, f: [f32; 3]) -> PartModel {
    let [fx, fy, fz] = f;
    let hb = (fy * 0.8).clamp(0.3, 2.5);
    let mut b = builder(Tone::Paint);
    chamfer_block(&mut b, [0.02, 0.0, 0.02], [fx - 0.02, hb, fz - 0.02], 0.06);
    // Opening coaming, dark.
    let m = (fx.min(fz) * 0.1).clamp(0.05, 0.3);
    set_tone(&mut b, Tone::Black);
    b.cuboid([m, hb, m], [fx - m, hb + 0.03, fz - m]);
    let mut kit = single(PieceRole::Base, b.finish());
    // Hatch panels on top: one panel hinged on the aft edge, or clamshells hinged on the port and starboard
    // edges meeting on the centreline.
    let (x0, x1, z0, z1) = (m + 0.02, fx - m - 0.02, m + 0.02, fz - m - 0.02);
    let y = hb + 0.03;
    let tp = 0.04;
    let panel = |xa: f32, xb: f32, za: f32, zb: f32, pivot: [f32; 3]| {
        let mut d = builder(Tone::Paint);
        d.cuboid([xa - pivot[0], 0.0, za - pivot[2]], [xb - pivot[0], tp, zb - pivot[2]]);
        set_tone(&mut d, Tone::Black);
        let (mx, mz) = ((xa + xb) * 0.5 - pivot[0], (za + zb) * 0.5 - pivot[2]);
        d.cuboid([mx - 0.08, tp, mz - 0.02], [mx + 0.08, tp + 0.03, mz + 0.02]);
        d.finish()
    };
    if doors >= 2 {
        let zm = (z0 + z1) * 0.5;
        kit.piece(PieceRole::Door, Some(0), [x0, y, z0], Joint::Fixed, panel(x0, x1, z0, zm - 0.01, [x0, y, z0]));
        kit.piece(PieceRole::Door, Some(0), [x0, y, z1], Joint::Fixed, panel(x0, x1, zm + 0.01, z1, [x0, y, z1]));
    } else {
        kit.piece(PieceRole::Door, Some(0), [x0, y, z0], Joint::Fixed, panel(x0, x1, z0, z1, [x0, y, z0]));
    }
    kit.finish()
}

fn recovery_net(width: f32, f: [f32; 3]) -> PartModel {
    let [fx, fy, fz] = f;
    let w = width.min(fx).max(0.5);
    let (xa, xb) = ((fx - w) * 0.5 + 0.05, (fx + w) * 0.5 - 0.05);
    let zr = fz - 0.06;
    let hr = (fy * 0.8).clamp(0.6, 1.1);
    let mut b = builder(Tone::Paint);
    for x in [xa, xb] { b.beam([x, 0.0, zr], [x, hr, zr], 0.03); }
    b.beam([xa, hr, zr], [xb, hr, zr], 0.03);
    // The net hangs from the rail out over the side and down the hull to below the waterline (pale, so a
    // swimmer's target reads against a dark hull).
    set_tone(&mut b, Tone::Pale);
    let bottom = -1.6;
    let top = hr - 0.05;
    let z_out = fz + 0.25;
    let at = |x: f32, t: f32| [x, top + (bottom - top) * t, zr + (z_out - zr) * (t * 2.0).min(1.0)];
    let cols = ((xb - xa) / 0.4).ceil().max(1.0) as usize;
    for i in 0..=cols {
        let x = xa + (xb - xa) * i as f32 / cols as f32;
        b.strut(at(x, 0.0), at(x, 0.5), 0.012);
        b.strut(at(x, 0.5), at(x, 1.0), 0.012);
    }
    let rows = 6;
    for k in 1..=rows {
        let t = k as f32 / rows as f32;
        b.strut(at(xa, t), at(xb, t), 0.012);
    }
    single(PieceRole::Base, b.finish()).finish()
}

// ---------------------------------------------------------------------------------------------------------
// Deck details

fn searchlight(d: f32, f: [f32; 3]) -> PartModel {
    let [fx, _fy, fz] = f;
    let d = d.min(fx * 0.7).min(fz * 0.7).max(0.15);
    let (cx, cz) = (fx * 0.5, fz * 0.5);
    let r = d * 0.5;
    let ped = d * 0.55;
    let yd = ped + r * 1.15;
    let mut b = builder(Tone::Paint);
    b.prism_y(cx, cz, r * 0.35, 0.0, ped, 8, true);
    b.prism_y(cx, cz, r * 0.6, ped, ped + 0.03, 8, true);
    // Yoke cheeks either side of the drum.
    for s in [-1.0f32, 1.0] {
        vplate(&mut b, [cx - r * 0.25, cz + s * (r + 0.03)], [cx + r * 0.25, cz + s * (r + 0.03)], ped, yd + r * 0.1, 0.03);
    }
    // Drum along +X, black back, lamp lens at the front.
    let (xr, xf) = (cx - d * 0.4, cx + d * 0.4);
    b.frustum([xr, yd, cz], [xf, yd, cz], r * 0.9, r, 10, false);
    set_tone(&mut b, Tone::Black);
    b.with(Xform::rot_y(180.0 * DEG), |b| disc_x(b, [-xr, yd, -cz], r * 0.9, 10));
    b.frustum([xf, yd, cz], [xf + 0.02, yd, cz], r * 1.04, r * 1.04, 10, false);
    set_tone(&mut b, Tone::Lamp);
    disc_x(&mut b, [xf + 0.01, yd, cz], r * 0.92, 10);
    let mut kit = single(PieceRole::Base, b.finish());
    kit.anchor(AnchorKind::Light, 0, [xf + 0.02, yd, cz], [1.0, 0.0, 0.0]);
    kit.finish()
}

fn railing(len: f32, f: [f32; 3]) -> PartModel {
    let [fx, fy, fz] = f;
    let l = len.min(fx);
    let x0 = (fx - l) * 0.5;
    let cz = fz * 0.5;
    let h = fy.clamp(0.5, 1.1);
    let posts = (l / 1.2).ceil().max(1.0) as usize + 1;
    let mut b = builder(Tone::Paint);
    for i in 0..posts {
        let x = x0 + 0.03 + (l - 0.06) * i as f32 / (posts - 1) as f32;
        b.prism_y(x, cz, 0.025, 0.0, h, 4, true);
    }
    for y in [h - 0.02, h * 0.5] { b.frustum([x0 + 0.01, y, cz], [x0 + l - 0.01, y, cz], 0.02, 0.02, 6, true); }
    single(PieceRole::Detail, b.finish()).finish()
}

fn bollard(pair: usize, f: [f32; 3]) -> PartModel {
    let [fx, fy, fz] = f;
    let cz = fz * 0.5;
    let n = pair.clamp(1, 2);
    let r = (if n == 2 { fx * 0.16 } else { fx * 0.3 }).min(fz * 0.3).clamp(0.05, 0.25);
    let h = fy.min(r * 3.5).max(0.15);
    let mut b = builder(Tone::Paint);
    b.cuboid([0.03, 0.0, cz - r * 1.4], [fx - 0.03, 0.04, cz + r * 1.4]);
    set_tone(&mut b, Tone::Black);
    let xs: &[f32] = if n == 2 { &[0.27, 0.73] } else { &[0.5] };
    for &t in xs {
        let x = fx * t;
        b.prism_y(x, cz, r, 0.04, h * 0.85, 8, false);
        b.prism_y(x, cz, r * 1.25, h * 0.85, h, 8, true);
    }
    single(PieceRole::Detail, b.finish()).finish()
}

fn liferaft(count: usize, f: [f32; 3]) -> PartModel {
    let [fx, fy, fz] = f;
    let count = count.clamp(1, 6);
    let tiers = if count > 3 && fy >= 1.0 { 2 } else { 1 };
    let per = count.div_ceil(tiers);
    let cd = (0.55f32).min(fz / per as f32 * 0.88).min((fy - 0.1).max(0.2) / tiers as f32 * 0.85).max(0.03);
    let cl = (cd * 2.2).min(fx * 0.9);
    let cx = fx * 0.5;
    let rr = cd * 0.5;
    let mut b = builder(Tone::Metal);
    // Rack: two cross bars under each tier and two uprights.
    let pitch = fz / per as f32;
    for tier in 0..tiers {
        let yb = 0.05 + tier as f32 * cd * 1.05;
        for x in [cx - cl * 0.3, cx + cl * 0.3] { b.cuboid([x - 0.03, yb - 0.05, 0.02], [x + 0.03, yb, fz - 0.02]); }
    }
    for x in [cx - cl * 0.3, cx + cl * 0.3] { b.cuboid([x - 0.03, 0.0, 0.02], [x + 0.03, 0.05 + (tiers as f32 - 1.0) * cd * 1.05, 0.06]); }
    for k in 0..count {
        let (tier, slot) = (k / per, k % per);
        let y = 0.05 + tier as f32 * cd * 1.05 + rr;
        let z = pitch * (slot as f32 + 0.5);
        set_tone(&mut b, Tone::Pale);
        b.frustum([cx - cl * 0.5, y, z], [cx + cl * 0.5, y, z], rr, rr, 8, true);
        set_tone(&mut b, Tone::Black);
        for t in [-0.25f32, 0.25] { b.frustum([cx + cl * t - 0.025, y, z], [cx + cl * t + 0.025, y, z], rr * 1.04, rr * 1.04, 8, false); }
    }
    single(PieceRole::Detail, b.finish()).finish()
}

fn vent(h: f32, f: [f32; 3]) -> PartModel {
    let [fx, fy, fz] = f;
    let h = h.min(fy.max(0.5));
    let (cx, cz) = (fx * 0.5, fz * 0.5);
    let pr = (fx.min(fz) * 0.18).clamp(0.04, 0.25);
    let dx = (h * 0.25).min((fx * 0.5 - pr * 1.6).max(0.0));
    let mut b = builder(Tone::Paint);
    b.prism_y(cx, cz, pr * 1.3, 0.0, 0.05, 8, true);
    b.frustum([cx, 0.0, cz], [cx, h * 0.68, cz], pr, pr, 8, false);
    // Cowl: flares forward and up so the mouth faces +X.
    let mouth = [cx + dx, h - pr * 1.2, cz];
    b.frustum([cx, h * 0.68, cz], mouth, pr, pr * 1.6, 8, false);
    set_tone(&mut b, Tone::Black);
    let dir = crate::prim::normalize_or([dx, h * 0.32 - pr * 1.2, 0.0], [0.0, 1.0, 0.0]);
    b.frustum(mouth, [mouth[0] + dir[0] * 0.01, mouth[1] + dir[1] * 0.01, cz], pr * 1.6, pr * 1.6, 8, true);
    single(PieceRole::Detail, b.finish()).finish()
}

fn anchor(size: f32, f: [f32; 3]) -> PartModel {
    let [fx, fy, fz] = f;
    let s = size.min(fy * 0.9).min(fx * 0.9).max(0.2);
    let cx = fx * 0.5;
    let zf = fz - 0.02;
    let t = (s * 0.1).min(fz * 0.4);
    let top = fy.max(s) - 0.02;
    let mut b = builder(Tone::Metal);
    // Hawse lip on the outboard face, chain running inboard across the top.
    b.frustum([cx, top - s * 0.08, zf - 0.04], [cx, top - s * 0.08, zf], s * 0.16, s * 0.18, 8, true);
    b.cuboid([cx - s * 0.04, top - 0.02, 0.02], [cx + s * 0.04, top, zf - 0.04]);
    // Stockless anchor: shank, crown and two flukes, black, against the outboard face.
    set_tone(&mut b, Tone::Black);
    let zc = zf - t * 0.5;
    b.cuboid([cx - s * 0.06, top - s * 0.85, zc - t * 0.5], [cx + s * 0.06, top - s * 0.1, zc + t * 0.5]);
    let yc = top - s * 0.85;
    let outline = [[-s * 0.35, yc + s * 0.25], [-s * 0.3, yc], [s * 0.3, yc], [s * 0.35, yc + s * 0.25], [s * 0.1, yc + s * 0.08], [-s * 0.1, yc + s * 0.08]];
    let pts: Vec<[f32; 2]> = outline.iter().map(|p| [p[0] + cx, p[1]]).collect();
    b.with(Xform::translation([0.0, 0.0, zc - t * 0.6]), |b| b.extrude_z(&pts, t * 1.2));
    single(PieceRole::Detail, b.finish()).finish()
}

/// Watertight door (vertical, in the footprint's thinner horizontal axis) or a deck hatch.
fn door(hatch: bool, f: [f32; 3]) -> PartModel {
    let [fx, fy, fz] = f;
    let m = 0.06;
    let tp = 0.05;
    if hatch {
        let hc = fy.clamp(0.1, 0.25) * 0.6;
        let mut b = builder(Tone::Paint);
        // Coaming: four low walls.
        let (x0, x1, z0, z1) = (0.02, fx - 0.02, 0.02, fz - 0.02);
        b.cuboid([x0, 0.0, z0], [x1, hc, z0 + m]);
        b.cuboid([x0, 0.0, z1 - m], [x1, hc, z1]);
        b.cuboid([x0, 0.0, z0 + m], [x0 + m, hc, z1 - m]);
        b.cuboid([x1 - m, 0.0, z0 + m], [x1, hc, z1 - m]);
        let mut kit = single(PieceRole::Base, b.finish());
        let pivot = [x0, hc, (z0 + z1) * 0.5];
        let mut d = builder(Tone::Paint);
        d.cuboid([0.0, 0.0, z0 - pivot[2]], [x1 - x0, tp, z1 - pivot[2]]);
        set_tone(&mut d, Tone::Black);
        for (u, v) in [(0.2f32, 0.15f32), (0.8, 0.15), (0.2, 0.85), (0.8, 0.85)] {
            let (x, z) = ((x1 - x0) * u, z0 - pivot[2] + (z1 - z0) * v);
            d.cuboid([x - 0.04, tp, z - 0.015], [x + 0.04, tp + 0.03, z + 0.015]);
        }
        kit.piece(PieceRole::Door, Some(0), pivot, Joint::Fixed, d.finish());
        return kit.finish();
    }
    // Canonical door: normal along X, width along Z, slab centred on x = 0; rotated when the footprint is
    // thinner in Z than in X.
    let along_x = fx > fz;
    let (w, slab) = if along_x { (fx, fz) } else { (fz, fx) };
    let place = if along_x { Xform::translation([0.0, 0.0, fz * 0.5]).then(&Xform::rot_y(90.0 * DEG)) } else { Xform::translation([fx * 0.5, 0.0, 0.0]) };
    let rot = if along_x { Xform::rot_y(90.0 * DEG) } else { Xform::IDENTITY };
    let h = fy.max(0.5);
    let ft = (slab * 0.8).min(0.12);
    let mut b = builder(Tone::Paint);
    b.with(place, |b| {
        b.cuboid([-ft * 0.5, 0.0, 0.0], [ft * 0.5, h, m]);
        b.cuboid([-ft * 0.5, 0.0, w - m], [ft * 0.5, h, w]);
        b.cuboid([-ft * 0.5, h - m, m], [ft * 0.5, h, w - m]);
        b.cuboid([-ft * 0.5, 0.0, m], [ft * 0.5, 0.12, w - m]);
    });
    let mut kit = single(PieceRole::Base, b.finish());
    let hinge = [0.0, 0.12, m];
    let pivot = place.apply(hinge);
    let mut d = builder(Tone::Paint);
    let (dh, dw) = (h - m - 0.12 - 0.01, w - 2.0 * m - 0.01);
    d.with(rot, |d| {
        d.cuboid([-tp * 0.5, 0.0, 0.005], [tp * 0.5, dh, dw]);
        set_tone(d, Tone::Black);
        for side in [-1.0f32, 1.0] {
            let x = side * (tp * 0.5 + 0.015);
            for (v, u) in [(0.2f32, 0.85f32), (0.5, 0.85), (0.8, 0.85), (0.5, 0.15)] {
                d.cuboid([x - 0.015, dh * v - 0.04, dw * u - 0.015], [x + 0.015, dh * v + 0.04, dw * u + 0.015]);
            }
        }
    });
    kit.piece(PieceRole::Door, Some(0), pivot, Joint::Fixed, d.finish());
    kit.finish()
}

// ---------------------------------------------------------------------------------------------------------
// Internal equipment (editor cutaway)

fn internal(kind: usize, f: [f32; 3]) -> PartModel {
    let [fx, fy, fz] = f;
    let g = 0.02;
    let (x0, x1, z0, z1, y1) = (g, fx - g, g, fz - g, fy - g);
    let c = (fx.min(fy).min(fz) * 0.12).min(0.08);
    let mut b = builder(Tone::Metal);
    match kind {
        1 => {
            // Tank: a horizontal faceted cylinder on two saddles, primer.
            let r = ((y1 - 0.06).min(z1 - z0) * 0.5).max(0.05);
            let (yc, cz) = (0.06 + r, fz * 0.5);
            for x in [x0 + (x1 - x0) * 0.2, x0 + (x1 - x0) * 0.8] { b.cuboid([x - 0.05, 0.0, cz - r * 0.7], [x + 0.05, yc - r * 0.6, cz + r * 0.7]); }
            set_tone(&mut b, Tone::Primer);
            b.frustum([x0, yc, cz], [x1, yc, cz], r, r, 8, true);
            set_tone(&mut b, Tone::Metal);
            b.prism_y(x0 + (x1 - x0) * 0.5, cz, (r * 0.15).max(0.03), yc + r * 0.9, (yc + r + 0.06).min(fy), 6, true);
        }
        2 => {
            // Magazine: dark chamfered box with a pale band round its middle.
            set_tone(&mut b, Tone::Deck);
            chamfer_block(&mut b, [x0, 0.0, z0], [x1, y1, z1], c);
            set_tone(&mut b, Tone::Pale);
            let (ya, yb) = (y1 * 0.45, y1 * 0.55);
            b.cuboid([x0 - 0.004, ya, z0 + c], [x1 + 0.004, yb, z1 - c]);
            b.cuboid([x0 + c, ya, z0 - 0.004], [x1 - c, yb, z1 + 0.004]);
        }
        3 => {
            // Accommodation: pale block with dark berth slots on both long faces.
            set_tone(&mut b, Tone::Pale);
            chamfer_block(&mut b, [x0, 0.0, z0], [x1, y1, z1], c);
            set_tone(&mut b, Tone::Black);
            let tiers = if y1 > 1.2 { 2 } else { 1 };
            let slots = ((x1 - x0) / 2.0).floor().clamp(1.0, 16.0) as usize;
            for s in 0..slots {
                let (xa, xb) = (x0 + (x1 - x0) * (s as f32 + 0.1) / slots as f32, x0 + (x1 - x0) * (s as f32 + 0.9) / slots as f32);
                for t in 0..tiers {
                    let (ya, yb) = (y1 * (0.15 + 0.45 * t as f32), y1 * (0.4 + 0.45 * t as f32));
                    b.cuboid([xa, ya, z0 - 0.004], [xb, yb, z0 + 0.01]);
                    b.cuboid([xa, ya, z1 - 0.01], [xb, yb, z1 + 0.004]);
                }
            }
        }
        _ => {
            // Machinery: black skid, gunmetal block, a row of cylinder heads and an exhaust stub.
            set_tone(&mut b, Tone::Black);
            let skid = (y1 * 0.1).clamp(0.02, 0.15);
            b.cuboid([x0, 0.0, z0], [x1, skid, z1]);
            set_tone(&mut b, Tone::Metal);
            let top = y1 * 0.82;
            chamfer_block(&mut b, [x0 + c, skid, z0 + c], [x1 - c, top, z1 - c], c);
            set_tone(&mut b, Tone::Black);
            let heads = ((x1 - x0) / 0.6).floor().clamp(1.0, 12.0) as usize;
            let hw = (x1 - x0 - 2.0 * c) / heads as f32;
            for k in 0..heads {
                let xa = x0 + c + hw * k as f32;
                b.cuboid([xa + hw * 0.12, top, fz * 0.5 - (z1 - z0) * 0.2], [xa + hw * 0.88, y1, fz * 0.5 + (z1 - z0) * 0.2]);
            }
        }
    }
    let mut kit = single(PieceRole::Base, b.finish());
    if kind == 0 { kit.anchor(AnchorKind::PumpOutlet, 0, [fx * 0.5, y1, fz * 0.5], [0.0, 1.0, 0.0]); }
    kit.finish()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parts::{Family, GENERATORS};
    use crate::prim::tests::check_consistent;

    fn fittings() -> Vec<Generator> { GENERATORS.iter().filter(|s| s.family == Family::Fittings).map(|s| s.generator).collect() }

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

    /// Generators allowed past the footprint in X/Z (module notes).
    fn overhangs(g: Generator) -> bool { matches!(g, Generator::Crane | Generator::RecoveryNet | Generator::Waterjet) }

    const FOOTPRINTS: [[u8; 3]; 7] = [[1, 1, 1], [2, 2, 2], [4, 2, 4], [6, 4, 6], [16, 8, 16], [0, 0, 0], [255, 255, 255]];

    #[test]
    fn every_fitting_builds_within_budget_and_footprint() {
        for g in fittings() {
            for params in param_sets(g) {
                for fp in FOOTPRINTS {
                    let m = build(g, &params, fp);
                    check_model(&m);
                    if overhangs(g) { continue; }
                    let f = foot_m(fp);
                    let e = 1e-3;
                    assert!(m.aabb.min[0] >= -e && m.aabb.max[0] <= f[0] + e && m.aabb.min[2] >= -e && m.aabb.max[2] <= f[2] + e,
                        "{g:?} {params:?} {fp:?} aabb {:?}", m.aabb);
                }
            }
        }
    }

    #[test]
    fn propulsors_spin_steer_and_wash() {
        let m = build(Generator::Propeller, &[1.5, 4.0], [4, 2, 4]);
        let p: Vec<_> = m.pieces.iter().filter(|p| p.role == PieceRole::Propeller).collect();
        assert_eq!(p.len(), 1);
        assert_eq!(p[0].joint, Joint::Spin { axis: [1.0, 0.0, 0.0] });
        assert!(m.anchors.iter().any(|a| a.kind == AnchorKind::Wash && a.dir == [-1.0, 0.0, 0.0]));
        // Hub centred on its pivot so it spins true.
        let n = p[0].mesh.positions.len() as f32;
        for k in [1, 2] { assert!((p[0].mesh.positions.iter().map(|v| v[k]).sum::<f32>() / n).abs() < 0.02); }
        // Hangs below the hull, unpainted.
        assert!(m.aabb.max[1] <= 1e-4 && m.aabb.min[1] < -1.0);
        for pc in &m.pieces { assert!(pc.mesh.colors.iter().all(|c| c[3] == 0.0)); }
        // Blade count shows.
        let t = |b: f32| build(Generator::Propeller, &[1.5, b], [4, 2, 4]).pieces[1].mesh.triangle_count();
        assert!(t(3.0) < t(7.0));

        let r = build(Generator::Rudder, &[1.5, 1.0], [2, 2, 2]);
        let arm: Vec<_> = r.pieces.iter().filter(|p| p.role == PieceRole::Arm).collect();
        assert_eq!(arm.len(), 1);
        assert_eq!(arm[0].joint, Joint::Train);
        assert!(r.aabb.min[1] < -1.5 && r.aabb.max[1] <= 1e-4);

        let w = build(Generator::Waterjet, &[0.8], [4, 2, 4]);
        let wash = w.anchors.iter().find(|a| a.kind == AnchorKind::Wash).unwrap();
        assert_eq!(w.pieces[wash.piece as usize].role, PieceRole::Arm);
        assert_eq!(w.pieces[wash.piece as usize].joint, Joint::Train);
        assert!(w.aabb.min[0] < 0.0, "nozzle stands aft of the transom");
    }

    #[test]
    fn funnels_and_lights_carry_their_anchors() {
        for (rake, cap) in [(0.0, 0.0), (20.0, 1.0)] {
            let m = build(Generator::Funnel, &[3.0, 1.5, 2.0, rake, cap], [6, 6, 4]);
            let ex: Vec<_> = m.anchors.iter().filter(|a| a.kind == AnchorKind::Exhaust).collect();
            assert_eq!(ex.len(), 1);
            assert!(ex[0].dir[1] > 0.9 && ex[0].dir[0] <= 0.0);
            assert!(ex[0].pos[1] > 2.5);
        }
        // The mouth reads as an opening from above: nothing dark near the top faces down, the well wall faces
        // the axis and a dark floor faces up below the rim.
        for cap in [0.0, 1.0] {
            let m = build(Generator::Funnel, &[3.0, 1.5, 2.0, 5.0, cap], [6, 6, 4]);
            let me = &m.pieces[0].mesh;
            let ex = m.anchors[0].pos;
            let h_body = ex[1] + 3.0 * 0.03 * if cap > 0.5 { 0.9 } else { 1.0 };
            let mut floor = false;
            for t in me.indices.chunks(3) {
                if me.colors[t[0] as usize] != Tone::Black.rgba() { continue; }
                let c = [0, 1, 2].map(|k| t.iter().map(|&i| me.positions[i as usize][k]).sum::<f32>() / 3.0);
                if c[1] < h_body * 0.96 || c[1] > h_body + 1e-3 { continue; }
                let n = me.normals[t[0] as usize];
                assert!(n[1] > -1e-3, "dark face points down at {c:?}");
                if n[1].abs() < 0.1 { assert!(n[0] * (ex[0] - c[0]) + n[2] * (ex[2] - c[2]) > 0.0, "well wall faces out at {c:?}"); }
                if n[1] > 0.99 && c[1] < h_body - 0.05 { floor = true; }
            }
            assert!(floor, "no floor in the mouth");
        }
        // Rake moves the mouth aft.
        let mouth = |rake: f32| build(Generator::Funnel, &[3.0, 1.5, 2.0, rake, 0.0], [8, 6, 4]).anchors[0].pos[0];
        assert!(mouth(20.0) < mouth(0.0) - 0.5);

        let s = build(Generator::Searchlight, &[0.6], [2, 2, 2]);
        let l: Vec<_> = s.anchors.iter().filter(|a| a.kind == AnchorKind::Light).collect();
        assert_eq!(l.len(), 1);
        assert_eq!(l[0].dir, [1.0, 0.0, 0.0]);
        assert!(s.pieces[0].mesh.custom0.iter().any(|c| c[0] == 1.0), "lamp lens emits");
    }

    #[test]
    fn internal_blocks_read_apart_by_value() {
        let mean_y = |k: f32| {
            let m = build(Generator::Internal, &[k], [6, 4, 4]);
            check_model(&m);
            let c = &m.pieces[0].mesh.colors;
            assert!(c.iter().all(|c| c[3] == 0.0), "internals never wear ship paint");
            c.iter().map(|c| c[0] + c[1] + c[2]).sum::<f32>() / (3.0 * c.len() as f32)
        };
        let v: Vec<f32> = (0..4).map(|k| mean_y(k as f32)).collect();
        for i in 0..4 { for j in i + 1..4 { assert!((v[i] - v[j]).abs() > 0.02, "tones {i} and {j}: {v:?}"); } }
        assert!(build(Generator::Internal, &[0.0], [4, 4, 4]).anchors.iter().any(|a| a.kind == AnchorKind::PumpOutlet));
        assert!(build(Generator::Internal, &[1.0], [4, 4, 4]).anchors.is_empty());
    }

    #[test]
    fn boats_doors_and_decks() {
        let d = build(Generator::Davit, &[6.0], [16, 6, 8]);
        let boat: Vec<_> = d.pieces.iter().filter(|p| p.role == PieceRole::Body).collect();
        assert_eq!(boat.len(), 1);
        let bb = boat[0].mesh.aabb();
        assert!(bb.max[0] - bb.min[0] > 5.0, "boat length {}", bb.max[0] - bb.min[0]);
        assert!(build(Generator::Davit, &[0.0], [8, 4, 4]).pieces.iter().all(|p| p.role != PieceRole::Body));

        for hatch in [0.0, 1.0] {
            for fp in [[1, 4, 3], [3, 4, 1], [3, 1, 3]] {
                let m = build(Generator::Door, &[hatch], fp);
                assert_eq!(m.pieces.iter().filter(|p| p.role == PieceRole::Door).count(), 1);
            }
        }
        assert_eq!(build(Generator::DroneBay, &[2.0], [6, 2, 6]).pieces.iter().filter(|p| p.role == PieceRole::Door).count(), 2);
        let h = build(Generator::HeliDeck, &[15.0, 12.0], [40, 1, 30]);
        let span = |m: &PartModel, k: usize| m.aabb.max[k] - m.aabb.min[k];
        assert!((span(&h, 0) - 15.0).abs() < 0.05 && (span(&h, 2) - 12.0).abs() < 0.05);
        let n = |c: f32| build(Generator::Liferaft, &[c], [6, 4, 6]).pieces[0].mesh.triangle_count();
        assert!(n(1.0) < n(3.0) && n(3.0) < n(6.0));
    }

    #[test]
    fn fallback_and_foreign_generators() {
        for fp in FOOTPRINTS {
            let m = fallback_box(fp);
            check_model(&m);
            let f = foot_m(fp);
            assert_eq!(m.aabb.max, f);
            assert_eq!(build(Generator::Box, &[], fp), m);
        }
        check_model(&build(Generator::GunTurret, &[], [2, 2, 2]));
        check_model(&build(Generator::Funnel, &[f32::NAN, f32::INFINITY, -3.0, 1e9, 7.0], [2, 2, 2]));
    }

    #[test]
    fn fittings_are_deterministic() {
        for g in fittings() { assert_eq!(build(g, &[], [6, 4, 6]), build(g, &[], [6, 4, 6])); }
    }
}
