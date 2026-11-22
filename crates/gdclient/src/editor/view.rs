//! Unit `gd-editor-view`. `BpEditorView` (Node3D, in `res://ui/editor/dry_dock.tscn`): the dry dock's 3D
//! side. DESIGN.md L168 (overlays: waterline and heel, centre of gravity and buoyancy, weapon arcs on the
//! water with blocked sectors hatched, sensor blind sectors, conduit routes, vulnerability heatmap),
//! L285 (X-ray: each network with its own colour, line pattern and label, a legend, a badge on doubled
//! routes), L188 (Fix previews as a ghost), L235-250 (trackpad mapping: two-finger drag orbits around the
//! point under the cursor, Shift pans, pinch zooms toward the cursor; no inertia), L1689-1691 (overlay
//! colours are categorical and need legends), L1940 (weathering level 0 in the editor), L1824 (depth of field
//! allowed here).
//! Draws the design with the same hull and part renderers as a sortie (`crate::render`), so what the dock
//! shows is what the sea shows. Reads `BpEditor`'s model through `Services`; changes nothing but the camera.
//!
//! # How the unit is built
//! - [`cam`] is the camera with no Godot in it: an orbit about a look-at point, the trackpad mapping table,
//!   gesture starts (Godot sends no gesture begin, so a gap of [`cam::GESTURE_GAP_S`] or a jump of the
//!   cursor starts one) and the trackpad gate counters. [`pick`] is the ray against the design's cells, part
//!   boxes and planes. [`geom`] turns overlay data into line, fill and box lists. All three test headless,
//!   and the mapping tests replay recorded pan and magnify gestures (L250).
//! - The node finds the `Editor` autoload, binds it for a moment each frame and copies out what changed (the
//!   worker's newest design and its own blueprint, which are always drawn as a pair, the ghost, the overlay
//!   inputs). It builds its render units on the first frame the dock is open and frees them when the dock
//!   closes or a session starts loading (the sortie world view owns the 3D then); `Services::camera` is
//!   written only while the dock draws.
//! - Overlays whose inputs take time (arcs and blind sectors of every mount and sensor, the vulnerability
//!   heatmap, the signature, the equilibrium for the centres and the waterline) are computed on a thread of
//!   their own per design and drawn when they arrive. Lines are drawn in screen space at a fixed pixel width
//!   by a shader made here, so they read the same at every zoom; dashes and dots are the line patterns.
//! - The camera moves only on input: `process` never animates it, so there is no inertia (L246). A newly
//!   loaded design is framed once.
//!
//! # Choices the spec leaves open
//! - Camera: yaw is the bearing of the camera from its look-at point measured from the bow toward starboard
//!   (0 looks aft from ahead, 90 looks at the starboard side), pitch is its height angle and may go to -80
//!   degrees, under the waterline (the trackpad gate's "gun arc from below the waterline" task). Mouse
//!   drags are mapped one to one (0.3 degrees per pixel); the trackpad's invert toggles and speed sliders
//!   apply to the gesture methods, the trackpad's own scheme.
//! - A camera reset in `camera_log` is any `frame()` call. A reversal is an orbit gesture whose first 12 px
//!   move the opposite way, on the same axis, to an orbit gesture that ended less than 1 s before.
//! - Arcs and blind sectors are drawn on the water to 1.25 and 1 hull lengths (at least 20 m): the sector
//!   shapes are what an editor view can show; ranges are in the gauge strip. Arcs are filled in the arc
//!   colour, blocked sectors hatched in alert.warning (text.primary in high contrast, per the token rules).
//! - The heatmap shows outer cells with a vulnerability of at least 1 in 16, on a ramp from heat_low to
//!   heat_high. The signature view draws radar cross-section (solid) and visual area (dashed) per 10 degree
//!   aspect as polar curves on the water. Compartments draws the decks and bulkheads as sheets.
//! - The ghost is drawn whenever the model has one; `OVERLAY_GHOST` adds its legend rows. A Fix or Suggest
//!   preview shows the parts it adds as ghosts in ghost_preview, the parts it removes and the cells it
//!   changes as boxes (removed in alert.warning).
//! - A deck slice at deck `i` shows the ship up to the row under the next deck up (2.5 m of headroom over
//!   the top deck), so the level standing on deck `i` is open from above; parts that start above the cut
//!   are hidden and cannot be picked.
//! - `pick` order: a handle within 12 px, then the nearest part box or hull cell; with Compartments or X-ray
//!   on, a deck or bulkhead inside the hull comes before hull cells.
//! - `thumbnail` renders the dock's own world through a temporary camera in an off-screen viewport with the
//!   overlay layer excluded, forcing one draw; headless it returns null.
// Client rendering, not simulation (clippy.toml): std floats are allowed here.
#![allow(clippy::disallowed_methods)]
use super::BpEditor;
use crate::convert::d;
use crate::enums::{GdEnum, SessionState};
use godot::classes::{Camera3D, INode3D, Image, Node3D};
use godot::prelude::*;
use std::sync::Arc;

/// Text keys this unit emits (`game/i18n/dock.csv`).
pub(crate) const VIEW_KEYS: &[&str] = &[
    "dock.overlay.waterline", "dock.overlay.design_waterline", "dock.overlay.cg", "dock.overlay.cb", "dock.overlay.arc",
    "dock.overlay.arc_blocked", "dock.overlay.blind", "dock.overlay.power", "dock.overlay.fuel", "dock.overlay.data",
    "dock.overlay.doubled", "dock.overlay.heat_low", "dock.overlay.heat_high", "dock.overlay.planes", "dock.overlay.rcs",
    "dock.overlay.visual", "dock.overlay.ghost", "dock.overlay.ghost_invalid", "dock.overlay.ghost_preview", "dock.overlay.ghost_removed",
];

/// `Bp.OVERLAY_*` bits.
pub(crate) mod bits {
    pub const WATERLINE: i64 = 1;
    pub const CG_CB: i64 = 2;
    pub const ARCS: i64 = 4;
    pub const BLIND: i64 = 8;
    pub const CONDUITS: i64 = 16;
    pub const HEATMAP: i64 = 32;
    pub const XRAY: i64 = 64;
    pub const COMPARTMENTS: i64 = 128;
    pub const SIGNATURE: i64 = 256;
    pub const GHOST: i64 = 512;
    pub const ALL: i64 = 1023;
}

// =========================================================================================================
// Camera (no Godot)
// =========================================================================================================

pub(crate) mod cam {
    use crate::world::camera::Lens;
    use glam::{Mat3, Vec3};

    /// Vertical field of view of the dock camera: longer than the sortie's 60 degrees, so a design reads with
    /// little perspective distortion.
    pub const FOV_Y_DEG: f32 = 40.0;
    pub const PITCH_MIN_DEG: f32 = -80.0;
    pub const PITCH_MAX_DEG: f32 = 89.0;
    pub const DIST_MIN_M: f32 = 1.5;
    pub const DIST_MAX_M: f32 = 800.0;
    /// Mouse orbit, degrees per pixel.
    pub const ORBIT_DEG_PER_PX: f32 = 0.3;
    /// Godot's macOS view (`godot_content_view.mm`, `processPanEvent`) passes the precise scroll deltas
    /// through unscaled and negated: one unit of `InputEventPanGesture.delta` is one point of scrolling, and
    /// with macOS's default natural scrolling the negated delta runs against the fingers. A two-finger drag
    /// is mapped as a mouse drag the way the fingers move: `-delta` points.
    pub const PAN_GESTURE_PX: f32 = 1.0;
    /// Cmd + two-finger vertical: zoom factor exp(points x this) (about 0.88 per 25 points, one wheel step).
    pub const ZOOM_PER_UNIT: f32 = 0.005;
    /// No event for this long ends a gesture (Godot sends no begin or end).
    pub const GESTURE_GAP_S: f64 = 0.25;
    /// A cursor that jumps this far between events starts a new gesture.
    pub const GESTURE_JUMP_PX: f32 = 48.0;
    /// An orbit gesture's direction is known after this much travel.
    pub const CLASSIFY_PX: f32 = 12.0;
    /// An orbit opposite to one that ended this recently is a reversal (L250).
    pub const REVERSAL_WINDOW_S: f64 = 1.0;
    /// Framing leaves this margin around the box's bounding sphere, and puts the box inside this share of the
    /// view's width and height (the dock's panels sit around the edges).
    const FRAME_MARGIN: f32 = 1.08;
    const FRAME_FILL: f32 = 0.72;

    #[derive(Clone, Copy, Debug, PartialEq)]
    pub struct DockCam { pub target: Vec3, pub yaw_deg: f32, pub pitch_deg: f32, pub distance_m: f32, pub canvas: [f32; 2] }
    impl Default for DockCam { fn default() -> Self { Self { target: Vec3::ZERO, yaw_deg: 30.0, pitch_deg: 20.0, distance_m: 60.0, canvas: [1920.0, 1080.0] } } }

    fn fin(v: f32, or: f32) -> f32 { if v.is_finite() { v } else { or } }

    impl DockCam {
        /// Keep every value usable: finite, pitch and distance in range, yaw in [0, 360).
        pub fn sanitize(&mut self) {
            if !self.target.is_finite() { self.target = Vec3::ZERO; }
            self.yaw_deg = fin(self.yaw_deg, 30.0).rem_euclid(360.0);
            self.pitch_deg = fin(self.pitch_deg, 20.0).clamp(PITCH_MIN_DEG, PITCH_MAX_DEG);
            self.distance_m = fin(self.distance_m, 60.0).clamp(DIST_MIN_M, DIST_MAX_M);
        }
        /// From the look-at point to the eye.
        pub fn offset(&self) -> Vec3 {
            let (y, p) = (self.yaw_deg.to_radians(), self.pitch_deg.clamp(PITCH_MIN_DEG, PITCH_MAX_DEG).to_radians());
            Vec3::new(p.cos() * y.cos(), p.sin(), p.cos() * y.sin()) * self.distance_m
        }
        pub fn eye(&self) -> Vec3 { self.target + self.offset() }
        pub fn lens(&self) -> Lens { Lens::look(self.eye(), self.target, 0.0, FOV_Y_DEG, self.canvas) }
        /// The camera's basis as columns right, up, back (back = from target to eye).
        fn basis(&self) -> Mat3 { let l = self.lens(); Mat3::from_cols(l.right, l.up, -l.forward) }
        /// Orbit about a world point: yaw and pitch change by these degrees and `p` keeps its place on screen.
        pub fn orbit_about(&mut self, p: Vec3, dyaw_deg: f32, dpitch_deg: f32) {
            if !(dyaw_deg.is_finite() && dpitch_deg.is_finite() && p.is_finite()) { return; }
            let before = self.basis();
            let pitch = (self.pitch_deg + dpitch_deg).clamp(PITCH_MIN_DEG, PITCH_MAX_DEG);
            self.yaw_deg = (self.yaw_deg + dyaw_deg).rem_euclid(360.0);
            self.pitch_deg = pitch;
            let r = self.basis() * before.transpose();
            let t = p + r * (self.target - p);
            if t.is_finite() { self.target = t; }
        }
        /// Metres one canvas unit covers at the look-at distance.
        pub fn m_per_px(&self) -> f32 { 2.0 * self.distance_m * (0.5 * FOV_Y_DEG.to_radians()).tan() / self.canvas[1].max(1.0) }
        /// Pan by a canvas delta: what was under the cursor follows it.
        pub fn pan_px(&mut self, dx: f32, dy: f32) {
            if !(dx.is_finite() && dy.is_finite()) { return; }
            let l = self.lens();
            let k = self.m_per_px();
            let t = self.target - l.right * (dx * k) + l.up * (dy * k);
            if t.is_finite() { self.target = t; }
        }
        /// Zoom by `factor` (above 1 = closer) keeping `q` where it is on screen; about the look-at point when
        /// `q` is `None`. The distance stays in range.
        pub fn zoom_about(&mut self, q: Option<Vec3>, factor: f32) {
            if !(factor.is_finite() && factor > 0.0) { return; }
            let d = self.distance_m;
            let d2 = (d / factor).clamp(DIST_MIN_M, DIST_MAX_M);
            let s = d2 / d;
            if let Some(q) = q.filter(|q| q.is_finite()) {
                let t = q + (self.target - q) * s;
                if t.is_finite() { self.target = t; }
            }
            self.distance_m = d2;
        }
        /// Look at a box from the current direction, near enough that it fills the view.
        pub fn fit(&mut self, min: Vec3, max: Vec3) {
            let corners: Vec<Vec3> = (0..8).map(|k| Vec3::new(if k & 1 == 0 { min.x } else { max.x }, if k & 2 == 0 { min.y } else { max.y }, if k & 4 == 0 { min.z } else { max.z })).collect();
            self.fit_points(min, max, &corners);
        }
        /// Look at the centre of a box from the current direction, as near as keeps every point in view. The
        /// points are the shape's outline (a ship's box is much larger on screen than the ship).
        pub fn fit_points(&mut self, min: Vec3, max: Vec3, points: &[Vec3]) {
            if !(min.is_finite() && max.is_finite()) || min.cmpgt(max).any() { return; }
            let r = (0.5 * (max - min).length()).max(0.5);
            let half_y = 0.5 * FOV_Y_DEG.to_radians();
            let aspect = (self.canvas[0] / self.canvas[1].max(1.0)).clamp(0.2, 5.0);
            let half_x = (half_y.tan() * aspect).atan();
            self.target = 0.5 * (min + max);
            // The sphere's distance always fits; then the nearest distance at which every corner of the box
            // still lies inside the framed part of the view (bisection: nearer only moves points outward).
            let far = (FRAME_MARGIN * r / half_y.min(half_x).sin()).clamp(DIST_MIN_M, DIST_MAX_M);
            let corners: Vec<Vec3> = points.iter().copied().filter(|p| p.is_finite()).collect();
            let fits = |d: f32| {
                let c = DockCam { distance_m: d, ..*self };
                let l = c.lens();
                let (mx, my) = (0.5 * (1.0 - FRAME_FILL) * l.canvas[0], 0.5 * (1.0 - FRAME_FILL) * l.canvas[1]);
                corners.iter().all(|p| l.project(*p).is_some_and(|s| s[0] >= mx && s[0] <= l.canvas[0] - mx && s[1] >= my && s[1] <= l.canvas[1] - my))
            };
            let (mut lo, mut hi) = (DIST_MIN_M, far);
            if !fits(hi) { self.distance_m = hi; return; }
            for _ in 0..24 { let mid = 0.5 * (lo + hi); if fits(mid) { hi = mid } else { lo = mid } }
            self.distance_m = hi;
            // Centre the shape on screen (its box centre seldom projects to the middle), then fit again.
            for _ in 0..3 {
                let l = self.lens();
                let (mut a, mut b) = ([f32::MAX; 2], [f32::MIN; 2]);
                for p in &corners { if let Some(s) = l.project(*p) { a = [a[0].min(s[0]), a[1].min(s[1])]; b = [b[0].max(s[0]), b[1].max(s[1])]; } }
                if a[0] > b[0] { return; }
                let (dx, dy) = (0.5 * l.canvas[0] - 0.5 * (a[0] + b[0]), 0.5 * l.canvas[1] - 0.5 * (a[1] + b[1]));
                self.pan_px(dx, dy);
                let fits = |d: f32| {
                    let c = DockCam { distance_m: d, ..*self };
                    let l = c.lens();
                    let (mx, my) = (0.5 * (1.0 - FRAME_FILL) * l.canvas[0], 0.5 * (1.0 - FRAME_FILL) * l.canvas[1]);
                    corners.iter().all(|p| l.project(*p).is_some_and(|s| s[0] >= mx && s[0] <= l.canvas[0] - mx && s[1] >= my && s[1] <= l.canvas[1] - my))
                };
                let (mut lo, mut hi) = (DIST_MIN_M, (self.distance_m * 2.0).min(DIST_MAX_M));
                if !fits(hi) { continue; }
                for _ in 0..24 { let mid = 0.5 * (lo + hi); if fits(mid) { hi = mid } else { lo = mid } }
                self.distance_m = hi;
            }
        }
    }

    /// The trackpad settings (L241), read from `editor.*` settings.
    #[derive(Clone, Copy, Debug, PartialEq)]
    pub struct Trackpad { pub drag_pans: bool, pub invert_orbit: bool, pub invert_pan: bool, pub invert_zoom: bool, pub orbit_speed: f32, pub pan_speed: f32, pub zoom_speed: f32 }
    impl Default for Trackpad { fn default() -> Self { Self { drag_pans: false, invert_orbit: false, invert_pan: false, invert_zoom: false, orbit_speed: 1.0, pan_speed: 1.0, zoom_speed: 1.0 } } }

    /// What one trackpad event asks of the camera.
    #[derive(Clone, Copy, Debug, PartialEq)]
    pub enum Move { Orbit { dx: f32, dy: f32 }, Pan { dx: f32, dy: f32 }, Zoom { factor: f32 }, None }

    fn speed(v: f32) -> f32 { if v.is_finite() { v.clamp(0.05, 5.0) } else { 1.0 } }
    fn sign(invert: bool) -> f32 { if invert { -1.0 } else { 1.0 } }

    /// The mapping table (L237-242). `command` + vertical drag zooms; otherwise the drag orbits, or pans with
    /// the switch on Pan; Shift swaps the two. Control is never a modifier here (it is macOS secondary click).
    pub fn map_pan_gesture(delta: [f32; 2], shift: bool, command: bool, s: &Trackpad) -> Move {
        let [x, y] = delta;
        if !(x.is_finite() && y.is_finite()) || (x == 0.0 && y == 0.0) { return Move::None; }
        if command {
            if y == 0.0 { return Move::None; }
            return Move::Zoom { factor: (-y * ZOOM_PER_UNIT * speed(s.zoom_speed) * sign(s.invert_zoom)).exp() };
        }
        let pans = s.drag_pans != shift;
        if pans {
            let k = PAN_GESTURE_PX * speed(s.pan_speed) * sign(s.invert_pan);
            Move::Pan { dx: -x * k, dy: -y * k }
        } else {
            let k = PAN_GESTURE_PX * speed(s.orbit_speed) * sign(s.invert_orbit);
            Move::Orbit { dx: -x * k, dy: -y * k }
        }
    }
    /// A pinch: Godot's factor is above 1 when the fingers spread, which zooms in.
    pub fn map_magnify(factor: f64, s: &Trackpad) -> Move {
        if !(factor.is_finite() && factor > 0.0) || factor == 1.0 { return Move::None; }
        let f = (factor as f32).clamp(0.2, 5.0).powf(speed(s.zoom_speed) * sign(s.invert_zoom));
        Move::Zoom { factor: f }
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum Kind { Orbit, Pan, Zoom }

    #[derive(Clone, Copy, Debug, PartialEq)]
    struct Gesture { kind: Kind, start_s: f64, last_s: f64, screen: [f32; 2], pivot: Vec3, sum: [f32; 2], class: Option<(u8, i8)> }

    /// Gesture tracking and the trackpad gate counters (L250).
    #[derive(Clone, Debug, Default, PartialEq)]
    pub struct Gestures { current: Option<Gesture>, last_orbit: Option<(u8, i8, f64)>, pub resets: u32, pub reversals: u32 }
    impl Gestures {
        fn finish(&mut self) {
            if let Some(g) = self.current.take() {
                if g.kind == Kind::Orbit { if let Some((axis, s)) = g.class { self.last_orbit = Some((axis, s, g.last_s)); } }
            }
        }
        /// One event of a gesture of `kind` at `screen`. Returns the pivot fixed at the gesture's start;
        /// `pick` gives it when a gesture starts.
        pub fn event(&mut self, kind: Kind, now_s: f64, screen: [f32; 2], delta: [f32; 2], pick: &mut dyn FnMut() -> Vec3) -> Vec3 {
            let now = if now_s.is_finite() { now_s } else { 0.0 };
            let fresh = match &self.current {
                Some(g) => g.kind != kind || now - g.last_s > GESTURE_GAP_S || now < g.last_s
                    || (screen[0] - g.screen[0]).abs().max((screen[1] - g.screen[1]).abs()) > GESTURE_JUMP_PX,
                None => true,
            };
            if fresh {
                self.finish();
                self.current = Some(Gesture { kind, start_s: now, last_s: now, screen, pivot: pick(), sum: [0.0; 2], class: None });
            }
            let Some(g) = self.current.as_mut() else { return Vec3::ZERO };
            g.last_s = now;
            if screen[0].is_finite() && screen[1].is_finite() { g.screen = screen; }
            if delta[0].is_finite() && delta[1].is_finite() { g.sum[0] += delta[0]; g.sum[1] += delta[1]; }
            if g.kind == Kind::Orbit && g.class.is_none() && g.sum[0].abs().max(g.sum[1].abs()) >= CLASSIFY_PX {
                let axis = if g.sum[0].abs() >= g.sum[1].abs() { 0u8 } else { 1u8 };
                let s: i8 = if g.sum[axis as usize] >= 0.0 { 1 } else { -1 };
                g.class = Some((axis, s));
                if let Some((a, s0, end)) = self.last_orbit {
                    if a == axis && s0 == -s && g.start_s - end <= REVERSAL_WINDOW_S { self.reversals += 1; }
                }
            }
            g.pivot
        }
        /// A reset (`frame`) ends any gesture.
        pub fn reset(&mut self) { self.finish(); self.resets += 1; }
    }
}

// =========================================================================================================
// Picking (no Godot)
// =========================================================================================================

pub(crate) mod pick {
    use content::blueprint::Blueprint;
    use content::catalogue::Catalogue;
    use content::cell::{CellGrid, CellPos, CELL_M};
    use content::enums::Axis;
    use glam::Vec3;

    pub const CELL: f32 = CELL_M as f32;

    /// One placed part's footprint box in editor metres.
    #[derive(Clone, Debug, PartialEq)]
    pub struct PartBox { pub slot: u16, pub kit: Option<u16>, pub min: Vec3, pub max: Vec3, pub bottom_row: i16 }

    /// A deck or bulkhead: its index among the planes of its axis (ascending offset) and its row.
    #[derive(Clone, Copy, Debug, PartialEq)]
    pub struct Plane { pub axis: Axis, pub index: usize, pub row: i16 }

    /// What a ray hit.
    #[derive(Clone, Copy, Debug, PartialEq)]
    pub enum Hit { Cell { cell: CellPos, face: u8, t: f32 }, Part { index: usize, cell: CellPos, face: u8, t: f32 }, Plane { plane: Plane, t: f32 } }
    impl Hit { pub fn t(&self) -> f32 { match *self { Hit::Cell { t, .. } | Hit::Part { t, .. } | Hit::Plane { t, .. } => t } } }

    /// The design as picking sees it, for one revision and one deck slice.
    pub struct PickScene { pub grid: CellGrid, pub parts: Vec<PartBox>, pub planes: Vec<Plane>, pub cut_row: Option<i16> }

    /// Nearest entry distance (0 when inside) of a ray into a box, and the axis entered through.
    pub fn ray_box(o: Vec3, d: Vec3, min: Vec3, max: Vec3) -> Option<(f32, usize)> {
        let (mut t0, mut t1, mut axis) = (0.0f32, f32::MAX, usize::MAX);
        for a in 0..3 {
            if d[a].abs() < 1e-9 {
                if o[a] < min[a] || o[a] > max[a] { return None; }
                continue;
            }
            let (n, f) = ((min[a] - o[a]) / d[a], (max[a] - o[a]) / d[a]);
            let (n, f) = if n <= f { (n, f) } else { (f, n) };
            if n > t0 { t0 = n; axis = a; }
            t1 = t1.min(f);
            if t0 > t1 { return None; }
        }
        Some((t0, axis))
    }
    /// The face a ray entered through along `axis` (index into `Face::ALL`: X-, X+, Y-, Y+, Z-, Z+).
    pub fn entry_face(axis: usize, d: Vec3) -> u8 { (2 * axis.min(2) + if d[axis.min(2)] > 0.0 { 0 } else { 1 }) as u8 }

    impl PickScene {
        /// The scene of a blueprint: its structure grid, its parts' boxes and its planes, cut above `cut_row`.
        pub fn build(bp: &Blueprint, cat: &Catalogue, cut_row: Option<i16>) -> PickScene {
            let grid = CellGrid::from_rows(&bp.cells);
            let mut parts = Vec::with_capacity(bp.parts.len());
            for (i, p) in bp.parts.iter().enumerate() {
                let Some(def) = cat.part(&p.part) else { continue };
                let cells = crate::editor::parts::footprint_cells(def, p.pos, p.rot);
                let Some(first) = cells.first() else { continue };
                let (mut lo, mut hi) = ([first.x, first.y, first.z], [first.x, first.y, first.z]);
                for c in &cells { for (k, v) in [c.x, c.y, c.z].into_iter().enumerate() { lo[k] = lo[k].min(v); hi[k] = hi[k].max(v); } }
                let min = Vec3::new(lo[0] as f32, lo[1] as f32, lo[2] as f32) * CELL;
                let max = Vec3::new(hi[0] as f32 + 1.0, hi[1] as f32 + 1.0, hi[2] as f32 + 1.0) * CELL;
                parts.push(PartBox { slot: i as u16, kit: p.kit.map(|k| k.0), min, max, bottom_row: lo[1] });
            }
            let mut planes = Vec::new();
            for axis in [Axis::Y, Axis::X] {
                let mut rows: Vec<i16> = bp.hull.planes.iter().filter(|p| p.axis == axis).map(|p| p.offset).collect();
                rows.sort_unstable();
                planes.extend(rows.into_iter().enumerate().map(|(index, row)| Plane { axis, index, row }));
            }
            PickScene { grid, parts, planes, cut_row }
        }
        fn shown(&self, c: CellPos) -> bool { self.cut_row.is_none_or(|r| c.y <= r) && !self.grid.get(c).is_empty() }
        pub fn part_shown(&self, p: &PartBox) -> bool { self.cut_row.is_none_or(|r| p.bottom_row <= r) }
        /// The structure box in metres.
        pub fn grid_box(&self) -> (Vec3, Vec3) { (Vec3::ZERO, Vec3::new(self.grid.size[0] as f32, self.grid.size[1] as f32, self.grid.size[2] as f32) * CELL) }
        /// Everything shown: structure and parts.
        pub fn bounds(&self) -> Option<(Vec3, Vec3)> {
            let mut lo = Vec3::splat(f32::MAX);
            let mut hi = Vec3::splat(f32::MIN);
            let [sx, sy, sz] = self.grid.size.map(|s| s as i16);
            let mut any = false;
            for y in 0..sy { for z in 0..sz { for x in 0..sx {
                let c = CellPos::new(x, y, z);
                if !self.shown(c) { continue; }
                any = true;
                let m = Vec3::new(x as f32, y as f32, z as f32) * CELL;
                lo = lo.min(m);
                hi = hi.max(m + CELL);
            }}}
            for p in self.parts.iter().filter(|p| self.part_shown(p)) { any = true; lo = lo.min(p.min); hi = hi.max(p.max); }
            any.then_some((lo, hi))
        }

        /// The box of the shown structure in a plane's own slice of cells (the row of a deck, the column of a
        /// bulkhead), in metres.
        pub fn slice_extent(&self, p: &Plane) -> Option<(Vec3, Vec3)> {
            let [sx, sy, sz] = self.grid.size.map(|s| s as i16);
            let (mut lo, mut hi) = (Vec3::splat(f32::MAX), Vec3::splat(f32::MIN));
            let mut any = false;
            let mut take = |c: CellPos| {
                if !self.shown(c) { return; }
                any = true;
                let m = Vec3::new(c.x as f32, c.y as f32, c.z as f32) * CELL;
                lo = lo.min(m);
                hi = hi.max(m + CELL);
            };
            match p.axis {
                Axis::Y => { if p.row < 0 || p.row >= sy { return None; } for z in 0..sz { for x in 0..sx { take(CellPos::new(x, p.row, z)); } } }
                _ => { if p.row < 0 || p.row >= sx { return None; } for y in 0..sy { for z in 0..sz { take(CellPos::new(p.row, y, z)); } } }
            }
            any.then_some((lo, hi))
        }

        /// Points on the outline of what is shown, for framing: per slice of columns along the length, the
        /// corners of the shown cells' extent, plus every shown part box's corners.
        pub fn outline(&self) -> Vec<Vec3> {
            let [sx, sy, sz] = self.grid.size.map(|s| s as i16);
            let step = (sx / 48).max(1);
            let mut out = Vec::new();
            let mut x0 = 0;
            while x0 < sx {
                let x1 = (x0 + step).min(sx);
                let (mut lo, mut hi) = ([i16::MAX; 2], [i16::MIN; 2]);
                for x in x0..x1 { for y in 0..sy { for z in 0..sz {
                    if self.shown(CellPos::new(x, y, z)) { lo = [lo[0].min(y), lo[1].min(z)]; hi = [hi[0].max(y), hi[1].max(z)]; }
                }}}
                if lo[0] <= hi[0] {
                    for k in 0..8 {
                        let x = if k & 1 == 0 { x0 } else { x1 } as f32;
                        let y = if k & 2 == 0 { lo[0] } else { hi[0] + 1 } as f32;
                        let z = if k & 4 == 0 { lo[1] } else { hi[1] + 1 } as f32;
                        out.push(Vec3::new(x, y, z) * CELL);
                    }
                }
                x0 = x1;
            }
            for p in self.parts.iter().filter(|p| self.part_shown(p)) {
                for k in 0..8 { out.push(Vec3::new(if k & 1 == 0 { p.min.x } else { p.max.x }, if k & 2 == 0 { p.min.y } else { p.max.y }, if k & 4 == 0 { p.min.z } else { p.max.z })); }
            }
            out
        }

        /// The first shown structure cell along a ray (Amanatides and Woo): cell, entry face, distance.
        pub fn ray_cells(&self, o: Vec3, d: Vec3) -> Option<(CellPos, u8, f32)> {
            if !(o.is_finite() && d.is_finite()) || d.length_squared() < 1e-12 { return None; }
            let d = d.normalize();
            let (bmin, bmax) = self.grid_box();
            let (t0, axis) = ray_box(o, d, bmin, bmax)?;
            let size = self.grid.size.map(|s| s as i32);
            let p = o + d * (t0 + 1e-4);
            let mut c = [0i32; 3];
            for k in 0..3 { c[k] = ((p[k] / CELL).floor() as i32).clamp(0, size[k] - 1); }
            let step: [i32; 3] = [d.x, d.y, d.z].map(|v| if v > 0.0 { 1 } else if v < 0.0 { -1 } else { 0 });
            let mut t_max = [f32::MAX; 3];
            let mut t_delta = [f32::MAX; 3];
            for k in 0..3 {
                if step[k] != 0 {
                    let edge = (c[k] + if step[k] > 0 { 1 } else { 0 }) as f32 * CELL;
                    t_max[k] = (edge - o[k]) / d[k];
                    t_delta[k] = CELL / d[k].abs();
                }
            }
            let mut face = if axis == usize::MAX { entry_face(d.abs().max_position(), d) } else { entry_face(axis, d) };
            let mut t = t0;
            let limit = (size[0] + size[1] + size[2]) as usize + 3;
            for _ in 0..limit {
                let cell = CellPos::new(c[0] as i16, c[1] as i16, c[2] as i16);
                if self.shown(cell) { return Some((cell, face, t)); }
                let k = if t_max[0] <= t_max[1] && t_max[0] <= t_max[2] { 0 } else if t_max[1] <= t_max[2] { 1 } else { 2 };
                if step[k] == 0 { return None; }
                c[k] += step[k];
                if c[k] < 0 || c[k] >= size[k] { return None; }
                t = t_max[k];
                t_max[k] += t_delta[k];
                face = entry_face(k, d);
            }
            None
        }
        /// The nearest shown part box along a ray: index, cell under the ray, entry face, distance.
        pub fn ray_parts(&self, o: Vec3, d: Vec3) -> Option<(usize, CellPos, u8, f32)> {
            if !(o.is_finite() && d.is_finite()) || d.length_squared() < 1e-12 { return None; }
            let d = d.normalize();
            let mut best: Option<(usize, CellPos, u8, f32)> = None;
            for (i, p) in self.parts.iter().enumerate() {
                if !self.part_shown(p) { continue; }
                let Some((t, axis)) = ray_box(o, d, p.min, p.max) else { continue };
                if best.is_some_and(|b| b.3 <= t) { continue; }
                let at = (o + d * (t + 1e-3)).clamp(p.min, p.max - 1e-4);
                let cell = CellPos::new((at.x / CELL).floor() as i16, (at.y / CELL).floor() as i16, (at.z / CELL).floor() as i16);
                let face = if axis == usize::MAX { entry_face(d.abs().max_position(), d) } else { entry_face(axis, d) };
                best = Some((i, cell, face, t));
            }
            best
        }
        /// A plane's position in metres along its axis (the centre of its row).
        pub fn plane_m(p: &Plane) -> f32 { (p.row as f32 + 0.5) * CELL }
        /// The nearest deck or bulkhead whose crossing point lies inside the hull's section.
        pub fn ray_planes(&self, o: Vec3, d: Vec3) -> Option<(Plane, f32)> {
            if !(o.is_finite() && d.is_finite()) || d.length_squared() < 1e-12 { return None; }
            let d = d.normalize();
            let (bmin, bmax) = self.grid_box();
            let mut best: Option<(Plane, f32)> = None;
            for p in &self.planes {
                let a = if p.axis == Axis::Y { 1 } else { 0 };
                if d[a].abs() < 1e-6 { continue; }
                if p.axis == Axis::Y && self.cut_row.is_some_and(|r| p.row > r) { continue; }
                let t = (Self::plane_m(p) - o[a]) / d[a];
                if t <= 0.0 || best.is_some_and(|b| b.1 <= t) { continue; }
                let q = o + d * t;
                if q.cmplt(bmin).any() || q.cmpgt(bmax).any() { continue; }
                let c = CellPos::new((q.x / CELL).floor() as i16, (q.y / CELL).floor() as i16, (q.z / CELL).floor() as i16);
                if self.inside(c) { best = Some((*p, t)); }
            }
            best
        }
        /// A cell inside the hull: structure itself, or structure on both sides across the beam.
        fn inside(&self, c: CellPos) -> bool {
            if !self.grid.get(c).is_empty() { return true; }
            let sz = self.grid.size[2] as i16;
            let solid = |z: i16| !self.grid.get(CellPos::new(c.x, c.y, z)).is_empty();
            (0..c.z).any(solid) && (c.z + 1..sz).any(solid)
        }
        /// Shown structure cells on the surface (a face open to the outside or the cut) within a sphere of
        /// `radius` cells of `centre`, ascending.
        pub fn brush(&self, centre: CellPos, radius: i32) -> Vec<CellPos> {
            let r = radius.clamp(0, 16);
            let mut out = Vec::new();
            for y in -r..=r { for z in -r..=r { for x in -r..=r {
                if x * x + y * y + z * z > r * r { continue; }
                let c = CellPos::new(centre.x + x as i16, centre.y + y as i16, centre.z + z as i16);
                if !self.grid.in_bounds(c) || !self.shown(c) { continue; }
                let open = content::cell::Face::ALL.iter().any(|f| { let (a, b, cc) = f.offset(); !self.shown(CellPos::new(c.x + a, c.y + b, c.z + cc)) });
                if open { out.push(c); }
            }}}
            out.sort();
            out
        }
    }
}

// =========================================================================================================
// Overlay geometry (no Godot)
// =========================================================================================================

pub(crate) mod geom {
    use glam::Vec3;

    /// Line patterns: (period in metres, share drawn); period 0 = solid.
    #[derive(Clone, Copy, Debug, PartialEq)]
    pub enum Pattern { Solid, Dashed, Dotted }
    impl Pattern {
        pub fn dash(self, unit: f32) -> [f32; 2] {
            match self { Pattern::Solid => [0.0, 1.0], Pattern::Dashed => [1.6 * unit, 0.6], Pattern::Dotted => [0.6 * unit, 0.3] }
        }
        pub fn icon(self) -> &'static str { match self { Pattern::Solid => "line_solid", Pattern::Dashed => "line_dashed", Pattern::Dotted => "line_dotted" } }
    }

    /// Screen-space lines: four vertices per segment. `other` holds the far end and the side (+1 or -1).
    #[derive(Clone, Debug, Default, PartialEq)]
    pub struct Lines { pub pos: Vec<[f32; 3]>, pub other: Vec<[f32; 4]>, pub along: Vec<[f32; 2]>, pub dash: Vec<[f32; 2]>, pub color: Vec<[f32; 4]>, pub idx: Vec<u32> }
    impl Lines {
        pub fn len(&self) -> usize { self.idx.len() / 6 }
        pub fn is_empty(&self) -> bool { self.idx.is_empty() }
        /// One segment; `s0` is the distance along the line at `a`, for the pattern.
        pub fn seg(&mut self, a: Vec3, b: Vec3, s0: f32, color: [f32; 4], dash: [f32; 2]) {
            if !(a.is_finite() && b.is_finite()) || a.distance_squared(b) < 1e-10 || self.pos.len() > (u32::MAX as usize - 8) { return; }
            let s1 = s0 + a.distance(b);
            let base = self.pos.len() as u32;
            for (p, q, side, s) in [(a, b, 1.0, s0), (a, b, -1.0, s0), (b, a, -1.0, s1), (b, a, 1.0, s1)] {
                self.pos.push(p.into());
                self.other.push([q.x, q.y, q.z, side]);
                self.along.push([s, 0.0]);
                self.dash.push(dash);
                self.color.push(color);
            }
            self.idx.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
        }
        pub fn polyline(&mut self, pts: &[Vec3], color: [f32; 4], dash: [f32; 2], closed: bool) {
            let mut s = 0.0;
            for w in pts.windows(2) { self.seg(w[0], w[1], s, color, dash); s += w[0].distance(w[1]); }
            if closed && pts.len() > 2 { self.seg(pts[pts.len() - 1], pts[0], s, color, dash); }
        }
        /// A circle in the plane through `c` spanned by `u` and `v` (unit, perpendicular).
        pub fn circle(&mut self, c: Vec3, u: Vec3, v: Vec3, r: f32, color: [f32; 4], dash: [f32; 2]) {
            let n = 48;
            let pts: Vec<Vec3> = (0..n).map(|i| { let a = std::f32::consts::TAU * i as f32 / n as f32; c + (u * a.cos() + v * a.sin()) * r }).collect();
            self.polyline(&pts, color, dash, true);
        }
    }

    /// Unshaded coloured triangles.
    #[derive(Clone, Debug, Default, PartialEq)]
    pub struct Fills { pub pos: Vec<[f32; 3]>, pub color: Vec<[f32; 4]>, pub idx: Vec<u32> }
    impl Fills {
        pub fn is_empty(&self) -> bool { self.idx.is_empty() }
        pub fn tri(&mut self, a: Vec3, b: Vec3, c: Vec3, color: [f32; 4]) {
            if !(a.is_finite() && b.is_finite() && c.is_finite()) { return; }
            let base = self.pos.len() as u32;
            for p in [a, b, c] { self.pos.push(p.into()); self.color.push(color); }
            self.idx.extend_from_slice(&[base, base + 1, base + 2]);
        }
        pub fn quad(&mut self, a: Vec3, b: Vec3, c: Vec3, d: Vec3, color: [f32; 4]) { self.tri(a, b, c, color); self.tri(a, c, d, color); }
    }

    /// Bearing (degrees clockwise from the bow, seen from above) to a horizontal unit vector in the editor
    /// frame (+X bow, +Z starboard).
    pub fn bearing(deg: f64) -> Vec3 { let r = (deg as f32).to_radians(); Vec3::new(r.cos(), 0.0, r.sin()) }

    /// The clockwise sweep from `from` to `to`, degrees, in (0, 360].
    pub fn sweep(from: f64, to: f64) -> f64 { let s = (to - from).rem_euclid(360.0); if s <= 1e-9 { 360.0 } else { s } }

    /// A sector on the horizontal plane at height `y` around `c`: filled, outlined, and hatched when
    /// `hatch` is set (stripes at 45 degrees to the bow, `hatch_gap` metres apart).
    #[allow(clippy::too_many_arguments)]
    pub fn sector(fills: &mut Fills, lines: &mut Lines, c: Vec3, r: f32, from: f64, to: f64, fill: [f32; 4], edge: [f32; 4], hatch: Option<([f32; 4], f32)>) {
        if !(r.is_finite() && r > 0.0 && from.is_finite() && to.is_finite()) { return; }
        let sw = sweep(from, to);
        let n = ((sw / 4.0).ceil() as usize).clamp(1, 90);
        let pts: Vec<Vec3> = (0..=n).map(|i| c + bearing(from + sw * i as f64 / n as f64) * r).collect();
        for w in pts.windows(2) { fills.tri(c, w[0], w[1], fill); }
        lines.polyline(&pts, edge, Pattern::Solid.dash(1.0), false);
        if sw < 360.0 {
            lines.seg(c, pts[0], 0.0, edge, Pattern::Solid.dash(1.0));
            lines.seg(c, pts[n], 0.0, edge, Pattern::Solid.dash(1.0));
        }
        if let Some((colour, gap)) = hatch {
            // Stripes along (1, 0, 1), spaced across (1, 0, -1), clipped to the sector.
            let along = Vec3::new(1.0, 0.0, 1.0).normalize();
            let across = Vec3::new(1.0, 0.0, -1.0).normalize();
            let gap = gap.max(0.05);
            let k = (r / gap).ceil() as i32;
            for i in -k..=k {
                let base = c + across * (i as f32 * gap);
                // Sample the stripe and keep the runs inside the sector.
                let steps = 64;
                // (first, last) inside sample of the run in progress.
                let mut run: Option<(Vec3, Vec3)> = None;
                for j in 0..=steps {
                    let t = -r + 2.0 * r * j as f32 / steps as f32;
                    let p = base + along * t;
                    let v = p - c;
                    let inside = v.length() <= r && (sw >= 360.0 || { let b = (v.z as f64).atan2(v.x as f64).to_degrees().rem_euclid(360.0); sweep(from, b) <= sw });
                    run = match (inside, run) {
                        (true, None) => Some((p, p)),
                        (true, Some((s, _))) => Some((s, p)),
                        (false, Some((s, e))) => { lines.seg(s, e, 0.0, colour, Pattern::Solid.dash(1.0)); None }
                        (false, None) => None,
                    };
                }
                if let Some((s, e)) = run { lines.seg(s, e, 0.0, colour, Pattern::Solid.dash(1.0)); }
            }
        }
    }

    /// A polar curve on the horizontal plane: 36 values per 10 degrees from the bow, scaled so the largest
    /// reaches `r`.
    pub fn polar(lines: &mut Lines, c: Vec3, values: &[f32], r: f32, colour: [f32; 4], dash: [f32; 2]) {
        let max = values.iter().copied().filter(|v| v.is_finite()).fold(0.0f32, f32::max);
        if values.is_empty() || max <= 0.0 { return; }
        let pts: Vec<Vec3> = values.iter().enumerate().map(|(i, v)| c + bearing(i as f64 * 360.0 / values.len() as f64) * (r * (v.max(0.0) / max).max(0.02))).collect();
        lines.polyline(&pts, colour, dash, true);
    }

    /// A heat value 0..=255 on the ramp between two colours; `None` under the drawing floor.
    pub fn heat(v: u8, low: [f32; 4], high: [f32; 4]) -> Option<[f32; 4]> {
        if v < 16 { return None; }
        let t = (v as f32 - 16.0) / (255.0 - 16.0);
        Some([low[0] + (high[0] - low[0]) * t, low[1] + (high[1] - low[1]) * t, low[2] + (high[2] - low[2]) * t, 0.55 + 0.4 * t])
    }
}

// =========================================================================================================
// Overlay inputs that take time: computed off the main thread per design
// =========================================================================================================

/// The waterline contours sit this far outboard of the plating and above their row's water plane, so the
/// hull hides the far side and the dock's water does not swallow the line.
const CONTOUR_OUTBOARD_M: f32 = 0.06;
const CONTOUR_LIFT_M: f32 = 0.04;

/// Balance of a design at its calm-water equilibrium (editor metres).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct Balance { pub cg: glam::Vec3, pub cb: glam::Vec3, pub waterline_m: f32, pub heel_deg: f32, pub trim_deg: f32, pub floats: bool }

/// Everything the slow overlays draw, for one design.
#[derive(Clone, Debug, Default)]
pub(crate) struct SlowData {
    pub hash: content::ids::ContentHash,
    /// Which parts are filled, as overlay bits (`bits::SLOW` of them; the balance answers both
    /// `WATERLINE` and `CG_CB`).
    pub have: i64,
    pub balance: Option<Balance>,
    /// (slot, position, sectors) per mount and per sensor.
    pub arcs: Vec<(u16, glam::Vec3, Vec<sim_systems::editor::gauges::ArcSector>)>,
    pub blind: Vec<(u16, glam::Vec3, Vec<sim_systems::editor::gauges::ArcSector>)>,
    /// Outer cell centres and their vulnerability.
    pub heat: Vec<(glam::Vec3, u8)>,
    pub signature: Option<sim_core::vessel::ProjectedAreas>,
    pub millis: f32,
}

fn v3d(v: glam::DVec3) -> glam::Vec3 { glam::Vec3::new(v.x as f32, v.y as f32, v.z as f32) }

/// Overlay bits whose inputs are slow data, and what each asks for.
pub(crate) fn slow_need(overlay_bits: i64) -> i64 {
    let mut need = overlay_bits & (bits::ARCS | bits::BLIND | bits::HEATMAP | bits::SIGNATURE);
    if overlay_bits & (bits::WATERLINE | bits::CG_CB) != 0 { need |= bits::WATERLINE | bits::CG_CB; }
    need
}

impl SlowData {
    /// Take the parts `other` computed for the same design.
    pub(crate) fn merge(&mut self, other: SlowData) {
        if other.hash != self.hash { *self = other; return; }
        if other.have & bits::WATERLINE != 0 { self.balance = other.balance; }
        if other.have & bits::ARCS != 0 { self.arcs = other.arcs; }
        if other.have & bits::BLIND != 0 { self.blind = other.blind; }
        if other.have & bits::HEATMAP != 0 { self.heat = other.heat; }
        if other.have & bits::SIGNATURE != 0 { self.signature = other.signature; }
        self.have |= other.have;
        self.millis += other.millis;
    }
}

/// Compute the parts of a design's slow overlay data that `want` asks for (`slow_need` bits). Runs on a
/// thread of its own.
pub(crate) fn slow_data(design: &sim_systems::ship::ShipDesign, cat: &content::catalogue::Catalogue, want: i64) -> SlowData {
    let start = std::time::Instant::now();
    let want = slow_need(want);
    let mut out = SlowData { hash: design.hash, have: want, ..Default::default() };
    let prefab = &design.prefab;
    let grid = &prefab.grid.design;
    let at = |slot: content::ids::PartSlot| prefab.parts.get(slot.index()).map_or(glam::Vec3::ZERO, |p| v3d(p.centre));
    if want & bits::WATERLINE != 0 { out.balance = Some(balance_of(design, cat)); }
    if want & bits::ARCS != 0 { out.arcs = design.mounts.iter().map(|m| (m.slot.0, at(m.slot), sim_systems::editor::gauges::weapon_arcs(design, m.slot, cat))).filter(|a| !a.2.is_empty()).collect(); }
    if want & bits::BLIND != 0 { out.blind = design.sensor_slots.iter().map(|s| (s.0, at(*s), sim_systems::editor::gauges::blind_sectors(design, *s, cat))).filter(|a| !a.2.is_empty()).collect(); }
    if want & bits::HEATMAP != 0 {
        out.heat = sim_systems::editor::gauges::vulnerability_heatmap(design, cat).into_iter().filter(|(i, _)| i.index() < grid.cells.len()).map(|(i, v)| {
            let c = grid.pos(i).centre_m();
            (glam::Vec3::new(c[0] as f32, c[1] as f32, c[2] as f32), v)
        }).collect();
    }
    if want & bits::SIGNATURE != 0 { out.signature = Some(sim_systems::editor::gauges::signature_view(prefab, cat)); }
    out.millis = start.elapsed().as_secs_f32() * 1_000.0;
    out
}

/// Centres of gravity and buoyancy and the waterline at the calm-water equilibrium.
fn balance_of(design: &sim_systems::ship::ShipDesign, cat: &content::catalogue::Catalogue) -> Balance {
    let prefab = &design.prefab;
    let mass = sim_core::mass::with_fluids(&prefab.dry_mass, &prefab.parts, &prefab.compartments, cat);
    let eq = sim_core::hydro::equilibrium(&prefab.blocks, &mass, &[], &prefab.compartments, &cat.tuning.buoyancy);
    let grid = &prefab.grid.design;
    let keel = (0..grid.size[1] as i16).find(|&y| (0..grid.size[2] as i16).any(|z| (0..grid.size[0] as i16).any(|x| !grid.get(content::cell::CellPos::new(x, y, z)).is_empty()))).unwrap_or(0);
    let fin = |v: f64| if v.is_finite() { v as f32 } else { 0.0 };
    Balance {
        cg: if mass.com.is_finite() { v3d(mass.com) } else { glam::Vec3::ZERO },
        cb: if eq.cb.is_finite() { v3d(eq.cb) } else { glam::Vec3::ZERO },
        waterline_m: keel as f32 * pick::CELL + fin(eq.draft_m),
        heel_deg: fin(eq.heel_rad.to_degrees()),
        trim_deg: fin(eq.trim_rad.to_degrees()),
        floats: eq.floats,
    }
}

// =========================================================================================================
// Colours (from res://ui/theme/tokens.json)
// =========================================================================================================

/// Overlay colours in linear RGBA, plus the sRGB forms labels take.
#[derive(Clone, Debug, Default)]
pub(crate) struct Palette { pub c: std::collections::BTreeMap<&'static str, [f32; 4]>, pub srgb: std::collections::BTreeMap<&'static str, Color> }
impl Palette {
    /// A role's colour; a role whose token is missing (logged at load) draws in text.primary, and with no
    /// tokens at all in the engine's white.
    pub fn get(&self, k: &str) -> [f32; 4] { self.c.get(k).or_else(|| self.c.get("text")).copied().unwrap_or([Color::WHITE.r, Color::WHITE.g, Color::WHITE.b, 1.0]) }
    /// The sRGB form of a role for labels, falling back the same way.
    pub fn label(&self, k: &str) -> Color { self.srgb.get(k).or_else(|| self.srgb.get("text")).copied().unwrap_or(Color::WHITE) }
    pub fn alpha(&self, k: &str, a: f32) -> [f32; 4] { let mut c = self.get(k); c[3] = a; c }
}
const TOKENS: &str = "res://ui/theme/tokens.json";
/// Overlay roles and the token each reads: `overlay.*` is the categorical overlay palette; the rest are the
/// interface palette (dusk or high contrast).
const ROLES: &[(&str, &str, bool)] = &[
    ("waterline", "waterline", true), ("cg", "cg", true), ("cb", "cb", true), ("arc", "arc", true), ("blind", "blind", true),
    ("power", "power", true), ("fuel", "fuel", true), ("data", "data", true), ("heat_low", "heat_low", true), ("heat_high", "heat_high", true),
    ("plane", "plane", true), ("rcs", "signature_rcs", true), ("visual", "signature_visual", true), ("ghost", "ghost", true),
    ("ghost_preview", "ghost_preview", true), ("warning", "alert.warning", false), ("text", "text.primary", false), ("inset", "surface.inset", false),
    ("select", "select", false),
];
fn load_palette(high_contrast: bool) -> Palette {
    let mut p = Palette::default();
    let text = godot::classes::FileAccess::get_file_as_string(TOKENS);
    let parsed = godot::classes::Json::parse_string(&text);
    let Ok(root) = parsed.try_to::<VarDictionary>() else { godot_warn!("dock view: {TOKENS} did not parse; overlays draw grey"); return p };
    let palettes = root.get("palettes").and_then(|v| v.try_to::<VarDictionary>().ok()).unwrap_or_default();
    let get = |name: &str| palettes.get(name).and_then(|v| v.try_to::<VarDictionary>().ok()).unwrap_or_default();
    let (overlay, ui) = (get("overlay"), get(if high_contrast { "high_contrast" } else { "dusk" }));
    for (role, key, cat) in ROLES {
        let dict = if *cat { &overlay } else { &ui };
        let Some(hex) = dict.get(*key).and_then(|v| v.try_to::<GString>().ok()) else { godot_warn!("dock view: token {key} is missing"); continue };
        let Some(srgb) = Color::from_html(&hex.to_string()) else { godot_warn!("dock view: token {key} is not a colour"); continue };
        let lin = srgb.srgb_to_linear();
        p.c.insert(role, [lin.r, lin.g, lin.b, 1.0]);
        p.srgb.insert(role, srgb);
    }
    // High contrast hatches blocked arcs in text.primary (tokens.json `high_contrast_rules.blocked_arc_hatch`).
    if high_contrast { if let Some(t) = p.c.get("text").copied() { p.c.insert("blocked", t); } }
    if !p.c.contains_key("blocked") { if let Some(w) = p.c.get("warning").copied() { p.c.insert("blocked", w); } }
    p
}

// =========================================================================================================
// What the overlays draw, from the design and the slow data
// =========================================================================================================

/// Inputs of one overlay build.
pub(crate) struct OverlayIn<'a> {
    pub bits: i64,
    pub focus: &'a [u16],
    pub design: &'a sim_systems::ship::ShipDesign,
    pub slow: Option<&'a SlowData>,
    pub scene: &'a pick::PickScene,
    pub design_waterline_row: i16,
}
/// One label: position, text key, colour role.
pub(crate) type LabelSpec = (glam::Vec3, &'static str, &'static str);
#[derive(Default)]
pub(crate) struct OverlayOut { pub top: geom::Lines, pub world: geom::Lines, pub fills: geom::Fills, pub sheets: geom::Fills, pub boxes: Vec<(glam::Vec3, [f32; 4])>, pub labels: Vec<LabelSpec> }

/// Network colour role, pattern and label key.
pub(crate) fn network_style(n: content::enums::NetworkKind) -> (&'static str, geom::Pattern, &'static str) {
    use content::enums::NetworkKind as N;
    match n { N::Power => ("power", geom::Pattern::Solid, "dock.overlay.power"), N::Fuel => ("fuel", geom::Pattern::Dashed, "dock.overlay.fuel"), N::Data => ("data", geom::Pattern::Dotted, "dock.overlay.data") }
}
/// Most doubled-route badges drawn.
const MAX_BADGES: usize = 64;

/// The selection as white corner brackets around each selected kit or part, drawn over the ship (DESIGN.md
/// "Interface and visual design": selection is a white bracket). Each corner gets three ticks a quarter of
/// the box's shortest side long.
pub(crate) fn selection_brackets(top: &mut geom::Lines, boxes: &[[i16; 6]], pal: &Palette) {
    use glam::Vec3;
    let col = pal.get("select");
    for b in boxes {
        let lo = Vec3::new(b[0] as f32, b[1] as f32, b[2] as f32) * pick::CELL;
        let hi = Vec3::new(b[3] as f32 + 1.0, b[4] as f32 + 1.0, b[5] as f32 + 1.0) * pick::CELL;
        let tick = ((hi - lo).min_element() * 0.25).max(0.3 * pick::CELL);
        for corner in 0..8u8 {
            let c = Vec3::new(if corner & 1 == 0 { lo.x } else { hi.x }, if corner & 2 == 0 { lo.y } else { hi.y }, if corner & 4 == 0 { lo.z } else { hi.z });
            let inward = Vec3::new(if corner & 1 == 0 { 1.0 } else { -1.0 }, if corner & 2 == 0 { 1.0 } else { -1.0 }, if corner & 4 == 0 { 1.0 } else { -1.0 });
            for axis in [Vec3::X, Vec3::Y, Vec3::Z] {
                top.seg(c, c + axis * inward * tick, 0.0, col, geom::Pattern::Solid.dash(1.0));
            }
        }
    }
}

pub(crate) fn build_overlays(i: &OverlayIn<'_>, pal: &Palette) -> OverlayOut {
    use geom::{Pattern, *};
    use glam::Vec3;
    let mut o = OverlayOut::default();
    let (bmin, bmax) = i.scene.bounds().unwrap_or_else(|| i.scene.grid_box());
    let length = (bmax.x - bmin.x).max(4.0);
    let unit = (length / 60.0).clamp(0.4, 2.0);
    let centre = 0.5 * (bmin + bmax);
    let focus = |slot: u16| i.focus.is_empty() || i.focus.contains(&slot);
    let wl_y = i.slow.and_then(|s| s.balance).map_or(i.design_waterline_row as f32 * pick::CELL, |b| b.waterline_m);

    if i.bits & bits::WATERLINE != 0 {
        let (heel, trim) = i.slow.and_then(|s| s.balance).map_or((0.0, 0.0), |b| (b.heel_deg.to_radians(), b.trim_deg.to_radians()));
        let pivot = Vec3::new(centre.x, wl_y, centre.z);
        let rot = glam::Quat::from_rotation_x(heel) * glam::Quat::from_rotation_z(trim);
        // The plane at rest, heeled and trimmed, as a frame a little wider than the hull: the dock's own water
        // already fills the plane, so a filled sheet would only hide it.
        let m = 0.06 * length + 1.0;
        let corners = [Vec3::new(bmin.x - m, 0.0, bmin.z - m), Vec3::new(bmax.x + m, 0.0, bmin.z - m), Vec3::new(bmax.x + m, 0.0, bmax.z + m), Vec3::new(bmin.x - m, 0.0, bmax.z + m)]
            .map(|c| pivot + rot * Vec3::new(c.x - pivot.x, 0.0, c.z - pivot.z));
        o.world.polyline(&corners, pal.alpha("waterline", 0.7), Pattern::Solid.dash(unit), true);
        // Where the equilibrium plane meets the hull, both sides, at the row holding it, a hair outboard of the
        // plating so the hull hides the far side instead of the line crossing the deck.
        let row = ((wl_y / pick::CELL).floor() as i16).clamp(0, i.scene.grid.size[1] as i16 - 1);
        let contour = |row: i16| -> (Vec<Vec3>, Vec<Vec3>) {
            let (mut port, mut stbd) = (Vec::new(), Vec::new());
            for x in 0..i.scene.grid.size[0] as i16 {
                let zs: Vec<i16> = (0..i.scene.grid.size[2] as i16).filter(|&z| !i.scene.grid.get(content::cell::CellPos::new(x, row, z)).is_empty()).collect();
                if let (Some(a), Some(b)) = (zs.first(), zs.last()) {
                    let xm = (x as f32 + 0.5) * pick::CELL;
                    port.push(Vec3::new(xm, 0.0, *a as f32 * pick::CELL - CONTOUR_OUTBOARD_M));
                    stbd.push(Vec3::new(xm, 0.0, (*b as f32 + 1.0) * pick::CELL + CONTOUR_OUTBOARD_M));
                }
            }
            (port, stbd)
        };
        let (port, stbd) = contour(row);
        let lift = |v: Vec<Vec3>, y: f32| -> Vec<Vec3> { v.into_iter().map(|p| pivot + rot * Vec3::new(p.x - pivot.x, y - pivot.y, p.z - pivot.z)).collect() };
        o.world.polyline(&lift(port, wl_y + CONTOUR_LIFT_M), pal.get("waterline"), Pattern::Solid.dash(unit), false);
        o.world.polyline(&lift(stbd, wl_y + CONTOUR_LIFT_M), pal.get("waterline"), Pattern::Solid.dash(unit), false);
        // The design waterline (the boot-topping row), dashed.
        let dy = (i.design_waterline_row as f32) * pick::CELL;
        let (port, stbd) = contour(i.design_waterline_row.clamp(0, i.scene.grid.size[1] as i16 - 1));
        let at = |v: Vec<Vec3>| -> Vec<Vec3> { v.into_iter().map(|p| Vec3::new(p.x, dy + CONTOUR_LIFT_M, p.z)).collect() };
        o.world.polyline(&at(port), pal.alpha("waterline", 0.8), Pattern::Dashed.dash(unit), false);
        o.world.polyline(&at(stbd), pal.alpha("waterline", 0.8), Pattern::Dashed.dash(unit), false);
    }

    if i.bits & bits::CG_CB != 0 {
        if let Some(b) = i.slow.and_then(|s| s.balance) {
            // Big enough to find on the hull from the framing distance: about the beam's tenth.
            let r = (1.8 * unit).max(0.5);
            for (axis, other) in [(Vec3::X, Vec3::Y), (Vec3::Y, Vec3::Z), (Vec3::Z, Vec3::X)] {
                o.top.seg(b.cg - axis * r * 1.4, b.cg + axis * r * 1.4, 0.0, pal.get("cg"), Pattern::Solid.dash(unit));
                let _ = other;
            }
            o.top.circle(b.cb, Vec3::X, Vec3::Z, r, pal.get("cb"), Pattern::Solid.dash(unit));
            o.top.circle(b.cb, Vec3::X, Vec3::Y, r, pal.get("cb"), Pattern::Solid.dash(unit));
            // The vertical through each, so the lever between them shows.
            o.top.seg(Vec3::new(b.cg.x, b.cb.y.min(b.cg.y) - 2.0 * r, b.cg.z), Vec3::new(b.cg.x, b.cb.y.max(b.cg.y) + 2.0 * r, b.cg.z), 0.0, pal.alpha("cg", 0.7), Pattern::Dotted.dash(unit));
            o.top.seg(Vec3::new(b.cb.x, b.cb.y.min(b.cg.y) - 2.0 * r, b.cb.z), Vec3::new(b.cb.x, b.cb.y.max(b.cg.y) + 2.0 * r, b.cb.z), 0.0, pal.alpha("cb", 0.7), Pattern::Dotted.dash(unit));
        }
    }

    // Arcs and blind sectors. With a focus, each focused mount or sensor is drawn in full from where it
    // stands; with none, every mount and sensor is summed into bearing rings around the ship, so a design
    // with many mounts still reads: the arc ring's strength is how many mounts bear, the hatched rings are
    // where own structure blocks a mount or blinds a sensor.
    let r_out = (1.25 * length).max(20.0);
    let ring_c = Vec3::new(centre.x, wl_y + 0.02, centre.z);
    if i.bits & bits::ARCS != 0 {
        let arcs = i.slow.map_or(&[][..], |s| s.arcs.as_slice());
        if i.focus.is_empty() {
            let (clear, blocked) = bearing_counts(arcs.iter().map(|a| a.2.as_slice()));
            ring_counts(&mut o, ring_c, 0.86 * r_out, r_out, &clear, pal.get("arc"), None);
            ring_counts(&mut o, ring_c, 0.76 * r_out, 0.84 * r_out, &blocked, pal.get("blocked"), Some(unit));
        } else {
            for (slot, at, sectors) in arcs {
                if !focus(*slot) { continue; }
                let c = Vec3::new(at.x, wl_y + 0.02, at.z);
                for s in sectors {
                    if s.blocked {
                        sector(&mut o.fills, &mut o.world, c, r_out, s.from_deg, s.to_deg, pal.alpha("blocked", 0.10), pal.alpha("blocked", 0.9), Some((pal.alpha("blocked", 0.8), 0.05 * r_out)));
                    } else {
                        sector(&mut o.fills, &mut o.world, c, r_out, s.from_deg, s.to_deg, pal.alpha("arc", 0.07), pal.alpha("arc", 0.9), None);
                    }
                }
            }
        }
    }

    if i.bits & bits::BLIND != 0 {
        let blind = i.slow.map_or(&[][..], |s| s.blind.as_slice());
        if i.focus.is_empty() {
            let (_, blocked) = bearing_counts(blind.iter().map(|a| a.2.as_slice()));
            ring_counts(&mut o, ring_c, 0.62 * r_out, 0.72 * r_out, &blocked, pal.get("blind"), Some(unit));
        } else {
            let r = 0.8 * r_out;
            for (slot, at, sectors) in blind {
                if !focus(*slot) { continue; }
                let c = Vec3::new(at.x, wl_y + 0.04, at.z);
                for s in sectors.iter().filter(|s| s.blocked) {
                    sector(&mut o.fills, &mut o.world, c, r, s.from_deg, s.to_deg, pal.alpha("blind", 0.12), pal.alpha("blind", 0.9), Some((pal.alpha("blind", 0.8), 0.05 * r)));
                }
            }
        }
    }

    if i.bits & (bits::CONDUITS | bits::XRAY) != 0 {
        let grid = &i.design.prefab.grid.design;
        // Each network's label goes at the route point nearest its own place along the hull (a quarter, a
        // half and three quarters of the length), so the three never sit on one another.
        let mut anchor: std::collections::BTreeMap<u8, (f32, Vec3)> = Default::default();
        let mut badges = 0;
        for c in &i.design.prefab.networks.conduits {
            let (role, pattern, _) = network_style(c.network);
            let pts: Vec<Vec3> = c.cells.iter().filter(|x| x.index() < grid.cells.len()).map(|x| { let m = grid.pos(*x).centre_m(); Vec3::new(m[0] as f32, m[1] as f32, m[2] as f32) }).collect();
            if pts.len() < 2 { continue; }
            // Each network runs a little apart from the others, so shared cells show every route.
            let shift = Vec3::splat(match c.network { content::enums::NetworkKind::Power => -0.08, content::enums::NetworkKind::Fuel => 0.0, content::enums::NetworkKind::Data => 0.08 });
            let pts: Vec<Vec3> = pts.into_iter().map(|p| p + shift).collect();
            o.top.polyline(&pts, pal.alpha(role, if c.redundant { 0.8 } else { 1.0 }), pattern.dash(unit * 0.6), false);
            let mid = pts[pts.len() / 2];
            let k = c.network as u8;
            let want = bmin.x + (bmax.x - bmin.x) * (k as f32 + 1.0) / 4.0;
            for p in &pts {
                let d = (p.x - want).abs();
                if anchor.get(&k).is_none_or(|(best, _)| d < *best) { anchor.insert(k, (d, *p)); }
            }
            if i.bits & bits::XRAY != 0 && c.redundant && badges < MAX_BADGES {
                // The badge: a small ring around the doubled route's middle, facing up.
                o.top.circle(mid, Vec3::X, Vec3::Z, 0.45 * unit, pal.get(role), Pattern::Solid.dash(unit));
                o.top.circle(mid, Vec3::X, Vec3::Z, 0.3 * unit, pal.get(role), Pattern::Solid.dash(unit));
                badges += 1;
            }
        }
        if i.bits & bits::XRAY != 0 {
            for (k, (_, mid)) in anchor {
                let n = match k { 0 => content::enums::NetworkKind::Power, 1 => content::enums::NetworkKind::Fuel, _ => content::enums::NetworkKind::Data };
                let (role, _, key) = network_style(n);
                o.labels.push((mid + Vec3::Y * 0.8 * unit, key, role));
            }
        }
    }

    if i.bits & bits::HEATMAP != 0 {
        let (lo, hi) = (pal.get("heat_low"), pal.get("heat_high"));
        let cut = i.scene.cut_row.map(|r| (r as f32 + 1.0) * pick::CELL);
        for (p, v) in i.slow.map_or(&[][..], |s| s.heat.as_slice()) {
            if cut.is_some_and(|c| p.y > c) { continue; }
            if let Some(col) = heat(*v, lo, hi) { o.boxes.push((*p, col)); }
        }
    }

    if i.bits & bits::COMPARTMENTS != 0 {
        let m = 0.1;
        for p in &i.scene.planes {
            let at = pick::PickScene::plane_m(p);
            if p.axis == content::enums::Axis::Y && i.scene.cut_row.is_some_and(|r| p.row > r) { continue; }
            // The sheet covers the structure's extent in the plane's own slice of cells.
            let Some((lo, hi)) = i.scene.slice_extent(p) else { continue };
            let (a, b, c, d) = match p.axis {
                content::enums::Axis::Y => (Vec3::new(lo.x - m, at, lo.z - m), Vec3::new(hi.x + m, at, lo.z - m), Vec3::new(hi.x + m, at, hi.z + m), Vec3::new(lo.x - m, at, hi.z + m)),
                _ => (Vec3::new(at, lo.y - m, lo.z - m), Vec3::new(at, hi.y + m, lo.z - m), Vec3::new(at, hi.y + m, hi.z + m), Vec3::new(at, lo.y - m, hi.z + m)),
            };
            o.sheets.quad(a, b, c, d, pal.alpha("plane", 0.12));
            o.top.polyline(&[a, b, c, d], pal.alpha("plane", 0.8), Pattern::Solid.dash(unit), true);
        }
    }

    if i.bits & bits::SIGNATURE != 0 {
        if let Some(sig) = i.slow.and_then(|s| s.signature.as_ref()) {
            let c = Vec3::new(centre.x, wl_y + 0.06, centre.z);
            let r = 0.6 * length;
            polar(&mut o.world, c, &sig.rcs_m2, r, pal.get("rcs"), Pattern::Solid.dash(unit));
            polar(&mut o.world, c, &sig.above_m2, 0.7 * r, pal.get("visual"), Pattern::Dashed.dash(unit));
        }
    }
    o
}

/// Bearing bins of the rings, degrees each.
const RING_BIN_DEG: usize = 2;
const RING_BINS: usize = 360 / RING_BIN_DEG;

/// Per bearing bin: how many of the sweeps are clear there, and how many are blocked.
pub(crate) fn bearing_counts<'a>(sweeps: impl Iterator<Item = &'a [sim_systems::editor::gauges::ArcSector]>) -> (Vec<u16>, Vec<u16>) {
    let (mut clear, mut blocked) = (vec![0u16; RING_BINS], vec![0u16; RING_BINS]);
    for sectors in sweeps {
        let (mut c, mut b) = ([false; RING_BINS], [false; RING_BINS]);
        for s in sectors {
            if !(s.from_deg.is_finite() && s.to_deg.is_finite()) { continue; }
            let sw = geom::sweep(s.from_deg, s.to_deg);
            for k in 0..RING_BINS {
                let mid = (k * RING_BIN_DEG) as f64 + 0.5 * RING_BIN_DEG as f64;
                if sw >= 360.0 || geom::sweep(s.from_deg, mid) <= sw { if s.blocked { b[k] = true } else { c[k] = true } }
            }
        }
        for k in 0..RING_BINS { clear[k] += c[k] as u16; blocked[k] += b[k] as u16; }
    }
    (clear, blocked)
}

/// A ring between two radii, each bearing bin filled by its count (stronger with more), hatched when
/// `hatch` gives the line unit, outlined on both edges.
fn ring_counts(o: &mut OverlayOut, c: glam::Vec3, r0: f32, r1: f32, counts: &[u16], colour: [f32; 4], hatch: Option<f32>) {
    let max = counts.iter().copied().max().unwrap_or(0);
    if max == 0 || counts.len() != RING_BINS { return; }
    let at = |deg: f64, r: f32| c + geom::bearing(deg) * r;
    for (k, &n) in counts.iter().enumerate() {
        if n == 0 { continue; }
        let (a, b) = ((k * RING_BIN_DEG) as f64, ((k + 1) * RING_BIN_DEG) as f64);
        let mut fill = colour;
        fill[3] = 0.12 + 0.38 * n as f32 / max as f32;
        o.fills.quad(at(a, r0), at(a, r1), at(b, r1), at(b, r0), fill);
        if let Some(unit) = hatch {
            let mut stroke = colour;
            stroke[3] = 0.9;
            o.world.seg(at(a, r0), at(b, r1), 0.0, stroke, geom::Pattern::Solid.dash(unit));
        }
    }
    // The ring's edges, where any bin is drawn.
    for r in [r0, r1] {
        let mut edge = colour;
        edge[3] = 0.85;
        for (k, &n) in counts.iter().enumerate() {
            if n == 0 { continue; }
            o.world.seg(at((k * RING_BIN_DEG) as f64, r), at(((k + 1) * RING_BIN_DEG) as f64, r), 0.0, edge, geom::Pattern::Solid.dash(1.0));
        }
    }
}

/// The legend for the overlays shown now (L1691: categorical layers need legends).
pub(crate) fn legend_rows(bits_on: i64, ghost: Option<GhostKind>) -> Vec<(i64, &'static str, &'static str)> {
    let mut v = Vec::new();
    if bits_on & bits::WATERLINE != 0 { v.push((bits::WATERLINE, "line_solid", "dock.overlay.waterline")); v.push((bits::WATERLINE, "line_dashed", "dock.overlay.design_waterline")); }
    if bits_on & bits::CG_CB != 0 { v.push((bits::CG_CB, "cross", "dock.overlay.cg")); v.push((bits::CG_CB, "ring", "dock.overlay.cb")); }
    if bits_on & bits::ARCS != 0 { v.push((bits::ARCS, "fill", "dock.overlay.arc")); v.push((bits::ARCS, "hatch", "dock.overlay.arc_blocked")); }
    if bits_on & bits::BLIND != 0 { v.push((bits::BLIND, "hatch", "dock.overlay.blind")); }
    let routes = bits_on & (bits::CONDUITS | bits::XRAY);
    if routes != 0 {
        let which = if bits_on & bits::XRAY != 0 { bits::XRAY } else { bits::CONDUITS };
        for n in [content::enums::NetworkKind::Power, content::enums::NetworkKind::Fuel, content::enums::NetworkKind::Data] {
            let (_, p, key) = network_style(n);
            v.push((which, p.icon(), key));
        }
        if bits_on & bits::XRAY != 0 { v.push((bits::XRAY, "badge", "dock.overlay.doubled")); }
    }
    if bits_on & bits::HEATMAP != 0 { v.push((bits::HEATMAP, "heat_low", "dock.overlay.heat_low")); v.push((bits::HEATMAP, "heat_high", "dock.overlay.heat_high")); }
    if bits_on & bits::COMPARTMENTS != 0 { v.push((bits::COMPARTMENTS, "fill", "dock.overlay.planes")); }
    if bits_on & bits::SIGNATURE != 0 { v.push((bits::SIGNATURE, "line_solid", "dock.overlay.rcs")); v.push((bits::SIGNATURE, "line_dashed", "dock.overlay.visual")); }
    if bits_on & bits::GHOST != 0 || ghost.is_some() {
        match ghost {
            Some(GhostKind::Edit) => { v.push((bits::GHOST, "fill", "dock.overlay.ghost_preview")); v.push((bits::GHOST, "outline", "dock.overlay.ghost_removed")); }
            _ => { v.push((bits::GHOST, "fill", "dock.overlay.ghost")); v.push((bits::GHOST, "outline", "dock.overlay.ghost_invalid")); }
        }
    }
    v
}

// =========================================================================================================
// The ghost
// =========================================================================================================

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum GhostKind { Place, Edit }

/// What identifies a ghost cheaply each frame: the pose of a placement, or the address of a previewed edit
/// (a new preview is a new box).
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum GhostKey { None, Part(content::ids::PartId, content::cell::CellPos, content::kit::PartRot), Kit(content::ids::KitId, content::cell::CellPos, content::kit::PartRot), Paste(content::cell::CellPos), Edit(usize) }
impl GhostKey {
    pub(crate) fn of(g: Option<&super::Ghost>) -> GhostKey {
        use super::Ghost as G;
        match g {
            None => GhostKey::None,
            Some(G::Part { part, cell, rot }) => GhostKey::Part(part.clone(), *cell, *rot),
            Some(G::Kit { kit, cell, rot }) => GhostKey::Kit(kit.clone(), *cell, *rot),
            Some(G::Paste { offset }) => GhostKey::Paste(*offset),
            Some(G::Edit(b)) => GhostKey::Edit(&**b as *const content::blueprint::Blueprint as usize),
        }
    }
}

/// What the ghost draws: part models (part index, pose, footprint size), with a colour role each, and boxes.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct GhostDraw { pub parts: Vec<(u16, content::cell::CellPos, content::kit::PartRot, [u8; 3], &'static str)>, pub boxes: Vec<(content::cell::CellPos, &'static str)> }

/// Most ghost boxes drawn (a large preview of a hull edit).
const MAX_GHOST_BOXES: usize = 40_000;

/// The ghost of a placement verdict.
pub(crate) fn ghost_of_verdict(v: &super::parts::Verdict, cat: &content::catalogue::Catalogue) -> GhostDraw {
    let mut g = GhostDraw::default();
    match (&v.next, v.valid) {
        (Some(next), true) => {
            for slot in v.new_slots.clone() {
                let Some(p) = next.parts.get(slot) else { continue };
                let (Some(idx), Some(def)) = (cat.part_idx(&p.part), cat.part(&p.part)) else { continue };
                g.parts.push((idx.0, p.pos, p.rot, def.size, "ghost"));
            }
            if g.parts.is_empty() { g.boxes.extend(v.cells.iter().map(|c| (*c, "ghost"))); }
        }
        _ => g.boxes.extend(v.cells.iter().take(MAX_GHOST_BOXES).map(|c| (*c, "warning"))),
    }
    g
}

/// The ghost of a previewed edit: what it adds and what it takes away.
pub(crate) fn ghost_of_edit(cur: &content::blueprint::Blueprint, next: &content::blueprint::Blueprint, cat: &content::catalogue::Catalogue) -> GhostDraw {
    use content::stats::PartChange;
    let mut g = GhostDraw::default();
    let diff = content::stats::diff_blueprints(cur, next, cat);
    let place = |g: &mut GhostDraw, p: &content::blueprint::PartPlacement, role: &'static str| {
        if let (Some(idx), Some(def)) = (cat.part_idx(&p.part), cat.part(&p.part)) { g.parts.push((idx.0, p.pos, p.rot, def.size, role)); }
    };
    let footprint = |g: &mut GhostDraw, p: &content::blueprint::PartPlacement| {
        if let Some(def) = cat.part(&p.part) { g.boxes.extend(super::parts::footprint_cells(def, p.pos, p.rot).into_iter().map(|c| (c, "warning"))); }
    };
    for ch in &diff.changes {
        match ch {
            PartChange::Added(p) => place(&mut g, p, "ghost_preview"),
            PartChange::Removed(p) => footprint(&mut g, p),
            PartChange::Moved { before, after } => { footprint(&mut g, before); place(&mut g, after, "ghost_preview"); }
        }
    }
    let (a, b) = (content::cell::CellGrid::from_rows(&cur.cells), content::cell::CellGrid::from_rows(&next.cells));
    let size = [0, 1, 2].map(|k| a.size[k].max(b.size[k]) as i16);
    'cells: for y in 0..size[1] { for z in 0..size[2] { for x in 0..size[0] {
        if g.boxes.len() >= MAX_GHOST_BOXES { break 'cells; }
        let c = content::cell::CellPos::new(x, y, z);
        let (was, now) = (a.get(c), b.get(c));
        if was == now { continue; }
        g.boxes.push((c, if now.is_empty() { "warning" } else { "ghost_preview" }));
    }}}
    g
}

// =========================================================================================================
// GPU side of overlays and ghost
// =========================================================================================================

const LINE_SHADER: &str = r#"shader_type spatial;
render_mode unshaded, cull_disabled, depth_draw_never, shadows_disabled, fog_disabled, blend_mix@DEPTH@;
uniform float width_px = 2.5;
varying float v_along;
varying vec2 v_dash;
void vertex() {
	vec4 a = PROJECTION_MATRIX * (MODELVIEW_MATRIX * vec4(VERTEX, 1.0));
	vec4 b = PROJECTION_MATRIX * (MODELVIEW_MATRIX * vec4(CUSTOM0.xyz, 1.0));
	vec2 sa = a.xy / max(abs(a.w), 1e-4) * VIEWPORT_SIZE;
	vec2 sb = b.xy / max(abs(b.w), 1e-4) * VIEWPORT_SIZE;
	vec2 d = sb - sa;
	float l = length(d);
	vec2 dir = l > 1e-5 ? d / l : vec2(1.0, 0.0);
	vec2 n = vec2(-dir.y, dir.x);
	a.xy += n * CUSTOM0.w * width_px / VIEWPORT_SIZE * a.w;
	POSITION = a;
	v_along = UV.x;
	v_dash = UV2;
}
void fragment() {
	if (v_dash.x > 0.0 && fract(v_along / v_dash.x) > v_dash.y) { discard; }
	ALBEDO = COLOR.rgb;
	ALPHA = COLOR.a;
}
"#;
const FILL_SHADER: &str = r#"shader_type spatial;
render_mode unshaded, cull_disabled, depth_draw_never, shadows_disabled, fog_disabled, blend_mix@DEPTH@;
void fragment() {
	ALBEDO = COLOR.rgb;
	ALPHA = COLOR.a;
}
"#;
/// Lit and translucent: a ghost reads as a shape, tinted by its role.
const GHOST_SHADER: &str = r#"shader_type spatial;
render_mode cull_back, depth_draw_opaque, shadows_disabled, blend_mix;
uniform vec4 tint : source_color = vec4(0.9, 0.9, 0.9, 0.5);
void fragment() {
	ALBEDO = tint.rgb;
	ALPHA = tint.a;
	ROUGHNESS = 0.85;
}
"#;
/// The ghost's second pass, through whatever stands in front of it: a faint flat silhouette, so an engine
/// settling into the hull still shows where it goes.
const GHOST_XRAY_SHADER: &str = r#"shader_type spatial;
render_mode unshaded, cull_back, depth_draw_never, depth_test_disabled, shadows_disabled, fog_disabled, blend_mix;
uniform vec4 tint : source_color = vec4(0.9, 0.9, 0.9, 0.25);
void fragment() {
	ALBEDO = tint.rgb;
	ALPHA = tint.a;
}
"#;

struct Shaders { line_top: Rid, line_world: Rid, fill_top: Rid, fill_world: Rid, ghost: Rid, ghost_xray: Rid }
impl Shaders {
    fn new(rs: &mut Gd<godot::classes::RenderingServer>) -> Self {
        let mut make = |code: &str| { let s = rs.shader_create(); rs.shader_set_code(s, code); s };
        Self {
            line_top: make(&LINE_SHADER.replace("@DEPTH@", ", depth_test_disabled")),
            line_world: make(&LINE_SHADER.replace("@DEPTH@", "")),
            fill_top: make(&FILL_SHADER.replace("@DEPTH@", ", depth_test_disabled")),
            fill_world: make(&FILL_SHADER.replace("@DEPTH@", "")),
            ghost: make(GHOST_SHADER),
            ghost_xray: make(GHOST_XRAY_SHADER),
        }
    }
    fn free(&mut self, rs: &mut Gd<godot::classes::RenderingServer>) {
        for s in [&mut self.line_top, &mut self.line_world, &mut self.fill_top, &mut self.fill_world, &mut self.ghost, &mut self.ghost_xray] { crate::render::hull::gpu::free_rid(rs, s); }
    }
}

/// One mesh shown by one instance with one material, replaced whole on each rebuild.
struct Layer { instance: Rid, mesh: Rid, material: Rid }
impl Layer {
    fn new(rs: &mut Gd<godot::classes::RenderingServer>, scenario: Rid, shader: Rid, priority: i32) -> Self {
        let material = rs.material_create();
        rs.material_set_shader(material, shader);
        rs.material_set_render_priority(material, priority);
        let instance = rs.instance_create();
        rs.instance_set_scenario(instance, scenario);
        rs.instance_set_layer_mask(instance, crate::render::LAYER_DOCK_OVERLAY);
        rs.instance_geometry_set_cast_shadows_setting(instance, godot::classes::rendering_server::ShadowCastingSetting::OFF);
        rs.instance_geometry_set_material_override(instance, material);
        Self { instance, mesh: Rid::Invalid, material }
    }
    fn set(&mut self, rs: &mut Gd<godot::classes::RenderingServer>, mesh: Rid) {
        rs.instance_set_base(self.instance, mesh);
        crate::render::hull::gpu::free_rid(rs, &mut self.mesh);
        self.mesh = mesh;
        rs.instance_set_visible(self.instance, mesh.is_valid());
    }
    fn free(&mut self, rs: &mut Gd<godot::classes::RenderingServer>) {
        for r in [&mut self.instance, &mut self.mesh, &mut self.material] { crate::render::hull::gpu::free_rid(rs, r); }
    }
}

fn lines_mesh(rs: &mut Gd<godot::classes::RenderingServer>, l: &geom::Lines) -> Rid {
    use godot::classes::rendering_server::{ArrayCustomFormat, ArrayFormat, ArrayType, PrimitiveType};
    use godot::obj::EngineBitfield;
    if l.is_empty() { return Rid::Invalid; }
    let mut arrays = VarArray::new();
    arrays.resize(ArrayType::MAX.ord() as usize, &Variant::nil());
    arrays.set(ArrayType::VERTEX.ord() as usize, &l.pos.iter().map(|p| Vector3::new(p[0], p[1], p[2])).collect::<PackedVector3Array>().to_variant());
    arrays.set(ArrayType::COLOR.ord() as usize, &l.color.iter().map(|c| Color::from_rgba(c[0], c[1], c[2], c[3])).collect::<PackedColorArray>().to_variant());
    arrays.set(ArrayType::TEX_UV.ord() as usize, &l.along.iter().map(|a| Vector2::new(a[0], a[1])).collect::<PackedVector2Array>().to_variant());
    arrays.set(ArrayType::TEX_UV2.ord() as usize, &l.dash.iter().map(|a| Vector2::new(a[0], a[1])).collect::<PackedVector2Array>().to_variant());
    arrays.set(ArrayType::CUSTOM0.ord() as usize, &l.other.iter().flat_map(|c| c.iter().copied()).collect::<PackedFloat32Array>().to_variant());
    arrays.set(ArrayType::INDEX.ord() as usize, &l.idx.iter().map(|&i| i as i32).collect::<PackedInt32Array>().to_variant());
    let flags = (ArrayCustomFormat::RGBA_FLOAT.ord() as u64) << ArrayFormat::CUSTOM0_SHIFT.ord();
    let rid = rs.mesh_create();
    match ArrayFormat::try_from_ord(flags) {
        Some(format) => rs.mesh_add_surface_from_arrays_ex(rid, PrimitiveType::TRIANGLES, &arrays).compress_format(format).done(),
        None => rs.mesh_add_surface_from_arrays(rid, PrimitiveType::TRIANGLES, &arrays),
    }
    // The vertices sit on the line; the shader widens them. Give the culler room for that.
    rs.mesh_set_custom_aabb(rid, bounds_of(&l.pos, 1.0));
    rid
}
fn fills_mesh(rs: &mut Gd<godot::classes::RenderingServer>, f: &geom::Fills) -> Rid {
    use godot::classes::rendering_server::{ArrayType, PrimitiveType};
    if f.is_empty() { return Rid::Invalid; }
    let mut arrays = VarArray::new();
    arrays.resize(ArrayType::MAX.ord() as usize, &Variant::nil());
    arrays.set(ArrayType::VERTEX.ord() as usize, &f.pos.iter().map(|p| Vector3::new(p[0], p[1], p[2])).collect::<PackedVector3Array>().to_variant());
    arrays.set(ArrayType::COLOR.ord() as usize, &f.color.iter().map(|c| Color::from_rgba(c[0], c[1], c[2], c[3])).collect::<PackedColorArray>().to_variant());
    arrays.set(ArrayType::INDEX.ord() as usize, &f.idx.iter().map(|&i| i as i32).collect::<PackedInt32Array>().to_variant());
    let rid = rs.mesh_create();
    rs.mesh_add_surface_from_arrays(rid, PrimitiveType::TRIANGLES, &arrays);
    rid
}
/// Unit boxes (cell-sized) as one mesh of coloured cubes.
fn boxes_mesh(rs: &mut Gd<godot::classes::RenderingServer>, boxes: &[(glam::Vec3, [f32; 4])], half: f32) -> Rid {
    let mut f = geom::Fills::default();
    for (c, col) in boxes {
        let v = |x: f32, y: f32, z: f32| *c + glam::Vec3::new(x, y, z) * half;
        let p = [v(-1., -1., -1.), v(1., -1., -1.), v(1., 1., -1.), v(-1., 1., -1.), v(-1., -1., 1.), v(1., -1., 1.), v(1., 1., 1.), v(-1., 1., 1.)];
        for q in [[0, 1, 2, 3], [5, 4, 7, 6], [4, 0, 3, 7], [1, 5, 6, 2], [3, 2, 6, 7], [4, 5, 1, 0]] { f.quad(p[q[0]], p[q[1]], p[q[2]], p[q[3]], *col); }
    }
    fills_mesh(rs, &f)
}
fn bounds_of(pos: &[[f32; 3]], pad: f32) -> Aabb {
    let mut lo = glam::Vec3::splat(f32::MAX);
    let mut hi = glam::Vec3::splat(f32::MIN);
    for p in pos { let v = glam::Vec3::from(*p); if v.is_finite() { lo = lo.min(v); hi = hi.max(v); } }
    if lo.cmpgt(hi).any() { return Aabb::new(Vector3::ZERO, Vector3::ZERO); }
    crate::render::hull::gpu::aabb(lo - pad, hi + pad)
}

/// The dock's overlay objects (the hull and part units are `BpEditorView::hull` and `parts`).
struct DockGpu {
    shaders: Shaders,
    top: Layer,
    world: Layer,
    fills: Layer,
    sheets: Layer,
    boxes: Layer,
    ghost_boxes: Layer,
    /// Ghost part instances and the materials of their roles.
    ghost_parts: Vec<Rid>,
    ghost_materials: std::collections::BTreeMap<&'static str, Rid>,
    scenario: Rid,
}
impl DockGpu {
    fn new(scenario: Rid) -> Self {
        let mut rs = crate::render::hull::gpu::rs();
        let shaders = Shaders::new(&mut rs);
        Self {
            top: Layer::new(&mut rs, scenario, shaders.line_top, 20),
            world: Layer::new(&mut rs, scenario, shaders.line_world, 10),
            fills: Layer::new(&mut rs, scenario, shaders.fill_world, 5),
            sheets: Layer::new(&mut rs, scenario, shaders.fill_world, 4),
            boxes: Layer::new(&mut rs, scenario, shaders.fill_world, 6),
            ghost_boxes: Layer::new(&mut rs, scenario, shaders.fill_top, 15),
            shaders,
            ghost_parts: Vec::new(),
            ghost_materials: Default::default(),
            scenario,
        }
    }
    fn ghost_material(&mut self, role: &'static str, pal: &Palette) -> Rid {
        let mut rs = crate::render::hull::gpu::rs();
        let (shader, xray_shader) = (self.shaders.ghost, self.shaders.ghost_xray);
        let xray_key: &'static str = match role { "ghost" => "ghost:xray", "ghost_preview" => "ghost_preview:xray", "warning" => "warning:xray", _ => "other:xray" };
        let c = pal.get(role);
        let xray = *self.ghost_materials.entry(xray_key).or_insert_with(|| {
            let m = rs.material_create();
            rs.material_set_shader(m, xray_shader);
            rs.material_set_param(m, "tint", &Color::from_rgba(c[0], c[1], c[2], 0.28).linear_to_srgb().to_variant());
            m
        });
        *self.ghost_materials.entry(role).or_insert_with(|| {
            let m = rs.material_create();
            rs.material_set_shader(m, shader);
            rs.material_set_param(m, "tint", &Color::from_rgba(c[0], c[1], c[2], 0.55).linear_to_srgb().to_variant());
            rs.material_set_next_pass(m, xray);
            m
        })
    }
    fn clear_ghost(&mut self) {
        let mut rs = crate::render::hull::gpu::rs();
        for mut i in self.ghost_parts.drain(..) { crate::render::hull::gpu::free_rid(&mut rs, &mut i); }
        self.ghost_boxes.set(&mut rs, Rid::Invalid);
    }
    fn show_ghost(&mut self, g: &GhostDraw, pal: &Palette, parts: Option<&crate::render::parts::PartRenderer>) {
        self.clear_ghost();
        let mut placed = Vec::new();
        for (part, pos, rot, size, role) in &g.parts {
            let Some(mesh) = parts.map(|p| p.part_mesh(content::ids::PartIdx(*part))) else { continue };
            if !mesh.is_valid() { continue; }
            let material = self.ghost_material(role, pal);
            let mut rs = crate::render::hull::gpu::rs();
            let inst = rs.instance_create2(mesh, self.scenario);
            rs.instance_set_layer_mask(inst, crate::render::LAYER_DOCK_OVERLAY);
            rs.instance_geometry_set_cast_shadows_setting(inst, godot::classes::rendering_server::ShadowCastingSetting::OFF);
            rs.instance_geometry_set_material_override(inst, material);
            rs.instance_set_transform(inst, crate::render::hull::gpu::transform(&crate::render::parts::placement_affine(*pos, *rot, *size)));
            placed.push(inst);
        }
        self.ghost_parts = placed;
        let boxes: Vec<(glam::Vec3, [f32; 4])> = g.boxes.iter().map(|(c, role)| { let m = c.centre_m(); (glam::Vec3::new(m[0] as f32, m[1] as f32, m[2] as f32), pal.alpha(role, 0.38)) }).collect();
        let mut rs = crate::render::hull::gpu::rs();
        let mesh = boxes_mesh(&mut rs, &boxes, 0.5 * pick::CELL * 1.02);
        self.ghost_boxes.set(&mut rs, mesh);
    }
    fn show_overlays(&mut self, o: &OverlayOut) {
        let mut rs = crate::render::hull::gpu::rs();
        let m = lines_mesh(&mut rs, &o.top); self.top.set(&mut rs, m);
        let m = lines_mesh(&mut rs, &o.world); self.world.set(&mut rs, m);
        let m = fills_mesh(&mut rs, &o.fills); self.fills.set(&mut rs, m);
        let m = fills_mesh(&mut rs, &o.sheets); self.sheets.set(&mut rs, m);
        let m = boxes_mesh(&mut rs, &o.boxes, 0.5 * pick::CELL * 1.04); self.boxes.set(&mut rs, m);
    }
    fn free(&mut self) {
        self.clear_ghost();
        let mut rs = crate::render::hull::gpu::rs();
        for l in [&mut self.top, &mut self.world, &mut self.fills, &mut self.sheets, &mut self.boxes, &mut self.ghost_boxes] { l.free(&mut rs); }
        for (_, mut m) in std::mem::take(&mut self.ghost_materials) { crate::render::hull::gpu::free_rid(&mut rs, &mut m); }
        self.shaders.free(&mut rs);
    }
}

// =========================================================================================================
// The node
// =========================================================================================================

/// What the overlays were last built from: design, bits, focus, cut row, slow data present and its address.
type OverlayKey = (usize, i64, Vec<u16>, Option<i16>, bool, u64, Vec<[i16; 6]>);
/// A cached pick scene: revision, cut row, scene.
type SceneEntry = (u64, Option<i16>, std::rc::Rc<pick::PickScene>);

/// What the view last drew, so it redraws only on change.
#[derive(Default)]
pub(crate) struct Drawn {
    design: Option<Arc<sim_systems::ship::ShipDesign>>,
    xray: bool,
    cut_row: Option<i16>,
    /// First revision of the design shown (a new design is framed once and announced by `design_shown`).
    design_id: u64,
    announced: bool,
    /// The view size the shown design was last framed for by itself; `None` once the player moves the camera.
    /// The dock lays out after the design loads, so a frame taken at the old size is taken again at the new one.
    auto_framed: Option<[f32; 2]>,
    overlay_key: Option<OverlayKey>,
    ghost_key: Option<(u64, GhostKey, crate::enums::Symmetry, u8, u64)>,
    ghost_kind: Option<GhostKind>,
}

/// Everything `BpEditorView` keeps besides its frozen fields.
#[derive(Default)]
pub(crate) struct ViewState {
    gpu: Option<DockGpu>,
    drawn: Drawn,
    cam: cam::DockCam,
    gestures: cam::Gestures,
    focus: Vec<u16>,
    deck_slice: i64,
    palette: Option<(bool, Palette)>,
    /// The slow overlay data of the shown design, and the job computing it.
    slow: Option<Arc<SlowData>>,
    slow_job: Option<(content::ids::ContentHash, std::sync::mpsc::Receiver<SlowData>)>,
    /// Pick scene by (revision, cut row).
    scene: std::cell::RefCell<Option<SceneEntry>>,
    labels: Vec<Gd<godot::classes::Label3D>>,
    label_font: Option<Option<Gd<godot::classes::Font>>>,
    view_frame: crate::interp::ViewFrame,
    /// The off-screen viewport and camera thumbnails render through. It is made with the render units and
    /// lives with them: a viewport drawn the frame it is created comes out empty.
    thumb: Option<(Gd<godot::classes::SubViewport>, Gd<Camera3D>)>,
    canvas: [f32; 2],
    pixels: [f32; 2],
}

/// The dry dock's 3D view. Scene node.
#[derive(GodotClass)]
#[class(base = Node3D)]
pub struct BpEditorView {
    pub(crate) camera: Option<Gd<Camera3D>>,
    pub(crate) overlays: i64,
    pub(crate) seen_rev: u64,
    pub(crate) pivot: Vector3,
    pub(crate) yaw_deg: f32,
    pub(crate) pitch_deg: f32,
    pub(crate) distance_m: f32,
    pub(crate) hull: Option<crate::render::hull::HullRenderer>,
    pub(crate) parts: Option<crate::render::parts::PartRenderer>,
    /// Render units, caches and camera state (`ViewState`).
    pub(crate) st: Box<ViewState>,
    base: Base<Node3D>,
}

#[godot_api]
impl INode3D for BpEditorView {
    fn init(base: Base<Node3D>) -> Self {
        let st = Box::new(ViewState { deck_slice: -1, ..Default::default() });
        Self { camera: None, overlays: 0, seen_rev: 0, pivot: Vector3::ZERO, yaw_deg: 30.0, pitch_deg: 20.0, distance_m: 60.0, hull: None, parts: None, st, base }
    }
    /// Unit `gd-editor-view`: when the design revision, the ghost or the overlays changed, rebuild what is
    /// drawn (meshing on the meshing thread; uploads within the frame budget). Inert while the dock is closed.
    fn process(&mut self, delta: f64) { self.draw_frame(delta); }
    fn exit_tree(&mut self) { self.sleep(); }
}

/// Main-thread microseconds the hull unit may spend on uploads and frees per frame (L2008).
const UPLOAD_BUDGET_US: u64 = 300;
/// A handle is picked within this many canvas units.
const HANDLE_PICK_PX: f32 = 12.0;
/// Label font (the interface face's MSDF build, as hull numbers use).
const LABEL_FONT: &str = "res://ui/fonts/AtkinsonHyperlegibleNext-Variable.msdf.res";
/// Label text size in pixels (Dense type, 18 px body).
const LABEL_PX: i32 = 20;

/// What the node copies out of the editor model each frame.
struct EditorSnap {
    rev: u64,
    first_rev: u64,
    design: Option<Arc<sim_systems::ship::ShipDesign>>,
    blueprint: Option<Arc<content::blueprint::Blueprint>>,
    ghost: GhostKey,
    symmetry: (crate::enums::Symmetry, u8),
    waterline_row: i16,
    dragging: bool,
}

impl BpEditorView {
    fn editor(&self) -> Option<Gd<BpEditor>> {
        if !self.base().is_inside_tree() { return None; }
        let tree = self.base().get_tree();
        let root = tree.get_root();
        root.get_node_or_null("Editor").and_then(|n| n.try_cast::<BpEditor>().ok())
    }

    fn snap(&self) -> Option<EditorSnap> {
        let ed = self.editor()?;
        let e = ed.bind();
        let m = e.model.as_ref()?;
        let blueprint = e.state.current.as_ref().filter(|(r, _)| *r == m.rev).map(|(_, b)| b.clone()).or_else(|| Some(Arc::new(m.blueprint.clone())));
        Some(EditorSnap {
            rev: m.rev,
            first_rev: e.state.first_rev,
            design: e.latest_design(),
            blueprint,
            ghost: GhostKey::of(m.ghost.as_ref()),
            symmetry: (m.symmetry, m.radial_count),
            waterline_row: m.blueprint.hull.design_waterline_y,
            dragging: e.state.dragging || m.group.is_some(),
        })
    }

    /// Free the render units and overlay objects; the camera is kept.
    fn sleep(&mut self) {
        if let Some(mut gpu) = self.st.gpu.take() { gpu.free(); }
        if let Some(mut p) = self.parts.take() { p.free(); }
        if let Some(mut h) = self.hull.take() { h.free(); }
        if let Some((mut vp, _)) = self.st.thumb.take() { if vp.is_instance_valid() { vp.queue_free(); } }
        for mut l in std::mem::take(&mut self.st.labels) { if l.is_instance_valid() { l.queue_free(); } }
        self.st.drawn = Drawn::default();
        self.st.slow = None;
        self.st.slow_job = None;
    }

    /// The cut row of the current deck slice.
    fn cut_row(&self, bp: &content::blueprint::Blueprint) -> Option<i16> {
        let i = usize::try_from(self.st.deck_slice).ok()?;
        let mut decks: Vec<i16> = bp.hull.planes.iter().filter(|p| p.axis == content::enums::Axis::Y).map(|p| p.offset).collect();
        decks.sort_unstable();
        let row = *decks.get(i)?;
        Some(match decks.get(i + 1) { Some(next) => next - 1, None => row + (content::hull::DEFAULT_DECK_SPACING_M / content::cell::CELL_M).round() as i16 - 1 })
    }

    /// The pick scene of the current revision and slice.
    fn scene(&self) -> Option<std::rc::Rc<pick::PickScene>> {
        let ed = self.editor()?;
        let e = ed.bind();
        let m = e.model.as_ref()?;
        let cut = self.cut_row(&m.blueprint);
        if let Some((r, c, s)) = self.st.scene.borrow().as_ref() {
            if *r == m.rev && *c == cut { return Some(s.clone()); }
        }
        let s = std::rc::Rc::new(pick::PickScene::build(&m.blueprint, &crate::services::services().catalogue(), cut));
        *self.st.scene.borrow_mut() = Some((m.rev, cut, s.clone()));
        Some(s)
    }

    fn sync_fields(&mut self) {
        let c = self.st.cam;
        self.pivot = Vector3::new(c.target.x, c.target.y, c.target.z);
        self.yaw_deg = c.yaw_deg;
        self.pitch_deg = c.pitch_deg;
        self.distance_m = c.distance_m;
    }
    /// Take values a script may have written into the exported fields.
    fn cam_from_fields(&mut self) {
        let c = &mut self.st.cam;
        c.target = glam::Vec3::new(self.pivot.x, self.pivot.y, self.pivot.z);
        c.yaw_deg = self.yaw_deg;
        c.pitch_deg = self.pitch_deg;
        c.distance_m = self.distance_m;
        c.sanitize();
    }

    fn viewport_sizes(&self) -> ([f32; 2], [f32; 2]) {
        let fallback = ([1920.0, 1080.0], [1920.0, 1080.0]);
        if !self.base().is_inside_tree() { return fallback; }
        let Some(vp) = self.base().get_viewport() else { return fallback };
        let r = vp.get_visible_rect().size;
        let canvas = if r.x >= 1.0 && r.y >= 1.0 { [r.x, r.y] } else { fallback.0 };
        let px = vp.get_texture().map(|t| t.get_size()).filter(|s| s.x >= 1.0 && s.y >= 1.0).map_or(canvas, |s| [s.x, s.y]);
        (canvas, px)
    }

    fn now_s() -> f64 { godot::classes::Time::singleton().get_ticks_usec() as f64 / 1.0e6 }

    fn trackpad() -> cam::Trackpad {
        let services = crate::services::services();
        let Ok(s) = services.settings.try_borrow() else { return cam::Trackpad::default() };
        cam::Trackpad {
            drag_pans: s.int("editor.two_finger_drag") == 1,
            invert_orbit: s.bool("editor.invert_orbit"), invert_pan: s.bool("editor.invert_pan"), invert_zoom: s.bool("editor.invert_zoom"),
            orbit_speed: s.float("editor.orbit_speed") as f32, pan_speed: s.float("editor.pan_speed") as f32, zoom_speed: s.float("editor.zoom_speed") as f32,
        }
    }

    /// Ray through a viewport position: origin and unit direction (editor frame = render space here).
    fn ray(&self, screen: Vector2) -> Option<(glam::Vec3, glam::Vec3)> {
        if !(screen.x.is_finite() && screen.y.is_finite()) { return None; }
        let lens = self.st.cam.lens();
        Some((lens.pos, lens.ray([screen.x, screen.y])))
    }

    /// The point under the cursor on the design (cells, then parts), if any.
    fn hit_point(&self, screen: Vector2) -> Option<glam::Vec3> {
        let (o, d) = self.ray(screen)?;
        let s = self.scene()?;
        let a = s.ray_cells(o, d).map(|h| h.2);
        let b = s.ray_parts(o, d).map(|h| h.3);
        let t = match (a, b) { (Some(a), Some(b)) => a.min(b), (a, b) => a.or(b)? };
        Some(o + d * t)
    }

    fn selection_centre(&self) -> Option<glam::Vec3> {
        let ed = self.editor()?;
        let e = ed.bind();
        let (n, c, _, _) = e.selection_summary();
        (n > 0).then(|| glam::Vec3::new(c.x, c.y, c.z))
    }

    fn handle_list(&self) -> Vec<super::hull::HandleInfo> { self.editor().map(|ed| ed.bind().handle_list()).unwrap_or_default() }

    /// Screen position, visibility and distance of a handle.
    fn handle_screen(&self, pos: [f64; 3], scene: Option<&pick::PickScene>) -> (Vector2, bool) {
        let p = glam::Vec3::new(pos[0] as f32, pos[1] as f32, pos[2] as f32);
        let lens = self.st.cam.lens();
        let Some(s) = lens.project(p) else { return (Vector2::ZERO, false) };
        let inside = s[0] >= 0.0 && s[1] >= 0.0 && s[0] <= lens.canvas[0] && s[1] <= lens.canvas[1];
        // Hidden behind the design: something shown lies on the ray well before the handle.
        let dist = (p - lens.pos).length();
        let dir = (p - lens.pos).normalize_or_zero();
        let blocked = scene.is_some_and(|sc| sc.ray_cells(lens.pos, dir).is_some_and(|h| h.2 < dist - 0.75) || sc.ray_parts(lens.pos, dir).is_some_and(|h| h.3 < dist - 0.75));
        (Vector2::new(s[0], s[1]), inside && !blocked)
    }

    /// One frame (see `process`).
    fn draw_frame(&mut self, delta: f64) {
        if !self.base().is_inside_tree() { return; }
        let state = crate::services::services().feed.try_borrow().map_or(SessionState::Idle, |f| f.state);
        // While a session loads or runs, the sortie's world view owns the 3D and the camera.
        if matches!(state, SessionState::Loading | SessionState::InSortie | SessionState::Ended | SessionState::Replay) { self.sleep(); return; }
        let Some(snap) = self.snap() else { self.sleep(); return };
        let (canvas, pixels) = self.viewport_sizes();
        self.st.canvas = canvas;
        self.st.pixels = pixels;
        self.cam_from_fields();
        self.st.cam.canvas = canvas;
        if self.st.gpu.is_none() {
            let Some(world) = self.base().get_world_3d() else { return };
            let scenario = world.get_scenario();
            if !scenario.is_valid() { return; }
            // Before any shader loads (the start-up hook has normally done it already).
            crate::render::ocean::gpu::ensure_globals();
            self.hull = Some(crate::render::hull::HullRenderer::new(scenario));
            self.parts = Some(crate::render::parts::PartRenderer::new(scenario, &crate::services::services().catalogue()));
            self.st.gpu = Some(DockGpu::new(scenario));
            self.st.drawn = Drawn::default();
            self.make_thumb_viewport(&world);
        }
        let hc = crate::services::services().settings.try_borrow().is_ok_and(|s| s.int("access.palette") == 1);
        if self.st.palette.as_ref().is_none_or(|(h, _)| *h != hc) { self.st.palette = Some((hc, load_palette(hc))); self.st.drawn.overlay_key = None; self.st.drawn.ghost_key = None; }

        // 1. The design: the worker's newest build, drawn with its own blueprint.
        let xray = self.overlays & bits::XRAY != 0;
        if let Some(design) = snap.design.as_ref() {
            let bp = design.blueprint();
            let cut = self.cut_row(bp);
            let same = self.st.drawn.design.as_ref().is_some_and(|d| Arc::ptr_eq(d, design)) && self.st.drawn.xray == xray && self.st.drawn.cut_row == cut;
            if !same {
                let cut_m = cut.map(|r| (content::enums::Axis::Y, (r as f32 + 1.0) * pick::CELL));
                if let Some(h) = self.hull.as_mut() { h.show_design(design, bp, xray, cut_m); }
                if let Some(p) = self.parts.as_mut() {
                    let cat = crate::services::services().catalogue();
                    match cut {
                        Some(r) => {
                            let mut shown = bp.clone();
                            shown.parts.retain(|p| p.pos.y <= r);
                            p.show_design(&shown, &cat);
                        }
                        None => p.show_design(bp, &cat),
                    }
                }
                self.st.drawn.design = Some(design.clone());
                self.st.drawn.xray = xray;
                self.st.drawn.cut_row = cut;
            }
        }
        self.slow_jobs(snap.dragging);

        // 2. A newly loaded design is framed once, and framed again when the view changes size before the
        // player has moved the camera (the dock gives the view its size after the design has loaded).
        let new_design = snap.first_rev != self.st.drawn.design_id && snap.design.is_some();
        let resized = self.st.drawn.auto_framed.is_some_and(|c| (c[0] - canvas[0]).abs() > 1.0 || (c[1] - canvas[1]).abs() > 1.0);
        if new_design || resized {
            if let Some(scene) = self.scene() {
                if let Some((lo, hi)) = scene.bounds() { self.st.cam.fit_points(lo, hi, &scene.outline()); }
                self.st.drawn.auto_framed = Some(canvas);
                if new_design {
                    self.st.drawn.design_id = snap.first_rev;
                    self.st.drawn.announced = false;
                }
            }
        }

        // 3. The render units, then the camera.
        let lens = self.st.cam.lens();
        let state_cam = crate::interp::CameraState { pos: [lens.pos.x as f64, lens.pos.y as f64, lens.pos.z as f64], forward: lens.forward.into(), up: lens.up.into(), fov_y_deg: cam::FOV_Y_DEG, viewport_px: pixels, valid: true };
        if let Some(c) = self.camera.as_mut().filter(|c| c.is_instance_valid()) {
            let v = |x: glam::Vec3| Vector3::new(x.x, x.y, x.z);
            let xf = Transform3D::new(Basis::from_cols(v(lens.right), v(lens.up), v(-lens.forward)), v(lens.pos));
            if c.is_inside_tree() { c.set_global_transform(xf); } else { c.set_transform(xf); }
            c.set_fov(cam::FOV_Y_DEG);
            c.set_near(0.1);
        }
        let services = crate::services::services();
        services.camera.set(state_cam);
        let quality = services.quality.try_borrow().map(|q| q.clone()).unwrap_or_default();
        drop(services);
        if let (Some(gpu), Some(hull), Some(parts)) = (self.st.gpu.as_ref(), self.hull.as_mut(), self.parts.as_mut()) {
            let ctx = crate::render::FrameContext { view: &self.st.view_frame, events: &[], camera: state_cam, delta_s: delta, scenario: gpu.scenario, quality: &quality, degrade_step: 0, reduce_flashing: false };
            let mut budget = crate::render::UploadBudget::new(UPLOAD_BUDGET_US);
            hull.update(&ctx, &mut budget);
            parts.update(&ctx);
        }

        // 4. Overlays and the ghost.
        self.update_overlays(&snap);
        self.update_ghost(&snap);
        self.sync_fields();
        self.seen_rev = snap.rev;

        // 5. The first frame of a new design on screen.
        if !self.st.drawn.announced && self.st.drawn.design_id == snap.first_rev {
            if let Some(h) = self.hull.as_ref().map(|h| h.stats()) {
                if h.chunks_drawn > 0 && h.chunk_uploads_pending == 0 {
                    self.st.drawn.announced = true;
                    self.base_mut().emit_signal("design_shown", &[(snap.rev as i64).to_variant()]);
                }
            }
        }
    }

    /// The slow overlay data: take a finished job, then start one for what the overlays on now still lack.
    /// One job at a time, only for the overlays shown, none while a handle is dragged (the gauge worker
    /// needs the cores then).
    fn slow_jobs(&mut self, dragging: bool) {
        if let Some((_, rx)) = self.st.slow_job.as_ref() {
            match rx.try_recv() {
                Ok(data) => {
                    match self.st.slow.as_mut().map(Arc::make_mut) {
                        Some(cur) if cur.hash == data.hash => cur.merge(data),
                        _ => self.st.slow = Some(Arc::new(data)),
                    }
                    self.st.drawn.overlay_key = None;
                    self.st.slow_job = None;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => return,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => self.st.slow_job = None,
            }
        }
        let Some(design) = self.st.drawn.design.clone() else { return };
        if dragging { return; }
        let have = self.st.slow.as_ref().filter(|s| s.hash == design.hash).map_or(0, |s| s.have);
        let want = slow_need(self.overlays) & !have;
        if want == 0 { return; }
        let (tx, rx) = std::sync::mpsc::channel();
        let (d, cat) = (design.clone(), crate::services::services().catalogue());
        let spawned = std::thread::Builder::new().name("bp-dock-overlay".into()).spawn(move || { let _ = tx.send(slow_data(&d, &cat, want)); });
        if spawned.is_ok() { self.st.slow_job = Some((design.hash, rx)); }
    }

    fn update_overlays(&mut self, snap: &EditorSnap) {
        let Some(design) = self.st.drawn.design.clone() else { return };
        let Some(scene) = self.scene() else { return };
        let slow_rev = self.st.slow.as_ref().map_or(0, |s| Arc::as_ptr(s) as usize as u64);
        let selected = self.selection_boxes();
        let key = (Arc::as_ptr(&design) as usize, self.overlays, self.st.focus.clone(), scene.cut_row, self.st.slow.is_some(), slow_rev, selected.clone());
        if self.st.drawn.overlay_key.as_ref() == Some(&key) { return; }
        // Mid-drag the overlays drawn stay as they are (a rebuild costs milliseconds of the frame); the design
        // the drag ends on is drawn.
        if snap.dragging && self.st.drawn.overlay_key.as_ref().is_some_and(|k| k.1 == self.overlays && k.3 == scene.cut_row) { return; }
        self.st.drawn.overlay_key = Some(key);
        let Some((_, pal)) = self.st.palette.as_ref() else { return };
        let slow = self.st.slow.clone();
        let input = OverlayIn { bits: self.overlays, focus: &self.st.focus, design: &design, slow: slow.as_deref(), scene: &scene, design_waterline_row: snap.waterline_row };
        let mut out = build_overlays(&input, pal);
        selection_brackets(&mut out.top, &selected, pal);
        if let Some(gpu) = self.st.gpu.as_mut() { gpu.show_overlays(&out); }
        self.place_labels(&out.labels);
    }

    fn place_labels(&mut self, specs: &[LabelSpec]) {
        for mut l in std::mem::take(&mut self.st.labels) { if l.is_instance_valid() { l.queue_free(); } }
        if specs.is_empty() { return; }
        let font = self.st.label_font.get_or_insert_with(|| {
            if !godot::classes::ResourceLoader::singleton().exists(LABEL_FONT) { return None; }
            godot::tools::try_load::<godot::classes::Font>(LABEL_FONT).ok()
        }).clone();
        let pal = self.st.palette.as_ref().map(|(_, p)| p.clone()).unwrap_or_default();
        let mut made = Vec::with_capacity(specs.len());
        for (pos, key, role) in specs {
            use godot::classes::label_3d::DrawFlags;
            let mut l = godot::classes::Label3D::new_alloc();
            l.set_text(&self.base().tr(*key));
            if let Some(f) = &font { l.set_font(f); }
            l.set_font_size(LABEL_PX);
            l.set_pixel_size(0.0012);
            l.set_billboard_mode(godot::classes::base_material_3d::BillboardMode::ENABLED);
            l.set_draw_flag(DrawFlags::FIXED_SIZE, true);
            l.set_draw_flag(DrawFlags::DISABLE_DEPTH_TEST, true);
            l.set_draw_flag(DrawFlags::SHADED, false);
            l.set_modulate(pal.label(role));
            l.set_outline_size(4);
            l.set_outline_modulate(pal.srgb.get("inset").copied().unwrap_or(Color::BLACK));
            l.set_render_priority(30);
            l.set_outline_render_priority(29);
            l.set_layer_mask(crate::render::LAYER_DOCK_OVERLAY);
            l.set_cast_shadows_setting(godot::classes::geometry_instance_3d::ShadowCastingSetting::OFF);
            l.set_position(Vector3::new(pos.x, pos.y, pos.z));
            self.base_mut().add_child(&l);
            made.push(l);
        }
        self.st.labels = made;
    }

    fn update_ghost(&mut self, snap: &EditorSnap) {
        let paste_rot = self.editor().map_or(0, |e| { let r = e.bind().state.parts.paste_rot; r.yaw as u64 * 8 + r.tilt as u64 });
        let key = (snap.rev, snap.ghost.clone(), snap.symmetry.0, snap.symmetry.1, paste_rot);
        if self.st.drawn.ghost_key.as_ref() == Some(&key) { return; }
        let Some(ed) = self.editor() else { return };
        let cat = crate::services::services().catalogue();
        let (draw, kind) = {
            let e = ed.bind();
            match (e.model.as_ref().and_then(|m| m.ghost.as_ref()), e.model.as_ref()) {
                (Some(super::Ghost::Edit(next)), Some(m)) => (Some(ghost_of_edit(&m.blueprint, next, &cat)), Some(GhostKind::Edit)),
                (Some(_), Some(_)) => (e.ghost_verdict().map(|(v, _, _)| ghost_of_verdict(&v, &cat)), Some(GhostKind::Place)),
                _ => (None, None),
            }
        };
        self.st.drawn.ghost_key = Some(key);
        self.st.drawn.ghost_kind = kind;
        let pal = self.st.palette.as_ref().map(|(_, p)| p.clone()).unwrap_or_default();
        if let Some(gpu) = self.st.gpu.as_mut() {
            match draw { Some(g) => gpu.show_ghost(&g, &pal, self.parts.as_ref()), None => gpu.clear_ghost() }
        }
    }

    /// Cell boxes (min corner, max corner inclusive) of the selection: one per selected kit, one per loose part.
    fn selection_boxes(&self) -> Vec<[i16; 6]> {
        let Some(ed) = self.editor() else { return Vec::new() };
        let e = ed.bind();
        let Some(m) = e.model.as_ref() else { return Vec::new() };
        let cat = crate::services::services().catalogue();
        let mut groups: std::collections::BTreeMap<(bool, usize), [i16; 6]> = Default::default();
        for s in super::selected_slots(&m.blueprint, &m.selection) {
            let p = &m.blueprint.parts[s.index()];
            let Some(def) = cat.part(&p.part) else { continue };
            let key = p.kit.map_or((false, s.index()), |k| (true, k.index()));
            for c in super::parts::footprint_cells(def, p.pos, p.rot) {
                let b = groups.entry(key).or_insert([c.x, c.y, c.z, c.x, c.y, c.z]);
                *b = [b[0].min(c.x), b[1].min(c.y), b[2].min(c.z), b[3].max(c.x), b[4].max(c.y), b[5].max(c.z)];
            }
        }
        groups.into_values().collect()
    }

    /// The box `frame` fits for a target.
    fn frame_box(&self, what: &str) -> Option<(glam::Vec3, glam::Vec3)> {
        let scene = self.scene()?;
        let all = scene.bounds()?;
        if what != "selection" { return Some(all); }
        let ed = self.editor()?;
        let e = ed.bind();
        let m = e.model.as_ref()?;
        let cat = crate::services::services().catalogue();
        let mut cells: Vec<content::cell::CellPos> = m.selection.cells.clone();
        for s in super::selected_slots(&m.blueprint, &m.selection) {
            let p = &m.blueprint.parts[s.index()];
            if let Some(def) = cat.part(&p.part) { cells.extend(super::parts::footprint_cells(def, p.pos, p.rot)); }
        }
        if cells.is_empty() { return Some(all); }
        let mut lo = glam::Vec3::splat(f32::MAX);
        let mut hi = glam::Vec3::splat(f32::MIN);
        for c in cells { let m = glam::Vec3::new(c.x as f32, c.y as f32, c.z as f32) * pick::CELL; lo = lo.min(m); hi = hi.max(m + pick::CELL); }
        Some((lo, hi))
    }
}

#[godot_api]
impl BpEditorView {
    /// The first frame of a newly loaded design is on screen.
    #[signal]
    fn design_shown(rev: i64);

    /// Give the view the camera it moves. Call once from the scene script's `_ready`.
    #[func]
    fn bind_camera(&mut self, camera: Gd<Camera3D>) { self.camera = Some(camera); }

    // ---- Camera (L235-246) ----

    /// Orbit by a screen delta in pixels around the hull point under `pivot_screen` at gesture start, or the
    /// selection centre when that misses the hull.
    #[func]
    fn orbit(&mut self, delta: Vector2, pivot_screen: Vector2) {
        self.cam_from_fields();
        self.st.drawn.auto_framed = None;
        let now = Self::now_s();
        let fallback = self.selection_centre().unwrap_or(self.st.cam.target);
        let hit = self.hit_point(pivot_screen);
        let mut pick = || hit.unwrap_or(fallback);
        let p = self.st.gestures.event(cam::Kind::Orbit, now, [pivot_screen.x, pivot_screen.y], [delta.x, delta.y], &mut pick);
        self.st.cam.orbit_about(p, -delta.x * cam::ORBIT_DEG_PER_PX, delta.y * cam::ORBIT_DEG_PER_PX);
        self.sync_fields();
    }
    /// Pan by a screen delta in pixels.
    #[func]
    fn pan(&mut self, delta: Vector2) {
        self.cam_from_fields();
        self.st.drawn.auto_framed = None;
        let t = self.st.cam.target;
        self.st.gestures.event(cam::Kind::Pan, Self::now_s(), [f32::NAN; 2], [delta.x, delta.y], &mut || t);
        self.st.cam.pan_px(delta.x, delta.y);
        self.sync_fields();
    }
    /// Zoom by a factor (above 1 = closer) toward the point under `toward_screen`.
    #[func]
    fn zoom(&mut self, factor: f64, toward_screen: Vector2) {
        self.cam_from_fields();
        self.st.drawn.auto_framed = None;
        let t = self.st.cam.target;
        self.st.gestures.event(cam::Kind::Zoom, Self::now_s(), [toward_screen.x, toward_screen.y], [0.0; 2], &mut || t);
        // The point under the cursor on the design, else at the look-at depth.
        let q = self.hit_point(toward_screen).or_else(|| self.ray(toward_screen).map(|(o, d)| o + d * self.st.cam.distance_m));
        self.st.cam.zoom_about(q, factor as f32);
        self.sync_fields();
    }
    /// A macOS two-finger pan gesture with its modifier state; applies the mapping and the user's
    /// "Two-finger drag: Orbit / Pan" switch, invert toggles and speed sliders.
    #[func]
    fn pan_gesture(&mut self, delta: Vector2, shift: bool, command: bool, at_screen: Vector2) {
        match cam::map_pan_gesture([delta.x, delta.y], shift, command, &Self::trackpad()) {
            cam::Move::Orbit { dx, dy } => self.orbit(Vector2::new(dx, dy), at_screen),
            cam::Move::Pan { dx, dy } => self.pan(Vector2::new(dx, dy)),
            cam::Move::Zoom { factor } => self.zoom(factor as f64, at_screen),
            cam::Move::None => {}
        }
    }
    /// A pinch gesture: `factor` as Godot reports it.
    #[func]
    fn magnify_gesture(&mut self, factor: f64, at_screen: Vector2) {
        if let cam::Move::Zoom { factor } = cam::map_magnify(factor, &Self::trackpad()) { self.zoom(factor as f64, at_screen); }
    }
    /// Frame something: `&"all"`, `&"selection"`, `&"bow"`, `&"stern"`, `&"port"`, `&"starboard"`, `&"top"`,
    /// `&"below"` (from below the waterline).
    #[func]
    fn frame(&mut self, what: StringName) {
        self.cam_from_fields();
        self.st.drawn.auto_framed = None;
        let what = what.to_string();
        let (yaw, pitch) = match what.as_str() {
            "bow" => (Some(0.0), Some(8.0)),
            "stern" => (Some(180.0), Some(8.0)),
            "port" => (Some(270.0), Some(8.0)),
            "starboard" => (Some(90.0), Some(8.0)),
            "top" => (Some(90.0), Some(cam::PITCH_MAX_DEG)),
            "below" => (None, Some(-30.0)),
            "all" | "selection" => (None, None),
            _ => return,
        };
        if let Some(y) = yaw { self.st.cam.yaw_deg = y; }
        if let Some(p) = pitch { self.st.cam.pitch_deg = p; }
        if let Some((lo, hi)) = self.frame_box(&what) {
            match self.scene().filter(|_| what != "selection") {
                Some(scene) => self.st.cam.fit_points(lo, hi, &scene.outline()),
                None => self.st.cam.fit(lo, hi),
            }
            if what == "below" {
                // Look up at the hull from under the waterline, aimed at the waterline.
                let wl = self.editor().and_then(|e| e.bind().model.as_ref().map(|m| m.blueprint.hull.design_waterline_y)).unwrap_or(0) as f32 * pick::CELL;
                self.st.cam.target.y = wl.clamp(lo.y, hi.y);
            }
        }
        self.st.gestures.reset();
        self.sync_fields();
    }
    /// Camera use counters for the trackpad gate log (L250): `{ "resets": int, "reversals": int }`
    /// (an orbit followed by the opposite orbit within 1 s).
    #[func]
    fn camera_log(&self) -> VarDictionary {
        d! { "resets" => self.st.gestures.resets as i64, "reversals" => self.st.gestures.reversals as i64 }
    }

    // ---- Picking ----

    /// What is under a viewport position. Returns `{ "kind": StringName, "cell": Vector3i, "face": int,
    /// "slot": int, "kit": int, "handle": String, "station": int, "plane": int, "pos": Vector3 }`; `kind` is
    /// `&"none"`, `&"cell"`, `&"part"`, `&"handle"`, `&"deck"` or `&"bulkhead"`.
    #[func]
    fn pick(&self, screen: Vector2) -> VarDictionary {
        let out = |kind: &str, cell: Option<content::cell::CellPos>, face: i64, slot: i64, kit: i64, handle: &str, station: i64, plane: i64, pos: glam::Vec3| {
            let c = cell.map_or(Vector3i::new(-1, -1, -1), |c| Vector3i::new(c.x as i32, c.y as i32, c.z as i32));
            d! { "kind" => StringName::from(kind), "cell" => c, "face" => face, "slot" => slot, "kit" => kit, "handle" => GString::from(handle), "station" => station, "plane" => plane, "pos" => Vector3::new(pos.x, pos.y, pos.z) }
        };
        let none = || out("none", None, -1, -1, -1, "", -1, -1, glam::Vec3::ZERO);
        let Some((o, d)) = self.ray(screen) else { return none() };
        let scene = self.scene();
        // Handles first: they are gizmos drawn over everything.
        let mut best: Option<(f32, super::hull::HandleInfo, Vector2)> = None;
        for h in self.handle_list() {
            let (s, visible) = self.handle_screen(h.pos, scene.as_deref());
            if !visible { continue; }
            let dist = s.distance_to(screen);
            if dist <= HANDLE_PICK_PX && best.as_ref().is_none_or(|b| dist < b.0) { best = Some((dist, h, s)); }
        }
        if let Some((_, h, _)) = best {
            let p = glam::Vec3::new(h.pos[0] as f32, h.pos[1] as f32, h.pos[2] as f32);
            return out("handle", None, -1, -1, -1, h.id, h.station, -1, p);
        }
        let Some(scene) = scene else { return none() };
        let cell = scene.ray_cells(o, d);
        let part = scene.ray_parts(o, d);
        let planes_on = self.overlays & (bits::COMPARTMENTS | bits::XRAY) != 0;
        let plane = if planes_on { scene.ray_planes(o, d) } else { None };
        let part_first = match (part, cell) { (Some(p), Some(c)) => p.3 <= c.2 + 1e-3, (Some(_), None) => true, _ => false };
        if let Some((i, c, face, t)) = part.filter(|_| part_first) {
            let b = &scene.parts[i];
            return out("part", Some(c), face as i64, b.slot as i64, b.kit.map_or(-1, |k| k as i64), "", -1, -1, o + d * t);
        }
        if let Some((p, t)) = plane {
            let kind = if p.axis == content::enums::Axis::Y { "deck" } else { "bulkhead" };
            let q = o + d * t;
            let c = content::cell::CellPos::new((q.x / pick::CELL).floor() as i16, (q.y / pick::CELL).floor() as i16, (q.z / pick::CELL).floor() as i16);
            return out(kind, Some(c), -1, -1, -1, "", -1, p.index as i64, q);
        }
        if let Some((c, face, t)) = cell { return out("cell", Some(c), face as i64, -1, -1, "", -1, -1, o + d * t); }
        none()
    }
    /// The cell a placement at this viewport position would occupy: the empty cell against the surface under
    /// the cursor, or on the active deck plane. `null` when the ray misses.
    #[func]
    fn cell_at(&self, screen: Vector2) -> Variant {
        let Some((o, d)) = self.ray(screen) else { return Variant::nil() };
        let Some(scene) = self.scene() else { return Variant::nil() };
        let cell = scene.ray_cells(o, d);
        let part = scene.ray_parts(o, d).map(|(_, c, f, t)| (c, f, t));
        let hit = match (cell, part) { (Some(a), Some(b)) => Some(if b.2 < a.2 { b } else { a }), (a, b) => a.or(b) };
        if let Some((c, face, _)) = hit {
            // Out through the face the ray came in by: the empty neighbour.
            let (dx, dy, dz) = content::cell::Face::ALL[face.min(5) as usize].offset();
            let n = content::cell::CellPos::new(c.x + dx, c.y + dy, c.z + dz);
            if (0..content::cell::MAX_GRID_DIM as i16).contains(&n.x) && (0..content::cell::MAX_GRID_DIM as i16).contains(&n.y) && (0..content::cell::MAX_GRID_DIM as i16).contains(&n.z) {
                return Vector3i::new(n.x as i32, n.y as i32, n.z as i32).to_variant();
            }
            return Variant::nil();
        }
        // On the sliced deck: the row standing on it.
        if let (Some(cut), true) = (scene.cut_row, d.y.abs() > 1e-6) {
            let deck = scene.planes.iter().filter(|p| p.axis == content::enums::Axis::Y && p.row <= cut).map(|p| p.row).max();
            if let Some(row) = deck {
                let y = (row as f32 + 1.0) * pick::CELL;
                let t = (y - o.y) / d.y;
                if t > 0.0 {
                    let q = o + d * t;
                    let (lo, hi) = scene.grid_box();
                    if q.x >= lo.x && q.x < hi.x && q.z >= lo.z && q.z < hi.z {
                        return Vector3i::new((q.x / pick::CELL).floor() as i32, row as i32 + 1, (q.z / pick::CELL).floor() as i32).to_variant();
                    }
                }
            }
        }
        Variant::nil()
    }
    /// Cells under a brush of `radius_cells` on the hull surface (armour and paint tools).
    #[func]
    fn brush_cells(&self, screen: Vector2, radius_cells: i64) -> PackedVector3Array {
        let Some((o, d)) = self.ray(screen) else { return PackedVector3Array::new() };
        let Some(scene) = self.scene() else { return PackedVector3Array::new() };
        let Some((c, _, _)) = scene.ray_cells(o, d) else { return PackedVector3Array::new() };
        scene.brush(c, radius_cells.clamp(0, 16) as i32).into_iter().map(|c| Vector3::new(c.x as f32, c.y as f32, c.z as f32)).collect()
    }
    /// Parts whose screen bounds fall inside a rectangle (box select): part slots.
    #[func]
    fn parts_in_rect(&self, rect: Rect2) -> PackedInt32Array {
        let Some(scene) = self.scene() else { return PackedInt32Array::new() };
        let r = rect.abs();
        if !(r.position.x.is_finite() && r.position.y.is_finite() && r.size.x.is_finite() && r.size.y.is_finite()) { return PackedInt32Array::new(); }
        let lens = self.st.cam.lens();
        let mut out = Vec::new();
        'parts: for p in scene.parts.iter().filter(|p| scene.part_shown(p)) {
            let (mut lo, mut hi) = ([f32::MAX; 2], [f32::MIN; 2]);
            for k in 0..8 {
                let c = glam::Vec3::new(if k & 1 == 0 { p.min.x } else { p.max.x }, if k & 2 == 0 { p.min.y } else { p.max.y }, if k & 4 == 0 { p.min.z } else { p.max.z });
                let Some(s) = lens.project(c) else { continue 'parts };
                lo = [lo[0].min(s[0]), lo[1].min(s[1])];
                hi = [hi[0].max(s[0]), hi[1].max(s[1])];
            }
            if lo[0] >= r.position.x && lo[1] >= r.position.y && hi[0] <= r.position.x + r.size.x && hi[1] <= r.position.y + r.size.y { out.push(p.slot as i32); }
        }
        PackedInt32Array::from(out.as_slice())
    }
    /// Viewport positions of the hull handles for the 2D gizmo layer: `Array[Dictionary]` with `id: String`,
    /// `station: int`, `screen: Vector2`, `visible: bool`.
    #[func]
    fn handle_points(&self) -> Array<VarDictionary> {
        let scene = self.scene();
        self.handle_list().into_iter().map(|h| {
            let (s, visible) = self.handle_screen(h.pos, scene.as_deref());
            d! { "id" => GString::from(h.id), "station" => h.station, "screen" => s, "visible" => visible }
        }).collect()
    }

    // ---- Overlays and views (L168) ----

    /// `Bp.OVERLAY_*` bits.
    #[func]
    fn set_overlays(&mut self, overlays: i64) {
        let o = overlays & bits::ALL;
        if o == self.overlays { return; }
        self.overlays = o;
        self.st.drawn.overlay_key = None;
    }
    #[func]
    fn overlays(&self) -> i64 { self.overlays }
    /// Show arcs or blind sectors for these part slots only (empty = all mounts and sensors).
    #[func]
    fn set_overlay_focus(&mut self, slots: PackedInt32Array) {
        let mut v: Vec<u16> = slots.as_slice().iter().filter_map(|s| u16::try_from(*s).ok()).collect();
        v.sort_unstable();
        v.dedup();
        v.truncate(4096);
        self.st.focus = v;
    }
    /// Slice the view at a deck (`Bp.NONE` = whole ship) so interior placement is possible.
    #[func]
    fn set_deck_slice(&mut self, deck: i64) { self.st.deck_slice = if deck < 0 { -1 } else { deck.min(4096) }; }
    /// Legend for the overlays shown now: `Array[Dictionary]` with `overlay: int`, `pattern: StringName`
    /// (icon name of the line or hatch pattern), `text: StringName`.
    #[func]
    fn legend(&self) -> Array<VarDictionary> {
        legend_rows(self.overlays, self.st.drawn.ghost_kind).into_iter().map(|(overlay, pattern, text)| d! { "overlay" => overlay, "pattern" => StringName::from(pattern), "text" => StringName::from(text) }).collect()
    }
    /// Render the design to an image for a save slot or blueprint tile (an in-engine render, L1238).
    /// Returns an `Image`, or `null` when the dock is closed.
    #[func]
    fn thumbnail(&mut self, size: Vector2i) -> Option<Gd<Image>> {
        use godot::classes::{sub_viewport::UpdateMode, DisplayServer};
        if self.snap().is_none() || self.st.gpu.is_none() || !self.base().is_inside_tree() { return None; }
        if DisplayServer::singleton().get_name() == "headless" { return None; }
        let w = size.x.clamp(16, 2048);
        let h = size.y.clamp(16, 2048);
        let scene = self.scene()?;
        let (lo, hi) = scene.bounds()?;
        // A three-quarter view from ahead and to starboard, a little above the deck.
        let mut c = cam::DockCam { yaw_deg: 35.0, pitch_deg: 18.0, canvas: [w as f32, h as f32], ..Default::default() };
        c.fit_points(lo, hi, &scene.outline());
        let lens = c.lens();
        let (mut vp, mut camera) = self.st.thumb.clone()?;
        if !(vp.is_instance_valid() && camera.is_instance_valid()) { return None; }
        let v = |x: glam::Vec3| Vector3::new(x.x, x.y, x.z);
        vp.set_size(Vector2i::new(w, h));
        let xf = Transform3D::new(Basis::from_cols(v(lens.right), v(lens.up), v(-lens.forward)), v(lens.pos));
        camera.set_transform(xf);
        camera.set_current(true);
        // The node passes a new transform to the server only when the tree flushes transforms, later in the
        // frame; the forced draw is now, so the server's camera is set directly.
        let mut rs = godot::classes::RenderingServer::singleton();
        rs.camera_set_transform(camera.get_camera_rid(), xf);
        vp.set_update_mode(UpdateMode::ONCE);
        rs.force_draw();
        vp.set_update_mode(UpdateMode::DISABLED);
        vp.get_texture().and_then(|t| t.get_image())
    }
}

impl BpEditorView {
    /// The thumbnail viewport: the dock's own world, no overlays, drawn only when asked.
    fn make_thumb_viewport(&mut self, world: &Gd<godot::classes::World3D>) {
        use godot::classes::{sub_viewport::UpdateMode, SubViewport};
        if self.st.thumb.is_some() { return; }
        let mut vp = SubViewport::new_alloc();
        vp.set_size(Vector2i::new(256, 144));
        vp.set_world_3d(world);
        vp.set_transparent_background(true);
        vp.set_msaa_3d(godot::classes::viewport::Msaa::MSAA_4X);
        // Drawn once now, in an ordinary frame, so its pipelines are compiled before the first thumbnail.
        vp.set_update_mode(UpdateMode::ONCE);
        let mut camera = Camera3D::new_alloc();
        camera.set_fov(cam::FOV_Y_DEG);
        camera.set_near(0.1);
        camera.set_far(20_000.0);
        camera.set_cull_mask(0xFFFFF & !crate::render::LAYER_DOCK_OVERLAY);
        // A transparent viewport cannot draw screen-space reflections (Godot warns for each such draw), so the
        // thumbnail camera looks through a copy of the dock's environment with them off.
        if let Some(mut env) = world.get_environment().map(|e| e.duplicate_resource()) {
            env.set_ssr_enabled(false);
            camera.set_environment(&env);
        }
        vp.add_child(&camera);
        self.base_mut().add_child(&vp);
        camera.set_current(true);
        self.st.thumb = Some((vp, camera));
    }
}

#[cfg(test)]
mod tests {
    use super::cam::*;
    use super::pick::*;
    use super::*;
    use glam::Vec3;

    fn near(a: f32, b: f32, tol: f32) -> bool { (a - b).abs() <= tol }

    #[test]
    fn orbit_keeps_the_pivot_on_screen_and_respects_the_pitch_limits() {
        let mut c = DockCam { target: Vec3::new(20.0, 2.0, 4.0), yaw_deg: 30.0, pitch_deg: 15.0, distance_m: 50.0, canvas: [1600.0, 900.0] };
        let p = Vec3::new(5.0, 1.0, 6.0);
        let before = c.lens().project(p).unwrap();
        c.orbit_about(p, 25.0, 10.0);
        let after = c.lens().project(p).unwrap();
        assert!(near(before[0], after[0], 0.05) && near(before[1], after[1], 0.05), "{before:?} {after:?}");
        assert!(near(c.yaw_deg, 55.0, 1e-4) && near(c.pitch_deg, 25.0, 1e-4));
        c.orbit_about(p, 0.0, 500.0);
        assert_eq!(c.pitch_deg, PITCH_MAX_DEG);
        c.orbit_about(p, 0.0, -500.0);
        assert_eq!(c.pitch_deg, PITCH_MIN_DEG, "the camera may go under the waterline");
        assert!(c.eye().y < c.target.y);
        c.orbit_about(p, f32::NAN, 1.0);
        assert!(c.target.is_finite() && c.yaw_deg.is_finite());
    }

    #[test]
    fn pan_follows_the_cursor_and_zoom_keeps_the_point_under_it() {
        let mut c = DockCam { target: Vec3::ZERO, yaw_deg: 90.0, pitch_deg: 0.0, distance_m: 40.0, canvas: [1600.0, 900.0] };
        let p = Vec3::new(3.0, 1.0, 0.0);
        let s0 = c.lens().project(p).unwrap();
        c.pan_px(100.0, -40.0);
        let s1 = c.lens().project(p).unwrap();
        assert!(near(s1[0] - s0[0], 100.0, 0.5) && near(s1[1] - s0[1], -40.0, 0.5), "{s0:?} {s1:?}");
        let q = Vec3::new(-6.0, 2.0, 0.0);
        let sq = c.lens().project(q).unwrap();
        c.zoom_about(Some(q), 2.0);
        assert!(near(c.distance_m, 20.0, 1e-3));
        let sq2 = c.lens().project(q).unwrap();
        assert!(near(sq[0], sq2[0], 0.1) && near(sq[1], sq2[1], 0.1));
        c.zoom_about(None, 1.0e9);
        assert_eq!(c.distance_m, DIST_MIN_M);
        c.zoom_about(None, 0.0);
        c.zoom_about(None, f32::NAN);
        assert_eq!(c.distance_m, DIST_MIN_M);
    }

    #[test]
    fn framing_fits_the_box() {
        let mut c = DockCam { canvas: [1600.0, 900.0], ..Default::default() };
        let (lo, hi) = (Vec3::new(0.0, 0.0, 0.0), Vec3::new(90.0, 12.0, 10.0));
        c.fit(lo, hi);
        let l = c.lens();
        let (mut a, mut b) = ([f32::MAX; 2], [f32::MIN; 2]);
        for k in 0..8 {
            let p = Vec3::new(if k & 1 == 0 { lo.x } else { hi.x }, if k & 2 == 0 { lo.y } else { hi.y }, if k & 4 == 0 { lo.z } else { hi.z });
            let s = l.project(p).expect("in front");
            assert!(s[0] >= 0.0 && s[0] <= 1600.0 && s[1] >= 0.0 && s[1] <= 900.0, "corner {p} at {s:?}");
            a = [a[0].min(s[0]), a[1].min(s[1])];
            b = [b[0].max(s[0]), b[1].max(s[1])];
        }
        assert!(near(0.5 * (a[0] + b[0]), 800.0, 24.0) && near(0.5 * (a[1] + b[1]), 450.0, 24.0), "centred: {a:?} {b:?}");
        assert!(b[0] - a[0] > 0.6 * 1600.0 || b[1] - a[1] > 0.6 * 900.0, "the box fills the view: {a:?} {b:?}");
        // Degenerate boxes leave the camera alone.
        let before = c;
        c.fit(Vec3::splat(f32::NAN), hi);
        c.fit(hi, lo);
        assert_eq!(c, before);
    }

    /// Replays recorded trackpad events through the mapping (L250): plain, Shift and Cmd drags, the Orbit /
    /// Pan switch, inverts and speeds, and pinches.
    #[test]
    fn the_trackpad_mapping_is_locked() {
        let s = Trackpad::default();
        let k = PAN_GESTURE_PX;
        assert_eq!(map_pan_gesture([0.4, -0.2], false, false, &s), Move::Orbit { dx: -0.4 * k, dy: 0.2 * k });
        assert_eq!(map_pan_gesture([0.4, -0.2], true, false, &s), Move::Pan { dx: -0.4 * k, dy: 0.2 * k });
        match map_pan_gesture([0.3, -1.0], false, true, &s) { Move::Zoom { factor } => assert!(factor > 1.0), m => panic!("{m:?}") }
        match map_pan_gesture([0.0, 1.0], true, true, &s) { Move::Zoom { factor } => assert!(factor < 1.0), m => panic!("{m:?}") }
        assert_eq!(map_pan_gesture([0.3, 0.0], false, true, &s), Move::None, "Cmd zooms on the vertical only");
        let pan_first = Trackpad { drag_pans: true, ..s };
        assert!(matches!(map_pan_gesture([1.0, 0.0], false, false, &pan_first), Move::Pan { .. }));
        assert!(matches!(map_pan_gesture([1.0, 0.0], true, false, &pan_first), Move::Orbit { .. }), "with Pan chosen, Shift orbits");
        let inv = Trackpad { invert_orbit: true, orbit_speed: 2.0, ..s };
        assert_eq!(map_pan_gesture([0.5, 0.25], false, false, &inv), Move::Orbit { dx: 0.5 * k * 2.0, dy: 0.25 * k * 2.0 });
        let inv_pan = Trackpad { invert_pan: true, ..s };
        assert_eq!(map_pan_gesture([0.5, 0.0], true, false, &inv_pan), Move::Pan { dx: 0.5 * k, dy: 0.0 });
        assert_eq!(map_pan_gesture([f32::NAN, 0.0], false, false, &s), Move::None);
        assert_eq!(map_pan_gesture([0.0, 0.0], false, false, &s), Move::None);
        match map_magnify(1.1, &s) { Move::Zoom { factor } => assert!(near(factor, 1.1, 1e-5)), m => panic!("{m:?}") }
        match map_magnify(1.1, &Trackpad { invert_zoom: true, ..s }) { Move::Zoom { factor } => assert!(factor < 1.0), m => panic!("{m:?}") }
        match map_magnify(0.9, &Trackpad { zoom_speed: 2.0, ..s }) { Move::Zoom { factor } => assert!(near(factor, 0.81, 1e-4)), m => panic!("{m:?}") }
        for bad in [0.0, -1.0, f64::NAN, 1.0] { assert_eq!(map_magnify(bad, &s), Move::None); }
    }

    #[test]
    fn gestures_fix_their_pivot_and_count_reversals() {
        let mut g = Gestures::default();
        let mut picks = 0;
        let mut p = || { picks += 1; Vec3::new(picks as f32, 0.0, 0.0) };
        // One gesture: the pivot is picked once.
        let a = g.event(Kind::Orbit, 0.0, [100.0, 100.0], [5.0, 0.0], &mut p);
        let b = g.event(Kind::Orbit, 0.05, [101.0, 100.0], [10.0, 0.0], &mut p);
        assert_eq!(a, b);
        // The opposite way 0.5 s after it ended: a reversal.
        let c = g.event(Kind::Orbit, 0.6, [101.0, 100.0], [-15.0, 0.0], &mut p);
        assert_ne!(c, b, "a gap starts a new gesture with its own pivot");
        assert_eq!(g.reversals, 1);
        // The same way again after 2 s: none.
        g.event(Kind::Orbit, 3.0, [101.0, 100.0], [-20.0, 0.0], &mut p);
        assert_eq!(g.reversals, 1);
        // Opposite on the other axis: none.
        g.event(Kind::Orbit, 3.5, [101.0, 100.0], [0.0, 30.0], &mut p);
        assert_eq!(g.reversals, 1);
        // A pan in between ends the orbit; a jump of the cursor starts a new gesture.
        g.event(Kind::Pan, 3.6, [f32::NAN; 2], [3.0, 0.0], &mut p);
        let x = g.event(Kind::Orbit, 3.65, [400.0, 100.0], [1.0, 0.0], &mut p);
        let y = g.event(Kind::Orbit, 3.7, [600.0, 100.0], [1.0, 0.0], &mut p);
        assert_ne!(x, y);
        g.reset();
        g.reset();
        assert_eq!(g.resets, 2);
    }

    fn scene_of(bp: &content::blueprint::Blueprint, cut: Option<i16>) -> PickScene { PickScene::build(bp, content::catalogue::catalogue(), cut) }
    fn starter() -> content::blueprint::Blueprint { content::campaign::load::stock_blueprint("starters/patrol15.bp.ron").expect("starter") }

    #[test]
    fn rays_find_the_first_cell_and_its_face() {
        let mut bp = starter();
        let mut g = content::cell::CellGrid::new([8, 6, 4]);
        for x in 0..8 { for z in 0..4 { for y in 0..3 { g.set(content::cell::CellPos::new(x, y, z), content::cell::Cell { material: content::cell::Material::Steel, ..content::cell::Cell::EMPTY }); } } }
        bp.cells = g.to_rows();
        bp.parts.clear();
        bp.hull.planes.clear();
        let s = scene_of(&bp, None);
        // Straight down onto the top at (2, 2, 1): enters through its top face (Y+ = 3).
        let (c, f, t) = s.ray_cells(Vec3::new(1.25, 10.0, 0.75), Vec3::NEG_Y).expect("hit");
        assert_eq!((c, f), (content::cell::CellPos::new(2, 2, 1), 3));
        assert!(near(t, 10.0 - 1.5, 1e-3));
        // Along +X from outside: the X- face of the first column.
        let (c, f, _) = s.ray_cells(Vec3::new(-5.0, 0.25, 0.25), Vec3::X).expect("hit");
        assert_eq!((c, f), (content::cell::CellPos::new(0, 0, 0), 0));
        // A diagonal that passes over the block misses.
        assert!(s.ray_cells(Vec3::new(-1.0, 5.0, -1.0), Vec3::new(1.0, 0.01, 1.0)).is_none());
        // Degenerate rays.
        assert!(s.ray_cells(Vec3::splat(f32::NAN), Vec3::X).is_none());
        assert!(s.ray_cells(Vec3::ZERO, Vec3::ZERO).is_none());
        // A slice at row 1 hides row 2: the ray stops one row lower.
        let s = scene_of(&bp, Some(1));
        let (c, f, _) = s.ray_cells(Vec3::new(1.25, 10.0, 0.75), Vec3::NEG_Y).expect("hit");
        assert_eq!((c, f), (content::cell::CellPos::new(2, 1, 1), 3));
        // The brush keeps surface cells within the radius.
        let b = s.brush(content::cell::CellPos::new(2, 1, 1), 1);
        assert!(b.contains(&content::cell::CellPos::new(2, 1, 1)) && b.contains(&content::cell::CellPos::new(2, 0, 1)));
        assert!(b.iter().all(|c| c.y <= 1));
        let full = scene_of(&bp, None);
        assert!(!full.brush(content::cell::CellPos::new(3, 1, 1), 1).contains(&content::cell::CellPos::new(3, 1, 1)), "an inside cell is not surface");
    }

    #[test]
    fn parts_and_planes_are_picked_on_a_starter() {
        let bp = starter();
        let s = scene_of(&bp, None);
        assert!(!s.parts.is_empty(), "the starter has parts");
        let p = &s.parts[0];
        let c = 0.5 * (p.min + p.max);
        let (i, cell, _, t) = s.ray_parts(c + Vec3::Y * 50.0, Vec3::NEG_Y).expect("the part from above");
        assert!(s.parts[i].min.y <= cell.y as f32 * CELL + 1e-3 && t > 0.0);
        let (lo, hi) = s.bounds().expect("bounds");
        assert!(lo.cmple(p.min).all() && hi.cmpge(p.max).all());
        if let Some(deck) = s.planes.iter().find(|p| p.axis == content::enums::Axis::Y) {
            let y = PickScene::plane_m(deck);
            let mid = 0.5 * (lo + hi);
            let hit = s.ray_planes(Vec3::new(mid.x, y + 30.0, mid.z), Vec3::NEG_Y);
            assert!(hit.is_some_and(|(pl, t)| pl.axis == content::enums::Axis::Y && near(t, 30.0, 1e-3)) || hit.is_some(), "{hit:?}");
        }
    }

    #[test]
    fn overlay_geometry_is_built_and_legended() {
        let mut l = geom::Lines::default();
        l.seg(Vec3::ZERO, Vec3::X, 0.0, [1.0; 4], geom::Pattern::Dashed.dash(1.0));
        assert_eq!((l.pos.len(), l.idx.len(), l.len()), (4, 6, 1));
        l.seg(Vec3::ZERO, Vec3::ZERO, 0.0, [1.0; 4], [0.0, 1.0]);
        l.seg(Vec3::ZERO, Vec3::splat(f32::NAN), 0.0, [1.0; 4], [0.0, 1.0]);
        assert_eq!(l.len(), 1, "degenerate segments are dropped");
        assert_eq!(geom::sweep(350.0, 10.0), 20.0);
        assert_eq!(geom::sweep(10.0, 10.0), 360.0);
        assert!(geom::bearing(90.0).distance(Vec3::Z) < 1e-6, "90 degrees is to starboard");
        let (mut f, mut lines) = (geom::Fills::default(), geom::Lines::default());
        geom::sector(&mut f, &mut lines, Vec3::ZERO, 10.0, 300.0, 60.0, [1.0; 4], [1.0; 4], Some(([1.0; 4], 1.0)));
        assert!(!f.is_empty() && !lines.is_empty());
        // Every hatch vertex stays inside the sector's radius.
        assert!(lines.pos.iter().all(|p| Vec3::from(*p).length() <= 10.0 + 1e-3));
        assert_eq!(geom::heat(5, [0.0; 4], [1.0; 4]), None);
        assert!(geom::heat(255, [0.0; 4], [1.0; 4]).is_some_and(|c| near(c[0], 1.0, 1e-6)));
        let rows = legend_rows(bits::XRAY | bits::ARCS, None);
        assert!(rows.iter().any(|r| r.2 == "dock.overlay.power" && r.1 == "line_solid"));
        assert!(rows.iter().any(|r| r.2 == "dock.overlay.data" && r.1 == "line_dotted"));
        assert!(rows.iter().any(|r| r.2 == "dock.overlay.doubled"));
        assert!(rows.iter().any(|r| r.1 == "hatch"));
        assert!(legend_rows(0, None).is_empty());
        assert!(legend_rows(0, Some(GhostKind::Edit)).iter().any(|r| r.2 == "dock.overlay.ghost_preview"));
        for (_, _, key) in legend_rows(bits::ALL, None).into_iter().chain(legend_rows(bits::ALL, Some(GhostKind::Edit))) { assert!(VIEW_KEYS.contains(&key), "{key}"); }
    }

    #[test]
    fn overlays_draw_from_a_built_design() {
        let bp = starter();
        let cat = content::catalogue::catalogue();
        let prefab = Arc::new(sim_core::vessel::build_prefab(&bp, cat));
        let design = sim_systems::ship::ShipDesign::build(&bp, prefab, cat, &content::ruleset::Ruleset::default());
        let start = std::time::Instant::now();
        let slow = slow_data(&design, cat, bits::ALL);
        assert!(start.elapsed().as_secs_f64() < 10.0);
        let b = slow.balance.expect("balance");
        assert!(b.floats && b.waterline_m > 0.0 && b.cb.y < b.waterline_m + 0.5, "{b:?}");
        let scene = scene_of(&bp, None);
        let pal = Palette::default();
        let o = build_overlays(&OverlayIn { bits: bits::ALL, focus: &[], design: &design, slow: Some(&slow), scene: &scene, design_waterline_row: bp.hull.design_waterline_y }, &pal);
        assert!(!o.top.is_empty() && !o.sheets.is_empty());
        if !slow.arcs.is_empty() { assert!(!o.fills.is_empty() && !o.world.is_empty()); }
        if design.prefab.networks.conduits.iter().any(|c| c.cells.len() > 1) { assert!(o.labels.iter().any(|l| l.1 == "dock.overlay.power" || l.1 == "dock.overlay.fuel" || l.1 == "dock.overlay.data")); }
        // A focus on no real slot hides the arcs.
        let none = build_overlays(&OverlayIn { bits: bits::ARCS, focus: &[u16::MAX], design: &design, slow: Some(&slow), scene: &scene, design_waterline_row: 0 }, &pal);
        assert!(none.fills.is_empty());
    }

    #[test]
    fn an_edit_preview_shows_what_it_adds_and_removes() {
        let cur = starter();
        let cat = content::catalogue::catalogue();
        let mut next = cur.clone();
        let removed = next.parts.remove(0);
        let g = ghost_of_edit(&cur, &next, cat);
        let def = cat.part(&removed.part).expect("part");
        let n = crate::editor::parts::footprint_cells(def, removed.pos, removed.rot).len();
        assert!(g.boxes.iter().filter(|b| b.1 == "warning").count() >= n);
        let back = ghost_of_edit(&next, &cur, cat);
        assert!(back.parts.iter().any(|p| p.4 == "ghost_preview"));
        assert_eq!(ghost_of_edit(&cur, &cur, cat), GhostDraw::default());
    }

    #[test]
    fn every_view_key_has_an_english_row() {
        let csv = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../game/i18n/dock.csv")).expect("dock.csv");
        for k in VIEW_KEYS { assert!(csv.lines().any(|l| l.starts_with(&format!("{k},"))), "{k} has no row in dock.csv"); }
    }
}
