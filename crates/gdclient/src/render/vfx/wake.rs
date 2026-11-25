//! Part of unit `gd-render-vfx`: the wake ribbons of the hulls (DESIGN.md L1967: a ribbon 1.0 to 2.5 beams
//! wide for 30 s; Kelvin arms at 19.47 degrees above 8 kn; bow wave by speed), kept as one track per hull
//! and drawn by the sea's own material (`res://shaders/water.gdshader`), so a wake lights and moves with the
//! waves instead of lying on them as a white strip. The bow wave stays a VFX sheet against the hull.
//!
//! A track is the stern's path: a point every [`TRACK_POINT_S`] while the hull makes way, back
//! [`hull_mesh::vfx::runtime::WAKE_LIFETIME_S`], in render space (moved with the origin). [`WakeField`] packs
//! the tracks nearest the camera for the water material: per track the bow, beam, length, heading, odometer
//! and the Kelvin arms' bounds, and sixteen points newest first (the bow, the stern now, then the laid points), each with the
//! water's age and its distance along the track. The shader takes the hull's speed from the points, so a
//! stopped hull's track fades out on its own and draws nothing new. Godot-free; tested headless.
use glam::Vec3;
use hull_mesh::vfx::runtime::{WAKE_FULL_KN, WAKE_LIFETIME_S};
use std::collections::VecDeque;

/// Seconds between the points laid on a moving hull's track: 14 points cover the ribbon's 30 s and a little more.
pub(crate) const TRACK_POINT_S: f64 = 2.31;
/// Tracks the water material draws at once (`wake_count`), nearest the camera first. The shader finds the
/// nearest track per vertex, so this costs little; only the first [`MAX_KELVIN`] draw Kelvin arms (per pixel).
pub(crate) const MAX_WAKES: usize = 8;
pub(crate) const MAX_KELVIN: usize = 4;
/// Points per track in the material (`wake_points`): the bow, the stern now and 14 laid points.
pub(crate) const WAKE_POINTS: usize = 16;
/// Bounds that hold nothing (min over max).
const EMPTY_BOX: [f32; 4] = [1.0, 1.0, -1.0, -1.0];
/// Tracks farther than this from the camera are not drawn (their wakes are under a pixel).
pub(crate) const WAKE_DRAW_M: f32 = 3000.0;
/// The odometer wraps here, so the shader's foam streaks keep their precision (one jump every 4 km).
pub(crate) const ODOMETER_WRAP_M: f64 = 4096.0;
/// Marks the end of a track in the material.
pub(crate) const END_AGE: f32 = 1.0e9;
/// Kelvin arms reach `len x (KELVIN_REACH.0 + KELVIN_REACH.1 x speed factor)` aft of the bow
/// (`water.gdshader`'s `reach`).
const KELVIN_REACH: (f32, f32) = (1.5, 4.5);
/// tan 19.47 degrees, the Kelvin half-angle (`hull_mesh::vfx::runtime::KELVIN_ANGLE_DEG`).
const KELVIN_TAN: f32 = 0.353_553;
/// Below this speed (8 kn, `hull_mesh::vfx::runtime::KELVIN_MIN_KN`) a hull makes no Kelvin arms.
const KELVIN_MIN_MS: f32 = 4.12;

/// Where a hull is this frame, for [`WakeTrack::step`]: stern and bow on the waterline (render space), speed
/// over the water in m/s, beam and waterline length in metres.
#[derive(Clone, Copy, Debug)]
pub(crate) struct WakeHull { pub stern: Vec3, pub bow: Vec3, pub speed_ms: f32, pub beam_m: f32, pub length_m: f32 }

/// One hull's wake.
#[derive(Clone, Debug, Default)]
pub(crate) struct WakeTrack {
    /// Laid points, newest first: stern position (render space, y 0), sim time laid, odometer then.
    points: VecDeque<(Vec3, f64, f64)>,
    /// The hull now: stern, bow, speed in m/s; `None` when it lays no wake (sunk or not yet seen).
    head: Option<(Vec3, Vec3, f32)>,
    /// Metres the stern has moved, wrapped at [`ODOMETER_WRAP_M`].
    odometer: f64,
    beam_m: f32,
    length_m: f32,
    /// Sim time of the newest point laid.
    last_s: f64,
}

impl WakeTrack {
    /// Follow the hull this frame. `moving` is the VFX's own test (making way, afloat); a hull that is not
    /// moving lays no point but keeps its old water ageing.
    pub(crate) fn step(&mut self, hull: WakeHull, moving: bool, sunk: bool, now: f64) {
        let WakeHull { stern, bow, speed_ms, beam_m, length_m } = hull;
        let stern = Vec3::new(stern.x, 0.0, stern.z);
        let bow = Vec3::new(bow.x, 0.0, bow.z);
        if !stern.is_finite() || !bow.is_finite() || sunk {
            self.head = None;
        } else {
            if let Some((last, _, _)) = self.head {
                let d = (stern - last).length() as f64;
                // A jump (a respawn or a rewind) is not travel.
                if d < 200.0 { self.odometer = (self.odometer + d) % ODOMETER_WRAP_M; }
            }
            self.head = Some((stern, bow, if speed_ms.is_finite() { speed_ms.max(0.0) } else { 0.0 }));
            self.beam_m = beam_m.max(0.5);
            self.length_m = length_m.max(1.0);
            if moving && (self.points.is_empty() || now - self.last_s >= TRACK_POINT_S || now < self.last_s) {
                self.points.push_front((stern, now, self.odometer));
                self.last_s = now;
            }
        }
        // Water older than the ribbon's life is gone; keep one point past it so the track's end fades rather than cuts.
        while self.points.len() > 1 && self.points.get(self.points.len() - 2).is_some_and(|p| now - p.1 > WAKE_LIFETIME_S as f64) { self.points.pop_back(); }
        if self.points.back().is_some_and(|p| now - p.1 > WAKE_LIFETIME_S as f64 + TRACK_POINT_S) { self.points.pop_back(); }
        while self.points.len() > WAKE_POINTS - 2 { self.points.pop_back(); }
    }

    pub(crate) fn rebase(&mut self, d: Vec3) {
        for p in &mut self.points { p.0 += Vec3::new(d.x, 0.0, d.z); }
        if let Some((s, b, _)) = &mut self.head { *s += Vec3::new(d.x, 0.0, d.z); *b += Vec3::new(d.x, 0.0, d.z); }
    }

    /// Laid points (for tests and the check harness).
    pub(crate) fn laid(&self) -> usize { self.points.len() }
    pub(crate) fn head(&self) -> Option<Vec3> { self.head.map(|h| h.0) }
    /// Whether anything of this wake is still drawn.
    pub(crate) fn visible(&self) -> bool { self.head.is_some() || !self.points.is_empty() }
}

/// The water material's wake inputs (`wake_count`, `wake_info`, `wake_points`), render space.
#[derive(Clone, Debug, PartialEq)]
pub struct WakeField {
    pub count: usize,
    pub info: [[f32; 4]; 3 * MAX_WAKES],
    pub points: [[f32; 4]; WAKE_POINTS * MAX_WAKES],
}

impl Default for WakeField {
    fn default() -> Self { Self { count: 0, info: [[0.0; 4]; 3 * MAX_WAKES], points: [[0.0, 0.0, END_AGE, 0.0]; WAKE_POINTS * MAX_WAKES] } }
}

impl WakeField {
    /// Pack the tracks nearest `camera` (render space) at sim time `now`. `kelvin` false (degrade step 3 and
    /// up) draws no Kelvin arms.
    pub(crate) fn pack<'a>(tracks: impl Iterator<Item = &'a WakeTrack>, camera: Vec3, now: f64, kelvin: bool) -> Self {
        let mut near: Vec<(f32, &WakeTrack)> = tracks
            .filter(|t| t.visible())
            .filter_map(|t| {
                let at = t.head.map(|h| h.0).or_else(|| t.points.front().map(|p| p.0))?;
                let d = Vec3::new(at.x - camera.x, 0.0, at.z - camera.z).length();
                (d.is_finite() && d < WAKE_DRAW_M).then_some((d, t))
            })
            .collect();
        near.sort_by(|a, b| a.0.total_cmp(&b.0));
        let mut field = Self::default();
        for (w, (_, t)) in near.into_iter().take(MAX_WAKES).enumerate() {
            let (stern, bow, speed) = match t.head {
                Some(h) => h,
                // A sunk hull: its old water only, from the newest point.
                None => { let p = t.points.front().map(|p| p.0).unwrap_or(Vec3::ZERO); (p, p, 0.0) }
            };
            let len = t.length_m;
            let mut pts: Vec<[f32; 4]> = Vec::with_capacity(WAKE_POINTS);
            // The bow: the water alongside the hull, reached before the stern (negative age and distance).
            pts.push([bow.x, bow.z, -len / speed.max(0.05), -len]);
            pts.push([stern.x, stern.z, 0.0, 0.0]);
            let mut along = 0.0f32;
            let mut prev = stern;
            for p in &t.points {
                let step = (p.0 - prev).length();
                if step < 0.05 && pts.len() == 2 { continue; }
                along += step;
                prev = p.0;
                pts.push([p.0.x, p.0.z, ((now - p.1) as f32).max(1.0e-3), along]);
                if pts.len() == WAKE_POINTS { break; }
            }
            // The shader finds the nearest track per vertex. The Kelvin arms reach only a few hull lengths,
            // where the track is the hull's line: they are drawn per pixel from the bow along the heading,
            // inside their own bounds.
            let sf = (speed / (WAKE_FULL_KN * 0.514_444)).clamp(0.0, 1.0);
            let reach = len * (KELVIN_REACH.0 + KELVIN_REACH.1 * sf);
            let fwd = Vec3::new(bow.x - stern.x, 0.0, bow.z - stern.z).normalize_or_zero();
            let arms = kelvin && w < MAX_KELVIN && speed > KELVIN_MIN_MS && fwd != Vec3::ZERO;
            for (k, p) in pts.iter().enumerate() { field.points[w * WAKE_POINTS + k] = *p; }
            field.info[3 * w] = [bow.x, bow.z, t.beam_m, len];
            field.info[3 * w + 1] = [t.odometer as f32, if arms { speed } else { 0.0 }, fwd.x, fwd.z];
            field.info[3 * w + 2] = if arms {
                // The wedge from the bow's shoulders back to `reach`, grown by the crest's width there.
                let right = Vec3::new(-fwd.z, 0.0, fwd.x);
                let half = 0.5 * t.beam_m + reach * KELVIN_TAN;
                let aft = bow - fwd * reach;
                let grow = 3.0 * (0.8 + 0.02 * reach) + 1.0;
                let corners = [bow + right * (0.5 * t.beam_m), bow - right * (0.5 * t.beam_m), aft + right * half, aft - right * half];
                let (lo, hi) = corners.iter().fold((Vec3::splat(f32::INFINITY), Vec3::splat(f32::NEG_INFINITY)), |(lo, hi), c| (lo.min(*c), hi.max(*c)));
                [lo.x - grow, lo.z - grow, hi.x + grow, hi.z + grow]
            } else { EMPTY_BOX };
            field.count = w + 1;
        }
        field
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(track: &mut WakeTrack, from_s: f64, to_s: f64, speed: f32, z0: f32) -> f32 {
        // Heading north (-Z) at `speed` m/s, 60 frames a second; a 40 m hull, 8 m beam.
        let mut z = z0;
        let mut t = from_s;
        while t <= to_s {
            let stern = Vec3::new(0.0, 0.0, z);
            track.step(WakeHull { stern, bow: stern + Vec3::new(0.0, 0.0, -40.0), speed_ms: speed, beam_m: 8.0, length_m: 40.0 }, speed > 0.26, false, t);
            z -= speed / 60.0;
            t += 1.0 / 60.0;
        }
        z
    }

    #[test]
    fn a_moving_hull_lays_points_and_old_water_goes() {
        let mut tr = WakeTrack::default();
        let z = run(&mut tr, 0.0, 10.0, 10.0, 0.0);
        assert!((5..=6).contains(&tr.laid()), "a point every 2.31 s over 10 s: {}", tr.laid());
        let f = WakeField::pack(std::iter::once(&tr), Vec3::new(0.0, 10.0, z), 10.0, true);
        assert_eq!(f.count, 1);
        // Bow, stern, then older water further aft (+Z) with growing age and distance.
        assert_eq!(f.points[1][2], 0.0);
        assert!(f.points[0][2] < 0.0 && f.points[0][3] == -40.0);
        for k in 2..WAKE_POINTS {
            if f.points[k][2] >= END_AGE { break; }
            assert!(f.points[k][2] > f.points[k - 1][2] && f.points[k][3] > f.points[k - 1][3] && f.points[k][1] > f.points[k - 1][1], "{k}: {:?}", f.points);
        }
        // The speed the shader reads from the points is the hull's.
        let (a, b) = (f.points[2], f.points[3]);
        assert!((((b[3] - a[3]) / (b[2] - a[2])) - 10.0).abs() < 0.2);
        // The Kelvin arms' wedge reaches well past the ribbon, and the arms carry the hull's speed and heading
        // (north, -Z).
        let (arms, bow_z) = (f.info[2], f.info[0][1]);
        assert!(arms[2] - arms[0] > 60.0 && arms[1] <= bow_z && arms[3] >= bow_z + 100.0, "from the bow aft (+Z): {arms:?}");
        assert!((f.info[1][1] - 10.0).abs() < 1e-3 && f.info[1][3] < -0.99, "{:?}", f.info[1]);
        // Stopped: no new points, and after the ribbon's life nothing is left but the hull.
        run(&mut tr, 10.0, 45.0, 0.0, z);
        assert_eq!(tr.laid(), 0);
        let f = WakeField::pack(std::iter::once(&tr), Vec3::ZERO, 45.0, true);
        assert_eq!(f.points[2][2], END_AGE);
    }

    #[test]
    fn the_ribbon_spans_thirty_seconds_at_most() {
        let mut tr = WakeTrack::default();
        run(&mut tr, 0.0, 60.0, 15.0, 0.0);
        let f = WakeField::pack(std::iter::once(&tr), Vec3::ZERO, 60.0, false);
        let last = (2..WAKE_POINTS).take_while(|k| f.points[*k][2] < END_AGE).last().unwrap();
        let age = f.points[last][2];
        assert!((WAKE_LIFETIME_S..=WAKE_LIFETIME_S + TRACK_POINT_S as f32 + 0.1).contains(&age), "{age}");
        assert_eq!(f.info[1][1], 0.0, "no Kelvin arms when degraded");
        assert!(f.info[2][0] > f.info[2][2]);
    }

    #[test]
    fn nearest_tracks_first_and_rebase_moves_them() {
        let mut a = WakeTrack::default();
        let mut b = WakeTrack::default();
        run(&mut a, 0.0, 3.0, 10.0, 0.0);
        run(&mut b, 0.0, 3.0, 10.0, 500.0);
        let f = WakeField::pack([&a, &b].into_iter(), Vec3::new(0.0, 0.0, 520.0), 3.0, true);
        assert_eq!(f.count, 2);
        assert!(f.points[1][1] > 400.0, "the camera's neighbour first");
        let far = WakeField::pack([&a].into_iter(), Vec3::new(0.0, 0.0, 9000.0), 3.0, true);
        assert_eq!(far.count, 0);
        let before = a.head().unwrap();
        a.rebase(Vec3::new(0.0, 0.0, -1000.0));
        assert_eq!(a.head().unwrap().z, before.z - 1000.0);
        // A sunk hull keeps its old water but lays none.
        let n = a.laid();
        a.step(WakeHull { stern: Vec3::ZERO, bow: Vec3::ZERO, speed_ms: 10.0, beam_m: 8.0, length_m: 40.0 }, false, true, 3.5);
        assert!(a.head().is_none() && a.laid() == n && a.visible());
    }
}
