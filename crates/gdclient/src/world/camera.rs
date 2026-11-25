//! Part of unit `gd-world-view`. Camera mathematics with no Godot in it: the orbit pose, the sea floor rule,
//! shake and roll, and the lens that turns viewport positions into rays and world points into viewport
//! positions. `mod.rs` moves the one `Camera3D` from what this computes. Tests run headless.
//!
//! # Conventions
//! - Yaw is the true bearing the camera looks along, in degrees: 0 looks north (world -Z), 90 east (+X).
//!   Pitch is how far the camera sits above the horizontal through its target: positive looks down on the
//!   ship. The orbit keeps its bearing when the ship turns, as a lookout's eye does.
//! - Viewport positions are in canvas units with the origin at the top left, as input events give them;
//!   the lens carries the canvas size it was built for.
//! - Everything here is in render space except [`Rig`], which keeps its focus in world metres so an origin
//!   rebase moves nothing.
// Client rendering, not simulation (clippy.toml): std floats are allowed here.
#![allow(clippy::disallowed_methods)]
use super::{Orbit, CAMERA_FLOOR_M, ORBIT_MAX_DISTANCE_M, ORBIT_MIN_DISTANCE_M};
use glam::{DVec3, Vec3};

/// Vertical field of view of the sortie camera, degrees (the reference the pixel rules are written for,
/// L1786).
pub const FOV_Y_DEG: f32 = 60.0;
/// Orbit pitch limits, degrees: from just under the horizontal (the sea floor rule then holds the camera
/// up) to nearly overhead.
pub const PITCH_MIN_DEG: f32 = -10.0;
pub const PITCH_MAX_DEG: f32 = 85.0;
/// One wheel step changes the orbit distance by this factor.
pub const ZOOM_STEP: f32 = 0.88;
/// The focus point's height follows the hull with this time constant, seconds, so the horizon does not
/// bob with every wave while the ship visibly does.
const FOCUS_HEIGHT_TAU_S: f64 = 1.2;
/// The camera rolls with this share of the followed hull's heel at the full setting, to at most
/// [`ROLL_MAX_RAD`].
const ROLL_SHARE: f32 = 0.25;
const ROLL_MAX_RAD: f32 = 0.07;
const ROLL_TAU_S: f32 = 0.6;
/// Shake: how fast it dies away, and the most it moves or turns the camera at the full setting.
const SHAKE_DECAY_PER_S: f32 = 1.6;
const SHAKE_MAX_M: f32 = 0.35;
const SHAKE_MAX_RAD: f32 = 0.012;
/// The orbit camera keeps this far outside the followed hull's box, so zooming in never puts the eye, or
/// the near plane, inside the ship.
pub const HULL_CLEAR_M: f32 = 4.0;
/// A sortie opens with the followed hull's longest side across this share of a 16:9 view's width, with the
/// distance kept inside [`FRAMING_MIN_M`]..=[`FRAMING_MAX_M`] so a launch still shows some sea around it.
const FRAMING_SHARE: f32 = 0.45;
pub const FRAMING_MIN_M: f32 = 30.0;
pub const FRAMING_MAX_M: f32 = 600.0;
/// A shake further ahead of playback than this is felt at once (a seek landed after it); the same half
/// second the effect units allow their events.
const SHAKE_HOLD_MAX_TICKS: f64 = 0.5 * sim_core::TICK_HZ as f64;
/// Most shakes held for playback; past it the oldest are let go unfelt.
const SHAKE_HOLD_MAX: usize = 256;

/// Offset from the orbit target to the camera.
pub fn orbit_offset(orbit: &Orbit) -> Vec3 {
    let yaw = orbit.yaw_deg.to_radians();
    let pitch = orbit.pitch_deg.clamp(PITCH_MIN_DEG, PITCH_MAX_DEG).to_radians();
    let d = orbit.distance_m.clamp(ORBIT_MIN_DISTANCE_M, ORBIT_MAX_DISTANCE_M);
    let look = Vec3::new(yaw.sin(), 0.0, -yaw.cos());
    -look * (d * pitch.cos()) + Vec3::Y * (d * pitch.sin())
}

/// Apply an orbit input: yaw wraps to `[0, 360)`, pitch clamps. Non-finite input is ignored.
pub fn orbit_by(orbit: &mut Orbit, yaw_deg: f64, pitch_deg: f64) {
    if yaw_deg.is_finite() { orbit.yaw_deg = (orbit.yaw_deg as f64 + yaw_deg).rem_euclid(360.0) as f32; }
    if pitch_deg.is_finite() { orbit.pitch_deg = (orbit.pitch_deg as f64 + pitch_deg).clamp(PITCH_MIN_DEG as f64, PITCH_MAX_DEG as f64) as f32; }
}

/// Apply wheel steps (positive = closer), clamped to the orbit limits. `stood_off_m` is where the camera
/// actually stood ([`Rig::stood_off_m`]): when the followed hull's box holds it further out than the orbit
/// asks, the zoom starts from there, so the wheel never has dead travel to wind back through.
pub fn zoom_by(orbit: &mut Orbit, steps: f64, stood_off_m: f32) {
    if !steps.is_finite() { return; }
    if stood_off_m.is_finite() && stood_off_m > orbit.distance_m { orbit.distance_m = stood_off_m.min(ORBIT_MAX_DISTANCE_M); }
    let d = orbit.distance_m as f64 * (ZOOM_STEP as f64).powf(steps.clamp(-64.0, 64.0));
    orbit.distance_m = d.clamp(ORBIT_MIN_DISTANCE_M as f64, ORBIT_MAX_DISTANCE_M as f64) as f32;
}

/// The orbit distance a sortie opens at for a hull of `size_m` (length, height, beam): the one that puts
/// its longest side across [`FRAMING_SHARE`] of the view's width at [`FOV_Y_DEG`].
pub fn framing_distance(size_m: Vec3) -> f32 {
    let longest = size_m.x.max(size_m.z).max(size_m.y);
    if !longest.is_finite() || longest <= 0.0 { return Orbit::default().distance_m; }
    // Pixels per metre at distance d are H / (2 tan(fov / 2) d), and the view is 16:9.
    let per_length = (9.0 / 16.0) / (2.0 * (0.5 * FOV_Y_DEG.to_radians()).tan() * FRAMING_SHARE);
    (longest * per_length).clamp(FRAMING_MIN_M, FRAMING_MAX_M).clamp(ORBIT_MIN_DISTANCE_M, ORBIT_MAX_DISTANCE_M)
}

/// The orbit offset lengthened, if need be, so the camera stands [`HULL_CLEAR_M`] outside the followed
/// hull's box. The box turns with the hull's heading only: one that heeled and pitched with the ship would
/// make a close camera breathe with every wave.
pub fn clear_of_hull(offset: Vec3, focus: &Focus) -> Vec3 {
    let d = offset.length();
    let half = focus.half_m;
    if !d.is_finite() || d < 1e-3 || !half.is_finite() { return offset; }
    let dir = offset / d;
    let mut bow = Vec3::new(focus.bow.x, 0.0, focus.bow.z).normalize_or_zero();
    if !bow.is_finite() || bow == Vec3::ZERO { bow = Vec3::X; }
    let starboard = bow.cross(Vec3::Y);
    // Where the ray from the box's centre along the offset leaves the box.
    let along = [dir.dot(bow).abs(), dir.y.abs(), dir.dot(starboard).abs()];
    let mut exit = f32::MAX;
    for a in 0..3 { if along[a] > 1e-6 { exit = exit.min(half[a].max(0.0) / along[a]); } }
    if exit == f32::MAX { return offset; }
    dir * d.max(exit + HULL_CLEAR_M)
}

/// A camera in render space with what projection needs.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Lens {
    pub pos: Vec3,
    pub right: Vec3,
    pub up: Vec3,
    pub forward: Vec3,
    pub tan_half_y: f32,
    /// Width over height.
    pub aspect: f32,
    /// Canvas size in units, for viewport positions.
    pub canvas: [f32; 2],
}

impl Lens {
    /// A camera at `pos` looking at `target`, rolled by `roll_rad` about its view axis (positive rolls the
    /// horizon clockwise on screen). A degenerate pair looks north.
    pub fn look(pos: Vec3, target: Vec3, roll_rad: f32, fov_y_deg: f32, canvas: [f32; 2]) -> Lens {
        let mut forward = (target - pos).normalize_or_zero();
        if !forward.is_finite() || forward == Vec3::ZERO { forward = Vec3::NEG_Z; }
        // Looking straight up or down: any horizontal right vector will do.
        let mut right = forward.cross(Vec3::Y);
        right = if right.length_squared() < 1e-8 { Vec3::X } else { right.normalize() };
        let mut up = right.cross(forward).normalize();
        if roll_rad != 0.0 && roll_rad.is_finite() {
            let (s, c) = roll_rad.sin_cos();
            let (r, u) = (right * c + up * s, up * c - right * s);
            right = r;
            up = u;
        }
        let fov = if fov_y_deg.is_finite() { fov_y_deg.clamp(1.0, 170.0) } else { FOV_Y_DEG };
        let w = if canvas[0].is_finite() && canvas[0] >= 1.0 { canvas[0] } else { 1920.0 };
        let h = if canvas[1].is_finite() && canvas[1] >= 1.0 { canvas[1] } else { 1080.0 };
        Lens { pos: if pos.is_finite() { pos } else { Vec3::ZERO }, right, up, forward, tan_half_y: (0.5 * fov.to_radians()).tan(), aspect: w / h, canvas: [w, h] }
    }

    /// Unit direction of the ray through a viewport position.
    pub fn ray(&self, screen: [f32; 2]) -> Vec3 {
        let nx = 2.0 * screen[0] / self.canvas[0] - 1.0;
        let ny = 1.0 - 2.0 * screen[1] / self.canvas[1];
        let d = self.forward + self.right * (nx * self.tan_half_y * self.aspect) + self.up * (ny * self.tan_half_y);
        let d = d.normalize_or_zero();
        if d.is_finite() && d != Vec3::ZERO { d } else { self.forward }
    }

    /// Camera-space coordinates of a point: x right, y up, z along the view (positive in front).
    fn view(&self, p: Vec3) -> Vec3 { let v = p - self.pos; Vec3::new(v.dot(self.right), v.dot(self.up), v.dot(self.forward)) }

    /// Viewport position of a render-space point; `None` behind the camera.
    pub fn project(&self, p: Vec3) -> Option<[f32; 2]> {
        let v = self.view(p);
        if !v.is_finite() || v.z <= 1e-3 { return None; }
        let nx = v.x / (v.z * self.tan_half_y * self.aspect);
        let ny = v.y / (v.z * self.tan_half_y);
        Some([(nx + 1.0) * 0.5 * self.canvas[0], (1.0 - ny) * 0.5 * self.canvas[1]])
    }

    /// Viewport position of a point for a marker: where it is when it is inside the viewport less
    /// `margin`, otherwise the point on that inset edge in its direction (a point behind the camera maps to
    /// the edge on the side it would come into view from). The flag says whether it was inside.
    pub fn project_clamped(&self, p: Vec3, margin: f32) -> ([f32; 2], bool) {
        let [w, h] = self.canvas;
        let m = margin.clamp(0.0, 0.45 * w.min(h));
        let centre = [0.5 * w, 0.5 * h];
        let v = self.view(p);
        if !v.is_finite() { return (centre, false); }
        if v.z > 1e-3 {
            let s = self.project(p).unwrap_or(centre);
            if s[0] >= m && s[0] <= w - m && s[1] >= m && s[1] <= h - m { return (s, true); }
        }
        // Direction on screen: the point's bearing off the view axis, which stays meaningful behind the eye.
        let mut dir = [v.x, -v.y];
        if dir[0].abs() + dir[1].abs() < 1e-6 { dir = [0.0, 1.0]; }
        let (hw, hh) = (0.5 * w - m, 0.5 * h - m);
        let scale = (hw / dir[0].abs().max(1e-6)).min(hh / dir[1].abs().max(1e-6));
        ([centre[0] + dir[0] * scale, centre[1] + dir[1] * scale], false)
    }

    /// Pixels (canvas units) one metre covers at `distance_m` in front of the camera.
    pub fn px_per_m(&self, distance_m: f32) -> f32 { self.canvas[1] / (2.0 * self.tan_half_y * distance_m.max(1e-3)) }
}

/// Nearest positive distance at which a ray enters an axis-aligned box, or `None`.
pub fn ray_box(origin: Vec3, dir: Vec3, min: Vec3, max: Vec3) -> Option<f32> {
    let (mut t0, mut t1) = (0.0f32, f32::MAX);
    for a in 0..3 {
        if dir[a].abs() < 1e-9 {
            if origin[a] < min[a] || origin[a] > max[a] { return None; }
            continue;
        }
        let (near, far) = ((min[a] - origin[a]) / dir[a], (max[a] - origin[a]) / dir[a]);
        let (near, far) = if near <= far { (near, far) } else { (far, near) };
        t0 = t0.max(near);
        t1 = t1.min(far);
        if t0 > t1 { return None; }
    }
    Some(t0)
}

/// Distance along a ray to its closest approach to a point, and the miss distance there; `None` behind.
pub fn ray_point(origin: Vec3, dir: Vec3, p: Vec3) -> Option<(f32, f32)> {
    let v = p - origin;
    let t = v.dot(dir);
    if t.is_nan() || t <= 0.0 { return None; }
    Some((t, (v - dir * t).length()))
}

/// Camera shake as a decaying trauma level (the square of it moves the camera).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Shake { pub trauma: f32, phase: f32 }
impl Shake {
    pub fn add(&mut self, amount: f32) { if amount.is_finite() && amount > 0.0 { self.trauma = (self.trauma + amount).min(1.0); } }
    /// Advance by `dt` and return this frame's position offset (metres, camera axes) and roll (radians) at
    /// `scale` 0..=1 (the accessibility slider).
    pub fn step(&mut self, dt: f32, scale: f32) -> (Vec3, f32) {
        let dt = if dt.is_finite() { dt.clamp(0.0, 0.25) } else { 0.0 };
        self.trauma = (self.trauma - SHAKE_DECAY_PER_S * dt).max(0.0);
        self.phase = (self.phase + dt) % 1_000.0;
        let k = self.trauma * self.trauma * scale.clamp(0.0, 1.0);
        if k <= 0.0 { return (Vec3::ZERO, 0.0); }
        // Three incommensurate tones per axis: irregular, band-limited, and the same on every machine.
        let n = |a: f32, b: f32, c: f32| 0.5 * (self.phase * a).sin() + 0.3 * (self.phase * b + 1.3).sin() + 0.2 * (self.phase * c + 2.1).sin();
        (Vec3::new(n(61.0, 97.0, 139.0), n(71.0, 113.0, 151.0), 0.0) * (SHAKE_MAX_M * k), n(53.0, 89.0, 127.0) * SHAKE_MAX_RAD * k)
    }
}

/// Shakes waiting for playback. Events reach the client the interpolation delay ahead of the tick the view
/// is drawn at, and the hull, part and effect units all show an event when playback reaches it; the camera
/// kicks on the same frame as the flash, not a tenth of a second before it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ShakeQueue { waiting: Vec<(u64, f32)> }
impl ShakeQueue {
    pub fn clear(&mut self) { self.waiting.clear(); }
    pub fn len(&self) -> usize { self.waiting.len() }
    /// Hold a shake of `amount` (0..=1) for the event of `tick`. Nothing is held for no shake.
    pub fn push(&mut self, tick: u64, amount: f32) {
        if !(amount.is_finite() && amount > 0.0) { return; }
        if self.waiting.len() >= SHAKE_HOLD_MAX { self.waiting.remove(0); }
        self.waiting.push((tick, amount));
    }
    /// The shakes playback has reached, summed (the caller's `Shake::add` caps the total). One too far
    /// ahead to be worth waiting for is due at once, and a playback tick that is not a number holds nothing.
    pub fn take_due(&mut self, render_tick: f64) -> f32 {
        let mut due = 0.0;
        self.waiting.retain(|&(tick, amount)| {
            let ahead = tick as f64 - render_tick;
            if ahead > 0.0 && ahead <= SHAKE_HOLD_MAX_TICKS { return true; }
            due += amount;
            false
        });
        due
    }
}

/// What the rig follows this frame.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Focus {
    /// Centre of the followed hull, world metres.
    pub centre: DVec3,
    /// The hull's up vector in world space (its heel and trim), unit.
    pub up: Vec3,
    /// The hull's bow direction (+X of its frame) in world space.
    pub bow: Vec3,
    /// Half the hull's box in its own frame, metres: length, height, beam. Zero while the design is unknown.
    pub half_m: Vec3,
}
impl Focus {
    /// A focus with no box: a point to orbit.
    pub fn point(centre: DVec3, up: Vec3) -> Focus { Focus { centre, up, bow: Vec3::X, half_m: Vec3::ZERO } }
}

/// A camera pose in world metres.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Pose { pub pos: DVec3, pub target: DVec3, pub roll_rad: f32 }

/// The state the sortie camera keeps between frames.
#[derive(Clone, Copy, Debug, Default)]
pub struct Rig {
    /// Where the orbit turns about, world metres; held when nothing is followed.
    pub target: Option<DVec3>,
    pub shake: Shake,
    /// True once this sortie's opening distance has been set for the followed hull, or the player has
    /// zoomed; the framing is offered once and never fought over.
    pub framed: bool,
    /// How far from its target the orbit camera stood last frame, metres: the orbit's distance, or more
    /// where the followed hull's box held it out. Zero before the first orbit frame.
    pub stood_off_m: f32,
    roll: f32,
}

impl Rig {
    pub fn reset(&mut self) { *self = Rig::default(); }

    /// This frame's camera. `free` overrides the orbit with a position and a look-at point. `sea` gives the
    /// surface height and `ground` the terrain height at a map position; the camera is kept
    /// [`CAMERA_FLOOR_M`] above both (L106). `shake_scale` and `roll_scale` are the accessibility sliders.
    #[allow(clippy::too_many_arguments)]
    pub fn frame(&mut self, orbit: &Orbit, free: Option<([f64; 3], [f64; 3])>, focus: Option<Focus>, dt: f64, shake_scale: f32, roll_scale: f32, sea: &dyn Fn(f64, f64) -> f64, ground: &dyn Fn(f64, f64) -> f64) -> Pose {
        let dt = if dt.is_finite() { dt.clamp(0.0, 0.25) } else { 0.0 };
        let (shake_offset, shake_roll) = self.shake.step(dt as f32, shake_scale);
        let floor = |p: DVec3| -> DVec3 {
            let least = sea(p.x, p.z).max(ground(p.x, p.z)) + CAMERA_FLOOR_M as f64;
            if least.is_finite() && p.y < least { DVec3::new(p.x, least, p.z) } else { p }
        };
        if let Some((pos, look)) = free {
            let (pos, look) = (DVec3::from(pos), DVec3::from(look));
            if pos.is_finite() && look.is_finite() {
                self.roll = 0.0;
                return Pose { pos: floor(pos), target: look, roll_rad: 0.0 };
            }
        }
        // Follow the hull's centre on the map exactly and its height through a low-pass.
        if let Some(f) = focus.filter(|f| f.centre.is_finite()) {
            let y = match self.target {
                Some(t) if (t.x - f.centre.x).hypot(t.z - f.centre.z) < 500.0 => t.y + (f.centre.y - t.y) * (1.0 - (-dt / FOCUS_HEIGHT_TAU_S).exp()),
                _ => f.centre.y,
            };
            self.target = Some(DVec3::new(f.centre.x, y, f.centre.z));
        }
        let target = self.target.unwrap_or(DVec3::ZERO);
        let offset = orbit_offset(orbit);
        let offset = focus.map_or(offset, |f| clear_of_hull(offset, &f));
        self.stood_off_m = offset.length();
        let pos = floor(target + offset.as_dvec3());
        // Roll with a share of the hull's heel as seen along the view axis.
        let lens = Lens::look(Vec3::ZERO, (target - pos).as_vec3(), 0.0, FOV_Y_DEG, [16.0, 9.0]);
        let want = focus.map_or(0.0, |f| {
            let up = if f.up.is_finite() && f.up.length_squared() > 0.25 { f.up.normalize() } else { Vec3::Y };
            (-up.dot(lens.right).atan2(up.dot(lens.up).max(1e-3)) * ROLL_SHARE * roll_scale.clamp(0.0, 1.0)).clamp(-ROLL_MAX_RAD, ROLL_MAX_RAD)
        });
        self.roll += (want - self.roll) * (1.0 - (-(dt as f32) / ROLL_TAU_S).exp());
        if !self.roll.is_finite() { self.roll = 0.0; }
        let shaken = pos + (lens.right * shake_offset.x + lens.up * shake_offset.y).as_dvec3();
        Pose { pos: floor(shaken), target, roll_rad: self.roll + shake_roll }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flat(_: f64, _: f64) -> f64 { 0.0 }
    fn deep(_: f64, _: f64) -> f64 { -200.0 }

    #[test]
    fn orbit_inputs_wrap_clamp_and_ignore_nonsense() {
        let mut o = Orbit::default();
        orbit_by(&mut o, 200.0, 100.0);
        assert!((o.yaw_deg - 40.0).abs() < 1e-4);
        assert_eq!(o.pitch_deg, PITCH_MAX_DEG);
        orbit_by(&mut o, -50.0, -500.0);
        assert!((o.yaw_deg - 350.0).abs() < 1e-4);
        assert_eq!(o.pitch_deg, PITCH_MIN_DEG);
        orbit_by(&mut o, f64::NAN, f64::INFINITY);
        assert!(o.yaw_deg.is_finite() && o.pitch_deg.is_finite());
        // Zoom: positive steps come closer, both ends clamp (8 m and 2,000 m).
        let d0 = o.distance_m;
        zoom_by(&mut o, 1.0, 0.0);
        assert!((o.distance_m - d0 * ZOOM_STEP).abs() < 1e-3);
        zoom_by(&mut o, 1_000.0, 0.0);
        assert_eq!(o.distance_m, ORBIT_MIN_DISTANCE_M);
        zoom_by(&mut o, -1_000.0, 0.0);
        assert_eq!(o.distance_m, ORBIT_MAX_DISTANCE_M);
        zoom_by(&mut o, f64::NAN, 0.0);
        assert_eq!(o.distance_m, ORBIT_MAX_DISTANCE_M);
    }

    #[test]
    fn the_orbit_looks_along_its_bearing_from_above() {
        // Looking east from 10 degrees up at 100 m: the camera is west of and above the target.
        let o = Orbit { yaw_deg: 90.0, pitch_deg: 10.0, distance_m: 100.0, follow: None };
        let off = orbit_offset(&o);
        assert!((off.x + 100.0 * 10f32.to_radians().cos()).abs() < 1e-3 && off.z.abs() < 1e-3);
        assert!((off.y - 100.0 * 10f32.to_radians().sin()).abs() < 1e-3);
        assert!((off.length() - 100.0).abs() < 1e-3);
    }

    #[test]
    fn rays_and_projection_are_inverse() {
        let lens = Lens::look(Vec3::new(10.0, 30.0, 50.0), Vec3::new(0.0, 2.0, -40.0), 0.03, 60.0, [1920.0, 1080.0]);
        assert!((lens.forward.length() - 1.0).abs() < 1e-5 && lens.right.dot(lens.up).abs() < 1e-5 && lens.right.dot(lens.forward).abs() < 1e-5);
        // The centre ray is the view axis; the target lands in the centre.
        assert!((lens.ray([960.0, 540.0]) - lens.forward).length() < 1e-5);
        let c = lens.project(Vec3::new(0.0, 2.0, -40.0)).unwrap();
        assert!((c[0] - 960.0).abs() < 0.01 && (c[1] - 540.0).abs() < 0.01);
        for s in [[0.0f32, 0.0], [1919.0, 3.0], [400.0, 900.0], [1500.0, 200.0]] {
            let p = lens.pos + lens.ray(s) * 137.0;
            let back = lens.project(p).unwrap();
            assert!((back[0] - s[0]).abs() < 0.05 && (back[1] - s[1]).abs() < 0.05, "{s:?} came back as {back:?}");
        }
        // L1786: one metre at 100 m covers 9.35 px at 1080p with a 60 degree vertical field of view.
        assert!((lens.px_per_m(100.0) - 9.353).abs() < 0.01);
        // Up on screen is up in the world (no roll): a higher point has a smaller y.
        let level = Lens::look(Vec3::ZERO, Vec3::NEG_Z, 0.0, 60.0, [1920.0, 1080.0]);
        assert!(level.project(Vec3::new(0.0, 5.0, -50.0)).unwrap()[1] < 540.0);
        assert!(level.project(Vec3::new(5.0, 0.0, -50.0)).unwrap()[0] > 960.0);
        assert!(level.project(Vec3::new(0.0, 0.0, 50.0)).is_none());
        // Degenerate input still gives a usable lens.
        let bad = Lens::look(Vec3::splat(f32::NAN), Vec3::ZERO, f32::NAN, f32::NAN, [0.0, f32::NAN]);
        assert!(bad.forward.is_finite() && bad.aspect.is_finite() && bad.ray([1.0, 1.0]).is_finite());
        let down = Lens::look(Vec3::new(0.0, 50.0, 0.0), Vec3::ZERO, 0.0, 60.0, [1920.0, 1080.0]);
        assert!(down.right.is_finite() && down.up.is_finite() && (down.forward + Vec3::Y).length() < 1e-5);
    }

    #[test]
    fn markers_off_screen_sit_on_the_edge_they_would_enter_from() {
        let lens = Lens::look(Vec3::ZERO, Vec3::NEG_Z, 0.0, 60.0, [1920.0, 1080.0]);
        let (s, on) = lens.project_clamped(Vec3::new(1.0, 0.5, -50.0), 24.0);
        assert!(on && (s[0] - 960.0).abs() < 40.0);
        // Far to the right in front: right edge.
        let (s, on) = lens.project_clamped(Vec3::new(500.0, 0.0, -50.0), 24.0);
        assert!(!on && (s[0] - (1920.0 - 24.0)).abs() < 0.01 && (s[1] - 540.0).abs() < 1.0);
        // Behind and to the left: left edge. Directly behind and below: bottom edge.
        let (s, on) = lens.project_clamped(Vec3::new(-30.0, 0.0, 80.0), 24.0);
        assert!(!on && (s[0] - 24.0).abs() < 0.01);
        let (s, on) = lens.project_clamped(Vec3::new(0.0, -10.0, 80.0), 24.0);
        assert!(!on && (s[1] - (1080.0 - 24.0)).abs() < 0.01);
        // Exactly behind: somewhere on the edge, never NaN.
        let (s, on) = lens.project_clamped(Vec3::new(0.0, 0.0, 80.0), 24.0);
        assert!(!on && s.iter().all(|v| v.is_finite()));
        assert!(!lens.project_clamped(Vec3::splat(f32::NAN), 24.0).1);
    }

    #[test]
    fn box_and_point_rays() {
        let (min, max) = (Vec3::new(-1.0, 0.0, -10.0), Vec3::new(1.0, 2.0, -8.0));
        assert!((ray_box(Vec3::new(0.0, 1.0, 0.0), Vec3::NEG_Z, min, max).unwrap() - 8.0).abs() < 1e-5);
        assert!(ray_box(Vec3::new(5.0, 1.0, 0.0), Vec3::NEG_Z, min, max).is_none());
        assert!(ray_box(Vec3::new(0.0, 1.0, 0.0), Vec3::Z, min, max).is_none());
        assert_eq!(ray_box(Vec3::new(0.0, 1.0, -9.0), Vec3::X, min, max), Some(0.0));
        let (t, miss) = ray_point(Vec3::ZERO, Vec3::NEG_Z, Vec3::new(3.0, 0.0, -20.0)).unwrap();
        assert!((t - 20.0).abs() < 1e-5 && (miss - 3.0).abs() < 1e-5);
        assert!(ray_point(Vec3::ZERO, Vec3::NEG_Z, Vec3::new(0.0, 0.0, 20.0)).is_none());
    }

    #[test]
    fn the_camera_stays_a_metre_above_sea_and_land() {
        let mut rig = Rig::default();
        let focus = Focus::point(DVec3::new(1_000.0, 3.0, -2_000.0), Vec3::Y);
        // Pitched below the horizontal at long range: the orbit would put the camera under water.
        let orbit = Orbit { yaw_deg: 0.0, pitch_deg: -10.0, distance_m: 300.0, follow: None };
        let swell = |x: f64, _z: f64| 2.0 + (x * 0.01).sin();
        let p = rig.frame(&orbit, None, Some(focus), 0.016, 1.0, 1.0, &swell, &deep);
        assert!(p.pos.y >= swell(p.pos.x, p.pos.z) + CAMERA_FLOOR_M as f64 - 1e-9, "{}", p.pos.y);
        assert_eq!(p.target, focus.centre);
        // Land counts too.
        let hill = |_: f64, _: f64| 40.0;
        let p = rig.frame(&orbit, None, Some(focus), 0.016, 1.0, 1.0, &flat, &hill);
        assert!(p.pos.y >= 41.0 - 1e-9);
        // A high orbit is left alone: the camera is where the orbit says.
        let orbit = Orbit { yaw_deg: 90.0, pitch_deg: 30.0, distance_m: 100.0, follow: None };
        let p = rig.frame(&orbit, None, Some(focus), 0.016, 0.0, 0.0, &flat, &deep);
        assert!(((p.pos - p.target).length() - 100.0).abs() < 1e-3);
        assert!(p.pos.x < p.target.x && (p.pos.z - p.target.z).abs() < 1e-3);
        // The free camera obeys the floor as well, and bad free input falls back to the orbit.
        let p = rig.frame(&orbit, Some(([0.0, -5.0, 0.0], [10.0, 0.0, 0.0])), None, 0.016, 1.0, 1.0, &flat, &deep);
        assert_eq!(p.pos, DVec3::new(0.0, 1.0, 0.0));
        let p = rig.frame(&orbit, Some(([f64::NAN, 5.0, 0.0], [10.0, 0.0, 0.0])), None, 0.016, 1.0, 1.0, &flat, &deep);
        assert!(p.pos.is_finite());
    }

    #[test]
    fn the_focus_height_lags_and_the_target_is_held_when_nothing_is_followed() {
        let mut rig = Rig::default();
        let orbit = Orbit::default();
        let at = |y: f64| Focus::point(DVec3::new(0.0, y, 0.0), Vec3::Y);
        rig.frame(&orbit, None, Some(at(3.0)), 0.016, 0.0, 0.0, &flat, &deep);
        // The hull heaves 2 m in one frame: the target follows a little, not all the way.
        let p = rig.frame(&orbit, None, Some(at(5.0)), 0.016, 0.0, 0.0, &flat, &deep);
        assert!(p.target.y > 3.0 && p.target.y < 3.1, "{}", p.target.y);
        // The hull is gone (sunk, out of sight): the camera keeps its last point.
        let q = rig.frame(&orbit, None, None, 0.016, 0.0, 0.0, &flat, &deep);
        assert_eq!(q.target, p.target);
        // A jump to another ship far away snaps.
        let far = Focus::point(DVec3::new(9_000.0, 7.0, 0.0), Vec3::Y);
        assert_eq!(rig.frame(&orbit, None, Some(far), 0.016, 0.0, 0.0, &flat, &deep).target, far.centre);
    }

    #[test]
    fn shake_and_roll_follow_their_sliders() {
        let mut rig = Rig::default();
        let orbit = Orbit { yaw_deg: 0.0, pitch_deg: 20.0, distance_m: 100.0, follow: None };
        // Hull heeled 20 degrees to starboard while heading east; the camera looks north at its beam.
        let heel = Focus::point(DVec3::ZERO, Vec3::new(0.0, 20f32.to_radians().cos(), 20f32.to_radians().sin()));
        let level = Focus::point(DVec3::ZERO, Vec3::Y);
        // The camera looks along the ship's roll axis from astern to see heel: yaw 90 with the ship heading east.
        let astern = Orbit { yaw_deg: 90.0, ..orbit };
        let mut roll = 0.0;
        for _ in 0..600 { roll = rig.frame(&astern, None, Some(heel), 0.016, 0.0, 1.0, &flat, &deep).roll_rad; }
        assert!(roll.abs() > 0.03 && roll.abs() <= ROLL_MAX_RAD + 1e-6, "{roll}");
        // Slider at zero: no roll, whatever the hull does.
        let mut rig0 = Rig::default();
        for _ in 0..600 { roll = rig0.frame(&astern, None, Some(heel), 0.016, 0.0, 0.0, &flat, &deep).roll_rad; }
        assert_eq!(roll, 0.0);
        // Shake moves the camera, dies away, and is nothing at slider zero.
        let mut rig = Rig::default();
        let rest = rig.frame(&orbit, None, Some(level), 0.016, 1.0, 0.0, &flat, &deep).pos;
        rig.shake.add(1.0);
        let mut most = 0.0f64;
        for _ in 0..30 { most = most.max((rig.frame(&orbit, None, Some(level), 0.016, 1.0, 0.0, &flat, &deep).pos - rest).length()); }
        assert!(most > 0.02 && most <= (SHAKE_MAX_M as f64) * 1.5, "{most}");
        for _ in 0..200 { rig.frame(&orbit, None, Some(level), 0.016, 1.0, 0.0, &flat, &deep); }
        assert_eq!(rig.shake.trauma, 0.0);
        assert_eq!(rig.frame(&orbit, None, Some(level), 0.016, 1.0, 0.0, &flat, &deep).pos, rest);
        rig.shake.add(1.0);
        assert_eq!(rig.frame(&orbit, None, Some(level), 0.016, 0.0, 0.0, &flat, &deep).pos, rest);
        rig.shake.add(f32::NAN);
        assert!(rig.shake.trauma.is_finite());
    }
    #[test]
    fn the_orbit_never_enters_the_followed_hull() {
        // A 120 m hull heading east, 30 m from keel to mast top, 14 m in the beam.
        let hull = Focus { centre: DVec3::new(500.0, 6.0, -200.0), up: Vec3::Y, bow: Vec3::X, half_m: Vec3::new(60.0, 15.0, 7.0) };
        let outside = |p: DVec3| { let l = (p - hull.centre).as_vec3(); l.x.abs() > 60.0 || l.y.abs() > 15.0 || l.z.abs() > 7.0 };
        // Zoomed right in from every bearing and pitch: the eye is outside the box, by the clearance along
        // its line of sight.
        for yaw in (0..360).step_by(15) {
            for pitch in [-10.0f32, 0.0, 12.0, 45.0, 85.0] {
                let orbit = Orbit { yaw_deg: yaw as f32, pitch_deg: pitch, distance_m: ORBIT_MIN_DISTANCE_M, follow: None };
                let mut rig = Rig::default();
                let p = rig.frame(&orbit, None, Some(hull), 0.016, 0.0, 0.0, &deep, &deep);
                assert!(outside(p.pos), "yaw {yaw} pitch {pitch}: the camera is inside the hull at {:?}", p.pos - hull.centre);
                let pulled = p.pos + (p.target - p.pos).normalize() * (HULL_CLEAR_M as f64 - 0.01);
                assert!(outside(pulled), "yaw {yaw} pitch {pitch}: less than the clearance from the hull");
            }
        }
        // From the beam the camera may come close (half the beam and the clearance); from ahead it stays off
        // the bow. Looking north at an east-heading hull is a beam view.
        let beam = clear_of_hull(orbit_offset(&Orbit { yaw_deg: 0.0, pitch_deg: 0.0, distance_m: 8.0, follow: None }), &hull);
        assert!((beam.length() - 11.0).abs() < 1e-3, "{}", beam.length());
        let ahead = clear_of_hull(orbit_offset(&Orbit { yaw_deg: 270.0, pitch_deg: 0.0, distance_m: 8.0, follow: None }), &hull);
        assert!((ahead.length() - 64.0).abs() < 1e-3 && ahead.x > 0.0, "{ahead:?}");
        // A camera already outside is left where the orbit puts it, and heel does not move the box.
        let far = orbit_offset(&Orbit { yaw_deg: 40.0, pitch_deg: 20.0, distance_m: 300.0, follow: None });
        assert_eq!(clear_of_hull(far, &hull), far);
        let heeled = Focus { up: Vec3::new(0.0, 0.94, 0.34), bow: Vec3::new(0.98, 0.17, 0.0), ..hull };
        assert!((clear_of_hull(beam, &heeled) - beam).length() < 1e-3);
        // No box, a hull standing on end, nonsense: the orbit's own offset, never NaN.
        let o = orbit_offset(&Orbit::default());
        assert_eq!(clear_of_hull(o, &Focus::point(DVec3::ZERO, Vec3::Y)), o);
        assert!(clear_of_hull(o, &Focus { bow: Vec3::Y, ..hull }).is_finite());
        assert_eq!(clear_of_hull(o, &Focus { half_m: Vec3::splat(f32::NAN), ..hull }), o);
        assert_eq!(clear_of_hull(Vec3::ZERO, &hull), Vec3::ZERO);
        // The wheel works from where the camera stands. Zoomed right in from ahead it is held 64 m out;
        // one step back moves it at once, and steps in do not pile up behind the box.
        let mut orbit = Orbit { yaw_deg: 270.0, pitch_deg: 0.0, distance_m: ORBIT_MIN_DISTANCE_M, follow: None };
        let mut rig = Rig::default();
        rig.frame(&orbit, None, Some(hull), 0.016, 0.0, 0.0, &deep, &deep);
        assert!((rig.stood_off_m - 64.0).abs() < 1e-3);
        for _ in 0..10 { zoom_by(&mut orbit, 1.0, rig.stood_off_m); rig.frame(&orbit, None, Some(hull), 0.016, 0.0, 0.0, &deep, &deep); }
        assert!(orbit.distance_m > 64.0 * ZOOM_STEP - 1e-3 && (rig.stood_off_m - 64.0).abs() < 1e-3);
        zoom_by(&mut orbit, -1.0, rig.stood_off_m);
        rig.frame(&orbit, None, Some(hull), 0.016, 0.0, 0.0, &deep, &deep);
        assert!((rig.stood_off_m - 64.0 / ZOOM_STEP).abs() < 1e-2, "{}", rig.stood_off_m);
    }

    #[test]
    fn a_sortie_opens_with_the_hull_framed() {
        // The hull's length across 45% of a 1080p view: 0.45 x 1920 px = 864 px.
        let lens = Lens::look(Vec3::ZERO, Vec3::NEG_Z, 0.0, FOV_Y_DEG, [1920.0, 1080.0]);
        for length in [40.0f32, 120.0, 250.0] {
            let d = framing_distance(Vec3::new(length, 20.0, 12.0));
            assert!((length * lens.px_per_m(d) - 864.0).abs() < 2.0, "{length} m hull at {d} m");
        }
        // A launch is not framed so tightly that the sea is lost; a tall rig counts; nonsense is the default.
        assert_eq!(framing_distance(Vec3::new(8.0, 3.0, 3.0)), FRAMING_MIN_M);
        assert_eq!(framing_distance(Vec3::new(5_000.0, 3.0, 3.0)), FRAMING_MAX_M);
        assert!(framing_distance(Vec3::new(20.0, 60.0, 8.0)) > framing_distance(Vec3::new(20.0, 10.0, 8.0)));
        assert_eq!(framing_distance(Vec3::splat(f32::NAN)), Orbit::default().distance_m);
        assert_eq!(framing_distance(Vec3::ZERO), Orbit::default().distance_m);
    }

    #[test]
    fn shakes_wait_for_playback_to_reach_their_event() {
        let mut q = ShakeQueue::default();
        // Playback is at tick 600; a hit for tick 608 and own gunfire for tick 606 arrive together.
        q.push(608, 0.35);
        q.push(606, 0.08);
        q.push(606, 0.0);
        q.push(606, f32::NAN);
        assert_eq!(q.len(), 2);
        assert_eq!(q.take_due(600.0), 0.0);
        assert_eq!(q.take_due(605.9), 0.0);
        assert!((q.take_due(606.0) - 0.08).abs() < 1e-6);
        assert!((q.take_due(608.4) - 0.35).abs() < 1e-6);
        assert_eq!((q.take_due(700.0), q.len()), (0.0, 0));
        // An event already behind playback, and one too far ahead to wait for, are felt at once.
        q.push(500, 0.2);
        q.push(600 + SHAKE_HOLD_MAX_TICKS as u64 + 1, 0.3);
        assert!((q.take_due(600.0) - 0.5).abs() < 1e-6);
        // A playback tick that is not a number holds nothing, and the queue cannot grow without bound.
        q.push(610, 0.4);
        assert!((q.take_due(f64::NAN) - 0.4).abs() < 1e-6);
        for i in 0..(SHAKE_HOLD_MAX as u64 + 50) { q.push(1_000 + i % 20, 0.01); }
        assert_eq!(q.len(), SHAKE_HOLD_MAX);
        q.clear();
        assert_eq!(q.len(), 0);
    }
}
