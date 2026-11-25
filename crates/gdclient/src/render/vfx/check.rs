//! Part of unit `gd-render-vfx`, shared with `gd-render-particles` and `gd-render-scenery`.
//! `BpEffectsCheck`: a test-only class that drives the VFX, particle and scenery renderers (with the hull
//! and part renderers, so effects have a ship to sit on) inside the engine on a scripted scene, so their
//! `RenderingServer` calls run for real (`res://test/tools/effects_check.gd`). Their logic is tested
//! headless in Rust; this covers what those tests cannot reach: materials, MultiMesh buffers, particle
//! systems, lights, the LUT texture and RID lifetime. Not referenced by shipped scenes.
//!
//! The scene: an own hull steaming at 20 kn past a burning, flooding enemy, a gun firing every second, fall
//! of shot around the enemy, hits, a missile in flight, chaff and a smoke screen, an island, and a storm
//! that turns to haze at 4 s.
#![allow(clippy::disallowed_methods)]
use crate::interp::{BodyPose, CameraState, DecoyPose, RenderOrigin, VesselPose, ViewFrame};
use crate::quality::QualityProfile;
use crate::render::hull::HullRenderer;
use crate::render::parts::PartRenderer;
use crate::render::particles::ParticleRenderer;
use crate::render::projectiles::ShellEnd;
use crate::render::scenery::SceneryRenderer;
use crate::render::vfx::VfxRenderer;
use crate::render::{FrameContext, UploadBudget};
use crate::seams::WireEvent;
use content::blueprint::{ExteriorBlueprint, PaintEntry, PartPlacement};
use content::cell::{ArmourTier, Cell, CellGrid, CellPos, CellShape, Material};
use content::ids::{CompartmentId, ContentHash, PartId, PartSlot, SideId};
use content::weather::{WeatherKind, WeatherState};
use godot::prelude::*;
use sim_core::events::PhysEvent;
use sim_core::EntityId;
use sim_systems::protocol::{EntityIntro, SimEvent, TurretView, VesselFlags};
use std::sync::Arc;

const HASH: ContentHash = ContentHash([0xE7; 32]);
const OWN: u64 = 1;
const ENEMY: u64 = 2;

/// A 40 m box hull with a deckhouse and one gun forward.
fn design() -> ExteriorBlueprint {
    let (len, depth, beam) = (80i16, 8i16, 14i16);
    let mut grid = CellGrid::new([len as u16, (depth + 6) as u16, beam as u16]);
    let steel = |paint: u8| Cell { material: Material::Steel, armour: ArmourTier::None, paint, shape: CellShape::CUBE };
    for y in 0..depth { for x in 0..len { for z in 0..beam { grid.set(CellPos::new(x, y, z), steel(if y < 3 { 8 } else { 2 })); } } }
    for y in depth..depth + 4 { for x in 30..46 { for z in 3..11 { grid.set(CellPos::new(x, y, z), steel(1)); } } }
    let table = content::blueprint::io::default_paint_table();
    let paint: Vec<PaintEntry> = table.iter().take(16).cloned().collect();
    let parts = vec![(PartSlot(0), PartPlacement { part: PartId::new("gun.76mm.single"), pos: CellPos::new(60, depth, 5), rot: Default::default(), kit: None, label: None, load: None })];
    ExteriorBlueprint { source_hash: HASH, name: "effects".into(), size: grid.size, cells: grid.to_rows(), paint, markings: Vec::new(), parts }
}

fn ev(tick: u64, event: SimEvent) -> WireEvent { WireEvent { tick, event } }
fn reveal(entity: u64) -> WireEvent {
    ev(0, SimEvent::EntityRevealed(Box::new(EntityIntro { entity: EntityId::from_u64(entity), side: SideId((entity - 1) as u8), blueprint: HASH, exterior_only: true, class: None, name: String::new(), player: None, status: Default::default(), damage: Vec::new(), removed: Vec::new(), parent: None, track: Default::default() })))
}
fn weather(kind: WeatherKind) -> WireEvent {
    let w = WeatherState { kind, visibility_m: kind.visibility_m(), cloud_cover: 0.6, wind_speed_mps: 9.0, wind_dir_rad: 1.0, sun_elevation_deg: 8.0, sun_azimuth_deg: 250.0, ..Default::default() };
    ev(0, SimEvent::Phys(PhysEvent::WeatherChanged { weather: w, wave_seed: 7 }))
}

struct Scene {
    hull: HullRenderer,
    parts: PartRenderer,
    vfx: VfxRenderer,
    particles: ParticleRenderer,
    scenery: SceneryRenderer,
    scenario: Rid,
    quality: QualityProfile,
    frame: u64,
    camera: CameraState,
    lightning: u32,
    max_vfx: u32,
    max_particles: u32,
    max_lights: u32,
    /// Distance and bore agreement between the design fallback and the part's muzzle anchor (slot 0).
    muzzle_error_m: f32,
    muzzle_dot: f32,
}

/// Test-only driver of the effect and scenery units. Instantiate with `BpEffectsCheck.new()`.
#[derive(GodotClass)]
#[class(base = RefCounted)]
pub struct BpEffectsCheck {
    scene: Option<Scene>,
    base: Base<RefCounted>,
}

#[godot_api]
impl IRefCounted for BpEffectsCheck {
    fn init(base: Base<RefCounted>) -> Self { Self { scene: None, base } }
}

#[godot_api]
impl BpEffectsCheck {
    /// Build the renderers in `scenario` (a `World3D.scenario`). Call `finish` before freeing the object.
    #[func]
    fn start(&mut self, scenario: Rid) {
        self.finish();
        let services = crate::services::services();
        services.designs.borrow_mut().insert(HASH, crate::worker::ReceivedDesign::Exterior(Arc::new(design())));
        let catalogue = services.catalogue();
        let camera = CameraState { pos: [-60.0, 25.0, 120.0], forward: [0.45, -0.15, -0.88], up: [0.0, 1.0, 0.0], fov_y_deg: 60.0, viewport_px: [1920.0, 1080.0], valid: true };
        let tier = crate::enums::QualityTier::Medium;
        let mut scenery = SceneryRenderer::new(scenario);
        let map = content::campaign::MapDef { size_m: [20_000.0, 20_000.0], depth_m: 200.0, islands: vec![content::campaign::Island { centre_m: [900.0, -1_400.0], radius_m: 500.0, height_m: 120.0, seed: 5 }], markers: Vec::new(), seed: 3 };
        scenery.set_map(&map);
        let mut vfx = VfxRenderer::new(scenario, tier);
        let mut particles = ParticleRenderer::new(scenario, tier, true);
        vfx.warm_up();
        particles.warm_up();
        self.scene = Some(Scene {
            hull: HullRenderer::new(scenario), parts: PartRenderer::new(scenario, &catalogue), vfx, particles, scenery, scenario,
            quality: QualityProfile::default(), frame: 0, camera, lightning: 0, max_vfx: 0, max_particles: 0, max_lights: 0, muzzle_error_m: f32::MAX, muzzle_dot: -1.0,
        });
    }
    /// The scripted camera: `{ "position": Vector3, "target": Vector3, "fov": float }`.
    #[func]
    fn camera(&self) -> VarDictionary {
        let mut d = VarDictionary::new();
        d.set("position", Vector3::new(-60.0, 25.0, 120.0));
        d.set("target", Vector3::new(30.0, 4.0, -60.0));
        d.set("fov", 60.0);
        d
    }
    /// Run one frame of the scripted scene.
    #[func]
    fn step(&mut self) {
        let Some(s) = &mut self.scene else { return };
        s.frame += 1;
        let f = s.frame;
        let t = f as f64 / 60.0;
        let own_x = -80.0 + 10.3 * t as f32;
        let own = VesselPose { entity: EntityId::from_u64(OWN), pos: [own_x, -1.5, 0.0], rot: [0.0, 0.0, 0.0, 1.0], vel: [10.3, 0.0, 0.0], own: true, power: 0.8, turrets: 0..1, ..Default::default() };
        let enemy = VesselPose { entity: EntityId::from_u64(ENEMY), pos: [10.0, -2.2, -90.0], rot: [0.0, 0.0, 0.02, 0.9998], vel: [1.0, 0.0, 0.0], flags: VesselFlags::BURNING, turrets: 1..1, ..Default::default() };
        let turrets = vec![TurretView { slot: PartSlot(0), train_rad: -0.9, elevation_rad: 0.15 }];
        let bodies = vec![BodyPose { entity: EntityId::from_u64(40), kind: content::ids::MunitionIdx(0), pos: [-300.0 + 250.0 * (t as f32 % 4.0), 15.0, -200.0], vel: [250.0, 0.0, 0.0], seeker: Default::default(), track: Default::default(), own_side: false }];
        let decoys = vec![
            DecoyPose { entity: EntityId::from_u64(50), kind: content::part::DecoyKind::Smoke, pos: [150.0, 15.0, -160.0], age_s: t as f32 + 4.0 },
            DecoyPose { entity: EntityId::from_u64(51), kind: content::part::DecoyKind::Chaff, pos: [60.0, 120.0, -260.0], age_s: t as f32 },
            DecoyPose { entity: EntityId::from_u64(52), kind: content::part::DecoyKind::Flare, pos: [-20.0, 60.0, -240.0], age_s: (t as f32) % 5.0 },
        ];
        let view = ViewFrame { rev: f, render_tick: f as f64, sim_time_s: t, origin: RenderOrigin::default(), origin_shift: if f == 200 { Some([0.0; 3]) } else { None }, vessels: vec![own, enemy], turrets, bodies, decoys, shells: Vec::new(), tracks: Vec::new(), own: Some(0), snapshot: None };
        let mut events = Vec::new();
        if f == 1 {
            events.push(ev(0, SimEvent::SortieStarted { seed: 11, tick_hz: 60 }));
            events.extend([reveal(OWN), reveal(ENEMY), weather(WeatherKind::Storm)]);
            events.push(ev(1, SimEvent::Phys(PhysEvent::FireStarted { vessel: EntityId::from_u64(ENEMY), compartment: CompartmentId(2), spread: false })));
            events.push(ev(1, SimEvent::Phys(PhysEvent::Breach { vessel: EntityId::from_u64(ENEMY), compartment: CompartmentId(3), below_waterline: true })));
        }
        if f == 240 { events.push(ev(f, SimEvent::Phys(PhysEvent::WeatherChanged { weather: WeatherState { kind: WeatherKind::Haze, visibility_m: 8_000.0, cloud_cover: 0.3, ..Default::default() }, wave_seed: 7 }))); }
        if f % 60 == 10 { events.push(ev(f, SimEvent::Phys(PhysEvent::MountFired { vessel: EntityId::from_u64(OWN), slot: PartSlot(0), rounds: 1 }))); }
        if f % 45 == 20 { events.push(ev(f, SimEvent::Phys(PhysEvent::BodyBurst { body: EntityId::from_u64(60), pos: glam::DVec3::new(80.0, 90.0, -220.0), underwater: false }))); }
        let mut ends = Vec::new();
        if f % 20 == 0 {
            let k = (f / 20) as f32;
            ends.push(ShellEnd { pos: [20.0 + 30.0 * (k * 1.7).sin(), 0.0, -110.0 - 25.0 * (k * 0.9).cos()], calibre_mm: 76 + (f % 3) as u16 * 50, water: true, own_side: true });
            events.push(ev(f, SimEvent::Phys(PhysEvent::Splash { projectile: sim_core::projectiles::ProjectileId(f as u32), pos: glam::DVec3::new(ends[0].pos[0] as f64, 0.0, ends[0].pos[2] as f64), calibre_mm: ends[0].calibre_mm, shooter: EntityId::from_u64(OWN) })));
        }
        if f % 90 == 45 { ends.push(ShellEnd { pos: [25.0, 2.0, -90.0], calibre_mm: 127, water: false, own_side: true }); }
        let ctx = FrameContext { view: &view, events: &events, camera: s.camera, delta_s: 1.0 / 60.0, scenario: s.scenario, quality: &s.quality, degrade_step: 0, reduce_flashing: false };
        let mut budget = UploadBudget::new(300);
        s.hull.update(&ctx, &mut budget);
        s.parts.update(&ctx);
        let mut anchors = Vec::new();
        // Every other second the muzzle comes from the part anchors; otherwise from the design fallback.
        if (f / 60) % 2 == 0 { s.parts.anchors(hull_mesh::parts::AnchorKind::Muzzle, &mut anchors); s.vfx.set_anchors(&anchors); }
        if f == 30 {
            let mut all = Vec::new();
            s.parts.anchors(hull_mesh::parts::AnchorKind::Muzzle, &mut all);
            let handed = std::mem::take(&mut s.vfx.core.anchors);
            let (fallback, _) = s.vfx.core.muzzles(&view, EntityId::from_u64(OWN), PartSlot(0));
            s.vfx.core.anchors = handed;
            if let (Some(a), Some((pos, dir))) = (all.iter().find(|a| a.vessel == EntityId::from_u64(OWN) && a.slot == PartSlot(0)), fallback.first()) {
                s.muzzle_error_m = (glam::Vec3::from(a.pos) - *pos).length();
                s.muzzle_dot = glam::Vec3::from(a.dir).normalize_or_zero().dot(*dir);
            }
        }
        s.vfx.update(&ctx, &ends);
        s.particles.update(&ctx);
        if s.scenery.update(&ctx).is_some() { s.lightning += 1; }
        s.max_vfx = s.max_vfx.max(s.vfx.stats().vfx_instances);
        s.max_particles = s.max_particles.max(s.particles.stats().particles);
        s.max_lights = s.max_lights.max(s.vfx.stats().lights);
    }
    /// Counters after the frames run so far.
    #[func]
    fn report(&mut self) -> VarDictionary {
        let mut d = VarDictionary::new();
        let Some(s) = &mut self.scene else { return d };
        d.set("frame", s.frame as i64);
        d.set("vfx_instances", s.vfx.stats().vfx_instances as i64);
        d.set("max_vfx_instances", s.max_vfx as i64);
        d.set("particles", s.particles.stats().particles as i64);
        d.set("max_particles", s.max_particles as i64);
        d.set("max_lights", s.max_lights as i64);
        d.set("lightning", s.lightning as i64);
        d.set("muzzle_error_m", s.muzzle_error_m as f64);
        d.set("muzzle_dot", s.muzzle_dot as f64);
        d.set("env_rev", s.scenery.rev() as i64);
        d.set("lut", s.scenery.lut().is_some());
        d.set("fog_density", s.scenery.environment().fog_density as f64);
        d.set("clouds", s.scenery.core.clouds.len() as i64);
        d.set("island_tiles", s.scenery.core.tiles.values().filter(|t| t.far_built).count() as i64);
        d.set("ground_height", s.scenery.ground_height(900.0, -1_400.0) as f64);
        d
    }
    /// The LUT texture (for the capture script's environment).
    #[func]
    fn lut(&self) -> Option<Gd<godot::classes::ImageTexture3D>> { self.scene.as_ref().and_then(|s| s.scenery.lut()) }
    /// Free every RID and stop the threads.
    #[func]
    fn finish(&mut self) {
        if let Some(mut s) = self.scene.take() {
            s.vfx.free();
            s.particles.free();
            s.scenery.free();
            s.parts.free();
            s.hull.free();
            crate::services::services().designs.borrow_mut().remove(&HASH);
            crate::render::hull::design::forget_all();
        }
    }
}
