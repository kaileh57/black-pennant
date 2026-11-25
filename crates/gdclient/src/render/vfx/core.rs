//! Part of unit `gd-render-vfx`. Every decision of VFX layer A with no Godot in it: which effects exist,
//! where they stand this frame, and what each kind's buffer holds. The engine side (`gpu.rs`) only uploads
//! [`VfxCore::floats`] and places the lights in [`VfxCore::lights`]. Tests drive this headless.
//!
//! # Timing
//! Effects are drawn at the playback tick, like everything else: an event is held until
//! `ViewFrame::render_tick` reaches its tick (or applied at once when it is more than
//! [`EVENT_HOLD_MAX_TICKS`] ahead, as after a seek). Shell ends arrive already timed by
//! `gd-render-projectiles`. All ages run on `ViewFrame::sim_time_s`, never on wall time, so a paused game
//! holds every effect still and a replay draws the same puffs.
//!
//! # What drives what
//! - One-shots from events: muzzle effects (`MountFired`, `BodySpawned`), explosions (`BodyImpact`,
//!   `BodyBurst`, `BodyDestroyed`, `CookOff`), shed debris (`Debris`), and splashes and hits from
//!   [`ShellEnd`]s.
//! - Attached effects from state, reconciled every frame: wakes, Kelvin arms and bow waves from
//!   `ViewFrame::vessels`; missile trails from `ViewFrame::bodies`; chaff, flares and defence smoke from
//!   `ViewFrame::decoys`; fires from `FireStarted` / `FireOut` and the `BURNING` flag (so a hull revealed
//!   already burning, or one whose design carries no compartments, still burns); flooding cues from
//!   `Breach`, `CompartmentHalfFlooded`, the own ship's compartment states and the hull's sinkage.
//! - Muzzle positions come from the part anchors when the view hands them over
//!   ([`VfxCore::anchors`]); otherwise from the mount's placement in the design and its replicated train
//!   and elevation, so a muzzle flash never depends on another unit having run.
// Client rendering, not simulation (clippy.toml): std floats are allowed here. Nothing drawn from them feeds
// back into the sim, and a last-bit difference between platforms moves nothing on screen.
#![allow(clippy::disallowed_methods)]
use super::super::hull::design::DrawDesign;
use super::super::hull::gpu::pose_affine;
use super::super::parts::AnchorPose;
use super::super::projectiles::ShellEnd;
use crate::enums::QualityTier;
use crate::interp::ViewFrame;
use crate::seams::WireEvent;
use content::ids::{ContentHash, PartSlot, SideId};
use content::part::DecoyKind;
use glam::{Affine3A, Vec3};
use hull_mesh::parts::AnchorKind;
use hull_mesh::vfx::runtime::{self as rt, FlashLimiter, Protection, RingPool, VfxHandle, VfxLight};
use hull_mesh::vfx::{CapGroup, VfxInstance, VfxKind};
use sim_core::events::PhysEvent;
use sim_core::EntityId;
use sim_systems::protocol::{SimEvent, VesselFlags};
use std::collections::BTreeMap;
use std::sync::Arc;
use super::wake::{WakeField, WakeHull, WakeTrack};

/// Kinds this unit draws: `VfxKind::ALL` without `Tracer` (`gd-render-projectiles` draws tracers).
pub(crate) const KINDS: usize = VfxKind::ALL.len() - 1;
/// An event more than this many ticks ahead of playback is applied at once (seek, stalled clock).
pub const EVENT_HOLD_MAX_TICKS: f64 = 0.5 * sim_core::TICK_HZ as f64;
/// Below this speed a ship lays no wake and pushes no bow wave.
pub const WAKE_MIN_KN: f32 = 0.5;
/// Beyond this distance from the camera fire smoke is halved from degrade step 2 (L2028).
pub const SMOKE_HALVED_BEYOND_M: f32 = 3_000.0;
/// Beyond this distance a splash is one prism from degrade step 3 (L2029).
pub const SPLASH_ONE_PRISM_BEYOND_M: f32 = 8_000.0;
/// Degrade steps (L2026-2031) this unit applies.
pub const DEGRADE_DEBRIS: u8 = 2;
pub const DEGRADE_TRAILS_KELVIN_SPLASH: u8 = 3;
pub const DEGRADE_LIGHTS_OWN_GUNS: u8 = 4;
/// Debris per event once degraded (L2028: halved, then 16 per event).
pub const DEGRADED_DEBRIS_PER_EVENT: u32 = 16;
/// Freeboard lost at which a foam band shows on a hull no flooding event has named: the sinkage itself is
/// the cue (a list or settling hull the viewer was never told about). It goes again below the lower value.
pub const FOAM_BAND_MIN_LOSS: f32 = 0.15;
pub const FOAM_BAND_OFF_LOSS: f32 = 0.08;
/// Time constant of the smoothed (signed) freeboard loss, seconds: waves heave and roll a hull every few
/// seconds, and the band must follow the settling, not the swell.
pub const FREEBOARD_SMOOTH_S: f32 = 15.0;
/// A hull whose design does not say where its waterline is (an exterior-only form) learns it from its poses
/// over this long after it is first drawn, smoothed, then keeps it.
pub const BASELINE_LEARN_S: f32 = 20.0;
/// Fire light flicker rate and depth (a deterministic function of simulation time).
pub const FIRE_FLICKER_HZ: f32 = 7.3;
/// Explosion radius per cook-off cell, metres (cells are 0.5 m; the blast reads larger than the cells).
pub const COOKOFF_M_PER_CELL: f32 = 1.5;
/// Warhead explosion radius for missile and torpedo hits and bursts, metres.
pub const WARHEAD_RADIUS_M: f32 = 4.0;
/// Fireball radius of a body shot down in the air, metres.
pub const SHOT_DOWN_RADIUS_M: f32 = 2.5;
/// Splash calibre equivalent of an underwater burst (a tall column, the 40 m cap).
pub const UNDERWATER_BURST_MM: u16 = 400;
/// Calibre assumed for a mount whose part says none (a launcher), mm.
pub const DEFAULT_MUZZLE_MM: u16 = 40;
/// Calibre equivalent of a missile or drone launch's smoke, mm.
pub const LAUNCH_SMOKE_MM: u16 = 120;
/// A shell end in the air more than this far from every drawn hull, and this high, is an air burst.
pub const AIR_BURST_MIN_Y_M: f32 = 3.0;
/// Pump discharge drop and flow for pumps whose state the viewer does not see.
pub const PUMP_FLOW_UNKNOWN: f32 = 0.7;

/// Index of a kind's pool (and buffer), or `None` for `Tracer`.
pub(crate) fn pool_index(kind: VfxKind) -> Option<usize> {
    VfxKind::ALL.iter().position(|k| *k == kind).and_then(|i| i.checked_sub(1))
}
/// The kind of pool `i`.
pub(crate) fn pool_kind(i: usize) -> VfxKind { VfxKind::ALL[i + 1] }
/// The shader's `vfx_kind`: the index in `VfxKind::ALL` (the include's `K_*` constants).
pub(crate) fn shader_kind(kind: VfxKind) -> i64 { VfxKind::ALL.iter().position(|k| *k == kind).unwrap_or(0) as i64 }

/// Wind in the render frame (m/s) from replicated weather: the direction it blows toward, compass radians
/// (0 = north = -Z, clockwise).
pub(crate) fn wind_vector(w: &content::weather::WeatherState) -> Vec3 {
    let (d, s) = (w.wind_dir_rad as f32, w.wind_speed_mps as f32);
    if !(d.is_finite() && s.is_finite()) { return Vec3::ZERO; }
    Vec3::new(d.sin(), 0.0, -d.cos()) * s.clamp(0.0, 80.0)
}

/// Compass heading (radians, 0 = -Z, clockwise) of a vessel-local +X under `pose`.
pub(crate) fn heading_of(pose: &Affine3A) -> f32 {
    let f = Vec3::from(pose.matrix3.x_axis);
    if f.x.abs() + f.z.abs() < 1e-6 { return 0.0; }
    f.x.atan2(-f.z)
}

fn seed_of(a: u64, b: u64) -> u32 { rt::effect_seed((a ^ (a >> 32)) as u32, (b ^ (b >> 32)) as u32) }
fn fin3(v: Vec3) -> Option<Vec3> { if v.is_finite() { Some(v) } else { None } }
fn kn(mps: f32) -> f32 { mps * 1.943_844_5 }

/// One VFX light this frame (the engine side places a pooled OmniLight on each).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct LightOut { pub pos: Vec3, pub range_m: f32, pub energy: f32, pub fire: bool }

/// A muzzle or flash light while it lasts.
#[derive(Clone, Copy, Debug)]
struct TimedLight { light: VfxLight, born_s: f64, energy: f32, own_gun: bool }

/// The geometry of one design the effects need, computed once per design hash.
#[derive(Clone, Debug, Default)]
pub(crate) struct FxDesign {
    /// Grid extent in metres (length along +X, height, beam along +Z).
    size: Vec3,
    /// Stern and stem on the design waterline row, vessel-local x in metres: where the wake leaves the hull
    /// and where the bow wave stands. A raked bow or a counter stern overhangs the grid box's ends.
    waterline_ends_m: Option<(f32, f32)>,
    waterline_m: Option<f32>,
    deck_m: Option<f32>,
    /// Per compartment: top-of-hull point above its centroid (vessel-local), flame width, lowest cell
    /// height, and the outboard waterline point on the nearer side with its outward direction.
    compartments: BTreeMap<u16, CompartmentGeo>,
    /// Mounts: slot, placement top centre (vessel-local), the placement's rotation, calibre, and the
    /// placement with the model's joint chain to its muzzles.
    mounts: Vec<Mount>,
    /// Pump outlets: slot and the outboard point at the deck edge on the part's side, with its outward.
    pumps: Vec<(PartSlot, Vec3, Vec3)>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct CompartmentGeo { top: Vec3, width_m: f32, bottom_m: f32, side: Vec3, outward: Vec3 }

impl FxDesign {
    pub(crate) fn from_design(d: &DrawDesign) -> Self {
        let cell = hull_mesh::CELL_M_F32;
        let size = Vec3::from(d.size_m());
        let grid = &d.grid;
        let [sx, sy, sz] = grid.size.map(|s| s as usize);
        let n = sx * sy * sz;
        let ok_grid = grid.cells.len() == n && n > 0;
        let solid = |x: usize, y: usize, z: usize| ok_grid && x < sx && y < sy && z < sz && !grid.cells[x + sx * (z + sz * y)].is_empty();
        let mut compartments = BTreeMap::new();
        for (id, cells) in &d.compartments {
            if cells.is_empty() || !ok_grid { continue; }
            let (mut cx, mut cz, mut ymin, mut ymax, mut count) = (0.0f64, 0.0f64, usize::MAX, 0usize, 0usize);
            for &i in cells {
                let i = i as usize;
                if i >= n { continue; }
                let (x, y, z) = (i % sx, i / (sx * sz), (i / sx) % sz);
                cx += x as f64;
                cz += z as f64;
                ymin = ymin.min(y);
                ymax = ymax.max(y);
                count += 1;
            }
            if count == 0 { continue; }
            let (x, z) = ((cx / count as f64).round() as usize, (cz / count as f64).round() as usize);
            // Up through the hull above the compartment to the first open cell: where its fire breaks out.
            let mut y = ymax + 1;
            while y < sy && solid(x, y, z) { y += 1; }
            let top = Vec3::new((x as f32 + 0.5) * cell, y as f32 * cell, (z as f32 + 0.5) * cell);
            let footprint = count as f32 / (ymax + 1 - ymin.min(ymax)) as f32;
            let width_m = (footprint.sqrt() * cell).clamp(1.0, 8.0);
            // Outboard on the nearer side at the compartment's station.
            let starboard = 2 * z >= sz;
            let mut zz = z;
            if starboard { while zz + 1 < sz && (0..sy).any(|yy| solid(x, yy, zz + 1)) { zz += 1; } } else { while zz > 0 && (0..sy).any(|yy| solid(x, yy, zz - 1)) { zz -= 1; } }
            let side_z = if starboard { (zz + 1) as f32 * cell } else { zz as f32 * cell };
            let outward = if starboard { Vec3::Z } else { Vec3::NEG_Z };
            compartments.insert(*id, CompartmentGeo { top, width_m, bottom_m: ymin as f32 * cell, side: Vec3::new(top.x, 0.0, side_z), outward });
        }
        let cat = content::catalogue::catalogue();
        let mut mounts = Vec::new();
        let mut pumps = Vec::new();
        for (slot, p) in &d.parts {
            let Some(def) = cat.part(&p.part) else { continue };
            let local = super::super::parts::placement_affine(p.pos, p.rot, def.size);
            let s = Vec3::new(def.size[0] as f32, def.size[1] as f32, def.size[2] as f32) * cell;
            let top = local.transform_point3(Vec3::new(0.5 * s.x, s.y, 0.5 * s.z));
            match &def.function {
                content::part::PartFunction::Gun(g) => mounts.push((*slot, top, glam::Mat3::from(local.matrix3), g.calibre_mm, local, MuzzleRig::of(&p.part, def))),
                content::part::PartFunction::Pump { .. } => {
                    let centre = local.transform_point3(0.5 * s);
                    let starboard = centre.z * 2.0 >= size.z;
                    let deck = d.deck_m.unwrap_or(size.y * 0.6);
                    pumps.push((*slot, Vec3::new(centre.x, deck.min(size.y), if starboard { size.z } else { 0.0 }), if starboard { Vec3::Z } else { Vec3::NEG_Z }));
                }
                _ => {}
            }
        }
        let waterline_ends_m = d.waterline_m.filter(|_| ok_grid && sy > 0).and_then(|w| {
            let row = ((w / cell).floor().max(0.0) as usize).min(sy - 1);
            let wet: Vec<usize> = (0..sx).filter(|&x| (0..sz).any(|z| solid(x, row, z))).collect();
            Some((*wet.first()? as f32 * cell, (*wet.last()? + 1) as f32 * cell))
        });
        Self { size, waterline_ends_m, waterline_m: d.waterline_m, deck_m: d.deck_m, compartments, mounts, pumps }
    }
}

/// A gun mount of a design: slot, placement top centre (vessel-local), placement rotation, calibre, the
/// placement itself and the model's muzzle rig.
type Mount = (PartSlot, Vec3, glam::Mat3, u16, Affine3A, Option<Arc<MuzzleRig>>);

/// The joint chain of a gun model down to its muzzle anchors: what `gd-render-parts` poses, without the
/// meshes. Built once per part type.
#[derive(Clone, Debug, Default)]
pub(crate) struct MuzzleRig { pieces: Vec<(Option<u8>, Vec3, hull_mesh::parts::Joint)>, anchors: Vec<(u8, Vec3, Vec3)> }
impl MuzzleRig {
    fn of(id: &content::ids::PartId, def: &content::part::PartDef) -> Option<Arc<MuzzleRig>> {
        thread_local! { static RIGS: std::cell::RefCell<BTreeMap<content::ids::PartId, Option<Arc<MuzzleRig>>>> = const { std::cell::RefCell::new(BTreeMap::new()) }; }
        if let Some(found) = RIGS.with(|r| r.borrow().get(id).cloned()) { return found; }
        let rig = hull_mesh::parts::build_part_model(def).ok().map(|m| {
            let pieces = m.pieces.iter().enumerate().map(|(i, p)| (p.parent.filter(|q| (*q as usize) < i), Vec3::from(p.pivot), p.joint)).collect();
            let anchors = m.anchors.iter().filter(|a| a.kind == AnchorKind::Muzzle && (a.piece as usize) < m.pieces.len()).map(|a| (a.piece, Vec3::from(a.pos), Vec3::from(a.dir))).collect::<Vec<_>>();
            Arc::new(MuzzleRig { pieces, anchors })
        }).filter(|r| !r.anchors.is_empty());
        RIGS.with(|r| r.borrow_mut().insert(id.clone(), rig.clone()));
        rig
    }
    /// Muzzle positions and bores for a mount placed at `base` (vessel pose times placement) trained and
    /// elevated as replicated. Recoil and spin do not move a muzzle enough to matter.
    fn muzzles(&self, base: Affine3A, train: f32, elev: f32) -> Vec<(Vec3, Vec3)> {
        use hull_mesh::parts::Joint;
        let mut xf: Vec<Affine3A> = Vec::with_capacity(self.pieces.len());
        for (parent, pivot, joint) in &self.pieces {
            let p = parent.and_then(|q| xf.get(q as usize).copied()).unwrap_or(base);
            let seat = Affine3A::from_translation(*pivot);
            let local = match joint { Joint::Train => seat * Affine3A::from_rotation_y(train), Joint::Elevate => seat * Affine3A::from_rotation_z(elev), _ => seat };
            xf.push(p * local);
        }
        self.anchors.iter().filter_map(|(piece, pos, dir)| {
            let m = xf.get(*piece as usize)?;
            let (p, d) = (m.transform_point3(*pos), m.transform_vector3(*dir).normalize_or(Vec3::X));
            (p.is_finite() && d.is_finite()).then_some((p, d))
        }).collect()
    }
}

/// One fire the unit draws.
#[derive(Clone, Copy, Debug)]
struct FireFx { id: u32, local: Vec3, width_m: f32, emitted_flames: u32, emitted_smoke: u32 }

/// One flooding compartment: its boil ring while the hole is below the waterline.
#[derive(Clone, Copy, Debug)]
struct FloodFx { breach_below: bool, boil: Option<VfxHandle> }

/// What the unit keeps per drawn vessel.
#[derive(Clone, Debug)]
struct VesselFx {
    hash: ContentHash,
    side: Option<SideId>,
    design: Option<Arc<FxDesign>>,
    exterior: bool,
    /// Fires by compartment (`u32::MAX` for the presence fire of a hull with no known compartment).
    fires: BTreeMap<u32, FireFx>,
    flooding: BTreeMap<u16, FloodFx>,
    /// The last place the hull was struck, vessel-local: where a fire of unknown compartment burns.
    last_hit_local: Option<Vec3>,
    /// The wake ribbon and Kelvin arms, as the stern's track: the sea's material draws them (`wake.rs`).
    track: WakeTrack,
    bow: Option<VfxHandle>,
    bands: [Option<VfxHandle>; 2],
    pumps: Vec<(PartSlot, VfxHandle)>,
    /// The waterline the hull floated at when first drawn (vessel-local), the baseline of its sinkage.
    baseline_wl: Option<f32>,
    /// Seconds of poses the baseline has learned from (only while the design has no waterline).
    baseline_s: f32,
    loss: [f32; 2],
    band_on: [bool; 2],
    seen: bool,
}
impl VesselFx {
    fn new(hash: ContentHash, side: Option<SideId>, exterior: bool) -> Self {
        Self { hash, side, design: None, exterior, fires: BTreeMap::new(), flooding: BTreeMap::new(), last_hit_local: None, track: WakeTrack::default(), bow: None, bands: [None; 2], pumps: Vec::new(), baseline_wl: None, baseline_s: 0.0, loss: [0.0; 2], band_on: [false; 2], seen: false }
    }
    fn handles(&mut self) -> impl Iterator<Item = &mut Option<VfxHandle>> + '_ {
        let floods = self.flooding.values_mut().map(|f| &mut f.boil);
        std::iter::once(&mut self.bow).chain(self.bands.iter_mut()).chain(floods)
    }
}

/// A missile, drone or torpedo and the end of its trail.
#[derive(Clone, Copy, Debug)]
struct TrailFx { last: Vec3, last_s: f64, incoming: bool, seen: bool }

/// A decoy's effect instances.
#[derive(Clone, Debug)]
struct DecoyFx { kind: DecoyKind, handles: Vec<VfxHandle>, seed: u32, seen: bool }

/// Inputs of one frame besides the view.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Lens {
    pub camera: Vec3,
    /// Unit view direction (render space).
    pub forward: Vec3,
    /// Seconds of wall time since the unit started, from `FrameContext::delta_s`: the flash limit counts
    /// flashes per second the viewer sees, so time compression must not loosen it (L1957).
    pub real_s: f64,
    /// `gpu::px_per_m_at_1m`; 0 when the camera is unknown (flashes then count as small).
    pub px_per_m: f32,
    pub viewport_px: [f32; 2],
    pub tier: QualityTier,
    pub degrade_step: u8,
    pub reduce_flashing: bool,
}

pub(crate) struct VfxCore {
    pub(crate) tier: QualityTier,
    pub(crate) pools: Vec<RingPool>,
    /// Per pool: this frame's buffer, its visible instance count and the box around what it draws.
    pub(crate) floats: Vec<Vec<f32>>,
    pub(crate) bounds: Vec<(Vec3, Vec3)>,
    /// Muzzle and pump-outlet anchors handed over by the view this frame (`VfxRenderer::set_anchors`).
    pub(crate) anchors: Vec<AnchorPose>,
    pub(crate) lights: Vec<LightOut>,
    /// How many OmniLights the engine side holds (`vfx_light_budget(tier)`), total and for fires.
    pub(crate) light_budget: (usize, usize),
    vessels: BTreeMap<EntityId, VesselFx>,
    designs: BTreeMap<ContentHash, Arc<FxDesign>>,
    sides: BTreeMap<EntityId, SideId>,
    trails: BTreeMap<EntityId, TrailFx>,
    decoys: BTreeMap<EntityId, DecoyFx>,
    timed: Vec<TimedLight>,
    queue: Vec<WireEvent>,
    wind: Vec3,
    last_s: Option<f64>,
    last_tick: Option<f64>,
    /// Draw one instance of every kind on the next frame (pipeline warm-up, L1358).
    pub(crate) warm: bool,
    /// Wall-clock seconds since the unit started (`FrameContext::delta_s` summed), for the flash limit.
    pub(crate) real_s: f64,
    warm_handles: Vec<VfxHandle>,
    counter: u64,
    /// This frame's wakes for the sea's material, nearest the camera first.
    pub(crate) wakes: WakeField,
}

impl VfxCore {
    pub(crate) fn new(tier: QualityTier) -> Self {
        let pools = (0..KINDS).map(|i| RingPool::new(pool_kind(i), tier)).collect();
        Self {
            tier, pools, floats: vec![Vec::new(); KINDS], bounds: vec![(Vec3::ZERO, Vec3::ZERO); KINDS], anchors: Vec::new(), lights: Vec::new(),
            light_budget: hull_mesh::vfx::vfx_light_budget(tier), vessels: BTreeMap::new(), designs: BTreeMap::new(), sides: BTreeMap::new(),
            trails: BTreeMap::new(), decoys: BTreeMap::new(), timed: Vec::new(), queue: Vec::new(), wind: Vec3::ZERO, last_s: None, last_tick: None,
            warm: false, real_s: 0.0, warm_handles: Vec::new(), counter: 0, wakes: WakeField::default(),
        }
    }

    pub(crate) fn pool(&mut self, kind: VfxKind) -> Option<&mut RingPool> { pool_index(kind).map(|i| &mut self.pools[i]) }
    /// Simulation time of the last frame.
    pub(crate) fn now_s(&self) -> f64 { self.last_s.unwrap_or(0.0) }
    pub(crate) fn live(&self) -> u32 { self.pools.iter().map(|p| p.live_count() as u32).sum() }

    fn spawn(&mut self, now: f64, kind: VfxKind, life: f32, prot: Protection, inst: VfxInstance) -> Option<VfxHandle> {
        self.pool(kind)?.spawn(now, life, prot, inst)
    }

    /// Forget everything (a new sortie).
    pub(crate) fn reset(&mut self) {
        self.reset_effects();
        self.vessels.clear();
        self.sides.clear();
    }

    /// Drop every effect and pending event but keep what the reveals said (a rewound timeline: the hulls
    /// are still the same hulls, and their reveals will not be sent again).
    pub(crate) fn reset_effects(&mut self) {
        let tier = self.tier;
        self.pools = (0..KINDS).map(|i| RingPool::new(pool_kind(i), tier)).collect();
        for v in self.vessels.values_mut() {
            let (hash, side, exterior, design, baseline) = (v.hash, v.side, v.exterior, v.design.clone(), v.baseline_wl);
            *v = VesselFx::new(hash, side, exterior);
            v.design = design;
            v.baseline_wl = baseline;
        }
        self.warm_handles.clear();
        self.trails.clear();
        self.decoys.clear();
        self.timed.clear();
        self.queue.clear();
        self.last_s = None;
        self.last_tick = None;
    }

    /// Rebuild the pools for another tier, keeping protected instances and following every stored handle.
    pub(crate) fn set_tier(&mut self, tier: QualityTier, now_s: f64) {
        if tier == self.tier { return; }
        self.tier = tier;
        self.light_budget = hull_mesh::vfx::vfx_light_budget(tier);
        let mut moved: BTreeMap<(VfxKind, u32, u32), VfxHandle> = BTreeMap::new();
        for p in &mut self.pools {
            for (old, new) in p.retier(now_s, tier) { moved.insert((old.kind, old.slot, old.generation), new); }
        }
        if moved.is_empty() { return; }
        let follow = |h: &mut VfxHandle| if let Some(n) = moved.get(&(h.kind, h.slot, h.generation)) { *h = *n; };
        for v in self.vessels.values_mut() {
            for h in v.handles().flatten() { follow(h); }
            for (_, h) in &mut v.pumps { follow(h); }
        }
        for d in self.decoys.values_mut() { d.handles.iter_mut().for_each(follow); }
    }

    fn next_seed(&mut self, tick: u64) -> u32 { self.counter = self.counter.wrapping_add(1); seed_of(tick, self.counter) }

    /// One frame. `lookup` resolves a design hash (`design::lookup` in the game).
    pub(crate) fn step(&mut self, view: &ViewFrame, events: &[WireEvent], shell_ends: &[ShellEnd], lens: Lens, flashes: &mut FlashLimiter, lookup: &dyn Fn(&ContentHash) -> Option<Arc<DrawDesign>>) {
        let now = view.sim_time_s;
        let tick = view.render_tick;
        flashes.reduce_flashing = lens.reduce_flashing;
        if lens.tier != self.tier { self.set_tier(lens.tier, now); }
        let rewound = self.last_s.is_some_and(|l| now + 1e-6 < l) && !events.iter().any(|e| matches!(e.event, SimEvent::SortieStarted { .. }));
        if rewound { self.reset_effects(); }
        let t0 = self.last_s.unwrap_or(now);
        self.last_s = Some(now);
        self.last_tick = Some(tick);
        if let Some(shift) = view.origin_shift.map(Vec3::from).and_then(fin3) { self.rebase(shift); }

        // 1. State events now; one-shots when playback reaches them.
        for ev in events {
            match &ev.event {
                SimEvent::SortieStarted { .. } => self.reset(),
                SimEvent::EntityRevealed(intro) => {
                    self.sides.insert(intro.entity, intro.side);
                    let v = VesselFx::new(intro.blueprint, Some(intro.side), intro.exterior_only);
                    if let Some(old) = self.vessels.insert(intro.entity, v) { self.drop_vessel(old); }
                }
                SimEvent::Phys(PhysEvent::VesselSpawned { vessel, side }) => { self.sides.insert(*vessel, *side); }
                SimEvent::Phys(PhysEvent::WeatherChanged { weather, .. }) => self.wind = wind_vector(weather),
                SimEvent::EntityHidden { entity } => self.forget(*entity),
                _ if Self::deferred(&ev.event) => self.queue.push(ev.clone()),
                _ => {}
            }
        }
        let mut due = Vec::new();
        self.queue.retain(|e| {
            let ahead = e.tick as f64 - tick;
            if ahead <= 0.0 || ahead > EVENT_HOLD_MAX_TICKS { due.push(e.clone()); false } else { true }
        });
        for ev in &due { self.apply(ev, view, lens, flashes, now); }

        // 2. Shell ends: splashes and hits.
        for end in shell_ends { self.shell_end(end, view, lens, flashes, now); }

        // 3. Attached and continuous effects.
        self.vessels_step(view, lens, lookup, t0, now);
        self.trails_step(view, now);
        self.decoys_step(view, now);
        if lens.degrade_step >= DEGRADE_TRAILS_KELVIN_SPLASH {
            if let Some(p) = self.pool(VfxKind::MissileTrail) { p.trim_older_than(now, rt::TRAIL_HEAD_S); }
        }
        for h in std::mem::take(&mut self.warm_handles) { if let Some(p) = self.pool(h.kind) { p.retire(h); } }
        if self.warm { self.warm_up(lens.camera, lens.forward, now); }

        // 4. Age, write, light.
        for i in 0..KINDS {
            let pool = &mut self.pools[i];
            // Nothing alive and nothing drawn last frame: the buffer is already empty.
            if pool.visible_count() == 0 && pool.live_count() == 0 { self.floats[i].clear(); self.bounds[i] = (Vec3::ZERO, Vec3::ZERO); continue; }
            pool.tick(now);
            pool.write_buffer(lens.camera.into(), &mut self.floats[i]);
            self.bounds[i] = instance_bounds(pool.kind, &self.floats[i][..pool.visible_count() * rt_floats()]);
        }
        self.lights_step(view, lens, now);
        self.wakes = WakeField::pack(self.vessels.values().map(|v| &v.track), lens.camera, now, lens.degrade_step < DEGRADE_TRAILS_KELVIN_SPLASH);
    }

    fn deferred(e: &SimEvent) -> bool {
        matches!(e, SimEvent::Phys(
            PhysEvent::MountFired { .. } | PhysEvent::BodySpawned { .. } | PhysEvent::BodyImpact { .. } | PhysEvent::BodyBurst { .. }
            | PhysEvent::BodyDestroyed { .. } | PhysEvent::CookOff { .. } | PhysEvent::Debris { .. } | PhysEvent::FireStarted { .. }
            | PhysEvent::FireOut { .. } | PhysEvent::Breach { .. } | PhysEvent::CompartmentHalfFlooded { .. } | PhysEvent::DecoyExpired { .. }
            | PhysEvent::VesselDespawned { .. } | PhysEvent::WreckDespawned { .. } | PhysEvent::ShellImpact { .. } | PhysEvent::StatusChanged { .. }
        ))
    }

    fn rebase(&mut self, d: Vec3) {
        for p in &mut self.pools { p.rebase(d.into()); }
        for v in self.vessels.values_mut() { v.track.rebase(d); }
        for t in self.trails.values_mut() { t.last += d; }
        for l in &mut self.timed { l.light.pos = (Vec3::from(l.light.pos) + d).into(); }
    }

    fn drop_vessel(&mut self, mut v: VesselFx) {
        let mut hs: Vec<VfxHandle> = v.handles().filter_map(|h| h.take()).collect();
        hs.extend(v.pumps.iter().map(|p| p.1));
        for h in hs { if let Some(p) = self.pool(h.kind) { p.retire(h); } }
    }

    fn forget(&mut self, e: EntityId) {
        if let Some(v) = self.vessels.remove(&e) { self.drop_vessel(v); }
        self.trails.remove(&e);
        if let Some(d) = self.decoys.remove(&e) { for h in d.handles { if let Some(p) = self.pool(h.kind) { p.retire(h); } } }
    }

    fn own_side(&self, view: &ViewFrame) -> Option<SideId> {
        let own = view.vessels.get(view.own? as usize)?;
        self.sides.get(&own.entity).copied()
    }

    fn pose_of(view: &ViewFrame, e: EntityId) -> Option<(Affine3A, &crate::interp::VesselPose)> {
        let i = view.vessels.binary_search_by_key(&e, |v| v.entity).ok().or_else(|| view.vessels.iter().position(|v| v.entity == e))?;
        let p = &view.vessels[i];
        Some((pose_affine(p.pos, p.rot), p))
    }

    /// Spawn a list of (kind, instance, lifetime), with flashes through the limiter. Returns whether a full
    /// flash was admitted (it may light).
    fn spawn_layout(&mut self, items: Vec<(VfxKind, VfxInstance, f32)>, lens: Lens, flashes: &mut FlashLimiter, now: f64, prot_debris: Protection, debris_limit: Option<u32>) -> bool {
        let mut lit = false;
        let mut debris = 0u32;
        for (kind, mut inst, life) in items {
            if kind == VfxKind::Debris {
                debris += 1;
                if debris_limit.is_some_and(|m| debris > m) { continue; }
            }
            if matches!(kind, VfxKind::HitFlash | VfxKind::MuzzleStar) {
                let o = Vec3::new(inst.xform[3], inst.xform[7], inst.xform[11]);
                let r = 0.5 * Vec3::new(inst.xform[0], inst.xform[4], inst.xform[8]).length();
                let frac = rt::flash_screen_fraction(r, (o - lens.camera).length(), lens.px_per_m, lens.viewport_px);
                let (mult, light) = flashes.admit(lens.real_s, frac);
                inst.custom[3] *= mult;
                lit |= light;
            }
            // Debris may be protected by the caller; flashes, puffs and fireballs take the oldest slot when full.
            let prot = match kind { VfxKind::Debris => prot_debris, _ => Protection::None };
            self.spawn(now, kind, life, prot, inst);
        }
        lit
    }

    fn debris_limit(lens: Lens, n: u32) -> u32 {
        if lens.degrade_step >= DEGRADE_DEBRIS { (n / 2).min(DEGRADED_DEBRIS_PER_EVENT) } else { n }
    }

    #[allow(clippy::too_many_arguments)]
    fn explosion(&mut self, pos: Vec3, radius: f32, debris: u32, lens: Lens, flashes: &mut FlashLimiter, now: f64, tick: u64) {
        let Some(pos) = fin3(pos) else { return };
        let seed = self.next_seed(tick);
        let lit = self.spawn_layout(rt::fireball_layout(pos.into(), radius, self.wind.into(), seed), lens, flashes, now, Protection::None, None);
        if debris > 0 {
            let items: Vec<_> = rt::hit_layout(pos.into(), [0.0, 1.0, 0.0], debris, seed ^ 0x5bd1).into_iter().filter(|i| i.0 == VfxKind::Debris).collect();
            let limit = Self::debris_limit(lens, debris);
            self.spawn_layout(items, lens, flashes, now, Protection::None, Some(limit));
        }
        if lit {
            let range = (radius * 6.0).clamp(8.0, 30.0);
            self.timed.push(TimedLight { light: VfxLight { pos: (pos + Vec3::Y * radius).into(), range_m: range, duration_s: rt::MUZZLE_LIGHT_MAX_S }, born_s: now, energy: 10.0, own_gun: false });
        }
    }

    fn splash(&mut self, pos: Vec3, calibre: u16, own: bool, lens: Lens, now: f64, tick: u64) {
        let Some(pos) = fin3(pos) else { return };
        let seed = self.next_seed(tick);
        let layout = rt::splash_layout([pos.x, 0.0, pos.z], calibre, seed);
        let far = (pos - lens.camera).length() > SPLASH_ONE_PRISM_BEYOND_M && lens.degrade_step >= DEGRADE_TRAILS_KELVIN_SPLASH && !own;
        let prot = if own { Protection::OwnSplash } else { Protection::None };
        for (k, col) in layout.columns.iter().enumerate() {
            if far && k > 0 { break; }
            self.spawn(now, VfxKind::SplashColumn, col.custom[1], prot, *col);
        }
        if !far { self.spawn(now, VfxKind::SplashFoam, rt::SPLASH_FOAM_S, prot, layout.foam); }
    }

    fn shell_end(&mut self, end: &ShellEnd, view: &ViewFrame, lens: Lens, flashes: &mut FlashLimiter, now: f64) {
        let pos = Vec3::from(end.pos);
        if !pos.is_finite() { return; }
        let tick = view.render_tick.max(0.0) as u64;
        if end.water { return self.splash(pos, end.calibre_mm, end.own_side, lens, now, tick); }
        // The hull it struck: the nearest drawn vessel whose box holds the point (generously).
        let mut best: Option<(f32, EntityId, Affine3A)> = None;
        for p in &view.vessels {
            let pose = pose_affine(p.pos, p.rot);
            let Some(v) = self.vessels.get(&p.entity) else { continue };
            let size = v.design.as_ref().map_or(Vec3::splat(20.0), |d| d.size);
            let local = pose.inverse().transform_point3(pos);
            let out = (local - local.clamp(Vec3::ZERO, size)).length();
            if out < 4.0 && best.is_none_or(|b| out < b.0) { best = Some((out, p.entity, pose)); }
        }
        let Some((_, vessel, pose)) = best else {
            if pos.y > AIR_BURST_MIN_Y_M {
                let seed = self.next_seed(tick);
                let (inst, life) = rt::flak_burst(pos.into(), end.calibre_mm, seed);
                self.spawn(now, VfxKind::FlakBurst, life, Protection::None, inst);
            }
            return;
        };
        // Outward from the hull's centreline, lifted a little.
        let size = self.vessels.get(&vessel).and_then(|v| v.design.as_ref()).map_or(Vec3::splat(20.0), |d| d.size);
        let local = pose.inverse().transform_point3(pos);
        let centre = Vec3::new(local.x, size.y * 0.5, size.z * 0.5);
        let n_local = (local - centre) * Vec3::new(0.2, 1.0, 1.0);
        let normal = (pose.transform_vector3(n_local).normalize_or(Vec3::Y) + Vec3::Y * 0.3).normalize_or(Vec3::Y);
        if let Some(v) = self.vessels.get_mut(&vessel) { v.last_hit_local = Some(local); }
        let cells = (end.calibre_mm as u32 / 8).clamp(3, 48);
        let seed = self.next_seed(tick);
        let limit = Self::debris_limit(lens, cells);
        let lit = self.spawn_layout(rt::hit_layout(pos.into(), normal.into(), cells, seed), lens, flashes, now, Protection::None, Some(limit));
        if lit {
            self.timed.push(TimedLight { light: VfxLight { pos: (pos + normal).into(), range_m: (8.0 + 0.05 * end.calibre_mm as f32).min(rt::MUZZLE_LIGHT_MAX_M), duration_s: 0.1 }, born_s: now, energy: 8.0, own_gun: false });
        }
    }

    /// Muzzle position and bore of one mount, from the anchors or the design.
    pub(crate) fn muzzles(&self, view: &ViewFrame, vessel: EntityId, slot: PartSlot) -> (Vec<(Vec3, Vec3)>, u16) {
        let design = self.vessels.get(&vessel).and_then(|v| v.design.clone());
        let calibre = design.as_ref().and_then(|d| d.mounts.iter().find(|m| m.0 == slot)).map_or(DEFAULT_MUZZLE_MM, |m| m.3);
        let from_anchors: Vec<(Vec3, Vec3)> = self.anchors.iter().filter(|a| a.vessel == vessel && a.slot == slot && a.kind == AnchorKind::Muzzle).map(|a| (Vec3::from(a.pos), Vec3::from(a.dir))).filter(|(p, d)| p.is_finite() && d.is_finite()).collect();
        if !from_anchors.is_empty() { return (from_anchors, calibre); }
        let Some((pose, p)) = Self::pose_of(view, vessel) else { return (Vec::new(), calibre) };
        let Some((top, placed, local, rig)) = design.as_ref().and_then(|d| d.mounts.iter().find(|m| m.0 == slot)).map(|m| (m.1, m.2, m.4, m.5.clone())) else { return (Vec::new(), calibre) };
        let turret = view.turrets.get(p.turrets.start as usize..p.turrets.end as usize).and_then(|ts| ts.iter().find(|t| t.slot == slot));
        let (train, elev) = turret.map_or((0.0, 0.1), |t| (t.train_rad, t.elevation_rad));
        if let Some(rig) = rig {
            let found = rig.muzzles(pose * local, train, elev);
            if !found.is_empty() { return (found, calibre); }
        }
        // Part frame: +X bow, +Y up; train about +Y and elevation about +Z, as `Joint::Train` and
        // `Joint::Elevate` turn the model; then the part's placement rotation into the vessel frame.
        let bore = (placed * (glam::Quat::from_rotation_y(train) * glam::Quat::from_rotation_z(elev) * Vec3::X)).normalize_or(Vec3::X);
        let reach = 0.5 + 0.02 * calibre as f32;
        let pos = pose.transform_point3(top + Vec3::Y * 0.3 + bore * reach);
        (vec![(pos, pose.transform_vector3(bore).normalize_or(Vec3::X))], calibre)
    }

    fn apply(&mut self, ev: &WireEvent, view: &ViewFrame, lens: Lens, flashes: &mut FlashLimiter, now: f64) {
        let world = |p: glam::DVec3| Vec3::from(view.origin.to_render(p));
        match &ev.event {
            SimEvent::Phys(PhysEvent::MountFired { vessel, slot, rounds }) => {
                let (muzzles, calibre) = self.muzzles(view, *vessel, *slot);
                let own = view.own.and_then(|i| view.vessels.get(i as usize)).is_some_and(|o| o.entity == *vessel);
                for (k, (pos, dir)) in muzzles.into_iter().enumerate().take((*rounds).max(1) as usize) {
                    let seed = self.next_seed(ev.tick ^ (k as u64) << 40);
                    let lit = self.spawn_layout(rt::muzzle_layout(pos.into(), dir.into(), calibre, self.wind.into(), seed), lens, flashes, now, Protection::None, None);
                    if lit && k == 0 { self.timed.push(TimedLight { light: rt::muzzle_light(pos.into(), dir.into(), calibre), born_s: now, energy: 6.0 + 0.02 * calibre as f32, own_gun: own }); }
                }
            }
            SimEvent::Phys(PhysEvent::BodySpawned { body, pos, vel, side, .. }) => {
                self.sides.insert(*body, *side);
                let p = world(*pos);
                let d = Vec3::new(vel.x as f32, vel.y as f32, vel.z as f32).normalize_or(Vec3::Y);
                if p.is_finite() {
                    let seed = self.next_seed(ev.tick ^ body.to_u64());
                    // A launch is smoke and haze, no star: the motor's light is the trail's head.
                    let items = rt::muzzle_layout(p.into(), d.into(), LAUNCH_SMOKE_MM, self.wind.into(), seed).into_iter().filter(|i| i.0 != VfxKind::MuzzleStar).collect();
                    self.spawn_layout(items, lens, flashes, now, Protection::None, None);
                }
            }
            SimEvent::Phys(PhysEvent::BodyImpact { world_point, vessel, .. }) => {
                let p = world(*world_point);
                if let Some((pose, _)) = Self::pose_of(view, *vessel) { if let Some(v) = self.vessels.get_mut(vessel) { v.last_hit_local = Some(pose.inverse().transform_point3(p)); } }
                self.explosion(p, WARHEAD_RADIUS_M, 32, lens, flashes, now, ev.tick);
            }
            SimEvent::Phys(PhysEvent::BodyBurst { pos, underwater, .. }) => {
                let p = world(*pos);
                if *underwater { self.splash(p, UNDERWATER_BURST_MM, false, lens, now, ev.tick); } else {
                    self.explosion(p, WARHEAD_RADIUS_M * 0.75, 0, lens, flashes, now, ev.tick);
                    let seed = self.next_seed(ev.tick);
                    let (inst, life) = rt::flak_burst(p.into(), 127, seed);
                    self.spawn(now, VfxKind::FlakBurst, life, Protection::None, inst);
                }
            }
            SimEvent::Phys(PhysEvent::BodyDestroyed { pos, shot_down, .. }) => {
                let p = world(*pos);
                if p.y < 0.5 { self.splash(p, 150, false, lens, now, ev.tick); } else { self.explosion(p, if *shot_down { SHOT_DOWN_RADIUS_M } else { WARHEAD_RADIUS_M }, 8, lens, flashes, now, ev.tick); }
            }
            SimEvent::Phys(PhysEvent::CookOff { radius_cells, pos, vessel, .. }) => {
                let p = world(*pos);
                if let Some((pose, _)) = Self::pose_of(view, *vessel) { if let Some(v) = self.vessels.get_mut(vessel) { v.last_hit_local = Some(pose.inverse().transform_point3(p)); } }
                self.explosion(p, (*radius_cells).max(1) as f32 * COOKOFF_M_PER_CELL, rt::MAX_DEBRIS, lens, flashes, now, ev.tick);
            }
            SimEvent::Phys(PhysEvent::Debris { cells, pos, .. }) => {
                let p = world(*pos);
                if p.is_finite() && !cells.is_empty() {
                    let n = (cells.len() as u32).min(rt::MAX_DEBRIS);
                    let seed = self.next_seed(ev.tick);
                    let items: Vec<_> = rt::hit_layout(p.into(), [0.0, 1.0, 0.0], n, seed).into_iter().filter(|i| i.0 == VfxKind::Debris).collect();
                    self.spawn_layout(items, lens, flashes, now, Protection::None, Some(Self::debris_limit(lens, n)));
                }
            }
            SimEvent::Phys(PhysEvent::ShellImpact { vessel, world_point, .. }) => {
                // The visible hit comes through the shell end; this only remembers where the hull was struck.
                let p = world(*world_point);
                if let Some((pose, _)) = Self::pose_of(view, *vessel) { if let Some(v) = self.vessels.get_mut(vessel) { v.last_hit_local = Some(pose.inverse().transform_point3(p)); } }
            }
            SimEvent::Phys(PhysEvent::FireStarted { vessel, compartment, .. }) => {
                if let Some(v) = self.vessels.get_mut(vessel) {
                    let id = compartment.0 as u32;
                    v.fires.entry(id).or_insert(FireFx { id: seed_of(vessel.to_u64(), id as u64), local: Vec3::ZERO, width_m: 0.0, emitted_flames: 0, emitted_smoke: 0 });
                }
            }
            SimEvent::Phys(PhysEvent::FireOut { vessel, compartment, .. }) => if let Some(v) = self.vessels.get_mut(vessel) { v.fires.remove(&(compartment.0 as u32)); },
            SimEvent::Phys(PhysEvent::Breach { vessel, compartment, below_waterline }) => if let Some(v) = self.vessels.get_mut(vessel) {
                let f = v.flooding.entry(compartment.0).or_insert(FloodFx { breach_below: false, boil: None });
                f.breach_below |= *below_waterline;
            },
            SimEvent::Phys(PhysEvent::CompartmentHalfFlooded { vessel, compartment }) => if let Some(v) = self.vessels.get_mut(vessel) {
                v.flooding.entry(compartment.0).or_insert(FloodFx { breach_below: false, boil: None });
            },
            SimEvent::Phys(PhysEvent::DecoyExpired { decoy }) => if let Some(d) = self.decoys.remove(decoy) { for h in d.handles { if let Some(p) = self.pool(h.kind) { p.retire(h); } } },
            SimEvent::Phys(PhysEvent::VesselDespawned { vessel }) => self.forget(*vessel),
            SimEvent::Phys(PhysEvent::WreckDespawned { wreck }) => self.forget(*wreck),
            _ => {}
        }
    }

    fn design_of(&mut self, hash: &ContentHash, lookup: &dyn Fn(&ContentHash) -> Option<Arc<DrawDesign>>) -> Option<Arc<FxDesign>> {
        if *hash == ContentHash::EMPTY { return None; }
        if let Some(d) = self.designs.get(hash) { return Some(d.clone()); }
        let built = Arc::new(FxDesign::from_design(&*lookup(hash)?));
        self.designs.insert(*hash, built.clone());
        Some(built)
    }

    fn vessels_step(&mut self, view: &ViewFrame, lens: Lens, lookup: &dyn Fn(&ContentHash) -> Option<Arc<DrawDesign>>, t0: f64, now: f64) {
        for v in self.vessels.values_mut() { v.seen = false; }
        let own_entity = view.own.and_then(|i| view.vessels.get(i as usize)).map(|o| o.entity);
        // Allies report whether they flood: a cleared report clears what the events said.
        let dry_allies: Vec<EntityId> = view.snapshot.as_ref().map(|s| s.allies.iter().filter(|a| !a.flooding).map(|a| a.vessel).collect()).unwrap_or_default();
        let own_comps: Vec<(u16, u8, u8, bool)> = view.snapshot.as_ref().and_then(|s| s.ship.as_ref()).filter(|s| Some(s.vessel) == own_entity).map(|s| s.compartments.iter().map(|c| (c.id.0, c.fill, c.fire, c.breached)).collect()).unwrap_or_default();
        let dt = (now - t0).clamp(0.0, 1.0) as f32;
        let wind = self.wind;
        for pose in &view.vessels {
            let entity = pose.entity;
            if !self.vessels.contains_key(&entity) {
                // Drawn without a reveal this unit saw (built after it): the hash is unknown, but wakes and
                // fire presence still follow from the pose.
                self.vessels.insert(entity, VesselFx::new(ContentHash::EMPTY, self.sides.get(&entity).copied(), true));
            }
            let hash = self.vessels[&entity].hash;
            let design = match self.vessels[&entity].design.clone() { Some(d) => Some(d), None => { let d = self.design_of(&hash, lookup); if let Some(d) = &d { self.vessels.get_mut(&entity).unwrap().design = Some(d.clone()); } d } };
            let xf = pose_affine(pose.pos, pose.rot);
            let geo = design.clone().unwrap_or_default();
            let size = if geo.size.length() > 0.0 { geo.size } else { Vec3::new(40.0, 6.0, 8.0) };
            let speed_kn = kn(Vec3::new(pose.vel[0], 0.0, pose.vel[2]).length()).min(80.0);
            let heading = heading_of(&xf);
            // A hull under the surface (sunk, or a submarine) lays no wake and shows no flooding cue.
            let sunk = pose.flags.contains(VesselFlags::SUBMERGED);
            let own = Some(entity) == own_entity;
            // The compartment report is the own ship's: no other hull's fires, fill or breaches read it.
            let own_comps: &[(u16, u8, u8, bool)] = if own { &own_comps } else { &[] };

            // Waterline baseline: the design's, or the first pose's.
            let up = xf.matrix3.y_axis.y;
            let pose_wl = if up.abs() > 0.2 { (-xf.translation.y / up).clamp(0.0, size.y) } else { size.y / 3.0 };
            let v = self.vessels.get_mut(&entity).unwrap();
            v.seen = true;
            let wl = match geo.waterline_m {
                Some(w) => *v.baseline_wl.insert(w),
                None => {
                    let b = v.baseline_wl.get_or_insert(pose_wl);
                    if v.baseline_s < BASELINE_LEARN_S && pose_wl.is_finite() {
                        v.baseline_s += dt;
                        *b += (pose_wl - *b) * (dt / 4.0).clamp(0.0, 1.0);
                    }
                    *b
                }
            };
            let surface = |p: Vec3| Vec3::new(p.x, 0.0, p.z);

            // Wake, Kelvin arms and bow wave.
            let (stern_x, stem_x) = geo.waterline_ends_m.unwrap_or((0.0, size.x));
            let stern = surface(xf.transform_point3(Vec3::new(stern_x, wl, size.z * 0.5)));
            let bow = surface(xf.transform_point3(Vec3::new(stem_x, wl, size.z * 0.5)));
            let moving = speed_kn > WAKE_MIN_KN && !sunk && stern.is_finite();
            // The wake ribbon and the Kelvin arms (L1967) are the stern's track, drawn by the sea's material.
            let hull = WakeHull { stern, bow, speed_ms: Vec3::new(pose.vel[0], 0.0, pose.vel[2]).length(), beam_m: size.z, length_m: (stem_x - stern_x).abs().max(1.0) };
            v.track.step(hull, moving, sunk, now);
            let bow_inst = rt::bow_wave_instance(bow.into(), heading, size.z, if moving { speed_kn } else { 0.0 });

            // Fires: the named compartments, or presence on a burning hull with none named.
            let burning = pose.flags.contains(VesselFlags::BURNING) || own_comps.iter().any(|c| c.2 > 0);
            if own { for c in own_comps { if c.2 > 0 { v.fires.entry(c.0 as u32).or_insert(FireFx { id: seed_of(entity.to_u64(), c.0 as u64), local: Vec3::ZERO, width_m: 0.0, emitted_flames: 0, emitted_smoke: 0 }); } } }
            let named = v.fires.keys().any(|k| *k != u32::MAX);
            if burning && !named { v.fires.entry(u32::MAX).or_insert(FireFx { id: seed_of(entity.to_u64(), u32::MAX as u64), local: Vec3::ZERO, width_m: 0.0, emitted_flames: 0, emitted_smoke: 0 }); } else if named || !burning { v.fires.remove(&u32::MAX); }
            if own && !own_comps.is_empty() { v.fires.retain(|k, _| *k == u32::MAX || own_comps.iter().any(|c| c.0 as u32 == *k && c.2 > 0)); }
            if sunk { v.fires.clear(); }
            let deck_m = geo.deck_m.unwrap_or(wl + (0.35 * (size.y - wl)).max(1.5)).min(size.y);
            for (k, f) in v.fires.iter_mut() {
                let comp = u16::try_from(*k).ok().and_then(|c| geo.compartments.get(&c));
                let (local, width) = match comp {
                    Some(c) => (c.top, c.width_m),
                    _ => (v.last_hit_local.map(|h| Vec3::new(h.x.clamp(0.0, size.x), deck_m.max(h.y.min(size.y)), h.z.clamp(0.0, size.z))).unwrap_or(Vec3::new(size.x * 0.5, deck_m, size.z * 0.5)), (size.z * 0.3).clamp(1.5, 6.0)),
                };
                f.local = local;
                f.width_m = width;
            }
            let fires: Vec<(u32, FireFx)> = v.fires.iter().map(|(k, f)| (*k, *f)).collect();
            let intensity_of = |k: u32| own_comps.iter().find(|c| c.0 as u32 == k).map_or(0.6, |c| (c.2 as f32 / 255.0).clamp(0.15, 1.0));

            // Flooding: named compartments, the own ship's fill, and sinkage.
            if own { for c in own_comps { if c.1 > 0 || c.3 { v.flooding.entry(c.0).or_insert(FloodFx { breach_below: c.3, boil: None }); } } }
            let mut dry_boils = Vec::new();
            if dry_allies.contains(&entity) { for f in std::mem::take(&mut v.flooding).into_values() { dry_boils.extend(f.boil); } }
            if own && !own_comps.is_empty() {
                let keep: Vec<u16> = own_comps.iter().filter(|c| c.1 > 0 || c.3).map(|c| c.0).collect();
                v.flooding.retain(|k, f| { let wet = keep.contains(k); if !wet { dry_boils.extend(f.boil.take()); } wet });
            }
            // Freeboard lost per side (port 0, starboard 1), smoothed.
            let freeboard0 = (deck_m - wl).max(0.5);
            let a = (dt / FREEBOARD_SMOOTH_S).clamp(0.0, 1.0);
            for (k, z) in [0.0f32, size.z].into_iter().enumerate() {
                let m = &xf.matrix3;
                let (r10, r11, r12) = (m.x_axis.y, m.y_axis.y, m.z_axis.y);
                let x = size.x * 0.5;
                let wl_side = if r11.abs() > 0.2 { (-xf.translation.y - r10 * x - r12 * z) / r11 } else { deck_m };
                // Signed: a roll to the other side and a wave trough count as negative, so the swell averages
                // out instead of adding up.
                let lost = ((wl_side - wl) / freeboard0).clamp(-2.0, 2.0);
                v.loss[k] = if lost.is_finite() { v.loss[k] + (lost - v.loss[k]) * a } else { v.loss[k] };
                let on = if v.band_on[k] { v.loss[k] > FOAM_BAND_OFF_LOSS } else { v.loss[k] > FOAM_BAND_MIN_LOSS };
                v.band_on[k] = on;
            }
            let flooding_known = !v.flooding.is_empty();
            let mut bands: [Option<VfxInstance>; 2] = [None, None];
            for (k, z) in [0.0f32, size.z].into_iter().enumerate() {
                let lost = v.loss[k].clamp(0.0, 1.0);
                if sunk || !(flooding_known || v.band_on[k]) { continue; }
                let a = surface(xf.transform_point3(Vec3::new(size.x * 0.12, wl, z)));
                let b = surface(xf.transform_point3(Vec3::new(size.x * 0.88, wl, z)));
                let out = xf.transform_vector3(if k == 0 { Vec3::NEG_Z } else { Vec3::Z });
                if a.is_finite() && b.is_finite() && out.is_finite() { bands[k] = Some(rt::foam_band(a.into(), b.into(), out.into(), lost)); }
            }
            let pumps_wanted: Vec<(PartSlot, Vec3, Vec3)> = if flooding_known && !sunk {
                let from_anchor: Vec<(PartSlot, Vec3, Vec3)> = self.anchors.iter().filter(|a| a.vessel == entity && a.kind == AnchorKind::PumpOutlet).map(|a| (a.slot, Vec3::from(a.pos), Vec3::from(a.dir))).collect();
                if !from_anchor.is_empty() { from_anchor } else { geo.pumps.iter().map(|(s, p, o)| (*s, xf.transform_point3(*p), xf.transform_vector3(*o))).collect() }
            } else { Vec::new() };
            let boils: Vec<(u16, Option<Vec3>)> = v.flooding.iter().map(|(k, f)| {
                let c = geo.compartments.get(k);
                // Water boils up where it enters: a breach the sim placed below the waterline, or (own ship) a
                // breached compartment that is taking water and reaches below the waterline.
                let entering = own_comps.iter().any(|oc| oc.0 == *k && oc.3 && oc.1 > 0) && c.is_some_and(|c| c.bottom_m < wl);
                let below = f.breach_below || entering;
                (*k, c.filter(|_| below && !sunk).map(|c| surface(xf.transform_point3(Vec3::new(c.side.x, wl, c.side.z) + c.outward * 0.8))))
            }).collect();
            let fill_of = |k: u16| own_comps.iter().find(|c| c.0 == k).map_or(0.5, |c| c.1 as f32 / 255.0);
            let depth_of = |k: u16| geo.compartments.get(&k).map_or(1.0, |c| (wl - c.bottom_m).max(0.2));

            // Apply: bow wave, wakes, bands, pumps, boils, fires.
            let v_bow = v.bow;
            let bow_live = v_bow.is_some_and(|h| self.pools[pool_index(VfxKind::BowWave).unwrap()].is_live(h));
            let bow_pool = &mut self.pools[pool_index(VfxKind::BowWave).unwrap()];
            if moving {
                match v_bow.filter(|_| bow_live) { Some(h) => bow_pool.update(h, bow_inst), None => { let h = bow_pool.spawn(now, f32::INFINITY, Protection::None, bow_inst); self.vessels.get_mut(&entity).unwrap().bow = h; } }
            } else if let Some(h) = v_bow { bow_pool.retire(h); self.vessels.get_mut(&entity).unwrap().bow = None; }
            {
                let pool = &mut self.pools[pool_index(VfxKind::FoamBand).unwrap()];
                let v = self.vessels.get_mut(&entity).unwrap();
                for (slot, want) in v.bands.iter_mut().zip(bands) {
                    match (want, slot.filter(|h| pool.is_live(*h))) {
                        (Some(i), Some(h)) => pool.update(h, i),
                        (Some(i), None) => *slot = pool.spawn(now, f32::INFINITY, Protection::Flooding, i),
                        (None, Some(h)) => { pool.retire(h); *slot = None; }
                        (None, None) => *slot = None,
                    }
                }
            }
            for h in dry_boils { if let Some(p) = self.pool(h.kind) { p.retire(h); } }
            self.attach_pumps(entity, &pumps_wanted, now);
            for (k, at) in boils {
                let seed = seed_of(entity.to_u64(), k as u64);
                let inst = at.map(|p| rt::boil_ring(p.into(), 0.6 + 1.6 * fill_of(k), depth_of(k), seed));
                let pool = &mut self.pools[pool_index(VfxKind::BoilRing).unwrap()];
                let v = self.vessels.get_mut(&entity).unwrap();
                let f = v.flooding.get_mut(&k).unwrap();
                match (inst, f.boil.filter(|h| pool.is_live(*h))) {
                    (Some(i), Some(h)) => pool.update(h, i),
                    (Some(i), None) => f.boil = pool.spawn(now, f32::INFINITY, Protection::Flooding, i),
                    (None, Some(h)) => { pool.retire(h); f.boil = None; }
                    (None, None) => f.boil = None,
                }
            }
            for (k, f) in fires {
                let base = xf.transform_point3(f.local);
                if !base.is_finite() { continue; }
                let intensity = intensity_of(k);
                let phase = (f.id & 0xffff) as f32 / 65_536.0;
                let n_flames = rt::emissions_between(t0, now, rt::flame_hz(intensity), phase);
                let n_smoke = rt::emissions_between(t0, now, rt::fire_smoke_hz(intensity), (phase + 0.37).fract());
                let far = (base - lens.camera).length() > SMOKE_HALVED_BEYOND_M && lens.degrade_step >= DEGRADE_DEBRIS;
                let mut fx = f;
                for _ in 0..n_flames {
                    fx.emitted_flames = fx.emitted_flames.wrapping_add(1);
                    let (inst, life) = rt::flame_tongue(base.into(), f.width_m, intensity, wind.into(), rt::effect_seed(f.id, fx.emitted_flames));
                    self.spawn(now, VfxKind::FlamePrism, life, Protection::Fire, inst);
                }
                let top = base + Vec3::Y * f.width_m * (1.2 + 0.8 * intensity);
                for _ in 0..n_smoke {
                    fx.emitted_smoke = fx.emitted_smoke.wrapping_add(1);
                    if far && fx.emitted_smoke % 2 == 1 { continue; }
                    let (inst, life) = rt::fire_smoke_puff(top.into(), intensity, wind.into(), rt::effect_seed(f.id ^ 0xA5A5, fx.emitted_smoke));
                    self.spawn(now, VfxKind::SmokePuff, life, Protection::None, inst);
                }
                if let Some(slot) = self.vessels.get_mut(&entity).and_then(|v| v.fires.get_mut(&k)) { slot.emitted_flames = fx.emitted_flames; slot.emitted_smoke = fx.emitted_smoke; }
            }
        }
        // Vessels no longer drawn lose their attached effects but keep their state (fires, flooding) until
        // hidden: a hull that drops out of the view for a frame resumes where it was.
        let gone: Vec<EntityId> = self.vessels.iter().filter(|(_, v)| !v.seen).map(|(e, _)| *e).collect();
        for e in gone {
            let v = self.vessels.get_mut(&e).unwrap();
            let hs: Vec<VfxHandle> = std::iter::once(v.bow.take()).chain(v.bands.iter_mut().map(|b| b.take())).flatten().chain(v.pumps.drain(..).map(|p| p.1)).chain(v.flooding.values_mut().filter_map(|f| f.boil.take())).collect();
            for h in hs { if let Some(p) = self.pool(h.kind) { p.retire(h); } }
        }
    }

    fn attach_pumps(&mut self, entity: EntityId, wanted: &[(PartSlot, Vec3, Vec3)], now: f64) {
        let pool = &mut self.pools[pool_index(VfxKind::PumpArc).unwrap()];
        let Some(v) = self.vessels.get_mut(&entity) else { return };
        v.pumps.retain(|(slot, h)| { let keep = wanted.iter().any(|w| w.0 == *slot) && pool.is_live(*h); if !keep { pool.retire(*h); } keep });
        for (slot, outlet, outward) in wanted {
            if !(outlet.is_finite() && outward.is_finite()) { continue; }
            let inst = rt::pump_arc((*outlet).into(), (*outward).into(), outlet.y.max(0.3), PUMP_FLOW_UNKNOWN, seed_of(entity.to_u64(), slot.0 as u64));
            match v.pumps.iter().find(|p| p.0 == *slot) {
                Some((_, h)) => pool.update(*h, inst),
                None => if let Some(h) = pool.spawn(now, f32::INFINITY, Protection::Flooding, inst) { v.pumps.push((*slot, h)); },
            }
        }
    }

    fn trails_step(&mut self, view: &ViewFrame, now: f64) {
        for t in self.trails.values_mut() { t.seen = false; }
        let life = rt::trail_length_s(self.tier);
        let own_side = self.own_side(view);
        let mut spawns = Vec::new();
        for b in &view.bodies {
            let pos = Vec3::from(b.pos);
            if !pos.is_finite() { continue; }
            // Torpedoes run under the surface and draw no smoke trail; a body below the sea does not either.
            let airborne = pos.y > 0.5;
            let incoming = !b.own_side && own_side.is_some();
            match self.trails.get_mut(&b.entity) {
                Some(t) => {
                    t.seen = true;
                    if (now - t.last_s) as f32 >= rt::TRAIL_EMIT_S || now < t.last_s {
                        let d = (pos - t.last).length();
                        if airborne && d > 0.05 && d < 2_000.0 && now >= t.last_s { spawns.push((rt::trail_segment(t.last.into(), pos.into()), incoming)); }
                        t.last = pos;
                        t.last_s = now;
                    }
                }
                None => { self.trails.insert(b.entity, TrailFx { last: pos, last_s: now, incoming, seen: true }); }
            }
        }
        self.trails.retain(|_, t| t.seen);
        for (mut inst, incoming) in spawns {
            // The trail's life in custom.y, so the shader's normalised age runs 0 to 1 over it: the smoke spreads,
            // thins and takes its ramp as it ages (without it every segment drew as fresh, dense smoke).
            inst.custom[1] = life;
            self.spawn(now, VfxKind::MissileTrail, life, if incoming { Protection::IncomingMissileHead } else { Protection::None }, inst);
        }
    }

    fn decoys_step(&mut self, view: &ViewFrame, now: f64) {
        let tuning = content::catalogue::catalogue().tuning.decoys;
        for d in self.decoys.values_mut() { d.seen = false; }
        for d in &view.decoys {
            let pos = Vec3::from(d.pos);
            if !pos.is_finite() || d.kind == DecoyKind::TowedDecoy { continue; }
            let age = if d.age_s.is_finite() { d.age_s.max(0.0) } else { 0.0 };
            let seed = seed_of(d.entity.to_u64(), 0xDEC0);
            let wanted: Vec<(VfxKind, VfxInstance, Protection)> = match d.kind {
                DecoyKind::Chaff => vec![(VfxKind::ChaffCloud, rt::chaff_cloud(pos.into(), age, &tuning, seed), Protection::None)],
                DecoyKind::Flare => vec![(VfxKind::FlareBurn, rt::flare_burn(pos.into(), age, &tuning, seed), Protection::None)],
                DecoyKind::Smoke => match smoke_cylinder(age, &tuning) {
                    Some((r, h)) => (0..rt::DEFENCE_SMOKE_PUFFS).map(|k| (VfxKind::DefenceSmoke, rt::defence_smoke_puff([pos.x, 0.0, pos.z], r, h, seed, k), Protection::DefenceSmoke)).collect(),
                    None => Vec::new(),
                },
                DecoyKind::TowedDecoy => Vec::new(),
            };
            let entry = self.decoys.entry(d.entity).or_insert_with(|| DecoyFx { kind: d.kind, handles: Vec::new(), seed, seen: true });
            entry.seen = true;
            let mut handles = std::mem::take(&mut entry.handles);
            if wanted.is_empty() {
                for h in handles.drain(..) { if let Some(i) = pool_index(h.kind) { self.pools[i].retire(h); } }
            }
            for (k, (kind, inst, prot)) in wanted.into_iter().enumerate() {
                let Some(i) = pool_index(kind) else { continue };
                let pool = &mut self.pools[i];
                match handles.get(k).copied().filter(|h| pool.is_live(*h)) {
                    Some(h) => pool.update(h, inst),
                    None => {
                        let life = match kind { VfxKind::ChaffCloud => (tuning.chaff_life_s as f32 - age).max(0.0), VfxKind::FlareBurn => (tuning.flare_life_s as f32 - age).max(0.0), _ => f32::INFINITY };
                        // The builders set their whole life in custom.y; age it from the decoy's own age.
                        let mut inst = inst;
                        if inst.custom[1] > 0.0 && life.is_finite() { inst.custom[1] = life.max(0.05); }
                        if let Some(h) = pool.spawn(now, life, prot, inst) { if k < handles.len() { handles[k] = h; } else { handles.push(h); } }
                    }
                }
            }
            if let Some(e) = self.decoys.get_mut(&d.entity) { e.handles = handles; }
        }
        let gone: Vec<EntityId> = self.decoys.iter().filter(|(_, d)| !d.seen).map(|(e, _)| *e).collect();
        for e in gone { if let Some(d) = self.decoys.remove(&e) { for h in d.handles { if let Some(i) = pool_index(h.kind) { self.pools[i].retire(h); } } } }
    }

    /// One instance of every kind, a metre across, 40 m in front of the camera for one frame: each material
    /// draws once so its pipeline compiles at load, not at first use.
    /// They live until the next frame whatever the clock does (it may stand still while loading).
    fn warm_up(&mut self, camera: Vec3, forward: Vec3, now: f64) {
        self.warm = false;
        let at = camera + forward.normalize_or(Vec3::NEG_Z) * 40.0;
        for i in 0..KINDS {
            let inst = VfxInstance { xform: rt::xform([1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0], at.into()), color: [1.0; 4], custom: [0.0, 0.0, 0.5, 0.0] };
            if let Some(h) = self.pools[i].spawn(now, f32::INFINITY, Protection::None, inst) { self.warm_handles.push(h); }
        }
    }

    fn lights_step(&mut self, view: &ViewFrame, lens: Lens, now: f64) {
        self.lights.clear();
        let (total, fire_max) = self.light_budget;
        if total == 0 { self.timed.clear(); return; }
        self.timed.retain(|l| (now - l.born_s) < l.light.duration_s as f64 && now >= l.born_s);
        let own_only = lens.degrade_step >= DEGRADE_LIGHTS_OWN_GUNS;
        // Flashes and muzzles first (own guns ahead of the rest), nearest first.
        let mut flashes: Vec<(bool, f32, LightOut)> = self.timed.iter().filter(|l| !own_only || l.own_gun).map(|l| {
            let pos = Vec3::from(l.light.pos);
            let t = ((now - l.born_s) as f32 / l.light.duration_s.max(1e-3)).clamp(0.0, 1.0);
            (l.own_gun, (pos - lens.camera).length(), LightOut { pos, range_m: l.light.range_m, energy: l.energy * (1.0 - t * t), fire: false })
        }).collect();
        flashes.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.total_cmp(&b.1)));
        self.lights.extend(flashes.into_iter().take(total).map(|f| f.2));
        if own_only { return; }
        let mut fires: Vec<(f32, LightOut)> = Vec::new();
        for p in &view.vessels {
            let Some(v) = self.vessels.get(&p.entity) else { continue };
            if v.fires.is_empty() { continue; }
            let xf = pose_affine(p.pos, p.rot);
            for f in v.fires.values() {
                let pos = xf.transform_point3(f.local + Vec3::Y * f.width_m);
                if !pos.is_finite() { continue; }
                let phase = (f.id & 0xff) as f32 * 0.1;
                let flicker = 0.85 + 0.15 * ((now as f32 * FIRE_FLICKER_HZ * std::f32::consts::TAU + phase).sin() * 0.6 + (now as f32 * 2.9 + phase * 3.0).sin() * 0.4);
                fires.push(((pos - lens.camera).length(), LightOut { pos, range_m: (f.width_m * 5.0).clamp(8.0, 25.0), energy: 3.0 * flicker, fire: true }));
            }
        }
        fires.sort_by(|a, b| a.0.total_cmp(&b.0));
        let room = total.saturating_sub(self.lights.len()).min(fire_max);
        self.lights.extend(fires.into_iter().take(room).map(|f| f.1));
    }
}

fn rt_floats() -> usize { hull_mesh::vfx::INSTANCE_FLOATS }

/// Defence smoke cylinder radius and height at `age_s`, the sim's own rule (`Decoys::smoke_cylinder`):
/// both grow linearly to full size over the growth time and the screen ends at its life.
pub(crate) fn smoke_cylinder(age_s: f32, t: &content::tuning::DecoyTuning) -> Option<(f32, f32)> {
    let age = age_s as f64;
    if !(age.is_finite()) || age >= t.smoke_life_s { return None; }
    let grow = if t.smoke_grow_s > 0.0 { (age / t.smoke_grow_s).clamp(0.0, 1.0) } else { 1.0 };
    let (r, h) = (t.smoke_radius_m.max(0.0) * grow, t.smoke_height_m.max(0.0) * grow);
    if r <= 0.0 || h <= 0.0 { return None; }
    Some((r as f32, h as f32))
}

/// The box around a kind's drawn instances, padded by how far its shader moves vertices from the instance
/// origin (drift times life for puffs, the ballistic reach of debris, the scale of everything).
pub(crate) fn instance_bounds(kind: VfxKind, floats: &[f32]) -> (Vec3, Vec3) {
    let (mut min, mut max) = (Vec3::splat(f32::MAX), Vec3::splat(f32::MIN));
    for i in floats.chunks_exact(rt_floats()) {
        let o = Vec3::new(i[3], i[7], i[11]);
        let sx = Vec3::new(i[0], i[4], i[8]).length();
        let sy = Vec3::new(i[1], i[5], i[9]).length();
        let sz = Vec3::new(i[2], i[6], i[10]).length();
        let scale = sx.max(sy).max(sz);
        if scale <= 0.0 || !o.is_finite() { continue; }
        let (life, w) = (i[17].min(60.0), i[19]);
        let motion = match kind {
            VfxKind::SmokePuff | VfxKind::MuzzleHaze | VfxKind::HitPuff => (w.abs() + 10.0) * life,
            VfxKind::Debris => { let v = w.abs(); v * life.min(5.0) + v * v / (2.0 * rt::GRAVITY) }
            VfxKind::KelvinArm => w.abs() * life,
            VfxKind::SplashColumn | VfxKind::FlamePrism | VfxKind::Fireball | VfxKind::DefenceSmoke => scale,
            _ => 0.0,
        };
        let r = scale * 1.5 + motion.min(400.0);
        min = min.min(o - r);
        max = max.max(o + r);
    }
    if min.cmpgt(max).any() { (Vec3::ZERO, Vec3::ZERO) } else { (min, max) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::interp::{BodyPose, DecoyPose, VesselPose};
    use crate::render::hull::design::tests::box_design;
    use content::ids::CompartmentId;
    use sim_systems::protocol::EntityIntro;

    const HZ: f64 = sim_core::TICK_HZ as f64;
    fn ev(tick: u64, event: SimEvent) -> WireEvent { WireEvent { tick, event } }
    fn reveal(entity: u64, side: u8) -> WireEvent {
        ev(0, SimEvent::EntityRevealed(Box::new(EntityIntro { entity: EntityId::from_u64(entity), side: SideId(side), blueprint: ContentHash([7; 32]), exterior_only: false, class: None, name: String::new(), player: None, status: Default::default(), damage: Vec::new(), removed: Vec::new(), parent: None, track: Default::default() })))
    }
    fn lens() -> Lens { Lens { camera: Vec3::new(0.0, 10.0, 60.0), forward: Vec3::NEG_Z, real_s: 0.0, px_per_m: 935.0, viewport_px: [1920.0, 1080.0], tier: QualityTier::Medium, degrade_step: 0, reduce_flashing: false } }
    fn vessel(entity: u64, own: bool, x: f32, vel_x: f32) -> VesselPose { VesselPose { entity: EntityId::from_u64(entity), own, pos: [x, -1.0, 0.0], rot: [0.0, 0.0, 0.0, 1.0], vel: [vel_x, 0.0, 0.0], ..Default::default() } }
    fn frame(tick: f64, vessels: Vec<VesselPose>) -> ViewFrame {
        let own = vessels.iter().position(|v| v.own).map(|i| i as u32);
        ViewFrame { render_tick: tick, sim_time_s: tick / HZ, vessels, own, ..Default::default() }
    }
    struct Rig { core: VfxCore, flashes: FlashLimiter, design: Arc<DrawDesign> }
    impl Rig {
        fn new() -> Self { Self { core: VfxCore::new(QualityTier::Medium), flashes: FlashLimiter::default(), design: Arc::new(box_design([80, 8, 16])) } }
        fn step(&mut self, view: &ViewFrame, events: &[WireEvent], ends: &[ShellEnd]) { self.step_with(view, events, ends, lens()) }
        fn step_with(&mut self, view: &ViewFrame, events: &[WireEvent], ends: &[ShellEnd], lens: Lens) {
            let d = self.design.clone();
            self.core.step(view, events, ends, lens, &mut self.flashes, &move |h| if *h == d.hash { Some(d.clone()) } else { None });
        }
        fn live(&self, kind: VfxKind) -> usize { self.core.pools[pool_index(kind).unwrap()].live_count() }
        fn pool(&self, kind: VfxKind) -> &RingPool { &self.core.pools[pool_index(kind).unwrap()] }
    }

    #[test]
    fn kind_indices_match_the_shader_include() {
        assert_eq!(pool_index(VfxKind::Tracer), None);
        assert_eq!(KINDS, 21);
        for (i, k) in VfxKind::ALL.iter().enumerate().skip(1) {
            assert_eq!(pool_index(*k), Some(i - 1));
            assert_eq!(pool_kind(i - 1), *k);
            assert_eq!(shader_kind(*k), i as i64);
        }
        // The K_* constants of inc/vfx_common.gdshaderinc.
        assert_eq!(shader_kind(VfxKind::SplashColumn), 1);
        assert_eq!(shader_kind(VfxKind::MissileTrail), 11);
        assert_eq!(shader_kind(VfxKind::FlareBurn), 21);
    }

    #[test]
    fn splashes_and_hits_from_shell_ends() {
        let mut r = Rig::new();
        let v = frame(60.0, vec![vessel(1, true, 0.0, 0.0)]);
        r.step(&v, &[reveal(1, 0)], &[]);
        let own = ShellEnd { pos: [500.0, 0.0, 0.0], calibre_mm: 127, water: true, own_side: true };
        r.step(&frame(61.0, v.vessels.clone()), &[], &[own]);
        let cols = r.live(VfxKind::SplashColumn);
        assert!((3..=5).contains(&cols), "{cols} columns");
        assert_eq!(r.live(VfxKind::SplashFoam), 1);
        assert!(r.pool(VfxKind::SplashColumn).protection.iter().filter(|p| **p == Protection::OwnSplash).count() == cols);
        // A distant enemy splash under degrade step 3 is one prism and no ring.
        let far = ShellEnd { pos: [0.0, 0.0, -9_000.0], calibre_mm: 127, water: true, own_side: false };
        r.step_with(&frame(62.0, v.vessels.clone()), &[], &[far], Lens { degrade_step: 3, ..lens() });
        assert_eq!(r.live(VfxKind::SplashColumn), cols + 1);
        assert_eq!(r.live(VfxKind::SplashFoam), 1);
        // A hit on the hull: a flash, puffs and debris, which degrade step 2 caps at 16.
        let hit = ShellEnd { pos: [20.0, 2.0, 0.0], calibre_mm: 400, water: false, own_side: false };
        r.step_with(&frame(63.0, v.vessels.clone()), &[], &[hit], Lens { degrade_step: 2, ..lens() });
        assert_eq!(r.live(VfxKind::HitFlash), 1);
        assert!(r.live(VfxKind::HitPuff) >= 3);
        assert_eq!(r.live(VfxKind::Debris), 16);
        // A shell ending high in the air far from every hull is a flak burst.
        let air = ShellEnd { pos: [300.0, 120.0, 300.0], calibre_mm: 40, water: false, own_side: false };
        r.step(&frame(64.0, v.vessels.clone()), &[], &[air]);
        assert_eq!(r.live(VfxKind::FlakBurst), 1);
        // Non-finite input is ignored.
        r.step(&frame(65.0, v.vessels.clone()), &[], &[ShellEnd { pos: [f32::NAN, 0.0, 0.0], calibre_mm: 76, water: true, own_side: true }]);
        assert_eq!(r.live(VfxKind::SplashColumn), cols + 1);
    }

    #[test]
    fn one_shots_wait_for_playback_and_flashes_are_limited() {
        let mut r = Rig::new();
        let e = EntityId::from_u64(1);
        let v = frame(100.0, vec![vessel(1, true, 0.0, 0.0)]);
        r.step(&v, &[reveal(1, 0)], &[]);
        // Muzzle anchors as the parts unit reports them: one per barrel.
        let anchor = |z: f32| AnchorPose { vessel: e, slot: PartSlot(0), kind: AnchorKind::Muzzle, pos: [30.0, 6.0, z], dir: [1.0, 0.0, 0.0] };
        let fired = ev(110, SimEvent::Phys(PhysEvent::MountFired { vessel: e, slot: PartSlot(0), rounds: 2 }));
        r.core.anchors = vec![anchor(-0.5), anchor(0.5)];
        r.step(&v, &[fired], &[]);
        assert_eq!(r.live(VfxKind::MuzzleStar), 0, "held until playback reaches tick 110");
        r.core.anchors = vec![anchor(-0.5), anchor(0.5)];
        r.step(&frame(110.0, v.vessels.clone()), &[], &[]);
        assert_eq!(r.live(VfxKind::MuzzleStar), 2);
        assert_eq!(r.live(VfxKind::MuzzleHaze), 2);
        assert!(r.core.lights.iter().any(|l| !l.fire), "the muzzle lights");
        // An event far ahead (a seek) applies at once.
        let boom = ev(10_000, SimEvent::Phys(PhysEvent::CookOff { vessel: e, magazine: PartSlot(3), radius_cells: 4, pos: glam::DVec3::new(10.0, 3.0, 0.0) }));
        r.step(&frame(111.0, v.vessels.clone()), &[boom], &[]);
        assert!(r.live(VfxKind::Fireball) >= 1);
        // Flash limiter: flashes blooming over 2.5% of the screen in one third of a second; only the first is
        // at full strength. Far away the same flashes are small and never limited.
        let mut r = Rig::new();
        r.step(&v, &[reveal(1, 0)], &[]);
        let ends: Vec<ShellEnd> = (0..4).map(|k| ShellEnd { pos: [k as f32, 2.0, 0.0], calibre_mm: 200, water: false, own_side: false }).collect();
        r.step(&frame(101.0, v.vessels.clone()), &[], &ends);
        assert!(r.pool(VfxKind::HitFlash).instances.iter().filter(|i| i.custom[1] > 0.0).all(|i| i.custom[3] == 1.0));
        let mut r = Rig::new();
        r.step(&v, &[reveal(1, 0)], &[]);
        r.step_with(&frame(101.0, v.vessels.clone()), &[], &ends, Lens { camera: Vec3::new(1.5, 3.0, 8.0), ..lens() });
        let flashes = r.pool(VfxKind::HitFlash);
        let mut w: Vec<f32> = flashes.instances.iter().zip(&flashes.lifetime_s).filter(|(_, l)| **l > 0.0).map(|(i, _)| i.custom[3]).collect();
        w.sort_by(f32::total_cmp);
        assert_eq!(w.len(), 4);
        assert_eq!(w[3], 1.0);
        assert!(w[..3].iter().all(|x| (*x - 0.25).abs() < 1e-6), "{w:?}");
    }

    #[test]
    fn wakes_kelvin_arms_and_bow_waves_follow_the_hull() {
        let mut r = Rig::new();
        let e = EntityId::from_u64(1);
        r.step(&frame(0.0, vec![vessel(1, true, 0.0, 10.0)]), &[reveal(1, 0)], &[]);
        // 10 m/s (19 kn) for 6 s: the stern's track lays a point every 2.31 s, and the sea's material gets it
        // with Kelvin arms (above 8 kn). No ribbon or arm meshes: the sea draws the wake.
        for k in 1..=360 { r.step(&frame(k as f64, vec![vessel(1, true, k as f32 * 10.0 / 60.0, 10.0)]), &[], &[]); }
        let laid = r.core.vessels[&e].track.laid();
        assert!((2..=3).contains(&laid), "6 s of way: {laid}");
        assert_eq!(r.core.wakes.count, 1);
        assert!(r.core.wakes.info[1][1] > 9.9, "arms at the hull's 10 m/s");
        assert_eq!(r.live(VfxKind::WakeSegment) + r.live(VfxKind::KelvinArm), 0);
        assert_eq!(r.live(VfxKind::BowWave), 1);
        // The track's water ages from the stern aft: the newest laid point is at most 2.31 s old.
        let p = r.core.wakes.points;
        assert!(p[1][2] == 0.0 && p[2][2] > 0.0 && p[2][2] <= 2.4, "{:?}", &p[..4]);
        // Degrade step 3 draws no Kelvin arms; stopping retires the bow wave and lays no more water, but the
        // old water still draws while it lives.
        for k in 361..=480 { r.step_with(&frame(k as f64, vec![vessel(1, true, 60.0, 0.0)]), &[], &[], Lens { degrade_step: 3, ..lens() }); }
        assert_eq!(r.core.wakes.info[1][1], 0.0);
        assert_eq!(r.live(VfxKind::BowWave), 0);
        let laid = r.core.vessels[&e].track.laid();
        r.step(&frame(481.0, vec![vessel(1, true, 60.0, 0.0)]), &[], &[]);
        assert_eq!(r.core.vessels[&e].track.laid(), laid);
        assert_eq!(r.core.wakes.count, 1);
        // Hidden: attached effects go, and so does its wake.
        r.step(&frame(482.0, vec![vessel(1, true, 60.0, 10.0)]), &[], &[]);
        r.step(&frame(483.0, vec![vessel(1, true, 60.2, 10.0)]), &[], &[]);
        assert_eq!(r.live(VfxKind::BowWave), 1);
        r.step(&frame(484.0, vec![]), &[ev(484, SimEvent::EntityHidden { entity: e })], &[]);
        assert_eq!(r.live(VfxKind::BowWave), 0);
        assert_eq!(r.core.wakes.count, 0);
    }

    #[test]
    fn fires_burn_until_put_out_and_light_the_hull() {
        let mut r = Rig::new();
        let e = EntityId::from_u64(2);
        let mut burning = vessel(2, false, 0.0, 0.0);
        r.step(&frame(0.0, vec![burning.clone()]), &[reveal(2, 1)], &[]);
        r.step(&frame(1.0, vec![burning.clone()]), &[ev(1, SimEvent::Phys(PhysEvent::FireStarted { vessel: e, compartment: CompartmentId(4), spread: false }))], &[]);
        for k in 2..=120 { r.step(&frame(k as f64, vec![burning.clone()]), &[], &[]); }
        let flames = r.live(VfxKind::FlamePrism);
        assert!(flames >= 4, "3 to 6 tongues a second for 2 s: {flames}");
        assert!(r.live(VfxKind::SmokePuff) >= 3);
        assert!(r.pool(VfxKind::FlamePrism).protection.contains(&Protection::Fire));
        assert!(r.core.lights.iter().any(|l| l.fire));
        // Out: no new tongues; the old ones finish their short lives.
        r.step(&frame(121.0, vec![burning.clone()]), &[ev(121, SimEvent::Phys(PhysEvent::FireOut { vessel: e, compartment: CompartmentId(4), flooded: false }))], &[]);
        for k in 122..=240 { r.step(&frame(k as f64, vec![burning.clone()]), &[], &[]); }
        assert_eq!(r.live(VfxKind::FlamePrism), 0);
        assert!(!r.core.lights.iter().any(|l| l.fire));
        // The BURNING flag alone (a hull revealed already on fire) still burns.
        burning.flags = VesselFlags::BURNING;
        for k in 241..=300 { r.step(&frame(k as f64, vec![burning.clone()]), &[], &[]); }
        assert!(r.live(VfxKind::FlamePrism) >= 1);
        // Degrade step 4: only own guns light.
        r.step_with(&frame(301.0, vec![burning.clone()]), &[], &[], Lens { degrade_step: 4, ..lens() });
        assert!(r.core.lights.is_empty());
    }

    #[test]
    fn the_own_ship_report_marks_only_the_own_ship() {
        use sim_systems::protocol::{CompartmentView, ShipStatus, Snapshot};
        let mut r = Rig::new();
        let (own, enemy, quiet) = (EntityId::from_u64(1), EntityId::from_u64(2), EntityId::from_u64(3));
        let comp = |id: u16, fill: u8, fire: u8, breached: bool| CompartmentView { id: CompartmentId(id), fill, fire, breached, valve_open: false, sealed: false };
        let snapshot = Arc::new(Snapshot { ship: Some(ShipStatus { vessel: own, compartments: vec![comp(4, 0, 255, false), comp(5, 200, 0, true)], ..Default::default() }), ..Default::default() });
        let at = |k: u64| { let mut f = frame(k as f64, vec![vessel(1, true, 0.0, 0.0), vessel(2, false, 300.0, 0.0), vessel(3, false, -300.0, 0.0)]); f.snapshot = Some(snapshot.clone()); f };
        r.step(&at(0), &[reveal(1, 0), reveal(2, 1), reveal(3, 1)], &[]);
        // The enemy burns in a compartment with the same number as the own ship's fire.
        r.step(&at(1), &[ev(1, SimEvent::Phys(PhysEvent::FireStarted { vessel: enemy, compartment: CompartmentId(4), spread: false }))], &[]);
        let seconds = 10;
        for k in 2..=(seconds * sim_core::TICK_HZ as u64) { r.step(&at(k), &[], &[]); }
        let v = |e: EntityId| &r.core.vessels[&e];
        assert!(v(own).fires.contains_key(&4) && v(own).flooding.contains_key(&5), "the own ship burns and floods from its report");
        assert!(v(quiet).fires.is_empty() && v(quiet).flooding.is_empty(), "a hull with no flag or event takes nothing from the own ship's report");
        assert!(v(quiet).bands.iter().all(Option::is_none) && v(quiet).flooding.values().all(|f| f.boil.is_none()));
        assert!(v(enemy).flooding.is_empty() && !v(enemy).fires.contains_key(&u32::MAX));
        // The enemy's fire burns at the default 0.6, not at the own ship's compartment 4 value (1.0).
        let tongues = v(enemy).fires[&4].emitted_flames as f32;
        let expected = rt::flame_hz(0.6) * (seconds as f32 - 1.0 / sim_core::TICK_HZ as f32);
        assert!((tongues - expected).abs() <= 2.0, "{tongues} tongues, expected about {expected}");
        // No flame tongue stands over the quiet hull.
        let flames = r.pool(VfxKind::FlamePrism);
        assert!(flames.instances.iter().all(|i| i.xform[3] > -100.0), "no flame was ever spawned near x = -300");
    }

    #[test]
    fn flooding_and_list_cues() {
        let mut r = Rig::new();
        let e = EntityId::from_u64(3);
        let level = vessel(3, false, 0.0, 0.0);
        r.step(&frame(0.0, vec![level.clone()]), &[reveal(3, 1)], &[]);
        assert_eq!(r.live(VfxKind::FoamBand), 0);
        r.step(&frame(1.0, vec![level.clone()]), &[ev(1, SimEvent::Phys(PhysEvent::Breach { vessel: e, compartment: CompartmentId(1), below_waterline: true }))], &[]);
        assert_eq!(r.live(VfxKind::FoamBand), 2, "a flooding hull shows its band on both sides");
        assert!(r.pool(VfxKind::FoamBand).protection.iter().filter(|p| **p == Protection::Flooding).count() == 2);
        // An unflooded hull that settles deep shows the band without any event.
        let mut r = Rig::new();
        r.step(&frame(0.0, vec![vessel(4, false, 0.0, 0.0)]), &[reveal(4, 1)], &[]);
        let mut deep = vessel(4, false, 0.0, 0.0);
        deep.pos[1] = -2.5;
        for k in 1..=600 { r.step(&frame(k as f64, vec![deep.clone()]), &[], &[]); }
        assert_eq!(r.live(VfxKind::FoamBand), 2);
    }

    #[test]
    fn trails_decoys_and_rebase() {
        let mut r = Rig::new();
        let body = |x: f32, own_side: bool| BodyPose { entity: EntityId::from_u64(40), kind: content::ids::MunitionIdx(0), pos: [x, 20.0, 0.0], vel: [250.0, 0.0, 0.0], seeker: Default::default(), track: Default::default(), own_side };
        let mut v = frame(0.0, vec![vessel(1, true, 0.0, 0.0)]);
        r.step(&v, &[reveal(1, 0)], &[]);
        for k in 1..=60 {
            v = frame(k as f64, vec![vessel(1, true, 0.0, 0.0)]);
            v.bodies = vec![body(k as f32 * 250.0 / 60.0, false)];
            r.step(&v, &[], &[]);
        }
        let trails = r.live(VfxKind::MissileTrail);
        assert!((8..=10).contains(&trails), "one segment per 0.1 s for 1 s: {trails}");
        assert!(r.pool(VfxKind::MissileTrail).protection.contains(&Protection::IncomingMissileHead));
        // Each segment carries the trail's life, so the shader ages it (spread, thin, ramp).
        let life = rt::trail_length_s(QualityTier::Medium);
        let p = r.pool(VfxKind::MissileTrail);
        assert!(p.instances.iter().zip(&p.lifetime_s).filter(|(_, l)| **l > 0.0).all(|(i, _)| i.custom[1] == life));
        // Decoys: a smoke screen is 24 puffs, a chaff cloud one; both go when the decoy leaves.
        v.decoys = vec![DecoyPose { entity: EntityId::from_u64(50), kind: DecoyKind::Smoke, pos: [100.0, 15.0, 0.0], age_s: 5.0 }, DecoyPose { entity: EntityId::from_u64(51), kind: DecoyKind::Chaff, pos: [0.0, 200.0, 100.0], age_s: 1.0 }];
        v.render_tick = 61.0;
        v.sim_time_s = 61.0 / HZ;
        r.step(&v, &[], &[]);
        assert_eq!(r.live(VfxKind::DefenceSmoke), rt::DEFENCE_SMOKE_PUFFS as usize);
        assert_eq!(r.live(VfxKind::ChaffCloud), 1);
        // The origin moves by 1 km: everything shifts with it.
        let before = r.pool(VfxKind::ChaffCloud).instances.iter().find(|i| i.xform[3] != 0.0 || i.xform[11] != 0.0).map(|i| i.xform[11]).unwrap();
        v.render_tick = 62.0;
        v.sim_time_s = 62.0 / HZ;
        v.origin_shift = Some([0.0, 0.0, -1000.0]);
        v.decoys[1].pos[2] -= 1000.0;
        v.decoys[0].pos[2] -= 1000.0;
        r.step(&v, &[], &[]);
        let after = r.pool(VfxKind::ChaffCloud).instances.iter().find(|i| i.xform[11] < -500.0).map(|i| i.xform[11]).unwrap();
        assert!((after - (before - 1000.0)).abs() < 1e-3);
        v.decoys.clear();
        v.origin_shift = None;
        r.step(&v, &[], &[]);
        assert_eq!(r.live(VfxKind::DefenceSmoke) + r.live(VfxKind::ChaffCloud), 0);
        // A new sortie forgets everything.
        r.step(&frame(0.0, vec![]), &[ev(0, SimEvent::SortieStarted { seed: 2, tick_hz: 60 })], &[]);
        assert_eq!(r.core.live(), 0);
    }

    #[test]
    fn tier_change_keeps_protected_effects_and_handles() {
        let mut r = Rig::new();
        let v = frame(0.0, vec![vessel(1, true, 0.0, 0.0)]);
        r.step(&v, &[reveal(1, 0)], &[]);
        let ends: Vec<ShellEnd> = (0..200).map(|k| ShellEnd { pos: [k as f32 * 3.0, 0.0, 200.0], calibre_mm: 76, water: true, own_side: k % 2 == 0 }).collect();
        r.step(&frame(1.0, v.vessels.clone()), &[], &ends);
        r.step_with(&frame(2.0, v.vessels.clone()), &[], &[], Lens { tier: QualityTier::Low, ..lens() });
        let p = r.pool(VfxKind::SplashColumn);
        assert_eq!(p.instances.len(), CapGroup::Splashes.cap(QualityTier::Low));
        assert!(p.live_count() <= p.cap);
        assert_eq!(r.core.light_budget, (8, 8));
    }

    #[test]
    fn smoke_cylinder_matches_the_sims_rule_and_bounds_pad() {
        let t = content::tuning::DecoyTuning::default();
        assert_eq!(smoke_cylinder(5.0, &t), Some((30.0, 15.0)));
        assert_eq!(smoke_cylinder(50.0, &t), Some((60.0, 30.0)));
        assert_eq!(smoke_cylinder(120.0, &t), None);
        assert_eq!(smoke_cylinder(f32::NAN, &t), None);
        let mut buf = vec![0.0f32; 20];
        VfxInstance { xform: rt::xform([2.0, 0.0, 0.0], [0.0, 2.0, 0.0], [0.0, 0.0, 2.0], [10.0, 5.0, 0.0]), color: [1.0; 4], custom: [0.0, 9.0, 0.5, 3.0] }.write(&mut buf);
        let (min, max) = instance_bounds(VfxKind::SmokePuff, &buf);
        assert!(min.x < 10.0 - 100.0 && max.y > 5.0 + 100.0, "drift reach is in the box: {min} {max}");
        assert_eq!(instance_bounds(VfxKind::SmokePuff, &[0.0; 20]), (Vec3::ZERO, Vec3::ZERO));
        assert!((wind_vector(&content::weather::WeatherState { wind_dir_rad: std::f64::consts::FRAC_PI_2, wind_speed_mps: 5.0, ..Default::default() }) - Vec3::new(5.0, 0.0, 0.0)).length() < 1e-4, "east is +X");
        // Vessel-local +X is the bow: unturned, it points east (+X); a quarter turn about +Y points it north.
        assert!((heading_of(&Affine3A::IDENTITY) - std::f32::consts::FRAC_PI_2).abs() < 1e-5);
        assert!(heading_of(&Affine3A::from_rotation_y(std::f32::consts::FRAC_PI_2)).abs() < 1e-5);
    }

    #[test]
    fn a_rewind_keeps_the_reveals() {
        let mut r = Rig::new();
        r.step(&frame(600.0, vec![vessel(1, true, 0.0, 10.0)]), &[reveal(1, 0)], &[]);
        r.step(&frame(601.0, vec![vessel(1, true, 0.2, 10.0)]), &[], &[]);
        assert_eq!(r.live(VfxKind::BowWave), 1);
        // A replay seek back to the start: effects go, the hull keeps its design.
        r.step(&frame(0.0, vec![vessel(1, true, 0.0, 10.0)]), &[], &[]);
        r.step(&frame(1.0, vec![vessel(1, true, 0.2, 10.0)]), &[], &[]);
        assert_eq!(r.live(VfxKind::BowWave), 1);
        assert_eq!(r.core.vessels[&EntityId::from_u64(1)].hash, ContentHash([7; 32]));
        assert!(r.core.vessels[&EntityId::from_u64(1)].design.is_some());
    }

    #[test]
    fn a_seaway_shows_no_flooding_band() {
        let mut r = Rig::new();
        // Heave 1.5 m and roll 10 degrees on 7 s and 9 s periods for a minute.
        r.step(&frame(0.0, vec![vessel(5, false, 0.0, 0.0)]), &[reveal(5, 1)], &[]);
        for k in 1..=3600 {
            let t = k as f32 / 60.0;
            let mut p = vessel(5, false, 0.0, 0.0);
            p.pos[1] = -1.0 + 1.5 * (t * std::f32::consts::TAU / 7.0).sin();
            let q = glam::Quat::from_rotation_x(10f32.to_radians() * (t * std::f32::consts::TAU / 9.0).sin());
            p.rot = [q.x, q.y, q.z, q.w];
            r.step(&frame(k as f64, vec![p]), &[], &[]);
            assert_eq!(r.live(VfxKind::FoamBand), 0, "no band at {t} s");
        }
    }

    #[test]
    fn the_muzzle_fallback_follows_the_mount() {
        // A design with one 76 mm mount turned to face aft (rotation 2 = 180 degrees about +Y).
        let mut d = box_design([80, 8, 16]);
        let part = content::ids::PartId::new("gun.76mm.single");
        assert!(content::catalogue::catalogue().part(&part).is_some());
        d.parts = vec![(PartSlot(0), content::blueprint::PartPlacement { part, pos: content::cell::CellPos::new(10, 8, 6), rot: content::kit::PartRot { yaw: 2, tilt: 0 }, kit: None, label: None, load: None })];
        let mut r = Rig { design: Arc::new(d), ..Rig::new() };
        let mut v = frame(0.0, vec![vessel(1, true, 0.0, 0.0)]);
        v.vessels[0].turrets = 0..1;
        v.turrets = vec![sim_systems::protocol::TurretView { slot: PartSlot(0), train_rad: 0.0, elevation_rad: 0.0 }];
        r.step(&v, &[reveal(1, 0)], &[]);
        let (muzzles, calibre) = r.core.muzzles(&v, EntityId::from_u64(1), PartSlot(0));
        assert_eq!(calibre, 76);
        let (pos, dir) = muzzles[0];
        assert!(dir.x < -0.99, "a mount turned aft fires aft: {dir}");
        assert!(pos.y > 3.5 && pos.x < 8.0, "on the deck at the mount: {pos}");
    }

    #[test]
    fn a_busy_frame_stays_inside_its_share() {
        // A heavy battle's worth of live effects; the frame's bookkeeping must leave room for the upload in
        // the 0.4 ms it shares with projectiles (L2007). Timed loosely: debug builds and loaded CI runners.
        let mut r = Rig::new();
        let v = frame(0.0, (0..20).map(|k| vessel(k + 1, k == 0, k as f32 * 60.0, 10.0)).collect());
        let reveals: Vec<WireEvent> = (0..20).map(|k| reveal(k + 1, (k % 2) as u8)).collect();
        r.step(&v, &reveals, &[]);
        for f in 1..=120u64 {
            let mut v = frame(f as f64, v.vessels.clone());
            for p in &mut v.vessels { p.pos[0] += f as f32 / 6.0; p.flags = VesselFlags::BURNING; }
            let ends: Vec<ShellEnd> = (0..6).map(|k| ShellEnd { pos: [k as f32 * 40.0 + f as f32, 0.0, 300.0], calibre_mm: 127, water: true, own_side: k % 2 == 0 }).collect();
            r.step(&v, &[], &ends);
        }
        let live = r.core.live();
        assert!(live > 1_500, "{live}");
        let t = std::time::Instant::now();
        let mut v = frame(121.0, v.vessels.clone());
        for p in &mut v.vessels { p.flags = VesselFlags::BURNING; }
        for k in 0..20 { v.render_tick = 121.0 + k as f64; v.sim_time_s = v.render_tick / HZ; r.step(&v, &[], &[]); }
        let per_frame_us = t.elapsed().as_micros() / 20;
        println!("vfx core: {live} live, {per_frame_us} us per frame");
        assert!(per_frame_us < 4_000, "{per_frame_us} us");
    }

}
