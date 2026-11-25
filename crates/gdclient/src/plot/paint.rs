//! Part of unit `gd-plot`. Draws a [`Picture`] with the theme's `Plot` items (sortie and replay) or `Chart`
//! items (briefing and after-action, the NOAA chart palette of L1643). Every colour comes from the theme;
//! alpha is the only thing this file changes, for value grading (rule 6) and quiet reference lines.
//!
//! Filled regions (land, shallows, uncertainty shapes, sample points, belief) go to the canvas item as
//! triangle arrays built here, so the engine never triangulates and a bad polygon cannot print an error.
//!
//! Decisions where the spec is silent:
//! - The sortie plot's land is `surface.raised` with a `rule.divider` coast and `surface.base` shallows: the
//!   `Plot` palette has no chart colours and normal state is grey.
//! - The graticule is `rule.reference` at 30% alpha so it never outweighs tracks; rings, bearing lines and
//!   the scale bar are full `rule.reference`.
//! - The own hull is drawn in `label` (text.primary) with its heading tick; allied hulls in the Friend tint.
#![allow(clippy::disallowed_methods)]
use super::chart::{Chart, MarkerGlyph};
use super::core::{self, Frame, Outline};
use super::picture::{LabelRole, Picture, RingKind, SymKind, Symbol};
use content::enums::{Domain, Stance};
use glam::Vec2;
use godot::classes::{Control, Font, RenderingServer};
use godot::obj::Singleton;
use godot::prelude::*;
use sim_systems::protocol::VerticalTrend;
use std::f32::consts::{PI, TAU};

/// Theme items the plot reads, loaded on theme change.
#[derive(Clone)]
pub struct Palette {
    pub chart: bool,
    pub background: Color,
    pub shallows: Color,
    pub land: Color,
    pub coast: Color,
    pub grid: Color,
    pub reference: Color,
    pub label: Color,
    pub label_outline: Color,
    pub overlay_label: Color,
    pub route: Color,
    pub uncertainty: Color,
    pub history: Color,
    pub select: Color,
    pub focus: Color,
    pub box_: Color,
    pub inactive: Color,
    pub own: Color,
    pub belief: Color,
    /// Friend, Neutral, Suspect, Hostile.
    pub stance: [Color; 4],
    pub font: Option<Gd<Font>>,
    pub font_bold: Option<Gd<Font>>,
    pub font_size: i32,
    pub outline: i32,
    pub symbol_size: f32,
    pub stroke_rule: f32,
    pub stroke_frame: f32,
    pub stroke_selection: f32,
    pub stroke_focus: f32,
}

fn alpha(c: Color, a: f32) -> Color { Color::from_rgba(c.r, c.g, c.b, c.a * a) }

impl Palette {
    /// Reads the theme of `ctrl` for the plot (`chart` false) or the chart.
    pub fn load(ctrl: &Control, chart: bool) -> Palette {
        let plot = |name: &str| ctrl.get_theme_color_ex(name).theme_type("Plot").done();
        let ch = |name: &str| ctrl.get_theme_color_ex(name).theme_type("Chart").done();
        let ty = if chart { "Chart" } else { "Plot" };
        let constant = |name: &str, or: i32| { let v = ctrl.get_theme_constant_ex(name).theme_type(ty).done(); if v > 0 { v } else { or } };
        let font = ctrl.get_theme_font_ex("label").theme_type(ty).done().or_else(|| ctrl.get_theme_default_font());
        let font_bold = ctrl.get_theme_font_ex("label_bold").theme_type(ty).done().or_else(|| font.clone());
        let size = ctrl.get_theme_font_size_ex("label").theme_type(ty).done();
        let stance = [plot("stance_friend"), plot("stance_neutral"), plot("stance_suspect"), plot("stance_hostile")];
        let symbol_size = constant("symbol_size", 24).max(12) as f32;
        let strokes = (constant("stroke_rule", 1) as f32, constant("stroke_frame", 2) as f32, constant("stroke_selection", 3) as f32, constant("stroke_focus", 2) as f32);
        if chart {
            let ink = ch("text");
            Palette {
                chart, background: ch("water_deep"), shallows: ch("water_shallow"), land: ch("land"), coast: ink, grid: alpha(ink, 0.3), reference: ink, label: ink,
                label_outline: ch("label_outline"), overlay_label: ch("label_overlay"), route: ch("overlay"), uncertainty: ink, history: ink, select: ink, focus: ink,
                box_: ink, inactive: alpha(ink, 0.6), own: ink, belief: ch("overlay_tint"), stance: [ink; 4], font, font_bold,
                font_size: if size > 0 { size } else { 20 }, outline: constant("label_outline_size", 4), symbol_size,
                stroke_rule: strokes.0, stroke_frame: strokes.1, stroke_selection: strokes.2, stroke_focus: strokes.3,
            }
        } else {
            Palette {
                chart, background: plot("background"), shallows: plot("surface_base"), land: plot("surface_raised"), coast: plot("rule_divider"),
                grid: alpha(plot("rule_reference"), 0.3), reference: plot("rule_reference"), label: plot("label"), label_outline: plot("label_outline"),
                overlay_label: plot("label"), route: plot("text_secondary"), uncertainty: plot("uncertainty"), history: plot("history"), select: plot("select"),
                focus: plot("ui_focus"), box_: plot("ui_box"), inactive: plot("text_inactive"), own: plot("label"), belief: plot("text_secondary"), stance, font, font_bold,
                font_size: if size > 0 { size } else { 20 }, outline: constant("label_outline_size", 4), symbol_size,
                stroke_rule: strokes.0, stroke_frame: strokes.1, stroke_selection: strokes.2, stroke_focus: strokes.3,
            }
        }
    }
    pub fn stance(&self, s: Stance) -> Color { self.stance[match s { Stance::Friend => 0, Stance::Neutral => 1, Stance::Suspect => 2, Stance::Hostile => 3 }] }
}

fn v(p: Vec2) -> Vector2 { Vector2::new(p.x, p.y) }

/// Triangles waiting to go to the canvas item in one call.
#[derive(Default)]
struct Tris { points: Vec<Vector2>, colors: Vec<Color>, indices: Vec<i32> }
impl Tris {
    fn vertex(&mut self, p: Vec2, c: Color) -> i32 { self.points.push(v(p)); self.colors.push(c); self.points.len() as i32 - 1 }
    fn tri(&mut self, a: i32, b: i32, c: i32) { self.indices.extend_from_slice(&[a, b, c]); }
    /// A small filled disc (octagon).
    fn disc(&mut self, c: Vec2, r: f32, col: Color) {
        let centre = self.vertex(c, col);
        let first = self.points.len() as i32;
        for i in 0..8 { let a = i as f32 / 8.0 * TAU; self.vertex(c + Vec2::new(a.cos(), a.sin()) * r, col); }
        for i in 0..8 { self.tri(centre, first + i, first + (i + 1) % 8); }
    }
    fn flush(&mut self, item: Rid) {
        if self.indices.is_empty() || self.points.is_empty() { return; }
        let indices = PackedInt32Array::from(self.indices.as_slice());
        let points = PackedVector2Array::from(self.points.as_slice());
        let colors = PackedColorArray::from(self.colors.as_slice());
        RenderingServer::singleton().canvas_item_add_triangle_array(item, &indices, &points, &colors);
        *self = Tris::default();
    }
}

/// Draws the picture into `ctrl` (inside its `_draw`).
pub fn paint(ctrl: &mut Control, pic: &Picture, chart: Option<&Chart>, pal: &Palette, focused: bool) {
    let size = ctrl.get_size();
    let item = ctrl.get_canvas_item();
    ctrl.draw_rect(Rect2::new(Vector2::ZERO, size), pal.background);
    let Some(frame) = pic.frame else { return };
    let s = (pal.symbol_size / core::ICON_GRID).clamp(0.5, 4.0);

    // Water and land.
    if let (true, Some(chart)) = (pic.land, chart) {
        let mut t = Tris::default();
        for (layer, col) in [(&chart.shallows, pal.shallows), (&chart.land, pal.land)] {
            for p in layer.iter().filter(|p| !p.tris.is_empty()) {
                let base = t.points.len() as i32;
                for q in &p.pts { t.vertex(frame.to_local(*q), col); }
                t.indices.extend(p.tris.iter().map(|&i| base + i as i32));
            }
            t.flush(item);
        }
        for p in &chart.land {
            let mut line: Vec<Vector2> = p.pts.iter().map(|q| v(frame.to_local(*q))).collect();
            if let Some(f) = line.first().copied() { line.push(f); }
            ctrl.draw_polyline_ex(&PackedVector2Array::from(line.as_slice()), pal.coast).width(pal.stroke_rule).antialiased(true).done();
        }
    }

    // Graticule: crisp 1 px lines on pixel centres.
    if !pic.grid.is_empty() {
        let pts: Vec<Vector2> = pic.grid.iter().map(|p| Vector2::new(p.x.floor() + 0.5, p.y.floor() + 0.5)).collect();
        ctrl.draw_multiline_ex(&PackedVector2Array::from(pts.as_slice()), pal.grid).width(pal.stroke_rule).done();
    }

    // Belief samples under everything that moves.
    if !pic.belief.is_empty() {
        let mut t = Tris::default();
        for (p, w) in &pic.belief { t.disc(*p, 9.0 * s, alpha(pal.belief, 0.12 + 0.4 * w.clamp(0.0, 1.0))); }
        t.flush(item);
    }

    // Rings: range rings and envelopes are real boundaries, hard 1 px lines.
    for r in &pic.rings {
        let (col, w) = match r.kind {
            RingKind::Range => (pal.reference, pal.stroke_rule),
            RingKind::OwnEnvelope => (pal.reference, pal.stroke_frame),
            RingKind::Envelope(st) => (alpha(pal.stance(st), 0.7), pal.stroke_rule),
        };
        // A ring is on screen only between the nearest and farthest points of the control from its centre.
        let corners = [Vec2::ZERO, Vec2::new(frame.size.x, 0.0), frame.size, Vec2::new(0.0, frame.size.y)];
        let far = corners.iter().map(|k| k.distance(r.centre)).fold(0.0f32, f32::max);
        let near = Vec2::new(r.centre.x.clamp(0.0, frame.size.x), r.centre.y.clamp(0.0, frame.size.y)).distance(r.centre);
        if r.radius < near - w || r.radius > far + w { continue; }
        let n = ((r.radius / 3.0) as i32).clamp(48, 720);
        ctrl.draw_arc_ex(v(r.centre), r.radius, 0.0, TAU, n, col).width(w).antialiased(true).done();
    }

    // Route and objectives.
    if pic.route.len() >= 2 {
        let pts: Vec<Vector2> = pic.route.iter().map(|p| v(*p)).collect();
        ctrl.draw_polyline_ex(&PackedVector2Array::from(pts.as_slice()), pal.route).width(pal.stroke_frame).antialiased(true).done();
    }
    for w in &pic.waypoints {
        let h = 4.0 * s;
        ctrl.draw_rect_ex(Rect2::new(v(*w - Vec2::splat(h)), Vector2::new(2.0 * h, 2.0 * h)), pal.route).filled(false).width(pal.stroke_frame).done();
    }
    for o in &pic.objectives { pin(ctrl, *o, s, pal.label, pal.stroke_frame); }
    for (p, g) in &pic.markers {
        match g {
            MarkerGlyph::Objective => pin(ctrl, *p, s, pal.label, pal.stroke_frame),
            MarkerGlyph::Spawn => {
                ctrl.draw_arc_ex(v(*p), 6.0 * s, 0.0, TAU, 32, pal.own).width(pal.stroke_frame).antialiased(true).done();
            }
            MarkerGlyph::Site => {
                let h = 5.0 * s;
                ctrl.draw_rect_ex(Rect2::new(v(*p - Vec2::splat(h)), Vector2::new(2.0 * h, 2.0 * h)), pal.label).filled(false).width(pal.stroke_frame).done();
                ctrl.draw_circle(v(*p), 1.5 * s, pal.label);
            }
            MarkerGlyph::Other => { ctrl.draw_arc_ex(v(*p), 5.0 * s, 0.0, TAU, 24, pal.label).width(pal.stroke_frame).antialiased(true).done(); }
        }
    }

    // Uncertainty: graded value, never hue (rule 6). One triangle array for all shapes, one for samples.
    if !pic.shapes.is_empty() {
        let mut t = Tris::default();
        let solid = alpha(pal.uncertainty, 0.16);
        let clear = alpha(pal.uncertainty, 0.0);
        for g in &pic.shapes {
            let n = g.inner.len() as i32;
            if n < 3 || g.outer.len() as i32 != n { continue; }
            let base = t.points.len() as i32;
            for p in &g.inner { t.vertex(*p, solid); }
            for p in &g.outer { t.vertex(*p, clear); }
            if g.fan { t.vertex(g.centre, solid); }
            for i in g.core_tris() { t.indices.push(base + i as i32); }
            for i in 0..n {
                let j = (i + 1) % n;
                t.tri(base + i, base + n + i, base + n + j);
                t.tri(base + i, base + n + j, base + j);
            }
        }
        t.flush(item);
        let mut d = Tris::default();
        for p in &pic.samples { d.disc(*p, 1.5, alpha(pal.uncertainty, 0.85)); }
        d.flush(item);
    }

    // History.
    for line in &pic.history {
        let pts: Vec<Vector2> = line.iter().map(|p| v(*p)).collect();
        ctrl.draw_polyline_ex(&PackedVector2Array::from(pts.as_slice()), alpha(pal.history, 0.7)).width(pal.stroke_rule).antialiased(true).done();
    }

    // Leaders, one batch per stance.
    for st in [Stance::Friend, Stance::Neutral, Stance::Suspect, Stance::Hostile] {
        let pts: Vec<Vector2> = pic.leaders.iter().filter(|l| l.2 == st).flat_map(|l| [v(l.0), v(l.1)]).collect();
        if !pts.is_empty() { ctrl.draw_multiline_ex(&PackedVector2Array::from(pts.as_slice()), alpha(pal.stance(st), 0.8)).width(pal.stroke_rule).antialiased(true).done(); }
    }

    // Mines.
    if !pic.mines.is_empty() {
        let mut t = Tris::default();
        for m in &pic.mines { t.disc(*m, 3.0 * s, pal.stance(Stance::Friend)); }
        t.flush(item);
    }

    // Emitters (two arcs off the symbol's upper right) and symbols: every stroke in one call, every solid
    // mark in one triangle array.
    let mut segs = Segs::default();
    let mut dots = Tris::default();
    for (p, st) in &pic.emitters {
        let col = pal.stance(*st);
        for r in [10.0 * s, 13.5 * s] {
            let arc: Vec<Vec2> = (0..=6).map(|i| { let a = -PI / 4.0 - 0.45 + 0.9 * i as f32 / 6.0; *p + Vec2::new(a.cos(), a.sin()) * r }).collect();
            segs.polyline(&arc, col, 0.0);
        }
    }

    for sym in &pic.symbols { symbol(&mut segs, &mut dots, sym, s, pal); }
    segs.flush(ctrl, pal.stroke_frame);
    dots.flush(item);

    // Change boxes and the selection bracket (white, 3 px).
    for b in &pic.boxes {
        let h = 15.0 * s;
        ctrl.draw_rect_ex(Rect2::new(v(*b - Vec2::splat(h)), Vector2::new(2.0 * h, 2.0 * h)), pal.box_).filled(false).width(pal.stroke_frame).done();
    }
    if let Some(c) = pic.bracket {
        let (h, leg) = (13.0 * s, 5.0 * s);
        let mut pts = Vec::with_capacity(16);
        for (sx, sy) in [(-1.0f32, -1.0f32), (1.0, -1.0), (1.0, 1.0), (-1.0, 1.0)] {
            let corner = c + Vec2::new(sx * h, sy * h);
            pts.extend_from_slice(&[v(corner), v(corner - Vec2::new(sx * leg, 0.0)), v(corner), v(corner - Vec2::new(0.0, sy * leg))]);
        }
        ctrl.draw_multiline_ex(&PackedVector2Array::from(pts.as_slice()), pal.select).width(pal.stroke_selection).done();
    }

    // Tool line and cursor.
    if let Some((a, b)) = pic.tool_line {
        let col = if pal.chart { pal.route } else { pal.label };
        ctrl.draw_line_ex(v(a), v(b), col).width(pal.stroke_frame).antialiased(true).done();
        for p in [a, b] { ctrl.draw_arc_ex(v(p), 3.0 * s, 0.0, TAU, 16, col).width(pal.stroke_frame).antialiased(true).done(); }
    }
    if let Some((c, snapped)) = pic.cursor {
        let r = if snapped { 15.0 * s } else { 5.0 * s };
        ctrl.draw_arc_ex(v(c), r, 0.0, TAU, 40, pal.focus).width(pal.stroke_focus).antialiased(true).done();
    }

    // Scale bar with end ticks (`draw_multiline` for ticks, L1735).
    if let Some((start, len)) = pic.scale_bar {
        let (a, b) = (Vector2::new(start.x.floor() + 0.5, start.y.floor() + 0.5), Vector2::new((start.x + len).floor() + 0.5, start.y.floor() + 0.5));
        let tick = 6.0;
        let pts = [a, b, a, a - Vector2::new(0.0, tick), b, b - Vector2::new(0.0, tick)];
        ctrl.draw_multiline_ex(&PackedVector2Array::from(&pts[..]), pal.reference).width(pal.stroke_rule).done();
    }

    // Labels last, on top of everything they name.
    for l in pic.labels.iter().chain(pic.scale_label.iter()) {
        let col = match l.role { LabelRole::Track => pal.label, LabelRole::Reference => if pal.chart { pal.label } else { pal.reference }, LabelRole::Overlay => pal.overlay_label };
        if l.plate {
            // A label on its own line (ring, graticule) cuts the line under it.
            let r = l.rect.grow(2.0);
            ctrl.draw_rect(Rect2::new(v(r.min), v(r.size())), pal.background);
        }
        text_block(ctrl, pal, l.rect.min, &l.lines, col, l.bold);
    }
    if let Some(m) = &pic.message {
        if let Some(font) = pal.font.as_ref() {
            let w = font.get_string_size_ex(m.as_str()).font_size(pal.font_size).done().x;
            let pos = Vec2::new((frame.size.x - w) * 0.5, frame.size.y * 0.5);
            text_block(ctrl, pal, pos, std::slice::from_ref(m), pal.label, false);
        }
    }
    if focused {
        let inset = pal.stroke_focus * 0.5;
        ctrl.draw_rect_ex(Rect2::new(Vector2::new(inset, inset), size - Vector2::new(2.0 * inset, 2.0 * inset)), pal.focus).filled(false).width(pal.stroke_focus).done();
    }
}

fn text_block(ctrl: &mut Control, pal: &Palette, top_left: Vec2, lines: &[String], col: Color, bold: bool) {
    let Some(font) = (if bold { pal.font_bold.as_ref() } else { pal.font.as_ref() }) else { return };
    let ascent = font.get_ascent_ex().font_size(pal.font_size).done();
    let line_h = font.get_height_ex().font_size(pal.font_size).done();
    for (i, line) in lines.iter().enumerate() {
        let pos = Vector2::new(top_left.x + 1.0, top_left.y + ascent + i as f32 * line_h);
        if pal.outline > 0 {
            ctrl.draw_string_outline_ex(font, pos, line.as_str()).font_size(pal.font_size).size(pal.outline).modulate(pal.label_outline).done();
        }
        ctrl.draw_string_ex(font, pos, line.as_str()).font_size(pal.font_size).modulate(col).done();
    }
}

/// The `pin` icon with its point on `p`.
fn pin(ctrl: &mut Control, p: Vec2, s: f32, col: Color, w: f32) {
    let o = |x: f32, y: f32| v(p + Vec2::new(x - 12.0, y - 21.0) * s);
    let head = [o(9.0, 3.0), o(15.0, 3.0), o(14.0, 9.0), o(18.0, 13.0), o(6.0, 13.0), o(10.0, 9.0), o(9.0, 3.0)];
    ctrl.draw_polyline_ex(&PackedVector2Array::from(&head[..]), col).width(w).antialiased(true).done();
    ctrl.draw_line_ex(o(12.0, 13.0), o(12.0, 21.0), col).width(w).antialiased(true).done();
}

/// Line segments in one colour each, drawn in a single call.
#[derive(Default)]
struct Segs { pts: Vec<Vector2>, cols: Vec<Color> }
impl Segs {
    /// A segment grown by `cap` at both ends, so 2 px strokes meet at corners without notches.
    fn seg(&mut self, a: Vec2, b: Vec2, col: Color, cap: f32) {
        let d = b - a;
        let len = d.length();
        if !(len.is_finite() && len > 1e-3) { return; }
        let e = d / len * cap;
        self.pts.push(v(a - e));
        self.pts.push(v(b + e));
        self.cols.push(col);
    }
    fn polyline(&mut self, pts: &[Vec2], col: Color, cap: f32) { for w in pts.windows(2) { self.seg(w[0], w[1], col, cap); } }
    fn flush(&mut self, ctrl: &mut Control, width: f32) {
        if self.cols.is_empty() { return; }
        ctrl.draw_multiline_colors_ex(&PackedVector2Array::from(self.pts.as_slice()), &PackedColorArray::from(self.cols.as_slice())).width(width).antialiased(true).done();
        *self = Segs::default();
    }
}

/// One symbol's strokes and solid marks.
fn symbol(segs: &mut Segs, dots: &mut Tris, sym: &Symbol, s: f32, pal: &Palette) {
    let col = match sym.kind {
        SymKind::Own => pal.own,
        SymKind::Ally => pal.stance(Stance::Friend),
        SymKind::Track if sym.last_known => pal.inactive,
        SymKind::Track => pal.stance(sym.stance),
    };
    let c = sym.pos;
    let cap = pal.stroke_frame * 0.5;
    let outline: Vec<Vec2> = core::outline_points(&core::stance_outline(sym.stance, s), 20).into_iter().map(|p| c + p).collect();
    // Curves join smoothly without caps; polygon corners need them.
    let curved = matches!(core::stance_outline(sym.stance, s), Outline::Circle { .. } | Outline::Capsule { .. });
    segs.polyline(&outline, col, if curved { 0.0 } else { cap });
    match sym.domain {
        Domain::Air | Domain::Subsurface => {
            let (a, b) = core::domain_bar(sym.domain == Domain::Air, s);
            segs.seg(c + a, c + b, col, 0.0);
        }
        Domain::Weapon => dots.disc(c + core::weapon_dot(sym.stance, s), 2.0 * s, col),
        Domain::Surface => {}
    }
    // Climb or dive: the trend chevron at the symbol's left (rule 5).
    // Steady is a glyph too for anything off the surface (the `trend_steady` dash); surface tracks have none.
    if sym.kind == SymKind::Track && !sym.last_known && sym.domain != Domain::Surface {
        let o = c + Vec2::new(-12.5 * s, 0.0);
        match sym.trend {
            VerticalTrend::Climbing | VerticalTrend::Diving => {
                let dir = if sym.trend == VerticalTrend::Climbing { -1.0f32 } else { 1.0 };
                segs.polyline(&[o + Vec2::new(-3.0, -dir * 2.0) * s, o + Vec2::new(0.0, dir * 2.0) * s, o + Vec2::new(3.0, -dir * 2.0) * s], col, cap);
            }
            VerticalTrend::Steady => segs.seg(o + Vec2::new(-3.0, 0.0) * s, o + Vec2::new(3.0, 0.0) * s, col, 0.0),
        }
    }
    // Heading tick for hulls drawn at their pose (the `track_own` icon's tick, turned to the heading).
    if let Some(h) = sym.heading.filter(|h| h.is_finite()) {
        let d = core::bearing_dir(h);
        segs.seg(c + d * 6.0 * s, c + d * 10.0 * s, col, 0.0);
    }
}
