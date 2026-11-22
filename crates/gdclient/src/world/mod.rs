//! Unit `gd-world-view`. `BpWorldView` (Node3D, root of `res://world/world.tscn`): owns all 3D rendering of
//! simulation state and the sortie cameras' data. DESIGN.md L106 (bridge view: orbit camera at least 1 m
//! above the sea; Tab switches to the plot table; no continuous zoom between them), L1088 (per-frame
//! adapter work), L1094 (picking through Rust queries), L1148 (floating origin), L1873 (camera far at least
//! 20,000 m), L1824 (depth of field only in editor and replay cameras), L1718 (shake, blur and roll
//! sliders), L2002-2012 (2 ms budget and its fallbacks).
//!
//! The node owns the render units in [`crate::render`] and drives them each frame from `Services::view` and
//! `Services::feed`. It moves one `Camera3D` (bound by the scene script) and writes `Services::camera`.
//! GDScript reads keys and gestures in `_unhandled_input` and calls the camera and picking methods here; it
//! never positions anything in 3D itself. The world scene's GDScript applies environment presets from
//! `environment()`.
//!
//! Hidden while the plot table is up: the 3D viewport keeps rendering only if the setting
//! `graphics.render_behind_plot` is on; simulation, audio and HUD continue either way.
//!
//! # How the unit is built
//! - [`camera`] is the camera mathematics (orbit, sea floor, shake, roll, the lens that makes rays and
//!   projects points) and [`picture`] what the view derives from the side picture (track markers and their
//!   declutter rank, the manual aim solution, the part under the cursor, which map a sortie is fought on).
//!   Neither touches Godot; both test headless. This file is the node: it owns the render units, runs the
//!   frame, moves the `Camera3D` and answers the queries. [`check`] drives it inside the engine.
//! - The render units are built on the first frame a session is loading, in a sortie, ended or a replay,
//!   and freed when the session goes idle. Events live in `Services::feed` for one frame, so every unit is
//!   updated every frame the scene exists, whatever the view: under the plot table the 3D viewport is
//!   switched off (`Viewport.disable_3d`) rather than the units paused.
//! - The camera's yaw is a true bearing and does not turn with the ship. Its target follows the followed
//!   hull's centre on the map exactly and its height through a low-pass. A sortie opens with the viewer's
//!   hull framed (its length across a little under half the view), and however far the player zooms in the
//!   camera stays outside the followed hull's box.
//! - Camera shake is felt when playback reaches the event that causes it, the frame the flash and the hole
//!   are drawn, not when the event arrives a tenth of a second earlier.
//! - The quality tier is followed without being told: `apply_quality` is what the scene script calls on
//!   `Quality.changed`, and a frame that finds another tier in the profile than the units were given
//!   applies it too.
//! - Manual aim: the direction sent to the simulation runs from the held mount to the point under the
//!   reticle, because the simulation lays the gun on what that ray reaches from the muzzle. It is left in
//!   `Services::aim_local` each frame; a reticle that was never set is the centre of the view.
//! - Hull numbers (L1801) are `Label3D` children placed on the quads `HullRenderer::marking_quads` hands
//!   out, lit and depth-tested like the hull, hidden once they are under three pixels tall.
//! - Signals are emitted after every `Services` borrow is dropped, because a handler may call straight
//!   back in. `warmed_up` is never emitted inside `warm_up()`.
// Client rendering, not simulation (clippy.toml): std floats are allowed here.
#![allow(clippy::disallowed_methods)]
pub(crate) mod camera;
pub(crate) mod check;
pub(crate) mod picture;

use crate::enums::{GdEnum, QualityTier, SessionState, View};
use crate::interp::{CameraState, RenderOrigin, ViewFrame};
use crate::render::ocean::{ClipmapSpec, OceanRenderer, WaterMaterial};
use crate::render::{FrameContext, RenderStats, UploadBudget};
use glam::{DVec3, Vec3};
use godot::classes::{Camera3D, INode3D, Label3D, Node3D};
use godot::prelude::*;
use sim_core::events::PhysEvent;
use sim_core::EntityId;
use sim_systems::ids::TrackId;
use sim_systems::protocol::SimEvent;
use std::collections::BTreeMap;

/// Orbit camera limits (bridge view).
pub const ORBIT_MIN_DISTANCE_M: f32 = 8.0;
pub const ORBIT_MAX_DISTANCE_M: f32 = 2_000.0;
/// The camera never goes below this height above the local sea surface (L106).
pub const CAMERA_FLOOR_M: f32 = 1.0;

/// Main-thread microseconds the hull unit may spend on uploads and frees per frame (L2008).
const UPLOAD_BUDGET_US: u64 = 300;
/// Frames of adapter time kept for the p99 in `stats()` (ten seconds at 60 Hz).
const ADAPTER_SAMPLES: usize = 600;
/// Frames the warm-up instances stay in view before `warmed_up` (they are placed on the first and drawn on
/// the second; the rest is margin for pipelines compiled across frames).
const WARM_FRAMES: u32 = 6;
/// Frames `warm_up` waits for a camera before it reports anyway.
const WARM_CAMERA_WAIT_FRAMES: u32 = 30;
/// Frames `warmed_up` waits for the first hull to be drawn before it reports anyway (three seconds at 60 Hz;
/// the loading screen has its own, longer limit).
const WARM_HULL_WAIT_FRAMES: u32 = 180;
/// A track symbol is picked when the cursor is within this many canvas units of it.
const PICK_MARKER_PX: f32 = 20.0;
/// Small bodies are picked as spheres of at least this radius, in metres and in canvas units.
const PICK_BODY_M: f32 = 3.0;
const PICK_BODY_PX: f32 = 12.0;
/// The near plane: close enough for the 8 m orbit, far enough to keep depth precision out to 20 km.
const CAMERA_NEAR_M: f32 = 0.5;
/// Hull numbers shorter than this on screen are not drawn.
const NUMBER_MIN_PX: f32 = 3.0;
/// Font size hull-number text is laid out at; the label's pixel size scales it to the quad.
const NUMBER_FONT_PX: i32 = 64;
/// The MSDF build of the interface face (L1801: hull numbers are in-engine MSDF text).
const NUMBER_FONT: &str = "res://ui/fonts/AtkinsonHyperlegibleNext-Variable.msdf.res";

/// Bridge-view orbit state.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Orbit { pub yaw_deg: f32, pub pitch_deg: f32, pub distance_m: f32, pub follow: Option<sim_core::EntityId> }
impl Default for Orbit { fn default() -> Self { Self { yaw_deg: 200.0, pitch_deg: 12.0, distance_m: 120.0, follow: None } } }

/// The render units of a live sortie.
pub struct Scene {
    pub hull: crate::render::hull::HullRenderer,
    pub parts: crate::render::parts::PartRenderer,
    pub projectiles: crate::render::projectiles::ProjectileRenderer,
    pub vfx: crate::render::vfx::VfxRenderer,
    pub particles: crate::render::particles::ParticleRenderer,
    pub ocean: crate::render::ocean::OceanRenderer,
    pub scenery: crate::render::scenery::SceneryRenderer,
}

/// Where a warm-up request stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Warm {
    Idle,
    /// Asked for; `waited` frames have passed without a camera to draw the warm-up instances for.
    Requested { waited: u32 },
    /// The instances are placed; this many frames until `warmed_up`.
    Drawing(u32),
}

/// What identifies the map a scene was given.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum MapKey { Sortie { mode: content::enums::GameMode, seed: u64, operation: String, players: usize }, Given }

/// One hull number on screen.
pub(crate) struct NumberLabel { label: Gd<Label3D>, quad: hull_mesh::pipeline::MarkingQuad, local: glam::Affine3A }

/// What `pick` found.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Picked { pub kind: PickKind, pub map: [f64; 2], pub track: TrackId, pub vessel: EntityId, pub slot: Option<content::ids::PartSlot>, pub pos: Vec3 }
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PickKind { None, Sea, Track, Own }
impl PickKind { fn name(self) -> &'static str { match self { PickKind::None => "none", PickKind::Sea => "sea", PickKind::Track => "track", PickKind::Own => "own" } } }
impl Default for Picked { fn default() -> Self { Self { kind: PickKind::None, map: [0.0; 2], track: TrackId::NONE, vessel: EntityId::NONE, slot: None, pos: Vec3::ZERO } } }

/// Signals collected during a frame and emitted once nothing is borrowed.
#[derive(Default)]
struct Outbox { rebased: Option<Vector3>, environment: bool, lightning: Option<crate::render::scenery::Lightning>, warmed: bool }

/// All 3D rendering of a sortie. Scene node, not an autoload.
#[derive(GodotClass)]
#[class(base = Node3D)]
pub struct BpWorldView {
    pub(crate) view: View,
    pub(crate) orbit: Orbit,
    pub(crate) free_camera: Option<([f64; 3], [f64; 3])>,
    pub(crate) camera: Option<Gd<Camera3D>>,
    pub(crate) scene: Option<Scene>,
    pub(crate) reticle: Vector2,
    pub(crate) env_rev: u64,
    pub(crate) adapter_us: Vec<u32>,
    pub(crate) rig: camera::Rig,
    /// Shakes of events playback has not reached yet.
    pub(crate) shakes: camera::ShakeQueue,
    /// The tier and renderer the units were last given (`build`, `apply_quality`).
    pub(crate) tier_applied: Option<(QualityTier, bool)>,
    /// The lens of the last frame drawn and the origin it is relative to; picking and projection use it.
    pub(crate) lens: Option<camera::Lens>,
    pub(crate) lens_origin: RenderOrigin,
    pub(crate) sim_time_s: f64,
    pub(crate) scenario: Rid,
    pub(crate) water: Option<WaterMaterial>,
    pub(crate) markers: Vec<picture::Marker>,
    pub(crate) markers_packed: Vec<f32>,
    pub(crate) aim: picture::AimReadout,
    pub(crate) warm: Warm,
    /// Frames `warmed_up` has been held back for the first hull.
    pub(crate) warm_held: u32,
    /// True while this node has switched the viewport's 3D off for the plot table.
    pub(crate) hid_3d: bool,
    pub(crate) map_key: Option<MapKey>,
    /// A map handed in by a scripted driver (`check`, test feeds) instead of the sortie intro's.
    pub(crate) map_given: Option<content::campaign::MapDef>,
    pub(crate) stats: RenderStats,
    pub(crate) extrapolated: f32,
    pub(crate) adapter_next: usize,
    pub(crate) numbers: BTreeMap<EntityId, Vec<NumberLabel>>,
    /// The hull-number font, loaded on first use; `None` inside once tried and missing.
    pub(crate) number_font: Option<Option<Gd<godot::classes::Font>>>,
    pub(crate) anchors: Vec<crate::render::parts::AnchorPose>,
    base: Base<Node3D>,
}

#[godot_api]
impl INode3D for BpWorldView {
    fn init(base: Base<Node3D>) -> Self {
        Self {
            view: View::Bridge, orbit: Orbit::default(), free_camera: None, camera: None, scene: None, reticle: Vector2::ZERO, env_rev: 0, adapter_us: Vec::new(),
            rig: camera::Rig::default(), shakes: camera::ShakeQueue::default(), tier_applied: None, lens: None, lens_origin: RenderOrigin::default(), sim_time_s: 0.0, scenario: Rid::Invalid, water: None, markers: Vec::new(), markers_packed: Vec::new(),
            aim: picture::AimReadout::default(), warm: Warm::Idle, warm_held: 0, hid_3d: false, map_key: None, map_given: None, stats: RenderStats::default(), extrapolated: 0.0, adapter_next: 0,
            numbers: BTreeMap::new(), number_font: None, anchors: Vec::new(), base,
        }
    }
    /// Unit `gd-world-view`: build the render units when a sortie starts, then each frame run them in this
    /// order under the 2 ms budget: camera, hull, parts, projectiles, VFX, particles, ocean, scenery; emit
    /// `origin_rebased`, `environment_changed` and `lightning` as they occur. Inert without a session.
    fn process(&mut self, delta: f64) {
        let services = crate::services::services();
        let Ok(state) = services.feed.try_borrow().map(|f| f.state) else { return };
        let live = matches!(state, SessionState::Loading | SessionState::InSortie | SessionState::Ended | SessionState::Replay);
        let mut out = Outbox::default();
        if live && self.scene.is_none() { self.build(); }
        if live && self.scene.is_some() {
            // A tier or renderer change the scene script did not pass on.
            let wanted = services.quality.try_borrow().ok().map(|q| (q.tier, q.forward_plus));
            if wanted.is_some() && wanted != self.tier_applied { self.apply_quality(); }
            self.frame(delta, &services, &mut out);
        } else {
            if self.scene.is_some() { self.teardown(); }
            self.show_3d(true);
            services.aim_local.set([0.0; 3]);
            // A warm-up asked for with nothing to draw finishes on the next frame.
            match self.warm {
                Warm::Idle => {}
                Warm::Requested { .. } => self.warm = Warm::Drawing(1),
                Warm::Drawing(0) => { self.warm = Warm::Idle; out.warmed = true; }
                Warm::Drawing(n) => self.warm = Warm::Drawing(n - 1),
            }
        }
        // Nothing is borrowed from here on: handlers may call back into this node or any facade.
        if let Some(shift) = out.rebased { self.base_mut().emit_signal("origin_rebased", &[shift.to_variant()]); }
        if out.environment { self.base_mut().emit_signal("environment_changed", &[]); }
        if let Some(l) = out.lightning { self.base_mut().emit_signal("lightning", &[(l.intensity as f64).to_variant(), (l.duration_s as f64).to_variant(), (l.bearing_deg as f64).to_variant()]); }
        if out.warmed { self.base_mut().emit_signal("warmed_up", &[]); }
    }
    /// Frees every RID and stops the meshing thread.
    fn exit_tree(&mut self) {
        self.teardown();
        self.show_3d(true);
        crate::services::services().aim_local.set([0.0; 3]);
    }
}

fn v3(v: Vec3) -> Vector3 { Vector3::new(v.x, v.y, v.z) }

impl BpWorldView {
    /// Switch the viewport's 3D on or off (the plot table covers it); remembers only what this node did.
    fn show_3d(&mut self, on: bool) {
        if on != self.hid_3d { return; }
        if !self.base().is_inside_tree() { self.hid_3d = false; return; }
        if let Some(mut viewport) = self.base().get_viewport() { viewport.set_disable_3d(!on); }
        self.hid_3d = !on;
    }

    /// Build the render units in this node's world. Does nothing outside the tree.
    fn build(&mut self) {
        if !self.base().is_inside_tree() { return; }
        let Some(world) = self.base().get_world_3d() else { return };
        let scenario = world.get_scenario();
        if !scenario.is_valid() { return; }
        let services = crate::services::services();
        let Ok(quality) = services.quality.try_borrow().map(|q| q.clone()) else { return };
        let catalogue = services.catalogue();
        // Before any shader loads (the start-up hook has normally done it already).
        crate::render::ocean::gpu::ensure_globals();
        let water = WaterMaterial::load();
        let tier = quality.tier;
        let scene = Scene {
            hull: crate::render::hull::HullRenderer::new(scenario),
            parts: crate::render::parts::PartRenderer::new(scenario, &catalogue),
            projectiles: crate::render::projectiles::ProjectileRenderer::new(scenario, &catalogue, tier),
            vfx: crate::render::vfx::VfxRenderer::new(scenario, tier),
            particles: crate::render::particles::ParticleRenderer::new(scenario, tier, quality.forward_plus),
            ocean: OceanRenderer::new(scenario, ClipmapSpec::for_tier(tier), water.rid()),
            scenery: crate::render::scenery::SceneryRenderer::new(scenario),
        };
        // Events last one frame and a reveal is the only thing that names a hull's design, so a scene built
        // after the sortie's first events cannot draw the hulls revealed before it.
        if let (Ok(feed), Ok(view)) = (services.feed.try_borrow(), services.view.try_borrow()) {
            let started = feed.events.iter().any(|e| matches!(e.event, SimEvent::SortieStarted { .. }));
            if !view.vessels.is_empty() && !started { godot_warn!("world view: built after the sortie's first events; vessels revealed earlier will not be drawn until they are revealed again"); }
        }
        self.scenario = scenario;
        self.water = Some(water);
        self.scene = Some(scene);
        self.map_key = None;
        self.env_rev = 0;
        self.rig.reset();
        self.shakes.clear();
        self.tier_applied = Some((tier, quality.forward_plus));
        self.lens = None;
        self.adapter_us.clear();
        self.adapter_next = 0;
    }

    /// Free the render units and everything this node made for them. Idempotent.
    fn teardown(&mut self) {
        self.clear_numbers();
        if let Some(mut scene) = self.scene.take() {
            scene.scenery.free();
            scene.ocean.free();
            scene.particles.free();
            scene.vfx.free();
            scene.projectiles.free();
            scene.parts.free();
            scene.hull.free();
        }
        if let Some(mut water) = self.water.take() {
            water.free();
            // The camera this node published is gone with its scene; the next sortie starts from its hull.
            crate::services::services().camera.set(CameraState::default());
        }
        self.scenario = Rid::Invalid;
        self.map_key = None;
        self.tier_applied = None;
        self.shakes.clear();
        self.lens = None;
        self.markers.clear();
        self.markers_packed.clear();
        self.aim = picture::AimReadout::default();
        self.stats = RenderStats::default();
        if let Ok(designs) = crate::services::services().designs.try_borrow() { if designs.is_empty() { crate::render::hull::design::forget_all(); } }
    }

    fn clear_numbers(&mut self) {
        for (_, labels) in std::mem::take(&mut self.numbers) {
            for mut n in labels { if n.label.is_instance_valid() { n.label.queue_free(); } }
        }
    }

    /// Give the scene a map directly (scripted drivers); it replaces the one worked out from the sortie
    /// intro until the scene is rebuilt.
    pub(crate) fn give_map(&mut self, map: content::campaign::MapDef) {
        self.map_given = Some(map);
        self.map_key = None;
    }

    /// Canvas size (the units of viewport positions) and the viewport's size in pixels.
    fn viewport_sizes(&self) -> ([f32; 2], [f32; 2]) {
        let fallback = ([1920.0, 1080.0], [1920.0, 1080.0]);
        if !self.base().is_inside_tree() { return fallback; }
        let Some(viewport) = self.base().get_viewport() else { return fallback };
        let rect = viewport.get_visible_rect().size;
        let canvas = if rect.x >= 1.0 && rect.y >= 1.0 { [rect.x, rect.y] } else { fallback.0 };
        let px = viewport.get_texture().map(|t| t.get_size()).filter(|s| s.x >= 1.0 && s.y >= 1.0).map_or(canvas, |s| [s.x, s.y]);
        (canvas, px)
    }

    fn frame(&mut self, delta: f64, services: &crate::services::Services, out: &mut Outbox) {
        let start = std::time::Instant::now();
        let (Ok(view), Ok(feed)) = (services.view.try_borrow(), services.feed.try_borrow()) else { return };
        let Ok(quality) = services.quality.try_borrow().map(|q| q.clone()) else { return };
        let (reduce_flashing, shake_scale, roll_scale, behind_plot) = match services.settings.try_borrow() {
            Ok(s) => (s.bool("access.reduce_flashing"), if s.bool("access.reduce_motion") { 0.0 } else { s.float("access.camera_shake") as f32 }, s.float("access.camera_roll") as f32, s.bool("graphics.render_behind_plot")),
            Err(_) => (false, 1.0, 1.0, false),
        };
        let (canvas, pixels) = self.viewport_sizes();
        let hide = self.view == View::Plot && !behind_plot;
        self.show_3d(!hide);
        let Some(scene) = self.scene.as_mut() else { return };

        // The map: from the driver if one was given, else worked out from the sortie intro.
        let key = match (&self.map_given, &feed.intro) {
            (Some(_), _) => Some(MapKey::Given),
            (None, Some(intro)) => Some(MapKey::Sortie { mode: intro.mode, seed: intro.seed, operation: intro.operation.clone(), players: feed.panel.as_ref().map_or(1, |p| p.players.len().max(1)) }),
            (None, None) => None,
        };
        if key.is_some() && key != self.map_key {
            let map = match key.as_ref() {
                Some(MapKey::Sortie { mode, seed, operation, players }) => Some(picture::sortie_map(&services.campaign(), *mode, *seed, operation, *players)),
                _ => self.map_given.clone(),
            };
            if let Some(map) = map { scene.scenery.set_map(&map); scene.ocean.set_map(&map); }
            self.map_key = key;
        }

        // What this frame's events mean for the camera.
        let followed = self.orbit.follow.and_then(|e| view.vessels.iter().find(|v| v.entity == e)).or_else(|| view.own.and_then(|i| view.vessels.get(i as usize)));
        let followed_id = followed.map(|v| v.entity);
        let eye = self.lens.map(|l| self.lens_origin.to_world(l.pos.into()));
        for ev in &feed.events {
            match &ev.event {
                SimEvent::SortieStarted { .. } => { self.rig.reset(); self.shakes.clear(); self.orbit.follow = None; }
                SimEvent::Phys(p) => self.shakes.push(ev.tick, shake_of(p, followed_id, eye)),
                _ => {}
            }
        }
        // Felt when playback reaches the event, with the flash and the hole (`camera::ShakeQueue`).
        self.rig.shake.add(self.shakes.take_due(view.render_tick));

        // 1. Camera.
        let sim_time = view.sim_time_s;
        let focus = followed.map(|v| {
            // From this frame's pose, not the hull unit's boxes: those are last frame's until it updates,
            // and across an origin rebase they are in the old origin's space.
            let pose = crate::render::hull::gpu::pose_affine(v.pos, v.rot);
            let size = scene.hull.core.vessels.get(&v.entity).map(|h| Vec3::from(h.design.size_m())).filter(|s| s.is_finite());
            // Until the design is known the orbit turns about a point a deck's height above the pose.
            let half = size.map_or(Vec3::new(0.0, 3.0, 0.0), |s| 0.5 * s);
            camera::Focus { centre: view.origin.to_world(pose.transform_point3(half).into()), up: pose.transform_vector3(Vec3::Y), bow: pose.transform_vector3(Vec3::X), half_m: size.map_or(Vec3::ZERO, |s| 0.5 * s) }
        });
        // The opening distance of a sortie fits the viewer's own hull, once, unless the player zoomed first.
        if !self.rig.framed && self.orbit.follow.is_none() && self.free_camera.is_none() {
            if let Some(f) = focus.filter(|f| f.half_m != Vec3::ZERO) {
                self.orbit.distance_m = camera::framing_distance(2.0 * f.half_m);
                self.rig.framed = true;
            }
        }
        let pose = {
            let ocean = &scene.ocean;
            self.rig.frame(&self.orbit, self.free_camera, focus, delta, shake_scale, roll_scale, &|x, z| ocean.height_at(x, z, sim_time), &|x, z| ocean.ground_height(x, z))
        };
        let lens = camera::Lens::look(Vec3::from(view.origin.to_render(pose.pos)), Vec3::from(view.origin.to_render(pose.target)), pose.roll_rad, camera::FOV_Y_DEG, canvas);
        let state = CameraState { pos: pose.pos.into(), forward: lens.forward.into(), up: lens.up.into(), fov_y_deg: camera::FOV_Y_DEG, viewport_px: pixels, valid: true };
        if let Some(cam) = self.camera.as_mut().filter(|c| c.is_instance_valid()) {
            let xf = Transform3D::new(Basis::from_cols(v3(lens.right), v3(lens.up), v3(-lens.forward)), v3(lens.pos));
            if cam.is_inside_tree() { cam.set_global_transform(xf); } else { cam.set_transform(xf); }
        }
        services.camera.set(state);
        self.lens = Some(lens);
        self.lens_origin = view.origin;
        self.sim_time_s = sim_time;

        // 2. The render units, in the contract's order. Every one runs every frame: events last one frame.
        let ctx = FrameContext { view: &view, events: &feed.events, camera: state, delta_s: delta, scenario: self.scenario, quality: &quality, degrade_step: quality.degrade_step, reduce_flashing };
        let has_camera = self.camera.as_ref().is_some_and(|c| c.is_instance_valid());
        match self.warm {
            Warm::Requested { waited } if has_camera || waited >= WARM_CAMERA_WAIT_FRAMES => {
                scene.vfx.warm_up();
                scene.particles.warm_up();
                self.warm = Warm::Drawing(WARM_FRAMES);
                self.warm_held = 0;
            }
            Warm::Requested { waited } => self.warm = Warm::Requested { waited: waited + 1 },
            _ => {}
        }
        let mut budget = UploadBudget::new(UPLOAD_BUDGET_US);
        scene.hull.update(&ctx, &mut budget);
        scene.parts.update(&ctx);
        for kind in [hull_mesh::parts::AnchorKind::Muzzle, hull_mesh::parts::AnchorKind::PumpOutlet] {
            scene.parts.anchors(kind, &mut self.anchors);
            scene.vfx.set_anchors(&self.anchors);
        }
        let ends = scene.projectiles.update(&ctx);
        scene.vfx.update(&ctx, &ends);
        scene.particles.update(&ctx);
        let masks = scene.hull.hull_masks(&ctx);
        scene.ocean.update(&ctx, &masks, &scene.vfx.core.wakes);
        let lightning = scene.scenery.update(&ctx);
        if let (Some(l), Ok(mut audio)) = (lightning.as_ref(), services.audio.try_borrow_mut()) { audio.play(thunder_for(l)); }

        // 3. What the HUD reads: markers, the aim solution, counters.
        let top = |e: EntityId| scene.hull.bounds(e).map(|(lo, hi)| Vec3::new(0.5 * (lo[0] + hi[0]), hi[1], 0.5 * (lo[2] + hi[2])));
        picture::track_markers(&view, view.snapshot.as_deref(), &lens, &top, &mut self.markers);
        picture::pack_markers(&self.markers, &mut self.markers_packed);
        self.aim = solve_aim(scene, &view, &lens, self.reticle, &self.markers, services).unwrap_or_default();
        services.aim_local.set(self.aim.aim_local);
        let (h, p, j, v, b) = (scene.hull.stats(), scene.parts.stats(), scene.projectiles.stats(), scene.vfx.stats(), scene.particles.stats());
        // The warm-up is done when its instances have been in view for a few frames and, in a sortie, the
        // first hull has been drawn too (its material compiles on first sight), or that has taken too long.
        match self.warm {
            Warm::Drawing(0) if h.chunks_drawn == 0 && self.warm_held < WARM_HULL_WAIT_FRAMES => self.warm_held += 1,
            Warm::Drawing(0) => { self.warm = Warm::Idle; out.warmed = true; }
            Warm::Drawing(n) => self.warm = Warm::Drawing(n - 1),
            _ => {}
        }
        self.extrapolated = feed.status.extrapolated();

        out.rebased = view.origin_shift.map(|s| Vector3::new(s[0], s[1], s[2]));
        if scene.scenery.rev() != self.env_rev { self.env_rev = scene.scenery.rev(); out.environment = true; }
        out.lightning = lightning;
        let numbers = number_quads(scene, &view, &lens, &self.numbers);
        drop(feed);
        drop(view);
        self.place_numbers(numbers);

        let micros = start.elapsed().as_micros().min(u32::MAX as u128) as u32;
        self.stats = RenderStats { chunks_drawn: h.chunks_drawn, chunk_uploads_pending: h.chunk_uploads_pending, part_instances: p.part_instances, projectile_instances: j.projectile_instances, vfx_instances: v.vfx_instances, particles: b.particles, lights: v.lights, adapter_us: micros };
        if self.adapter_us.len() < ADAPTER_SAMPLES { self.adapter_us.push(micros); } else { self.adapter_us[self.adapter_next % ADAPTER_SAMPLES] = micros; }
        self.adapter_next = (self.adapter_next + 1) % ADAPTER_SAMPLES;
    }

    /// Create, move and hide the hull-number labels for this frame's vessels.
    fn place_numbers(&mut self, wanted: Vec<NumberSet>) {
        let gone: Vec<EntityId> = self.numbers.keys().filter(|e| !wanted.iter().any(|w| w.vessel == **e)).copied().collect();
        for e in gone { if let Some(labels) = self.numbers.remove(&e) { for mut n in labels { if n.label.is_instance_valid() { n.label.queue_free(); } } } }
        for set in wanted {
            if let Some(quads) = set.remake {
                if let Some(old) = self.numbers.remove(&set.vessel) { for mut n in old { if n.label.is_instance_valid() { n.label.queue_free(); } } }
                let font = self.number_font.get_or_insert_with(|| {
                    if !godot::classes::ResourceLoader::singleton().exists(NUMBER_FONT) { return None; }
                    godot::tools::try_load::<godot::classes::Font>(NUMBER_FONT).ok()
                }).clone();
                let mut made = Vec::with_capacity(quads.len());
                for (quad, colour) in quads {
                    let mut label = Label3D::new_alloc();
                    label.set_text(quad.text.as_str());
                    if let Some(font) = &font { label.set_font(font); }
                    label.set_font_size(NUMBER_FONT_PX);
                    label.set_pixel_size(quad.size_m[1].max(0.05) / NUMBER_FONT_PX as f32);
                    label.set_outline_size(0);
                    label.set_modulate(Color::from_rgb(colour[0], colour[1], colour[2]).linear_to_srgb());
                    label.set_draw_flag(godot::classes::label_3d::DrawFlags::SHADED, true);
                    label.set_draw_flag(godot::classes::label_3d::DrawFlags::DOUBLE_SIDED, false);
                    label.set_alpha_cut_mode(godot::classes::label_3d::AlphaCutMode::DISCARD);
                    label.set_texture_filter(godot::classes::base_material_3d::TextureFilter::LINEAR_WITH_MIPMAPS_ANISOTROPIC);
                    label.set_layer_mask(crate::render::LAYER_HULL);
                    label.set_cast_shadows_setting(godot::classes::geometry_instance_3d::ShadowCastingSetting::OFF);
                    self.base_mut().add_child(&label);
                    made.push(NumberLabel { label, local: number_frame(&quad), quad });
                }
                self.numbers.insert(set.vessel, made);
            }
            let Some(labels) = self.numbers.get_mut(&set.vessel) else { continue };
            for n in labels.iter_mut() {
                if !n.label.is_instance_valid() { continue; }
                n.label.set_visible(set.visible);
                if set.visible { n.label.set_transform(crate::render::hull::gpu::transform(&(set.pose * n.local))); }
            }
        }
    }

    /// The ray through a viewport position: origin and unit direction in render space.
    fn ray(&self, screen: Vector2) -> Option<(camera::Lens, Vec3)> {
        let lens = self.lens?;
        if !(screen.x.is_finite() && screen.y.is_finite()) { return None; }
        Some((lens, lens.ray([screen.x, screen.y])))
    }

    pub(crate) fn pick_at(&self, screen: Vector2) -> Picked {
        let (Some(scene), Some((lens, dir))) = (self.scene.as_ref(), self.ray(screen)) else { return Picked::default() };
        let services = crate::services::services();
        let Ok(view) = services.view.try_borrow() else { return Picked::default() };
        let mut picked = pick_ray(scene, &view, self.lens_origin, &lens, dir, self.sim_time_s, false);
        // Track symbols are click targets too: a far hull is a few pixels, its symbol is not.
        if matches!(picked.kind, PickKind::None | PickKind::Sea) {
            let near = self.markers.iter().filter(|m| m.on_screen).map(|m| (m, (m.screen[0] - screen.x).hypot(m.screen[1] - screen.y))).filter(|(_, d)| *d <= PICK_MARKER_PX).min_by(|a, b| a.1.total_cmp(&b.1));
            if let Some((m, _)) = near {
                let world = self.lens_origin.to_world(m.pos.into());
                let vessel = view.vessels.iter().find(|v| v.track == m.track).map_or(EntityId::NONE, |v| v.entity);
                picked = Picked { kind: PickKind::Track, map: [world.x, world.z], track: m.track, vessel, slot: None, pos: m.pos };
            }
        }
        picked
    }
}

/// How hard one event shakes the camera, 0..=1: what happens to the followed hull, and bursts near the eye.
fn shake_of(event: &PhysEvent, followed: Option<EntityId>, eye: Option<DVec3>) -> f32 {
    let on = |vessel: &EntityId| followed == Some(*vessel);
    let near = |pos: &DVec3, reach_m: f64, most: f32| eye.map_or(0.0, |e| { let d = (*pos - e).length(); if d.is_finite() && d < reach_m { most * (1.0 - d / reach_m) as f32 } else { 0.0 } });
    match event {
        PhysEvent::MountFired { vessel, rounds, .. } if on(vessel) => 0.06 + 0.02 * (*rounds).min(4) as f32,
        PhysEvent::ShellImpact { vessel, .. } if on(vessel) => 0.35,
        PhysEvent::BodyImpact { vessel, .. } if on(vessel) => 0.8,
        PhysEvent::CookOff { vessel, .. } if on(vessel) => 1.0,
        PhysEvent::CookOff { pos, .. } => near(pos, 600.0, 0.6),
        PhysEvent::BodyBurst { pos, .. } => near(pos, 300.0, 0.5),
        PhysEvent::Collision { a, b, .. } if on(a) || on(b) => 0.5,
        PhysEvent::Grounded { vessel, .. } if on(vessel) => 0.6,
        _ => 0.0,
    }
}

/// Cast a ray (render space) at the drawn world: hulls by their cells, small bodies as spheres, then the
/// sea. `skip_own` leaves the viewer's hull out (aiming through one's own superstructure).
fn pick_ray(scene: &Scene, view: &ViewFrame, origin: RenderOrigin, lens: &camera::Lens, dir: Vec3, sim_time_s: f64, skip_own: bool) -> Picked {
    let o = lens.pos;
    let mut best: Option<(f32, Picked)> = None;
    let mut consider = |t: f32, p: Picked| { if t.is_finite() && best.is_none_or(|b| t < b.0) { best = Some((t, p)); } };
    for v in &view.vessels {
        if skip_own && v.own { continue; }
        let Some((lo, hi)) = scene.hull.bounds(v.entity) else { continue };
        if camera::ray_box(o, dir, Vec3::from(lo), Vec3::from(hi)).is_none() { continue; }
        let Some((cell, _face, t)) = scene.hull.raycast(v.entity, o.into(), dir.into()) else { continue };
        let pos = o + dir * t;
        let world = origin.to_world(pos.into());
        let base = Picked { map: [world.x, world.z], vessel: v.entity, pos, ..Picked::default() };
        if v.own {
            let slot = scene.hull.core.vessels.get(&v.entity).and_then(|h| picture::part_at_cell(&h.design.parts, cell, &|id| content::catalogue::catalogue().part(id).map(|d| d.size)));
            consider(t, Picked { kind: PickKind::Own, slot, ..base });
        } else if !v.track.is_none() {
            consider(t, Picked { kind: PickKind::Track, track: v.track, ..base });
        } else {
            // A drawn hull the viewer holds no track on still blocks what is behind it.
            consider(t, Picked { kind: PickKind::None, ..base });
        }
    }
    for b in &view.bodies {
        if b.track.is_none() { continue; }
        let Some((t, miss)) = camera::ray_point(o, dir, Vec3::from(b.pos)) else { continue };
        if miss <= PICK_BODY_M.max(PICK_BODY_PX / lens.px_per_m(t)) {
            let world = origin.to_world(b.pos);
            consider(t, Picked { kind: PickKind::Track, map: [world.x, world.z], track: b.track, vessel: b.entity, slot: None, pos: Vec3::from(b.pos) });
        }
    }
    let eye = origin.to_world(o.into());
    if let Some(hit) = scene.ocean.raycast(eye.into(), dir.into(), sim_time_s) {
        let pos = Vec3::from(origin.to_render(DVec3::from(hit)));
        consider((pos - o).length(), Picked { kind: PickKind::Sea, map: [hit[0], hit[2]], pos, ..Picked::default() });
    }
    best.map_or(Picked::default(), |b| b.1)
}

/// The manual aim solution for the mount under the player's hand, if there is one.
fn solve_aim(scene: &Scene, view: &ViewFrame, lens: &camera::Lens, reticle: Vector2, markers: &[picture::Marker], services: &crate::services::Services) -> Option<picture::AimReadout> {
    let snapshot = view.snapshot.as_ref()?;
    let ship = snapshot.ship.as_ref()?;
    let own = snapshot.vessels.iter().find(|v| v.entity == ship.vessel)?;
    let hash = scene.hull.core.vessels.get(&ship.vessel).map(|v| v.design.hash)?;
    let designs = services.designs.try_borrow().ok()?;
    let crate::worker::ReceivedDesign::Full { design, .. } = designs.get(&hash)? else { return None };
    let mount = picture::held_mount(ship, &design.mounts)?;
    let catalogue = content::catalogue::catalogue();
    let drag_k = catalogue.parts.get_index(mount.part.index()).and_then(|(_, d)| match &d.function { content::part::PartFunction::Gun(g) => Some(g.band), _ => None })
        .and_then(|band| catalogue.bands.iter().find(|b| b.band == band)).map(|b| b.drag_k).filter(|k| k.is_finite() && *k >= 0.0).unwrap_or(0.0);
    // A reticle that was never placed is the centre of the view.
    let screen = if reticle == Vector2::ZERO || !(reticle.x.is_finite() && reticle.y.is_finite()) { [0.5 * lens.canvas[0], 0.5 * lens.canvas[1]] } else { [reticle.x, reticle.y] };
    let dir = lens.ray(screen);
    let hit = pick_ray(scene, view, view.origin, lens, dir, view.sim_time_s, true);
    let at = match hit.kind {
        PickKind::None if hit.vessel.is_none() => {
            // Open sky: a track symbol under the reticle is still what the player means.
            match markers.iter().filter(|m| m.on_screen).find(|m| (m.screen[0] - screen[0]).hypot(m.screen[1] - screen[1]) <= PICK_MARKER_PX) {
                Some(m) => picture::AimAt::Point { world: view.origin.to_world(m.pos.into()), track: m.track },
                None => picture::AimAt::Sky { dir: dir.as_dvec3() },
            }
        }
        _ => picture::AimAt::Point { world: view.origin.to_world(hit.pos.into()), track: hit.track },
    };
    Some(picture::aim_readout(&own.kin.pose, mount, drag_k, at))
}

/// The hull numbers of one drawn vessel this frame. `remake` holds the quads, with the paint each is drawn
/// in, only when the labels kept for the vessel no longer match them.
struct NumberSet { vessel: EntityId, pose: glam::Affine3A, visible: bool, remake: Option<Vec<(hull_mesh::pipeline::MarkingQuad, [f32; 3])>> }

/// Every drawn vessel that carries hull numbers, with whether they are tall enough on screen to draw.
fn number_quads(scene: &Scene, view: &ViewFrame, lens: &camera::Lens, have: &BTreeMap<EntityId, Vec<NumberLabel>>) -> Vec<NumberSet> {
    let is_number = |q: &&hull_mesh::pipeline::MarkingQuad| q.kind == content::blueprint::MarkingKind::HullNumber && !q.text.is_empty();
    let mut out = Vec::new();
    for v in &view.vessels {
        let quads = scene.hull.marking_quads(v.entity);
        if !quads.iter().any(|q| is_number(&q)) { continue; }
        let Some(hull) = scene.hull.core.vessels.get(&v.entity) else { continue };
        let pose = crate::render::hull::gpu::pose_affine(v.pos, v.rot);
        let distance = (Vec3::from(v.pos) - lens.pos).length();
        let tallest = quads.iter().filter(is_number).map(|q| q.size_m[1]).fold(0.0f32, f32::max);
        let visible = tallest * lens.px_per_m(distance) >= NUMBER_MIN_PX;
        let current = have.get(&v.entity).is_some_and(|labels| labels.len() == quads.iter().filter(is_number).count() && labels.iter().zip(quads.iter().filter(is_number)).all(|(l, q)| l.quad == *q));
        let remake = if current { None } else {
            Some(quads.iter().filter(is_number).map(|q| (q.clone(), hull.design.palette.get(q.paint as usize).copied().unwrap_or([0.8, 0.8, 0.8]))).collect())
        };
        out.push(NumberSet { vessel: v.entity, pose, visible, remake });
    }
    out
}

/// Vessel-local frame of a hull-number label: text runs along the quad's `right`, reads from outside the
/// hull (a `Label3D` faces +Z), and is centred on the quad.
fn number_frame(quad: &hull_mesh::pipeline::MarkingQuad) -> glam::Affine3A {
    let n = Vec3::from(quad.normal).normalize_or(Vec3::Z);
    let mut r = Vec3::from(quad.right);
    r = (r - n * r.dot(n)).normalize_or(n.any_orthonormal_vector());
    let u = n.cross(r);
    glam::Affine3A::from_cols(r.into(), u.into(), n.into(), Vec3::from(quad.centre).into())
}

/// `&"dusk_clear"` and the like: time of day, then weather, in the words the generated LUT files use.
fn preset_name(weather: content::weather::WeatherKind, time: content::enums::TimeOfDay) -> String {
    use content::enums::TimeOfDay;
    use content::weather::WeatherKind;
    let w = match weather { WeatherKind::Clear => "clear", WeatherKind::Haze => "haze", WeatherKind::Overcast => "overcast", WeatherKind::Rain => "rain", WeatherKind::Storm => "storm", WeatherKind::FogBank => "fog" };
    let t = match time { TimeOfDay::Night => "night", TimeOfDay::Dusk => "dusk", TimeOfDay::Day => "day" };
    format!("{t}_{w}")
}

/// The 99th percentile of the kept adapter times, microseconds.
fn p99(samples: &[u32]) -> u32 {
    if samples.is_empty() { return 0; }
    let mut sorted = samples.to_vec();
    sorted.sort_unstable();
    sorted[((sorted.len() as f64 * 0.99).ceil() as usize).clamp(1, sorted.len()) - 1]
}

#[godot_api]
impl BpWorldView {
    /// The view switched; `view` is `Bp.VIEW_*`.
    #[signal]
    fn view_changed(view: i64);
    /// The render origin moved by `shift` (render space, metres). Anything GDScript keeps in 3D must add it.
    #[signal]
    fn origin_rebased(shift: Vector3);
    /// `environment()` changed enough to re-apply (weather turn, blend step at 1 Hz).
    #[signal]
    fn environment_changed();
    /// A lightning pulse starts now: `intensity` 0..1, `duration_s` 0.08 to 0.15, `bearing_deg` true.
    #[signal]
    fn lightning(intensity: f64, duration_s: f64, bearing_deg: f64);
    /// Every material and effect has been drawn once after loading (L1358); the loading screen may close.
    #[signal]
    fn warmed_up();

    // ---- Views and camera ----

    /// Give the node the camera it moves. Call once from the scene script's `_ready`.
    ///
    /// The node sets the camera's field of view (60 degrees vertical, height kept), a 0.5 m near plane and
    /// a far plane of at least 20,000 m (L1873), and from then on its transform every frame of a sortie.
    #[func]
    fn bind_camera(&mut self, camera: Gd<Camera3D>) {
        let mut camera = camera;
        camera.set_keep_aspect_mode(godot::classes::camera_3d::KeepAspect::HEIGHT);
        camera.set_fov(camera::FOV_Y_DEG);
        camera.set_near(CAMERA_NEAR_M);
        if camera.get_far() < hull_mesh::env::ocean::MIN_FAR_M as f32 { camera.set_far(hull_mesh::env::ocean::MIN_FAR_M as f32); }
        self.camera = Some(camera);
    }
    /// `Bp.VIEW_BRIDGE` or `Bp.VIEW_PLOT`.
    #[func]
    fn set_view(&mut self, view: i64) {
        let Some(v) = View::from_gd(view) else { return };
        if v == self.view { return; }
        self.view = v;
        let behind = crate::services::services().settings.try_borrow().is_ok_and(|s| s.bool("graphics.render_behind_plot"));
        self.show_3d(!(v == View::Plot && !behind));
        self.base_mut().emit_signal("view_changed", &[v.to_gd().to_variant()]);
    }
    #[func]
    fn view(&self) -> i64 { use crate::enums::GdEnum; self.view.to_gd() }
    /// Switch between the two views (the `view_switch` action). Returns the new view.
    #[func]
    fn toggle_view(&mut self) -> i64 {
        let next = if self.view == View::Bridge { View::Plot } else { View::Bridge };
        self.set_view(next.to_gd());
        self.view.to_gd()
    }
    /// Orbit by a delta in degrees (mouse drag, right stick). No inertia.
    #[func]
    fn orbit(&mut self, yaw_deg: f64, pitch_deg: f64) { camera::orbit_by(&mut self.orbit, yaw_deg, pitch_deg); }
    /// Zoom the orbit distance by wheel steps (positive = closer), clamped to 8 m .. 2,000 m.
    ///
    /// A sortie opens at the distance that frames the viewer's hull; the first zoom takes the distance
    /// over for the rest of the sortie. The camera itself never comes inside the followed hull's box, so
    /// on a large ship the nearest view is from just off its side; where the box holds the camera out, the
    /// steps count from where it stands.
    #[func]
    fn zoom(&mut self, steps: f64) {
        if steps.is_finite() && steps != 0.0 { self.rig.framed = true; }
        camera::zoom_by(&mut self.orbit, steps, self.rig.stood_off_m);
    }
    /// Returns `{ "yaw_deg": float, "pitch_deg": float, "distance_m": float, "follow": int }`.
    #[func]
    fn orbit_state(&self) -> VarDictionary {
        crate::convert::d! {
            "yaw_deg" => self.orbit.yaw_deg as f64,
            "pitch_deg" => self.orbit.pitch_deg as f64,
            "distance_m" => self.orbit.distance_m as f64,
            "follow" => self.orbit.follow.map_or(-1, crate::convert::entity_to_gd),
        }
    }
    /// Orbit around another drawn vessel (replays, spectating, kill cam); `Bp.NONE` returns to the own hull.
    #[func]
    fn follow(&mut self, vessel: i64) -> bool {
        let Some(entity) = crate::convert::entity_from_gd(vessel).filter(|e| !e.is_none()) else { self.orbit.follow = None; return true };
        let drawn = crate::services::services().view.try_borrow().is_ok_and(|v| v.vessels.iter().any(|p| p.entity == entity));
        if drawn { self.orbit.follow = Some(entity); }
        drawn
    }
    /// Free camera for replays and captures: position and look-at target as map position plus height in
    /// metres. `enabled` false returns to the orbit camera.
    #[func]
    fn set_free_camera(&mut self, enabled: bool, pos: Vector2, height_m: f64, look_at: Vector2, look_height_m: f64) {
        let (p, l) = ([pos.x as f64, height_m, pos.y as f64], [look_at.x as f64, look_height_m, look_at.y as f64]);
        self.free_camera = if enabled && p.iter().chain(&l).all(|v| v.is_finite()) { Some((p, l)) } else { None };
    }

    // ---- Picking and projection (L1094) ----

    /// What is under a viewport position. Returns `{ "kind": StringName, "map": Vector2, "track": int,
    /// "vessel": int, "slot": int, "pos": Vector3 }` where `kind` is `&"none"`, `&"sea"`, `&"track"` (a
    /// drawn vessel or body the viewer holds as a track) or `&"own"` (the viewer's hull; `slot` is the part
    /// under the cursor or `Bp.NONE`). Ids not relevant to the kind are `Bp.NONE`.
    ///
    /// A track is also picked through its bridge-view symbol: within 20 canvas units of the marker when no
    /// hull is under the cursor. `pos` is in render space.
    #[func]
    fn pick(&self, screen: Vector2) -> VarDictionary {
        let p = self.pick_at(screen);
        let (track, vessel) = match p.kind { PickKind::Track => (p.track, p.vessel), PickKind::Own => (TrackId::NONE, p.vessel), _ => (TrackId::NONE, EntityId::NONE) };
        crate::convert::d! {
            "kind" => StringName::from(p.kind.name()),
            "map" => Vector2::new(p.map[0] as f32, p.map[1] as f32),
            "track" => crate::convert::track_to_gd(track),
            "vessel" => if vessel.is_none() { -1 } else { crate::convert::entity_to_gd(vessel) },
            "slot" => p.slot.map_or(-1, crate::convert::slot_to_gd),
            "pos" => v3(p.pos),
        }
    }
    /// The sea point under a viewport position as a map position, or `null` above the horizon.
    #[func]
    fn sea_point(&self, screen: Vector2) -> Variant {
        let (Some(scene), Some((lens, dir))) = (self.scene.as_ref(), self.ray(screen)) else { return Variant::nil() };
        let eye = self.lens_origin.to_world(lens.pos.into());
        match scene.ocean.raycast(eye.into(), dir.into(), self.sim_time_s) {
            Some(hit) => Vector2::new(hit[0] as f32, hit[2] as f32).to_variant(),
            None => Variant::nil(),
        }
    }
    /// Viewport position of a map position at a height in metres; `Vector2(-1, -1)` when behind the camera.
    #[func]
    fn project(&self, map: Vector2, height_m: f64) -> Vector2 {
        let Some(lens) = self.lens else { return Vector2::new(-1.0, -1.0) };
        let p = self.lens_origin.to_render(DVec3::new(map.x as f64, height_m, map.y as f64));
        lens.project(p.into()).map_or(Vector2::new(-1.0, -1.0), |s| Vector2::new(s[0], s[1]))
    }
    /// Track markers for the bridge-view overlay, rebuilt each frame the camera or the picture moved.
    /// Layout: 6 floats per track, ascending track number: `[track, x, y, on_screen (0 or 1), range_m,
    /// rank]`; off-screen tracks are clamped to the viewport edge. Stance symbols are drawn by the HUD from
    /// `Hud.track(id)`; no 3D model is ever used as an icon (rule 13).
    ///
    /// `rank` is the declutter order of rule 14, lowest kept longest: 0 the designated track, 1 an inbound
    /// weapon, 2 Hostile inside the longest own weapon envelope, 3 Hostile outside it, 4 Suspect, 5 a track
    /// still at the Unknown rung, 6 Neutral and Friend. `range_m` is from the viewer's hull. A track drawn
    /// as a vessel has its marker above that hull; edge markers sit 28 canvas units inside the viewport.
    #[func]
    fn track_markers(&self) -> PackedFloat32Array { PackedFloat32Array::from(self.markers_packed.as_slice()) }
    /// Map position of the render origin (for GDScript that places helper nodes).
    #[func]
    fn render_origin(&self) -> Vector2 { Vector2::new(self.lens_origin.x as f32, self.lens_origin.z as f32) }
    /// Render-space position of a map position at a height.
    #[func]
    fn to_render(&self, map: Vector2, height_m: f64) -> Vector3 {
        let p = self.lens_origin.to_render(DVec3::new(map.x as f64, height_m, map.y as f64));
        Vector3::new(p[0], p[1], p[2])
    }

    // ---- Manual aim (L949-953) ----

    /// Viewport position of the aim reticle while a mount or group is held. `Orders.fire_at_reticle` and the
    /// 30 Hz input use the ray through it, expressed in the vessel-local frame.
    ///
    /// A reticle that was never set, or `Vector2.ZERO`, is the centre of the view. The direction that goes
    /// to the simulation (`Services::aim_local`) runs from the held mount to the point this ray reaches,
    /// which is what the simulation lays the gun on.
    #[func]
    fn set_reticle(&mut self, screen: Vector2) { self.reticle = screen; }
    /// Where the held mount's shot would land on the sea or a track: `{ "valid": bool, "map": Vector2,
    /// "range_m": float, "bearing_deg": float, "track": int, "in_arc": bool, "time_of_flight_s": float }`.
    ///
    /// `valid` is false when no mount or group is held, the viewer's own design is not known in full, or
    /// the reticle is on open sky. `range_m` is horizontal from the mount and `bearing_deg` true. `in_arc`
    /// is false when the mount's training or elevation limits, or its reach, rule the point out.
    #[func]
    fn aim_readout(&self) -> VarDictionary {
        let a = &self.aim;
        crate::convert::d! {
            "valid" => a.valid,
            "map" => Vector2::new(a.map[0] as f32, a.map[1] as f32),
            "range_m" => a.range_m,
            "bearing_deg" => a.bearing_deg,
            "track" => crate::convert::track_to_gd(a.track),
            "in_arc" => a.in_arc,
            "time_of_flight_s" => a.time_of_flight_s,
        }
    }

    // ---- Environment and quality ----

    /// Returns `EnvironmentDict`. Re-read on `environment_changed`.
    #[func]
    fn environment(&self) -> VarDictionary {
        let default = crate::render::scenery::EnvironmentState::default();
        let env = self.scene.as_ref().map_or(&default, |s| s.scenery.environment());
        let time = content::enums::TimeOfDay::from_sun_elevation_deg(env.weather.sun_elevation_deg);
        crate::convert::d! {
            "weather" => crate::convert::weather_dict(&env.weather),
            "blend_from" => env.blend_from.to_gd(),
            "blend_to" => env.blend_to.to_gd(),
            "blend" => env.blend as f64,
            "time_of_day" => time.to_gd(),
            "sun_dir" => Vector3::new(env.sun_dir[0], env.sun_dir[1], env.sun_dir[2]),
            "fog_density" => env.fog_density as f64,
            "fog_sun_scatter" => env.fog_sun_scatter as f64,
            "aerial_perspective" => env.aerial_perspective as f64,
            "wind_offset" => Vector2::new(env.wind_offset_m[0], env.wind_offset_m[1]),
            "cloud_seed" => env.cloud_seed as i64,
            "preset" => StringName::from(preset_name(env.blend_from, time).as_str()),
            "preset_to" => StringName::from(preset_name(env.blend_to, time).as_str()),
        }
    }
    /// The colour-grading LUT for the current environment (`SceneryRenderer::lut`): one `ImageTexture3D`,
    /// 17 cubed (33 cubed on High), blended on the CPU between presets at 1 Hz and updated in place, so the
    /// scene script assigns it to `Environment.adjustment_color_correction` once. `null` before the first
    /// weather event or when `cargo xtask gen` has not written the LUT files.
    #[func]
    fn lut(&self) -> Option<Gd<godot::classes::ImageTexture3D>> { self.scene.as_ref().and_then(|s| s.scenery.lut()) }
    /// Re-read the quality profile after `Quality.changed` and pass the tier on: `set_tier` on the
    /// projectile and VFX units, the particle cap, `OceanRenderer::set_spec(ClipmapSpec::for_tier(tier))`;
    /// the hull unit reads the tier from `FrameContext::quality` for its LOD pick.
    #[func]
    fn apply_quality(&mut self) {
        let Ok(quality) = crate::services::services().quality.try_borrow().map(|q| q.clone()) else { return };
        let scenario = self.scenario;
        let Some(scene) = self.scene.as_mut() else { return };
        let tier: QualityTier = quality.tier;
        self.tier_applied = Some((tier, quality.forward_plus));
        scene.projectiles.set_tier(tier);
        scene.vfx.set_tier(tier);
        let cap = hull_mesh::vfx::layer_b_particle_cap(tier) as u32;
        // Layer B exists only on Forward+ with a non-zero cap; crossing that line rebuilds the unit.
        if (quality.forward_plus && cap > 0) != scene.particles.enabled {
            scene.particles.free();
            scene.particles = crate::render::particles::ParticleRenderer::new(scenario, tier, quality.forward_plus);
        }
        scene.particles.set_cap(cap);
        scene.ocean.set_spec(ClipmapSpec::for_tier(tier));
    }
    /// Draw every material and effect once, off screen; emits `warmed_up` when done.
    ///
    /// Never emits inside the call: the instances are placed on a following frame, in front of the camera
    /// behind the loading cover (a culled instance compiles nothing), and `warmed_up` follows once they have
    /// been in view for a few frames and the first hull of the sortie has been drawn (at most three seconds
    /// are spent waiting for one). With no sortie loaded, or no camera bound within half a second, it still
    /// reports.
    #[func]
    fn warm_up(&mut self) {
        self.warm = Warm::Requested { waited: 0 };
        // Outside the tree nothing processes: report on the next idle step instead of never.
        if !self.base().is_inside_tree() {
            self.warm = Warm::Idle;
            self.base_mut().call_deferred("emit_signal", &["warmed_up".to_variant()]);
        }
    }
    /// Counters for the performance overlay and the benchmark: `{ "adapter_ms_p99": float,
    /// "adapter_ms_last": float, "chunks": int, "chunk_uploads_pending": int, "part_instances": int,
    /// "projectiles": int, "vfx_instances": int, "particles": int, "lights": int, "extrapolated": float }`.
    ///
    /// The adapter times cover this node's frame (camera, every render unit, markers and aim); the p99 is
    /// over the last 600 frames. `extrapolated` is the share of sampled frames that had to extrapolate.
    #[func]
    fn stats(&self) -> VarDictionary {
        let s = &self.stats;
        crate::convert::d! {
            "adapter_ms_p99" => p99(&self.adapter_us) as f64 / 1000.0,
            "adapter_ms_last" => s.adapter_us as f64 / 1000.0,
            "chunks" => s.chunks_drawn as i64,
            "chunk_uploads_pending" => s.chunk_uploads_pending as i64,
            "part_instances" => s.part_instances as i64,
            "projectiles" => s.projectile_instances as i64,
            "vfx_instances" => s.vfx_instances as i64,
            "particles" => s.particles as i64,
            "lights" => s.lights as i64,
            "extrapolated" => self.extrapolated as f64,
        }
    }
}


/// The thunder that follows a lightning flash (the audio feed leaves thunder to the view that draws the flash).
/// A brighter flash is a nearer strike: 1 km at full intensity out to 10 km at none. Thunder arrives at the
/// speed of sound, so the flash always leads; strikes inside 3 km roll as near thunder.
pub(crate) fn thunder_for(l: &crate::render::scenery::Lightning) -> crate::audio_feed::AudioEvent {
    use crate::seams::sound::{mix, AmbienceSfx, SoundId};
    let intensity = if l.intensity.is_finite() { l.intensity.clamp(0.0, 1.0) } else { 0.5 };
    let distance_m = 1_000.0 + (1.0 - intensity) * 9_000.0;
    let near = distance_m < 3_000.0;
    // Thunder is a far louder source than a gun: a gentle fall-off keeps a distant roll audible under the sea bed.
    let volume_db = -12.0 * (distance_m - 1_000.0) / 9_000.0;
    crate::audio_feed::AudioEvent {
        sound: SoundId::Ambience(if near { AmbienceSfx::ThunderNear } else { AmbienceSfx::ThunderFar }),
        volume_db,
        pitch: 1.0,
        pos: None,
        max_distance_m: 0.0,
        delay_s: mix::arrival_delay_s(distance_m),
        priority: 1,
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thunder_follows_the_flash_and_fades_with_distance() {
        use crate::render::scenery::Lightning;
        use crate::seams::sound::{AmbienceSfx, SoundId};
        let near = thunder_for(&Lightning { intensity: 1.0, duration_s: 0.1, bearing_deg: 0.0 });
        let far = thunder_for(&Lightning { intensity: 0.1, duration_s: 0.1, bearing_deg: 0.0 });
        assert_eq!(near.sound, SoundId::Ambience(AmbienceSfx::ThunderNear));
        assert_eq!(far.sound, SoundId::Ambience(AmbienceSfx::ThunderFar));
        assert!(near.delay_s > 2.0 && far.delay_s > near.delay_s, "{} {}", near.delay_s, far.delay_s);
        assert!(near.volume_db > far.volume_db && far.volume_db >= -12.0);
        assert!(thunder_for(&Lightning { intensity: f32::NAN, duration_s: 0.1, bearing_deg: 0.0 }).delay_s.is_finite());
    }

    #[test]
    fn shake_comes_from_what_happens_to_the_followed_hull_and_near_the_eye() {
        let me = EntityId::from_u64(1);
        let other = EntityId::from_u64(2);
        let eye = Some(DVec3::new(0.0, 10.0, 0.0));
        let fired = |vessel| PhysEvent::MountFired { vessel, slot: content::ids::PartSlot(0), rounds: 2 };
        assert!(shake_of(&fired(me), Some(me), eye) > 0.0);
        assert_eq!(shake_of(&fired(other), Some(me), eye), 0.0);
        assert_eq!(shake_of(&fired(me), None, eye), 0.0);
        let burst = |x: f64| PhysEvent::BodyBurst { body: EntityId::from_u64(9), pos: DVec3::new(x, 10.0, 0.0), underwater: false };
        assert!(shake_of(&burst(30.0), Some(me), eye) > shake_of(&burst(250.0), Some(me), eye));
        assert_eq!(shake_of(&burst(400.0), Some(me), eye), 0.0);
        assert_eq!(shake_of(&burst(30.0), Some(me), None), 0.0);
        assert_eq!(shake_of(&burst(f64::NAN), Some(me), eye), 0.0);
        assert_eq!(shake_of(&PhysEvent::Grounded { vessel: me, point: DVec3::ZERO }, Some(me), eye), 0.6);
        assert_eq!(shake_of(&PhysEvent::VesselDespawned { vessel: me }, Some(me), eye), 0.0);
    }

    #[test]
    fn hull_number_frames_read_from_outside_the_hull() {
        use hull_mesh::pipeline::MarkingQuad;
        // A number on the starboard side (+Z normal) of a hull, baseline running aft (-X).
        let quad = MarkingQuad { centre: [20.0, 3.0, 6.002], normal: [0.0, 0.0, 1.0], right: [-1.0, 0.0, 0.0], size_m: [3.0, 1.5], paint: 3, kind: content::blueprint::MarkingKind::HullNumber, text: "42".into() };
        let f = number_frame(&quad);
        // Right-handed, text up is world up, the label's face (+Z) is the outward normal.
        assert!((f.matrix3.x_axis.cross(f.matrix3.y_axis) - f.matrix3.z_axis).length() < 1e-5);
        assert!((Vec3::from(f.matrix3.z_axis) - Vec3::Z).length() < 1e-6);
        assert!((Vec3::from(f.matrix3.y_axis) - Vec3::Y).length() < 1e-6 || (Vec3::from(f.matrix3.y_axis) + Vec3::Y).length() < 1e-6);
        assert_eq!(Vec3::from(f.translation), Vec3::new(20.0, 3.0, 6.002));
        // A degenerate quad still gives an orthonormal frame.
        let bad = MarkingQuad { normal: [0.0; 3], right: [0.0; 3], ..quad };
        let f = number_frame(&bad);
        assert!(f.matrix3.x_axis.is_finite() && (f.matrix3.determinant() - 1.0).abs() < 1e-4);
    }

    #[test]
    fn presets_percentiles_and_pick_names() {
        use content::enums::TimeOfDay;
        use content::weather::WeatherKind;
        assert_eq!(preset_name(WeatherKind::Clear, TimeOfDay::Dusk), "dusk_clear");
        assert_eq!(preset_name(WeatherKind::FogBank, TimeOfDay::Night), "night_fog");
        assert_eq!(p99(&[]), 0);
        assert_eq!(p99(&[700]), 700);
        let mut v: Vec<u32> = (1..=200).collect();
        v.reverse();
        assert_eq!(p99(&v), 198);
        assert_eq!([PickKind::None.name(), PickKind::Sea.name(), PickKind::Track.name(), PickKind::Own.name()], ["none", "sea", "track", "own"]);
    }
}
