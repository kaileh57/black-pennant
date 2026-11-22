//! Unit `gd-render-projectiles`. Shells, tracers, guided bodies and decoys.
//! DESIGN.md L926 (shells are never in snapshots: one spawn event, client integration with the same drag
//! model, then a reliable impact event; guided missile state at 5 Hz, interpolated), L1961 (tracers are
//! opaque emissive boxes, length speed x 1/40 s, at least 1.5 px wide), L939 (client-integrated shell
//! positions within 0.5 m of the server's at impact), L1972 (2,000 live).
//!
//! This unit integrates nothing. Shells are flown by the link (`net::interp::ShellTracker`, driven by
//! `net::ClientSession` or by `link::channel::Presenter`) and arrive as `ViewFrame::shells`, already at the
//! render tick and in render space. What a shell looks like comes from its `PhysEvent::ProjectileSpawned`
//! (`sim_core::projectiles::ProjectileSpawn`: calibre, band), which this unit keeps by id from the frame's
//! events until the shell leaves `ViewFrame::shells`.
//!
//! Tracers are `hull_mesh::vfx::VfxKind::Tracer` instances built with
//! `hull_mesh::vfx::runtime::tracer_instance` in a `RingPool` (cap group `Projectiles`). Guided bodies and
//! decoys are `ViewFrame::bodies` and `ViewFrame::decoys` drawn with `hull_mesh::parts::build_body_model`
//! models, one MultiMesh per (body generator, munition) piece, in the part vertex format.
//!
//! # Timing of the end of a shell
//! The link removes a shell from `ViewFrame::shells` when its impact or splash event arrives, which is the
//! interpolation delay (100 to 150 ms) ahead of the playback tick everything else is drawn at. So this unit
//! coasts the tracer along its last velocity until the playback tick reaches the event's tick and reports
//! the [`ShellEnd`] then: the tracer flies into its splash, and the splash stands where the drawn ship is.
//! An event more than [`END_HOLD_MAX_TICKS`] ahead (a seek, a stalled clock) is reported at once.
//!
//! # What is drawn
//! - Every client-flown shell carries a tracer (`LiveShell::tracer` is true for all of them): fall of shot
//!   has to be readable, and the tracer is the only picture a shell has.
//! - A shell that appears in `ViewFrame::shells` with no remembered spawn (the renderer was built after its
//!   event) is drawn at [`DEFAULT_CALIBRE_MM`].
//! - Bodies point their nose along their velocity with no roll; a body slower than [`HEADING_MIN_MPS`] keeps
//!   its last heading. Rotors and propellers turn by simulation time, so a pause stops them.
//! - Of the decoys only the towed decoy is a body. Chaff, flares and defence smoke are layer A effects of
//!   `gd-render-vfx`.
use super::hull::gpu::{self, InstanceBuffer, MultiMeshSlot};
use super::{FrameContext, RenderStats, LAYER_PARTS, LAYER_VFX};
use crate::enums::QualityTier;
use crate::interp::ViewFrame;
use crate::seams::WireEvent;
use content::ids::SideId;
use glam::{Affine3A, DVec3, Mat3A, Quat, Vec3};
use godot::prelude::*;
use hull_mesh::parts::{Joint, PartModel, PieceRole};
use hull_mesh::vfx::runtime::{tracer_instance, Protection, RingPool, VfxHandle};
use hull_mesh::vfx::{CapGroup, VfxKind};
use sim_core::events::PhysEvent;
use sim_core::projectiles::ProjectileId;
use sim_core::EntityId;
use sim_systems::protocol::SimEvent;
use std::collections::BTreeMap;

/// Calibre drawn for a shell whose spawn event this renderer never saw.
pub const DEFAULT_CALIBRE_MM: u16 = 76;
/// A remembered spawn that has not shown up in `ViewFrame::shells` this many ticks after its spawn tick is
/// forgotten (its end event was lost with a hidden entity, or the link dropped it).
pub const UNSEEN_MAX_TICKS: f64 = 4.0 * sim_core::TICK_HZ as f64;
/// Longest a finished shell is coasted while the playback tick catches up with its end event.
pub const END_HOLD_MAX_TICKS: f64 = 0.5 * sim_core::TICK_HZ as f64;
/// Below this speed a body keeps its previous heading.
pub const HEADING_MIN_MPS: f32 = 0.5;
/// Turn rates of spinning body pieces, revolutions per second. Chosen off the divisors of 30 and 60 Hz so
/// blades do not stand still on screen.
const ROTOR_REV_S: f32 = 4.3;
const PROPELLER_REV_S: f32 = 6.7;
/// Margin added to the tracer MultiMesh's box: the longest tracer (1,000 m/s for 1/40 s) and its width.
const TRACER_BOUND_M: f32 = 30.0;

/// What the renderer remembers about one live shell between its spawn event and its last pose.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LiveShell { pub id: sim_core::projectiles::ProjectileId, pub calibre_mm: u16, pub spawn_tick: u64, pub tracer: bool, pub own_side: bool }

/// Private companion of one [`LiveShell`], same index.
#[derive(Clone, Copy, Debug)]
pub(crate) struct ShellExtra { shooter: EntityId, side_known: bool, handle: Option<VfxHandle>, seen: bool, pos: Vec3, vel: Vec3 }
impl ShellExtra {
    fn new(shooter: EntityId) -> Self { Self { shooter, side_known: false, handle: None, seen: false, pos: Vec3::ZERO, vel: Vec3::ZERO } }
}

/// A shell whose end event has arrived but which the playback tick has not reached yet.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Ending { end: DVec3, tick: u64, calibre_mm: u16, water: bool, own_side: bool, handle: Option<VfxHandle>, pos: Vec3, vel: Vec3 }

/// Godot-free state of the shells: the tracer pool, who shot what, and the shells still coasting to their end.
pub(crate) struct Tracers {
    pub(crate) pool: RingPool,
    pub(crate) extra: Vec<ShellExtra>,
    pub(crate) ending: Vec<Ending>,
    sides: BTreeMap<EntityId, SideId>,
    last_tick: Option<f64>,
    scratch_shells: Vec<LiveShell>,
    scratch_extra: Vec<ShellExtra>,
    pub(crate) floats: Vec<f32>,
}

impl Tracers {
    pub(crate) fn new(tier: QualityTier) -> Self {
        Self { pool: RingPool::new(VfxKind::Tracer, tier), extra: Vec::new(), ending: Vec::new(), sides: BTreeMap::new(), last_tick: None, scratch_shells: Vec::new(), scratch_extra: Vec::new(), floats: Vec::new() }
    }

    fn reset(&mut self, shells: &mut Vec<LiveShell>) {
        for e in self.extra.drain(..) { if let Some(h) = e.handle { self.pool.retire(h); } }
        for e in self.ending.drain(..) { if let Some(h) = e.handle { self.pool.retire(h); } }
        shells.clear();
        self.sides.clear();
        self.last_tick = None;
    }

    fn own_side(&self, view: &ViewFrame) -> Option<SideId> {
        let own = view.vessels.get(view.own? as usize)?;
        self.sides.get(&own.entity).copied()
    }

    /// One frame of shell bookkeeping. Appends to `ends` every shell whose end the playback tick has reached,
    /// in render space, and leaves the tracer buffer in `self.floats` (`pool.visible_count()` instances).
    pub(crate) fn step(&mut self, shells: &mut Vec<LiveShell>, tier: QualityTier, view: &ViewFrame, events: &[WireEvent], camera: Vec3, px_per_m: f32, ends: &mut Vec<ShellEnd>) {
        let now_s = view.sim_time_s;
        let tick = view.render_tick;
        // A change of tier changes the cap: keep what fits and follow the slots that moved.
        if self.pool.cap != CapGroup::Projectiles.cap(tier) {
            let moved = self.pool.retier(now_s, tier);
            if !moved.is_empty() {
                let map: BTreeMap<(u32, u32), VfxHandle> = moved.iter().map(|(old, new)| ((old.slot, old.generation), *new)).collect();
                let follow = |h: &mut Option<VfxHandle>| if let Some(old) = *h { if let Some(new) = map.get(&(old.slot, old.generation)) { *h = Some(*new); } };
                self.extra.iter_mut().for_each(|e| follow(&mut e.handle));
                self.ending.iter_mut().for_each(|e| follow(&mut e.handle));
            }
        }
        if let Some(shift) = view.origin_shift {
            let d = Vec3::from(shift);
            if d.is_finite() {
                self.pool.rebase(shift);
                self.extra.iter_mut().for_each(|e| e.pos += d);
                self.ending.iter_mut().for_each(|e| e.pos += d);
            }
        }

        // 1. Events: sides, spawns, ends.
        for ev in events {
            match &ev.event {
                SimEvent::SortieStarted { .. } => self.reset(shells),
                SimEvent::EntityRevealed(intro) => { self.sides.insert(intro.entity, intro.side); }
                SimEvent::Phys(PhysEvent::VesselSpawned { vessel, side }) => { self.sides.insert(*vessel, *side); }
                SimEvent::Phys(PhysEvent::ProjectileSpawned(spawn)) => {
                    let live = LiveShell { id: spawn.id, calibre_mm: spawn.calibre_mm, spawn_tick: spawn.tick, tracer: true, own_side: false };
                    match shells.binary_search_by_key(&spawn.id, |s| s.id) {
                        Ok(i) => { shells[i] = LiveShell { own_side: shells[i].own_side, ..live }; self.extra[i].shooter = spawn.shooter; self.extra[i].side_known = false; }
                        Err(i) => { shells.insert(i, live); self.extra.insert(i, ShellExtra::new(spawn.shooter)); }
                    }
                }
                SimEvent::Phys(PhysEvent::Splash { projectile, pos, calibre_mm, shooter }) => self.end(shells, view, *projectile, *pos, Some(*calibre_mm), true, *shooter, ev.tick),
                SimEvent::Phys(PhysEvent::ShellImpact { projectile, world_point, shooter, .. }) => self.end(shells, view, *projectile, *world_point, None, false, *shooter, ev.tick),
                _ => {}
            }
        }
        let own_side = self.own_side(view);

        // 2. Walk this frame's poses against what is remembered (both ascend by id).
        self.scratch_shells.clear();
        self.scratch_extra.clear();
        let mut known = 0usize;
        let place = |pool: &mut RingPool, live: &LiveShell, extra: &mut ShellExtra, pos: Vec3, vel: Vec3| {
            extra.pos = pos;
            extra.vel = vel;
            extra.seen = true;
            if !live.tracer { return; }
            let inst = tracer_instance(pos.into(), vel.into(), live.calibre_mm, (pos - camera).length(), px_per_m);
            match extra.handle.filter(|h| pool.is_live(*h)) {
                Some(h) => pool.update(h, inst),
                None => extra.handle = pool.spawn(now_s, f32::INFINITY, Protection::None, inst),
            }
        };
        for pose in &view.shells {
            // Remembered shells below this id have no pose this frame.
            while known < shells.len() && shells[known].id < pose.id {
                let (live, extra) = (shells[known], self.extra[known]);
                known += 1;
                if keep_unposed(&live, &extra, tick) { self.scratch_shells.push(live); self.scratch_extra.push(extra); } else if let Some(h) = extra.handle { self.pool.retire(h); }
            }
            let (mut live, mut extra) = if known < shells.len() && shells[known].id == pose.id {
                known += 1;
                (shells[known - 1], self.extra[known - 1])
            } else {
                (LiveShell { id: pose.id, calibre_mm: DEFAULT_CALIBRE_MM, spawn_tick: tick.max(0.0) as u64, tracer: true, own_side: false }, ShellExtra::new(EntityId::NONE))
            };
            if !extra.side_known {
                if let (Some(own), Some(side)) = (own_side, self.sides.get(&extra.shooter)) { live.own_side = *side == own; extra.side_known = true; }
            }
            place(&mut self.pool, &live, &mut extra, Vec3::from(pose.pos), Vec3::from(pose.vel));
            self.scratch_shells.push(live);
            self.scratch_extra.push(extra);
        }
        while known < shells.len() {
            let (live, extra) = (shells[known], self.extra[known]);
            known += 1;
            if keep_unposed(&live, &extra, tick) { self.scratch_shells.push(live); self.scratch_extra.push(extra); } else if let Some(h) = extra.handle { self.pool.retire(h); }
        }
        std::mem::swap(shells, &mut self.scratch_shells);
        std::mem::swap(&mut self.extra, &mut self.scratch_extra);

        // 3. Shells coasting to their end: report the ones playback has reached, move the rest.
        let dt_s = match self.last_tick { Some(last) if tick >= last => ((tick - last) / sim_core::TICK_HZ as f64).min(0.25) as f32, _ => 0.0 };
        let rewound = self.last_tick.is_some_and(|last| tick < last);
        self.last_tick = Some(tick);
        let origin = view.origin;
        let pool = &mut self.pool;
        self.ending.retain_mut(|e| {
            let ahead = e.tick as f64 - tick;
            let end = Vec3::from(origin.to_render(e.end));
            if ahead <= 0.0 || ahead > END_HOLD_MAX_TICKS || rewound || !end.is_finite() {
                if let Some(h) = e.handle { pool.retire(h); }
                if end.is_finite() { ends.push(ShellEnd { pos: end.into(), calibre_mm: e.calibre_mm, water: e.water, own_side: e.own_side }); }
                return false;
            }
            if let Some(h) = e.handle.filter(|h| pool.is_live(*h)) {
                // Never past the end point: a shell that reaches it early waits there.
                let step = e.vel * dt_s;
                let to_end = end - e.pos;
                e.pos = if step.length_squared() >= to_end.length_squared() || to_end.dot(e.vel) <= 0.0 { end } else { e.pos + step };
                pool.update(h, tracer_instance(e.pos.into(), e.vel.into(), e.calibre_mm, (e.pos - camera).length(), px_per_m));
            }
            true
        });

        // 4. Age, then write the buffer.
        self.pool.tick(now_s);
        self.pool.write_buffer(camera.into(), &mut self.floats);
    }

    /// An impact or splash event for `id`: forget the shell and queue its end for when playback gets there.
    #[allow(clippy::too_many_arguments)]
    fn end(&mut self, shells: &mut Vec<LiveShell>, view: &ViewFrame, id: ProjectileId, at: DVec3, calibre_mm: Option<u16>, water: bool, shooter: EntityId, tick: u64) {
        if !at.is_finite() { return; }
        let own_side = self.own_side(view);
        let by_shooter = |sides: &BTreeMap<EntityId, SideId>| matches!((own_side, sides.get(&shooter)), (Some(own), Some(side)) if *side == own);
        let ending = match shells.binary_search_by_key(&id, |s| s.id) {
            Ok(i) => {
                let (live, extra) = (shells.remove(i), self.extra.remove(i));
                let own = if extra.side_known { live.own_side } else { by_shooter(&self.sides) };
                // A shell never drawn has no tracer to coast; its end is still reported on time.
                let handle = if extra.seen { extra.handle } else { if let Some(h) = extra.handle { self.pool.retire(h); } None };
                Ending { end: at, tick, calibre_mm: calibre_mm.unwrap_or(live.calibre_mm), water, own_side: own, handle, pos: extra.pos, vel: extra.vel }
            }
            Err(_) => Ending { end: at, tick, calibre_mm: calibre_mm.unwrap_or(DEFAULT_CALIBRE_MM), water, own_side: by_shooter(&self.sides), handle: None, pos: Vec3::ZERO, vel: Vec3::ZERO },
        };
        self.ending.push(ending);
    }
}

/// Whether a remembered shell with no pose this frame stays remembered: one already drawn has ended without
/// an event (its link forgot it), one not yet drawn waits for playback to reach its spawn.
fn keep_unposed(live: &LiveShell, extra: &ShellExtra, render_tick: f64) -> bool {
    !extra.seen && render_tick <= live.spawn_tick as f64 + UNSEEN_MAX_TICKS
}

/// A body's frame from its heading: +X along `dir`, +Y as near world up as that allows.
pub(crate) fn heading_basis(dir: Vec3) -> Mat3A {
    let x = dir.normalize_or(Vec3::X);
    let up = if x.y.abs() > 0.999 { Vec3::X } else { Vec3::Y };
    let z = x.cross(up).normalize_or(Vec3::Z);
    Mat3A::from_cols(x.into(), z.cross(x).into(), z.into())
}

/// One piece of a body model as the per-frame loop needs it.
pub(crate) struct BodyPiece { parent: Option<u8>, pivot: Vec3, spin: Option<(Vec3, f32)>, radius: f32, pub(crate) buffer: InstanceBuffer }
/// One munition's (or the towed decoy's) model.
pub(crate) struct BodyModel { pub(crate) pieces: Vec<BodyPiece>, painted: bool }

impl BodyModel {
    pub(crate) fn from_model(model: &PartModel) -> Self {
        let pieces = model.pieces.iter().enumerate().map(|(i, p)| {
            let spin = match p.joint {
                Joint::Spin { axis } => Some((Vec3::from(axis).normalize_or(Vec3::X), if p.role == PieceRole::Rotor { ROTOR_REV_S } else { PROPELLER_REV_S })),
                _ => None,
            };
            // A parent index that does not precede the piece would loop: treat the piece as a root.
            let parent = p.parent.filter(|&q| (q as usize) < i);
            let radius = p.mesh.positions.iter().map(|v| Vec3::from(*v).length()).fold(0.0f32, f32::max);
            BodyPiece { parent, pivot: Vec3::from(p.pivot), spin, radius, buffer: InstanceBuffer::default() }
        }).collect();
        let painted = model.pieces.iter().any(|p| p.mesh.colors.iter().any(|c| c[3] > 0.5));
        Self { pieces, painted }
    }
    fn begin(&mut self) { self.pieces.iter_mut().for_each(|p| p.buffer.begin()); }
    /// Append one body at `base` with its spinning pieces at `spin_turns` revolutions per unit rate.
    fn place(&mut self, base: Affine3A, sim_time_s: f64, phase: f32, paint: [f32; 3], scratch: &mut Vec<Affine3A>) {
        scratch.clear();
        let (color, custom) = super::parts::instance_data(if self.painted { paint } else { [0.0; 3] }, 0.0);
        for piece in &mut self.pieces {
            let parent = piece.parent.and_then(|q| scratch.get(q as usize).copied()).unwrap_or(base);
            let mut local = Affine3A::from_translation(piece.pivot);
            if let Some((axis, rev_s)) = piece.spin {
                // Reduce in f64 first: an hour of simulation time has no f32 precision left for an angle.
                let turns = (sim_time_s * rev_s as f64 + phase as f64).fract() as f32;
                local = local * Affine3A::from_quat(Quat::from_axis_angle(axis, turns * std::f32::consts::TAU));
            }
            let m = parent * local;
            scratch.push(m);
            piece.buffer.push(&m, color, custom, piece.radius);
        }
    }
}

/// Godot-free state of guided bodies and towed decoys: models, last headings, instance buffers.
pub(crate) struct Bodies {
    /// By `MunitionIdx`; `None` where the catalogue row has no drawable model.
    pub(crate) models: Vec<Option<BodyModel>>,
    pub(crate) towed: Option<BodyModel>,
    /// Last heading and position per drawn entity, ascending.
    headings: Vec<(EntityId, Vec3, Vec3)>,
    scratch_headings: Vec<(EntityId, Vec3, Vec3)>,
    scratch: Vec<Affine3A>,
    paint: [f32; 3],
    pub(crate) drawn: u32,
}

impl Bodies {
    pub(crate) fn new(models: Vec<Option<BodyModel>>, towed: Option<BodyModel>, paint: [f32; 3]) -> Self {
        Self { models, towed, headings: Vec::new(), scratch_headings: Vec::new(), scratch: Vec::new(), paint, drawn: 0 }
    }

    pub(crate) fn step(&mut self, view: &ViewFrame) {
        self.models.iter_mut().flatten().for_each(BodyModel::begin);
        if let Some(t) = &mut self.towed { t.begin(); }
        self.drawn = 0;
        let shift = view.origin_shift.map(Vec3::from).filter(|d| d.is_finite()).unwrap_or(Vec3::ZERO);
        self.scratch_headings.clear();
        // Bodies and decoys each ascend by entity, and entity ids are unique across both, so the two walks
        // can share one sorted memory if the results are merged afterwards.
        let previous = std::mem::take(&mut self.headings);
        let find = |entity: EntityId| previous.binary_search_by_key(&entity, |h| h.0).ok().map(|i| (previous[i].1, previous[i].2 + shift));
        for b in &view.bodies {
            let (pos, vel) = (Vec3::from(b.pos), Vec3::from(b.vel));
            if !pos.is_finite() { continue; }
            let dir = if vel.is_finite() && vel.length() >= HEADING_MIN_MPS { vel.normalize() } else { find(b.entity).map_or(Vec3::X, |h| h.0) };
            self.scratch_headings.push((b.entity, dir, pos));
            let Some(Some(model)) = self.models.get_mut(b.kind.index()) else { continue };
            let phase = (b.entity.index % 97) as f32 / 97.0;
            model.place(Affine3A::from_mat3_translation(heading_basis(dir).into(), pos), view.sim_time_s, phase, self.paint, &mut self.scratch);
            self.drawn += 1;
        }
        for d in &view.decoys {
            if d.kind != content::part::DecoyKind::TowedDecoy { continue; }
            let pos = Vec3::from(d.pos);
            if !pos.is_finite() { continue; }
            // A towed body follows its cable: it points the way it has been moving.
            let dir = match find(d.entity) {
                Some((old_dir, old_pos)) => { let step = pos - old_pos; if step.length() > 0.01 { step.normalize() } else { old_dir } }
                None => Vec3::X,
            };
            self.scratch_headings.push((d.entity, dir, pos));
            let Some(model) = &mut self.towed else { continue };
            model.place(Affine3A::from_mat3_translation(heading_basis(dir).into(), pos), view.sim_time_s, 0.0, self.paint, &mut self.scratch);
            self.drawn += 1;
        }
        self.scratch_headings.sort_unstable_by_key(|h| h.0);
        self.scratch_headings.dedup_by_key(|h| h.0);
        self.headings = std::mem::replace(&mut self.scratch_headings, previous);
    }
}

/// RIDs of the unit.
pub(crate) struct Gpu {
    tracer_shader: Option<Gd<godot::classes::Shader>>,
    part_shader: Option<Gd<godot::classes::Shader>>,
    tracer_material: Rid,
    part_material: Rid,
    ramp: Rid,
    tracer_mesh: Rid,
    tracers: MultiMeshSlot,
    /// Mesh and MultiMesh per body piece, in `Bodies::models` order then the towed decoy, pieces in order.
    meshes: Vec<Rid>,
    slots: Vec<MultiMeshSlot>,
    emission_cap: f32,
}

pub struct ProjectileRenderer {
    pub(crate) shells: Vec<LiveShell>,
    pub(crate) tier: QualityTier,
    pub(crate) stats: RenderStats,
    pub(crate) tracers: Tracers,
    pub(crate) bodies: Bodies,
    pub(crate) gpu: Option<Gpu>,
}

/// Shader parameter names of `res://shaders/vfx_opaque.gdshader` this unit sets.
mod param {
    pub const VFX_KIND: &str = "vfx_kind";
    pub const RAMP: &str = "ramp";
    pub const EMISSION_CAP: &str = "emission_cap";
}
const TRACER_SHADER: &str = "res://shaders/vfx_opaque.gdshader";

/// Every body model of the catalogue, by munition index, and the towed decoy's. A row whose generator is
/// unknown falls back to the generator of its kind; a model with no geometry is left out.
pub(crate) fn body_models(catalogue: &content::catalogue::Catalogue) -> (Vec<Option<PartModel>>, Option<PartModel>) {
    use hull_mesh::parts::{bodies, build_body_model, BodyGenerator};
    let usable = |m: PartModel| if m.pieces.iter().any(|p| !p.mesh.is_empty()) { Some(m) } else { None };
    let models = catalogue.munitions.values().map(|def| usable(build_body_model(def).unwrap_or_else(|_| bodies::build(BodyGenerator::for_kind(def.kind), def)))).collect();
    // The towed decoy has no munition row; the generator only reads the hit radius, and a non-positive one
    // takes the generator's own size.
    let towed = catalogue.munitions.values().next().and_then(|template| {
        let mut def = template.clone();
        def.radius_m = 0.0;
        usable(bodies::build(BodyGenerator::TowedDecoy, &def))
    });
    (models, towed)
}

/// The paint painted bodies wear (aircraft, drones, boats): the haze grey swatch of the default table.
fn body_paint() -> [f32; 3] {
    let table = content::blueprint::io::default_paint_table();
    content::blueprint::io::default_paint_index("hull.haze").and_then(|i| table.get(i as usize)).or(table.first()).map_or([0.27, 0.28, 0.32], |p| p.rgb)
}

impl ProjectileRenderer {
    pub fn new(scenario: godot::prelude::Rid, catalogue: &content::catalogue::Catalogue, tier: QualityTier) -> Self {
        let mut rs = gpu::rs();
        let (models, towed) = body_models(catalogue);
        let tracer_shader = gpu::load_shader(TRACER_SHADER);
        let part_shader = gpu::load_shader(super::parts::PART_SHADER);
        if tracer_shader.is_none() || part_shader.is_none() { godot_warn!("projectile renderer: a shader is missing; drawing with the default material"); }
        let tracer_material = gpu::material_on(&mut rs, &tracer_shader);
        let part_material = gpu::material_on(&mut rs, &part_shader);
        let ramp = gpu::texture_from_gen(&mut rs, &crate::seams::gen::ramp_path(VfxKind::Tracer.ramp().name()));
        if tracer_material.is_valid() {
            let kind = VfxKind::ALL.iter().position(|k| *k == VfxKind::Tracer).unwrap_or(0) as i64;
            rs.material_set_param(tracer_material, param::VFX_KIND, &kind.to_variant());
            if ramp.is_valid() { rs.material_set_param(tracer_material, param::RAMP, &ramp.to_variant()); }
            rs.material_set_param(tracer_material, param::EMISSION_CAP, &16.0f32.to_variant());
        }
        let tracer_mesh = gpu::mesh_from_data(&mut rs, &hull_mesh::vfx::meshes::base_mesh(VfxKind::Tracer), tracer_material);
        let tracers = MultiMeshSlot::new(&mut rs, scenario, tracer_mesh, LAYER_VFX, false, Rid::Invalid);
        let (mut meshes, mut slots) = (Vec::new(), Vec::new());
        for model in models.iter().flatten().chain(towed.iter()) {
            for piece in &model.pieces {
                let mesh = gpu::mesh_from_data(&mut rs, &piece.mesh, part_material);
                slots.push(MultiMeshSlot::new(&mut rs, scenario, mesh, LAYER_PARTS, true, Rid::Invalid));
                meshes.push(mesh);
            }
        }
        let bodies = Bodies::new(models.iter().map(|m| m.as_ref().map(BodyModel::from_model)).collect(), towed.as_ref().map(BodyModel::from_model), body_paint());
        let gpu = Gpu { tracer_shader, part_shader, tracer_material, part_material, ramp, tracer_mesh, tracers, meshes, slots, emission_cap: 16.0 };
        Self { shells: Vec::new(), tier, stats: RenderStats::default(), tracers: Tracers::new(tier), bodies, gpu: Some(gpu) }
    }
    /// Per frame: note shells from spawn events, place a tracer at each `ViewFrame::shells` pose, forget
    /// shells that ended, write the tracer, body and decoy buffers, and report where shells ended (from
    /// impact and splash events, render space) for the VFX unit.
    pub fn update(&mut self, ctx: &FrameContext) -> Vec<ShellEnd> {
        let mut ends = Vec::new();
        let camera = gpu::camera_render_pos(ctx);
        self.tracers.step(&mut self.shells, self.tier, ctx.view, ctx.events, camera, gpu::px_per_m_at_1m(&ctx.camera), &mut ends);
        self.bodies.step(ctx.view);
        let live = self.tracers.pool.live_count() as u32;
        self.stats = RenderStats { projectile_instances: live + self.bodies.drawn, ..RenderStats::default() };
        let Some(g) = &mut self.gpu else { return ends };
        let mut rs = gpu::rs();
        let cap = if ctx.reduce_flashing { hull_mesh::vfx::emission::REDUCE_FLASHING_CAP } else { hull_mesh::vfx::emission::FLASH.1 };
        if cap != g.emission_cap && g.tracer_material.is_valid() {
            rs.material_set_param(g.tracer_material, param::EMISSION_CAP, &cap.to_variant());
            g.emission_cap = cap;
        }
        // Tracers: the pool's buffer up to its last drawn slot, boxed by the instances' origins.
        let n = self.tracers.pool.visible_count() * super::INSTANCE_FLOATS;
        let floats = &self.tracers.floats[..n.min(self.tracers.floats.len())];
        let (mut min, mut max) = (Vec3::splat(f32::MAX), Vec3::splat(f32::MIN));
        for inst in floats.chunks_exact(super::INSTANCE_FLOATS) {
            // A hidden slot is all zero: it adds the origin to the box, which only loosens it.
            let o = Vec3::new(inst[3], inst[7], inst[11]);
            min = min.min(o);
            max = max.max(o);
        }
        g.tracers.upload(&mut rs, floats, min - TRACER_BOUND_M, max + TRACER_BOUND_M);
        let mut slot = g.slots.iter_mut();
        for model in self.bodies.models.iter().flatten().chain(self.bodies.towed.iter()) {
            for piece in &model.pieces {
                if let Some(s) = slot.next() { s.upload_buffer(&mut rs, &piece.buffer); }
            }
        }
        ends
    }
    /// Caps follow the tier: `CapGroup::Projectiles.cap(tier)`.
    pub fn set_tier(&mut self, tier: QualityTier) { self.tier = tier; }
    pub fn stats(&self) -> RenderStats { self.stats }
    /// Free every RID. Idempotent.
    pub fn free(&mut self) {
        let Some(mut g) = self.gpu.take() else { return };
        let mut rs = gpu::rs();
        g.tracers.free(&mut rs);
        g.slots.iter_mut().for_each(|s| s.free(&mut rs));
        g.meshes.iter_mut().for_each(|m| gpu::free_rid(&mut rs, m));
        for rid in [&mut g.tracer_mesh, &mut g.tracer_material, &mut g.part_material, &mut g.ramp] { gpu::free_rid(&mut rs, rid); }
        self.shells.clear();
    }
}

/// Where a shell ended this frame, for splashes and hit effects.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ShellEnd { pub pos: [f32; 3], pub calibre_mm: u16, pub water: bool, pub own_side: bool }

#[cfg(test)]
mod tests {
    use super::*;
    use crate::interp::{BodyPose, DecoyPose, RenderOrigin, ShellView, VesselPose};
    use sim_core::projectiles::ProjectileSpawn;
    use sim_systems::protocol::EntityIntro;

    fn ev(tick: u64, event: SimEvent) -> WireEvent { WireEvent { tick, event } }
    fn reveal(entity: u64, side: u8) -> WireEvent {
        ev(0, SimEvent::EntityRevealed(Box::new(EntityIntro { entity: EntityId::from_u64(entity), side: SideId(side), blueprint: Default::default(), exterior_only: false, class: None, name: String::new(), player: None, status: Default::default(), damage: Vec::new(), removed: Vec::new(), parent: None, track: Default::default() })))
    }
    fn spawn(id: u32, tick: u64, shooter: u64, calibre_mm: u16) -> WireEvent {
        ev(tick, SimEvent::Phys(PhysEvent::ProjectileSpawned(ProjectileSpawn { id: ProjectileId(id), tick, origin: DVec3::ZERO, velocity: DVec3::X * 800.0, calibre_mm, band: content::part::weapons::CalibreBand::Auto, shooter: EntityId::from_u64(shooter), mount: Default::default(), rounds: 1, point_defence: false })))
    }
    fn view(tick: f64, shells: &[(u32, f32)]) -> ViewFrame {
        let own = VesselPose { entity: EntityId::from_u64(1), own: true, rot: [0.0, 0.0, 0.0, 1.0], ..Default::default() };
        ViewFrame { render_tick: tick, sim_time_s: tick / sim_core::TICK_HZ as f64, origin: RenderOrigin::default(), vessels: vec![own], own: Some(0), shells: shells.iter().map(|&(id, x)| ShellView { id: ProjectileId(id), pos: [x, 10.0, 0.0], vel: [800.0, 0.0, 0.0] }).collect(), ..Default::default() }
    }
    fn step(t: &mut Tracers, shells: &mut Vec<LiveShell>, v: &ViewFrame, events: &[WireEvent]) -> Vec<ShellEnd> {
        let mut ends = Vec::new();
        t.step(shells, QualityTier::Medium, v, events, Vec3::new(0.0, 5.0, -50.0), 935.0, &mut ends);
        ends
    }

    #[test]
    fn a_shell_flies_coasts_and_ends_when_playback_arrives() {
        let mut t = Tracers::new(QualityTier::Medium);
        let mut shells = Vec::new();
        // Own hull 1 (side 0) and an enemy 2 (side 1) are revealed; each fires one round.
        let events = [reveal(1, 0), reveal(2, 1), spawn(7, 100, 1, 127), spawn(8, 100, 2, 30)];
        assert!(step(&mut t, &mut shells, &view(95.0, &[]), &events).is_empty());
        assert_eq!(shells.len(), 2);
        assert_eq!(t.pool.live_count(), 0, "not drawn before playback reaches the spawn");
        step(&mut t, &mut shells, &view(101.0, &[(7, 10.0), (8, 10.0)]), &[]);
        assert_eq!((shells[0].own_side, shells[1].own_side), (true, false));
        assert_eq!(shells[0].calibre_mm, 127);
        assert_eq!(t.pool.live_count(), 2);
        // The splash of shell 7 arrives for tick 108 while playback is at 102: the link has dropped the pose.
        let splash = ev(108, SimEvent::Phys(PhysEvent::Splash { projectile: ProjectileId(7), pos: DVec3::new(100.0, 0.0, 0.0), calibre_mm: 127, shooter: EntityId::from_u64(1) }));
        let ends = step(&mut t, &mut shells, &view(102.0, &[(8, 23.0)]), &[splash]);
        assert!(ends.is_empty(), "held until playback reaches the event");
        assert_eq!(shells.len(), 1);
        assert_eq!(t.pool.live_count(), 2, "the tracer coasts");
        let before = t.ending[0].pos;
        assert!(step(&mut t, &mut shells, &view(103.0, &[(8, 36.0)]), &[]).is_empty());
        let moved = t.ending[0].pos - before;
        assert!((moved.x - 800.0 / 60.0).abs() < 1e-3, "one tick at 800 m/s, got {moved}");
        // Coasting never passes the end point.
        for tick in 104..108 { step(&mut t, &mut shells, &view(tick as f64, &[(8, 40.0)]), &[]); }
        assert!(t.ending[0].pos.x <= 100.0 + 1e-3);
        let ends = step(&mut t, &mut shells, &view(108.0, &[(8, 50.0)]), &[]);
        assert_eq!(ends, vec![ShellEnd { pos: [100.0, 0.0, 0.0], calibre_mm: 127, water: true, own_side: true }]);
        assert_eq!(t.pool.live_count(), 1);
        assert!(t.ending.is_empty());
        // An impact far ahead of playback (a seek) is reported at once; its calibre comes from the spawn.
        let hit = ev(5_000, SimEvent::Phys(PhysEvent::ShellImpact { projectile: ProjectileId(8), vessel: EntityId::from_u64(1), cell: Default::default(), outcome: sim_core::armour::HitOutcome::Penetrated, world_point: DVec3::new(60.0, 4.0, 0.0), shooter: EntityId::from_u64(2), burst: false }));
        let ends = step(&mut t, &mut shells, &view(109.0, &[]), &[hit]);
        assert_eq!(ends, vec![ShellEnd { pos: [60.0, 4.0, 0.0], calibre_mm: 30, water: false, own_side: false }]);
        assert!(shells.is_empty() && t.extra.is_empty());
        assert_eq!(t.pool.live_count(), 0);
    }

    #[test]
    fn unknown_lost_and_stale_shells() {
        let mut t = Tracers::new(QualityTier::Medium);
        let mut shells = Vec::new();
        // A pose with no remembered spawn is still drawn.
        step(&mut t, &mut shells, &view(10.0, &[(3, 5.0)]), &[]);
        assert_eq!(shells, vec![LiveShell { id: ProjectileId(3), calibre_mm: DEFAULT_CALIBRE_MM, spawn_tick: 10, tracer: true, own_side: false }]);
        assert_eq!(t.pool.live_count(), 1);
        // It vanishes with no event: the tracer goes and nothing is reported.
        assert!(step(&mut t, &mut shells, &view(11.0, &[]), &[]).is_empty());
        assert!(shells.is_empty());
        assert_eq!(t.pool.live_count(), 0);
        // A spawn that never shows up is forgotten after the limit, not before.
        step(&mut t, &mut shells, &view(12.0, &[]), &[spawn(4, 12, 1, 76)]);
        step(&mut t, &mut shells, &view(12.0 + UNSEEN_MAX_TICKS, &[]), &[]);
        assert_eq!(shells.len(), 1);
        step(&mut t, &mut shells, &view(13.0 + UNSEEN_MAX_TICKS, &[]), &[]);
        assert!(shells.is_empty() && t.extra.is_empty());
        // An end for a shell nobody remembers is still reported, at the default calibre for an impact.
        let hit = ev(1, SimEvent::Phys(PhysEvent::ShellImpact { projectile: ProjectileId(99), vessel: EntityId::from_u64(1), cell: Default::default(), outcome: sim_core::armour::HitOutcome::Stopped, world_point: DVec3::new(1.0, 2.0, 3.0), shooter: EntityId::NONE, burst: false }));
        let bad = ev(1, SimEvent::Phys(PhysEvent::Splash { projectile: ProjectileId(98), pos: DVec3::NAN, calibre_mm: 76, shooter: EntityId::NONE }));
        let ends = step(&mut t, &mut shells, &view(700.0, &[]), &[hit, bad]);
        assert_eq!(ends, vec![ShellEnd { pos: [1.0, 2.0, 3.0], calibre_mm: DEFAULT_CALIBRE_MM, water: false, own_side: false }]);
        // A new sortie clears everything.
        step(&mut t, &mut shells, &view(701.0, &[(5, 1.0)]), &[]);
        step(&mut t, &mut shells, &view(0.0, &[]), &[ev(0, SimEvent::SortieStarted { seed: 1, tick_hz: 60 })]);
        assert!(shells.is_empty() && t.pool.live_count() == 0);
    }

    #[test]
    fn tracer_buffer_and_rebase() {
        let mut t = Tracers::new(QualityTier::Low);
        let mut shells = Vec::new();
        step(&mut t, &mut shells, &view(1.0, &[(1, 100.0)]), &[]);
        // A fresh slot may be hidden for its first frame; by the second it is in the buffer.
        step(&mut t, &mut shells, &view(2.0, &[(1, 113.0)]), &[]);
        let n = t.pool.visible_count();
        assert_eq!(n, 1);
        let inst = &t.floats[..crate::render::INSTANCE_FLOATS];
        // Centred half a length (800 / 40 / 2 = 10 m) behind the shell.
        assert!((inst[3] - 103.0).abs() < 1e-3 && (inst[7] - 10.0).abs() < 1e-3, "{inst:?}");
        let mut v = view(3.0, &[(1, 126.0 - 1000.0)]);
        v.origin_shift = Some([-1000.0, 0.0, 0.0]);
        step(&mut t, &mut shells, &v, &[]);
        assert!((t.floats[3] - (116.0 - 1000.0)).abs() < 1e-2);
        assert_eq!(t.pool.cap, CapGroup::Projectiles.cap(QualityTier::Low));
    }

    #[test]
    fn bodies_point_along_their_velocity() {
        let b = heading_basis(Vec3::new(0.0, 0.0, -3.0));
        assert!((Vec3::from(b.x_axis) - Vec3::NEG_Z).length() < 1e-6);
        assert!((Vec3::from(b.y_axis) - Vec3::Y).length() < 1e-6);
        assert!((glam::Mat3::from(b).determinant() - 1.0).abs() < 1e-5);
        for dir in [Vec3::Y, Vec3::NEG_Y, Vec3::ZERO, Vec3::new(f32::NAN, 0.0, 0.0), Vec3::new(1.0, 5.0, 2.0)] {
            let m = glam::Mat3::from(heading_basis(dir));
            assert!(m.is_finite() && (m.determinant() - 1.0).abs() < 1e-4, "{dir}");
        }

        let (models, towed) = body_models(content::catalogue::catalogue());
        assert!(!models.is_empty() && models.iter().all(Option::is_some), "every munition draws");
        assert!(towed.is_some());
        let mut bodies = Bodies::new(models.iter().map(|m| m.as_ref().map(BodyModel::from_model)).collect(), towed.as_ref().map(BodyModel::from_model), [0.3; 3]);
        let body = |vel: [f32; 3]| BodyPose { entity: EntityId::from_u64(40), kind: content::ids::MunitionIdx(0), pos: [10.0, 5.0, 0.0], vel, seeker: Default::default(), track: Default::default(), own_side: true };
        let decoy = |x: f32| DecoyPose { entity: EntityId::from_u64(50), kind: content::part::DecoyKind::TowedDecoy, pos: [x, 0.0, 0.0], age_s: 1.0 };
        let chaff = DecoyPose { entity: EntityId::from_u64(51), kind: content::part::DecoyKind::Chaff, pos: [0.0; 3], age_s: 1.0 };
        let mut v = ViewFrame { bodies: vec![body([0.0, 0.0, 250.0])], decoys: vec![decoy(0.0), chaff], ..Default::default() };
        bodies.step(&v);
        assert_eq!(bodies.drawn, 2, "the missile and the towed decoy; chaff is an effect");
        let pieces = &bodies.models[0].as_ref().unwrap().pieces;
        assert!(pieces.iter().all(|p| p.buffer.count() == 1));
        let root = &pieces[0].buffer.data;
        // Nose (+X) along +Z: basis.x = (0, 0, 1), so row 2 column 0 is 1.
        assert!((root[8] - 1.0).abs() < 1e-5 && root[0].abs() < 1e-5, "{:?}", &root[..12]);
        // Stopped: the heading is kept. The towed decoy turns to follow its motion.
        v.bodies[0] = body([0.0, 0.0, 0.0]);
        v.decoys[0] = decoy(-3.0);
        bodies.step(&v);
        let root = &bodies.models[0].as_ref().unwrap().pieces[0].buffer.data;
        assert!((root[8] - 1.0).abs() < 1e-5);
        let tow = &bodies.towed.as_ref().unwrap().pieces[0].buffer.data;
        assert!((tow[0] + 1.0).abs() < 1e-5, "towed decoy points along -X, {:?}", &tow[..12]);
        // Nothing drawn: buffers empty, memory cleared.
        bodies.step(&ViewFrame::default());
        assert_eq!(bodies.drawn, 0);
        assert!(bodies.headings.is_empty());
        // A munition index past the catalogue is skipped, not a panic.
        v.bodies[0].kind = content::ids::MunitionIdx(u16::MAX);
        bodies.step(&v);
        assert_eq!(bodies.drawn, 1);
    }
}
