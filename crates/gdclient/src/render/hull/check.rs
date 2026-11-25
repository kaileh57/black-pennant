//! Part of unit `gd-render-hull`. `BpRenderCheck`: a test-only class that drives the hull, part and
//! projectile renderers inside the engine on a scripted scene, so their `RenderingServer` calls are exercised
//! for real (`res://test/tools/render_check.gd`). The units' logic is tested headless in Rust; this covers
//! what those tests cannot reach: surface formats, texture creation, MultiMesh buffers, RID lifetime.
//! Not referenced by shipped scenes.
//!
//! The scene: two hulls of one design (one beside the camera at LOD0, one 2 km off at a coarse LOD), a third
//! in dock mode at the origin when asked, a stream of shells, one guided body and one towed decoy. Frame 30
//! holes the near hull; frame 60 fires its gun.
// Client rendering, not simulation (clippy.toml): std floats are allowed here. Nothing drawn from them feeds
// back into the sim.
#![allow(clippy::disallowed_methods)]
use super::design::DrawDesign;
use crate::interp::{BodyPose, CameraState, DecoyPose, RenderOrigin, ShellView, VesselPose, ViewFrame};
use crate::quality::QualityProfile;
use crate::render::parts::PartRenderer;
use crate::render::projectiles::ProjectileRenderer;
use crate::render::{FrameContext, UploadBudget};
use crate::seams::WireEvent;
use content::blueprint::{ExteriorBlueprint, Marking, MarkingKind, PaintEntry, PartPlacement};
use content::cell::{ArmourTier, Cell, CellChange, CellGrid, CellPos, CellShape, Face, Material, ShapeKind};
use content::ids::{CellIndex, ContentHash, PartId, PartSlot};
use godot::prelude::*;
use sim_core::events::PhysEvent;
use sim_core::projectiles::{ProjectileId, ProjectileSpawn};
use sim_core::EntityId;
use sim_systems::protocol::{EntityIntro, SimEvent, TurretView};
use std::sync::Arc;

const HASH: ContentHash = ContentHash([0xC4; 32]);
const NEAR: u64 = 1;
const FAR: u64 = 2;

/// A 60 m hull with a raked bow, a deckhouse with a glazed bridge, and a few fitted parts.
fn design() -> ExteriorBlueprint {
    let (len, depth, beam) = (120i16, 10i16, 20i16);
    let mut grid = CellGrid::new([len as u16, (depth + 8) as u16, beam as u16]);
    let steel = |paint: u8| Cell { material: Material::Steel, armour: ArmourTier::None, paint, shape: CellShape::CUBE };
    for y in 0..depth {
        for x in 0..len {
            // The beam narrows over the last 30 cells toward the bow and the keel is narrower than the deck.
            let taper = ((x - (len - 30)).max(0) as f32 / 30.0).powi(2);
            let half = ((beam as f32 / 2.0) * (1.0 - taper) - (depth - 1 - y) as f32 * 0.35).max(1.0);
            for z in 0..beam {
                let off = (z as f32 + 0.5 - beam as f32 / 2.0).abs();
                if off <= half {
                    // Boot topping and antifouling below the waterline row, haze grey above, a darker deck.
                    let paint = if y == depth - 1 { 6 } else if y < 3 { 8 } else if y == 3 { 7 } else { 2 };
                    grid.set(CellPos::new(x, y, z), steel(paint));
                }
            }
        }
    }
    for y in depth..depth + 5 { for x in 50..74 { for z in 5..15 {
        let glass = y == depth + 3 && (x == 73 || z == 5 || z == 14);
        grid.set(CellPos::new(x, y, z), if glass { Cell { material: Material::Glass, ..steel(1) } } else { steel(1) });
    } } }
    let table = content::blueprint::io::default_paint_table();
    let paint: Vec<PaintEntry> = table.iter().take(16).cloned().collect();
    let place = |part: &str, x: i16, y: i16, z: i16| PartPlacement { part: PartId::new(part), pos: CellPos::new(x, y, z), rot: Default::default(), kit: None, label: None, load: None };
    let parts = vec![
        (PartSlot(0), place("gun.76mm.single", 84, depth, 7)),
        (PartSlot(1), place("radar.search", 60, depth + 5, 8)),
        (PartSlot(2), place("radar.nav", 66, depth + 5, 9)),
        (PartSlot(3), place("gun.ciws.20mm", 40, depth, 8)),
        (PartSlot(4), place("launcher.ashm.light", 26, depth, 6)),
    ];
    let markings = vec![
        Marking { kind: MarkingKind::Stripe, cell: CellPos::new(40, 6, beam - 2), face: Face::ZPos, paint: 0, size_cells: 8, text: String::new() },
        Marking { kind: MarkingKind::HullNumber, cell: CellPos::new(96, 7, 1), face: Face::ZNeg, paint: 0, size_cells: 3, text: "74".into() },
    ];
    ExteriorBlueprint { source_hash: HASH, name: "check".into(), size: grid.size, cells: grid.to_rows(), paint, markings, parts }
}

fn ev(tick: u64, event: SimEvent) -> WireEvent { WireEvent { tick, event } }
fn reveal(entity: u64) -> WireEvent {
    ev(0, SimEvent::EntityRevealed(Box::new(EntityIntro { entity: EntityId::from_u64(entity), side: content::ids::SideId((entity - 1) as u8), blueprint: HASH, exterior_only: true, class: None, name: String::new(), player: None, status: Default::default(), damage: Vec::new(), removed: Vec::new(), parent: None, track: Default::default() })))
}

struct Scene {
    hull: super::HullRenderer,
    parts: PartRenderer,
    projectiles: ProjectileRenderer,
    scenario: Rid,
    quality: QualityProfile,
    frame: u64,
    ends: u32,
    camera: CameraState,
}

/// Test-only driver of the 3D render units. Instantiate with `BpRenderCheck.new()`.
#[derive(GodotClass)]
#[class(base = RefCounted)]
pub struct BpRenderCheck {
    scene: Option<Scene>,
    base: Base<RefCounted>,
}

#[godot_api]
impl IRefCounted for BpRenderCheck {
    fn init(base: Base<RefCounted>) -> Self { Self { scene: None, base } }
}

#[godot_api]
impl BpRenderCheck {
    /// Build the renderers in `scenario` (a `World3D.scenario`). Call `finish` before freeing the object.
    #[func]
    fn start(&mut self, scenario: Rid) {
        self.finish();
        let services = crate::services::services();
        services.designs.borrow_mut().insert(HASH, crate::worker::ReceivedDesign::Exterior(Arc::new(design())));
        let catalogue = services.catalogue();
        let camera = CameraState { pos: [70.0, 14.0, -55.0], forward: [-0.45, -0.2, 0.87], up: [0.0, 1.0, 0.0], fov_y_deg: 60.0, viewport_px: [1920.0, 1080.0], valid: true };
        self.scene = Some(Scene {
            hull: super::HullRenderer::new(scenario),
            parts: PartRenderer::new(scenario, &catalogue),
            projectiles: ProjectileRenderer::new(scenario, &catalogue, crate::enums::QualityTier::Medium),
            scenario,
            quality: QualityProfile::default(),
            frame: 0,
            ends: 0,
            camera,
        });
    }
    /// Where the scripted camera is and what it looks at, for the script's own `Camera3D`:
    /// `{ "position": Vector3, "target": Vector3, "fov": float }`.
    #[func]
    fn camera(&self) -> VarDictionary {
        let mut d = VarDictionary::new();
        d.set("position", Vector3::new(70.0, 14.0, -55.0));
        d.set("target", Vector3::new(34.0, 4.0, 5.0));
        d.set("fov", 60.0);
        d
    }
    /// Draw the scripted design in dock mode at the origin as well (`show_design` path of the part renderer).
    #[func]
    fn show_dock_parts(&mut self) {
        let Some(s) = &mut self.scene else { return };
        let ext = design();
        let blueprint_parts: Vec<PartPlacement> = ext.parts.iter().map(|(_, p)| p.clone()).collect();
        let mut blueprint = content::blueprint::io::new_blueprint("check", 60.0);
        blueprint.cells = ext.cells.clone();
        blueprint.parts = blueprint_parts;
        s.parts.show_design(&blueprint, &crate::services::services().catalogue());
    }
    /// Run one frame of the scripted scene with the given upload budget in microseconds.
    #[func]
    fn step(&mut self, budget_us: i64) {
        let Some(s) = &mut self.scene else { return };
        s.frame += 1;
        let f = s.frame;
        let t = f as f64 / 60.0;
        let near = VesselPose { entity: EntityId::from_u64(NEAR), pos: [0.0, -2.0, 0.0], rot: [0.0, 0.0, 0.0, 1.0], own: true, power: 0.6, rudder: 0.3, turrets: 0..2, ..Default::default() };
        let far = VesselPose { entity: EntityId::from_u64(FAR), pos: [-1_900.0, -2.0, 700.0], rot: [0.0, 0.38, 0.0, 0.92], power: 1.0, turrets: 2..2, ..Default::default() };
        let turrets = vec![TurretView { slot: PartSlot(0), train_rad: (t * 0.5).sin() as f32, elevation_rad: 0.2 }, TurretView { slot: PartSlot(3), train_rad: -1.0, elevation_rad: 0.5 }];
        let shells = (0..6u32).map(|i| ShellView { id: ProjectileId(i), pos: [50.0 + i as f32 * 12.0 + (f % 60) as f32, 8.0 + i as f32, -10.0], vel: [700.0, 20.0, 0.0] }).collect();
        let bodies = vec![BodyPose { entity: EntityId::from_u64(40), kind: content::ids::MunitionIdx(0), pos: [30.0, 12.0, -20.0], vel: [250.0, 0.0, 10.0], seeker: Default::default(), track: Default::default(), own_side: true }];
        let decoys = vec![DecoyPose { entity: EntityId::from_u64(41), kind: content::part::DecoyKind::TowedDecoy, pos: [-40.0 + t as f32, 0.0, 5.0], age_s: t as f32 }];
        let view = ViewFrame { rev: f, render_tick: f as f64, sim_time_s: t, origin: RenderOrigin::default(), origin_shift: None, vessels: vec![near, far], turrets, bodies, decoys, shells, tracks: Vec::new(), own: Some(0), snapshot: None };
        let mut events = Vec::new();
        if f == 1 {
            events.extend([reveal(NEAR), reveal(FAR)]);
            events.extend((0..6u32).map(|i| ev(0, SimEvent::Phys(PhysEvent::ProjectileSpawned(ProjectileSpawn { id: ProjectileId(i), tick: 0, origin: glam::DVec3::ZERO, velocity: glam::DVec3::X * 700.0, calibre_mm: 76, band: content::part::weapons::CalibreBand::Auto, shooter: EntityId::from_u64(NEAR), mount: PartSlot(0), rounds: 1, point_defence: false })))));
        }
        if f == 30 {
            // A hole through the side amidships, by a missile: geometry, scorch and a mask change.
            let size = [120u32, 18, 20];
            let cells: Vec<CellChange> = (30..36).flat_map(|x| (4..9).flat_map(move |y| (0..4).map(move |z| CellChange { cell: CellIndex(x + size[0] * (z + size[2] * y)), bucket: 8 }))).collect();
            events.push(ev(f, SimEvent::Phys(PhysEvent::CellsDamaged { vessel: EntityId::from_u64(NEAR), changes: cells, cause: sim_core::damage::DamageCause::Missile, attacker: EntityId::NONE })));
            events.push(ev(f, SimEvent::Phys(PhysEvent::PartDamaged { vessel: EntityId::from_u64(NEAR), slot: PartSlot(4), level: content::enums::DamageLevel::Destroyed })));
        }
        if f == 60 {
            events.push(ev(f, SimEvent::Phys(PhysEvent::MountFired { vessel: EntityId::from_u64(NEAR), slot: PartSlot(0), rounds: 1 })));
            events.push(ev(f, SimEvent::Phys(PhysEvent::Splash { projectile: ProjectileId(5), pos: glam::DVec3::new(200.0, 0.0, -10.0), calibre_mm: 76, shooter: EntityId::from_u64(NEAR) })));
        }
        let ctx = FrameContext { view: &view, events: &events, camera: s.camera, delta_s: 1.0 / 60.0, scenario: s.scenario, quality: &s.quality, degrade_step: 0, reduce_flashing: false };
        let mut budget = UploadBudget::new(budget_us.max(0) as u64);
        s.hull.update(&ctx, &mut budget);
        s.parts.update(&ctx);
        s.ends += s.projectiles.update(&ctx).len() as u32;
        let _ = s.hull.hull_masks(&ctx);
    }
    /// Counters after the frames run so far.
    #[func]
    fn report(&mut self) -> VarDictionary {
        let mut d = VarDictionary::new();
        let Some(s) = &mut self.scene else { return d };
        let lod = |l: Option<crate::seams::Lod>| l.map_or(-1, |l| l as i64);
        let near = EntityId::from_u64(NEAR);
        let mut anchors = Vec::new();
        s.parts.anchors(hull_mesh::parts::AnchorKind::Muzzle, &mut anchors);
        let hit = s.hull.raycast(near, [20.0, 1.0, -30.0], [0.0, 0.0, 1.0]);
        d.set("frame", s.frame as i64);
        d.set("chunks_drawn", s.hull.stats().chunks_drawn as i64);
        d.set("uploads_pending", s.hull.stats().chunk_uploads_pending as i64);
        d.set("near_lod", lod(s.hull.lod(near)));
        d.set("far_lod", lod(s.hull.lod(EntityId::from_u64(FAR))));
        d.set("markings", s.hull.marking_quads(near).len() as i64);
        d.set("bounds", s.hull.bounds(near).is_some());
        d.set("ray_hit", hit.is_some());
        d.set("part_instances", s.parts.stats().part_instances as i64);
        d.set("muzzles", anchors.len() as i64);
        d.set("part_mesh", s.parts.part_mesh(content::ids::PartIdx(0)).is_valid());
        d.set("projectile_instances", s.projectiles.stats().projectile_instances as i64);
        d.set("shell_ends", s.ends as i64);
        let ctx_view = ViewFrame::default();
        let ctx = FrameContext { view: &ctx_view, events: &[], camera: s.camera, delta_s: 0.0, scenario: s.scenario, quality: &s.quality, degrade_step: 0, reduce_flashing: false };
        let masks = s.hull.hull_masks(&ctx);
        d.set("mask_texture", masks.texture.is_valid());
        d
    }
    /// Free every RID and stop the meshing thread.
    #[func]
    fn finish(&mut self) {
        if let Some(mut s) = self.scene.take() {
            s.hull.free();
            s.parts.free();
            s.projectiles.free();
            crate::services::services().designs.borrow_mut().remove(&HASH);
            super::design::forget_all();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_scripted_design_is_drawable() {
        let d = DrawDesign::from_exterior(&design(), HASH);
        assert!(d.meshable());
        assert_eq!(d.grid.size, [120, 18, 20]);
        assert_eq!(d.parts.len(), 5);
        let known = d.parts.iter().filter(|(_, p)| content::catalogue::catalogue().part(&p.part).is_some()).count();
        assert_eq!(known, 5, "the scripted parts are catalogue parts");
        assert!(d.palette.len() >= 9);
        // Both markings sit on standing cells with an open face, so both place.
        let placed: usize = d.markings.iter().map(|m| hull_mesh::pipeline::marking_quads(std::slice::from_ref(m), &d.grid).len()).sum();
        assert_eq!(placed, 2);
    }
}
