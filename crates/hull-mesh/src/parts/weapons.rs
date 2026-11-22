//! Unit `hm-parts-weapons`. Gun mounts, launchers, tubes, racks, decoy launchers and mine rails.
//! DESIGN.md L1233, L539-553 (weapon families: 7 guns 20-127 mm, heavy 130-155 mm in Act 3, missiles, torpedo
//! tubes, depth-charge racks, decoys), L1968 (muzzle anchors), L1919-1921 (geometric, cell-built look).
//!
//! Each model: a fixed `Base`; trainable weapons add a `Turret` (Joint::Train) and a `Cradle` (Joint::Elevate)
//! carrying `Barrel` pieces (Joint::Recoil) with one `Muzzle` anchor per barrel; launchers add `LaunchCell`
//! anchors per cell or tube. Barrel length and bore scale with calibre (calibres 20-155 mm); shielded turrets
//! read as faceted gunhouses. Silhouettes must stay distinct at 42 px (G1, L1807), so proportions are
//! deliberate, not uniform. 20-80 triangles per small piece; total under 2,000 per model.
//!
//! # Model notes
//! - Barrels are `calibre x L/D` long with real-gun ratios (20 mm L/80 down to 155 mm L/52), wear a thick
//!   sleeve at the mantlet, and end in a flash hider (autocannon), a muzzle brake (57-100 mm) or a fume
//!   extractor bulge (over 100 mm). Each barrel's bore is a black disc. Barrels overhang the footprint, as real
//!   barrels overhang their gun's deck seat.
//! - Gunhouses come in three faceted styles by calibre so mounts stay tellable apart: a low wedge (under
//!   70 mm), a rounded faceted cupola (70-90 mm) and a tall stealth house with chines (over 90 mm).
//! - The CIWS barrel cluster is one `Barrel` piece with `Joint::Spin { axis: +X }` (the client spins it while
//!   the mount fires) and one `Muzzle` anchor on the cluster axis.
//! - Sim launchers train but never elevate (`LauncherDef` has no elevation rate), so a trainable box launcher's
//!   `Cradle` is `Joint::Fixed` with the `elevation` parameter baked in; trainable torpedo tubes carry their
//!   tubes on the `Turret` piece. Fixed launchers keep everything on the `Base` piece.
//! - Depth-charge racks and mine rails drop over the aft (x = 0) end of the footprint; the part's rotation
//!   puts that end at the stern.
use super::{AnchorKind, Generator, Joint, PartModel, PieceRole, Tone};
use crate::prim::kit::{builder, disc_x, ellipse_ring, foot_m, footprint_block, sanitize, set_tone, vplate, Kit};
use crate::prim::{MeshBuilder, Xform};
use crate::types::MeshData;

const DEG: f32 = core::f32::consts::PI / 180.0;

/// Build `g` (a weapons-family generator) from resolved parameters (see `GENERATORS` for order and ranges).
/// Unit: `hm-parts-weapons`.
pub fn build(g: Generator, params: &[f32], footprint: [u8; 3]) -> PartModel {
    let p = sanitize(g, params);
    let f = foot_m(footprint);
    match g {
        Generator::GunTurret => gun_turret(p[0], p[1] as usize, p[2] >= 0.5, f),
        Generator::GunOpen => gun_open(p[0], p[1] as usize, f),
        Generator::Ciws => ciws(p[0], p[1] as usize, p[2] >= 0.5, f),
        Generator::LauncherBox => launcher_box(p[0] as usize, p[1], p[2], p[3] >= 0.5, p[4], f),
        Generator::LauncherVls => launcher_vls(p[0] as usize, p[1] as usize, p[2], f),
        Generator::TorpedoTubes => torpedo_tubes(p[0] as usize, p[1], p[2], p[3] >= 0.5, f),
        Generator::DepthChargeRack => depth_charge_rack(p[0] as usize, f),
        Generator::DecoyLauncher => decoy_launcher(p[0] as usize, p[1], f),
        Generator::MineRail => mine_rail(p[0], f),
        _ => footprint_block(footprint),
    }
}

// ---------------------------------------------------------------------------------------------------------
// Barrels

/// Barrel length in calibres by bore (piecewise linear through real naval guns: Oerlikon 20 mm L/80, Bofors
/// 40 mm L/70, 57 mm L/70, OTO 76 mm L/62, 127 mm L/54, 155 mm L/52).
fn calibres_long(calibre_mm: f32) -> f32 {
    const T: [(f32, f32); 6] = [(20.0, 80.0), (40.0, 70.0), (57.0, 70.0), (76.0, 62.0), (127.0, 54.0), (155.0, 52.0)];
    if calibre_mm <= T[0].0 { return T[0].1; }
    for w in T.windows(2) {
        let ((c0, l0), (c1, l1)) = (w[0], w[1]);
        if calibre_mm <= c1 { return l0 + (l1 - l0) * (calibre_mm - c0) / (c1 - c0); }
    }
    T[T.len() - 1].1
}

/// Recoil travel in metres (4 mm per mm of calibre), clamped.
fn recoil_travel(calibre_mm: f32) -> f32 { (calibre_mm * 0.004).clamp(0.04, 0.7) }

#[derive(Clone, Copy, PartialEq)]
enum Tip { FlashHider, Brake, Evacuator }

fn tip_for(calibre_mm: f32) -> Tip {
    if calibre_mm <= 45.0 { Tip::FlashHider } else if calibre_mm <= 100.0 { Tip::Brake } else { Tip::Evacuator }
}

/// One barrel along +X in its piece frame: from `rear` (behind the pivot, so recoil never opens a gap) to the
/// muzzle at (`muzzle_x`, 0, 0), with a thick sleeve to `sleeve_x`.
fn barrel_mesh(bore: f32, rear: f32, sleeve_x: f32, muzzle_x: f32, tip: Tip) -> MeshData {
    let sides = if bore < 0.045 { 6 } else { 8 };
    let r_muzzle = (bore * 0.95).max(0.012);
    let r_breech = (bore * 1.35).max(r_muzzle);
    let r_sleeve = bore * 1.8 + 0.01;
    let sleeve_x = sleeve_x.min(muzzle_x * 0.6).max(rear + 0.05);
    let mut b = builder(Tone::Paint);
    b.frustum([rear, 0.0, 0.0], [sleeve_x, 0.0, 0.0], r_sleeve, r_sleeve * 0.92, sides, true);
    let tip_len = match tip { Tip::FlashHider => bore * 3.5, Tip::Brake => bore * 5.5, Tip::Evacuator => 0.0 };
    let tube_end = (muzzle_x - tip_len).max(sleeve_x + 0.05);
    b.frustum([sleeve_x, 0.0, 0.0], [tube_end, 0.0, 0.0], r_breech, r_muzzle, sides, tip == Tip::Evacuator);
    match tip {
        Tip::FlashHider => b.frustum([tube_end, 0.0, 0.0], [muzzle_x, 0.0, 0.0], r_muzzle, r_muzzle * 1.45, sides, true),
        Tip::Brake => b.frustum([tube_end, 0.0, 0.0], [muzzle_x, 0.0, 0.0], r_muzzle * 1.5, r_muzzle * 1.5, sides, true),
        Tip::Evacuator => {
            let x0 = sleeve_x + (tube_end - sleeve_x) * 0.4;
            let r = (r_breech + (r_muzzle - r_breech) * 0.4) * 1.35;
            b.frustum([x0, 0.0, 0.0], [x0 + bore * 9.0, 0.0, 0.0], r, r, sides, true);
        }
    }
    set_tone(&mut b, Tone::Black);
    disc_x(&mut b, [muzzle_x + 0.002, 0.0, 0.0], bore * 0.5, sides);
    b.finish()
}

/// Lateral barrel offsets (z) for `n` barrels at spacing `s`.
fn offsets(n: usize, s: f32) -> Vec<f32> { (0..n).map(|k| (k as f32 - (n as f32 - 1.0) * 0.5) * s).collect() }

/// Add barrel pieces under `cradle` with a muzzle anchor each.
fn add_barrels(kit: &mut Kit, cradle: u8, calibre: f32, zs: &[f32], sleeve_x: f32, muzzle_x: f32) {
    let bore = calibre / 1000.0;
    let travel = recoil_travel(calibre);
    let rear = -(travel + 0.05 + bore * 2.0);
    for &z in zs {
        let mesh = barrel_mesh(bore, rear, sleeve_x, muzzle_x, tip_for(calibre));
        let i = kit.piece(PieceRole::Barrel, Some(cradle), [0.0, 0.0, z], Joint::Recoil { travel_m: travel }, mesh);
        kit.anchor(AnchorKind::Muzzle, i, [muzzle_x + 0.002, 0.0, 0.0], [1.0, 0.0, 0.0]);
    }
}

// ---------------------------------------------------------------------------------------------------------
// Gun turret (shielded gunhouse or open mount)

/// Gunhouse rings (turret frame, floor at y = 0) for one calibre style, plus the centreline front x at the floor
/// and at the roof.
fn gunhouse(calibre: f32, hl: f32, hw: f32, h: f32) -> (Vec<Vec<[f32; 3]>>, f32, f32) {
    if calibre < 70.0 {
        // Low wedge: chamfered nose, steep glacis, vertical rear, slight tumblehome.
        let bottom = [(hl, -0.55), (hl * 0.5, -1.0), (-hl, -1.0), (-hl, 1.0), (hl * 0.5, 1.0), (hl, 0.55)];
        let top_front = (hl - h * 1.1).max(-hl * 0.1);
        let k = (top_front + hl * 0.96) / (2.0 * hl);
        let ring = |y: f32, top: bool| -> Vec<[f32; 3]> {
            bottom.iter().map(|&(x, z)| if top { [-hl * 0.96 + (x + hl) * k, y, z * hw * 0.82] } else { [x, y, z * hw] }).collect()
        };
        (vec![ring(0.0, false), ring(h, true)], hl, top_front)
    } else if calibre <= 90.0 {
        // Rounded faceted cupola, centred slightly aft.
        let n = 10;
        let start = core::f32::consts::PI / n as f32;
        let levels = [(0.0, 1.0), (0.5, 0.97), (0.82, 0.78), (1.0, 0.42)];
        let rings: Vec<Vec<[f32; 3]>> = levels.iter().map(|&(t, s)| ellipse_ring(-hl * 0.05, h * t, 0.0, hl * 0.95 * s, hw * s, n, start)).collect();
        let front = |s: f32| -hl * 0.05 + hl * 0.95 * s * libm::cosf(start);
        (rings, front(1.0), front(0.97))
    } else {
        // Tall stealth house: pointed chined nose, sloped sides, flat roof.
        let bottom = [(hl, -0.35), (hl * 0.45, -1.0), (-hl, -1.0), (-hl, 1.0), (hl * 0.45, 1.0), (hl, 0.35)];
        let top_front = (hl - h * 0.75).max(0.0);
        let top = [(top_front, -0.3), (hl * 0.45 - h * 0.2, -0.78), (-hl * 0.95, -0.78), (-hl * 0.95, 0.78), (hl * 0.45 - h * 0.2, 0.78), (top_front, 0.3)];
        let b: Vec<[f32; 3]> = bottom.iter().map(|&(x, z)| [x, 0.0, z * hw]).collect();
        let t: Vec<[f32; 3]> = top.iter().map(|&(x, z)| [x, h, z * hw]).collect();
        (vec![b, t], hl, top_front)
    }
}

fn gun_turret(calibre: f32, barrels: usize, shield: bool, f: [f32; 3]) -> PartModel {
    let [fx, fy, fz] = f;
    let (cx, cz) = (fx * 0.5, fz * 0.5);
    let bore = calibre / 1000.0;
    let barrels = barrels.max(1);
    let mut kit = Kit::new();

    // Base: barbette ring with a dark training seam.
    let base_h = (fy * 0.15).clamp(0.08, 0.5);
    let rb = (fx.min(fz) * 0.45).max(0.2);
    let mut b = builder(Tone::Paint);
    b.prism_y(cx, cz, rb, 0.0, base_h, 12, true);
    set_tone(&mut b, Tone::Black);
    b.ring_y(cx, cz, rb * 0.86, rb * 0.97, base_h + 0.003, 12);
    let base = kit.piece(PieceRole::Base, None, [0.0; 3], Joint::Fixed, b.finish());

    let avail = (fy - base_h).max(0.3);
    let hl = fx * 0.47;
    let hw = fz * 0.45;
    let spacing = (bore * 7.0 + 0.08).min(2.0 * hw * 0.7 / barrels as f32);
    let zs = offsets(barrels, spacing);
    let gun_hw = (barrels as f32 - 1.0) * spacing * 0.5 + bore * 1.8 + 0.06;
    let length_vis = bore * calibres_long(calibre) * 0.85;

    let mut t = builder(Tone::Paint);
    let mut c = builder(Tone::Paint);
    let (trunnion, front_x);
    if shield {
        let h = avail.min((0.8 + bore * 9.0) * 1.15).max(0.3);
        let (rings, front_floor, front_roof) = gunhouse(calibre, hl, hw, h);
        t.loft(&rings, false, true);
        let ty = h * if calibre < 70.0 { 0.38 } else { 0.45 };
        let front_at = front_floor + (front_roof - front_floor) * (ty / h);
        let tx = (hl * 0.25).min(front_at - 0.1);
        trunnion = [tx, ty, 0.0];
        front_x = front_at - tx + 0.04;
        if calibre > 90.0 {
            // Roof sight and vent box.
            t.cuboid([-hl * 0.6, h, hw * 0.15], [-hl * 0.2, h + h * 0.12, hw * 0.45]);
        }
        // Mantlet: the elevating shield around the barrels where they leave the house.
        let mh = (0.1 + bore * 1.8).min(h * 0.25);
        c.cuboid([front_x - (0.25 + bore * 2.0), -mh, -gun_hw], [front_x, mh, gun_hw]);
    } else {
        // Open mount: turntable, trunnion cheeks and a splinter shield with a slot for the barrels.
        let ty = (0.45 + bore * 5.0).min(avail * 0.8);
        t.prism_y(0.0, 0.0, rb * 0.85, 0.0, 0.1, 12, true);
        let ct = 0.05 + bore * 0.4;
        let cheek_x = (hl * 0.35).max(0.15);
        for s in [-1.0f32, 1.0] {
            let (z0, z1) = (s * (gun_hw + 0.02), s * (gun_hw + 0.02 + ct));
            t.cuboid([-cheek_x, 0.1, z0.min(z1)], [cheek_x, ty + 0.12 + bore, z0.max(z1)]);
        }
        let xs = (hl * 0.6).max(cheek_x + 0.05);
        let sw = (gun_hw + 0.3 + bore * 3.0).min(hw * 1.1).max(gun_hw + 0.1);
        let top = ty + 0.3 + bore * 3.0;
        for s in [-1.0f32, 1.0] {
            vplate(&mut t, [xs, s * gun_hw * 0.75], [xs - 0.05, s * sw * 0.8], 0.1, top, 0.04);
            vplate(&mut t, [xs - 0.05, s * sw * 0.8], [xs - 0.2, s * sw], 0.1, top * 0.9, 0.04);
        }
        vplate(&mut t, [xs, -gun_hw * 0.8], [xs, gun_hw * 0.8], 0.1, (ty - 0.12 - bore * 1.5).max(0.15), 0.04);
        trunnion = [0.0, ty, 0.0];
        front_x = 0.15;
        // Breech block and recoil cylinders on the cradle.
        let bh = 0.08 + bore * 1.6;
        c.cuboid([-(0.3 + bore * 4.0), -bh, -gun_hw + 0.02], [0.15, bh, gun_hw - 0.02]);
        set_tone(&mut c, Tone::Metal);
        for &z in &zs {
            c.frustum([-0.1, bh + bore * 0.5, z], [bore * 10.0 + 0.15, bh + bore * 0.5, z], bore * 0.6, bore * 0.6, 6, true);
        }
    }
    let turret = kit.piece(PieceRole::Turret, Some(base), [cx, base_h, cz], Joint::Train, t.finish());
    let cradle = kit.piece(PieceRole::Cradle, Some(turret), trunnion, Joint::Elevate, c.finish());
    add_barrels(&mut kit, cradle, calibre, &zs, front_x + bore * 8.0, front_x + length_vis);
    kit.finish()
}

// ---------------------------------------------------------------------------------------------------------
// Open autocannon mount

fn gun_open(calibre: f32, barrels: usize, f: [f32; 3]) -> PartModel {
    let [fx, fy, fz] = f;
    let (cx, cz) = (fx * 0.5, fz * 0.5);
    let bore = calibre / 1000.0;
    let barrels = barrels.max(1);
    let mut kit = Kit::new();

    let y_p = (fy * 0.42).clamp(0.25, 1.1);
    let r_col = (0.08 + bore * 1.6).min(fx.min(fz) * 0.3);
    let mut b = builder(Tone::Paint);
    b.prism_y(cx, cz, (fx.min(fz) * 0.32).max(r_col * 1.4), 0.0, 0.06, 8, true);
    b.prism_y(cx, cz, r_col, 0.06, y_p, 8, true);
    let base = kit.piece(PieceRole::Base, None, [0.0; 3], Joint::Fixed, b.finish());

    let spacing = bore * 5.0 + 0.08;
    let zs = offsets(barrels, spacing);
    let gun_hw = (barrels as f32 - 1.0) * spacing * 0.5 + bore * 1.2 + 0.04;
    let t_h = 0.22 + bore * 5.0;
    let cheek_x = 0.12 + bore * 2.0;
    let mut t = builder(Tone::Paint);
    t.cuboid([-cheek_x * 1.4, 0.0, -(gun_hw + 0.06)], [cheek_x * 1.2, 0.06, gun_hw + 0.06]);
    for s in [-1.0f32, 1.0] {
        let (z0, z1) = (s * (gun_hw + 0.01), s * (gun_hw + 0.05));
        t.cuboid([-cheek_x, 0.06, z0.min(z1)], [cheek_x, t_h + 0.06, z0.max(z1)]);
    }
    if calibre <= 40.0 {
        // Splinter shield in two wings either side of the barrel slot.
        let xs = cheek_x + 0.08 + bore * 3.0;
        let sw = gun_hw + 0.2 + bore * 4.0;
        for s in [-1.0f32, 1.0] {
            vplate(&mut t, [xs, s * gun_hw * 0.6], [xs - 0.12, s * sw], 0.08, t_h + 0.3, 0.025);
        }
    }
    let turret = kit.piece(PieceRole::Turret, Some(base), [cx, y_p, cz], Joint::Train, t.finish());

    let rh = 0.05 + bore * 1.4;
    let mut c = builder(Tone::Paint);
    c.cuboid([-(0.25 + bore * 6.0), -rh, -(gun_hw - 0.01)], [0.1 + bore, rh, gun_hw - 0.01]);
    if calibre <= 30.0 {
        // Drum magazine on top of each receiver.
        set_tone(&mut c, Tone::Metal);
        for &z in &zs { c.prism_y(-0.05 - bore * 2.0, z, 0.08 + bore * 2.0, rh, rh + 0.07 + bore, 10, true); }
    } else {
        // Ready-use ammunition box on the port side.
        c.cuboid([-(0.2 + bore * 4.0), -rh * 0.8, -(gun_hw + 0.12 + bore * 2.0)], [0.0, rh * 1.2, -(gun_hw + 0.005)]);
    }
    let cradle = kit.piece(PieceRole::Cradle, Some(turret), [0.0, t_h, 0.0], Joint::Elevate, c.finish());
    add_barrels(&mut kit, cradle, calibre, &zs, 0.1 + bore * 4.0, 0.1 + bore * calibres_long(calibre) * 0.9);
    kit.finish()
}

// ---------------------------------------------------------------------------------------------------------
// Close-in weapon system

fn ciws(calibre: f32, barrels: usize, radome: bool, f: [f32; 3]) -> PartModel {
    let [fx, fy, fz] = f;
    let (cx, cz) = (fx * 0.5, fz * 0.5);
    let bore = calibre / 1000.0;
    let barrels = barrels.max(1);
    let r = (fx.min(fz) * 0.45).max(0.3);
    let mut kit = Kit::new();

    let base_h = (fy * 0.22).clamp(0.15, 0.8);
    let mut b = builder(Tone::Paint);
    b.prism_y(cx, cz, r * 0.85, 0.0, base_h - 0.05, 10, true);
    b.prism_y(cx, cz, r * 0.95, base_h - 0.05, base_h, 10, true);
    let base = kit.piece(PieceRole::Base, None, [0.0; 3], Joint::Fixed, b.finish());

    let avail = (fy - base_h).max(0.6);
    let t_h = (avail * 0.35).max(0.3);
    let above = (avail - t_h).max(0.3);
    let rc = bore * 2.0 + 0.02;
    let rc_out = rc + bore * 1.5;
    let gun_hw = r * 0.45;
    let mut t = builder(Tone::Paint);
    t.prism_y(0.0, 0.0, r * 0.75, 0.0, 0.12, 10, true);
    for s in [-1.0f32, 1.0] {
        let (z0, z1) = (s * (gun_hw + 0.02), s * (gun_hw + 0.1));
        t.cuboid([-r * 0.3, 0.12, z0.min(z1)], [r * 0.3, t_h + rc_out, z0.max(z1)]);
    }
    let turret = kit.piece(PieceRole::Turret, Some(base), [cx, base_h, cz], Joint::Train, t.finish());

    let mut c = builder(Tone::Paint);
    if radome {
        // Tall dome above the gun (fire-control radar inside), ammunition drum housing under it.
        let y_d0 = rc_out + 0.05;
        let rd = (r * 0.55).min(((above - y_d0) * 0.5).max(0.1));
        let y_d1 = (y_d0 + rd * 2.2).max(y_d0 + 0.15);
        c.cuboid([-r * 0.5, -rc_out - 0.05, -gun_hw * 0.85], [r * 0.15, y_d0 + 0.02, gun_hw * 0.85]);
        set_tone(&mut c, Tone::Pale);
        let xd = -r * 0.15;
        let n = 10;
        let st = core::f32::consts::PI / n as f32;
        let rings = vec![
            ellipse_ring(xd, y_d0, 0.0, rd, rd, n, st),
            ellipse_ring(xd, y_d1 - rd * 0.9, 0.0, rd, rd, n, st),
            ellipse_ring(xd, y_d1 - rd * 0.45, 0.0, rd * 0.87, rd * 0.87, n, st),
            ellipse_ring(xd, y_d1 - rd * 0.12, 0.0, rd * 0.5, rd * 0.5, n, st),
            ellipse_ring(xd, y_d1, 0.0, rd * 0.2, rd * 0.2, n, st),
        ];
        c.loft(&rings, true, true);
    } else {
        // Compact faceted housing behind the cluster with a tracking radar on top.
        let radar_h = r * 0.25;
        let y0 = -(rc_out + 0.1);
        let y1 = (rc_out + r * 0.6).min(above - radar_h).max(rc_out + 0.05);
        let bottom = vec![[r * 0.25, y0, -gun_hw], [-r * 0.55, y0, -gun_hw], [-r * 0.55, y0, gun_hw], [r * 0.25, y0, gun_hw]];
        let top = vec![[r * 0.05, y1, -gun_hw * 0.85], [-r * 0.5, y1, -gun_hw * 0.85], [-r * 0.5, y1, gun_hw * 0.85], [r * 0.05, y1, gun_hw * 0.85]];
        c.loft(&[bottom, top], true, true);
        set_tone(&mut c, Tone::Pale);
        c.cuboid([-r * 0.15, y1, -r * 0.18], [r * 0.15, y1 + radar_h, r * 0.18]);
    }
    let cradle = kit.piece(PieceRole::Cradle, Some(turret), [0.0, t_h, 0.0], Joint::Elevate, c.finish());

    // Rotating barrel cluster: rear housing, mid and muzzle clamps, barrels on a circle.
    let length = bore * calibres_long(calibre);
    let mut g = builder(Tone::Paint);
    g.frustum([-0.3, 0.0, 0.0], [0.05, 0.0, 0.0], rc + bore * 1.5, rc + bore * 1.5, 10, true);
    g.frustum([length * 0.55, 0.0, 0.0], [length * 0.6, 0.0, 0.0], rc + bore, rc + bore, 8, true);
    g.frustum([length * 0.92, 0.0, 0.0], [length * 0.96, 0.0, 0.0], rc + bore, rc + bore, 8, true);
    set_tone(&mut g, Tone::Metal);
    for k in 0..barrels {
        let a = core::f32::consts::TAU * k as f32 / barrels as f32;
        let (y, z) = (rc * libm::cosf(a), rc * libm::sinf(a));
        g.frustum([0.05, y, z], [length, y, z], bore * 0.8, bore * 0.8, 6, true);
    }
    let cluster = kit.piece(PieceRole::Barrel, Some(cradle), [r * 0.25, 0.0, 0.0], Joint::Spin { axis: [1.0, 0.0, 0.0] }, g.finish());
    kit.anchor(AnchorKind::Muzzle, cluster, [length + 0.01, 0.0, 0.0], [1.0, 0.0, 0.0]);
    kit.finish()
}

// ---------------------------------------------------------------------------------------------------------
// Box launcher (canister racks)

/// Canister pack in its own frame: canisters along +X from x = 0 to `len`, bottom row on y = 0, centred on
/// z = 0. Returns the mouth centres (pack frame).
fn canister_pack(b: &mut MeshBuilder, cells: usize, len: f32, d: f32) -> Vec<[f32; 3]> {
    let rows = if cells <= 2 { 1 } else { 2 };
    let cols = cells.div_ceil(rows);
    let pitch = d * 1.12;
    let h = d * 0.5;
    let mut mouths = Vec::with_capacity(cells);
    set_tone(b, Tone::Paint);
    for k in 0..cells {
        let (j, i) = (k / cols, k % cols);
        let in_row = cols.min(cells - j * cols);
        let y = h + j as f32 * pitch;
        let z = (i as f32 - (in_row as f32 - 1.0) * 0.5) * pitch;
        b.cuboid([0.0, y - h, z - h], [len, y + h, z + h]);
        mouths.push([len + 0.02, y, z]);
    }
    // Pack frame bands front and rear.
    let half_w = (cols as f32 - 1.0) * 0.5 * pitch + h + 0.03;
    let top = (rows as f32 - 1.0) * pitch + 2.0 * h + 0.03;
    for (x0, x1) in [(len * 0.06, len * 0.06 + 0.08), (len * 0.9, len * 0.9 + 0.08)] {
        b.cuboid([x0, -0.03, -half_w], [x1, top, half_w]);
    }
    // Front covers mark the cell mouths.
    set_tone(b, Tone::Metal);
    for m in &mouths { b.cuboid([len, m[1] - h * 0.85, m[2] - h * 0.85], [len + 0.02, m[1] + h * 0.85, m[2] + h * 0.85]); }
    set_tone(b, Tone::Paint);
    mouths
}

fn launcher_box(cells: usize, cell_length: f32, cell_diameter: f32, trainable: bool, elevation_deg: f32, f: [f32; 3]) -> PartModel {
    let [fx, fy, fz] = f;
    let (cx, cz) = (fx * 0.5, fz * 0.5);
    let cells = cells.max(1);
    let rows = if cells <= 2 { 1 } else { 2 };
    let cols = cells.div_ceil(rows);
    let e = elevation_deg * DEG;
    let (ce, se) = (libm::cosf(e), libm::sinf(e));
    let d = cell_diameter.min(fz * 0.95 / (cols as f32 * 1.12)).max(0.15);
    let len = cell_length.min(fx * 0.98 / ce.max(0.3)).max(0.8);
    let pack_h = (rows as f32 - 1.0) * d * 1.12 + d;
    let pack_hw = (cols as f32 - 1.0) * 0.5 * d * 1.12 + d * 0.5 + 0.03;
    let mut kit = Kit::new();
    let tilt = Xform::rot_z(e);

    if !trainable {
        // Fixed rack: canisters on a stand, rear low, front raised by the elevation.
        let y_s = 0.15;
        let x0 = cx - len * ce * 0.5 + pack_h * se * 0.5;
        let place = Xform::translation([x0, y_s, cz]).then(&tilt);
        let mut b = builder(Tone::Paint);
        let mut mouths = Vec::new();
        b.with(place, |b| mouths = canister_pack(b, cells, len, d));
        let post = 0.03 + d * 0.04;
        for xl in [len * 0.15, len * 0.75] {
            for s in [-1.0f32, 1.0] {
                let top = place.apply([xl, -0.03, s * (pack_hw - post)]);
                b.beam([top[0], 0.0, top[2]], top, post);
            }
        }
        let foot_a = place.apply([len * 0.15, 0.0, 0.0])[0];
        let foot_b = place.apply([len * 0.75, 0.0, 0.0])[0];
        for s in [-1.0f32, 1.0] {
            let z = cz + s * (pack_hw - post);
            b.cuboid([foot_a - 0.1, 0.0, z - post * 1.5], [foot_b + 0.1, 0.05, z + post * 1.5]);
        }
        let base = kit.piece(PieceRole::Base, None, [0.0; 3], Joint::Fixed, b.finish());
        for m in mouths { kit.anchor(AnchorKind::LaunchCell, base, place.apply(m), [ce, se, 0.0]); }
    } else {
        let r_p = (fx.min(fz) * 0.3).max(0.2);
        let y_p = (fy * 0.25).clamp(0.2, 0.9);
        let mut b = builder(Tone::Paint);
        b.prism_y(cx, cz, r_p, 0.0, y_p, 10, true);
        let base = kit.piece(PieceRole::Base, None, [0.0; 3], Joint::Fixed, b.finish());
        let t_h = 0.35 + pack_h * 0.5;
        let mut t = builder(Tone::Paint);
        t.prism_y(0.0, 0.0, r_p * 1.1, 0.0, 0.1, 10, true);
        for s in [-1.0f32, 1.0] {
            let (z0, z1) = (s * (pack_hw + 0.01), s * (pack_hw + 0.09));
            t.cuboid([-0.25, 0.1, z0.min(z1)], [0.25, t_h + 0.12, z0.max(z1)]);
        }
        let turret = kit.piece(PieceRole::Turret, Some(base), [cx, y_p, cz], Joint::Train, t.finish());
        let place = tilt.then(&Xform::translation([-len * 0.5, -pack_h * 0.5, 0.0]));
        let mut c = builder(Tone::Paint);
        let mut mouths = Vec::new();
        c.with(place, |c| mouths = canister_pack(c, cells, len, d));
        let cradle = kit.piece(PieceRole::Cradle, Some(turret), [0.0, t_h, 0.0], Joint::Fixed, c.finish());
        for m in mouths { kit.anchor(AnchorKind::LaunchCell, cradle, place.apply(m), [ce, se, 0.0]); }
    }
    kit.finish()
}

// ---------------------------------------------------------------------------------------------------------
// Vertical launch module

fn launcher_vls(columns: usize, rows: usize, cell: f32, f: [f32; 3]) -> PartModel {
    let [fx, fy, fz] = f;
    let (cx, cz) = (fx * 0.5, fz * 0.5);
    let (columns, rows) = (columns.max(1), rows.max(1));
    let gap_frac = if columns >= 2 && columns % 2 == 0 { 0.4 } else { 0.0 };
    let pitch = cell.min((fx - 0.1).max(0.1) / rows as f32).min((fz - 0.1).max(0.1) / (columns as f32 + gap_frac)).max(0.01);
    let gap = gap_frac * pitch;
    let (gx, gz) = (rows as f32 * pitch, columns as f32 * pitch + gap);
    let (x0, z0) = (cx - gx * 0.5, cz - gz * 0.5);
    let m = ((fx - gx) * 0.5).min((fz - gz) * 0.5).clamp(0.0, 0.08);
    let top = fy;
    let mut b = builder(Tone::Paint);
    b.cuboid([x0 - m, 0.0, z0 - m], [x0 + gx + m, top - 0.04, z0 + gz + m]);
    set_tone(&mut b, Tone::Black);
    b.cuboid([x0, top - 0.04, z0], [x0 + gx, top - 0.01, z0 + gz]);
    let mut lids = Vec::with_capacity(rows * columns);
    set_tone(&mut b, Tone::Paint);
    let half = pitch * 0.43;
    for r in 0..rows {
        for c in 0..columns {
            let lx = x0 + (r as f32 + 0.5) * pitch;
            let lz = z0 + (c as f32 + 0.5) * pitch + if gap > 0.0 && c >= columns / 2 { gap } else { 0.0 };
            b.cuboid([lx - half, top - 0.03, lz - half], [lx + half, top + 0.02, lz + half]);
            lids.push([lx, top + 0.02, lz]);
        }
    }
    if gap > 0.0 {
        // Exhaust uptake hatch between the two halves.
        let zc = z0 + (columns / 2) as f32 * pitch + gap * 0.5;
        set_tone(&mut b, Tone::Metal);
        b.cuboid([x0 + pitch * 0.07, top - 0.03, zc - gap * 0.35], [x0 + gx - pitch * 0.07, top + 0.01, zc + gap * 0.35]);
    }
    let mut kit = Kit::new();
    let base = kit.piece(PieceRole::Base, None, [0.0; 3], Joint::Fixed, b.finish());
    for l in lids { kit.anchor(AnchorKind::LaunchCell, base, l, [0.0, 1.0, 0.0]); }
    kit.finish()
}

// ---------------------------------------------------------------------------------------------------------
// Torpedo tubes

/// Tube centres (y, z) relative to the bottom-centre of the nest, for `n` tubes of outer radius `r`.
fn tube_layout(n: usize, r: f32) -> Vec<(f32, f32)> {
    let p = 2.0 * r * 1.08;
    match n {
        0 | 1 => vec![(r, 0.0)],
        2 => vec![(r, -p * 0.5), (r, p * 0.5)],
        3 => vec![(r, -p * 0.5), (r, p * 0.5), (r + p * 0.87, 0.0)],
        _ => vec![(r, -p * 0.5), (r, p * 0.5), (r + p, -p * 0.5), (r + p, p * 0.5)],
    }
}

/// Tubes along X centred on x = 0, nest bottom at `y0`; returns the mouth centres.
fn tube_nest(b: &mut MeshBuilder, n: usize, len: f32, r_in: f32, r_out: f32, y0: f32) -> Vec<[f32; 3]> {
    let mut mouths = Vec::new();
    for (y, z) in tube_layout(n, r_out) {
        let y = y0 + y;
        set_tone(b, Tone::Paint);
        b.frustum([-len * 0.5, y, z], [len * 0.5, y, z], r_out, r_out, 10, true);
        b.frustum([-len * 0.5 - 0.12, y, z], [-len * 0.5 + 0.15, y, z], r_out * 1.18, r_out * 1.18, 10, true);
        b.frustum([len * 0.5 - 0.1, y, z], [len * 0.5, y, z], r_out * 1.1, r_out * 1.1, 10, true);
        set_tone(b, Tone::Black);
        disc_x(b, [len * 0.5 + 0.003, y, z], r_in, 10);
        mouths.push([len * 0.5 + 0.003, y, z]);
    }
    set_tone(b, Tone::Paint);
    mouths
}

fn torpedo_tubes(tubes: usize, length: f32, diameter: f32, trainable: bool, f: [f32; 3]) -> PartModel {
    let [fx, fy, fz] = f;
    let (cx, cz) = (fx * 0.5, fz * 0.5);
    let tubes = tubes.clamp(1, 4);
    let r_in = diameter * 0.5;
    let r_out = r_in + 0.03 + diameter * 0.05;
    let len = length.min(fx * 0.98).max(1.0);
    let wide = if tubes >= 2 { r_out * 2.08 } else { r_out };
    let mut kit = Kit::new();
    if trainable {
        let r_p = (fz * 0.3).clamp(0.15, 0.5);
        let y_p = (fy * 0.3).clamp(0.25, 0.9);
        let mut b = builder(Tone::Paint);
        b.prism_y(cx, cz, r_p, 0.0, y_p, 8, true);
        let base = kit.piece(PieceRole::Base, None, [0.0; 3], Joint::Fixed, b.finish());
        let mut t = builder(Tone::Paint);
        t.cuboid([-len * 0.25, 0.0, -wide], [len * 0.25, 0.12, wide]);
        for x in [-len * 0.2, len * 0.2] {
            t.cuboid([x - 0.06, 0.0, -wide - 0.03], [x + 0.06, 0.12 + r_out * 1.2, wide + 0.03]);
        }
        let mouths = tube_nest(&mut t, tubes, len, r_in, r_out, 0.12);
        let turret = kit.piece(PieceRole::Turret, Some(base), [cx, y_p, cz], Joint::Train, t.finish());
        for m in mouths { kit.anchor(AnchorKind::LaunchCell, turret, m, [1.0, 0.0, 0.0]); }
    } else {
        let y_c = (fy * 0.15).max(0.15);
        let mut b = builder(Tone::Paint);
        let mut mouths = Vec::new();
        b.with(Xform::translation([cx, 0.0, cz]), |b| {
            for x in [-len * 0.3, len * 0.3] { b.cuboid([x - 0.08, 0.0, -wide], [x + 0.08, y_c + r_out * 0.6, wide]); }
            mouths = tube_nest(b, tubes, len, r_in, r_out, y_c);
        });
        let base = kit.piece(PieceRole::Base, None, [0.0; 3], Joint::Fixed, b.finish());
        for m in mouths { kit.anchor(AnchorKind::LaunchCell, base, [m[0] + cx, m[1], m[2] + cz], [1.0, 0.0, 0.0]); }
    }
    kit.finish()
}

// ---------------------------------------------------------------------------------------------------------
// Depth-charge rack (Act 3)

fn depth_charge_rack(charges: usize, f: [f32; 3]) -> PartModel {
    let [fx, fy, fz] = f;
    let cz = fz * 0.5;
    let n = charges.max(1);
    let (x_lo, x_hi) = (0.05f32, (fx - 0.05).max(0.3));
    let span = x_hi - x_lo;
    let s = (span * 0.92 / n as f32).min(0.5);
    let r = (0.225f32).min(s * 0.48).min(fz * 0.35);
    let ld = (0.6f32).min(fz * 0.8);
    let y_hi = (fy * 0.6).clamp(0.4, 1.4);
    let y_lo = y_hi - (span * 0.08).min(0.4);
    let rail_y = |x: f32| y_lo + (y_hi - y_lo) * (x - x_lo) / span;
    let mut b = builder(Tone::Paint);
    for side in [-1.0f32, 1.0] {
        let z = cz + side * ld * 0.4;
        b.beam([x_lo, y_lo, z], [x_hi, y_hi, z], 0.03);
        let zg = cz + side * (ld * 0.5 + 0.04);
        b.beam([x_lo, y_lo + r * 1.3, zg], [x_hi, y_hi + r * 1.3, zg], 0.025);
        let posts = ((span / 1.2) as usize).clamp(1, 12) + 1;
        for k in 0..posts {
            let x = x_lo + span * k as f32 / (posts - 1) as f32;
            b.beam([x, 0.0, zg], [x, rail_y(x) + r * 1.3, zg], 0.03);
            b.strut([x, rail_y(x) - 0.03, z], [x, rail_y(x) - 0.03, zg], 0.025);
        }
    }
    // Release gate at the aft end.
    b.cuboid([x_lo - 0.03, y_lo, cz - ld * 0.45], [x_lo + 0.03, y_lo + r * 0.8, cz + ld * 0.45]);
    set_tone(&mut b, Tone::Metal);
    for k in 0..n {
        let x = x_lo + 0.06 + s * (k as f32 + 0.5);
        let y = rail_y(x) + r + 0.03;
        b.frustum([x, y, cz - ld * 0.5], [x, y, cz + ld * 0.5], r, r, 10, true);
    }
    let mut kit = Kit::new();
    let base = kit.piece(PieceRole::Base, None, [0.0; 3], Joint::Fixed, b.finish());
    kit.anchor(AnchorKind::LaunchCell, base, [x_lo - 0.05, y_lo + r, cz], [-1.0, -0.4, 0.0]);
    kit.finish()
}

// ---------------------------------------------------------------------------------------------------------
// Decoy launcher (chaff, flare and smoke rounds)

fn decoy_launcher(barrels: usize, elevation_deg: f32, f: [f32; 3]) -> PartModel {
    let [fx, fy, fz] = f;
    let (cx, cz) = (fx * 0.5, fz * 0.5);
    let n = barrels.max(1);
    let rows = if n >= 4 { 2 } else { 1 };
    let per_row = [n.div_ceil(rows), n - n.div_ceil(rows)];
    let k = (fz * 0.9 / (per_row[0] as f32 * 0.2)).clamp(0.3, 1.0);
    let (r_in, r_out) = (0.065 * k, 0.085 * k);
    let len = (1.2f32).min(fx * 0.7).max(0.5);
    let pitch = r_out * 2.3;
    let y_b = (fy * 0.35).clamp(0.2, 0.7);
    let w = per_row[0] as f32 * pitch * 0.5 + 0.05;
    let mut b = builder(Tone::Paint);
    b.cuboid([cx - 0.3, 0.0, cz - w], [cx + 0.15, y_b, cz + w]);
    b.cuboid([cx - 0.4, 0.0, cz - w - 0.05], [cx + 0.25, 0.04, cz + w + 0.05]);
    let mut cells = Vec::new();
    for (row, &cnt) in per_row.iter().enumerate().take(rows) {
        // Upper row steeper, as on fixed multi-barrel decoy launchers.
        let e = (elevation_deg + row as f32 * 15.0).min(80.0) * DEG;
        for i in 0..cnt {
            let off = i as f32 - (cnt as f32 - 1.0) * 0.5;
            let az = off * 10.0 * DEG;
            let root = [cx - 0.1 - row as f32 * 0.12, y_b + 0.02 + row as f32 * r_out * 2.2, cz + off * pitch];
            let place = Xform::translation(root).then(&Xform::rot_y(az)).then(&Xform::rot_z(e));
            b.with(place, |b| {
                set_tone(b, Tone::Paint);
                b.frustum([-0.05, 0.0, 0.0], [len, 0.0, 0.0], r_out, r_out, 8, true);
                set_tone(b, Tone::Black);
                disc_x(b, [len + 0.003, 0.0, 0.0], r_in, 8);
            });
            cells.push((place.apply([len + 0.003, 0.0, 0.0]), place.apply_vec([1.0, 0.0, 0.0])));
        }
    }
    let mut kit = Kit::new();
    let base = kit.piece(PieceRole::Base, None, [0.0; 3], Joint::Fixed, b.finish());
    for (p, d) in cells { kit.anchor(AnchorKind::LaunchCell, base, p, d); }
    kit.finish()
}

// ---------------------------------------------------------------------------------------------------------
// Mine rail

fn mine_rail(length_cells: f32, f: [f32; 3]) -> PartModel {
    let [fx, _fy, fz] = f;
    let cz = fz * 0.5;
    let len = (length_cells * 0.5).min(fx).max(0.5);
    let gauge = (0.6f32).min(fz * 0.7).max(0.2);
    let k = (gauge / 0.6).min(1.0);
    let mut b = builder(Tone::Metal);
    for s in [-1.0f32, 1.0] { b.beam([0.0, 0.05, cz + s * gauge * 0.5], [len, 0.05, cz + s * gauge * 0.5], 0.035); }
    let sleepers = ((len / 2.0) as usize).max(1) + 1;
    let half_sleeper = (gauge * 0.5 + 0.1).min(fz * 0.5);
    for i in 0..sleepers {
        let x = (len - 0.1).max(0.0) * i as f32 / (sleepers - 1) as f32 + 0.05;
        b.cuboid([x - 0.06, 0.0, cz - half_sleeper], [x + 0.06, 0.03, cz + half_sleeper]);
    }
    let mines = ((len / 1.1) as usize).clamp(1, 18);
    let spacing = len / mines as f32;
    let k = k.min(spacing / 1.1);
    let n = 8;
    let st = core::f32::consts::PI / n as f32;
    for i in 0..mines {
        let x = (i as f32 + 0.5) * spacing;
        set_tone(&mut b, Tone::Paint);
        let y_s = 0.08 + 0.42 * k;
        b.cuboid([x - 0.32 * k, 0.08, cz - 0.3 * k], [x + 0.32 * k, y_s, cz + 0.3 * k]);
        set_tone(&mut b, Tone::Metal);
        let yc = y_s + 0.42 * k;
        let rings = vec![
            ellipse_ring(x, yc - 0.4 * k, cz, 0.22 * k, 0.22 * k, n, st),
            ellipse_ring(x, yc - 0.2 * k, cz, 0.4 * k, 0.4 * k, n, st),
            ellipse_ring(x, yc + 0.2 * k, cz, 0.4 * k, 0.4 * k, n, st),
            ellipse_ring(x, yc + 0.4 * k, cz, 0.22 * k, 0.22 * k, n, st),
        ];
        b.loft(&rings, true, true);
        set_tone(&mut b, Tone::Black);
        b.frustum([x, yc + 0.4 * k, cz], [x, yc + 0.56 * k, cz], 0.045 * k, 0.045 * k, 4, true);
    }
    let mut kit = Kit::new();
    let base = kit.piece(PieceRole::Base, None, [0.0; 3], Joint::Fixed, b.finish());
    kit.anchor(AnchorKind::LaunchCell, base, [0.0, 0.5 * k, cz], [-1.0, -0.5, 0.0]);
    kit.finish()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parts::{build_model, Family, GENERATORS};
    use crate::prim::tests::check_consistent;
    use content::ids::ModelId;
    use content::part::ProcModel;

    fn weapons() -> Vec<Generator> { GENERATORS.iter().filter(|s| s.family == Family::Weapons).map(|s| s.generator).collect() }

    /// Parameter sets at the minimum, default and maximum of every range.
    fn param_sets(g: Generator) -> Vec<Vec<f32>> {
        let ps = g.spec().params;
        vec![ps.iter().map(|p| p.min).collect(), ps.iter().map(|p| p.default).collect(), ps.iter().map(|p| p.max).collect()]
    }

    fn tris(m: &PartModel) -> usize { m.pieces.iter().map(|p| p.mesh.triangle_count()).sum() }

    pub(super) fn check_model(m: &PartModel) {
        assert!(!m.pieces.is_empty());
        for (i, p) in m.pieces.iter().enumerate() {
            if let Some(q) = p.parent { assert!((q as usize) < i, "parent after child"); } else { assert_eq!(i, 0, "only the base is a root"); }
            assert!(!p.mesh.is_empty(), "empty piece {i} ({:?})", p.role);
            check_consistent(&p.mesh);
            for c in &p.mesh.colors { assert!(c.iter().all(|v| (0.0..=1.0).contains(v))); }
        }
        for a in &m.anchors {
            assert!((a.piece as usize) < m.pieces.len());
            assert!((a.dir.iter().map(|v| v * v).sum::<f32>() - 1.0).abs() < 1e-4);
            assert!(a.pos.iter().all(|v| v.is_finite()));
        }
        assert!(!m.aabb.is_empty());
        assert!(tris(m) < 2000, "{} triangles", tris(m));
    }

    #[test]
    fn every_weapon_builds_within_budget_at_range_extremes() {
        for g in weapons() {
            for params in param_sets(g) {
                for fp in [[1, 1, 1], [4, 3, 4], [8, 4, 6], [40, 6, 12], [0, 0, 0], [255, 255, 255]] {
                    check_model(&build(g, &params, fp));
                }
            }
        }
    }

    #[test]
    fn guns_are_articulated_with_one_muzzle_per_barrel() {
        for (g, cal, n) in [(Generator::GunTurret, 76.0, 1.0), (Generator::GunTurret, 155.0, 3.0), (Generator::GunTurret, 127.0, 2.0), (Generator::GunOpen, 20.0, 2.0), (Generator::GunOpen, 57.0, 1.0)] {
            for shield in [0.0, 1.0] {
                let m = build(g, &[cal, n, shield], [4, 3, 4]);
                let role = |r: PieceRole| m.pieces.iter().filter(|p| p.role == r).count();
                assert_eq!(role(PieceRole::Base), 1);
                let t = m.pieces.iter().position(|p| p.role == PieceRole::Turret).unwrap();
                let c = m.pieces.iter().position(|p| p.role == PieceRole::Cradle).unwrap();
                assert_eq!(m.pieces[t].joint, Joint::Train);
                assert_eq!(m.pieces[c].joint, Joint::Elevate);
                assert_eq!(m.pieces[c].parent, Some(t as u8));
                let barrels: Vec<_> = m.pieces.iter().filter(|p| p.role == PieceRole::Barrel).collect();
                assert_eq!(barrels.len(), n as usize);
                assert!(barrels.iter().all(|b| matches!(b.joint, Joint::Recoil { travel_m } if travel_m > 0.0) && b.parent == Some(c as u8)));
                assert_eq!(m.anchors.iter().filter(|a| a.kind == AnchorKind::Muzzle).count(), n as usize);
            }
        }
    }

    #[test]
    fn barrels_lengthen_with_calibre() {
        let reach = |cal: f32| build(Generator::GunTurret, &[cal, 1.0, 1.0], [6, 4, 6]).aabb.max[0];
        assert!(reach(57.0) < reach(76.0) && reach(76.0) < reach(127.0) && reach(127.0) < reach(155.0));
        // The 76 mm barrel is about 62 calibres (4.7 m), most of it outside the house.
        let m = build(Generator::GunTurret, &[76.0, 1.0, 1.0], [4, 3, 4]);
        assert!(m.aabb.max[0] > 4.0 && m.aabb.max[0] < 6.5, "76 mm reach {}", m.aabb.max[0]);
        assert!((calibres_long(76.0) - 62.0).abs() < 1e-4 && (calibres_long(155.0) - 52.0).abs() < 1e-4);
        assert!((calibres_long(12.0) - 80.0).abs() < 1e-4 && (calibres_long(30.0) - 75.0).abs() < 1e-4);
    }

    #[test]
    fn gunhouse_styles_have_distinct_proportions() {
        // Height over length of the turret piece: the wedge is low, the stealth house tall.
        let ratio = |cal: f32| {
            let m = build(Generator::GunTurret, &[cal, 1.0, 1.0], [6, 6, 5]);
            let t = m.pieces[1].mesh.aabb();
            (t.max[1] - t.min[1]) / (t.max[0] - t.min[0])
        };
        assert!(ratio(57.0) < ratio(127.0));
    }

    #[test]
    fn ciws_cluster_spins_and_has_one_muzzle() {
        for radome in [0.0, 1.0] {
            let m = build(Generator::Ciws, &[20.0, 6.0, radome], [4, 6, 4]);
            let cluster = m.pieces.iter().find(|p| p.role == PieceRole::Barrel).unwrap();
            assert_eq!(cluster.joint, Joint::Spin { axis: [1.0, 0.0, 0.0] });
            assert_eq!(m.anchors.iter().filter(|a| a.kind == AnchorKind::Muzzle).count(), 1);
        }
    }

    #[test]
    fn launchers_have_one_launch_cell_per_cell() {
        let cells = |m: &PartModel| m.anchors.iter().filter(|a| a.kind == AnchorKind::LaunchCell).count();
        for n in 1..=8 {
            for tr in [0.0, 1.0] {
                let m = build(Generator::LauncherBox, &[n as f32, 6.0, 0.6, tr, 20.0], [12, 4, 6]);
                assert_eq!(cells(&m), n);
                for a in &m.anchors { assert!((a.dir[1] - libm::sinf(20.0 * DEG)).abs() < 1e-4); }
            }
        }
        let m = build(Generator::LauncherVls, &[2.0, 4.0, 0.6], [6, 8, 4]);
        assert_eq!(cells(&m), 8);
        assert!(m.anchors.iter().all(|a| a.dir == [0.0, 1.0, 0.0]));
        assert_eq!(cells(&build(Generator::LauncherVls, &[8.0, 8.0, 1.0], [8, 8, 8])), 64);
        for n in 1..=4 {
            for tr in [0.0, 1.0] { assert_eq!(cells(&build(Generator::TorpedoTubes, &[n as f32, 6.5, 0.53, tr], [14, 3, 4])), n); }
        }
        for n in 2..=12 { assert_eq!(cells(&build(Generator::DecoyLauncher, &[n as f32, 45.0], [3, 3, 3])), n); }
        assert_eq!(cells(&build(Generator::MineRail, &[12.0], [12, 2, 3])), 1);
        assert_eq!(cells(&build(Generator::DepthChargeRack, &[6.0], [8, 3, 3])), 1);
    }

    #[test]
    fn deck_fittings_stay_inside_their_footprint() {
        let inside = |m: &PartModel, fp: [u8; 3]| {
            let f = foot_m(fp);
            let a = m.aabb;
            a.min[0] >= -0.06 && a.min[2] >= -1e-3 && a.max[0] <= f[0] + 0.06 && a.max[2] <= f[2] + 1e-3 && a.min[1] >= -1e-3
        };
        for (g, params, fp) in [
            (Generator::LauncherVls, vec![2.0, 4.0, 0.6], [6u8, 8, 4]),
            (Generator::LauncherVls, vec![8.0, 8.0, 1.0], [4, 4, 4]),
            (Generator::MineRail, vec![40.0], [24, 2, 3]),
            (Generator::DepthChargeRack, vec![12.0], [8, 3, 3]),
            (Generator::LauncherBox, vec![4.0, 6.0, 0.6, 0.0, 0.0], [12, 4, 6]),
        ] {
            let m = build(g, &params, fp);
            assert!(inside(&m, fp), "{g:?} {:?} outside {fp:?}", m.aabb);
        }
    }

    #[test]
    fn registry_path_and_bad_direct_input() {
        let m = build_model(&ProcModel { generator: ModelId::new("gun.turret"), params: vec![76.0, 1.0] }, [4, 3, 4]).unwrap();
        check_model(&m);
        // Direct calls with junk parameters, too few parameters or a foreign generator never panic.
        check_model(&build(Generator::GunTurret, &[f32::NAN, -5.0, 9.0], [4, 3, 4]));
        check_model(&build(Generator::LauncherVls, &[], [0, 0, 0]));
        check_model(&build(Generator::MastPole, &[1.0], [2, 2, 2]));
    }

    #[test]
    fn models_are_deterministic() {
        for g in weapons() { assert_eq!(build(g, &[], [6, 4, 6]), build(g, &[], [6, 4, 6])); }
    }

    /// Every weapon and sensor row of the embedded catalogue builds through the registry path: explicit
    /// generator rows must name a known generator inside its documented ranges, and rows without one take a
    /// default model that always resolves.
    #[test]
    fn every_catalogue_weapon_and_sensor_row_builds() {
        let cat = content::catalogue::catalogue();
        let mut built = 0;
        for (id, def) in &cat.parts {
            let model = if def.model.generator.as_str().is_empty() { crate::parts::default_model(def) } else { def.model.clone() };
            let Some(g) = Generator::from_key(model.generator.as_str()) else { panic!("{id:?}: unknown generator {:?}", model.generator) };
            if g.spec().family == Family::Fittings { continue; }
            let m = crate::parts::build_part_model(def).unwrap_or_else(|e| panic!("{id:?}: {e}"));
            check_model(&m);
            built += 1;
        }
        assert!(built > 0, "catalogue holds no weapon or sensor parts");
    }
}
