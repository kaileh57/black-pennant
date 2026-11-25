//! Unit `gd-render-vfx`. VFX layer A, drawn on every renderer: one MultiMesh per `hull_mesh::vfx::VfxKind`,
//! one `multimesh_set_buffer` per kind per frame, 80 bytes per instance, age written into `custom.x`.
//! DESIGN.md L1947-1973 (layers, style, emission ladder, per-effect rules, lights, caps), L1957 and L1717
//! (flash limit: more than 2.5% of screen blooming counts against 3 per second; extras draw at 25% emission
//! with no light; Reduce flashing caps emission at 4), L2026-2033 (degrade order and what never degrades),
//! L1831 (anchors live 30 s past an origin rebase).
//!
//! The bookkeeping is `hull-mesh`'s and is not repeated here: this unit owns one
//! `hull_mesh::vfx::runtime::RingPool` per kind (sized by `CapGroup::cap(tier)`, protected slots by
//! `Protection`), builds instances with `splash_layout`, `hit_layout`, `bow_wave_instance` and
//! `trail_segment`, runs every flash through one `FlashLimiter`, calls
//! `RingPool::tick`, `rebase` (on `ViewFrame::origin_shift`) and `write_buffer` (which sorts the blended
//! kinds back to front), and uploads the result. Meshes are `hull_mesh::vfx::meshes::base_mesh(kind)`;
//! each material samples the ramp `gen::ramp_path(kind.ramp().name())`; emission values come from
//! `hull_mesh::vfx::emission`.
//!
//! Effects (L1961-1969): splash prisms and foam rings, hit flashes and debris cubes, fire prisms and smoke
//! puffs, defence smoke (a simulation object, same on every tier), missile trails, wakes (bow wave sheets
//! here; the ribbon and the Kelvin arms above 8 kn are each hull's stern track, [`wake`], drawn by the sea's
//! material so they light and move with the waves), muzzle stars and lights, flooding cues (pump arcs, boil rings, growing foam
//! band). Tracers belong to `gd-render-projectiles`.
//! Never degraded: own fall-of-shot splashes (`PROTECTED_OWN_SPLASHES` slots), incoming missile heads, live
//! tracers, hit flashes, fire presence, flooding and list cues, defence smoke.
//!
//! # How the unit is built
//! [`core`] holds every decision with no Godot in it (events and their timing, attached effects, the flash
//! limiter, degrade steps, lights) and its tests run headless; [`gpu`] uploads one buffer per kind and
//! places the pooled OmniLights. Muzzle positions are the part anchors when the view hands them over
//! ([`VfxRenderer::set_anchors`]); without them a muzzle is placed from the mount's footprint and its
//! replicated train and elevation, so the unit works from the [`FrameContext`] alone.
pub(crate) mod check;
pub(crate) mod core;
pub(crate) mod gpu;
pub(crate) mod wake;

use super::parts::AnchorPose;
use super::projectiles::ShellEnd;
use super::{FrameContext, RenderStats};
use crate::enums::QualityTier;
use hull_mesh::vfx::runtime::{FlashLimiter, RingPool};

pub struct VfxRenderer {
    pub(crate) tier: QualityTier,
    /// One pool per `VfxKind::ALL` entry except `Tracer`, in that order.
    pub(crate) pools: Vec<RingPool>,
    pub(crate) flashes: FlashLimiter,
    pub(crate) stats: RenderStats,
    /// Everything else of the frame loop; its `pools` are swapped in from [`VfxRenderer::pools`] for the
    /// length of `update`, so the field above always holds the live pools between frames.
    pub(crate) core: core::VfxCore,
    pub(crate) gpu: Option<gpu::Gpu>,
}

impl VfxRenderer {
    /// Create the VFX meshes, their materials and ramps, one MultiMesh and `RingPool` per kind, and the pooled
    /// OmniLight RIDs (`vfx_light_budget(tier)`).
    pub fn new(scenario: godot::prelude::Rid, tier: QualityTier) -> Self {
        let mut core = core::VfxCore::new(tier);
        let pools = std::mem::take(&mut core.pools);
        let gpu = gpu::Gpu::new(scenario, tier);
        Self { tier, pools, flashes: FlashLimiter::default(), stats: RenderStats::default(), core, gpu: Some(gpu) }
    }
    /// Per frame: spawn effects from physical events and `shell_ends`, age and retire instances, follow wakes
    /// and trails from `view`, apply the flash limit and the degrade step, write and upload each buffer.
    pub fn update(&mut self, ctx: &FrameContext, shell_ends: &[ShellEnd]) {
        if ctx.delta_s.is_finite() { self.core.real_s += ctx.delta_s.clamp(0.0, 1.0); }
        let forward = glam::Vec3::from(ctx.camera.forward);
        let lens = core::Lens {
            camera: super::hull::gpu::camera_render_pos(ctx),
            forward: if forward.is_finite() { forward.normalize_or(glam::Vec3::NEG_Z) } else { glam::Vec3::NEG_Z },
            real_s: self.core.real_s,
            px_per_m: super::hull::gpu::px_per_m_at_1m(&ctx.camera),
            viewport_px: ctx.camera.viewport_px,
            tier: self.tier,
            degrade_step: ctx.degrade_step,
            reduce_flashing: ctx.reduce_flashing,
        };
        self.step(ctx.view, ctx.events, shell_ends, lens, &|hash| super::hull::design::lookup(hash));
        let Some(g) = &mut self.gpu else { return };
        if g.tier != self.tier { g.set_tier(self.tier); }
        g.upload(&self.core, &self.pools, ctx.reduce_flashing);
        self.stats.lights = g.lights_shown;
    }
    /// The Godot-free part of `update`.
    pub(crate) fn step(&mut self, view: &crate::interp::ViewFrame, events: &[crate::seams::WireEvent], shell_ends: &[ShellEnd], lens: core::Lens, lookup: &dyn Fn(&content::ids::ContentHash) -> Option<std::sync::Arc<super::hull::design::DrawDesign>>) {
        std::mem::swap(&mut self.pools, &mut self.core.pools);
        self.core.step(view, events, shell_ends, lens, &mut self.flashes, lookup);
        std::mem::swap(&mut self.pools, &mut self.core.pools);
        self.core.anchors.clear();
        self.stats = RenderStats { vfx_instances: self.pools.iter().map(|p| p.live_count() as u32).sum(), lights: self.core.lights.len() as u32, ..RenderStats::default() };
    }
    /// Muzzle and pump-outlet anchors of the drawn parts for the next `update`
    /// (`PartRenderer::anchors(AnchorKind::Muzzle | PumpOutlet, ..)`); other kinds are ignored. Optional:
    /// without them muzzles are placed from the design. Consumed by the next `update`.
    pub fn set_anchors(&mut self, anchors: &[AnchorPose]) {
        use hull_mesh::parts::AnchorKind;
        self.core.anchors.extend(anchors.iter().filter(|a| matches!(a.kind, AnchorKind::Muzzle | AnchorKind::PumpOutlet)).copied());
    }
    /// Draw every effect type once, off screen, so no pipeline compiles at first use (L1358, L1973).
    ///
    /// The instances are placed on the next `update`, a metre across, 40 m in front of the camera for one
    /// frame: a culled instance compiles nothing, so they must be inside the view.
    pub fn warm_up(&mut self) { self.core.warm = true; }
    /// Rebuild the pools for another tier's caps, keeping protected instances.
    pub fn set_tier(&mut self, tier: QualityTier) {
        if tier == self.tier { return; }
        self.tier = tier;
        std::mem::swap(&mut self.pools, &mut self.core.pools);
        let now = self.core.now_s();
        self.core.set_tier(tier, now);
        std::mem::swap(&mut self.pools, &mut self.core.pools);
    }
    pub fn stats(&self) -> RenderStats { self.stats }
    pub fn free(&mut self) {
        if let Some(mut g) = self.gpu.take() { g.free(); }
        self.stats = RenderStats::default();
    }
}
