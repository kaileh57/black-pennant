//! Unit `gd-render-scenery`. Islands, cloud masses and the environment state the GDScript world scene
//! applies. DESIGN.md L1855-1883 (weather table, time of day, fog from visibility, sun fixed per sortie,
//! blends over 20 to 60 s, lightning), L1913-1917 (sky, faceted opaque cloud clusters laid out from the
//! server seed and drifting with the synced wind offset, flat-shaded islands with the same fog), G5 (no 90
//! degree sector of uniform horizon).
//! Presets themselves (sun colour and energy, sky, ambient floor, fog colour, exposure, LUT) are resources
//! under `res://world/presets/`; `res://world/environment.gd` applies them from [`EnvironmentState`].
//!
//! Meshes are `hull_mesh::env::scenery`: `cloud_layout(seed, cloud_cover, storm, radius_m)` then
//! `cloud_cluster(kind, seed, size_m)` per cluster (vertex `CUSTOM0.x` = wrap-lighting term), and
//! `island_mesh(height, origin, min, max, edge_m, bands)` with `sim_core::terrain` heights passed in as the
//! closure and `IslandBands` from the world tokens (vertex colour = height band). Uploaded through
//! `convert::mesh_arrays`.
//! The colour-grading LUT is generated, not a resource: `gen::lut_path(weather, time_of_day, high)` names one
//! `TexBlob` per preset (RGB8, 17 cubed, 33 cubed on High); [`SceneryRenderer::lut`] blends the two presets
//! of the current weather turn on the CPU at 1 Hz into one `ImageTexture3D` (L1846).
//!
//! # How the unit is built
//! [`core`] holds the environment state, blends, wind drift, cloud field, lightning, LUT choice and the
//! island tiles with the unit's meshing thread (`bp-scenery`), with no Godot in it; [`gpu`] uploads meshes
//! (at most a few per frame), places the island and cloud instances relative to the render origin, and
//! blends the LUT into the one `ImageTexture3D`.
pub(crate) mod core;
pub(crate) mod gpu;

use super::FrameContext;
use content::weather::{WeatherKind, WeatherState};
use godot::classes::ImageTexture3D;
use godot::prelude::Gd;

/// What `BpWorldView.environment()` reports (the `EnvironmentDict` shape).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct EnvironmentState {
    pub weather: WeatherState,
    pub blend_from: WeatherKind,
    pub blend_to: WeatherKind,
    /// 0..=1 through a 20 to 60 s blend.
    pub blend: f32,
    pub sun_dir: [f32; 3],
    /// 3.912 / V (Koschmieder, L1857).
    pub fog_density: f32,
    /// 0.2 x (1 - smoothstep(10, 25 degrees, sun elevation)), L1872.
    pub fog_sun_scatter: f32,
    pub aerial_perspective: f32,
    pub wind_offset_m: [f32; 2],
    pub cloud_seed: u64,
}

/// A lightning pulse: 80 to 150 ms, at most one every 2 s, none with Reduce flashing or the lightning
/// setting off (L1875, L1717).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Lightning { pub intensity: f32, pub duration_s: f32, pub bearing_deg: f32 }

pub struct SceneryRenderer {
    pub(crate) env: EnvironmentState,
    pub(crate) rev: u64,
    pub(crate) lut: Option<Gd<ImageTexture3D>>,
    pub(crate) core: core::SceneryCore,
    pub(crate) worker: core::Worker,
    pub(crate) gpu: Option<gpu::Gpu>,
}

impl SceneryRenderer {
    pub fn new(scenario: godot::prelude::Rid) -> Self {
        Self { env: EnvironmentState::default(), rev: 0, lut: None, core: core::SceneryCore::new(), worker: core::Worker::spawn(), gpu: Some(gpu::Gpu::new(scenario)) }
    }
    /// Build island meshes (8 m triangles near, 32 m far, token colour bands) from the sortie map.
    ///
    /// Returns at once: the far tiles are meshed on the unit's thread and appear as they finish; near tiles
    /// follow the camera in `update`.
    pub fn set_map(&mut self, map: &content::campaign::MapDef) {
        let bands = gpu::island_bands();
        if let Some(g) = &mut self.gpu { g.clear_islands(); }
        self.core.set_map(map, bands, &mut self.worker);
    }
    /// Per frame: follow weather events and blends, drift clouds, rebase with the origin, and return a
    /// lightning pulse when one starts.
    pub fn update(&mut self, ctx: &FrameContext) -> Option<Lightning> {
        self.core.lightning_on = crate::services::services().settings.try_borrow().map_or(true, |s| s.bool("graphics.lightning"));
        let changes = self.core.step(ctx);
        for key in self.core.near_requests() {
            self.worker.send(core::Job::Island { generation: self.core.island_generation, key, near: true }, self.core.source.clone());
        }
        if changes.layout {
            for (index, c) in self.core.clouds.iter().enumerate() {
                self.worker.send(core::Job::Cloud { generation: self.core.cloud_generation, index, kind: c.kind, seed: c.seed, size_m: c.size_m }, None);
            }
        }
        if let Some(g) = &mut self.gpu {
            g.update(ctx, &mut self.core, &mut self.worker, &changes);
            if changes.lut || (self.lut.is_none() && self.core.presets().is_some() && !g.lut_tried) {
                if let Some(t) = g.lut(&self.core) { self.lut = Some(t); }
            }
        }
        self.env = self.core.env.clone();
        self.rev = self.core.rev;
        changes.lightning
    }
    pub fn environment(&self) -> &EnvironmentState { &self.env }
    /// The colour-grading LUT for `environment()`: the preset's `TexBlob`, or the CPU blend of `blend_from`
    /// and `blend_to` at `blend`, refreshed in `update` at 1 Hz while a blend runs. `None` until the first
    /// weather event or when the generated files are missing (the environment then runs without a LUT).
    /// The same object is updated in place, so GDScript assigns it once.
    pub fn lut(&self) -> Option<Gd<ImageTexture3D>> { self.lut.clone() }
    /// Increments when `environment()` changed enough to re-apply (1 Hz during blends).
    pub fn rev(&self) -> u64 { self.rev }
    /// Height of the terrain or sea bed at a map position, for the shore foam heightfield and picking.
    pub fn ground_height(&self, world_x: f64, world_z: f64) -> f32 { self.core.ground_height(world_x, world_z) }
    pub fn free(&mut self) {
        self.worker.stop();
        if let Some(mut g) = self.gpu.take() { g.free(); }
        self.lut = None;
    }
}
