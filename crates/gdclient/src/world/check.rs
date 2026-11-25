//! Part of unit `gd-world-view`, shared with `gd-render-ocean`. `BpWorldCheck`: a test-only class that plays
//! a scripted sortie into `Services::feed` and `Services::view`, the way `BpSession` will, so a
//! `BpWorldView` in the tree runs its real frame inside the engine (`res://test/tools/world_check.gd`).
//! The units' logic is tested headless in Rust; this covers what those tests cannot reach: the scene's
//! lifetime, the camera node, signals and their re-entry, the sea's materials and textures, the shader
//! clock against the engine's `TIME`. Not referenced by shipped scenes.
//!
//! The scene: two stock hulls on a sea-state 4 swell at dusk, the viewer's steaming east at 12 kn with the
//! other 900 m off its port bow and held as a track; a second track with no hull in sight; an island with a
//! beach inside the shore window. The weather turns at 4 s, the viewer takes its forward gun at 1 s and
//! fires it every second, and at 6 s the whole formation is moved 9 km so the render origin rebases.
#![allow(clippy::disallowed_methods)]
use super::BpWorldView;
use crate::enums::{SessionKind, SessionState};
use crate::interp::{RenderOrigin, TrackPose, VesselPose, ViewFrame, ORIGIN_REBASE_M, ORIGIN_SNAP_M};
use crate::seams::WireEvent;
use content::campaign::{Island, MapDef};
use content::enums::{Domain, IdRung, Stance};
use content::ids::{ContentHash, PartSlot, SideId};
use content::weather::{SeaState, WeatherKind, WeatherState};
use glam::{DQuat, DVec3};
use godot::prelude::*;
use sim_core::events::PhysEvent;
use sim_core::math::{Kinematics, Pose};
use sim_core::waves::WaveSet;
use sim_core::EntityId;
use sim_systems::ids::TrackId;
use sim_systems::protocol::{ControlRole, EntityIntro, Relevance, ShipStatus, SimEvent, Snapshot, TrackView, TurretView, UncertaintyShape, VesselFlags, VesselView};
use std::sync::Arc;

const OWN: u64 = 1;
const ENEMY: u64 = 2;
const ENEMY_TRACK: TrackId = TrackId(12);
const FAR_TRACK: TrackId = TrackId(20);
const WAVE_SEED: u64 = 0x5EA;
/// Where the scene starts on the map: off the origin, so every origin-relative path is exercised.
const START: [f64; 2] = [2_300.0, -5_200.0];
/// The move that forces an origin rebase, and the frame it happens on.
const JUMP_M: f64 = 9_000.0;
const JUMP_FRAME: u64 = 360;
const TURN_FRAME: u64 = 240;
const SPEED_MPS: f64 = 6.2;

struct Fleet { hash: ContentHash, waterline_m: f64, size_m: [f32; 3], gun: Option<PartSlot>, mounts: usize }

struct Run {
    fleet: Fleet,
    frame: u64,
    /// The render origin, once placed.
    origin: Option<RenderOrigin>,
    weather: WeatherState,
    waves: WaveSet,
}

/// Test-only driver of `BpWorldView`. Instantiate with `BpWorldCheck.new()`.
#[derive(GodotClass)]
#[class(base = RefCounted)]
pub struct BpWorldCheck {
    run: Option<Run>,
    base: Base<RefCounted>,
}

#[godot_api]
impl IRefCounted for BpWorldCheck {
    fn init(base: Base<RefCounted>) -> Self { Self { run: None, base } }
}

fn weather(turned: bool) -> WeatherState {
    let (kind, sea, cover) = if turned { (WeatherKind::Overcast, SeaState(5), 0.8) } else { (WeatherKind::Clear, SeaState(4), 0.35) };
    WeatherState { kind, sea_state: sea, wind_dir_rad: 1.9, wind_speed_mps: 9.0, cloud_cover: cover, visibility_m: kind.visibility_m(), rain: 0.0, sun_elevation_deg: 9.0, sun_azimuth_deg: 252.0 }
}

fn map() -> MapDef {
    MapDef {
        size_m: [40_000.0, 40_000.0], depth_m: 180.0,
        islands: vec![
            Island { centre_m: [START[0] + 500.0, START[1] - 1_300.0], radius_m: 420.0, height_m: 90.0, seed: 3 },
            Island { centre_m: [START[0] + JUMP_M + 2_500.0, START[1] + 1_800.0], radius_m: 900.0, height_m: 160.0, seed: 8 },
        ],
        markers: Vec::new(), seed: 21,
    }
}

/// A stock design with a gun, built in full: one that also carries a hull number if there is one.
fn fleet() -> Option<(Fleet, crate::worker::ReceivedDesign)> {
    let catalogue = content::catalogue::catalogue();
    let armed = |bp: &content::blueprint::Blueprint| bp.parts.iter().any(|p| catalogue.part(&p.part).is_some_and(|d| matches!(d.function, content::part::PartFunction::Gun(_))));
    let numbered = |bp: &content::blueprint::Blueprint| bp.markings.iter().any(|m| m.kind == content::blueprint::MarkingKind::HullNumber && !m.text.is_empty());
    let mut candidates: Vec<content::blueprint::Blueprint> = content::data::stock_blueprint_files().filter_map(|(file, text)| content::blueprint::io::load_blueprint(text, file).ok()).map(|l| l.blueprint).filter(|bp| armed(bp)).collect();
    candidates.sort_by_key(|bp| !numbered(bp));
    for blueprint in candidates {
        let blueprint = Arc::new(blueprint);
        let prefab = Arc::new(sim_core::vessel::build_prefab(&blueprint, catalogue));
        let design = Arc::new(sim_systems::ship::ShipDesign::build(&blueprint, prefab, catalogue, &content::ruleset::Ruleset::default()));
        let gun = design.mounts.iter().find(|m| m.muzzle_mps > 0.0).map(|m| m.slot);
        if gun.is_none() { continue; }
        let received = crate::worker::ReceivedDesign::Full { design: design.clone(), blueprint };
        let drawn = crate::render::hull::design::DrawDesign::from_received(&received, design.hash);
        let fleet = Fleet { hash: design.hash, waterline_m: drawn.waterline_m.unwrap_or(2.0) as f64, size_m: drawn.size_m(), gun, mounts: design.mounts.len() };
        return Some((fleet, received));
    }
    None
}

impl Run {
    fn base(&self) -> [f64; 2] { if self.frame >= JUMP_FRAME { [START[0] + JUMP_M, START[1]] } else { START } }
    fn time_s(&self) -> f64 { self.frame as f64 / sim_core::TICK_HZ as f64 }

    /// A hull afloat: its grid origin placed so the hull's middle is at `centre`, heading `heading_rad`,
    /// riding the wave under it.
    fn afloat(&self, centre: [f64; 2], heading_rad: f64) -> Pose {
        let t = self.time_s();
        let sample = self.waves.sample(centre[0], centre[1], t);
        let level = Pose::from_heading(DVec3::ZERO, heading_rad);
        // Lean with the surface, damped: a hull this size follows a share of the wave slope.
        let tilt = DQuat::from_rotation_arc(DVec3::Y, (DVec3::Y * 3.0 + sample.normal).normalize());
        let rot = (tilt * level.rot).normalize();
        let half = DVec3::new(self.fleet.size_m[0] as f64 * 0.5, self.fleet.waterline_m, self.fleet.size_m[2] as f64 * 0.5);
        Pose::new(DVec3::new(centre[0], sample.height, centre[1]) - rot * half, rot)
    }

    fn snapshot(&self) -> Snapshot {
        let t = self.time_s();
        let base = self.base();
        let own_centre = [base[0] + SPEED_MPS * t, base[1]];
        let enemy_centre = [base[0] + 600.0 + SPEED_MPS * t * 0.3, base[1] - 700.0];
        let east = std::f64::consts::FRAC_PI_2;
        let vessel = |entity: u64, centre: [f64; 2], heading: f64, speed: f64, track: TrackId, relevance: Relevance| VesselView {
            entity: EntityId::from_u64(entity),
            kin: Kinematics { pose: self.afloat(centre, heading), lin_vel: DVec3::new(heading.sin() * speed, 0.0, -heading.cos() * speed), ang_vel: DVec3::ZERO },
            status: Default::default(), flags: VesselFlags::default(),
            turrets: self.fleet.gun.map(|slot| TurretView { slot, train_rad: 0.6, elevation_rad: 0.1 }).into_iter().collect(),
            rudder: 0.0, power: 0.6, track, relevance,
        };
        let track = |id: TrackId, pos: DVec3, stance: Stance| TrackView {
            id, domain: Domain::Surface, stance, stance_reason: Default::default(), rung: IdRung::Type, rung_reason: Default::default(), grade: Default::default(),
            pos, vel: DVec3::ZERO, shape: UncertaintyShape::Circle { radius_m: 40.0 }, last_update_tick: self.frame, last_known: false, class: None, candidates: 0,
            sources: Default::default(), trend: Default::default(), inbound: false, engaged_by: 0, envelope_m: 0.0, emitting: false, relevance: Relevance::Near, struck: false,
        };
        let held = if self.frame >= 60 { self.fleet.gun.map(ControlRole::Mount).into_iter().collect() } else { Vec::new() };
        Snapshot {
            tick: self.frame,
            vessels: vec![vessel(OWN, own_centre, east, SPEED_MPS, TrackId::NONE, Relevance::Own), vessel(ENEMY, enemy_centre, east + 2.6, SPEED_MPS * 0.3, ENEMY_TRACK, Relevance::Near)],
            tracks: vec![
                track(ENEMY_TRACK, DVec3::new(enemy_centre[0] + 6.0, 0.0, enemy_centre[1] - 4.0), Stance::Hostile),
                track(FAR_TRACK, DVec3::new(base[0] - 3_000.0, 0.0, base[1] - 5_000.0), Stance::Suspect),
            ],
            ship: Some(ShipStatus { vessel: EntityId::from_u64(OWN), speed_kn: 12.0, designated: FAR_TRACK, held, longest_envelope_m: 8_000.0, ..Default::default() }),
            ..Default::default()
        }
    }

    /// This frame's view in render space. `interp::ViewBuilder` does this in play; the script is its own
    /// source of poses, so it only needs the conversion and the origin rule (snapped to 1 km, re-centred
    /// when the camera strays 4 km, L1088).
    fn view(&mut self, snapshot: Arc<Snapshot>, camera: crate::interp::CameraState) -> ViewFrame {
        let own = snapshot.vessels[0].kin.pose.pos;
        let anchor = if camera.valid { [camera.pos[0], camera.pos[2]] } else { [own.x, own.z] };
        let snapped = RenderOrigin { x: (anchor[0] / ORIGIN_SNAP_M).round() * ORIGIN_SNAP_M, z: (anchor[1] / ORIGIN_SNAP_M).round() * ORIGIN_SNAP_M };
        let mut shift = None;
        let origin = match self.origin {
            None => snapped,
            Some(o) if (anchor[0] - o.x).hypot(anchor[1] - o.z) > ORIGIN_REBASE_M => { shift = Some([(o.x - snapped.x) as f32, 0.0, (o.z - snapped.z) as f32]); snapped }
            Some(o) => o,
        };
        self.origin = Some(origin);
        let mut turrets = Vec::new();
        let vessels = snapshot.vessels.iter().enumerate().map(|(i, v)| {
            let start = turrets.len() as u32;
            turrets.extend(v.turrets.iter().copied());
            let (r, vel) = (v.kin.pose.rot, v.kin.lin_vel);
            VesselPose {
                entity: v.entity, pos: origin.to_render(v.kin.pose.pos), rot: [r.x as f32, r.y as f32, r.z as f32, r.w as f32], vel: [vel.x as f32, vel.y as f32, vel.z as f32],
                status: v.status, flags: v.flags, track: v.track, rudder: v.rudder, power: v.power, turrets: start..turrets.len() as u32, extrapolated: false, own: i == 0,
            }
        }).collect();
        let tracks = snapshot.tracks.iter().map(|t| TrackPose { id: t.id, pos: origin.to_render(t.pos), world_xz: [t.pos.x, t.pos.z] }).collect();
        ViewFrame {
            rev: self.frame, render_tick: self.frame as f64, sim_time_s: self.time_s(), origin, origin_shift: shift, vessels, turrets, bodies: Vec::new(), decoys: Vec::new(), shells: Vec::new(), tracks,
            own: Some(0), snapshot: Some(snapshot),
        }
    }

    fn events(&self) -> Vec<WireEvent> {
        let f = self.frame;
        let ev = |event: SimEvent| WireEvent { tick: f, event };
        let reveal = |entity: u64, own: bool| ev(SimEvent::EntityRevealed(Box::new(EntityIntro {
            entity: EntityId::from_u64(entity), side: SideId(if own { 0 } else { 1 }), blueprint: self.fleet.hash, exterior_only: false, class: None, name: String::new(), player: None,
            status: Default::default(), damage: Vec::new(), removed: Vec::new(), parent: None, track: if own { TrackId::NONE } else { ENEMY_TRACK },
        })));
        let mut out = Vec::new();
        if f == 1 {
            out.push(ev(SimEvent::SortieStarted { seed: 77, tick_hz: sim_core::TICK_HZ }));
            out.push(ev(SimEvent::Phys(PhysEvent::WeatherChanged { weather: weather(false), wave_seed: WAVE_SEED })));
            out.extend([reveal(OWN, true), reveal(ENEMY, false)]);
        }
        if f == TURN_FRAME { out.push(ev(SimEvent::Phys(PhysEvent::WeatherChanged { weather: weather(true), wave_seed: WAVE_SEED }))); }
        if let Some(slot) = self.fleet.gun {
            if f >= 60 && f.is_multiple_of(60) { out.push(ev(SimEvent::Phys(PhysEvent::MountFired { vessel: EntityId::from_u64(OWN), slot, rounds: 1 }))); }
        }
        out
    }
}

#[godot_api]
impl BpWorldCheck {
    /// Prepare the scripted sortie for `view`: build the stock design, register it, and hand over the map.
    /// Returns false when no stock design with a gun could be built. Call `finish` before freeing.
    #[func]
    fn start(&mut self, view: Gd<BpWorldView>) -> bool {
        self.finish();
        let Some((fleet, received)) = fleet() else { return false };
        let services = crate::services::services();
        services.designs.borrow_mut().insert(fleet.hash, received);
        let mut view = view;
        view.bind_mut().give_map(map());
        let weather = weather(false);
        let waves = WaveSet::generate(WAVE_SEED, &weather, &services.catalogue().tuning.waves);
        self.run = Some(Run { fleet, frame: 0, origin: None, weather, waves });
        true
    }
    /// Play one frame into the services, as `BpSession::process` would. Call before the view processes.
    #[func]
    fn step(&mut self, delta: f64) {
        let Some(run) = &mut self.run else { return };
        run.frame += 1;
        if run.frame == TURN_FRAME {
            run.weather = weather(true);
            run.waves = WaveSet::generate(WAVE_SEED, &run.weather, &crate::services::services().catalogue().tuning.waves);
        }
        let services = crate::services::services();
        let snapshot = Arc::new(run.snapshot());
        let events = run.events();
        {
            let mut feed = services.feed.borrow_mut();
            feed.frame = run.frame;
            feed.now_s += delta.max(0.0);
            feed.delta_s = delta;
            feed.state = SessionState::InSortie;
            feed.kind = SessionKind::Scripted;
            feed.events = events;
            feed.snapshot = Some(snapshot.clone());
            feed.snapshot_rev = run.frame;
        }
        let view = run.view(snapshot, services.camera.get());
        *services.view.borrow_mut() = view;
    }
    /// Put the effective quality profile on another tier (`Bp.QUALITY_*`) and have the view apply it, as
    /// `Quality.changed` would. Returns the clipmap levels the sea then has, or -1 without a scene.
    #[func]
    fn retier(&mut self, view: Gd<BpWorldView>, tier: i64) -> i64 {
        use crate::enums::GdEnum;
        let Some(tier) = crate::enums::QualityTier::from_gd(tier) else { return -1 };
        crate::services::services().quality.borrow_mut().tier = tier;
        let mut view = view;
        let mut v = view.bind_mut();
        v.apply_quality();
        v.scene.as_ref().map_or(-1, |s| s.ocean.spec.levels as i64)
    }
    /// Put the effective quality profile on another tier and tell nobody: the view must notice on its next
    /// frame and apply it, for a scene script that never passes `Quality.changed` on.
    #[func]
    fn set_tier_quietly(&mut self, tier: i64) -> bool {
        use crate::enums::GdEnum;
        let Some(tier) = crate::enums::QualityTier::from_gd(tier) else { return false };
        crate::services::services().quality.borrow_mut().tier = tier;
        true
    }
    /// The clipmap levels the view's sea has now, or -1 without a scene.
    #[func]
    fn sea_levels(&self, view: Gd<BpWorldView>) -> i64 { view.bind().scene.as_ref().map_or(-1, |s| s.ocean.spec.levels as i64) }
    /// End the scripted session: the view frees its scene on its next frame.
    #[func]
    fn finish(&mut self) {
        let Some(run) = self.run.take() else { return };
        let services = crate::services::services();
        {
            let mut feed = services.feed.borrow_mut();
            feed.state = SessionState::Idle;
            feed.events.clear();
            feed.snapshot = None;
        }
        *services.view.borrow_mut() = ViewFrame::default();
        services.designs.borrow_mut().remove(&run.fleet.hash);
    }
    /// What the scripted scene should look like to the view right now, and what the view made of it.
    #[func]
    fn report(&self, view: Gd<BpWorldView>) -> VarDictionary {
        let mut d = VarDictionary::new();
        let Some(run) = &self.run else { return d };
        let services = crate::services::services();
        let v = view.bind();
        let t = run.time_s();
        let camera = services.camera.get();
        d.set("frame", run.frame as i64);
        d.set("scene", v.scene.is_some());
        d.set("mounts", run.fleet.mounts as i64);
        d.set("own_size_m", Vector3::new(run.fleet.size_m[0], run.fleet.size_m[1], run.fleet.size_m[2]));
        d.set("camera_valid", camera.valid);
        // The floor rule (L106), against the simulation's own wave set.
        d.set("camera_above_sea_m", camera.pos[1] - run.waves.height_at(camera.pos[0], camera.pos[2], t));
        let frame = services.view.borrow();
        d.set("origin", Vector2::new(frame.origin.x as f32, frame.origin.z as f32));
        d.set("own_map", Vector2::new((run.base()[0] + SPEED_MPS * t) as f32, run.base()[1] as f32));
        if let Some(scene) = &v.scene {
            // The renderer's sea is the simulation's: same set, same height.
            let probe = [run.base()[0] + 40.0, run.base()[1] - 25.0];
            d.set("wave_set_matches", scene.ocean.core.waves == run.waves);
            d.set("sea_height_error_m", (scene.ocean.height_at(probe[0], probe[1], t) - run.waves.height_at(probe[0], probe[1], t)).abs());
            d.set("sea_state_shown", scene.ocean.core.weather().map_or(-1, |w| w.sea_state.0 as i64));
            d.set("shore_window", scene.ocean.shore.window().is_some());
            d.set("ground_at_island_m", scene.ocean.ground_height(START[0] + 500.0, START[1] - 1_300.0));
            d.set("own_lod", scene.hull.lod(EntityId::from_u64(OWN)).map_or(-1, |l| l as i64));
            d.set("hull_numbers", v.numbers.values().map(|n| n.len() as i64).sum::<i64>());
            d.set("hull_number_text", v.numbers.values().flatten().next().map_or(String::new(), |n| n.quad.text.clone()).as_str());
        }
        // Where things are on screen, for the script's picking checks.
        if let (Some(lens), Some(scene)) = (v.lens, v.scene.as_ref()) {
            let centre_of = |entity: u64| scene.hull.bounds(EntityId::from_u64(entity)).map(|(lo, hi)| (glam::Vec3::from(lo) + glam::Vec3::from(hi)) * 0.5);
            let screen = |p: Option<glam::Vec3>| p.and_then(|p| lens.project(p)).map_or(Vector2::new(-1.0, -1.0), |s| Vector2::new(s[0], s[1]));
            d.set("own_screen", screen(centre_of(OWN)));
            d.set("enemy_screen", screen(centre_of(ENEMY)));
            d.set("canvas", Vector2::new(lens.canvas[0], lens.canvas[1]));
        }
        d.set("aim_local", { let a = services.aim_local.get(); Vector3::new(a[0], a[1], a[2]) });
        d.set("clock_calibrated", crate::render::ocean::clock::calibrated());
        d.set("clock_running", crate::render::ocean::clock::shader_time().is_some());
        d
    }
    /// World positions on the sea surface around the viewer's hull at the rendered time, from the
    /// simulation's wave set: floats placed here must sit on the drawn water. Render-space positions.
    #[func]
    fn sea_probes(&self) -> PackedVector3Array {
        let mut out = PackedVector3Array::new();
        let Some(run) = &self.run else { return out };
        let services = crate::services::services();
        let frame = services.view.borrow();
        let (t, base) = (frame.sim_time_s, run.base());
        let own = [base[0] + SPEED_MPS * t, base[1]];
        for i in 0..5 {
            for j in 0..5 {
                let (x, z) = (own[0] - 60.0 + 30.0 * i as f64, own[1] + 30.0 + 22.0 * j as f64);
                let p = frame.origin.to_render(DVec3::new(x, run.waves.height_at(x, z, t), z));
                out.push(Vector3::new(p[0], p[1], p[2]));
            }
        }
        out
    }
}
