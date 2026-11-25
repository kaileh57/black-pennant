//! Part of unit `gd-render-ocean`. The engine side: the global shader parameters, the clipmap meshes and
//! instances on the ocean layer, the water material's textures and per-frame parameters. Main thread only;
//! exercised only inside Godot.
#![allow(clippy::disallowed_methods)]
use super::core::{Look, OceanFrame};
use super::shore::{self, ShoreUpload};
use crate::render::vfx::wake::WakeField;
use crate::render::hull::gpu;
use crate::render::hull::HullMasks;
use crate::render::LAYER_OCEAN;
use godot::classes::image::Format;
use godot::classes::rendering_server::{GlobalShaderParameterType, ShadowCastingSetting, TextureLayeredType};
use godot::classes::{Image, RenderingServer, Shader};
use godot::prelude::*;
use hull_mesh::env::ocean::{level_mesh, skirt_mesh, ClipmapSpec, MIN_FAR_M};
use sim_core::waves::{WAVE_MAT_UNIFORMS, WAVE_T_REF_UNIFORM};

pub(crate) const WATER_SHADER: &str = "res://shaders/water.gdshader";
const WORLD_TOKENS: &str = "res://art/tokens/world_tokens.json";
/// Global shader parameters of `res://shaders/inc/cloud_shadow.gdshaderinc`.
pub(crate) const CLOUD_SHADOW_UNIFORM: &str = "bp_cloud_shadow";
pub(crate) const CLOUD_SHADOW_XF_UNIFORM: &str = "bp_cloud_shadow_xf";

/// Parameter names of `res://shaders/water.gdshader`.
mod param {
    pub const COLORS: [(&str, &str); 4] = [("deep_color", "sea.deep"), ("scatter_color", "sea.scatter"), ("shallow_color", "sea.shallow"), ("foam_color", "sea.foam")];
    pub const SEA_STATE: &str = "sea_state";
    pub const RAIN: &str = "rain";
    pub const NYQUIST_FADE: &str = "nyquist_fade";
    pub const DETAIL_NORMALS: &str = "detail_normals";
    pub const DETAIL_TURBULENCE: &str = "detail_turbulence";
    pub const DETAIL_STRENGTH: &str = "detail_strength";
    pub const DETAIL_TILINGS: &str = "detail_tilings";
    pub const DETAIL_PHASE: &str = "detail_phase";
    pub const DETAIL_ORIGIN: &str = "detail_origin";
    pub const HULL_COUNT: &str = "hull_count";
    pub const HULL_MASK_FROM_RENDER: &str = "hull_mask_from_render";
    pub const HULL_MASKS: &str = "hull_masks";
    pub const SHORE_ENABLED: &str = "shore_enabled";
    pub const SHORE_HEIGHT: &str = "shore_height";
    pub const SHORE_WINDOW: &str = "shore_window";
    pub const SHORE_DECODE: &str = "shore_decode";
    pub const WIND_DIR: &str = "wind_dir";
    pub const FOAM_STREAKS: &str = "foam_streaks";
    pub const WAKE_COUNT: &str = "wake_count";
    pub const WAKE_INFO: &str = "wake_info";
    pub const WAKE_POINTS: &str = "wake_points";
}

fn zero_projection() -> Projection { Projection::from_cols(Vector4::ZERO, Vector4::ZERO, Vector4::ZERO, Vector4::ZERO) }

thread_local! {
    static GLOBALS_REGISTERED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Register the wave and cloud-shadow global shader parameters when the project has not
/// (`project.godot` `[shader_globals]`). A shader that names an unregistered global compiles with a warning
/// and draws wrong, so this runs when the main loop starts, before any shader loads; it is safe to repeat.
///
/// The engine only lists its globals in the editor, so "registered" is read from the project settings, and
/// each global added here is also entered there (in memory, never saved): scripts that make the same check
/// (`res://test/capture/sea_capture.gd`) then find it and do not add it twice.
pub(crate) fn ensure_globals() {
    if GLOBALS_REGISTERED.with(|g| g.replace(true)) { return; }
    let mut rs = gpu::rs();
    let mut settings = godot::classes::ProjectSettings::singleton();
    let mut add = |name: &str, kind: GlobalShaderParameterType, type_name: &str, value: Variant| {
        let key = format!("shader_globals/{name}");
        if settings.has_setting(key.as_str()) { return; }
        rs.global_shader_parameter_add(name, kind, &value);
        let mut entry = VarDictionary::new();
        entry.set("type", type_name);
        entry.set("value", &value);
        settings.set_setting(key.as_str(), &entry.to_variant());
    };
    for name in WAVE_MAT_UNIFORMS { add(name, GlobalShaderParameterType::MAT4, "mat4", zero_projection().to_variant()); }
    add(WAVE_T_REF_UNIFORM, GlobalShaderParameterType::FLOAT, "float", 0.0f64.to_variant());
    add(CLOUD_SHADOW_UNIFORM, GlobalShaderParameterType::SAMPLER2D, "sampler2D", Rid::Invalid.to_variant());
    add(CLOUD_SHADOW_XF_UNIFORM, GlobalShaderParameterType::VEC4, "vec4", Vector4::ZERO.to_variant());
}

/// The water shader and one material on it, for whoever owns an [`super::OceanRenderer`]: the renderer
/// borrows the material RID and sets its parameters; the owner keeps this alive and frees it afterwards.
pub struct WaterMaterial { pub(crate) shader: Option<Gd<Shader>>, pub(crate) rid: Rid }
impl WaterMaterial {
    /// Load `res://shaders/water.gdshader` and create a material on it. Without the shader (the generated
    /// wave include is missing, or the file does not compile) the RID is invalid and the sea draws with the
    /// engine's default material.
    pub fn load() -> Self {
        ensure_globals();
        let shader = gpu::load_shader(WATER_SHADER);
        if shader.is_none() { godot_warn!("ocean: {WATER_SHADER} could not be loaded (run cargo xtask gen for its wave include); the sea draws with the default material"); }
        let rid = gpu::material_on(&mut gpu::rs(), &shader);
        Self { shader, rid }
    }
    pub fn rid(&self) -> Rid { self.rid }
    pub fn free(&mut self) {
        gpu::free_rid(&mut gpu::rs(), &mut self.rid);
        self.shader = None;
    }
}

/// The `sea.*` world tokens as the colours a `source_color` uniform expects (sRGB-encoded).
fn sea_tokens() -> Vec<(&'static str, Color)> {
    let mut out = Vec::new();
    if !godot::classes::FileAccess::file_exists(WORLD_TOKENS) { godot_warn!("ocean: {WORLD_TOKENS} is missing; the sea draws in the shader's default colours"); return out; }
    let text = godot::classes::FileAccess::get_file_as_string(WORLD_TOKENS);
    let Ok(root) = godot::classes::Json::parse_string(&text).try_to::<VarDictionary>() else { return out };
    let Some(list) = root.get("sea").and_then(|v| v.try_to::<VarArray>().ok()) else { return out };
    for entry in list.iter_shared() {
        let Ok(entry) = entry.try_to::<VarDictionary>() else { continue };
        let Some(token) = entry.get("token").and_then(|v| v.try_to::<GString>().ok()).map(|s| s.to_string()) else { continue };
        let Some(&(uniform, _)) = param::COLORS.iter().find(|(_, t)| *t == token) else { continue };
        let Some(rgb) = entry.get("linear_rgb").and_then(|v| v.try_to::<VarArray>().ok()) else { continue };
        let c = |i: usize| rgb.get(i).and_then(|v| crate::convert::variant_f64(&v)).filter(|c| (0.0..=1.0).contains(c)).map(|c| c as f32);
        if let (Some(r), Some(g), Some(b)) = (c(0), c(1), c(2)) { out.push((uniform, Color::from_rgb(r, g, b).linear_to_srgb())); }
    }
    out
}

/// A `Texture2DArray` RID from one generated file, or `Rid::Invalid` when `cargo xtask gen` has not written
/// it or it is not the expected array.
fn array_from_gen(rs: &mut Gd<RenderingServer>, relative: &str) -> Rid {
    let Some(blob) = crate::convert::load_tex_blob(relative) else { return Rid::Invalid };
    let images = crate::convert::tex_blob_images(&blob);
    if images.is_empty() || images.len() != blob.layers as usize { return Rid::Invalid; }
    let mut layers: Array<Gd<Image>> = Array::new();
    for image in &images { layers.push(image); }
    rs.texture_2d_layered_create(&layers, TextureLayeredType::LAYERED_2D_ARRAY)
}

struct Level { mesh: Rid, instance: Rid, at: [f32; 3] }

pub(crate) struct Gpu {
    scenario: Rid,
    material: Rid,
    levels: Vec<Level>,
    skirt: Level,
    spec: Option<ClipmapSpec>,
    detail_normals: Rid,
    detail_turbulence: Rid,
    cloud_noise: Rid,
    shore_texture: Rid,
    // What the material was last told, so a parameter is sent only when it changes.
    look: Option<Look>,
    nyquist_fade: f32,
    detail_strength: f32,
    detail_tilings: i32,
    detail_origin: [f32; 4],
    hull_count: i64,
    hull_texture: Rid,
    hull_matrices: Vec<[f32; 16]>,
    shore_on: bool,
    shore_window: [f32; 3],
    reach_m: f32,
    wind_dir: [f32; 2],
    foam_streaks: Option<bool>,
    /// The wakes last sent (`wake_count` 0 sends nothing more).
    wakes: Option<WakeField>,
    /// `t_ref` on the unit's own count, used while no shader clock runs (the editor, a late install).
    own_time_s: f64,
    warned_clock: bool,
}

impl Gpu {
    pub(crate) fn new(scenario: Rid, spec: ClipmapSpec, material: Rid) -> Self {
        ensure_globals();
        let mut rs = gpu::rs();
        let detail_normals = array_from_gen(&mut rs, crate::seams::gen::OCEAN_DETAIL_NORMALS);
        let detail_turbulence = array_from_gen(&mut rs, crate::seams::gen::OCEAN_DETAIL_TURBULENCE);
        let cloud_noise = gpu::texture_from_gen(&mut rs, crate::seams::gen::NOISE_CLOUD_SHADOW);
        if !(detail_normals.is_valid() && detail_turbulence.is_valid()) { godot_warn!("ocean: the baked sea detail is missing (run cargo xtask gen); the sea draws without ripples and whitecap texture"); }
        if cloud_noise.is_valid() { rs.global_shader_parameter_set(CLOUD_SHADOW_UNIFORM, &cloud_noise.to_variant()); } else { godot_warn!("ocean: the cloud-shadow noise is missing (run cargo xtask gen); no cloud shadows"); }
        if material.is_valid() {
            for (uniform, colour) in sea_tokens() { rs.material_set_param(material, uniform, &colour.to_variant()); }
            if detail_normals.is_valid() && detail_turbulence.is_valid() {
                rs.material_set_param(material, param::DETAIL_NORMALS, &detail_normals.to_variant());
                rs.material_set_param(material, param::DETAIL_TURBULENCE, &detail_turbulence.to_variant());
            }
            rs.material_set_param(material, param::SHORE_DECODE, &Vector2::new(shore::SHORE_RANGE_M, shore::SHORE_MIN_M).to_variant());
            rs.material_set_param(material, param::SHORE_ENABLED, &false.to_variant());
            rs.material_set_param(material, param::HULL_COUNT, &0i64.to_variant());
        }
        let mut gpu = Self {
            scenario, material, levels: Vec::new(), skirt: Level { mesh: Rid::Invalid, instance: Rid::Invalid, at: [f32::NAN; 3] }, spec: None,
            detail_normals, detail_turbulence, cloud_noise, shore_texture: Rid::Invalid,
            look: None, nyquist_fade: f32::NAN, detail_strength: f32::NAN, detail_tilings: -1, detail_origin: [f32::NAN; 4],
            hull_count: 0, hull_texture: Rid::Invalid, hull_matrices: Vec::new(), shore_on: false, shore_window: [f32::NAN; 3], reach_m: f32::NAN, wind_dir: [f32::NAN; 2], foam_streaks: None, wakes: None,
            own_time_s: 0.0, warned_clock: false,
        };
        gpu.build(spec);
        super::clock::sea_drawn(true);
        gpu
    }

    fn instance(&self, rs: &mut Gd<RenderingServer>, mesh: Rid) -> Rid {
        if !mesh.is_valid() { return Rid::Invalid; }
        let instance = rs.instance_create2(mesh, self.scenario);
        rs.instance_set_layer_mask(instance, LAYER_OCEAN);
        rs.instance_geometry_set_cast_shadows_setting(instance, ShadowCastingSetting::OFF);
        instance
    }

    /// Build the clipmap for a spec, replacing any other. The meshes of one tier are a few hundred
    /// kilobytes; this runs when the sortie loads and when the player changes tier.
    pub(crate) fn build(&mut self, spec: ClipmapSpec) {
        if self.spec == Some(spec) { return; }
        let mut rs = gpu::rs();
        self.free_meshes(&mut rs);
        let mut level = 0u8;
        loop {
            let data = level_mesh(&spec, level);
            if data.is_empty() { break; }
            let mesh = gpu::mesh_from_data(&mut rs, &data, self.material);
            let instance = self.instance(&mut rs, mesh);
            self.levels.push(Level { mesh, instance, at: [f32::NAN; 3] });
            if level == u8::MAX { break; }
            level += 1;
        }
        let mesh = gpu::mesh_from_data(&mut rs, &skirt_mesh(&spec, MIN_FAR_M), self.material);
        let instance = self.instance(&mut rs, mesh);
        self.skirt = Level { mesh, instance, at: [f32::NAN; 3] };
        self.spec = Some(spec);
        self.reach_m = f32::NAN;
    }

    fn free_meshes(&mut self, rs: &mut Gd<RenderingServer>) {
        for mut l in std::mem::take(&mut self.levels).into_iter().chain(std::iter::once(std::mem::replace(&mut self.skirt, Level { mesh: Rid::Invalid, instance: Rid::Invalid, at: [f32::NAN; 3] }))) {
            gpu::free_rid(rs, &mut l.instance);
            gpu::free_rid(rs, &mut l.mesh);
        }
        self.spec = None;
    }

    /// The shaders' `TIME` for this frame: the start-up clock's, or this unit's own count without one.
    fn shader_time(&mut self, delta_s: f64) -> f64 {
        if delta_s.is_finite() && delta_s > 0.0 { self.own_time_s = (self.own_time_s + delta_s) % sim_core::waves::TIME_ROLLOVER_S; }
        match super::clock::shader_time() {
            Some(t) => t,
            None => {
                if !self.warned_clock { self.warned_clock = true; godot_warn!("ocean: no shader clock is running; wave phases follow this unit's own frame count and may be offset from the simulation"); }
                self.own_time_s
            }
        }
    }

    pub(crate) fn apply(&mut self, frame: &OceanFrame, masks: &HullMasks, wakes: Option<&WakeField>, upload: Option<ShoreUpload>, shore_window: Option<[f64; 2]>, delta_s: f64) {
        let mut rs = gpu::rs();
        // Waves: 8 matrices and the reference time, about 600 bytes (L1891).
        for (name, m) in WAVE_MAT_UNIFORMS.iter().zip(frame.uniforms.mats.iter()) {
            let col = |c: usize| Vector4::new(m[4 * c], m[4 * c + 1], m[4 * c + 2], m[4 * c + 3]);
            rs.global_shader_parameter_set(*name, &Projection::from_cols(col(0), col(1), col(2), col(3)).to_variant());
        }
        let t_ref = self.shader_time(delta_s);
        rs.global_shader_parameter_set(WAVE_T_REF_UNIFORM, &t_ref.to_variant());
        let xf = frame.cloud_xf;
        let cover = if self.cloud_noise.is_valid() { xf[3] } else { 0.0 };
        rs.global_shader_parameter_set(CLOUD_SHADOW_XF_UNIFORM, &Vector4::new(xf[0], xf[1], xf[2], cover).to_variant());

        // Clipmap placement.
        let margin_changed = frame.reach_m != self.reach_m;
        self.reach_m = frame.reach_m;
        let last = frame.levels.len().saturating_sub(1);
        for (i, level) in self.levels.iter_mut().enumerate() {
            let Some(at) = frame.levels.get(i).copied() else { continue };
            if !level.instance.is_valid() { continue; }
            if at != level.at { level.at = at; rs.instance_set_transform(level.instance, Transform3D::new(Basis::IDENTITY, Vector3::new(at[0], at[1], at[2]))); }
            if margin_changed { rs.instance_set_extra_visibility_margin(level.instance, frame.reach_m); }
        }
        if let (Some(at), true) = (frame.levels.get(last).copied(), self.skirt.instance.is_valid()) {
            if at != self.skirt.at { self.skirt.at = at; rs.instance_set_transform(self.skirt.instance, Transform3D::new(Basis::IDENTITY, Vector3::new(at[0], at[1], at[2]))); }
            if margin_changed { rs.instance_set_extra_visibility_margin(self.skirt.instance, frame.reach_m); }
        }

        let material = self.material;
        if !material.is_valid() { return; }
        if self.look != Some(frame.look) {
            self.look = Some(frame.look);
            rs.material_set_param(material, param::SEA_STATE, &frame.look.sea_state.to_variant());
            rs.material_set_param(material, param::RAIN, &frame.look.rain.to_variant());
        }
        if frame.nyquist_fade != self.nyquist_fade { self.nyquist_fade = frame.nyquist_fade; rs.material_set_param(material, param::NYQUIST_FADE, &frame.nyquist_fade.to_variant()); }
        let strength = if self.detail_normals.is_valid() && self.detail_turbulence.is_valid() { frame.detail_strength } else { 0.0 };
        if strength != self.detail_strength { self.detail_strength = strength; rs.material_set_param(material, param::DETAIL_STRENGTH, &strength.to_variant()); }
        if frame.detail_tilings != self.detail_tilings { self.detail_tilings = frame.detail_tilings; rs.material_set_param(material, param::DETAIL_TILINGS, &(frame.detail_tilings as i64).to_variant()); }
        if frame.detail_origin != self.detail_origin {
            self.detail_origin = frame.detail_origin;
            let o = frame.detail_origin;
            rs.material_set_param(material, param::DETAIL_ORIGIN, &Vector4::new(o[0], o[1], o[2], o[3]).to_variant());
        }
        rs.material_set_param(material, param::DETAIL_PHASE, &frame.detail_phase.to_variant());
        if self.foam_streaks != Some(frame.foam_streaks) { self.foam_streaks = Some(frame.foam_streaks); rs.material_set_param(material, param::FOAM_STREAKS, &frame.foam_streaks.to_variant()); }
        if frame.wind_dir != self.wind_dir { self.wind_dir = frame.wind_dir; rs.material_set_param(material, param::WIND_DIR, &Vector2::new(frame.wind_dir[0], frame.wind_dir[1]).to_variant()); }

        // Wakes (L1967): about 1.2 KB while a hull makes way, nothing while none does.
        let empty = WakeField::default();
        let wakes = wakes.unwrap_or(&empty);
        if self.wakes.as_ref() != Some(wakes) {
            if self.wakes.as_ref().is_none_or(|w| w.count != wakes.count) { rs.material_set_param(material, param::WAKE_COUNT, &(wakes.count as i64).to_variant()); }
            if wakes.count > 0 {
                let pack = |v: &[[f32; 4]]| PackedVector4Array::from_iter(v.iter().map(|p| Vector4::new(p[0], p[1], p[2], p[3])));
                rs.material_set_param(material, param::WAKE_INFO, &pack(&wakes.info).to_variant());
                rs.material_set_param(material, param::WAKE_POINTS, &pack(&wakes.points).to_variant());
            }
            self.wakes = Some(wakes.clone());
        }

        // Hull masks (L1909).
        let count = if masks.texture.is_valid() { (masks.count as usize).min(hull_mesh::mask::MAX_MASKED_HULLS).min(masks.world_to_hull.len()) } else { 0 };
        if masks.texture != self.hull_texture && masks.texture.is_valid() { self.hull_texture = masks.texture; rs.material_set_param(material, param::HULL_MASKS, &masks.texture.to_variant()); }
        if count as i64 != self.hull_count { self.hull_count = count as i64; rs.material_set_param(material, param::HULL_COUNT, &(count as i64).to_variant()); }
        if count > 0 && self.hull_matrices.as_slice() != &masks.world_to_hull[..count] {
            self.hull_matrices.clear();
            self.hull_matrices.extend_from_slice(&masks.world_to_hull[..count]);
            let mut matrices: Array<Projection> = Array::new();
            for i in 0..hull_mesh::mask::MAX_MASKED_HULLS {
                let m = masks.world_to_hull.get(i).filter(|_| i < count).copied();
                matrices.push(match m {
                    Some(m) => { let col = |c: usize| Vector4::new(m[4 * c], m[4 * c + 1], m[4 * c + 2], m[4 * c + 3]); Projection::from_cols(col(0), col(1), col(2), col(3)) }
                    None => zero_projection(),
                });
            }
            rs.material_set_param(material, param::HULL_MASK_FROM_RENDER, &matrices.to_variant());
        }

        // Shore heightfield: one 8 MB upload per re-centre, then only the window moves with the origin.
        if let Some(upload) = upload {
            let n = shore::SHORE_TEXELS as i32;
            if let Some(image) = Image::create_from_data(n, n, false, Format::R16, &PackedByteArray::from(upload.bytes.as_slice())) {
                if self.shore_texture.is_valid() { rs.texture_2d_update(self.shore_texture, &image, 0); } else {
                    self.shore_texture = rs.texture_2d_create(&image);
                    rs.material_set_param(material, param::SHORE_HEIGHT, &self.shore_texture.to_variant());
                }
            }
        }
        let window = shore_window.filter(|_| self.shore_texture.is_valid());
        let on = window.is_some();
        if on != self.shore_on { self.shore_on = on; rs.material_set_param(material, param::SHORE_ENABLED, &on.to_variant()); }
        if let Some(centre) = window {
            let corner = shore::corner(centre);
            let w = [(corner[0] - frame.origin[0]) as f32, (corner[1] - frame.origin[1]) as f32, (1.0 / shore::SHORE_EXTENT_M) as f32];
            if w != self.shore_window { self.shore_window = w; rs.material_set_param(material, param::SHORE_WINDOW, &Vector4::new(w[0], w[1], w[2], 0.0).to_variant()); }
        }
    }

    pub(crate) fn free(&mut self) {
        let mut rs = gpu::rs();
        self.free_meshes(&mut rs);
        super::clock::sea_drawn(false);
        // The globals outlive the unit (other shaders read them): leave them neutral.
        rs.global_shader_parameter_set(CLOUD_SHADOW_XF_UNIFORM, &Vector4::ZERO.to_variant());
        if self.cloud_noise.is_valid() { rs.global_shader_parameter_set(CLOUD_SHADOW_UNIFORM, &Rid::Invalid.to_variant()); }
        for name in WAVE_MAT_UNIFORMS { rs.global_shader_parameter_set(name, &zero_projection().to_variant()); }
        if self.material.is_valid() {
            rs.material_set_param(self.material, param::SHORE_ENABLED, &false.to_variant());
            rs.material_set_param(self.material, param::HULL_COUNT, &0i64.to_variant());
            rs.material_set_param(self.material, param::WAKE_COUNT, &0i64.to_variant());
            rs.material_set_param(self.material, param::DETAIL_STRENGTH, &0.0f32.to_variant());
        }
        for rid in [&mut self.detail_normals, &mut self.detail_turbulence, &mut self.cloud_noise, &mut self.shore_texture] { gpu::free_rid(&mut rs, rid); }
        self.material = Rid::Invalid;
    }
}
