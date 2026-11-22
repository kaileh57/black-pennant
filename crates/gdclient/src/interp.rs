//! Unit `gd-interp`. From the link's sampled frame to render space, and the floating render origin.
//! DESIGN.md L910-916 (remote vessels 100 to 150 ms in the past, decaying error offset, snap over 10 m),
//! L1088 and L1148 (f64 render origin snapped to 1 km, re-centred when the camera strays 4 km), L1831 (old
//! VFX anchors live 30 s past a rebase), L926 (guided missile state at 5 Hz, interpolated), L2004 (0.6 ms
//! shared with the link's decode and sample).
//!
//! # Split with `net`
//! Interpolation in time is not done here. `net::interp::SnapshotBuffer` (inside `net::ClientSession`, or
//! inside `link::channel::Presenter` for the in-process, replay and scripted links) keeps the snapshot
//! history, the 4-interval delay, the jitter-adaptive fallback, extrapolation and its gate counters, and
//! `net::interp::ShellTracker` flies shells. Each frame the link hands over one
//! [`RenderFrame`](crate::seams::RenderFrame): a whole `Snapshot` already at the playback tick. The delay and
//! its fallbacks are `net::interp::InterpTuning`, chosen when the link is built (`link::net::LinkOptions`).
//!
//! This unit does what needs the camera and the renderer's number format:
//! - f64 world poses to f32 render space about the [`RenderOrigin`], and the origin rebase;
//! - presentation prediction of what is under the player's hand (camera, held turret slew, reticle), blended
//!   back to the authoritative pose with `net::interp::CorrectionFilter` (150 ms half-life, snap over 10 m);
//! - carrying each track's estimate forward to the render tick by its velocity.
//!
//! Called once per frame by `BpSession::process`. Output is plain data; the render units, the plot, the HUD
//! markers and the audio feed all read the same [`ViewFrame`]. Nothing here touches Godot.
use crate::seams::ShellPose;
use content::ids::MunitionIdx;
use net::interp::CorrectionFilter;
use sim_core::projectiles::ProjectileId;
use sim_core::vessel::VesselStatus;
use sim_core::EntityId;
use sim_systems::ids::TrackId;
use sim_systems::protocol::{SeekerPhase, Snapshot, TurretView, VesselFlags};
use glam::{DQuat, DVec3};
use sim_core::math::Pose;
use sim_systems::protocol::Relevance;
use std::sync::Arc;

/// A sampled pose that differs from where the previous frame's pose would have carried it by more than
/// this is a correction (late snapshot, end of extrapolation, a split, a rejoin teleport); smaller
/// differences are ordinary motion and are drawn as sampled.
pub const CORRECTION_MIN_M: f64 = 0.05;
/// Angular counterpart of [`CORRECTION_MIN_M`], radians (about 0.5 degrees).
pub const CORRECTION_MIN_RAD: f64 = 0.0087;
/// A frame longer than this (window dragged, debugger, app suspended) breaks continuity: poses are drawn as
/// sampled with no correction.
pub const CONTINUITY_MAX_S: f64 = 0.5;
/// Fastest time compression (L47: 1x, 2x and 4x). One continuous frame advances simulation time by at most
/// this times [`CONTINUITY_MAX_S`]; a larger step (seek, rejoin) breaks continuity.
pub const MAX_TIME_RATE: f64 = 4.0;
/// Tracks are carried forward by their velocity at most this long past their last update; an older
/// estimate stays where it was rather than sailing off the plot.
pub const TRACK_CARRY_MAX_S: f64 = 30.0;

/// Origin rebase rule (L1088).
pub const ORIGIN_SNAP_M: f64 = 1_000.0;
pub const ORIGIN_REBASE_M: f64 = 4_000.0;

/// The f64 world position that render space is relative to. `y` is always 0.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct RenderOrigin { pub x: f64, pub z: f64 }
impl RenderOrigin {
    /// World metres to render space.
    pub fn to_render(self, world: glam::DVec3) -> [f32; 3] { [(world.x - self.x) as f32, world.y as f32, (world.z - self.z) as f32] }
    pub fn to_world(self, render: [f32; 3]) -> glam::DVec3 { glam::DVec3::new(render[0] as f64 + self.x, render[1] as f64, render[2] as f64 + self.z) }
}

/// The active camera in world space, written by the view nodes each frame.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct CameraState { pub pos: [f64; 3], pub forward: [f32; 3], pub up: [f32; 3], pub fov_y_deg: f32, pub viewport_px: [f32; 2], pub valid: bool }

#[derive(Clone, Debug, Default, PartialEq)]
pub struct VesselPose {
    pub entity: EntityId,
    pub pos: [f32; 3],
    /// Unit quaternion x, y, z, w.
    pub rot: [f32; 4],
    pub vel: [f32; 3],
    pub status: VesselStatus,
    pub flags: VesselFlags,
    pub track: TrackId,
    pub rudder: f32,
    pub power: f32,
    /// Range of this vessel's turrets in [`ViewFrame::turrets`].
    pub turrets: std::ops::Range<u32>,
    /// True when the buffer ran out and the pose is extrapolated (`RenderFrame::extrapolating`; gate: at
    /// most 1% of frames, L915).
    pub extrapolated: bool,
    pub own: bool,
}

/// One client-flown shell in render space (from [`ShellPose`]).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ShellView { pub id: ProjectileId, pub pos: [f32; 3], pub vel: [f32; 3] }

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BodyPose { pub entity: EntityId, pub kind: MunitionIdx, pub pos: [f32; 3], pub vel: [f32; 3], pub seeker: SeekerPhase, pub track: TrackId, pub own_side: bool }

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DecoyPose { pub entity: EntityId, pub kind: content::part::DecoyKind, pub pos: [f32; 3], pub age_s: f32 }

/// A track's estimate carried forward to render time by its velocity (plot and bridge-view markers).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TrackPose { pub id: TrackId, pub pos: [f32; 3], pub world_xz: [f64; 2] }

/// Everything the main thread draws this frame, in ascending entity order.
#[derive(Clone, Debug, Default)]
pub struct ViewFrame {
    /// Increments every frame a session is live.
    pub rev: u64,
    /// Playback position in simulation ticks (`RenderFrame::render_tick`, fractional) and in seconds; drives
    /// wave phases (L1889). Its floor is the client render tick stamped on commands (L949).
    pub render_tick: f64,
    pub sim_time_s: f64,
    pub origin: RenderOrigin,
    /// Set on the one frame the origin moved: add this to every render-space anchor kept across frames.
    pub origin_shift: Option<[f32; 3]>,
    pub vessels: Vec<VesselPose>,
    pub turrets: Vec<TurretView>,
    pub bodies: Vec<BodyPose>,
    pub decoys: Vec<DecoyPose>,
    /// Client-flown shells at the render tick, ascending id.
    pub shells: Vec<ShellView>,
    pub tracks: Vec<TrackPose>,
    /// Index of the viewer's own hull in `vessels`.
    pub own: Option<u32>,
    /// The sampled snapshot this view was built from (`RenderFrame::snapshot`): poses at the playback tick
    /// in world metres, slow sections as the link sampled them. The HUD's slow lists read this.
    pub snapshot: Option<Arc<Snapshot>>,
}

/// Builds each frame's [`ViewFrame`] and keeps what must persist between frames.
#[derive(Default)]
pub struct ViewBuilder {
    pub(crate) origin: RenderOrigin,
    /// Decaying offsets of presentation-predicted bodies (own hull, held turrets).
    pub(crate) corrections: CorrectionFilter,
    pub(crate) rev: u64,
    /// Whether `origin` has been placed for this session.
    pub(crate) origin_set: bool,
    /// Each vessel's sampled pose last frame (world frame), ascending entity: a jump in the sampled pose is
    /// measured against where this would have carried it.
    pub(crate) shown: Vec<ShownPose>,
    /// Playback tick of the previous frame; `None` until the first frame and after a break in continuity.
    pub(crate) last_render_tick: Option<f64>,
    /// Seconds until every registered correction has decayed below a millimetre; the filter is only
    /// consulted while this runs.
    pub(crate) correcting_s: f64,
}

/// One vessel as sampled: pose plus the velocities that carry it to the next frame.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct ShownPose { pub entity: EntityId, pub pos: DVec3, pub rot: DQuat, pub vel: DVec3, pub ang_vel: DVec3 }

impl ViewBuilder {
    pub fn new() -> Self { Self::default() }
    /// Forget everything (session ended, replay seek, rejoin).
    pub fn clear(&mut self) { *self = Self::default(); }
    /// Build this frame's view from the link's sample: convert vessel, body, decoy and shell poses to render
    /// space, carry tracks forward by their velocity to `frame.render_tick`, apply and decay correction
    /// offsets (`delta_s`), re-centre the origin against `camera` (reporting the shift), and reuse `out`'s
    /// allocations. `tuning` supplies the correction half-life and snap distance. Never interpolates between
    /// snapshots: `frame` is already at the playback tick.
    pub fn build(&mut self, frame: Arc<Snapshot>, render_tick: f64, extrapolating: bool, shells: &[ShellPose], delta_s: f64, tuning: &crate::seams::InterpTuning, camera: CameraState, out: &mut ViewFrame) {
        let render_tick = if render_tick.is_finite() { render_tick.max(0.0) } else { frame.tick as f64 };
        let delta_s = if delta_s.is_finite() { delta_s.max(0.0) } else { 0.0 };
        self.rev = self.rev.wrapping_add(1);
        out.rev = self.rev;
        out.render_tick = render_tick;
        out.sim_time_s = render_tick / sim_core::TICK_HZ as f64;

        // 1. Origin first, so every pose of this frame is relative to where it ends up (L1088, L1831).
        let anchor = if camera.valid && camera.pos.iter().all(|c| c.is_finite()) {
            Some((camera.pos[0], camera.pos[2]))
        } else {
            frame.vessels.iter().find(|v| v.relevance == Relevance::Own).or(frame.vessels.first()).map(|v| (v.kin.pose.pos.x, v.kin.pose.pos.z))
        };
        out.origin_shift = None;
        if let Some((ax, az)) = anchor.filter(|(x, z)| x.is_finite() && z.is_finite()) {
            let snapped = RenderOrigin { x: snap_km(ax), z: snap_km(az) };
            if !self.origin_set {
                self.origin = snapped;
                self.origin_set = true;
            } else if (ax - self.origin.x).hypot(az - self.origin.z) > ORIGIN_REBASE_M {
                let old = self.origin;
                self.origin = snapped;
                out.origin_shift = Some([(old.x - snapped.x) as f32, 0.0, (old.z - snapped.z) as f32]);
            }
        }
        out.origin = self.origin;
        let origin = self.origin;

        // 2. Corrections: decay what is registered (wall time), then measure each vessel's jump in the sampled
        // stream against where its previous sample would have carried it in simulation time (L912). Sim
        // time, not frame time: during a pause the sample stands still, and under 4x compression it moves
        // four times as far per frame; neither is a correction.
        let sim_dt = self.last_render_tick.map_or(-1.0, |last| (render_tick - last) / sim_core::TICK_HZ as f64);
        self.last_render_tick = Some(render_tick);
        let continuous = delta_s > 0.0 && delta_s <= CONTINUITY_MAX_S && (0.0..=CONTINUITY_MAX_S * MAX_TIME_RATE).contains(&sim_dt);
        if self.correcting_s > 0.0 {
            self.corrections.decay(delta_s, tuning);
            self.correcting_s -= delta_s;
            if self.correcting_s <= 0.0 { self.correcting_s = 0.0; }
        }
        let previous = std::mem::take(&mut self.shown);
        let mut prev_iter = previous.iter().peekable();
        out.vessels.clear();
        out.turrets.clear();
        out.own = None;
        for v in &frame.vessels {
            let sampled = Pose::new(v.kin.pose.pos, v.kin.pose.rot.normalize());
            if !finite_pose(&sampled) { continue; }
            // `previous` and `frame.vessels` both ascend by entity: walk them together.
            while prev_iter.peek().is_some_and(|p| p.entity < v.entity) { prev_iter.next(); }
            let before = prev_iter.peek().filter(|p| p.entity == v.entity).copied();
            if let (true, Some(p)) = (continuous, before) {
                let predicted = Pose::new(p.pos + p.vel * sim_dt, (DQuat::from_scaled_axis(p.ang_vel * sim_dt) * p.rot).normalize());
                let jump = (predicted.pos - sampled.pos).length();
                let turn = predicted.rot.angle_between(sampled.rot);
                if jump > CORRECTION_MIN_M || turn > CORRECTION_MIN_RAD {
                    // What would have been drawn without the jump: the carried sample plus the offset still
                    // decaying. The filter's new offset is that minus the new sample, so the hull does not move
                    // on this frame and the whole error then decays.
                    let shown = if self.correcting_s > 0.0 { self.corrections.apply(v.entity, &predicted) } else { predicted };
                    let shown = if finite_pose(&shown) { shown } else { predicted };
                    self.corrections.correct(v.entity, &shown, &sampled, tuning);
                    // Ten half-lives leave under 0.1% of the largest (10 m) offset.
                    self.correcting_s = self.correcting_s.max(10.0 * tuning.correction_half_life_s.max(0.0) + delta_s);
                }
            }
            let drawn = if self.correcting_s > 0.0 { self.corrections.apply(v.entity, &sampled) } else { sampled };
            let drawn = if finite_pose(&drawn) { drawn } else { sampled };
            // The sample, not the drawn pose, is what the next frame's jump test carries forward.
            self.shown.push(ShownPose { entity: v.entity, pos: sampled.pos, rot: sampled.rot, vel: finite_or_zero(v.kin.lin_vel), ang_vel: finite_or_zero(v.kin.ang_vel) });

            let first = out.turrets.len() as u32;
            out.turrets.extend(v.turrets.iter().copied());
            let own = v.relevance == Relevance::Own;
            if own { out.own = Some(out.vessels.len() as u32); }
            let q = drawn.rot;
            out.vessels.push(VesselPose {
                entity: v.entity,
                pos: origin.to_render(drawn.pos),
                rot: [q.x as f32, q.y as f32, q.z as f32, q.w as f32],
                vel: f3(finite_or_zero(v.kin.lin_vel)),
                status: v.status,
                flags: v.flags,
                track: v.track,
                rudder: if v.rudder.is_finite() { v.rudder.clamp(-1.0, 1.0) } else { 0.0 },
                power: if v.power.is_finite() { v.power.clamp(0.0, 1.0) } else { 0.0 },
                turrets: first..out.turrets.len() as u32,
                extrapolated: extrapolating,
                own,
            });
        }

        // 3. Small bodies (guided missiles arrive at 5 Hz and are already interpolated by the link, L926),
        // decoys and client-flown shells.
        out.bodies.clear();
        out.bodies.extend(frame.bodies.iter().filter(|b| finite3(b.pos)).map(|b| BodyPose {
            entity: b.entity,
            kind: b.kind,
            pos: origin.to_render(b.pos),
            vel: f3(finite_or_zero(b.vel)),
            seeker: b.seeker,
            track: b.track,
            // The snapshot marks own-side items by relevance (`Relevance::OwnSide` and above).
            own_side: b.relevance >= Relevance::OwnSide,
        }));
        out.decoys.clear();
        out.decoys.extend(frame.decoys.iter().filter(|d| finite3(d.pos)).map(|d| DecoyPose { entity: d.entity, kind: d.kind, pos: origin.to_render(d.pos), age_s: if d.age_s.is_finite() { d.age_s.max(0.0) } else { 0.0 } }));
        out.shells.clear();
        out.shells.extend(shells.iter().filter(|s| finite3(s.pos)).map(|s| ShellView { id: s.id, pos: origin.to_render(s.pos), vel: f3(finite_or_zero(s.vel)) }));

        // 4. Tracks carried forward from their last update to the render tick by their velocity.
        out.tracks.clear();
        out.tracks.extend(frame.tracks.iter().filter(|t| finite3(t.pos)).map(|t| {
            let world = carried_track_pos(t.pos, t.vel, t.last_update_tick, render_tick, t.last_known);
            TrackPose { id: t.id, pos: origin.to_render(world), world_xz: [world.x, world.z] }
        }));
        out.snapshot = Some(frame);
    }
    /// Clear `out` to an empty view that keeps the origin (outside a sortie, before the first frame).
    pub fn build_empty(&mut self, out: &mut ViewFrame) {
        out.origin = self.origin;
        out.origin_shift = None;
        out.vessels.clear();
        out.turrets.clear();
        out.bodies.clear();
        out.decoys.clear();
        out.shells.clear();
        out.tracks.clear();
        out.own = None;
        out.snapshot = None;
        // Nothing drawn now: the next sortie frame starts without continuity.
        self.shown.clear();
        self.last_render_tick = None;
    }
    pub fn origin(&self) -> RenderOrigin { self.origin }
    /// Largest correction offset in metres right now (own-hull 0.5 m gate, L933).
    pub fn max_correction_m(&self) -> f64 { self.corrections.max_offset_m() }
}

/// Nearest multiple of 1 km (L1088).
fn snap_km(v: f64) -> f64 { (v / ORIGIN_SNAP_M).round() * ORIGIN_SNAP_M }
fn finite3(v: DVec3) -> bool { v.is_finite() }
fn finite_or_zero(v: DVec3) -> DVec3 { if v.is_finite() { v } else { DVec3::ZERO } }
fn finite_pose(p: &Pose) -> bool { p.pos.is_finite() && p.rot.is_finite() && p.rot.length_squared() > 0.5 }
fn f3(v: DVec3) -> [f32; 3] { [v.x as f32, v.y as f32, v.z as f32] }

/// A track's estimate at `render_tick`: dead reckoning from its last update, held at most
/// [`TRACK_CARRY_MAX_S`]; last-known marks stay where they were dropped.
pub fn carried_track_pos(pos: DVec3, vel: DVec3, last_update_tick: u64, render_tick: f64, last_known: bool) -> DVec3 {
    if last_known || !vel.is_finite() { return pos; }
    let dt = ((render_tick - last_update_tick as f64) / sim_core::TICK_HZ as f64).clamp(0.0, TRACK_CARRY_MAX_S);
    pos + vel * dt
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn origin_round_trips_and_snaps() {
        let o = RenderOrigin { x: 12_000.0, z: -3_000.0 };
        let w = DVec3::new(12_345.25, 4.5, -2_999.5);
        let r = o.to_render(w);
        assert_eq!(r, [345.25, 4.5, 0.5]);
        assert!((o.to_world(r) - w).length() < 1e-9);
        assert_eq!(snap_km(4_499.0), 4_000.0);
        assert_eq!(snap_km(-1_501.0), -2_000.0);
    }

    fn vessel(id: u64, x: f64, z: f64, vx: f64, own: bool, turrets: usize) -> sim_systems::protocol::VesselView {
        use sim_core::math::Kinematics;
        sim_systems::protocol::VesselView {
            entity: EntityId::from_u64(id),
            kin: Kinematics { pose: Pose::new(DVec3::new(x, 0.5, z), DQuat::IDENTITY), lin_vel: DVec3::new(vx, 0.0, 0.0), ang_vel: DVec3::ZERO },
            status: VesselStatus::Active,
            flags: VesselFlags::empty(),
            turrets: (0..turrets).map(|i| TurretView { slot: content::ids::PartSlot(i as u16), train_rad: 0.1, elevation_rad: 0.0 }).collect(),
            rudder: 2.0,
            power: f32::NAN,
            track: TrackId::NONE,
            relevance: if own { Relevance::Own } else { Relevance::Near },
        }
    }

    #[test]
    fn view_frames_follow_the_camera_and_rebase() {
        let tuning = crate::seams::InterpTuning::default();
        let mut b = ViewBuilder::new();
        let mut out = ViewFrame::default();
        let snap = |x: f64| Arc::new(Snapshot { tick: 60, vessels: vec![vessel(1, x, 0.0, 6.0, false, 2), vessel(2, x + 100.0, 50.0, 6.0, true, 1)], ..Default::default() });
        let cam = |x: f64| CameraState { pos: [x, 20.0, 0.0], valid: true, ..Default::default() };
        b.build(snap(1_400.0), 60.0, false, &[], 1.0 / 60.0, &tuning, cam(1_400.0), &mut out);
        assert_eq!(out.origin, RenderOrigin { x: 1_000.0, z: 0.0 });
        assert_eq!(out.origin_shift, None);
        assert_eq!(out.own, Some(1));
        assert_eq!((out.vessels[0].turrets.clone(), out.vessels[1].turrets.clone()), (0..2, 2..3));
        assert_eq!(out.vessels[0].pos, [400.0, 0.5, 0.0]);
        assert_eq!((out.vessels[0].rudder, out.vessels[0].power), (1.0, 0.0));
        // Smooth motion: each frame moves by velocity times frame time, so nothing registers as a correction.
        let mut x = 1_400.0;
        for i in 1..=10 {
            x += 6.0 / 60.0;
            b.build(snap(x), 60.0 + i as f64, false, &[], 1.0 / 60.0, &tuning, cam(1_400.0), &mut out);
        }
        assert_eq!(b.correcting_s, 0.0);
        assert!((out.vessels[0].pos[0] as f64 - (x - 1_000.0)).abs() < 1e-3);
        // The camera strays past 4 km: the origin snaps to the nearest kilometre and reports the shift.
        x += 6.0 / 60.0;
        b.build(snap(x), 71.0, false, &[], 1.0 / 60.0, &tuning, cam(5_600.0), &mut out);
        assert_eq!(out.origin, RenderOrigin { x: 6_000.0, z: 0.0 });
        assert_eq!(out.origin_shift, Some([-5_000.0, 0.0, 0.0]));
        assert!((out.vessels[0].pos[0] as f64 - (x - 6_000.0)).abs() < 1e-3);
        x += 6.0 / 60.0;
        b.build(snap(x), 72.0, false, &[], 1.0 / 60.0, &tuning, cam(5_600.0), &mut out);
        assert_eq!(out.origin_shift, None);
        b.build_empty(&mut out);
        assert!(out.vessels.is_empty() && out.snapshot.is_none() && out.origin.x == 6_000.0);
    }

    fn one(x: f64, vx: f64) -> Arc<Snapshot> { Arc::new(Snapshot { tick: 60, vessels: vec![vessel(1, x, 0.0, vx, true, 0)], ..Default::default() }) }
    fn cam() -> CameraState { CameraState { pos: [0.0, 20.0, 0.0], valid: true, ..Default::default() } }

    #[test]
    fn pause_and_compression_are_not_corrections() {
        let tuning = crate::seams::InterpTuning::default();
        let mut b = ViewBuilder::new();
        let mut out = ViewFrame::default();
        // Paused: the same sample at the same tick every frame while the hull still reports 6 m/s.
        for _ in 0..30 { b.build(one(100.0, 6.0), 600.0, false, &[], 1.0 / 60.0, &tuning, cam(), &mut out); }
        assert_eq!(b.correcting_s, 0.0);
        assert_eq!(out.vessels[0].pos[0], 100.0);
        // 4x compression: four ticks and four times the distance per frame.
        let (mut x, mut tick) = (100.0, 600.0);
        for _ in 0..30 {
            x += 4.0 * 6.0 / 60.0;
            tick += 4.0;
            b.build(one(x, 6.0), tick, false, &[], 1.0 / 60.0, &tuning, cam(), &mut out);
        }
        assert_eq!(b.correcting_s, 0.0);
        // A seek far ahead breaks continuity rather than registering a correction.
        b.build(one(x + 500.0, 6.0), tick + 6_000.0, false, &[], 1.0 / 60.0, &tuning, cam(), &mut out);
        assert_eq!(b.correcting_s, 0.0);
        assert!((out.vessels[0].pos[0] as f64 - (x + 500.0)).abs() < 1e-3);
    }

    /// Needs `net::interp::CorrectionFilter` (unit `net-interp`); run with `--ignored` once it lands.
    #[test]
    #[ignore = "net::interp::CorrectionFilter is still a stub of unit net-interp"]
    fn a_jump_is_smoothed_then_decays_away() {
        let tuning = crate::seams::InterpTuning::default();
        let mut b = ViewBuilder::new();
        let mut out = ViewFrame::default();
        let (mut x, mut tick) = (100.0, 600.0);
        for _ in 0..5 { x += 0.1; tick += 1.0; b.build(one(x, 6.0), tick, false, &[], 1.0 / 60.0, &tuning, cam(), &mut out); }
        let before = out.vessels[0].pos[0] as f64;
        // A late snapshot moves the sample 2 m back: the drawn hull keeps going and the error decays.
        x += 0.1 - 2.0;
        tick += 1.0;
        b.build(one(x, 6.0), tick, false, &[], 1.0 / 60.0, &tuning, cam(), &mut out);
        assert!((out.vessels[0].pos[0] as f64 - (before + 0.1)).abs() < 0.01, "the jump shows on screen");
        assert!(b.max_correction_m() > 1.9);
        for _ in 0..120 { x += 0.1; tick += 1.0; b.build(one(x, 6.0), tick, false, &[], 1.0 / 60.0, &tuning, cam(), &mut out); }
        assert!(b.max_correction_m() < 0.01, "{}", b.max_correction_m());
        assert_eq!(b.correcting_s, 0.0);
        assert!((out.vessels[0].pos[0] as f64 - x).abs() < 0.01);
    }

    #[test]
    fn tracks_carry_forward_and_stop() {
        let p = DVec3::new(100.0, 0.0, 0.0);
        let v = DVec3::new(10.0, 0.0, -5.0);
        assert_eq!(carried_track_pos(p, v, 60, 120.0, false), DVec3::new(110.0, 0.0, -5.0));
        assert_eq!(carried_track_pos(p, v, 60, 120.0, true), p);
        // Never backwards, never past the carry limit.
        assert_eq!(carried_track_pos(p, v, 600, 120.0, false), p);
        assert_eq!(carried_track_pos(p, v, 0, 1.0e9, false), p + v * TRACK_CARRY_MAX_S);
    }
}
