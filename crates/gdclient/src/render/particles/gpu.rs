//! Part of unit `gd-render-particles`. The engine side of Layer B: 20 `RenderingServer` particle systems
//! (GPUParticles3D without nodes), five process materials on `vfx_particles.gdshader` (one per kind, at most
//! 12 allowed, L1973) and five draw-pass materials on the VFX shaders. Every emitter runs with `emitting`
//! false, `local_coords` true, `fixed_fps` 30 (15 from degrade step 3) interpolated, INDEX draw order, no
//! trails, collision or shadows (L1951); particles arrive only through `particles_emit`. Main thread only;
//! exercised only inside Godot.
use super::core::{ParticleCore, ParticleKind, SYSTEM_LIFETIME_S, AABB_PAD_M};
use crate::render::hull::gpu;
use crate::render::LAYER_VFX;
use godot::classes::rendering_server::{ParticlesDrawOrder, ParticlesMode, ShadowCastingSetting};
use godot::classes::{RenderingServer, Shader};
use godot::prelude::*;

pub(crate) const PROCESS_SHADER: &str = "res://shaders/vfx_particles.gdshader";

mod param {
    pub const PARTICLE_KIND: &str = "particle_kind";
    pub const WIND: &str = "wind";
    pub const LIFE_SCALE: &str = "life_scale";
}

struct System { particles: Rid, instance: Rid, kind: ParticleKind, amount: u32, at: Vector3, min: Vector3, max: Vector3, shown: bool }

pub(crate) struct Gpu {
    _shaders: Vec<Option<Gd<Shader>>>,
    process: Vec<Rid>,
    draw: Vec<Rid>,
    meshes: Vec<Rid>,
    ramps: Vec<Rid>,
    systems: Vec<System>,
    fps: i32,
    wind: Vector3,
    pub(crate) warm: bool,
}

impl Gpu {
    pub(crate) fn new(scenario: Rid, core: &ParticleCore) -> Self {
        let mut rs = gpu::rs();
        let process_shader = gpu::load_shader(PROCESS_SHADER);
        let opaque = gpu::load_shader(crate::render::vfx::gpu::OPAQUE_SHADER);
        let blend = gpu::load_shader(crate::render::vfx::gpu::BLEND_SHADER);
        if process_shader.is_none() { godot_warn!("particle renderer: {PROCESS_SHADER} is missing; Layer B draws nothing"); }
        let (mut process, mut draw, mut meshes, mut ramps) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
        for kind in ParticleKind::ALL {
            let p = gpu::material_on(&mut rs, &process_shader);
            if p.is_valid() {
                rs.material_set_param(p, param::PARTICLE_KIND, &(kind.index() as i64).to_variant());
                rs.material_set_param(p, param::LIFE_SCALE, &1.0f32.to_variant());
            }
            let d = gpu::material_on(&mut rs, if kind.blended() { &blend } else { &opaque });
            let ramp = gpu::texture_from_gen(&mut rs, &crate::seams::gen::ramp_path(kind.ramp().name()));
            if d.is_valid() {
                rs.material_set_param(d, crate::render::vfx::gpu::param::VFX_KIND, &kind.draw_kind().to_variant());
                if ramp.is_valid() { rs.material_set_param(d, crate::render::vfx::gpu::param::RAMP, &ramp.to_variant()); }
                rs.material_set_param(d, crate::render::vfx::gpu::param::EMISSION_CAP, &hull_mesh::vfx::emission::FLASH.1.to_variant());
            }
            meshes.push(gpu::mesh_from_data(&mut rs, &hull_mesh::vfx::meshes::base_mesh(kind.mesh()), d));
            process.push(p);
            draw.push(d);
            ramps.push(ramp);
        }
        let mut g = Self { _shaders: vec![process_shader, opaque, blend], process, draw, meshes, ramps, systems: Vec::new(), fps: super::core::FIXED_FPS, wind: Vector3::ZERO, warm: false };
        for e in &core.emitters {
            let particles = rs.particles_create();
            rs.particles_set_mode(particles, ParticlesMode::MODE_3D);
            rs.particles_set_emitting(particles, false);
            rs.particles_set_amount(particles, e.amount.max(1) as i32);
            rs.particles_set_lifetime(particles, SYSTEM_LIFETIME_S);
            rs.particles_set_one_shot(particles, false);
            rs.particles_set_use_local_coordinates(particles, true);
            rs.particles_set_fixed_fps(particles, g.fps);
            rs.particles_set_interpolate(particles, true);
            rs.particles_set_fractional_delta(particles, true);
            rs.particles_set_draw_order(particles, ParticlesDrawOrder::INDEX);
            rs.particles_set_trails(particles, false, 0.3);
            rs.particles_set_draw_passes(particles, 1);
            let instance = rs.instance_create2(particles, scenario);
            rs.instance_set_layer_mask(instance, LAYER_VFX);
            rs.instance_geometry_set_cast_shadows_setting(instance, ShadowCastingSetting::OFF);
            rs.instance_set_visible(instance, false);
            let mut s = System { particles, instance, kind: e.kind, amount: e.amount, at: Vector3::ZERO, min: Vector3::ZERO, max: Vector3::ZERO, shown: false };
            g.bind(&mut rs, &mut s);
            g.systems.push(s);
        }
        g
    }

    fn bind(&self, rs: &mut Gd<RenderingServer>, s: &mut System) {
        let k = s.kind.index();
        rs.particles_set_process_material(s.particles, self.process[k]);
        rs.particles_set_draw_pass_mesh(s.particles, 0, self.meshes[k]);
    }

    pub(crate) fn update(&mut self, core: &ParticleCore, resized: bool) {
        let mut rs = gpu::rs();
        if core.fps != self.fps {
            self.fps = core.fps;
            for s in &self.systems { rs.particles_set_fixed_fps(s.particles, self.fps); }
        }
        let wind = gpu::v3(core.wind);
        if wind != self.wind {
            self.wind = wind;
            for p in &self.process { if p.is_valid() { rs.material_set_param(*p, param::WIND, &wind.to_variant()); } }
        }
        for (i, e) in core.emitters.iter().enumerate() {
            let Some(s) = self.systems.get_mut(i) else { break };
            if resized && e.amount != s.amount {
                s.amount = e.amount;
                rs.particles_set_amount(s.particles, e.amount.max(1) as i32);
            }
            if e.kind != s.kind {
                s.kind = e.kind;
                let k = s.kind.index();
                rs.particles_set_process_material(s.particles, self.process[k]);
                rs.particles_set_draw_pass_mesh(s.particles, 0, self.meshes[k]);
            }
            let at = gpu::v3(e.at);
            if at != s.at {
                s.at = at;
                rs.instance_set_transform(s.instance, Transform3D::new(Basis::IDENTITY, at));
            }
            let (min, max) = (gpu::v3(e.min - AABB_PAD_M), gpu::v3(e.max + AABB_PAD_M));
            if min != s.min || max != s.max {
                s.min = min;
                s.max = max;
                rs.particles_set_custom_aabb(s.particles, Aabb::new(min, max - min));
            }
        }
        use godot::classes::RenderingServer as R;
        for emit in &core.emits {
            let Some(s) = self.systems.get_mut(emit.emitter as usize) else { continue };
            let mut flags = R::PARTICLES_EMIT_FLAG_POSITION | R::PARTICLES_EMIT_FLAG_CUSTOM;
            let velocity = match emit.velocity { Some(v) => { flags |= R::PARTICLES_EMIT_FLAG_VELOCITY; gpu::v3(v) } None => Vector3::ZERO };
            // custom = (age, lifetime or 0 for the kind's own, seed, height of the emission point above the sea).
            let custom = Color::from_rgba(0.0, emit.life_s, emit.seed, emit.local.y);
            let xf = Transform3D::new(Basis::IDENTITY, gpu::v3(emit.local));
            rs.particles_emit(s.particles, xf, velocity, Color::from_rgba(1.0, 1.0, 1.0, 1.0), custom, flags as u32);
            if !s.shown { rs.instance_set_visible(s.instance, true); s.shown = true; }
        }
        if self.warm {
            self.warm = false;
            for s in &self.systems { rs.particles_request_process_time(s.particles, 0.1); }
        }
    }

    /// Free every RID. Idempotent.
    pub(crate) fn free(&mut self) {
        let mut rs = gpu::rs();
        for s in &mut self.systems { gpu::free_rid(&mut rs, &mut s.instance); gpu::free_rid(&mut rs, &mut s.particles); }
        self.systems.clear();
        for rid in self.meshes.iter_mut().chain(self.draw.iter_mut()).chain(self.process.iter_mut()).chain(self.ramps.iter_mut()) { gpu::free_rid(&mut rs, rid); }
        self.meshes.clear();
        self.draw.clear();
        self.process.clear();
        self.ramps.clear();
    }
}
