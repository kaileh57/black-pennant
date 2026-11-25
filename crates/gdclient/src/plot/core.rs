//! Part of unit `gd-plot`. The plot's geometry with no Godot in it: the map to local transform, scale steps
//! and zoom, reference spacing, symbol outlines (the exact geometry of the `stance_*` icons), uncertainty
//! outlines and sample points, label declutter and hit testing. Tests run headless.
//!
//! Coordinates: map positions are world metres (x east, z south) as `DVec2(x, z)`; local positions are the
//! control's pixels at 100% UI scale, x right and y down, so north is up and a bearing of 090 points to +x.
// Client presentation, not simulation (clippy.toml): std floats are allowed here.
#![allow(clippy::disallowed_methods)]
use super::SCALE_STEPS_M_PER_PX;
use content::enums::Stance;
use glam::{DVec2, Vec2};
use sim_systems::protocol::UncertaintyShape;
use std::f32::consts::{PI, TAU};

/// The icons are drawn on a 24 px grid; the theme's `symbol_size` scales it.
pub const ICON_GRID: f32 = 24.0;
/// A symbol counts as hit within this many pixels of its centre (targets 24x24, accessibility floor 5).
pub const HIT_RADIUS_PX: f32 = 14.0;
/// The gamepad cursor snaps to the nearest symbol within this distance.
pub const SNAP_RADIUS_PX: f32 = 24.0;
/// A press that moves less than this is a click; more is a drag.
pub const CLICK_SLOP_PX: f32 = 5.0;
/// Uncertainty shapes smaller than this radius on screen hide under the symbol and are not drawn.
pub const MIN_SHAPE_PX: f32 = 10.0;

// ---------------------------------------------------------------------------------------------------------
// Transform and scale
// ---------------------------------------------------------------------------------------------------------

/// What the control shows: a map point at its centre and a scale.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Frame { pub centre: DVec2, pub mpp: f64, pub size: Vec2 }

impl Frame {
    pub fn new(centre: DVec2, mpp: f64, size: Vec2) -> Self {
        let centre = if centre.is_finite() { centre } else { DVec2::ZERO };
        let mpp = if mpp.is_finite() && mpp > 0.0 { mpp } else { SCALE_STEPS_M_PER_PX[super::DEFAULT_SCALE_STEP] as f64 };
        let size = Vec2::new(finite_or(size.x, 0.0).max(0.0), finite_or(size.y, 0.0).max(0.0));
        Self { centre, mpp, size }
    }
    pub fn half(&self) -> Vec2 { self.size * 0.5 }
    /// Map metres to local pixels.
    pub fn to_local(&self, map: DVec2) -> Vec2 {
        let d = (map - self.centre) / self.mpp;
        self.half() + Vec2::new(d.x as f32, d.y as f32)
    }
    /// Local pixels to map metres.
    pub fn to_map(&self, local: Vec2) -> DVec2 {
        let d = local - self.half();
        self.centre + DVec2::new(d.x as f64, d.y as f64) * self.mpp
    }
    /// Metres to pixels.
    pub fn px(&self, metres: f64) -> f32 { (metres / self.mpp) as f32 }
    /// The visible map rectangle (min, max), grown by `margin_px` on every side.
    pub fn map_bounds(&self, margin_px: f32) -> (DVec2, DVec2) {
        let a = self.to_map(Vec2::splat(-margin_px));
        let b = self.to_map(self.size + Vec2::splat(margin_px));
        (a.min(b), a.max(b))
    }
    /// True when a local point is inside the control grown by `margin_px`.
    pub fn contains(&self, p: Vec2, margin_px: f32) -> bool {
        p.is_finite() && p.x >= -margin_px && p.y >= -margin_px && p.x <= self.size.x + margin_px && p.y <= self.size.y + margin_px
    }
}

pub fn finite_or(v: f32, or: f32) -> f32 { if v.is_finite() { v } else { or } }

/// The scale step for an index; out-of-range indices clamp.
pub fn clamp_step(step: i64) -> usize { step.clamp(0, SCALE_STEPS_M_PER_PX.len() as i64 - 1) as usize }
pub fn step_mpp(step: usize) -> f64 { SCALE_STEPS_M_PER_PX[step.min(SCALE_STEPS_M_PER_PX.len() - 1)] as f64 }

/// The new centre that keeps the map point under `at` fixed when the scale changes from `frame.mpp` to
/// `new_mpp`.
pub fn zoom_about(frame: &Frame, new_mpp: f64, at: Vec2) -> DVec2 {
    let fixed = frame.to_map(at);
    let d = at - frame.half();
    fixed - DVec2::new(d.x as f64, d.y as f64) * new_mpp
}

/// The finest scale step at which a map of `size_m` fits a control of `px`; the coarsest when none does.
pub fn fit_step(size_m: DVec2, px: Vec2) -> usize {
    if !(size_m.x > 0.0 && size_m.y > 0.0 && px.x > 1.0 && px.y > 1.0) || !size_m.is_finite() { return super::DEFAULT_SCALE_STEP; }
    (0..SCALE_STEPS_M_PER_PX.len()).find(|&i| step_mpp(i) * px.x as f64 >= size_m.x && step_mpp(i) * px.y as f64 >= size_m.y).unwrap_or(SCALE_STEPS_M_PER_PX.len() - 1)
}

/// The smallest 1, 2 or 5 times a power of ten at or above `target` (metres); at least 1 m.
pub fn nice_step(target: f64) -> f64 {
    if !target.is_finite() || target <= 1.0 { return 1.0; }
    let mut p = 1.0;
    while p * 10.0 <= target { p *= 10.0; }
    for m in [1.0, 2.0, 5.0, 10.0] { if p * m >= target { return p * m; } }
    p * 10.0
}

/// Graticule spacing in metres: lines at least `min_px` apart on screen.
pub fn grid_step(mpp: f64, min_px: f64) -> f64 { nice_step(mpp * min_px) }

/// Range-ring spacing: at least `min_px` between rings.
pub fn ring_step(mpp: f64, min_px: f64) -> f64 { nice_step(mpp * min_px) }

/// Multiples of `step` inside `[lo, hi]`, at most `cap` of them (nearest to the middle kept).
pub fn multiples(lo: f64, hi: f64, step: f64, cap: usize) -> Vec<f64> {
    if !(lo.is_finite() && hi.is_finite() && step.is_finite() && step > 0.0) || hi < lo { return Vec::new(); }
    let first = (lo / step).ceil() as i64;
    let last = (hi / step).floor() as i64;
    if last < first { return Vec::new(); }
    let n = (last - first + 1) as usize;
    let (first, last) = if n > cap { let mid = (first + last) / 2; let h = cap as i64 / 2; (mid - h, mid - h + cap as i64 - 1) } else { (first, last) };
    (first..=last).map(|k| k as f64 * step).collect()
}

/// Bearing in degrees 0..360 from `from` to `to` on the map (north is -z), and range in metres. `None` for
/// non-finite input.
pub fn bearing_range(from: DVec2, to: DVec2) -> Option<(f64, f64)> {
    let d = to - from;
    if !d.is_finite() { return None; }
    let range = d.length();
    let b = d.x.atan2(-d.y).to_degrees();
    Some((if range > 0.0 { b.rem_euclid(360.0) } else { 0.0 }, range))
}

/// A screen direction (x right, y down) for a bearing in radians.
pub fn bearing_dir(rad: f32) -> Vec2 { Vec2::new(rad.sin(), -rad.cos()) }

// ---------------------------------------------------------------------------------------------------------
// Symbols
// ---------------------------------------------------------------------------------------------------------

/// A stance frame, as the `stance_*` icons draw it (offsets from the icon centre on the 24 px grid).
#[derive(Clone, Debug, PartialEq)]
pub enum Outline {
    Circle { r: f32 },
    /// Closed polygon.
    Poly(Vec<Vec2>),
    /// Horizontal capsule: half width, half height, the ends are half circles.
    Capsule { half_w: f32, half_h: f32 },
}

/// Frame of a stance scaled by `s` (symbol size / 24).
pub fn stance_outline(stance: Stance, s: f32) -> Outline {
    let v = |x: f32, y: f32| Vec2::new(x - 12.0, y - 12.0) * s;
    match stance {
        Stance::Friend => Outline::Circle { r: 6.0 * s },
        Stance::Hostile => Outline::Poly(vec![v(12.0, 6.0), v(19.0, 18.0), v(5.0, 18.0)]),
        Stance::Suspect => Outline::Poly(vec![v(5.0, 6.0), v(19.0, 6.0), v(19.0, 12.0), v(12.0, 18.0), v(5.0, 12.0)]),
        Stance::Neutral => Outline::Capsule { half_w: 8.0 * s, half_h: 4.0 * s },
    }
}

/// Where the weapon's solid centre mark sits for each frame (the icons' `circle cy`), radius 2.
pub fn weapon_dot(stance: Stance, s: f32) -> Vec2 {
    let cy = match stance { Stance::Hostile => 14.0, Stance::Suspect => 11.0, Stance::Friend | Stance::Neutral => 12.5 };
    Vec2::new(0.0, cy - 12.0) * s
}

/// The domain bar: air above at y 2, subsurface below at y 22, from x 7 to 17.
pub fn domain_bar(above: bool, s: f32) -> (Vec2, Vec2) {
    let y = if above { 2.0 } else { 22.0 } - 12.0;
    (Vec2::new(-5.0, y) * s, Vec2::new(5.0, y) * s)
}

/// Closed point list of an outline (for polylines and the bounding box).
pub fn outline_points(o: &Outline, segments: usize) -> Vec<Vec2> {
    let n = segments.max(8);
    match o {
        Outline::Circle { r } => (0..=n).map(|i| { let a = i as f32 / n as f32 * TAU; Vec2::new(a.cos(), a.sin()) * *r }).collect(),
        Outline::Poly(p) => { let mut v = p.clone(); if let Some(f) = p.first() { v.push(*f); } v }
        Outline::Capsule { half_w, half_h } => {
            let r = *half_h;
            let cx = (half_w - r).max(0.0);
            let k = n / 2;
            let mut v = Vec::with_capacity(2 * k + 3);
            // Right end from top to bottom, left end from bottom to top.
            for i in 0..=k { let a = -PI / 2.0 + i as f32 / k as f32 * PI; v.push(Vec2::new(cx + r * a.cos(), r * a.sin())); }
            for i in 0..=k { let a = PI / 2.0 + i as f32 / k as f32 * PI; v.push(Vec2::new(-cx + r * a.cos(), r * a.sin())); }
            if let Some(f) = v.first().copied() { v.push(f); }
            v
        }
    }
}

/// Half extent of a symbol (frame, bar and glyphs) for label placement: the icon grid's half.
pub fn symbol_half(s: f32) -> f32 { 0.5 * ICON_GRID * s }

// ---------------------------------------------------------------------------------------------------------
// Uncertainty
// ---------------------------------------------------------------------------------------------------------

/// An uncertainty shape on screen: a filled region whose edge is graded. `outer` is the 95% contour,
/// where the fade reaches zero; `inner` is where the fade starts. Both are closed rings with the same point
/// count, so the band between them triangulates as a strip. The solid inside is a fan from `centre` for
/// circles and ellipses; for a wedge (`fan` false) the ring is its far arc then its near arc reversed, and
/// the inside is the strip between the two arcs.
#[derive(Clone, Debug, PartialEq)]
pub struct ShapeGeom {
    pub centre: Vec2,
    pub inner: Vec<Vec2>,
    pub outer: Vec<Vec2>,
    pub fan: bool,
    /// Largest radius on screen, pixels.
    pub extent_px: f32,
}

impl ShapeGeom {
    /// Triangles of the solid inside, as indices into `inner` (plus `inner.len()` for the centre of a fan).
    pub fn core_tris(&self) -> Vec<u32> {
        let n = self.inner.len() as u32;
        let mut out = Vec::new();
        if n < 3 { return out; }
        if self.fan {
            for i in 0..n { out.extend_from_slice(&[n, i, (i + 1) % n]); }
        } else {
            // Far arc 0..=m, near arc reversed m+1..=2m+1: index of the near point at step i is 2m+1-i.
            let m = n / 2 - 1;
            for i in 0..m { let (a, b, c, d) = (i, i + 1, 2 * m + 1 - (i + 1), 2 * m + 1 - i); out.extend_from_slice(&[a, b, c, a, c, d]); }
        }
        out
    }
}

/// Fraction of the 95% contour where the edge fade begins.
pub const FADE_START: f32 = 0.78;

/// The screen geometry of a shape centred at the track's map position. `None` when it is too small to show
/// beside the symbol, degenerate or not finite.
pub fn shape_geom(frame: &Frame, pos: DVec2, shape: &UncertaintyShape) -> Option<ShapeGeom> {
    let c = frame.to_local(pos);
    if !c.is_finite() { return None; }
    let pxf = |m: f32| if m.is_finite() { (m.max(0.0) as f64 / frame.mpp) as f32 } else { 0.0 };
    match *shape {
        UncertaintyShape::Circle { radius_m } => ellipse_geom(c, pxf(radius_m), pxf(radius_m), 0.0),
        UncertaintyShape::Ellipse { semi_major_m, semi_minor_m, bearing_rad } => {
            let b = if bearing_rad.is_finite() { bearing_rad } else { 0.0 };
            ellipse_geom(c, pxf(semi_major_m), pxf(semi_minor_m), b)
        }
        UncertaintyShape::Wedge { apex, bearing_rad, half_angle_rad, near_m, far_m } => {
            if !apex.is_finite() || !bearing_rad.is_finite() || !half_angle_rad.is_finite() { return None; }
            let a = frame.to_local(apex);
            if !a.is_finite() { return None; }
            let (mut near, mut far) = (pxf(near_m), pxf(far_m));
            if near > far { std::mem::swap(&mut near, &mut far); }
            let half = half_angle_rad.abs().clamp(0.0005, 1.5);
            if far < MIN_SHAPE_PX || far * half < 0.75 { return None; }
            let m = ((half * far / 6.0) as usize).clamp(6, 48);
            let arc = |r: f32, h: f32, out: &mut Vec<Vec2>, rev: bool| {
                for k in 0..=m { let i = if rev { m - k } else { k }; let t = -h + 2.0 * h * i as f32 / m as f32; out.push(a + bearing_dir(bearing_rad + t) * r); }
            };
            // The fade is at least 3 px wide and at most 40; inward on every side.
            let fade = ((far - near) * (1.0 - FADE_START)).clamp(3.0, 40.0).min((far - near) * 0.45).max(0.5);
            let inner_half = (half - fade / far.max(1.0)).max(half * 0.35);
            let ring = |n_r: f32, f_r: f32, h: f32| { let mut v = Vec::with_capacity(2 * m + 2); arc(f_r, h, &mut v, false); arc(n_r, h, &mut v, true); v };
            let outer = ring(near, far, half);
            let inner = ring((near + fade).min(far - fade), far - fade, inner_half);
            let mid = a + bearing_dir(bearing_rad) * (0.5 * (near + far));
            Some(ShapeGeom { centre: mid, inner, outer, fan: false, extent_px: far })
        }
    }
}

fn ellipse_geom(c: Vec2, major: f32, minor: f32, bearing: f32) -> Option<ShapeGeom> {
    let (major, minor) = (major.max(minor), minor.min(major));
    if major < MIN_SHAPE_PX || !(major.is_finite() && minor.is_finite()) { return None; }
    let minor = minor.max(1.0);
    let n = ((major / 4.0) as usize).clamp(24, 96);
    let axis = bearing_dir(bearing);
    let side = Vec2::new(-axis.y, axis.x);
    let ring = |k: f32| -> Vec<Vec2> { (0..n).map(|i| { let t = i as f32 / n as f32 * TAU; c + axis * (t.cos() * major * k) + side * (t.sin() * minor * k) }).collect() };
    // The fade band is at least 3 px wide even for thin ellipses.
    let k = (1.0 - (3.0 / minor).max(1.0 - FADE_START)).clamp(0.2, FADE_START);
    Some(ShapeGeom { centre: c, inner: ring(k), outer: ring(1.0), fan: true, extent_px: major })
}

/// Hash of a track number for sample placement: the same points every frame for the same track.
fn mix(mut x: u64) -> u64 {
    x ^= x >> 33;
    x = x.wrapping_mul(0xff51_afd7_ed55_8ccd);
    x ^= x >> 33;
    x = x.wrapping_mul(0xc4ce_b9fe_1a85_ec53);
    x ^ (x >> 33)
}
fn unit(h: u64) -> f32 { ((h >> 40) as f32) / ((1u64 << 24) as f32) }

/// Sample points inside a shape: normal draws in the shape's frame, kept inside the 95% contour, the same
/// points for the same track every frame. `count` points at most.
pub fn shape_samples(frame: &Frame, pos: DVec2, shape: &UncertaintyShape, seed: u64, count: usize) -> Vec<Vec2> {
    let mut out = Vec::with_capacity(count);
    // 95% contour in 2D is 2.45 sigma.
    let sigma = 1.0 / 2.45;
    for i in 0..count as u64 {
        let h1 = mix(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15).wrapping_add(i * 2 + 1));
        let h2 = mix(h1 ^ 0xA5A5_A5A5_5A5A_5A5A);
        let (u1, u2) = (unit(h1).max(1e-6), unit(h2));
        // Box-Muller, then clamp inside the contour (unit disc).
        let r = ((-2.0 * u1.ln()).sqrt() * sigma).min(0.96);
        let t = u2 * TAU;
        let (dx, dy) = (r * t.cos(), r * t.sin());
        let p = match *shape {
            UncertaintyShape::Circle { radius_m } => {
                let r = frame.px(radius_m.max(0.0) as f64);
                frame.to_local(pos) + Vec2::new(dx, dy) * r
            }
            UncertaintyShape::Ellipse { semi_major_m, semi_minor_m, bearing_rad } => {
                let axis = bearing_dir(finite_or(bearing_rad, 0.0));
                let side = Vec2::new(-axis.y, axis.x);
                let (a, b) = (frame.px(semi_major_m.max(0.0) as f64), frame.px(semi_minor_m.max(0.0) as f64));
                frame.to_local(pos) + axis * (dx * a.max(b)) + side * (dy * a.min(b))
            }
            UncertaintyShape::Wedge { apex, bearing_rad, half_angle_rad, near_m, far_m } => {
                let a = frame.to_local(apex);
                let (n, f) = (frame.px(near_m.min(far_m).max(0.0) as f64), frame.px(far_m.max(near_m).max(0.0) as f64));
                // Range uniform-ish across the band, bearing normal across the half angle.
                let along = n + (f - n) * (0.5 + 0.5 * dx.clamp(-1.0, 1.0));
                let across = half_angle_rad.abs().min(1.5) * dy.clamp(-1.0, 1.0);
                a + bearing_dir(finite_or(bearing_rad, 0.0) + across) * along
            }
        };
        if p.is_finite() { out.push(p); }
    }
    out
}

/// How many sample points a shape of this screen size gets: a handful, more for bigger shapes.
pub fn sample_count(extent_px: f32) -> usize { (5.0 + extent_px / 14.0).clamp(5.0, 14.0) as usize }

// ---------------------------------------------------------------------------------------------------------
// Labels and declutter (rule 14, L1670)
// ---------------------------------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect { pub min: Vec2, pub max: Vec2 }
impl Rect {
    pub fn at(pos: Vec2, size: Vec2) -> Self { Rect { min: pos, max: pos + size } }
    pub fn around(c: Vec2, half: f32) -> Self { Rect { min: c - Vec2::splat(half), max: c + Vec2::splat(half) } }
    pub fn overlaps(&self, o: &Rect) -> bool { self.min.x < o.max.x && o.min.x < self.max.x && self.min.y < o.max.y && o.min.y < self.max.y }
    pub fn contains(&self, p: Vec2) -> bool { p.x >= self.min.x && p.x <= self.max.x && p.y >= self.min.y && p.y <= self.max.y }
    pub fn inside(&self, o: &Rect) -> bool { self.min.x >= o.min.x && self.min.y >= o.min.y && self.max.x <= o.max.x && self.max.y <= o.max.y }
    pub fn size(&self) -> Vec2 { self.max - self.min }
    pub fn grow(&self, by: f32) -> Rect { Rect { min: self.min - Vec2::splat(by), max: self.max + Vec2::splat(by) } }
    pub fn distance(&self, p: Vec2) -> f32 {
        let d = (self.min - p).max(p - self.max).max(Vec2::ZERO);
        d.length()
    }
}

/// Symbols of this rank or better (own hull and selection, inbound weapons, Hostile) are never covered by a
/// label: the declutter gate keeps the top priorities unoverlapped (L1764).
pub const TOP_RANK: u8 = 3;

/// Label sides, tried in this order unless the label had another side last frame: right, left, above,
/// below, then the four corners.
pub const SIDES: usize = 8;

/// One label to place.
#[derive(Clone, Debug, PartialEq)]
pub struct LabelReq {
    /// Centre of the thing labelled.
    pub anchor: Vec2,
    /// Half extent of the thing labelled (the label sits outside it).
    pub clear: f32,
    pub size: Vec2,
    /// Declutter rank: lower stays longer.
    pub rank: u8,
    /// Higher wins inside a rank (threat value).
    pub weight: f64,
    /// Stable identity for ordering and the remembered side.
    pub key: u64,
    /// The side used last frame, tried first.
    pub prev_side: Option<u8>,
    /// Shown whatever overlaps it (the hovered track).
    pub force: bool,
    /// Placed centred on `anchor` with no choice of side (ring and graticule labels sit on their line).
    pub fixed: bool,
    /// May cover other symbols when no side clears them (the top ranks); otherwise such a label hides.
    pub cover: bool,
}

/// Where a label went.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Placed { pub rect: Rect, pub side: u8 }

/// The label's rectangle on a side: right, left, above, below, upper right, lower right, upper left,
/// lower left.
pub fn label_rect(r: &LabelReq, side: u8) -> Rect {
    if r.fixed { return Rect::at(r.anchor - r.size * 0.5, r.size); }
    let gap = 4.0;
    let (right, left) = (r.anchor.x + r.clear + gap, r.anchor.x - r.clear - gap - r.size.x);
    let (above, below) = (r.anchor.y - r.clear - gap - r.size.y, r.anchor.y + r.clear + gap);
    let pos = match side {
        0 => Vec2::new(right, r.anchor.y - r.size.y * 0.5),
        1 => Vec2::new(left, r.anchor.y - r.size.y * 0.5),
        2 => Vec2::new(r.anchor.x - r.size.x * 0.5, above),
        3 => Vec2::new(r.anchor.x - r.size.x * 0.5, below),
        // Corners: diagonal from the symbol, clear of its square.
        4 => Vec2::new(r.anchor.x + 0.7 * r.clear, above),
        5 => Vec2::new(r.anchor.x + 0.7 * r.clear, below),
        6 => Vec2::new(r.anchor.x - 0.7 * r.clear - r.size.x, above),
        _ => Vec2::new(r.anchor.x - 0.7 * r.clear - r.size.x, below),
    };
    Rect::at(pos, r.size)
}

/// Places labels so none overlaps another: in rank order (then weight, then key), each takes the first
/// side that stays inside `bounds`, overlaps no placed label and clears every symbol in `symbols` (each with
/// its rank); a label allowed to `cover` may instead take the first side that covers only symbols of a
/// worse rank than its own and worse than [`TOP_RANK`], so the top priorities are never covered. A label with no such side is hidden
/// (`None`), as is one whose anchor is outside `bounds`. A forced label is placed last, on the
/// first side free of other labels if there is one, else on its first side inside `bounds`.
pub fn declutter(reqs: &[LabelReq], symbols: &[(Rect, u8)], bounds: Rect) -> Vec<Option<Placed>> {
    declutter_around(reqs, symbols, bounds, &[])
}

/// [`declutter`] with `blocked` rectangles (HUD clusters over the plot) that every label treats as
/// already taken: a label beside one moves to a free side or hides, never sits under it.
pub fn declutter_around(reqs: &[LabelReq], symbols: &[(Rect, u8)], bounds: Rect, blocked: &[Rect]) -> Vec<Option<Placed>> {
    let mut order: Vec<usize> = (0..reqs.len()).collect();
    order.sort_by(|&a, &b| {
        let (x, y) = (&reqs[a], &reqs[b]);
        x.rank.cmp(&y.rank).then(y.weight.total_cmp(&x.weight)).then(x.key.cmp(&y.key))
    });
    let mut out: Vec<Option<Placed>> = vec![None; reqs.len()];
    let mut placed: Vec<Rect> = Vec::with_capacity(reqs.len() + blocked.len());
    placed.extend(blocked.iter().copied().filter(|b| b.min.is_finite() && b.max.is_finite()));
    let mut forced = Vec::new();
    for &i in &order {
        let r = &reqs[i];
        if r.force { forced.push(i); continue; }
        if !bounds.contains(r.anchor) || !r.size.is_finite() { continue; }
        let own = Rect::around(r.anchor, r.clear);
        let sides = if r.fixed { [0; SIDES] } else { side_order(r.prev_side) };
        let mut fallback = None;
        for side in sides {
            let rect = label_rect(r, side);
            if !rect.inside(&bounds) || placed.iter().any(|p| p.overlaps(&rect)) { continue; }
            let mut hits = symbols.iter().filter(|(s, _)| *s != own && s.overlaps(&rect));
            let first = hits.next();
            if first.is_none() { out[i] = Some(Placed { rect, side }); break; }
            let worse_only = first.into_iter().chain(hits).all(|(_, rank)| *rank > r.rank && *rank > TOP_RANK);
            if fallback.is_none() && worse_only { fallback = Some(Placed { rect, side }); }
        }
        if out[i].is_none() && r.cover { out[i] = fallback; }
        if let Some(p) = out[i] { placed.push(p.rect); }
    }
    for i in forced {
        let r = &reqs[i];
        let sides = if r.fixed { [0; SIDES] } else { side_order(r.prev_side) };
        let cands: Vec<Placed> = sides.iter().map(|&side| Placed { rect: label_rect(r, side), side }).collect();
        let best = cands.iter().find(|p| p.rect.inside(&bounds) && !placed.iter().any(|q| q.overlaps(&p.rect)))
            .or_else(|| cands.iter().find(|p| p.rect.inside(&bounds)))
            .copied()
            .unwrap_or(cands[0]);
        placed.push(best.rect);
        out[i] = Some(best);
    }
    out
}

fn side_order(prev: Option<u8>) -> [u8; SIDES] {
    let mut v = [0u8; SIDES];
    for (i, s) in v.iter_mut().enumerate() { *s = i as u8; }
    if let Some(p) = prev.filter(|s| (*s as usize) < SIDES) {
        v[0] = p;
        let mut k = 1;
        for s in 0..SIDES as u8 { if s != p { v[k] = s; k += 1; } }
    }
    v
}

// ---------------------------------------------------------------------------------------------------------
// Hit testing
// ---------------------------------------------------------------------------------------------------------

/// Index of the nearest point within `radius` of `at`, ties to the lower index.
pub fn nearest_within(points: &[Vec2], at: Vec2, radius: f32) -> Option<usize> {
    let mut best: Option<(usize, f32)> = None;
    for (i, p) in points.iter().enumerate() {
        if !p.is_finite() { continue; }
        let d = p.distance(at);
        if d <= radius && best.is_none_or(|(_, bd)| d < bd) { best = Some((i, d)); }
    }
    best.map(|(i, _)| i)
}

/// The gamepad cursor after a move: the free position moves by `delta` and stays inside the control; the
/// shown position snaps to the nearest symbol within [`SNAP_RADIUS_PX`]. Returns (free, shown, snapped
/// index).
pub fn move_cursor(free: Vec2, delta: Vec2, size: Vec2, symbols: &[Vec2]) -> (Vec2, Vec2, Option<usize>) {
    let delta = if delta.is_finite() { delta } else { Vec2::ZERO };
    let free = if free.is_finite() { free } else { size * 0.5 };
    let free = (free + delta).clamp(Vec2::ZERO, size.max(Vec2::ZERO));
    match nearest_within(symbols, free, SNAP_RADIUS_PX) {
        Some(i) => (free, symbols[i], Some(i)),
        None => (free, free, None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame() -> Frame { Frame::new(DVec2::new(1_000.0, -2_000.0), 25.0, Vec2::new(1600.0, 900.0)) }

    #[test]
    fn transform_round_trips_and_north_is_up() {
        let f = frame();
        assert_eq!(f.to_local(f.centre), Vec2::new(800.0, 450.0));
        for p in [Vec2::ZERO, Vec2::new(13.5, 870.25), Vec2::new(1600.0, 0.0)] {
            assert!((f.to_local(f.to_map(p)) - p).length() < 1e-3);
        }
        // 2.5 km north is 100 px up; east is +x.
        assert_eq!(f.to_local(f.centre + DVec2::new(0.0, -2_500.0)), Vec2::new(800.0, 350.0));
        let (b, r) = bearing_range(DVec2::ZERO, DVec2::new(1_000.0, 0.0)).unwrap();
        assert!((b - 90.0).abs() < 1e-9 && (r - 1_000.0).abs() < 1e-9);
        let (b, _) = bearing_range(DVec2::ZERO, DVec2::new(0.0, -10.0)).unwrap();
        assert!(b.abs() < 1e-9);
        let (b, _) = bearing_range(DVec2::ZERO, DVec2::new(-10.0, 0.0)).unwrap();
        assert!((b - 270.0).abs() < 1e-9);
        assert!(bearing_range(DVec2::ZERO, DVec2::new(f64::NAN, 0.0)).is_none());
        let d = bearing_dir(std::f32::consts::FRAC_PI_2);
        assert!((d - Vec2::X).length() < 1e-6);
        // Bad input never yields a non-finite frame.
        let bad = Frame::new(DVec2::new(f64::NAN, 0.0), -1.0, Vec2::new(f32::NAN, 10.0));
        assert!(bad.to_map(Vec2::new(3.0, 3.0)).is_finite());
    }

    #[test]
    fn zoom_keeps_the_point_under_the_cursor() {
        let f = frame();
        let at = Vec2::new(1200.0, 300.0);
        let before = f.to_map(at);
        for step in 0..SCALE_STEPS_M_PER_PX.len() {
            let mpp = step_mpp(step);
            let g = Frame::new(zoom_about(&f, mpp, at), mpp, f.size);
            assert!((g.to_map(at) - before).length() < 1e-6 * mpp.max(1.0), "step {step}");
        }
        assert_eq!(clamp_step(-4), 0);
        assert_eq!(clamp_step(99), SCALE_STEPS_M_PER_PX.len() - 1);
        assert_eq!(step_mpp(4), 25.0);
    }

    #[test]
    fn fit_and_nice_steps() {
        // A 20 km square map on a 1600 x 900 control fits at 25 m/px (22.5 km tall), not 10.
        assert_eq!(fit_step(DVec2::splat(20_000.0), Vec2::new(1600.0, 900.0)), 4);
        assert_eq!(fit_step(DVec2::splat(1.0e9), Vec2::new(1600.0, 900.0)), 7);
        assert_eq!(fit_step(DVec2::ZERO, Vec2::new(1600.0, 900.0)), super::super::DEFAULT_SCALE_STEP);
        assert_eq!(nice_step(0.3), 1.0);
        assert_eq!(nice_step(3.0), 5.0);
        assert_eq!(nice_step(1_000.0), 1_000.0);
        assert_eq!(nice_step(1_001.0), 2_000.0);
        assert_eq!(nice_step(4_200.0), 5_000.0);
        assert_eq!(nice_step(f64::NAN), 1.0);
        assert_eq!(multiples(-2_500.0, 2_500.0, 1_000.0, 100), vec![-2_000.0, -1_000.0, 0.0, 1_000.0, 2_000.0]);
        assert_eq!(multiples(0.0, 1.0e9, 1.0, 5).len(), 5);
        assert!(multiples(5.0, 1.0, 1.0, 5).is_empty());
        assert!(multiples(0.0, 1.0, 0.0, 5).is_empty());
    }

    #[test]
    fn symbols_match_the_icon_geometry() {
        // Hostile apex 6 px above the centre; suspect's point 6 px below; capsule 2:1.
        match stance_outline(Stance::Hostile, 1.0) { Outline::Poly(p) => assert_eq!(p[0], Vec2::new(0.0, -6.0)), o => panic!("{o:?}") }
        match stance_outline(Stance::Suspect, 1.0) { Outline::Poly(p) => assert_eq!(p[3], Vec2::new(0.0, 6.0)), o => panic!("{o:?}") }
        assert_eq!(stance_outline(Stance::Neutral, 2.0), Outline::Capsule { half_w: 16.0, half_h: 8.0 });
        assert_eq!(stance_outline(Stance::Friend, 1.0), Outline::Circle { r: 6.0 });
        assert_eq!(domain_bar(true, 1.0), (Vec2::new(-5.0, -10.0), Vec2::new(5.0, -10.0)));
        assert_eq!(domain_bar(false, 1.0).0.y, 10.0);
        let cap = outline_points(&stance_outline(Stance::Neutral, 1.0), 16);
        assert_eq!(cap.first(), cap.last());
        assert!(cap.iter().all(|p| p.x.abs() <= 8.0 + 1e-4 && p.y.abs() <= 4.0 + 1e-4));
    }

    #[test]
    fn shapes_keep_their_sensor_form() {
        let f = Frame::new(DVec2::ZERO, 10.0, Vec2::new(1000.0, 1000.0));
        // A 50 m circle at 10 m/px is 5 px: hidden under the symbol.
        assert!(shape_geom(&f, DVec2::ZERO, &UncertaintyShape::Circle { radius_m: 50.0 }).is_none());
        let c = shape_geom(&f, DVec2::ZERO, &UncertaintyShape::Circle { radius_m: 400.0 }).unwrap();
        assert!(c.fan && (c.extent_px - 40.0).abs() < 1e-3 && c.inner.len() == c.outer.len());
        assert_eq!(c.core_tris().len(), c.inner.len() * 3);
        assert!(c.outer.iter().all(|p| ((*p - c.centre).length() - 40.0).abs() < 1e-2));
        // An ellipse with its major axis east-west.
        let e = shape_geom(&f, DVec2::ZERO, &UncertaintyShape::Ellipse { semi_major_m: 1_000.0, semi_minor_m: 200.0, bearing_rad: std::f32::consts::FRAC_PI_2 }).unwrap();
        let xs = e.outer.iter().map(|p| (p.x - 500.0).abs()).fold(0.0, f32::max);
        let ys = e.outer.iter().map(|p| (p.y - 500.0).abs()).fold(0.0, f32::max);
        assert!((xs - 100.0).abs() < 0.5 && (ys - 20.0).abs() < 0.5, "{xs} {ys}");
        // A wedge from an apex bearing north, near and far arcs, fading inward.
        let w = shape_geom(&f, DVec2::new(0.0, 5_000.0), &UncertaintyShape::Wedge { apex: glam::DVec2::new(0.0, 5_000.0), bearing_rad: 0.0, half_angle_rad: 0.1, near_m: 500.0, far_m: 4_000.0 }).unwrap();
        assert!(!w.fan && w.inner.len() == w.outer.len() && w.inner.len().is_multiple_of(2));
        assert!(w.outer.iter().all(|p| p.y < 1000.0 - 49.0 && p.y > 1000.0 - 401.0));
        let tris = w.core_tris();
        assert!(!tris.is_empty() && tris.iter().all(|&i| (i as usize) < w.inner.len()));
        // The inner ring lies inside the outer one: nearer the bearing line and inside the range band.
        let r = |p: &Vec2| (*p - Vec2::new(500.0, 1000.0)).length();
        assert!(w.inner.iter().all(|p| r(p) > 50.0 && r(p) < 400.0));
        // Bad numbers never produce a shape or a NaN.
        assert!(shape_geom(&f, DVec2::ZERO, &UncertaintyShape::Circle { radius_m: f32::NAN }).is_none());
        assert!(shape_geom(&f, DVec2::new(f64::NAN, 0.0), &UncertaintyShape::Circle { radius_m: 900.0 }).is_none());
        assert!(shape_geom(&f, DVec2::ZERO, &UncertaintyShape::Wedge { apex: glam::DVec2::ZERO, bearing_rad: f32::INFINITY, half_angle_rad: 0.1, near_m: 0.0, far_m: 9_000.0 }).is_none());
        let flipped = shape_geom(&f, DVec2::ZERO, &UncertaintyShape::Wedge { apex: glam::DVec2::ZERO, bearing_rad: 1.0, half_angle_rad: -4.0, near_m: 9_000.0, far_m: 0.0 }).unwrap();
        assert!(flipped.outer.iter().chain(&flipped.inner).all(|p| p.is_finite()));
    }

    #[test]
    fn samples_are_stable_and_inside() {
        let f = Frame::new(DVec2::ZERO, 10.0, Vec2::new(1000.0, 1000.0));
        let s = UncertaintyShape::Ellipse { semi_major_m: 800.0, semi_minor_m: 300.0, bearing_rad: 0.4 };
        let a = shape_samples(&f, DVec2::ZERO, &s, 412, 9);
        assert_eq!(a, shape_samples(&f, DVec2::ZERO, &s, 412, 9));
        assert_ne!(a, shape_samples(&f, DVec2::ZERO, &s, 413, 9));
        assert_eq!(a.len(), 9);
        assert!(a.iter().all(|p| (*p - Vec2::splat(500.0)).length() <= 80.0 + 1e-3));
        let c = shape_samples(&f, DVec2::ZERO, &UncertaintyShape::Circle { radius_m: 500.0 }, 7, 14);
        assert!(c.iter().all(|p| (*p - Vec2::splat(500.0)).length() <= 50.0));
        assert_eq!(sample_count(0.0), 5);
        assert_eq!(sample_count(1.0e6), 14);
    }

    fn req(key: u64, x: f32, y: f32, rank: u8) -> LabelReq {
        LabelReq { anchor: Vec2::new(x, y), clear: 12.0, size: Vec2::new(80.0, 24.0), rank, weight: 0.0, key, prev_side: None, force: false, fixed: false, cover: true }
    }

    #[test]
    fn declutter_keeps_labels_out_from_under_hud_clusters() {
        let bounds = Rect::at(Vec2::ZERO, Vec2::new(1000.0, 1000.0));
        // A cluster over the right of the anchor: the label takes another side.
        let cluster = Rect::at(Vec2::new(510.0, 400.0), Vec2::new(300.0, 200.0));
        let out = declutter_around(&[req(1, 500.0, 500.0, 1)], &[], bounds, &[cluster]);
        let p = out[0].expect("a free side exists");
        assert_ne!(p.side, 0);
        assert!(!p.rect.overlaps(&cluster));
        // Boxed in on every side: hidden, never drawn under the cluster.
        let all = Rect::at(Vec2::new(300.0, 300.0), Vec2::new(400.0, 400.0));
        assert!(declutter_around(&[req(1, 500.0, 500.0, 1)], &[], bounds, &[all])[0].is_none());
    }

    #[test]
    fn declutter_hides_lowest_first_and_never_overlaps() {
        let bounds = Rect::at(Vec2::ZERO, Vec2::new(1000.0, 1000.0));
        // Ten labels on one spot: the free sides take the best, the lowest ranks hide.
        let reqs: Vec<LabelReq> = (0..10).map(|i| req(i, 500.0, 500.0, 10 - i as u8)).collect();
        let out = declutter(&reqs, &[], bounds);
        assert!(out[0].is_none(), "rank 10 hides first: {out:?}");
        let shown = out.iter().filter(|p| p.is_some()).count();
        assert!((4..10).contains(&shown), "{shown}");
        // Hiding goes strictly from the lowest rank up.
        let first_shown = out.iter().position(|p| p.is_some()).unwrap();
        assert!(out[first_shown..].iter().all(|p| p.is_some()), "{out:?}");
        let rects: Vec<Rect> = out.iter().flatten().map(|p| p.rect).collect();
        for (i, a) in rects.iter().enumerate() { for b in &rects[i + 1..] { assert!(!a.overlaps(b)); } }
        // Rank 1 (key 9) takes its first side, right of the anchor.
        assert_eq!(out[9].unwrap().side, 0);
        // No side ever covers the label's own symbol.
        let r1 = req(1, 500.0, 500.0, 1);
        for side in 0..SIDES as u8 { assert!(!label_rect(&r1, side).overlaps(&Rect::around(r1.anchor, r1.clear)), "side {side}"); }
        // A remembered side is kept while it is free.
        let mut r = req(9, 500.0, 500.0, 1);
        r.prev_side = Some(2);
        assert_eq!(declutter(&[r.clone()], &[], bounds)[0].unwrap().side, 2);
        // A label prefers a side that clears other symbols.
        let blocker = Rect::around(Vec2::new(560.0, 500.0), 12.0);
        let mine = Rect::around(Vec2::new(500.0, 500.0), 12.0);
        assert_eq!(declutter(&[req(1, 500.0, 500.0, 1)], &[(mine, 1), (blocker, 6)], bounds)[0].unwrap().side, 1);
        // Boxed in by symbols: a top-rank label covers one, a low-rank label hides.
        let ring = |rank: u8| -> Vec<(Rect, u8)> { [(560.0, 500.0), (440.0, 500.0), (500.0, 465.0), (500.0, 535.0), (560.0, 470.0), (560.0, 530.0), (440.0, 470.0), (440.0, 530.0)].iter().map(|&(x, y)| (Rect::around(Vec2::new(x, y), 12.0), rank)).chain([(mine, 1)]).collect() };
        assert!(declutter(&[req(1, 500.0, 500.0, 1)], &ring(6), bounds)[0].is_some(), "a top rank covers lower symbols");
        assert!(declutter(&[req(1, 500.0, 500.0, 0)], &ring(3), bounds)[0].is_none(), "nothing covers a top-priority symbol");
        assert!(declutter(&[req(1, 500.0, 500.0, 1)], &ring(1), bounds)[0].is_none(), "never a symbol of its own rank or better");
        let mut low = req(1, 500.0, 500.0, 6);
        low.cover = false;
        assert!(declutter(&[low], &ring(7), bounds)[0].is_none());
        // Off-screen anchors and edge labels.
        assert!(declutter(&[req(1, -50.0, 500.0, 0)], &[], bounds)[0].is_none());
        assert_eq!(declutter(&[req(1, 990.0, 500.0, 0)], &[], bounds)[0].unwrap().side, 1);
        // A forced label shows even where another sits.
        let mut f = req(3, 500.0, 500.0, 9);
        f.force = true;
        let out = declutter(&[req(1, 500.0, 500.0, 0), f], &[], bounds);
        assert!(out[1].is_some());
        // Ties break on weight, then key: stable between frames.
        let mut a = req(1, 500.0, 500.0, 3);
        let mut b = req(2, 500.0, 500.0, 3);
        a.weight = 1.0;
        b.weight = 2.0;
        let out = declutter(&[a, b], &[], bounds);
        assert_eq!(out[1].unwrap().side, 0);
        assert_eq!(side_order(Some(3)), [3, 0, 1, 2, 4, 5, 6, 7]);
        // A fixed label sits centred on its anchor and hides when a better one is there.
        let mut ring = req(7, 500.0, 500.0, 9);
        ring.fixed = true;
        let out = declutter(&[ring.clone()], &[], bounds);
        assert_eq!(out[0].unwrap().rect.min, Vec2::new(460.0, 488.0));
        assert!(declutter(&[req(1, 450.0, 500.0, 0), ring], &[], bounds)[1].is_none());
        assert_eq!(side_order(Some(9)), [0, 1, 2, 3, 4, 5, 6, 7]);
    }

    #[test]
    fn cursor_snaps_and_escapes() {
        let syms = [Vec2::new(100.0, 100.0), Vec2::new(300.0, 100.0)];
        let size = Vec2::new(400.0, 300.0);
        let (free, shown, hit) = move_cursor(Vec2::new(80.0, 100.0), Vec2::new(5.0, 0.0), size, &syms);
        assert_eq!((free, shown, hit), (Vec2::new(85.0, 100.0), syms[0], Some(0)));
        let (free, shown, hit) = move_cursor(free, Vec2::new(60.0, 0.0), size, &syms);
        assert_eq!((shown, hit), (free, None));
        let (free, _, _) = move_cursor(free, Vec2::new(9_999.0, -9_999.0), size, &syms);
        assert_eq!(free, Vec2::new(400.0, 0.0));
        let (free, _, _) = move_cursor(Vec2::new(f32::NAN, 0.0), Vec2::new(f32::NAN, 1.0), size, &syms);
        assert_eq!(free, Vec2::new(200.0, 150.0));
        assert_eq!(nearest_within(&syms, Vec2::new(200.0, 100.0), 150.0), Some(0));
        assert_eq!(nearest_within(&syms, Vec2::new(200.0, 300.0), 10.0), None);
    }
}
