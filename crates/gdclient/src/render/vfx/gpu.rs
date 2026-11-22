//! Part of unit `gd-render-vfx`. The engine side of VFX layer A: one mesh, material and MultiMesh per kind on
//! `vfx_opaque.gdshader` (blended kinds on `vfx_blend.gdshader`), each material sampling its kind's
//! generated ramp; and the pooled OmniLights (`vfx_light_budget(tier)`), unshadowed, coloured from the world
//! tokens' flash and fire ramps. Main thread only; exercised only inside Godot.
use super::core::{pool_kind, shader_kind, VfxCore, KINDS};
use crate::enums::QualityTier;
use crate::render::hull::gpu::{self, MultiMeshSlot};
use crate::render::{INSTANCE_FLOATS, LAYER_VFX};
use godot::classes::rendering_server::LightParam;
use godot::classes::{RenderingServer, Shader};
use godot::prelude::*;
use hull_mesh::vfx::{RampId, VfxKind};

pub(crate) const OPAQUE_SHADER: &str = "res://shaders/vfx_opaque.gdshader";
pub(crate) const BLEND_SHADER: &str = "res://shaders/vfx_blend.gdshader";
const WORLD_TOKENS: &str = "res://art/tokens/world_tokens.json";

/// Shader parameter names shared by the two VFX shaders.
pub(crate) mod param {
    pub const VFX_KIND: &str = "vfx_kind";
    pub const RAMP: &str = "ramp";
    pub const EMISSION_CAP: &str = "emission_cap";
}

/// Linear RGB of one ramp stop in the world tokens (`ramps[id].stops`, the stop nearest `t`), or `None` when
/// the token file or the entry is missing.
pub(crate) fn ramp_token(id: &str, t: f64) -> Option<[f32; 3]> {
    if !godot::classes::FileAccess::file_exists(WORLD_TOKENS) { return None; }
    let text = godot::classes::FileAccess::get_file_as_string(WORLD_TOKENS);
    let root = godot::classes::Json::parse_string(&text).try_to::<VarDictionary>().ok()?;
    let ramps = root.get("ramps")?.try_to::<VarArray>().ok()?;
    for r in ramps.iter_shared() {
        let Ok(r) = r.try_to::<VarDictionary>() else { continue };
        if r.get("id").and_then(|v| v.try_to::<GString>().ok()).map(|g| g.to_string()) != Some(id.to_string()) { continue; }
        let stops = r.get("stops")?.try_to::<VarArray>().ok()?;
        let mut best: Option<(f64, [f32; 3])> = None;
        for s in stops.iter_shared() {
            let Ok(s) = s.try_to::<VarDictionary>() else { continue };
            let st = s.get("t").and_then(|v| crate::convert::variant_f64(&v)).unwrap_or(0.0);
            let Some(rgba) = s.get("linear_rgba").and_then(|v| v.try_to::<VarArray>().ok()) else { continue };
            let c = |i: usize| rgba.get(i).and_then(|v| crate::convert::variant_f64(&v)).filter(|c| (0.0..=4.0).contains(c)).map(|c| c as f32);
            let (Some(r), Some(g), Some(b)) = (c(0), c(1), c(2)) else { continue };
            if best.is_none_or(|b| (st - t).abs() < (b.0 - t).abs()) { best = Some((st, [r, g, b])); }
        }
        return best.map(|b| b.1);
    }
    None
}

struct Light { light: Rid, instance: Rid, shown: bool }

pub(crate) struct Gpu {
    pub(crate) tier: QualityTier,
    scenario: Rid,
    _shaders: (Option<Gd<Shader>>, Option<Gd<Shader>>),
    ramps: Vec<Rid>,
    materials: Vec<Rid>,
    meshes: Vec<Rid>,
    slots: Vec<MultiMeshSlot>,
    lights: Vec<Light>,
    emission_cap: f32,
    flash_color: Color,
    fire_color: Color,
    pub(crate) lights_shown: u32,
}

fn to_color(c: Option<[f32; 3]>) -> Color { c.map_or(Color::from_rgb(1.0, 1.0, 1.0), |c| Color::from_rgb(c[0], c[1], c[2])) }

impl Gpu {
    pub(crate) fn new(scenario: Rid, tier: QualityTier) -> Self {
        let mut rs = gpu::rs();
        let opaque = gpu::load_shader(OPAQUE_SHADER);
        let blend = gpu::load_shader(BLEND_SHADER);
        if opaque.is_none() || blend.is_none() { godot_warn!("vfx renderer: a VFX shader is missing; drawing with the default material"); }
        let ramps: Vec<Rid> = RampId::ALL.iter().map(|r| gpu::texture_from_gen(&mut rs, &crate::seams::gen::ramp_path(r.name()))).collect();
        if ramps.iter().any(|r| !r.is_valid()) { godot_warn!("vfx renderer: generated VFX ramps are missing (run cargo xtask gen); effects draw white"); }
        let (mut materials, mut meshes, mut slots) = (Vec::new(), Vec::new(), Vec::new());
        for i in 0..KINDS {
            let kind = pool_kind(i);
            let material = gpu::material_on(&mut rs, if kind.blended() { &blend } else { &opaque });
            if material.is_valid() {
                rs.material_set_param(material, param::VFX_KIND, &shader_kind(kind).to_variant());
                let ramp = RampId::ALL.iter().position(|r| *r == kind.ramp()).map_or(Rid::Invalid, |k| ramps[k]);
                if ramp.is_valid() { rs.material_set_param(material, param::RAMP, &ramp.to_variant()); }
                rs.material_set_param(material, param::EMISSION_CAP, &hull_mesh::vfx::emission::FLASH.1.to_variant());
            }
            let mesh = gpu::mesh_from_data(&mut rs, &hull_mesh::vfx::meshes::base_mesh(kind), material);
            slots.push(MultiMeshSlot::new(&mut rs, scenario, mesh, LAYER_VFX, false, Rid::Invalid));
            materials.push(material);
            meshes.push(mesh);
        }
        let mut g = Self {
            tier, scenario, _shaders: (opaque, blend), ramps, materials, meshes, slots, lights: Vec::new(), emission_cap: hull_mesh::vfx::emission::FLASH.1,
            flash_color: to_color(ramp_token("flash", 0.25)), fire_color: to_color(ramp_token("fire", 0.2)), lights_shown: 0,
        };
        g.make_lights(&mut rs);
        g
    }

    fn make_lights(&mut self, rs: &mut Gd<RenderingServer>) {
        let (total, _) = hull_mesh::vfx::vfx_light_budget(self.tier);
        while self.lights.len() > total {
            if let Some(mut l) = self.lights.pop() { gpu::free_rid(rs, &mut l.instance); gpu::free_rid(rs, &mut l.light); }
        }
        while self.lights.len() < total {
            let light = rs.omni_light_create();
            rs.light_set_shadow(light, false);
            rs.light_set_param(light, LightParam::ATTENUATION, 1.5);
            rs.light_set_param(light, LightParam::SPECULAR, 0.5);
            let instance = rs.instance_create2(light, self.scenario);
            rs.instance_set_visible(instance, false);
            self.lights.push(Light { light, instance, shown: false });
        }
    }

    pub(crate) fn set_tier(&mut self, tier: QualityTier) {
        self.tier = tier;
        let mut rs = gpu::rs();
        self.make_lights(&mut rs);
    }

    /// `pools` are the renderer's pools (`VfxRenderer::pools`), in `core::pool_kind` order.
    pub(crate) fn upload(&mut self, core: &VfxCore, pools: &[hull_mesh::vfx::runtime::RingPool], reduce_flashing: bool) {
        let mut rs = gpu::rs();
        let cap = if reduce_flashing { hull_mesh::vfx::emission::REDUCE_FLASHING_CAP } else { hull_mesh::vfx::emission::FLASH.1 };
        if cap != self.emission_cap {
            for m in &self.materials { if m.is_valid() { rs.material_set_param(*m, param::EMISSION_CAP, &cap.to_variant()); } }
            self.emission_cap = cap;
        }
        for (i, slot) in self.slots.iter_mut().enumerate() {
            let Some(pool) = pools.get(i) else { break };
            let n = pool.visible_count() * INSTANCE_FLOATS;
            let floats = &core.floats[i][..n.min(core.floats[i].len())];
            let (min, max) = core.bounds[i];
            slot.upload(&mut rs, floats, min, max);
        }
        self.lights_shown = 0;
        for (k, l) in self.lights.iter_mut().enumerate() {
            match core.lights.get(k) {
                Some(out) => {
                    rs.light_set_color(l.light, if out.fire { self.fire_color } else { self.flash_color });
                    rs.light_set_param(l.light, LightParam::ENERGY, out.energy.max(0.0));
                    rs.light_set_param(l.light, LightParam::RANGE, out.range_m.clamp(0.5, hull_mesh::vfx::runtime::MUZZLE_LIGHT_MAX_M));
                    // Volumetric fog (High) is lit by muzzle flashes, never by fires (L1840).
                    rs.light_set_param(l.light, LightParam::VOLUMETRIC_FOG_ENERGY, if out.fire { 0.0 } else { 1.0 });
                    rs.instance_set_transform(l.instance, Transform3D::new(Basis::IDENTITY, gpu::v3(out.pos)));
                    if !l.shown { rs.instance_set_visible(l.instance, true); l.shown = true; }
                    self.lights_shown += 1;
                }
                None => if l.shown { rs.instance_set_visible(l.instance, false); l.shown = false; },
            }
        }
    }

    /// Free every RID. Idempotent.
    pub(crate) fn free(&mut self) {
        let mut rs = gpu::rs();
        self.slots.iter_mut().for_each(|s| s.free(&mut rs));
        self.slots.clear();
        for l in &mut self.lights { gpu::free_rid(&mut rs, &mut l.instance); gpu::free_rid(&mut rs, &mut l.light); }
        self.lights.clear();
        for rid in self.meshes.iter_mut().chain(self.materials.iter_mut()).chain(self.ramps.iter_mut()) { gpu::free_rid(&mut rs, rid); }
        self.meshes.clear();
        self.materials.clear();
        self.ramps.clear();
        let _ = VfxKind::ALL;
    }
}
