//! Unit `gd-render-ocean` (hard: numerically subtle; must match the simulation's waves to millimetres).
//! The sea surface: geometry clipmap, per-frame wave uniforms, foam and shore inputs.
//! DESIGN.md L1885-1911 (ocean), L1116 (the same Gerstner set as sim-core, 12 components), L1889-1891 (phase
//! per component computed in f64 against the render origin each frame; 8 global mat4s plus `t_ref`, about
//! 600 bytes per frame; `time_rollover_secs` 2048), L1893 (tests: f32 shader port within 2 mm of sim-core;
//! Newton `height_at` to 1 mm in 4 steps), L1895-1901 (clipmap levels per tier), L1907 (foam, shore
//! heightfield), L1911 (camera at least 1 m above the surface).
//!
//! The shader side is `res://shaders/water.gdshader` with the generated include
//! `res://shaders/gen/waves.gdshaderinc` (written by `cargo xtask gen` from sim-core's packing). This unit
//! sets the global shader parameters named in that include; the names are fixed by sim-core's emitter.
//!
//! Geometry is `hull_mesh::env::ocean`: `level_mesh(spec, level)` per level and `skirt_mesh(spec, far_m)`,
//! uploaded once per tier through `convert::mesh_arrays` (vertex `CUSTOM0`: x = level spacing, y = morph
//! weight, z = level index, w = 1 on skirt vertices), and `snap_origins(spec, camera_x, camera_z)` per frame
//! for the f64 level centres, from which this unit subtracts the render origin. The clipmap table is
//! `ClipmapSpec::for_tier` there (Fallback uses Low); gdclient keeps no copy of it.
//! Generated inputs: `gen::OCEAN_DETAIL_NORMALS` and `gen::OCEAN_DETAIL_TURBULENCE` (64 frames of 256
//! squared looping every `OCEAN_DETAIL_LOOP_S`, sampled at `OCEAN_DETAIL_SCALES_M`), and
//! `gen::NOISE_CLOUD_SHADOW` as the global shader parameter `bp_cloud_shadow`.
//!
//! # How the unit is built
//! [`core`] holds every decision with no Godot in it (the wave set and the tick it changes at, uniform
//! packing, the Nyquist-fade rule, clipmap placement, the surface queries) and tests headless; [`gpu`]
//! owns the RIDs and sends parameters only when they change; [`shore`] samples the shore heightfield on a
//! thread; [`clock`] is the replica of the shaders' `TIME` (L1891), a node added to the tree root when the
//! main loop starts so that it has counted every drawn frame.
//! - The simulation swaps wave sets in one tick, so the drawn sea does too, at the playback tick of the
//!   `WeatherChanged` event. Whitecap level, rain and cloud cover ease over 20 s.
//! - `bp_waves_t_ref` is the clock's value for the frame, so `TIME - t_ref` is zero at the draw. The clock
//!   needs `_process` deltas that are the steps the renderer draws with, which the engine's default
//!   `max_physics_steps_per_frame` of 8 breaks on every frame over 133 ms; [`clock`] raises it at start-up
//!   and says why. `res://test/tools/world_check.gd` measures the result on the GPU.
//! - The global shader parameters (`bp_waves_0..7`, `bp_waves_t_ref`, `bp_cloud_shadow`,
//!   `bp_cloud_shadow_xf`) are registered at start-up by [`gpu::ensure_globals`] when `project.godot` has no
//!   `[shader_globals]` entry for them.
//! - The shore heightfield needs the sortie map: [`OceanRenderer::set_map`]. Without one the shore branch
//!   stays off.
//! - The owner makes the water material with [`WaterMaterial::load`] and frees it after the renderer.
pub(crate) mod clock;
pub(crate) mod core;
pub(crate) mod gpu;
pub(crate) mod shore;

use super::FrameContext;
pub use gpu::WaterMaterial;
pub use hull_mesh::env::ocean::ClipmapSpec;

pub struct OceanRenderer {
    pub(crate) spec: ClipmapSpec,
    pub(crate) wave_seed: u64,
    pub(crate) core: core::OceanCore,
    pub(crate) shore: shore::Shore,
    pub(crate) gpu: Option<gpu::Gpu>,
}

impl OceanRenderer {
    /// Build the clipmap meshes (levels snap to 2s in f64, morph over the outer n/10 band, skirt to the far
    /// plane) and bind the water material.
    ///
    /// `water_material` is a material on `res://shaders/water.gdshader` ([`WaterMaterial::load`]); the
    /// renderer sets its parameters and textures but does not own it. An invalid RID draws the clipmap with
    /// the engine's default material.
    pub fn new(scenario: godot::prelude::Rid, spec: ClipmapSpec, water_material: godot::prelude::Rid) -> Self {
        let tuning = crate::services::services().catalogue().tuning.waves;
        Self { spec, wave_seed: 0, core: core::OceanCore::new(spec, tuning), shore: shore::Shore::new(), gpu: Some(gpu::Gpu::new(scenario, spec, water_material)) }
    }
    /// Per frame: take wave seed and weather from events, recompute per-component phases in f64 for the
    /// current origin and `view.sim_time_s`, set the global wave uniforms and `t_ref`, move the clipmap to
    /// the camera, and pass the hull masks and the shore heightfield window.
    ///
    /// `wakes` are the hulls' wake tracks from `gd-render-vfx` (`VfxRenderer::wakes`), drawn by the water.
    pub fn update(&mut self, ctx: &FrameContext, masks: &super::hull::HullMasks, wakes: &super::vfx::wake::WakeField) {
        let camera = if ctx.camera.valid { Some(ctx.camera.pos) } else { None };
        let mut frame = self.core.step(ctx.view, ctx.events, camera, ctx.degrade_step, ctx.delta_s);
        frame.foam_streaks = !matches!(ctx.quality.tier, crate::enums::QualityTier::Low | crate::enums::QualityTier::Fallback);
        self.wave_seed = self.core.wave_seed;
        let upload = if self.core.dock.is_some() { None } else { self.shore.step(frame.camera_xz) };
        let window = if self.core.dock.is_some() { None } else { self.shore.window() };
        if let Some(g) = &mut self.gpu { g.apply(&frame, masks, if self.core.dock.is_some() { None } else { Some(wakes) }, upload, window, ctx.delta_s); }
    }
    /// Sea surface height in metres at a map position at the rendered time (camera floor, picking, HUD).
    /// Same wave set as the simulation, evaluated on the client.
    pub fn height_at(&self, world_x: f64, world_z: f64, sim_time_s: f64) -> f64 { self.core.height_at(world_x, world_z, sim_time_s) }
    /// Ray against the displaced surface (4 Newton steps): the hit in world metres.
    ///
    /// `None` when the ray never comes down to the water within 60 km (above the horizon) or is not finite.
    pub fn raycast(&self, origin: [f64; 3], dir: [f32; 3], sim_time_s: f64) -> Option<[f64; 3]> { self.core.raycast(origin, dir, sim_time_s) }
    /// Calm water for the dry dock: flat plane at the design waterline.
    pub fn set_dock_mode(&mut self, on: bool, waterline_m: f32) {
        self.core.dock = if on { Some(if waterline_m.is_finite() { waterline_m } else { 0.0 }) } else { None };
    }
    pub fn set_spec(&mut self, spec: ClipmapSpec) {
        if spec == self.spec { return; }
        self.spec = spec;
        self.core.spec = spec;
        if let Some(g) = &mut self.gpu { g.build(spec); }
    }
    /// The sortie map, for the shore heightfield (shore foam and the shallow tint, L1907). The window is
    /// sampled on a thread and follows the camera; it exists only near land.
    pub fn set_map(&mut self, map: &content::campaign::MapDef) { self.shore.set_map(map); }
    /// Terrain or sea-bed height at a map position from the map given to [`OceanRenderer::set_map`]
    /// (negative under water; open-water depth without a map).
    pub fn ground_height(&self, world_x: f64, world_z: f64) -> f64 { self.shore.ground_height(world_x, world_z) }
    /// The weather in force at the rendered time, once the first `WeatherChanged` has arrived.
    pub fn weather(&self) -> Option<&content::weather::WeatherState> { self.core.weather() }
    pub fn free(&mut self) {
        if let Some(mut g) = self.gpu.take() { g.free(); }
        self.shore.clear();
    }
}
