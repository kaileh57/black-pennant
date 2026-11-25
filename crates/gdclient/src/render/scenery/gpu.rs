//! Part of unit `gd-render-scenery`. The engine side: island tile and cloud cluster meshes and instances on
//! the scenery layer, the island and cloud materials, and the colour-grading LUT (`ImageTexture3D`, one
//! object updated in place). Main thread only; exercised only inside Godot.
// Client rendering, not simulation (clippy.toml): std floats are allowed here. Nothing drawn from them feeds
// back into the sim, and a last-bit difference between platforms moves nothing on screen.
#![allow(clippy::disallowed_methods)]
use super::core::{self as sc, Built, Changes, SceneryCore, TileKey, Worker};
use crate::render::hull::gpu;
use crate::render::{FrameContext, LAYER_SCENERY};
use content::enums::TimeOfDay;
use content::weather::WeatherKind;
use godot::classes::rendering_server::ShadowCastingSetting;
use godot::classes::{Image, ImageTexture3D, RenderingServer, Shader};
use godot::prelude::*;
use hull_mesh::env::scenery::IslandBands;
use std::collections::BTreeMap;

pub(crate) const ISLAND_SHADER: &str = "res://shaders/island.gdshader";
pub(crate) const CLOUD_SHADER: &str = "res://shaders/cloud.gdshader";
const WORLD_TOKENS: &str = "res://art/tokens/world_tokens.json";

mod param {
    pub const WET_HEIGHT_M: &str = "wet_height_m";
    pub const HAZE_COLOR: &str = "haze_color";
    pub const HAZE_DENSITY: &str = "haze_density";
}

/// Each preset's fog colour (linear RGB) from the world tokens, keyed by weather and time; empty when the file
/// cannot be read (the clouds then keep the environment's own fog).
fn fog_colors() -> BTreeMap<(WeatherKind, TimeOfDay), [f32; 3]> {
    let mut out = BTreeMap::new();
    if !godot::classes::FileAccess::file_exists(WORLD_TOKENS) { return out; }
    let text = godot::classes::FileAccess::get_file_as_string(WORLD_TOKENS);
    let Ok(root) = godot::classes::Json::parse_string(&text).try_to::<VarDictionary>() else { return out };
    let Some(list) = root.get("presets").and_then(|v| v.try_to::<VarArray>().ok()) else { return out };
    let word = |d: &VarDictionary, k: &str| d.get(k).and_then(|v| v.try_to::<GString>().ok()).map(|s| s.to_string()).unwrap_or_default();
    for p in list.iter_shared() {
        let Ok(p) = p.try_to::<VarDictionary>() else { continue };
        let weather = match word(&p, "weather").as_str() { "clear" => WeatherKind::Clear, "haze" => WeatherKind::Haze, "overcast" => WeatherKind::Overcast, "rain" => WeatherKind::Rain, "storm" => WeatherKind::Storm, "fog_bank" => WeatherKind::FogBank, _ => continue };
        let time = match word(&p, "time").as_str() { "dusk" => TimeOfDay::Dusk, "day" => TimeOfDay::Day, "night" => TimeOfDay::Night, _ => continue };
        let Some(rgb) = p.get("fog_color").and_then(|v| v.try_to::<VarArray>().ok()) else { continue };
        let c = |i: usize| rgb.get(i).and_then(|v| crate::convert::variant_f64(&v)).filter(|c| (0.0..=1.0).contains(c)).map(|c| c as f32);
        if let (Some(r), Some(g), Some(b)) = (c(0), c(1), c(2)) { out.insert((weather, time), [r, g, b]); }
    }
    out
}

/// The island height bands from the world tokens (`island_bands`: `top_m`, `linear_rgb`), lowest first;
/// empty when the file cannot be read (the islands then draw in the generator's neutral grey).
pub(crate) fn island_bands() -> IslandBands {
    let mut bands = IslandBands { band_top_m: Vec::new(), colors: Vec::new() };
    if !godot::classes::FileAccess::file_exists(WORLD_TOKENS) { godot_warn!("scenery: {WORLD_TOKENS} is missing; islands draw without their colour bands"); return bands; }
    let text = godot::classes::FileAccess::get_file_as_string(WORLD_TOKENS);
    let Ok(root) = godot::classes::Json::parse_string(&text).try_to::<VarDictionary>() else { return bands };
    let Some(list) = root.get("island_bands").and_then(|v| v.try_to::<VarArray>().ok()) else { return bands };
    for b in list.iter_shared() {
        let Ok(b) = b.try_to::<VarDictionary>() else { continue };
        let top = b.get("top_m").and_then(|v| crate::convert::variant_f64(&v));
        let rgb = b.get("linear_rgb").and_then(|v| v.try_to::<VarArray>().ok());
        let (Some(top), Some(rgb)) = (top, rgb) else { continue };
        let c = |i: usize| rgb.get(i).and_then(|v| crate::convert::variant_f64(&v)).filter(|c| (0.0..=1.0).contains(c)).map(|c| c as f32);
        if let (Some(r), Some(g), Some(bl)) = (c(0), c(1), c(2)) { bands.band_top_m.push(top as f32); bands.colors.push([r, g, bl]); }
    }
    bands
}

/// A LUT file by preset and resolution.
type LutKey = (WeatherKind, TimeOfDay, bool);

struct TileGpu { far: Rid, near: Rid, instance: Rid, on_near: bool }
impl Default for TileGpu { fn default() -> Self { Self { far: Rid::Invalid, near: Rid::Invalid, instance: Rid::Invalid, on_near: false } } }
struct CloudGpu { mesh: Rid, instance: Rid }
impl Default for CloudGpu { fn default() -> Self { Self { mesh: Rid::Invalid, instance: Rid::Invalid } } }

pub(crate) struct Gpu {
    scenario: Rid,
    _shaders: (Option<Gd<Shader>>, Option<Gd<Shader>>),
    island_material: Rid,
    cloud_material: Rid,
    tiles: BTreeMap<TileKey, TileGpu>,
    clouds: Vec<CloudGpu>,
    /// The previous layout's clusters, drawn until the new layout is all uploaded (no empty sky between).
    retiring: Vec<CloudGpu>,
    retiring_frames: u32,
    cloud_generation: u32,
    pending: Vec<Built>,
    wet_height_m: f32,
    origin: [f64; 2],
    luts: BTreeMap<LutKey, Option<(u32, Vec<u8>)>>,
    lut: Option<Gd<ImageTexture3D>>,
    lut_edge: u32,
    pub(crate) lut_tried: bool,
    fog_colors: BTreeMap<(WeatherKind, TimeOfDay), [f32; 3]>,
    /// What the cloud material's haze was last set to (colour, density).
    haze: ([f32; 3], f32),
}

impl Gpu {
    pub(crate) fn new(scenario: Rid) -> Self {
        let mut rs = gpu::rs();
        let island = gpu::load_shader(ISLAND_SHADER);
        let cloud = gpu::load_shader(CLOUD_SHADER);
        if island.is_none() || cloud.is_none() { godot_warn!("scenery: an island or cloud shader is missing; drawing with the default material"); }
        let island_material = gpu::material_on(&mut rs, &island);
        let cloud_material = gpu::material_on(&mut rs, &cloud);
        Self { scenario, _shaders: (island, cloud), island_material, cloud_material, tiles: BTreeMap::new(), clouds: Vec::new(), retiring: Vec::new(), retiring_frames: 0, cloud_generation: u32::MAX, pending: Vec::new(), wet_height_m: f32::NAN, origin: [f64::NAN; 2], luts: BTreeMap::new(), lut: None, lut_edge: 0, lut_tried: false, fog_colors: fog_colors(), haze: ([f32::NAN; 3], f32::NAN) }
    }

    pub(crate) fn clear_islands(&mut self) {
        let mut rs = gpu::rs();
        for (_, mut t) in std::mem::take(&mut self.tiles) { Self::free_tile(&mut rs, &mut t); }
        self.pending.retain(|b| !matches!(b, Built::Island { .. }));
    }

    fn free_tile(rs: &mut Gd<RenderingServer>, t: &mut TileGpu) {
        gpu::free_rid(rs, &mut t.instance);
        gpu::free_rid(rs, &mut t.far);
        gpu::free_rid(rs, &mut t.near);
    }

    fn tile_xform(&self, key: TileKey) -> Transform3D {
        let x = key.0 as f64 * sc::TILE_M - self.origin[0];
        let z = key.1 as f64 * sc::TILE_M - self.origin[1];
        Transform3D::new(Basis::IDENTITY, Vector3::new(x as f32, 0.0, z as f32))
    }

    pub(crate) fn update(&mut self, ctx: &FrameContext, core: &mut SceneryCore, worker: &mut Worker, ch: &Changes) {
        let mut rs = gpu::rs();
        let origin = [ctx.view.origin.x, ctx.view.origin.z];
        let moved = origin != self.origin;
        self.origin = origin;
        // Materials follow the weather.
        let hs = core.env.weather.sea_state.significant_height_m() as f32;
        let wet = (0.5 * hs).max(0.5);
        if wet != self.wet_height_m && self.island_material.is_valid() {
            self.wet_height_m = wet;
            rs.material_set_param(self.island_material, param::WET_HEIGHT_M, &wet.to_variant());
        }
        // Cloud haze: the environment's fog colour and density, which the cloud shader thins with altitude.
        if let Some(((wa, ta), (wb, tb), t)) = core.presets() {
            if let (Some(a), Some(b)) = (self.fog_colors.get(&(wa, ta)), self.fog_colors.get(&(wb, tb))) {
                let colour = [0, 1, 2].map(|i| a[i] + (b[i] - a[i]) * t.clamp(0.0, 1.0));
                let haze = (colour, core.env.fog_density.max(0.0));
                if haze != self.haze && self.cloud_material.is_valid() {
                    self.haze = haze;
                    rs.material_set_param(self.cloud_material, param::HAZE_COLOR, &Vector3::new(colour[0], colour[1], colour[2]).to_variant());
                    rs.material_set_param(self.cloud_material, param::HAZE_DENSITY, &haze.1.to_variant());
                }
            }
        }
        // New cloud layout: drop the old clusters.
        if core.cloud_generation != self.cloud_generation {
            self.cloud_generation = core.cloud_generation;
            for mut c in std::mem::take(&mut self.retiring) { gpu::free_rid(&mut rs, &mut c.instance); gpu::free_rid(&mut rs, &mut c.mesh); }
            self.retiring = std::mem::take(&mut self.clouds).into_iter().filter(|c| c.instance.is_valid()).collect();
            self.retiring_frames = 0;
            self.clouds.resize_with(core.clouds.len(), CloudGpu::default);
            self.pending.retain(|b| !matches!(b, Built::Cloud { .. }));
        }
        // Finished meshes, a few per frame.
        worker.poll(&mut self.pending);
        let mut uploaded = 0;
        while uploaded < sc::UPLOADS_PER_FRAME && !self.pending.is_empty() {
            let b = self.pending.remove(0);
            match b {
                Built::Island { generation, key, near, mesh } => {
                    if generation != core.island_generation { continue; }
                    let Some(state) = core.tiles.get_mut(&key) else { continue };
                    let rid = gpu::mesh_from_data(&mut rs, &mesh, self.island_material);
                    uploaded += 1;
                    let t = self.tiles.entry(key).or_default();
                    if near {
                        state.near_pending = false;
                        if !state.near_wanted { gpu::free_rid(&mut rs, &mut { rid }); continue; }
                        gpu::free_rid(&mut rs, &mut t.near);
                        t.near = rid;
                        state.near_built = true;
                    } else {
                        gpu::free_rid(&mut rs, &mut t.far);
                        t.far = rid;
                        state.far_built = true;
                    }
                }
                Built::Cloud { generation, index, mesh } => {
                    if generation != core.cloud_generation || index >= self.clouds.len() { continue; }
                    let rid = gpu::mesh_from_data(&mut rs, &mesh, self.cloud_material);
                    uploaded += 1;
                    let c = &mut self.clouds[index];
                    gpu::free_rid(&mut rs, &mut c.mesh);
                    c.mesh = rid;
                    if rid.is_valid() && !c.instance.is_valid() {
                        c.instance = rs.instance_create2(rid, self.scenario);
                        rs.instance_set_layer_mask(c.instance, LAYER_SCENERY);
                        rs.instance_geometry_set_cast_shadows_setting(c.instance, ShadowCastingSetting::OFF);
                    } else if rid.is_valid() { rs.instance_set_base(c.instance, rid); }
                    if let Some(w) = core.cloud_world.get(index) { rs.instance_set_transform(c.instance, Transform3D::new(Basis::IDENTITY, Vector3::new((w[0] - origin[0]) as f32, w[1] as f32, (w[2] - origin[1]) as f32))); }
                }
            }
        }
        // The old layout goes once the new one is all there (or after 5 s, whatever is missing).
        if !self.retiring.is_empty() {
            self.retiring_frames += 1;
            if self.clouds.iter().all(|c| c.instance.is_valid()) || self.retiring_frames > 300 {
                for mut c in std::mem::take(&mut self.retiring) { gpu::free_rid(&mut rs, &mut c.instance); gpu::free_rid(&mut rs, &mut c.mesh); }
            }
        }
        // Island instances: the mesh each tile shows, placed relative to the origin.
        let keys: Vec<TileKey> = self.tiles.keys().copied().collect();
        for key in keys {
            let Some(state) = core.tiles.get(&key).copied() else { continue };
            let xf = self.tile_xform(key);
            let scenario = self.scenario;
            let t = self.tiles.get_mut(&key).unwrap();
            if !state.near_built && t.near.is_valid() { gpu::free_rid(&mut rs, &mut t.near); }
            let want = if state.show_near && t.near.is_valid() { t.near } else { t.far };
            if !want.is_valid() { continue; }
            if !t.instance.is_valid() {
                t.instance = rs.instance_create2(want, scenario);
                rs.instance_set_layer_mask(t.instance, LAYER_SCENERY);
                rs.instance_geometry_set_cast_shadows_setting(t.instance, ShadowCastingSetting::ON);
                rs.instance_set_transform(t.instance, xf);
                t.on_near = want == t.near;
            } else if (want == t.near) != t.on_near {
                rs.instance_set_base(t.instance, want);
                t.on_near = want == t.near;
            }
            if moved || ch.islands_moved { rs.instance_set_transform(t.instance, xf); }
        }
        // Clouds step at 1 Hz (and on a rebase).
        if ch.cloud_xforms || moved {
            for (c, w) in self.clouds.iter().zip(&core.cloud_world) {
                if c.instance.is_valid() { rs.instance_set_transform(c.instance, Transform3D::new(Basis::IDENTITY, Vector3::new((w[0] - origin[0]) as f32, w[1] as f32, (w[2] - origin[1]) as f32))); }
            }
        }
    }

    /// RGB8 bytes of one preset's LUT (first available of its fallbacks), cached.
    fn lut_bytes(&mut self, weather: WeatherKind, time: TimeOfDay, high: bool) -> Option<(u32, Vec<u8>)> {
        for (w, t) in sc::lut_fallbacks(weather, time) {
            for h in [high, false] {
                let entry = self.luts.entry((w, t, h)).or_insert_with(|| {
                    let blob = crate::convert::load_tex_blob(&crate::seams::gen::lut_path(w, t, h))?;
                    let edge = blob.width;
                    let ok = blob.format == crate::seams::gen::TexFormat::Rgb8 && blob.width == blob.height && blob.layers == edge && blob.mip_levels <= 1 && blob.data.len() == (edge * edge * edge * 3) as usize && edge >= 2;
                    if ok { Some((edge, blob.data)) } else { None }
                });
                if let Some(found) = entry { return Some(found.clone()); }
            }
        }
        None
    }

    /// The blended LUT for the current presets, written into the one texture object. `None` when no LUT file
    /// can be read.
    pub(crate) fn lut(&mut self, core: &SceneryCore) -> Option<Gd<ImageTexture3D>> {
        let ((wa, ta), (wb, tb), t) = core.presets()?;
        self.lut_tried = true;
        let a = self.lut_bytes(wa, ta, core.high_lut);
        let b = self.lut_bytes(wb, tb, core.high_lut);
        let (edge, bytes) = match (a, b) {
            (Some(a), Some(b)) if a.0 == b.0 => (a.0, sc::blend_lut(&a.1, &b.1, t)),
            (_, Some(b)) => b,
            (Some(a), None) => a,
            (None, None) => {
                if self.lut.is_none() { godot_warn!("scenery: no colour-grading LUT could be read (run cargo xtask gen); grading is off"); }
                return self.lut.clone();
            }
        };
        let slice = (edge * edge * 3) as usize;
        let mut images: Array<Gd<Image>> = Array::new();
        for s in bytes.chunks_exact(slice) {
            let Some(img) = Image::create_from_data(edge as i32, edge as i32, false, godot::classes::image::Format::RGB8, &PackedByteArray::from(s)) else { return self.lut.clone() };
            images.push(&img);
        }
        if images.len() != edge as usize { return self.lut.clone(); }
        let tex = self.lut.get_or_insert_with(ImageTexture3D::new_gd);
        if self.lut_edge == edge { tex.update(&images); } else {
            tex.create(godot::classes::image::Format::RGB8, edge as i32, edge as i32, edge as i32, false, &images);
            self.lut_edge = edge;
        }
        self.lut.clone()
    }

    /// Free every RID. Idempotent.
    pub(crate) fn free(&mut self) {
        let mut rs = gpu::rs();
        for (_, mut t) in std::mem::take(&mut self.tiles) { Self::free_tile(&mut rs, &mut t); }
        for mut c in std::mem::take(&mut self.clouds).into_iter().chain(std::mem::take(&mut self.retiring)) { gpu::free_rid(&mut rs, &mut c.instance); gpu::free_rid(&mut rs, &mut c.mesh); }
        gpu::free_rid(&mut rs, &mut self.island_material);
        gpu::free_rid(&mut rs, &mut self.cloud_material);
        self.pending.clear();
        self.lut = None;
    }
}
