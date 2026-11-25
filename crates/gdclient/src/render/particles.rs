//! Unit `gd-render-particles`. VFX layer B, Forward+ only: pooled GPU particle emitters for spray, sparks,
//! embers, flak and mist. DESIGN.md L1951 (seed emitter plus sub-emitter per anchor, at most 20 nodes, at
//! most 128 `emit_particle` calls per frame, `local_coords` true, `fixed_fps` 30 interpolated, no trails,
//! collision or shadows, turbulence on at most 2 emitters), L1973 (at most 12 process materials; loading
//! calls `request_particles_process(0.1)` on hidden copies), L2026-2031 (degrade: sparks, embers and spray go
//! first; then 15 fps), S7 (halve the cap until it fits).
//! Absent on the Compatibility fallback, where layer A alone carries every gameplay cue.
//! The live cap starts at `hull_mesh::vfx::layer_b_particle_cap(tier)` (0 on `QualityTier::Fallback`); layer B
//! has no meshes in `hull-mesh`.
//!
//! # How the unit is built
//! [`core`] decides every emission with no Godot in it (sources, emitters, the 128-call budget, the cap
//! split, the degrade steps) and tests headless; [`gpu`] holds the 20 `RenderingServer` particle RIDs, five
//! process materials on `vfx_particles.gdshader`, five draw-pass materials on the VFX shaders, and issues the
//! `particles_emit` calls. The live cap is the tier's, times `QualityProfile::particle_scale`, then capped
//! by [`ParticleRenderer::set_cap`] (the S7 halving): it is applied on the next `update`.
pub(crate) mod core;
pub(crate) mod gpu;

use super::{FrameContext, RenderStats};
use crate::enums::QualityTier;

pub struct ParticleRenderer {
    pub(crate) cap: u32,
    pub(crate) enabled: bool,
    pub(crate) stats: RenderStats,
    pub(crate) tier: QualityTier,
    pub(crate) core: core::ParticleCore,
    pub(crate) gpu: Option<gpu::Gpu>,
}

/// The tier's Layer B cap (0 on the fallback).
pub(crate) fn tier_cap(tier: QualityTier) -> u32 { hull_mesh::vfx::layer_b_particle_cap(tier) as u32 }

impl ParticleRenderer {
    /// `forward_plus` false builds nothing.
    pub fn new(scenario: godot::prelude::Rid, tier: QualityTier, forward_plus: bool) -> Self {
        let enabled = forward_plus && tier_cap(tier) > 0;
        let cap = if enabled { tier_cap(tier) } else { 0 };
        let core = core::ParticleCore::new(cap);
        let gpu = if enabled { Some(gpu::Gpu::new(scenario, &core)) } else { None };
        Self { cap, enabled, stats: RenderStats::default(), tier, core, gpu }
    }
    /// Per frame: move anchors with the render origin, emit from this frame's events within the call cap.
    pub fn update(&mut self, ctx: &FrameContext) {
        if !self.enabled { self.stats = RenderStats::default(); return; }
        let scale = if ctx.quality.particle_scale.is_finite() { ctx.quality.particle_scale.clamp(0.0, 1.0) } else { 1.0 };
        let live_cap = (self.cap.min(tier_cap(ctx.quality.tier)) as f32 * scale) as u32;
        let resized = live_cap != self.core.cap && self.core.set_cap(live_cap);
        let camera = super::hull::gpu::camera_render_pos(ctx);
        let forward = glam::Vec3::from(ctx.camera.forward);
        let forward = if forward.is_finite() { forward.normalize_or(glam::Vec3::NEG_Z) } else { glam::Vec3::NEG_Z };
        self.core.step(ctx.view, ctx.events, camera, forward, ctx.degrade_step, &|hash| super::hull::design::lookup(hash));
        if let Some(g) = &mut self.gpu { g.update(&self.core, resized); }
        self.stats = RenderStats { particles: self.core.live, ..RenderStats::default() };
    }
    pub fn warm_up(&mut self) {
        self.core.warm = true;
        if let Some(g) = &mut self.gpu { g.warm = true; }
    }
    /// Live particle cap: the tier's cap, halved by the S7 fallback or the degrade order as needed.
    pub fn set_cap(&mut self, cap: u32) { self.cap = cap; }
    pub fn stats(&self) -> RenderStats { self.stats }
    pub fn free(&mut self) {
        if let Some(mut g) = self.gpu.take() { g.free(); }
        self.enabled = false;
        self.stats = RenderStats::default();
    }
}
