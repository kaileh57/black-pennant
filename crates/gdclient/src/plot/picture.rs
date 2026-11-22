//! Part of unit `gd-plot`. What the plot shows this redraw, built from the newest view frame, its snapshot,
//! the HUD's selection and history, and the static chart, as plain data in local pixels. No Godot calls:
//! text comes through [`Env`], so the whole build is unit-tested headless.
//!
//! Decisions where the spec is silent:
//! - Track labels read "0412 Hostile" (number and stance word, rule 4: stance by frame, tint and label);
//!   last-known marks "0412 last known". The selected track adds a second line with its class (rung Class or
//!   Exact) or its ID rung word; the designated track's second line starts "Designated".
//! - Label ranks extend rule 14 below waypoints' neighbours: own hull and selected 0, inbound weapons 1,
//!   Hostile in own weapon range 2, Hostile outside 3, Suspect 4, Unknown rung 5, Neutral, Friend and allied
//!   hulls 6, last-known marks 7, objectives and chart markers 8, ring labels 9, waypoints 10, graticule
//!   labels 11. Inside a rank the HUD's threat value decides, then the key, so the order never flickers.
//! - A track paired with a vessel inside visual range is drawn at the vessel's pose, as the bridge view's
//!   marker is; any other at its estimate carried forward to render time (`ViewFrame::tracks`).
//! - Weapon envelopes are drawn for Suspect and Hostile tracks only (the ones the compression test of L113
//!   uses); Friend and Neutral never block anything, so their rings would be clutter.
//! - Moving tracks get a one-minute leader in their stance tint, listed in the legend.
//! - Changes get the box (rules 5 and 7): a 2 px `ui.box` square for 10 s after a rung or stance change, a
//!   merge (the surviving number) or a split (both numbers).
#![allow(clippy::disallowed_methods)]
use super::chart::{Chart, MarkerGlyph};
use super::core::{self, Frame, LabelReq, Rect, ShapeGeom};
use super::PlotMode;
use crate::hud::tracks::{band, geometry, rank as band_rank, threat_value, Band, Own};
use crate::interp::ViewFrame;
use crate::text::{fill, number_text, range_text, Lookup};
use content::enums::{Domain, IdRung, Stance};
use content::ids::{ClassId, MunitionIdx};
use glam::{DVec2, DVec3, Vec2};
use sim_core::EntityId;
use sim_systems::ids::TrackId;
use sim_systems::protocol::{ObjectiveState, Relevance, Snapshot, TrackView, VerticalTrend};
use std::collections::BTreeMap;

/// Layer bits (`Bp.PLOT_*`).
pub mod layer {
    pub const TRACKS: i64 = 1;
    pub const UNCERTAINTY: i64 = 2;
    pub const ENVELOPES: i64 = 4;
    pub const EMITTERS: i64 = 8;
    pub const ROUTE: i64 = 16;
    pub const RINGS: i64 = 32;
    pub const GRATICULE: i64 = 64;
    pub const HISTORY: i64 = 128;
    pub const LAND: i64 = 256;
    pub const OBJECTIVES: i64 = 512;
    pub const MINES: i64 = 1024;
    pub const ALLIES: i64 = 2048;
    pub const LABELS: i64 = 4096;
    pub const ALL: i64 = 8191;
    /// Everything except history (the contract's default).
    pub const DEFAULT: i64 = ALL & !HISTORY;
}

/// Seconds of travel a leader shows: a minute for ships, ten seconds for aircraft and weapons (a minute of
/// a missile crosses the whole plot).
pub const LEADER_S: f64 = 60.0;
pub const LEADER_FAST_S: f64 = 10.0;
/// Slower than this a track gets no leader.
const LEADER_MIN_MPS: f64 = 0.5;
/// Range rings and graticule lines keep at least this far apart on screen.
const RING_MIN_PX: f64 = 150.0;
const GRID_MIN_PX: f64 = 180.0;
/// Most rings and graticule lines per axis.
const RING_CAP: usize = 16;
const GRID_CAP: usize = 40;
/// Most history points kept per track on screen.
const HISTORY_MIN_STEP_PX: f32 = 2.0;
/// Label keys by kind, so every label has a stable identity across redraws.
const KEY_OWN: u64 = 1 << 40;
const KEY_ALLY: u64 = 2 << 40;
const KEY_RING: u64 = 3 << 40;
const KEY_WAYPOINT: u64 = 4 << 40;
const KEY_OBJECTIVE: u64 = 5 << 40;
const KEY_MARKER: u64 = 6 << 40;
const KEY_GRID: u64 = 7 << 40;
const KEY_ENVELOPE: u64 = 8 << 40;
const KEY_TOOL: u64 = 9 << 40;

/// Ranks beyond the HUD's track ranks (module docs).
const RANK_ALLY: u8 = 6;
const RANK_OBJECTIVE: u8 = 8;
const RANK_RING: u8 = 9;
const RANK_WAYPOINT: u8 = 10;
const RANK_GRID: u8 = 11;

/// Everything the build needs from the engine, behind a trait so tests run without it.
pub trait Env {
    fn lk(&self) -> &dyn Lookup;
    /// Width and height of one line of label text.
    fn measure(&self, text: &str) -> Vec2;
    fn class_name(&self, class: &ClassId) -> Option<String>;
    /// The name an own-side hull was introduced with.
    fn vessel_name(&self, entity: EntityId) -> Option<String>;
    fn is_mine(&self, kind: MunitionIdx) -> bool;
}

/// The plot's interaction state that shows in the picture.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Overlay {
    /// A tool line being drawn or a measurement (map positions) and whether it is a measurement.
    pub line: Option<(DVec2, DVec2, bool)>,
    /// The gamepad cursor in local pixels, if shown.
    pub cursor: Option<Vec2>,
}

/// What the build reads.
pub struct Inputs<'a> {
    pub mode: PlotMode,
    pub layers: i64,
    pub frame: Frame,
    /// The live view (sortie and replay); `None` for the static charts.
    pub view: Option<&'a ViewFrame>,
    pub selected: TrackId,
    pub hovered: TrackId,
    pub history: Option<&'a BTreeMap<TrackId, Vec<[f32; 2]>>>,
    /// Tracks inside their 10 s change box, ascending.
    pub boxed: &'a [TrackId],
    pub chart: Option<&'a Chart>,
    /// Briefing route (map positions).
    pub route: &'a [DVec2],
    /// After-action belief samples (x, z, weight).
    pub belief: &'a [[f64; 3]],
    pub prev_sides: &'a BTreeMap<u64, u8>,
    pub overlay: Overlay,
    /// Theme `symbol_size` (24 at 100%).
    pub symbol_size: f32,
    /// Margins (left, top, right, bottom, local pixels) that HUD clusters cover; the graticule's
    /// edge labels sit just inside them so no cluster hides them.
    pub clear: [f32; 4],
    /// HUD clusters over the plot (local rectangles): labels take another side or hide.
    pub blocked: &'a [Rect],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SymKind { Track, Own, Ally }

/// One symbol to draw.
#[derive(Clone, Debug, PartialEq)]
pub struct Symbol {
    pub kind: SymKind,
    pub pos: Vec2,
    pub stance: Stance,
    pub domain: Domain,
    pub trend: VerticalTrend,
    pub last_known: bool,
    pub emitting: bool,
    /// Heading for own and allied hulls (radians, compass).
    pub heading: Option<f32>,
    pub track: TrackId,
    /// Bearing from the own hull in degrees, -1 without one (accessible name).
    pub bearing_deg: f64,
    /// Accessible name (tracks only).
    pub name: String,
    /// Declutter rank of the thing the symbol shows (0 own hull and selection).
    pub rank: u8,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RingKind { Range, OwnEnvelope, Envelope(Stance) }

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Ring { pub centre: Vec2, pub radius: f32, pub kind: RingKind }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LabelRole { Track, Reference, Overlay }

#[derive(Clone, Debug, PartialEq)]
pub struct Label {
    pub rect: Rect,
    pub lines: Vec<String>,
    pub role: LabelRole,
    pub track: TrackId,
    pub bold: bool,
    /// Drawn on a plate of the background colour: a label sitting on its own line (rings, graticule).
    pub plate: bool,
    /// Shown whatever it overlaps (hover, cursor, tool readouts).
    pub forced: bool,
}

/// The whole picture in local pixels, bottom to top.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Picture {
    pub chart_palette: bool,
    pub frame: Option<Frame>,
    pub land: bool,
    pub grid: Vec<Vec2>,
    pub rings: Vec<Ring>,
    pub route: Vec<Vec2>,
    pub waypoints: Vec<Vec2>,
    pub objectives: Vec<Vec2>,
    pub markers: Vec<(Vec2, MarkerGlyph)>,
    pub shapes: Vec<ShapeGeom>,
    pub samples: Vec<Vec2>,
    pub history: Vec<Vec<Vec2>>,
    pub leaders: Vec<(Vec2, Vec2, Stance)>,
    pub emitters: Vec<(Vec2, Stance)>,
    pub belief: Vec<(Vec2, f32)>,
    pub mines: Vec<Vec2>,
    pub symbols: Vec<Symbol>,
    pub labels: Vec<Label>,
    /// Tracks whose labels declutter hid, with their symbol position.
    pub hidden: Vec<(TrackId, Vec2)>,
    /// Sides each placed label took (for next redraw's hysteresis).
    pub sides: BTreeMap<u64, u8>,
    pub bracket: Option<Vec2>,
    pub boxes: Vec<Vec2>,
    pub tool_line: Option<(Vec2, Vec2)>,
    pub cursor: Option<(Vec2, bool)>,
    /// Scale bar: left end, length in pixels.
    pub scale_bar: Option<(Vec2, f32)>,
    pub scale_label: Option<Label>,
    /// Centred message when there is nothing to show.
    pub message: Option<String>,
    /// Legend rows for what is drawn: (icon, text key).
    pub legend: Vec<(&'static str, &'static str)>,
}

impl Picture {
    /// Pairs of a placed label and a symbol of rank 3 or better that it covers (the top priorities of rule
    /// 14 must stay unoverlapped, L1764). Hover and readout labels do not count.
    pub fn top_covers(&self, symbol_half: f32) -> usize {
        self.labels.iter().filter(|l| !l.forced)
            .map(|l| self.symbols.iter().filter(|s| s.rank <= core::TOP_RANK && !(s.kind == SymKind::Track && s.track == l.track) && Rect::around(s.pos, symbol_half).overlaps(&l.rect)).count())
            .sum()
    }
    /// Index of the symbol under a local point: the nearest symbol within the hit radius, else a track
    /// label containing it.
    pub fn hit(&self, at: Vec2) -> Option<usize> { self.hit_within(at, core::HIT_RADIUS_PX) }
    /// The nearest track symbol within `radius`, else a track label containing the point. Own and allied
    /// hulls are never hit: they are not tracks.
    pub fn hit_within(&self, at: Vec2, radius: f32) -> Option<usize> {
        let pts: Vec<Vec2> = self.symbols.iter().map(|s| if s.kind == SymKind::Track { s.pos } else { Vec2::splat(f32::NAN) }).collect();
        if let Some(i) = core::nearest_within(&pts, at, radius) { return Some(i); }
        let l = self.labels.iter().find(|l| l.role == LabelRole::Track && !l.track.is_none() && l.rect.contains(at))?;
        self.symbols.iter().position(|s| s.kind == SymKind::Track && s.track == l.track)
    }
}

/// Text keys this file uses that the plot's CSV owns.
pub const TEXT_KEYS: &[&str] = &[
    "plot.label.track", "plot.label.last_known", "plot.label.own", "plot.label.ally", "plot.label.designated", "plot.label.designated_with",
    "plot.label.waypoint", "plot.label.rung", "plot.ring.own_envelope", "plot.ring.envelope", "plot.grid.east", "plot.grid.west", "plot.grid.north",
    "plot.grid.south", "plot.bearing_range", "plot.empty.sortie", "plot.empty.chart",
    "plot.legend.own", "plot.legend.ally", "plot.legend.leader", "plot.legend.envelope", "plot.legend.emitter", "plot.legend.route",
    "plot.legend.objective", "plot.legend.mine", "plot.legend.history", "plot.legend.changed", "plot.legend.belief", "plot.legend.marker",
    "plot.legend.range_ring", "plot.legend.steady",
];

struct Builder<'a, 'e> {
    inp: &'a Inputs<'a>,
    env: &'e dyn Env,
    pic: Picture,
    reqs: Vec<(LabelReq, Label)>,
    line_h: f32,
    s: f32,
}

fn has(layers: i64, bit: i64) -> bool { layers & bit != 0 }

/// A distance as the HUD writes ranges, but whole kilometres without a decimal ("5 km", "2.5 km",
/// "500 m"): reference lines fall on round numbers.
pub fn round_range_text(lk: &dyn Lookup, metres: f64) -> String {
    let m = if metres.is_finite() { metres.abs() } else { 0.0 };
    if m >= 1_000.0 && (m / 1_000.0).fract().abs() < 1e-9 { return fill(&lk.tr("units.unit.km"), &[number_text(lk, m / 1_000.0, 0)]); }
    if m >= 1_000.0 && ((m / 100.0).fract().abs() < 1e-9) { return fill(&lk.tr("units.unit.km"), &[number_text(lk, m / 1_000.0, 1)]); }
    crate::text::range_text(lk, m)
}
fn flat(v: DVec3) -> DVec2 { DVec2::new(v.x, v.z) }

/// The own hull's map position in a view, for following.
pub fn own_map_pos(view: &ViewFrame) -> Option<DVec2> {
    let v = view.vessels.get(view.own? as usize)?;
    let w = view.origin.to_world(v.pos);
    let p = DVec2::new(w.x, w.z);
    p.is_finite().then_some(p)
}

/// A track's drawn map position: its vessel's pose when one inside visual range is paired with it (and is
/// not the viewer's), else its carried estimate.
fn track_map_pos(view: &ViewFrame, id: TrackId, carried: [f64; 2]) -> DVec2 {
    if let Some(v) = view.vessels.iter().find(|v| v.track == id && !v.own) {
        let w = view.origin.to_world(v.pos);
        if w.is_finite() { return DVec2::new(w.x, w.z); }
    }
    DVec2::new(carried[0], carried[1])
}

fn heading_of(rot: [f32; 4]) -> Option<f32> {
    let q = glam::Quat::from_xyzw(rot[0], rot[1], rot[2], rot[3]);
    if !q.is_finite() || q.length_squared() < 0.5 { return None; }
    let f = q.normalize() * glam::Vec3::X;
    (f.x * f.x + f.z * f.z > 1e-6).then(|| f.x.atan2(-f.z))
}

/// Builds the picture. `env.measure` sizes labels; nothing else touches the engine.
pub fn build(inp: &Inputs<'_>, env: &dyn Env) -> Picture {
    let s = (core::finite_or(inp.symbol_size, core::ICON_GRID) / core::ICON_GRID).clamp(0.5, 4.0);
    let line_h = env.measure("0").y.max(1.0);
    let mut b = Builder { inp, env, pic: Picture { frame: Some(inp.frame), chart_palette: matches!(inp.mode, PlotMode::Briefing | PlotMode::AfterAction), ..Default::default() }, reqs: Vec::new(), line_h, s };
    let live = inp.view.filter(|_| matches!(inp.mode, PlotMode::Sortie | PlotMode::Replay));
    let snapshot = live.and_then(|v| v.snapshot.as_deref());

    b.chart_layer();
    if has(inp.layers, layer::GRATICULE) { b.graticule(); }
    match (live, snapshot) {
        (Some(view), Some(snap)) => b.live(view, snap),
        (Some(view), None) if !view.vessels.is_empty() => b.live(view, &Snapshot::default()),
        _ => {}
    }
    if !matches!(inp.mode, PlotMode::Sortie | PlotMode::Replay) || live.is_none() { b.static_route(); }
    b.belief();
    b.overlay();
    b.scale_bar();
    b.place_labels();
    let nothing = b.pic.symbols.is_empty() && !b.pic.land && b.pic.markers.is_empty() && b.pic.route.is_empty() && b.pic.belief.is_empty();
    if nothing {
        // An empty plot says so, with no reference lines around nothing.
        let key = if b.pic.chart_palette { "plot.empty.chart" } else { "plot.empty.sortie" };
        b.pic = Picture { chart_palette: b.pic.chart_palette, frame: b.pic.frame, message: Some(env.lk().tr(key)), ..Default::default() };
    }
    b.pic
}

impl Builder<'_, '_> {
    fn frame(&self) -> &Frame { &self.inp.frame }
    fn tr(&self, key: &str) -> String { self.env.lk().tr(key) }
    fn legend(&mut self, row: (&'static str, &'static str)) { if !self.pic.legend.contains(&row) { self.pic.legend.push(row); } }

    fn label(&mut self, req: LabelReq, lines: Vec<String>, role: LabelRole, track: TrackId, bold: bool) {
        let mut req = req;
        let w = lines.iter().map(|l| self.env.measure(l).x).fold(0.0f32, f32::max);
        req.size = Vec2::new(w.ceil(), self.line_h * lines.len().max(1) as f32);
        req.prev_side = self.inp.prev_sides.get(&req.key).copied();
        let (plate, forced) = (req.fixed, req.force);
        self.reqs.push((req, Label { rect: Rect::at(Vec2::ZERO, Vec2::ZERO), lines, role, track, bold, plate, forced }));
    }
    fn req(anchor: Vec2, clear: f32, rank: u8, weight: f64, key: u64) -> LabelReq {
        // The top ranks (own hull, selection, inbound weapons, Hostile) may cover a symbol rather than hide.
        LabelReq { anchor, clear, size: Vec2::ZERO, rank, weight, key, prev_side: None, force: false, fixed: false, cover: rank <= 3 }
    }

    fn chart_layer(&mut self) {
        let Some(chart) = self.inp.chart else { return };
        let f = *self.frame();
        if has(self.inp.layers, layer::LAND) && (!chart.land.is_empty() || !chart.shallows.is_empty()) { self.pic.land = true; }
        if self.pic.chart_palette || has(self.inp.layers, layer::OBJECTIVES) {
            for (i, m) in chart.markers.iter().enumerate() {
                // The start point means nothing once the sortie runs.
                if m.glyph == MarkerGlyph::Spawn && !self.pic.chart_palette { continue; }
                let p = f.to_local(m.pos);
                if !f.contains(p, 64.0) { continue; }
                self.pic.markers.push((p, m.glyph));
                // Marker names in map data are identifiers; the player reads the kind.
                let text = if !m.label_key.is_empty() { self.tr(&m.label_key) } else { m.name.clone() };
                if !text.trim().is_empty() && has(self.inp.layers, layer::LABELS) {
                    self.label(Self::req(p, 8.0 * self.s, RANK_OBJECTIVE, 0.0, KEY_MARKER | i as u64), vec![text], LabelRole::Reference, TrackId::NONE, false);
                }
                self.legend(("cursor_ring", "plot.legend.marker"));
            }
        }
    }

    fn graticule(&mut self) {
        let f = *self.frame();
        if f.size.x < 2.0 || f.size.y < 2.0 { return; }
        let step = core::grid_step(f.mpp, GRID_MIN_PX);
        let (lo, hi) = f.map_bounds(0.0);
        let lk = self.env.lk();
        let km = |v: f64| round_range_text(lk, v.abs());
        // Edge labels keep to the part of the plot no HUD cluster covers.
        let c = self.inp.clear.map(|v| if v.is_finite() { v.max(0.0) } else { 0.0 });
        let (left, top, right, bottom) = (c[0], c[1], f.size.x - c[2], f.size.y - c[3]);
        for x in core::multiples(lo.x, hi.x, step, GRID_CAP) {
            let px = f.to_local(DVec2::new(x, 0.0)).x;
            self.pic.grid.extend_from_slice(&[Vec2::new(px, 0.0), Vec2::new(px, f.size.y)]);
            let text = if x.abs() < 0.5 { km(0.0) } else { fill(&lk.tr(if x > 0.0 { "plot.grid.east" } else { "plot.grid.west" }), &[km(x)]) };
            if has(self.inp.layers, layer::LABELS) {
                let size = self.env.measure(&text);
                if px - size.x * 0.5 < left || px + size.x * 0.5 > right { continue; }
                let mut r = Self::req(Vec2::new(px, top + 6.0 + size.y * 0.5), 0.0, RANK_GRID, 0.0, KEY_GRID | (x / step).round() as i64 as u64 & 0xFFFF_FFFF);
                r.fixed = true;
                self.label(r, vec![text], LabelRole::Reference, TrackId::NONE, false);
            }
        }
        for z in core::multiples(lo.y, hi.y, step, GRID_CAP) {
            let py = f.to_local(DVec2::new(0.0, z)).y;
            self.pic.grid.extend_from_slice(&[Vec2::new(0.0, py), Vec2::new(f.size.x, py)]);
            // Map z grows southward.
            let text = if z.abs() < 0.5 { km(0.0) } else { fill(&lk.tr(if z < 0.0 { "plot.grid.north" } else { "plot.grid.south" }), &[km(z)]) };
            if has(self.inp.layers, layer::LABELS) {
                let size = self.env.measure(&text);
                if py - size.y * 0.5 < top || py + size.y * 0.5 > bottom { continue; }
                let mut r = Self::req(Vec2::new(left + 6.0 + size.x * 0.5, py), 0.0, RANK_GRID, 0.0, (KEY_GRID | 1 << 39) | ((z / step).round() as i64 as u64 & 0xFFFF_FFFF));
                r.fixed = true;
                self.label(r, vec![text], LabelRole::Reference, TrackId::NONE, false);
            }
        }
    }

    fn ring_label(&mut self, centre: Vec2, radius: f32, text: String, key: u64, rank: u8) {
        if !has(self.inp.layers, layer::LABELS) { return; }
        let mut r = Self::req(centre - Vec2::new(0.0, radius), 0.0, rank, 0.0, key);
        r.fixed = true;
        self.label(r, vec![text], LabelRole::Reference, TrackId::NONE, false);
    }

    fn live(&mut self, view: &ViewFrame, snap: &Snapshot) {
        let f = *self.frame();
        let layers = self.inp.layers;
        let lk = self.env.lk();
        let own_vessel = view.own.and_then(|i| view.vessels.get(i as usize));
        let own_pos = own_map_pos(view);
        let ship = snap.ship.as_ref();
        let own_geo = own_pos.map(|p| Own {
            pos: p,
            vel: own_vessel.map_or(DVec2::ZERO, |v| DVec2::new(v.vel[0] as f64, v.vel[2] as f64)),
            envelope_m: ship.map_or(0.0, |s| if s.longest_envelope_m.is_finite() { s.longest_envelope_m.max(0.0) as f64 } else { 0.0 }),
        });
        let own_local = own_pos.map(|p| f.to_local(p));

        // Range rings and the own weapon envelope.
        if let Some(c) = own_local {
            if has(layers, layer::RINGS) {
                let step = core::ring_step(f.mpp, RING_MIN_PX);
                let far = [Vec2::ZERO, Vec2::new(f.size.x, 0.0), f.size, Vec2::new(0.0, f.size.y)].iter().map(|k| k.distance(c)).fold(0.0f32, f32::max) as f64 * f.mpp;
                let mut k = 1;
                while (k as f64) * step <= far && k <= RING_CAP {
                    let r = f.px(k as f64 * step);
                    self.pic.rings.push(super::picture::Ring { centre: c, radius: r, kind: RingKind::Range });
                    let text = round_range_text(lk, k as f64 * step);
                    self.ring_label(c, r, text, KEY_RING | k as u64, RANK_RING);
                    k += 1;
                }
                if k > 1 { self.legend(("shape_circle", "plot.legend.range_ring")); }
            }
            if has(layers, layer::ENVELOPES) {
                if let Some(e) = own_geo.map(|o| o.envelope_m).filter(|e| *e > 0.0) {
                    let r = f.px(e);
                    if r > 4.0 && r < 100_000.0 {
                        self.pic.rings.push(Ring { centre: c, radius: r, kind: RingKind::OwnEnvelope });
                        let text = fill(&lk.tr("plot.ring.own_envelope"), &[range_text(lk, e)]);
                        // Its label sits on the ring's south side, away from the range labels.
                        if has(layers, layer::LABELS) {
                            let mut q = Self::req(c + Vec2::new(0.0, r), 0.0, RANK_RING, 1.0, KEY_RING);
                            q.fixed = true;
                            self.label(q, vec![text], LabelRole::Reference, TrackId::NONE, false);
                        }
                        self.legend(("shape_circle", "plot.legend.envelope"));
                    }
                }
            }
        }

        // Planned route.
        if has(layers, layer::ROUTE) {
            if let (Some(c), Some(route)) = (own_local, ship.map(|s| &s.route).filter(|r| !r.is_empty())) {
                // The first leg leaves from the own symbol's edge.
                let first = route.iter().find(|w| w.is_finite()).map(|w| f.to_local(*w)).filter(|w| w.distance(c) > 1.0);
                self.pic.route.push(first.map_or(c, |w| c + (w - c).normalize() * core::symbol_half(self.s) * 0.75));
                for (i, w) in route.iter().enumerate().filter(|(_, w)| w.is_finite()) {
                    let p = f.to_local(*w);
                    self.pic.route.push(p);
                    self.pic.waypoints.push(p);
                    if has(layers, layer::LABELS) {
                        let text = fill(&lk.tr("plot.label.waypoint"), &[number_text(lk, i as f64 + 1.0, 0)]);
                        self.label(Self::req(p, 6.0, RANK_WAYPOINT, 0.0, KEY_WAYPOINT | i as u64), vec![text], LabelRole::Reference, TrackId::NONE, false);
                    }
                }
                self.legend(("line_solid", "plot.legend.route"));
            }
        }

        // Objectives.
        if has(layers, layer::OBJECTIVES) {
            for o in snap.objectives.iter().filter(|o| o.state == ObjectiveState::Active) {
                let Some(m) = o.marker.filter(|m| m.is_finite()) else { continue };
                let p = f.to_local(m);
                if !f.contains(p, 32.0) { continue; }
                self.pic.objectives.push(p);
                let text = crate::text::msg_text(lk, &o.text);
                if has(layers, layer::LABELS) && !text.is_empty() {
                    self.label(Self::req(p, 8.0 * self.s, RANK_OBJECTIVE, 0.0, KEY_OBJECTIVE | o.id.0 as u64), vec![text], LabelRole::Reference, TrackId::NONE, false);
                }
                self.legend(("pin", "plot.legend.objective"));
            }
        }

        // Allied hulls: own-side vessels other than the viewer's.
        let allies: Vec<&crate::interp::VesselPose> = view.vessels.iter().enumerate()
            .filter(|(i, v)| Some(*i as u32) != view.own && !v.own && snap.vessels.iter().find(|s| s.entity == v.entity).is_some_and(|s| s.relevance == Relevance::OwnSide))
            .map(|(_, v)| v).collect();
        let ally_tracks: Vec<TrackId> = allies.iter().map(|v| v.track).filter(|t| !t.is_none()).collect();
        let own_track = own_vessel.map(|v| v.track).filter(|t| !t.is_none());

        // Tracks.
        let designated = ship.map_or(TrackId::NONE, |s| s.designated);
        let margin = 48.0;
        let mut drew_shapes = [false; 3];
        for tp in &view.tracks {
            let Ok(i) = snap.tracks.binary_search_by_key(&tp.id, |t| t.id) else { continue };
            let t = &snap.tracks[i];
            if Some(t.id) == own_track { continue; }
            if ally_tracks.contains(&t.id) && t.stance == Stance::Friend { continue; }
            let pos = track_map_pos(view, t.id, tp.world_xz);
            if !pos.is_finite() { continue; }
            let p = f.to_local(pos);
            let on_screen = f.contains(p, margin);
            // Envelopes and shapes reach past the edge even when the symbol is off it.
            let live = !t.last_known;
            let geo_t = TrackView { pos: DVec3::new(pos.x, t.pos.y, pos.y), ..t.clone() };
            let g = geometry(&geo_t, own_geo.as_ref());
            if live && has(layers, layer::ENVELOPES) && matches!(t.stance, Stance::Hostile | Stance::Suspect) && t.envelope_m.is_finite() && t.envelope_m > 0.0 {
                let r = f.px(t.envelope_m as f64);
                if r > 4.0 && r < 100_000.0 && (p - f.half()).length() < r + f.size.length() {
                    self.pic.rings.push(Ring { centre: p, radius: r, kind: RingKind::Envelope(t.stance) });
                    if has(layers, layer::LABELS) {
                        let text = fill(&lk.tr("plot.ring.envelope"), &[lk.digits(&crate::text::track_label_text(t.id.0 as i64)), range_text(lk, t.envelope_m as f64)]);
                        let mut q = Self::req(p + Vec2::new(0.0, r), 0.0, RANK_RING, 0.0, KEY_ENVELOPE | t.id.0 as u64);
                        q.fixed = true;
                        self.label(q, vec![text], LabelRole::Reference, TrackId::NONE, false);
                    }
                    self.legend(("shape_circle", "plot.legend.envelope"));
                }
            }
            if live && has(layers, layer::UNCERTAINTY) {
                if let Some(geom) = core::shape_geom(&f, pos, &t.shape) {
                    if (geom.centre - f.half()).length() < geom.extent_px + f.size.length() {
                        let n = core::sample_count(geom.extent_px);
                        let seed = t.id.0 as u64;
                        // Wedge samples come from the shape itself; circles and ellipses centre on the symbol.
                        self.pic.samples.extend(core::shape_samples(&f, pos, &t.shape, seed, n));
                        let k = match t.shape { sim_systems::protocol::UncertaintyShape::Wedge { .. } => 0, sim_systems::protocol::UncertaintyShape::Ellipse { .. } => 1, _ => 2 };
                        drew_shapes[k] = true;
                        self.pic.shapes.push(geom);
                    }
                }
            }
            if has(layers, layer::HISTORY) {
                if let Some(h) = self.inp.history.and_then(|h| h.get(&t.id)).filter(|h| h.len() >= 2) {
                    let mut line: Vec<Vec2> = Vec::with_capacity(h.len() + 1);
                    for q in h.iter().filter(|q| q[0].is_finite() && q[1].is_finite()) {
                        let lp = f.to_local(DVec2::new(q[0] as f64, q[1] as f64));
                        if line.last().is_none_or(|l: &Vec2| l.distance(lp) >= HISTORY_MIN_STEP_PX) { line.push(lp); }
                    }
                    if live { line.push(p); }
                    if line.len() >= 2 { self.pic.history.push(line); self.legend(("track_history", "plot.legend.history")); }
                }
            }
            if !on_screen || !has(layers, layer::TRACKS) { continue; }
            let speed = flat(t.vel);
            if live && speed.is_finite() && speed.length() > LEADER_MIN_MPS {
                let secs = if matches!(t.domain, Domain::Air | Domain::Weapon) { LEADER_FAST_S } else { LEADER_S };
                let end = f.to_local(pos + speed * secs);
                // From the frame's edge, so the line never crosses the symbol.
                let half = core::symbol_half(self.s) * 0.75;
                let len = end.distance(p);
                if end.is_finite() && len > half + 3.0 {
                    let start = p + (end - p) / len * half;
                    self.pic.leaders.push((start, end, t.stance));
                    self.legend(("rule", "plot.legend.leader"));
                }
            }
            if live && t.emitting && t.stance != Stance::Friend && has(layers, layer::EMITTERS) {
                self.pic.emitters.push((p, t.stance));
                self.legend(("part_sensor", "plot.legend.emitter"));
            }
            let selected = self.inp.selected == t.id;
            let b = band(t, &g, own_geo.as_ref());
            let rank = band_rank(b, selected).clamp(0, 7) as u8;
            let weight = threat_value(b, if b == Band::HostileInRange || b == Band::Inbound { g.cpa_m } else { g.range_m });
            let number = lk.digits(&crate::text::track_label_text(t.id.0 as i64));
            let stance_word = crate::text::word_of(lk, "stance", t.stance);
            let line1 = if t.last_known { fill(&lk.tr("plot.label.last_known"), std::slice::from_ref(&number)) } else { fill(&lk.tr("plot.label.track"), &[number.clone(), stance_word.clone()]) };
            let mut lines = vec![line1];
            if selected || designated == t.id {
                let detail = if selected {
                    t.class.as_ref().filter(|_| matches!(t.rung, IdRung::Class | IdRung::Exact)).and_then(|c| self.env.class_name(c)).or_else(|| Some(fill(&lk.tr("plot.label.rung"), &[crate::text::word_of(lk, "rung", t.rung)]))).filter(|s| !s.is_empty())
                } else { None };
                let line2 = match (designated == t.id, detail) {
                    (true, Some(d)) => Some(fill(&lk.tr("plot.label.designated_with"), &[d])),
                    (true, None) => Some(lk.tr("plot.label.designated")),
                    (false, d) => d,
                };
                lines.extend(line2);
            }
            let name = if t.last_known {
                fill(&lk.tr("hud.track.name_last_known"), &[number.clone(), stance_word.clone()])
            } else if g.bearing_deg >= 0.0 {
                fill(&lk.tr("hud.track.name"), &[number.clone(), stance_word.clone(), lk.digits(&crate::text::bearing_text(g.bearing_deg))])
            } else {
                fill(&lk.tr("hud.track.name_no_bearing"), &[number.clone(), stance_word.clone()])
            };
            let sym = Symbol { kind: SymKind::Track, pos: p, stance: t.stance, domain: t.domain, trend: t.trend, last_known: t.last_known, emitting: t.emitting, heading: None, track: t.id, bearing_deg: g.bearing_deg, name, rank };
            self.stance_legend(&sym);
            if selected { self.pic.bracket = Some(p); self.legend(("select_bracket", "hud.legend.selected")); }
            if self.inp.boxed.binary_search(&t.id).is_ok() { self.pic.boxes.push(p); self.legend(("shape_rect", "plot.legend.changed")); }
            self.pic.symbols.push(sym);
            let hovered = self.inp.hovered == t.id;
            if has(layers, layer::LABELS) || hovered {
                let mut q = Self::req(p, core::symbol_half(self.s), rank, weight, t.id.0 as u64);
                q.force = hovered;
                self.label(q, lines, LabelRole::Track, t.id, selected);
            }
        }
        let shape_rows = [("track_bearing_wedge", "hud.legend.wedge"), ("track_samples", "hud.legend.ellipse"), ("track_fire_control", "hud.legend.circle")];
        for (k, row) in shape_rows.iter().enumerate() { if drew_shapes[k] { self.legend(*row); } }

        // Allied hulls.
        if has(layers, layer::ALLIES) {
            for v in &allies {
                let w = view.origin.to_world(v.pos);
                let p = f.to_local(DVec2::new(w.x, w.z));
                if !f.contains(p, margin) { continue; }
                self.pic.symbols.push(Symbol { kind: SymKind::Ally, pos: p, stance: Stance::Friend, domain: Domain::Surface, trend: VerticalTrend::Steady, last_known: false, emitting: false, heading: heading_of(v.rot), track: v.track, bearing_deg: -1.0, name: String::new(), rank: RANK_ALLY });
                self.legend(("stance_friend_surface", "plot.legend.ally"));
                if has(layers, layer::LABELS) {
                    let text = self.env.vessel_name(v.entity).filter(|n| !n.trim().is_empty()).unwrap_or_else(|| lk.tr("plot.label.ally"));
                    self.label(Self::req(p, core::symbol_half(self.s), RANK_ALLY, 0.0, KEY_ALLY | (v.entity.to_u64() & 0xFF_FFFF_FFFF)), vec![text], LabelRole::Track, TrackId::NONE, false);
                }
            }
        }

        // Own mines.
        if has(layers, layer::MINES) {
            for m in view.bodies.iter().filter(|b| b.own_side && self.env.is_mine(b.kind)) {
                let w = view.origin.to_world(m.pos);
                let p = f.to_local(DVec2::new(w.x, w.z));
                if f.contains(p, 8.0) { self.pic.mines.push(p); self.legend(("dot", "plot.legend.mine")); }
            }
        }

        // The own hull, on top of everything it carries.
        if let (Some(c), Some(v)) = (own_local, own_vessel) {
            let heading = ship.map(|s| s.heading_rad).filter(|h| h.is_finite()).or_else(|| heading_of(v.rot));
            self.pic.symbols.push(Symbol { kind: SymKind::Own, pos: c, stance: Stance::Friend, domain: Domain::Surface, trend: VerticalTrend::Steady, last_known: false, emitting: false, heading, track: TrackId::NONE, bearing_deg: -1.0, name: String::new(), rank: 0 });
            self.pic.legend.insert(0, ("track_own", "plot.legend.own"));
            if has(layers, layer::LABELS) {
                self.label(Self::req(c, core::symbol_half(self.s), 0, f64::MAX, KEY_OWN), vec![lk.tr("plot.label.own")], LabelRole::Track, TrackId::NONE, true);
            }
        }
    }

    fn stance_legend(&mut self, s: &Symbol) {
        if s.last_known { self.legend(("track_history", "hud.legend.last_known")); return; }
        let row = match s.stance {
            Stance::Hostile => ("stance_hostile_surface", "hud.legend.hostile"),
            Stance::Suspect => ("stance_suspect_surface", "hud.legend.suspect"),
            Stance::Neutral => ("stance_neutral_surface", "hud.legend.neutral"),
            Stance::Friend => ("stance_friend_surface", "hud.legend.friend"),
        };
        self.legend(row);
        if s.domain != Domain::Surface {
            match s.trend {
                VerticalTrend::Climbing => self.legend(("trend_climbing", "hud.legend.climbing")),
                VerticalTrend::Diving => self.legend(("trend_diving", "hud.legend.diving")),
                VerticalTrend::Steady => self.legend(("trend_steady", "plot.legend.steady")),
            }
        }
    }

    fn static_route(&mut self) {
        if self.inp.route.is_empty() || !has(self.inp.layers, layer::ROUTE) { return; }
        let f = *self.frame();
        let lk = self.env.lk();
        for (i, w) in self.inp.route.iter().enumerate().filter(|(_, w)| w.is_finite()) {
            let p = f.to_local(*w);
            self.pic.route.push(p);
            self.pic.waypoints.push(p);
            if has(self.inp.layers, layer::LABELS) {
                let text = fill(&lk.tr("plot.label.waypoint"), &[number_text(lk, i as f64 + 1.0, 0)]);
                self.label(Self::req(p, 6.0, RANK_WAYPOINT, 0.0, KEY_WAYPOINT | i as u64), vec![text], LabelRole::Overlay, TrackId::NONE, false);
            }
        }
        if self.pic.route.len() >= 2 { self.legend(("line_solid", "plot.legend.route")); }
    }

    fn belief(&mut self) {
        if self.inp.belief.is_empty() { return; }
        let f = *self.frame();
        let max = self.inp.belief.iter().map(|b| b[2]).filter(|w| w.is_finite()).fold(0.0f64, f64::max);
        if max <= 0.0 { return; }
        for b in self.inp.belief {
            if !(b[0].is_finite() && b[1].is_finite() && b[2].is_finite()) || b[2] <= 0.0 { continue; }
            let p = f.to_local(DVec2::new(b[0], b[1]));
            if f.contains(p, 16.0) { self.pic.belief.push((p, (b[2] / max) as f32)); }
        }
        if !self.pic.belief.is_empty() { self.legend(("fill_dots", "plot.legend.belief")); }
    }

    fn overlay(&mut self) {
        let f = *self.frame();
        let lk = self.env.lk();
        if let Some((a, b, _measure)) = self.inp.overlay.line {
            let (pa, pb) = (f.to_local(a), f.to_local(b));
            if pa.is_finite() && pb.is_finite() {
                self.pic.tool_line = Some((pa, pb));
                if let Some((brg, range)) = core::bearing_range(a, b) {
                    let text = fill(&lk.tr("plot.bearing_range"), &[lk.digits(&crate::text::bearing_text(brg)), range_text(lk, range)]);
                    let mut q = Self::req(pb, 10.0, 0, f64::MAX, KEY_TOOL);
                    q.force = true;
                    self.label(q, vec![text], LabelRole::Reference, TrackId::NONE, false);
                }
            }
        }
        if let Some(c) = self.inp.overlay.cursor.filter(|c| c.is_finite()) {
            let pts: Vec<Vec2> = self.pic.symbols.iter().filter(|s| s.kind == SymKind::Track).map(|s| s.pos).collect();
            let snapped = core::nearest_within(&pts, c, core::SNAP_RADIUS_PX).map(|i| pts[i]);
            let at = snapped.unwrap_or(c);
            self.pic.cursor = Some((at, snapped.is_some()));
            let own = self.pic.symbols.iter().find(|s| s.kind == SymKind::Own).map(|s| f.to_map(s.pos));
            if let Some((brg, range)) = own.and_then(|o| core::bearing_range(o, f.to_map(at))) {
                let text = fill(&lk.tr("plot.bearing_range"), &[lk.digits(&crate::text::bearing_text(brg)), range_text(lk, range)]);
                let mut q = Self::req(at, core::symbol_half(self.s) + 4.0, 0, f64::MAX - 1.0, KEY_TOOL | 1);
                q.force = true;
                q.prev_side = Some(3);
                self.label(q, vec![text], LabelRole::Reference, TrackId::NONE, false);
            }
        }
    }

    fn scale_bar(&mut self) {
        let f = *self.frame();
        if f.size.x < 200.0 || f.size.y < 80.0 { return; }
        // About a sixth of the width, rounded to 1, 2 or 5.
        let target = f.mpp * (f.size.x as f64 / 6.0).clamp(80.0, 320.0);
        let mut len_m = core::nice_step(target);
        if f.px(len_m) > f.size.x * 0.4 { len_m /= 2.0; }
        let len = f.px(len_m);
        let start = Vec2::new(24.0, f.size.y - 24.0);
        self.pic.scale_bar = Some((start, len));
        let text = round_range_text(self.env.lk(), len_m);
        let size = self.env.measure(&text);
        self.pic.scale_label = Some(Label { rect: Rect::at(Vec2::new(start.x, start.y - 6.0 - size.y), size), lines: vec![text], role: LabelRole::Reference, track: TrackId::NONE, bold: false, plate: false, forced: false });
    }

    fn place_labels(&mut self) {
        let f = *self.frame();
        let bounds = Rect::at(Vec2::ZERO, f.size);
        // A track label's `clear` is the symbol half, so its own symbol's rectangle is skipped by equality.
        let mut symbols: Vec<(Rect, u8)> = self.pic.symbols.iter().map(|s| (Rect::around(s.pos, core::symbol_half(self.s)), s.rank)).collect();
        // The scale bar and its label stand in the corner like a symbol: lower labels give way to them.
        if let (Some((start, len)), Some(l)) = (self.pic.scale_bar, self.pic.scale_label.as_ref()) {
            symbols.push((Rect { min: l.rect.min.min(start - Vec2::new(4.0, 4.0)), max: l.rect.max.max(start + Vec2::new(len + 4.0, 4.0)) }, 0));
        }
        let reqs: Vec<LabelReq> = self.reqs.iter().map(|(r, _)| r.clone()).collect();
        let placed = core::declutter_around(&reqs, &symbols, bounds, self.inp.blocked);
        for ((req, mut label), p) in std::mem::take(&mut self.reqs).into_iter().zip(placed) {
            match p {
                Some(p) => {
                    label.rect = p.rect;
                    if !req.fixed { self.pic.sides.insert(req.key, p.side); }
                    self.pic.labels.push(label);
                }
                None => if label.role == LabelRole::Track && !label.track.is_none() { self.pic.hidden.push((label.track, req.anchor)); },
            }
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::interp::{TrackPose, VesselPose};
    use sim_systems::protocol::{RungReason, ShipStatus, SourceMask, StanceReason, UncertaintyShape};

    pub(crate) struct TestEnv(pub crate::text::tests::CsvLookup);
    impl Env for TestEnv {
        fn lk(&self) -> &dyn Lookup { &self.0 }
        fn measure(&self, text: &str) -> Vec2 { Vec2::new(10.0 * text.chars().count() as f32, 22.0) }
        fn class_name(&self, _: &ClassId) -> Option<String> { Some("Kestrel class".into()) }
        fn vessel_name(&self, _: EntityId) -> Option<String> { Some("Brisk".into()) }
        fn is_mine(&self, _: MunitionIdx) -> bool { true }
    }
    pub(crate) fn env() -> TestEnv { TestEnv(crate::text::tests::CsvLookup::load()) }

    pub(crate) fn track(id: u32, stance: Stance, x: f64, z: f64) -> TrackView {
        TrackView {
            id: TrackId(id), domain: Domain::Surface, stance, stance_reason: StanceReason::Default, rung: IdRung::Type, rung_reason: RungReason::None,
            grade: Default::default(), pos: DVec3::new(x, 0.0, z), vel: DVec3::ZERO, shape: UncertaintyShape::Circle { radius_m: 30.0 },
            last_update_tick: 0, last_known: false, class: None, candidates: 1, sources: SourceMask::SEARCH_RADAR, trend: VerticalTrend::Steady,
            inbound: false, engaged_by: 0, envelope_m: 0.0, emitting: false, relevance: Relevance::Near, struck: false,
        }
    }

    /// A view with the own hull at the map origin heading east and `tracks` in the picture.
    pub(crate) fn view_with(mut tracks: Vec<TrackView>) -> ViewFrame {
        tracks.sort_by_key(|t| t.id);
        let own = sim_systems::protocol::VesselView {
            entity: EntityId::from_u64(1), kin: sim_core::math::Kinematics { pose: sim_core::math::Pose::from_heading(DVec3::ZERO, std::f64::consts::FRAC_PI_2), lin_vel: DVec3::new(6.0, 0.0, 0.0), ang_vel: DVec3::ZERO },
            status: Default::default(), flags: Default::default(), turrets: Vec::new(), rudder: 0.0, power: 0.5, track: TrackId::NONE, relevance: Relevance::Own,
        };
        let snap = Snapshot {
            tick: 600, vessels: vec![own], tracks: tracks.clone(),
            ship: Some(ShipStatus { vessel: EntityId::from_u64(1), heading_rad: std::f32::consts::FRAC_PI_2, longest_envelope_m: 4_000.0, route: vec![DVec2::new(3_000.0, -1_000.0)], ..Default::default() }),
            ..Default::default()
        };
        let q = glam::DQuat::from_rotation_y(0.0);
        ViewFrame {
            rev: 1, render_tick: 600.0, sim_time_s: 20.0, origin: Default::default(), origin_shift: None,
            vessels: vec![VesselPose { entity: EntityId::from_u64(1), pos: [0.0, 0.0, 0.0], rot: [q.x as f32, q.y as f32, q.z as f32, q.w as f32], vel: [6.0, 0.0, 0.0], status: Default::default(), flags: Default::default(), track: TrackId::NONE, rudder: 0.0, power: 0.5, turrets: 0..0, extrapolated: false, own: true }],
            turrets: Vec::new(), bodies: Vec::new(), decoys: Vec::new(), shells: Vec::new(),
            tracks: tracks.iter().map(|t| TrackPose { id: t.id, pos: [t.pos.x as f32, 0.0, t.pos.z as f32], world_xz: [t.pos.x, t.pos.z] }).collect(),
            own: Some(0), snapshot: Some(std::sync::Arc::new(snap)),
        }
    }

    pub(crate) fn inputs<'a>(view: &'a ViewFrame, prev: &'a BTreeMap<u64, u8>) -> Inputs<'a> {
        Inputs {
            mode: PlotMode::Sortie, layers: layer::DEFAULT, frame: Frame::new(DVec2::ZERO, 25.0, Vec2::new(1600.0, 900.0)), view: Some(view),
            selected: TrackId::NONE, hovered: TrackId::NONE, history: None, boxed: &[], chart: None, route: &[], belief: &[], prev_sides: prev,
            overlay: Overlay::default(), symbol_size: 24.0, clear: [0.0; 4], blocked: &[],
        }
    }

    #[test]
    fn a_live_picture_has_own_hull_tracks_rings_and_labels() {
        let mut far = track(12, Stance::Hostile, 3_000.0, -2_000.0);
        far.envelope_m = 6_000.0;
        far.shape = UncertaintyShape::Ellipse { semi_major_m: 900.0, semi_minor_m: 300.0, bearing_rad: 0.3 };
        far.vel = DVec3::new(-8.0, 0.0, 0.0);
        far.emitting = true;
        let mut lk = track(7, Stance::Suspect, -2_000.0, 1_000.0);
        lk.last_known = true;
        let view = view_with(vec![far, lk, track(3, Stance::Neutral, 1_000.0, 1_000.0)]);
        let prev = BTreeMap::new();
        let env = env();
        let pic = build(&inputs(&view, &prev), &env);
        assert!(pic.message.is_none());
        let own = pic.symbols.iter().find(|s| s.kind == SymKind::Own).expect("own hull");
        assert_eq!(own.pos, Vec2::new(800.0, 450.0));
        assert!((own.heading.unwrap() - std::f32::consts::FRAC_PI_2).abs() < 1e-5);
        // 3 km east, 2 km north at 25 m/px.
        let h = pic.symbols.iter().find(|s| s.track == TrackId(12)).unwrap();
        assert_eq!(h.pos, Vec2::new(920.0, 370.0));
        assert_eq!(h.name, "Track 0012, Hostile, bearing 056");
        assert!(pic.labels.iter().any(|l| l.lines[0] == "0012 Hostile"));
        assert!(pic.labels.iter().any(|l| l.lines[0] == "0007 last known"));
        assert!(pic.labels.iter().any(|l| l.lines[0] == "Own ship"));
        // The hostile's envelope, the own envelope and range rings; one emitter; one leader; one shape.
        assert!(pic.rings.iter().any(|r| r.kind == RingKind::Envelope(Stance::Hostile) && (r.radius - 240.0).abs() < 1e-3));
        assert!(pic.rings.iter().any(|r| r.kind == RingKind::OwnEnvelope && (r.radius - 160.0).abs() < 1e-3));
        assert!(pic.rings.iter().filter(|r| r.kind == RingKind::Range).count() >= 3);
        assert_eq!(pic.emitters.len(), 1);
        assert_eq!(pic.leaders.len(), 1);
        assert_eq!(pic.shapes.len(), 1);
        assert!(!pic.samples.is_empty());
        // The route starts at the own hull's symbol edge.
        assert!(pic.route.first().is_some_and(|p| (p.distance(Vec2::new(800.0, 450.0)) - 9.0).abs() < 1e-3));
        // Every legend icon exists.
        let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../../game/ui/icons");
        for (icon, key) in &pic.legend {
            assert!(std::path::Path::new(&format!("{dir}/{icon}.svg")).exists(), "{icon}");
            assert_ne!(env.0.tr(key), *key, "{key} has no English");
        }
        assert_eq!(pic.legend[0], ("track_own", "plot.legend.own"));
        // Labels never overlap.
        for (i, a) in pic.labels.iter().enumerate() { for b in &pic.labels[i + 1..] { assert!(!a.rect.overlaps(&b.rect), "{:?} {:?}", a.lines, b.lines); } }
        assert_eq!(round_range_text(&env.0, 5_000.0), "5 km");
        assert_eq!(round_range_text(&env.0, 2_500.0), "2.5 km");
        assert_eq!(round_range_text(&env.0, 500.0), "500 m");
    }

    #[test]
    fn layers_and_selection_change_what_is_drawn() {
        let view = view_with(vec![track(12, Stance::Hostile, 3_000.0, -2_000.0), track(13, Stance::Friend, -500.0, 0.0)]);
        let prev = BTreeMap::new();
        let env = env();
        let mut inp = inputs(&view, &prev);
        inp.layers = layer::TRACKS;
        inp.selected = TrackId(12);
        inp.boxed = &[TrackId(12)];
        let pic = build(&inp, &env);
        assert!(pic.rings.is_empty() && pic.route.is_empty() && pic.grid.is_empty());
        assert!(pic.labels.is_empty(), "labels layer off");
        assert_eq!(pic.bracket, Some(Vec2::new(920.0, 370.0)));
        assert_eq!(pic.boxes.len(), 1);
        // Hover shows a hidden label even with the labels layer off.
        inp.hovered = TrackId(13);
        let pic = build(&inp, &env);
        assert_eq!(pic.labels.len(), 1);
        assert_eq!(pic.labels[0].track, TrackId(13));
        // Selected gets a second line with its rung, designation shows.
        inp.layers = layer::DEFAULT;
        inp.hovered = TrackId::NONE;
        let pic = build(&inp, &env);
        let l = pic.labels.iter().find(|l| l.track == TrackId(12)).unwrap();
        assert_eq!(l.lines, vec!["0012 Hostile".to_string(), "ID Type".to_string()]);
        assert!(l.bold);
        // Hit testing finds the symbol and its label.
        let i = pic.hit(Vec2::new(925.0, 372.0)).unwrap();
        assert_eq!(pic.symbols[i].track, TrackId(12));
        let c = l.rect.min + l.rect.size() * 0.5;
        assert_eq!(pic.hit(c).map(|i| pic.symbols[i].track), Some(TrackId(12)));
        assert_eq!(pic.hit(Vec2::new(5.0, 5.0)), None);
    }

    #[test]
    fn crowded_pictures_hide_lowest_first() {
        // Forty tracks on top of each other, a hostile inbound weapon among them.
        let mut tracks: Vec<TrackView> = (1..=40).map(|i| track(i, if i % 2 == 0 { Stance::Neutral } else { Stance::Suspect }, 2_000.0 + i as f64, 0.0)).collect();
        tracks[19].stance = Stance::Hostile;
        tracks[19].inbound = true;
        tracks[19].domain = Domain::Weapon;
        let view = view_with(tracks);
        let prev = BTreeMap::new();
        let env = env();
        let pic = build(&inputs(&view, &prev), &env);
        assert!(pic.labels.iter().any(|l| l.track == TrackId(20)), "the inbound weapon keeps its label");
        assert!(!pic.hidden.is_empty());
        assert!(pic.hidden.iter().all(|(t, _)| *t != TrackId(20)));
        assert_eq!(pic.top_covers(12.0), 0, "the top priorities stay uncovered");
        for (i, a) in pic.labels.iter().enumerate() { for b in &pic.labels[i + 1..] { assert!(!a.rect.overlaps(&b.rect)); } }
        // The same input places every label on the same side again.
        let again = build(&Inputs { prev_sides: &pic.sides, ..inputs(&view, &prev) }, &env);
        assert_eq!(again.labels, pic.labels);
    }

    #[test]
    fn static_charts_and_empty_states() {
        let env = env();
        let prev = BTreeMap::new();
        let chart = Chart::from_def(&content::campaign::MapDef {
            size_m: [20_000.0, 20_000.0], depth_m: 200.0,
            islands: vec![content::campaign::Island { centre_m: [1_000.0, -500.0], radius_m: 600.0, height_m: 80.0, seed: 3 }],
            markers: vec![content::campaign::Marker { kind: content::campaign::MarkerKind::RadarSite, name: String::new(), pos_m: [1_000.0, -500.0], heading_deg: 0.0 }], seed: 1,
        });
        let route = [DVec2::new(-5_000.0, 5_000.0), DVec2::new(0.0, 0.0), DVec2::new(f64::NAN, 0.0)];
        let belief = [[100.0, 100.0, 2.0], [200.0, 100.0, 1.0], [f64::NAN, 0.0, 1.0], [0.0, 0.0, -1.0]];
        let blank = ViewFrame::default();
        let inp = Inputs { mode: PlotMode::Briefing, view: None, chart: Some(&chart), route: &route, belief: &belief, ..inputs(&blank, &prev) };
        let pic = build(&inp, &env);
        assert!(pic.chart_palette && pic.land && pic.message.is_none());
        assert_eq!(pic.route.len(), 2);
        assert_eq!(pic.belief, vec![(inp.frame.to_local(DVec2::new(100.0, 100.0)), 1.0), (inp.frame.to_local(DVec2::new(200.0, 100.0)), 0.5)]);
        assert!(pic.labels.iter().any(|l| l.lines[0] == "Radar site"), "{:?}", pic.labels);
        // Nothing to show: a message, never a blank plot.
        let empty = build(&Inputs { mode: PlotMode::Briefing, view: None, ..inputs(&blank, &prev) }, &env);
        assert_eq!(empty.message.as_deref(), Some(env.0.tr("plot.empty.chart").as_str()));
        let idle = ViewFrame::default();
        let empty = build(&inputs(&idle, &prev), &env);
        assert_eq!(empty.message.as_deref(), Some(env.0.tr("plot.empty.sortie").as_str()));
        // A zero-size control builds without panicking.
        let one = view_with(vec![track(1, Stance::Hostile, 10.0, 10.0)]);
        let _ = build(&Inputs { frame: Frame::new(DVec2::ZERO, 25.0, Vec2::ZERO), ..inputs(&one, &prev) }, &env);
    }

    #[test]
    fn graticule_edge_labels_keep_inside_the_clear_margins() {
        let prev = BTreeMap::new();
        let env = env();
        let one = view_with(vec![track(1, Stance::Hostile, 10.0, 10.0)]);
        let grid = |p: &Picture| {
            let edge = |t: &str| ["east", "west", "north", "south"].iter().any(|w| t.ends_with(w)) || t == "0 m";
            p.labels.iter().filter(|l| l.plate && edge(&l.lines[0])).map(|l| l.rect).collect::<Vec<_>>()
        };
        let open = build(&Inputs { layers: layer::DEFAULT | layer::LABELS, ..inputs(&one, &prev) }, &env);
        assert!(grid(&open).iter().any(|r| r.min.y < 40.0), "edge labels at the top by default");
        let clear = [300.0, 200.0, 0.0, 0.0];
        let kept = build(&Inputs { layers: layer::DEFAULT | layer::LABELS, clear, ..inputs(&one, &prev) }, &env);
        let rects = grid(&kept);
        assert!(!rects.is_empty());
        for r in rects {
            assert!(r.min.x >= clear[0] - 1.0 && r.min.y >= clear[1] - 1.0, "{r:?} under a cluster");
        }
    }

    #[test]
    fn cursor_reads_bearing_and_range_from_the_own_hull() {
        let view = view_with(vec![track(12, Stance::Hostile, 3_000.0, 0.0)]);
        let prev = BTreeMap::new();
        let env = env();
        let mut inp = inputs(&view, &prev);
        inp.overlay.cursor = Some(Vec2::new(915.0, 452.0));
        let pic = build(&inp, &env);
        assert_eq!(pic.cursor, Some((Vec2::new(920.0, 450.0), true)));
        assert!(pic.labels.iter().any(|l| l.lines[0] == "090, 3.0 km"), "{:?}", pic.labels.iter().map(|l| &l.lines).collect::<Vec<_>>());
        inp.overlay.cursor = None;
        inp.overlay.line = Some((DVec2::ZERO, DVec2::new(0.0, -1_500.0), true));
        let pic = build(&inp, &env);
        assert!(pic.tool_line.is_some());
        assert!(pic.labels.iter().any(|l| l.lines[0] == "000, 1,500 m"));
    }

    #[test]
    fn every_plot_text_key_has_english() {
        let env = env();
        for key in TEXT_KEYS.iter().chain(super::super::access::TEXT_KEYS) { assert_ne!(env.0.tr(key), *key, "{key}"); }
    }
}
