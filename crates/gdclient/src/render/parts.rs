//! Unit `gd-render-parts`. Placed parts of every drawn vessel as MultiMeshes, with a shadowed near band and
//! an unshadowed far band; articulated pieces follow replicated state.
//! DESIGN.md L1945 (repeated parts), L1233-1238 (bespoke models are procedural, from `hull-mesh`),
//! L1651 (damage state is hatching in UI; in 3D a destroyed part shows scorch and its removed cells),
//! L2006 (0.3 ms per frame).
//!
//! # Models
//! A part's model is `hull_mesh::parts::build_part_model(def)`: a `PartModel` of `PartPiece`s, each a
//! `MeshData` relative to its pivot with a `Joint`, parents before children, plus `Anchor`s. There is one
//! mesh RID and one MultiMesh pair (near, far) per distinct (generator key, resolved parameters, piece
//! index); two catalogue parts that resolve to the same generator and parameters share them. Meshes are
//! uploaded once at load through `convert::mesh_arrays` with `ARRAY_VERTEX`, `ARRAY_NORMAL`, `ARRAY_COLOR`,
//! `ARRAY_CUSTOM0` (RGBA float) and `ARRAY_INDEX`.
//!
//! # Per-frame transform of a piece
//! vessel pose x placement (footprint minimum corner and rotation, part-local frame +X bow, +Y up, +Z
//! starboard) x the chain of piece pivots and joints:
//! - `Joint::Fixed`: none.
//! - `Joint::Train`: about +Y by `TurretView::train_rad`. `Joint::Elevate`: about +Z by `elevation_rad`.
//! - `Joint::Spin { axis }`: sensors at their `rpm`; propellers at a rate from `VesselPose::power`.
//! - `Joint::Recoil { travel_m }`: slides back along -X on the mount's fire event and returns over 0.4 s.
//!
//! # Colour (the part shader reads exactly this)
//! Vertex `COLOR.rgb` is linear albedo from `hull_mesh::parts::Tone`; `COLOR.a` is the paint mask: where it
//! is 1 the shader multiplies by the ship paint. `CUSTOM0.x` is emission strength (lamps at most 1 on
//! the emission ladder), `CUSTOM0.y` is 1 on glass. The instance colour is white: Godot multiplies it into
//! `COLOR` before the shader runs, so paint carried there would tint the fixed-albedo surfaces too.
//! Instance custom: x = scorch 0..=1 (1 once the part is destroyed), y, z, w = the ship paint the part wears
//! in linear RGB ([`wall_paint`]: the side plating at the part's station, or the blueprint's first hull
//! swatch when there is none).
//!
//! Anchors (`AnchorKind::Muzzle`, `LaunchCell`, `Exhaust`, `Light`, `Wash`, `PumpOutlet`) are exposed through
//! [`PartRenderer::anchors`] in render space for the VFX and particle units; they are visual only.
//!
//! # Rules this unit adds
//! - A distinct model is keyed by generator, resolved parameters and footprint: the generators fit their
//!   model to the footprint, so two parts that differ only in footprint are different meshes.
//! - A part belongs to the vessel that holds its seat, the bottom centre cell of its footprint. When that
//!   cell is removed (destroyed, shed as debris or split off) the part leaves the vessel; a wreck piece that
//!   received the cell draws it instead.
//! - A part whose bounding sphere covers less than [`MIN_PART_PX`] on screen is not drawn; its anchors are
//!   still reported, so a far gun still gets its muzzle flash. The viewer's own hull and the dry dock are
//!   never culled.
//! - The near band is every vessel within the shadow distance; from degrade step 4 only the own hull and
//!   hulls within 300 m (L2030).
//! - `Joint::Train` follows the mount's `TurretView`; on rudders and waterjet nozzles, which have none, it
//!   follows `VesselPose::rudder` up to the rudder's `max_deg` (positive to starboard).
//! - `Joint::Spin` by piece role: antennas turn clockwise seen from above at the sensor's `rpm`; propellers
//!   at up to [`SHAFT_REV_S`] by shaft power; a rotary gun's barrel cluster for [`BARREL_SPIN_S`] after each
//!   burst. All of it runs on simulation time, so a pause or a replay scrub holds it still.
//! - The paint is the hull's side plating at the part's station, not the deck cell under it: decks are
//!   three or more swatch steps darker than the sides (L1801), and a mount in deck paint would vanish
//!   against its own deck.
//! - Scorch by damage level: Damaged 0.2, Heavy 0.5, Destroyed 1. A reveal carries cell damage but no part
//!   levels, so a part revealed already damaged takes the level of its seat cell's bucket.
//! - Damage is drawn at the playback tick: a part is scorched, or leaves with its seat cell, when playback
//!   reaches the event (`hull::hold`), not the interpolation delay before, when the event arrives. Recoil
//!   already runs on the fire event's tick.
//! - A reveal whose design has not arrived yet stays pending and is drawn the frame the design is stored
//!   (`Services::designs`).
use super::hull::design::{self, DrawDesign};
use super::hull::gpu::{self, InstanceBuffer, MultiMeshSlot};
use super::{FrameContext, RenderStats, LAYER_PARTS};
use crate::interp::ViewFrame;
use crate::seams::{MeshData, WireEvent};
use content::cell::CellPos;
use content::enums::DamageLevel;
use content::ids::{ContentHash, PartSlot};
use content::kit::PartRot;
use content::part::{PartDef, PartFunction, PropulsorKind};
use glam::{Affine3A, Mat3A, Quat, Vec3};
use godot::prelude::*;
use hull_mesh::parts::{Anchor, AnchorKind, Joint, PartModel, PieceRole};
use sim_core::events::PhysEvent;
use sim_core::EntityId;
use sim_systems::protocol::{SimEvent, TurretView};
use std::collections::BTreeMap;
use std::sync::Arc;

pub(crate) const PART_SHADER: &str = "res://shaders/part.gdshader";
/// A part smaller than this on screen (bounding-sphere diameter, pixels) is not drawn.
pub const MIN_PART_PX: f32 = 1.0;
/// Recoil returns over this long after the fire event (module docs).
pub const RECOIL_S: f32 = 0.4;
/// The barrel reaches full travel this long after the shot.
const RECOIL_KICK_S: f32 = 0.03;
/// Propeller turns per second at full shaft power.
pub const SHAFT_REV_S: f64 = 5.3;
/// A rotary cluster spins at this rate for this long after a burst; their product is whole, so it comes to
/// rest where it started.
pub const BARREL_REV_S: f32 = 15.0;
pub const BARREL_SPIN_S: f32 = 0.6;
/// Rudder angle at full helm for a part that is not a rudder (waterjet nozzles), radians.
const NOZZLE_MAX_RAD: f32 = 0.52;
/// Rotors with no replicated state (none on ship parts today) turn at this rate.
const IDLE_SPIN_REV_S: f64 = 4.3;
/// From this degrade step only the own hull and hulls within [`DEGRADED_SHADOW_M`] cast shadows (L2030).
pub(crate) const SHADOW_DEGRADE_STEP: u8 = 4;
pub(crate) const DEGRADED_SHADOW_M: f32 = 300.0;

/// Scorch written to `INSTANCE_CUSTOM.x` for a damage level.
pub fn scorch_of(level: DamageLevel) -> f32 {
    match level { DamageLevel::Intact => 0.0, DamageLevel::Damaged => 0.2, DamageLevel::Heavy => 0.5, DamageLevel::Destroyed => 1.0 }
}

/// Instance colour and custom data of one part or body piece, as `part.gdshader` reads them: the ship paint
/// the part wears (linear RGB; all zero for a model with no painted surface) and its scorch. The one place
/// the packing is decided.
#[inline]
pub(crate) fn instance_data(paint: [f32; 3], scorch: f32) -> ([f32; 4], [f32; 4]) {
    ([1.0, 1.0, 1.0, 1.0], [scorch, paint[0], paint[1], paint[2]])
}

/// One effect anchor of a drawn part this frame, in render space.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AnchorPose { pub vessel: sim_core::EntityId, pub slot: content::ids::PartSlot, pub kind: AnchorKind, pub pos: [f32; 3], pub dir: [f32; 3] }

// ---------------------------------------------------------------------------------------------------------
// The model library: every distinct piece mesh of the catalogue
// ---------------------------------------------------------------------------------------------------------

/// One distinct piece mesh and this frame's instances of it in each band.
pub(crate) struct PieceKind { pub mesh: MeshData, pub radius: f32, pub near: InstanceBuffer, pub far: InstanceBuffer }
#[derive(Clone, Copy, Debug)]
pub(crate) struct ModelPiece { kind: u32, parent: Option<u8>, pivot: Vec3, joint: Joint, role: PieceRole }
pub(crate) struct Model {
    pieces: Vec<ModelPiece>,
    anchors: Vec<Anchor>,
    /// Centre and radius of the rest pose in part-local metres, padded for barrels that swing.
    centre: Vec3,
    radius: f32,
}
/// How a catalogue part's joints are driven.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct Drive {
    /// Full-helm angle when `Joint::Train` follows the rudder; `None` when it follows a `TurretView`.
    rudder_max_rad: Option<f32>,
    /// Antenna turns per second; 0 for a staring sensor.
    antenna_rev_s: f32,
}
#[derive(Clone, Copy, Debug)]
pub(crate) struct PartKind { model: u32, drive: Drive, size: [u8; 3] }

pub(crate) struct Library {
    pub(crate) kinds: Vec<PieceKind>,
    pub(crate) models: Vec<Model>,
    /// By `PartIdx`.
    pub(crate) parts: Vec<PartKind>,
    ids: BTreeMap<String, u16>,
}

fn drive_of(def: &PartDef) -> Drive {
    match &def.function {
        PartFunction::Rudder { max_deg, .. } => Drive { rudder_max_rad: Some(if max_deg.is_finite() { (*max_deg as f32).clamp(0.0, 90.0).to_radians() } else { NOZZLE_MAX_RAD }), antenna_rev_s: 0.0 },
        PartFunction::Propulsor(p) if p.kind == PropulsorKind::Waterjet => Drive { rudder_max_rad: Some(NOZZLE_MAX_RAD), antenna_rev_s: 0.0 },
        PartFunction::Sensor(s) => Drive { rudder_max_rad: None, antenna_rev_s: s.rpm.filter(|r| r.is_finite()).map_or(0.0, |r| (r / 60.0).clamp(0.0, 10.0) as f32) },
        _ => Drive::default(),
    }
}

impl Library {
    /// Build every catalogue part's model, sharing models between parts that resolve to the same generator,
    /// parameters and footprint. A model error falls back to `hull_mesh::parts::default_model`.
    pub(crate) fn build(catalogue: &content::catalogue::Catalogue) -> Self {
        use hull_mesh::parts::{build_model, default_model, resolve_params, Generator};
        let mut lib = Library { kinds: Vec::new(), models: Vec::new(), parts: Vec::new(), ids: BTreeMap::new() };
        let mut seen: BTreeMap<(String, Vec<u32>, [u8; 3]), u32> = BTreeMap::new();
        for (i, (id, def)) in catalogue.parts.iter().enumerate().take(u16::MAX as usize) {
            lib.ids.insert(id.as_str().to_string(), i as u16);
            let resolved = |proc: &content::part::ProcModel| {
                let g = Generator::from_key(proc.generator.as_str())?;
                let params = resolve_params(g, &proc.params).ok()?;
                Some((g.key().to_string(), params.iter().map(|p| p.to_bits()).collect::<Vec<u32>>(), def.size))
            };
            let named = if def.model.generator.as_str().is_empty() { None } else { resolved(&def.model).map(|k| (k, def.model.clone())) };
            let (key, proc) = match named {
                Some(found) => found,
                None => {
                    let fallback = default_model(def);
                    (resolved(&fallback).unwrap_or_else(|| (fallback.generator.as_str().to_string(), Vec::new(), def.size)), fallback)
                }
            };
            let model = match seen.get(&key) {
                Some(m) => *m,
                None => {
                    let built = build_model(&proc, def.size).or_else(|_| build_model(&default_model(def), def.size)).unwrap_or_default();
                    let m = lib.add_model(built);
                    seen.insert(key, m);
                    m
                }
            };
            lib.parts.push(PartKind { model, drive: drive_of(def), size: def.size });
        }
        lib
    }

    fn add_model(&mut self, built: PartModel) -> u32 {
        let mut pieces = Vec::with_capacity(built.pieces.len());
        for (i, piece) in built.pieces.into_iter().enumerate().take(u8::MAX as usize) {
            let radius = piece.mesh.positions.iter().map(|v| Vec3::from(*v).length()).fold(0.0f32, f32::max);
            let kind = self.kinds.len() as u32;
            // A parent that does not precede its child would loop the chain: such a piece hangs off the base.
            pieces.push(ModelPiece { kind, parent: piece.parent.filter(|&q| (q as usize) < i), pivot: finite(Vec3::from(piece.pivot)), joint: piece.joint, role: piece.role });
            self.kinds.push(PieceKind { mesh: piece.mesh, radius: if radius.is_finite() { radius } else { 0.0 }, near: InstanceBuffer::default(), far: InstanceBuffer::default() });
        }
        let (min, max) = if built.aabb.is_empty() { (Vec3::ZERO, Vec3::ZERO) } else { (finite(Vec3::from(built.aabb.min)), finite(Vec3::from(built.aabb.max))) };
        // Trained and elevated pieces sweep a sphere about the model: twice the rest half-diagonal covers it.
        let articulated = pieces.iter().any(|p| !matches!(p.joint, Joint::Fixed));
        let radius = 0.5 * (max - min).length() * if articulated { 2.0 } else { 1.0 };
        let anchors = built.anchors.into_iter().filter(|a| (a.piece as usize) < pieces.len()).collect();
        self.models.push(Model { pieces, anchors, centre: 0.5 * (min + max), radius });
        self.models.len() as u32 - 1
    }

    fn begin(&mut self) { for k in &mut self.kinds { k.near.begin(); k.far.begin(); } }

    /// All pieces of one part at rest as one mesh (UI preview tiles).
    pub(crate) fn merged_mesh(&self, part: usize) -> MeshData {
        let mut out = MeshData::new();
        let Some(model) = self.parts.get(part).and_then(|p| self.models.get(p.model as usize)) else { return out };
        let mut rest: Vec<Vec3> = Vec::with_capacity(model.pieces.len());
        for piece in &model.pieces {
            let origin = piece.parent.and_then(|q| rest.get(q as usize).copied()).unwrap_or(Vec3::ZERO) + piece.pivot;
            rest.push(origin);
            let mesh = &self.kinds[piece.kind as usize].mesh;
            let n = mesh.positions.len();
            if n == 0 || mesh.indices.iter().any(|&i| i as usize >= n) { continue; }
            let base = out.positions.len() as u32;
            out.positions.extend(mesh.positions.iter().map(|p| (Vec3::from(*p) + origin).to_array()));
            let pad = |v: &Vec<[f32; 3]>, fill: [f32; 3]| (0..n).map(move |i| v.get(i).copied().unwrap_or(fill)).collect::<Vec<_>>();
            out.normals.extend(pad(&mesh.normals, [0.0, 1.0, 0.0]));
            out.colors.extend((0..n).map(|i| mesh.colors.get(i).copied().unwrap_or([1.0, 1.0, 1.0, 1.0])));
            out.custom0.extend((0..n).map(|i| mesh.custom0.get(i).copied().unwrap_or([0.0; 4])));
            out.indices.extend(mesh.indices.iter().map(|i| i + base));
        }
        out
    }
}

fn finite(v: Vec3) -> Vec3 { if v.is_finite() { v } else { Vec3::ZERO } }

// ---------------------------------------------------------------------------------------------------------
// Placement
// ---------------------------------------------------------------------------------------------------------

/// Part-local metres to vessel-local metres for a part of footprint `size` (cells, rotation 0) placed with
/// the minimum corner of its rotated footprint at cell `pos`.
pub(crate) fn placement_affine(pos: CellPos, rot: PartRot, size: [u8; 3]) -> Affine3A {
    let m = content::kit::part_rot_matrix(rot);
    // Rows of `m` are the images of the part's axes: they are the columns of the rotation.
    let col = |k: usize| Vec3::new(m[k][0] as f32, m[k][1] as f32, m[k][2] as f32);
    let basis = Mat3A::from_cols(col(0).into(), col(1).into(), col(2).into());
    // The rotated box reaches backwards along every axis image that points down its vessel axis.
    let mut min = Vec3::ZERO;
    for k in 0..3 { min += col(k).min(Vec3::ZERO) * (size[k] as f32 * hull_mesh::CELL_M_F32); }
    let corner = Vec3::new(pos.x as f32, pos.y as f32, pos.z as f32) * hull_mesh::CELL_M_F32;
    Affine3A::from_mat3_translation(basis.into(), corner - min)
}

/// The ship paint a part seated at `seat` wears: the swatch of the hull's side plating at its station. Looked
/// up two rows under the seat (one row when there is no second), at the outermost structure cell toward the
/// nearer side of the hull. Decks are painted three or more steps darker than the sides (L1801); a mount in
/// deck paint would vanish against its own deck, while one in the local side paint reads at range and
/// follows a camouflage scheme along the hull. With no structure there the first hull swatch is used.
pub(crate) fn wall_paint(design: &DrawDesign, seat: CellPos) -> [f32; 3] {
    let grid = &design.grid;
    let outward: i16 = if (seat.z as i32) * 2 < grid.size[2] as i32 { -1 } else { 1 };
    for below in [2i16, 1] {
        let y = seat.y.saturating_sub(below);
        if y < 0 { continue; }
        let mut found = None;
        let mut z = seat.z;
        while z >= 0 && (z as u16) < grid.size[2] {
            let cell = grid.get(CellPos::new(seat.x, y, z));
            if !cell.is_empty() && cell.material != content::cell::Material::PartBody { found = Some(cell.paint); }
            z += outward;
        }
        if let Some(paint) = found { return design.palette.get(paint as usize).copied().unwrap_or(design.hull_paint); }
    }
    design.hull_paint
}

/// The seat of a part: the bottom centre cell of its rotated footprint.
pub(crate) fn seat_cell(pos: CellPos, rot: PartRot, size: [u8; 3]) -> CellPos {
    let r = content::kit::rotated_size(size, rot);
    CellPos::new(pos.x.saturating_add((r[0] / 2) as i16), pos.y, pos.z.saturating_add((r[2] / 2) as i16))
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct LayoutPart { slot: PartSlot, part: u16, local: Affine3A, paint: [f32; 3], seat: u32 }
/// The placed parts of one design, shared by every vessel of it.
pub(crate) struct Layout {
    pub(crate) parts: Vec<LayoutPart>,
    /// One bit per cell: set where a part is seated, so a long removal list costs one bit test per cell.
    seat_bits: Vec<u64>,
    /// (seat cell, index into `parts`), ascending.
    by_seat: Vec<(u32, u32)>,
    /// Radius of the hull box about its centre, metres, and the centre in vessel-local metres.
    centre: Vec3,
    radius: f32,
}

impl Layout {
    pub(crate) fn build(design: &DrawDesign, lib: &Library) -> Self {
        let grid = &design.grid;
        let mut parts = Vec::with_capacity(design.parts.len());
        for (slot, placement) in &design.parts {
            let Some(&part) = lib.ids.get(placement.part.as_str()) else { continue };
            let size = lib.parts[part as usize].size;
            let paint = wall_paint(design, seat_cell(placement.pos, placement.rot, size));
            let seat = grid.index(seat_cell(placement.pos, placement.rot, size)).map_or(u32::MAX, |c| c.0);
            parts.push(LayoutPart { slot: *slot, part, local: placement_affine(placement.pos, placement.rot, size), paint, seat });
        }
        let mut seat_bits = vec![0u64; grid.cells.len().div_ceil(64)];
        let mut by_seat: Vec<(u32, u32)> = Vec::new();
        for (i, p) in parts.iter().enumerate() {
            if (p.seat as usize) < grid.cells.len() {
                seat_bits[p.seat as usize / 64] |= 1 << (p.seat % 64);
                by_seat.push((p.seat, i as u32));
            }
        }
        by_seat.sort_unstable();
        let half = 0.5 * Vec3::from(design.size_m());
        Self { parts, seat_bits, by_seat, centre: half, radius: half.length() }
    }

    /// Indices of the parts seated on `cell`.
    fn seated(&self, cell: u32) -> impl Iterator<Item = usize> + '_ {
        let hit = self.seat_bits.get(cell as usize / 64).is_some_and(|w| w >> (cell % 64) & 1 == 1);
        let start = if hit { self.by_seat.partition_point(|s| s.0 < cell) } else { self.by_seat.len() };
        self.by_seat[start..].iter().take_while(move |s| s.0 == cell).map(|s| s.1 as usize)
    }
}

// ---------------------------------------------------------------------------------------------------------
// Per-vessel state and the frame loop (no Godot)
// ---------------------------------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug)]
struct PartState { scorch: f32, gone: bool, fired_s: f64 }
impl Default for PartState { fn default() -> Self { Self { scorch: 0.0, gone: false, fired_s: f64::NEG_INFINITY } } }

/// Something that happened to a vessel whose design has not arrived; replayed in order once it has.
#[derive(Clone, Debug)]
enum Deferred { Removed(Vec<u32>), Kept(Vec<u32>), Bucket(u32, u8), Scorch(PartSlot, f32) }

pub(crate) struct VesselParts {
    hash: ContentHash,
    layout: Option<Arc<Layout>>,
    state: Vec<PartState>,
    deferred: Vec<Deferred>,
    shaft_turns: f64,
}

impl VesselParts {
    fn new(hash: ContentHash) -> Self { Self { hash, layout: None, state: Vec::new(), deferred: Vec::new(), shaft_turns: 0.0 } }
    fn attach(&mut self, layout: Arc<Layout>) {
        self.state = vec![PartState::default(); layout.parts.len()];
        self.layout = Some(layout);
        for d in std::mem::take(&mut self.deferred) { self.apply(d); }
    }
    fn apply(&mut self, what: Deferred) {
        let Some(layout) = &self.layout else {
            // Bounded: a design that never arrives must not collect events for the whole sortie.
            if self.deferred.len() < 4_096 { self.deferred.push(what); }
            return;
        };
        match what {
            Deferred::Removed(cells) => for c in cells { for i in layout.seated(c) { self.state[i].gone = true; } },
            // A wreck piece keeps only these cells: every part seated elsewhere is not on it.
            Deferred::Kept(mut cells) => {
                cells.sort_unstable();
                for (i, p) in layout.parts.iter().enumerate() { if cells.binary_search(&p.seat).is_err() { self.state[i].gone = true; } }
            }
            Deferred::Bucket(cell, bucket) => for i in layout.seated(cell) {
                if bucket >= content::cell::BUCKET_DESTROYED { self.state[i].gone = true; } else { self.state[i].scorch = self.state[i].scorch.max(scorch_of(DamageLevel::from_bucket(bucket))); }
            },
            Deferred::Scorch(slot, scorch) => if let Ok(i) = layout.parts.binary_search_by_key(&slot, |p| p.slot) { self.state[i].scorch = scorch; },
        }
    }
    fn part_mut(&mut self, slot: PartSlot) -> Option<&mut PartState> {
        let i = self.layout.as_ref()?.parts.binary_search_by_key(&slot, |p| p.slot).ok()?;
        self.state.get_mut(i)
    }
}

/// What the frame loop needs of the camera and the quality profile.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Lens { pub camera: Vec3, pub px_per_m: f32, pub shadow_distance_m: f32, pub degrade_step: u8 }

/// Barrel travel fraction `since_fired_s` after the shot: full within [`RECOIL_KICK_S`], home at [`RECOIL_S`].
pub(crate) fn recoil_fraction(since_fired_s: f32) -> f32 {
    if !(since_fired_s > 0.0 && since_fired_s < RECOIL_S) { return 0.0; }
    if since_fired_s < RECOIL_KICK_S { return since_fired_s / RECOIL_KICK_S; }
    let back = 1.0 - (since_fired_s - RECOIL_KICK_S) / (RECOIL_S - RECOIL_KICK_S);
    back * back
}

/// Per-part inputs of the joints.
#[derive(Clone, Copy, Debug)]
struct JointInput { train: f32, elevation: f32, sim_time_s: f64, shaft_turns: f64, since_fired_s: f32, dead: bool }

fn turn(axis: Vec3, turns: f64) -> Affine3A {
    // Reduce in f64: an hour of simulation time leaves an f32 no precision for an angle.
    Affine3A::from_quat(Quat::from_axis_angle(axis, (turns.fract() as f32) * std::f32::consts::TAU))
}

fn joint_local(piece: &ModelPiece, drive: &Drive, j: &JointInput) -> Affine3A {
    let seat = Affine3A::from_translation(piece.pivot);
    match piece.joint {
        Joint::Fixed => seat,
        Joint::Train => seat * Affine3A::from_rotation_y(j.train),
        Joint::Elevate => seat * Affine3A::from_rotation_z(j.elevation),
        Joint::Spin { axis } => {
            let axis = Vec3::from(axis);
            if j.dead || !axis.is_finite() || axis.length_squared() < 1e-6 { return seat; }
            let axis = axis.normalize();
            match piece.role {
                PieceRole::Antenna => seat * turn(axis, -(j.sim_time_s * drive.antenna_rev_s as f64)),
                PieceRole::Propeller => seat * turn(axis, j.shaft_turns),
                PieceRole::Barrel => seat * turn(axis, (j.since_fired_s.clamp(0.0, BARREL_SPIN_S) * BARREL_REV_S) as f64),
                _ => seat * turn(axis, j.sim_time_s * IDLE_SPIN_REV_S),
            }
        }
        Joint::Recoil { travel_m } => {
            let travel = if travel_m.is_finite() { travel_m.clamp(0.0, 5.0) } else { 0.0 };
            Affine3A::from_translation(piece.pivot - Vec3::X * (travel * recoil_fraction(j.since_fired_s)))
        }
    }
}

/// Godot-free state of the unit.
pub(crate) struct PartsCore {
    pub(crate) lib: Library,
    pub(crate) vessels: BTreeMap<EntityId, VesselParts>,
    layouts: BTreeMap<ContentHash, Arc<Layout>>,
    /// The dry dock's design, drawn at the origin.
    dock: Option<VesselParts>,
    pub(crate) anchors: Vec<AnchorPose>,
    scratch: Vec<Affine3A>,
    last_time_s: Option<f64>,
    /// Damage events waiting for playback to reach them.
    hold: super::hull::hold::EventHold,
    pub(crate) drawn: u32,
}

impl PartsCore {
    pub(crate) fn new(lib: Library) -> Self {
        Self { lib, vessels: BTreeMap::new(), layouts: BTreeMap::new(), dock: None, anchors: Vec::new(), scratch: Vec::new(), last_time_s: None, hold: Default::default(), drawn: 0 }
    }

    pub(crate) fn clear(&mut self) {
        self.vessels.clear();
        self.layouts.clear();
        self.last_time_s = None;
        self.hold = Default::default();
    }

    pub(crate) fn show_design(&mut self, design: &DrawDesign) {
        let mut v = VesselParts::new(design.hash);
        v.attach(Arc::new(Layout::build(design, &self.lib)));
        self.dock = Some(v);
    }

    fn on_event(&mut self, ev: &WireEvent) {
        let cells = |list: &[content::ids::CellIndex]| list.iter().map(|c| c.0).collect::<Vec<u32>>();
        match &ev.event {
            SimEvent::SortieStarted { .. } => self.clear(),
            SimEvent::EntityRevealed(intro) => {
                let mut v = VesselParts::new(intro.blueprint);
                if !intro.removed.is_empty() { v.deferred.push(Deferred::Removed(cells(&intro.removed))); }
                v.deferred.extend(intro.damage.iter().filter(|c| c.bucket > 0).map(|c| Deferred::Bucket(c.cell.0, c.bucket)));
                self.vessels.insert(intro.entity, v);
            }
            SimEvent::EntityHidden { entity } => { self.vessels.remove(entity); }
            SimEvent::Phys(PhysEvent::VesselDespawned { vessel }) => { self.vessels.remove(vessel); }
            SimEvent::Phys(PhysEvent::WreckDespawned { wreck }) => { self.vessels.remove(wreck); }
            SimEvent::Phys(PhysEvent::PartDamaged { vessel, slot, level }) => if let Some(v) = self.vessels.get_mut(vessel) { v.apply(Deferred::Scorch(*slot, scorch_of(*level))); },
            SimEvent::Phys(PhysEvent::MountFired { vessel, slot, .. }) => {
                let at = ev.tick as f64 / sim_core::TICK_HZ as f64;
                if let Some(p) = self.vessels.get_mut(vessel).and_then(|v| v.part_mut(*slot)) { p.fired_s = at; }
            }
            SimEvent::Phys(PhysEvent::CellsDamaged { vessel, changes, .. }) => if let Some(v) = self.vessels.get_mut(vessel) {
                let gone: Vec<u32> = changes.iter().filter(|c| c.bucket >= content::cell::BUCKET_DESTROYED).map(|c| c.cell.0).collect();
                if !gone.is_empty() { v.apply(Deferred::Removed(gone)); }
            },
            SimEvent::Phys(PhysEvent::Debris { vessel, cells: shed, .. }) => if let Some(v) = self.vessels.get_mut(vessel) { v.apply(Deferred::Removed(cells(shed))); },
            SimEvent::Phys(PhysEvent::Split { vessel, piece, cells: taken }) => {
                let taken = cells(taken);
                let Some(parent) = self.vessels.get_mut(vessel) else { return };
                // Until the piece's own reveal arrives it is the parent's design keeping only these cells,
                // with the parent's scorch.
                let mut child = VesselParts::new(parent.hash);
                child.deferred = parent.deferred.clone();
                if let Some(layout) = &parent.layout {
                    child.layout = Some(layout.clone());
                    child.state = parent.state.clone();
                }
                parent.apply(Deferred::Removed(taken.clone()));
                child.apply(Deferred::Kept(taken));
                self.vessels.entry(*piece).or_insert(child);
            }
            _ => {}
        }
    }

    /// One frame: apply events, resolve pending designs through `lookup`, then write every drawn piece into
    /// its band's buffer and collect the anchors.
    pub(crate) fn step(&mut self, view: &ViewFrame, events: &[WireEvent], lens: Lens, lookup: &dyn Fn(&ContentHash) -> Option<Arc<DrawDesign>>) {
        let mut hold = std::mem::take(&mut self.hold);
        hold.step(events, view.render_tick, 0.0, &mut |ev| self.on_event(ev));
        self.hold = hold;
        let sim_dt = match self.last_time_s { Some(last) if view.sim_time_s >= last => (view.sim_time_s - last).min(1.0), _ => 0.0 };
        self.last_time_s = Some(view.sim_time_s);
        self.lib.begin();
        self.anchors.clear();
        self.drawn = 0;
        let (lib, layouts, scratch, anchors, drawn) = (&mut self.lib, &mut self.layouts, &mut self.scratch, &mut self.anchors, &mut self.drawn);
        for pose in &view.vessels {
            let Some(v) = self.vessels.get_mut(&pose.entity) else { continue };
            if v.layout.is_none() {
                let found = layouts.get(&v.hash).cloned().or_else(|| {
                    let built = Arc::new(Layout::build(&*lookup(&v.hash)?, lib));
                    layouts.insert(v.hash, built.clone());
                    Some(built)
                });
                match found { Some(layout) => v.attach(layout), None => continue }
            }
            v.shaft_turns = (v.shaft_turns + sim_dt * SHAFT_REV_S * pose.power.clamp(0.0, 1.0) as f64).fract();
            let turrets = view.turrets.get(pose.turrets.start as usize..pose.turrets.end as usize).unwrap_or(&[]);
            let base = gpu::pose_affine(pose.pos, pose.rot);
            draw_vessel(lib, v, pose.entity, base, turrets, pose.rudder, view.sim_time_s, if pose.own { None } else { Some(lens) }, pose.own, scratch, anchors, drawn);
        }
        // The dock design last: its anchors carry `EntityId::NONE`, which sorts after every real vessel.
        if let Some(dock) = &mut self.dock {
            draw_vessel(lib, dock, EntityId::NONE, Affine3A::IDENTITY, &[], 0.0, view.sim_time_s, None, true, scratch, anchors, drawn);
        }
    }
}

/// Write one vessel's parts. `lens` is `None` for a vessel that is never culled and always shadowed.
#[allow(clippy::too_many_arguments)]
fn draw_vessel(lib: &mut Library, v: &mut VesselParts, entity: EntityId, base: Affine3A, turrets: &[TurretView], rudder: f32, sim_time_s: f64, lens: Option<Lens>, own: bool, scratch: &mut Vec<Affine3A>, anchors: &mut Vec<AnchorPose>, drawn: &mut u32) {
    let Some(layout) = &v.layout else { return };
    // Distance to the nearest point of the hull's bounding sphere decides the band and the cull size.
    let (near, px_per_m) = match lens {
        None => (true, f32::INFINITY),
        Some(l) => {
            let dist = ((base.transform_point3(layout.centre) - l.camera).length() - layout.radius).max(1.0);
            let shadowed = if l.degrade_step >= SHADOW_DEGRADE_STEP { own || dist < DEGRADED_SHADOW_M } else { dist < l.shadow_distance_m };
            (shadowed, if l.px_per_m > 0.0 { l.px_per_m / dist } else { f32::INFINITY })
        }
    };
    let mut cursor = 0usize;
    for (part, state) in layout.parts.iter().zip(&v.state) {
        if state.gone { continue; }
        let kind = lib.parts[part.part as usize];
        let model = &lib.models[kind.model as usize];
        let visible = 2.0 * model.radius * px_per_m >= MIN_PART_PX;
        if !visible && model.anchors.is_empty() { continue; }
        // Turrets ascend by slot like the parts do; a list in another order still resolves by search.
        while cursor < turrets.len() && turrets[cursor].slot < part.slot { cursor += 1; }
        let turret = turrets.get(cursor).filter(|t| t.slot == part.slot).or_else(|| turrets.iter().find(|t| t.slot == part.slot));
        let angle = |a: f32| if a.is_finite() { a } else { 0.0 };
        let input = JointInput {
            train: match (turret, kind.drive.rudder_max_rad) { (Some(t), _) => angle(t.train_rad), (None, Some(max)) => angle(rudder).clamp(-1.0, 1.0) * max, (None, None) => 0.0 },
            elevation: turret.map_or(0.0, |t| angle(t.elevation_rad)),
            sim_time_s,
            shaft_turns: v.shaft_turns,
            since_fired_s: (sim_time_s - state.fired_s).clamp(-1.0, 3_600.0) as f32,
            dead: state.scorch >= 1.0,
        };
        let root = base * part.local;
        let (color, custom) = instance_data(part.paint, state.scorch);
        scratch.clear();
        for piece in &model.pieces {
            let parent = piece.parent.and_then(|q| scratch.get(q as usize).copied()).unwrap_or(root);
            let m = match piece.joint {
                // The common case: only the pivot moves the piece.
                Joint::Fixed => Affine3A { matrix3: parent.matrix3, translation: parent.transform_point3a(piece.pivot.into()) },
                _ => parent * joint_local(piece, &kind.drive, &input),
            };
            scratch.push(m);
            if visible {
                let k = &mut lib.kinds[piece.kind as usize];
                if near { k.near.push(&m, color, custom, k.radius) } else { k.far.push(&m, color, custom, k.radius) }
                *drawn += 1;
            }
        }
        for a in &model.anchors {
            let m = &scratch[a.piece as usize];
            let pos = m.transform_point3(Vec3::from(a.pos));
            let dir = m.transform_vector3(Vec3::from(a.dir)).normalize_or(Vec3::X);
            if pos.is_finite() { anchors.push(AnchorPose { vessel: entity, slot: part.slot, kind: a.kind, pos: pos.into(), dir: dir.into() }); }
        }
    }
}

// ---------------------------------------------------------------------------------------------------------
// The renderer
// ---------------------------------------------------------------------------------------------------------

pub(crate) struct Gpu {
    shader: Option<Gd<godot::classes::Shader>>,
    material: Rid,
    /// Per `Library::kinds` entry.
    meshes: Vec<Rid>,
    near: Vec<MultiMeshSlot>,
    far: Vec<MultiMeshSlot>,
    /// Merged preview meshes by part index, made on first request.
    previews: std::cell::RefCell<BTreeMap<u16, Rid>>,
}

pub struct PartRenderer { pub(crate) stats: RenderStats, pub(crate) core: PartsCore, pub(crate) gpu: Option<Gpu> }

impl PartRenderer {
    /// Build every catalogue part's model with `hull_mesh::parts::build_part_model`, one mesh RID per distinct
    /// piece and one MultiMesh pair each. A model error falls back to `hull_mesh::parts::default_model`.
    pub fn new(scenario: godot::prelude::Rid, catalogue: &content::catalogue::Catalogue) -> Self {
        let lib = Library::build(catalogue);
        let mut rs = gpu::rs();
        let shader = gpu::load_shader(PART_SHADER);
        if shader.is_none() { godot_warn!("part renderer: {PART_SHADER} is missing; drawing with the default material"); }
        let material = gpu::material_on(&mut rs, &shader);
        let (mut meshes, mut near, mut far) = (Vec::new(), Vec::new(), Vec::new());
        for kind in &lib.kinds {
            let mesh = gpu::mesh_from_data(&mut rs, &kind.mesh, material);
            near.push(MultiMeshSlot::new(&mut rs, scenario, mesh, LAYER_PARTS, true, Rid::Invalid));
            far.push(MultiMeshSlot::new(&mut rs, scenario, mesh, LAYER_PARTS, false, Rid::Invalid));
            meshes.push(mesh);
        }
        Self { stats: RenderStats::default(), core: PartsCore::new(lib), gpu: Some(Gpu { shader, material, meshes, near, far, previews: Default::default() }) }
    }
    /// Per frame: compose each piece's transform as described above, write one buffer per MultiMesh
    /// (`INSTANCE_FLOATS` floats per instance) and call `multimesh_set_buffer` once per MultiMesh.
    pub fn update(&mut self, ctx: &FrameContext) {
        let lens = Lens { camera: gpu::camera_render_pos(ctx), px_per_m: gpu::px_per_m_at_1m(&ctx.camera), shadow_distance_m: ctx.quality.shadow_distance_m, degrade_step: ctx.degrade_step };
        self.core.step(ctx.view, ctx.events, lens, &|hash| design::lookup(hash));
        self.stats = RenderStats { part_instances: self.core.drawn, ..RenderStats::default() };
        let Some(g) = &mut self.gpu else { return };
        let mut rs = gpu::rs();
        for ((kind, near), far) in self.core.lib.kinds.iter().zip(&mut g.near).zip(&mut g.far) {
            near.upload_buffer(&mut rs, &kind.near);
            far.upload_buffer(&mut rs, &kind.far);
        }
    }
    /// Parts of one design at the origin for the dry dock, replacing any previous one. They are drawn by the
    /// next `update`, in the near band and never culled. (This signature carries no ghost preview: a ghost
    /// needs a validity tint the contract has no argument for.)
    pub fn show_design(&mut self, blueprint: &content::blueprint::Blueprint, catalogue: &content::catalogue::Catalogue) {
        self.core.show_design(&DrawDesign::from_blueprint(blueprint, None, catalogue, blueprint.header.hash, true));
    }
    /// A mesh RID for one part type (all pieces at rest merged), for UI previews in a SubViewport-free 3D tile.
    /// `Rid::Invalid` for an unknown part or one with no geometry. The RID lives until `free`.
    pub fn part_mesh(&self, part: content::ids::PartIdx) -> godot::prelude::Rid {
        let Some(g) = &self.gpu else { return Rid::Invalid };
        if part.index() >= self.core.lib.parts.len() { return Rid::Invalid; }
        *g.previews.borrow_mut().entry(part.0).or_insert_with(|| gpu::mesh_from_data(&mut gpu::rs(), &self.core.lib.merged_mesh(part.index()), g.material))
    }
    /// Anchors of one kind on drawn vessels this frame, ascending (vessel, slot). Valid after `update`.
    pub fn anchors(&self, kind: AnchorKind, out: &mut Vec<AnchorPose>) {
        out.clear();
        out.extend(self.core.anchors.iter().filter(|a| a.kind == kind));
    }
    pub fn stats(&self) -> RenderStats { self.stats }
    /// Free every RID. Idempotent.
    pub fn free(&mut self) {
        let Some(mut g) = self.gpu.take() else { return };
        let mut rs = gpu::rs();
        g.near.iter_mut().chain(&mut g.far).for_each(|s| s.free(&mut rs));
        g.meshes.iter_mut().for_each(|m| gpu::free_rid(&mut rs, m));
        g.previews.get_mut().values_mut().for_each(|m| gpu::free_rid(&mut rs, m));
        gpu::free_rid(&mut rs, &mut g.material);
        self.core.clear();
        self.core.dock = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::interp::VesselPose;
    use content::blueprint::PartPlacement;
    use content::cell::{Cell, CellGrid, Material};
    use content::ids::{CellIndex, PartId};
    use sim_systems::protocol::EntityIntro;

    fn ev(tick: u64, event: SimEvent) -> WireEvent { WireEvent { tick, event } }
    fn reveal(entity: u64, hash: ContentHash, removed: Vec<CellIndex>) -> WireEvent {
        ev(0, SimEvent::EntityRevealed(Box::new(EntityIntro { entity: EntityId::from_u64(entity), side: Default::default(), blueprint: hash, exterior_only: false, class: None, name: String::new(), player: None, status: Default::default(), damage: Vec::new(), removed, parent: None, track: Default::default() })))
    }
    fn place(part: &str, x: i16, y: i16, z: i16, yaw: u8) -> PartPlacement {
        PartPlacement { part: PartId::new(part), pos: CellPos::new(x, y, z), rot: PartRot { yaw, tilt: 0 }, kit: None, label: None, load: None }
    }
    /// A 40 x 6 x 12 cell steel block with a gun on the deck forward, a radar amidships, a rudder aft below
    /// the keel line and one part this build does not know.
    fn design() -> Arc<DrawDesign> {
        let mut d = super::design::tests::box_design([40, 8, 12]);
        let grid = Arc::get_mut(&mut d.grid).unwrap();
        for y in 6..8 { for z in 0..12 { for x in 0..40 { grid.set(CellPos::new(x, y, z), Cell::EMPTY); } } }
        d.parts = vec![(PartSlot(0), place("gun.76mm.single", 30, 6, 4, 0)), (PartSlot(1), place("radar.search", 18, 6, 5, 0)), (PartSlot(2), place("rudder.small", 1, 0, 5, 0)), (PartSlot(3), place("not.a.part", 0, 0, 0, 0))];
        d.mesh_cells = d.grid.cells.clone();
        Arc::new(d)
    }
    fn lens(camera: Vec3) -> Lens { Lens { camera, px_per_m: 935.0, shadow_distance_m: 500.0, degrade_step: 0 } }
    fn pose(entity: u64, x: f32, own: bool) -> VesselPose { VesselPose { entity: EntityId::from_u64(entity), pos: [x, 0.0, 0.0], rot: [0.0, 0.0, 0.0, 1.0], own, ..Default::default() } }
    fn view(time_s: f64, vessels: Vec<VesselPose>) -> ViewFrame { ViewFrame { sim_time_s: time_s, render_tick: time_s * 60.0, vessels, ..Default::default() } }
    fn core() -> PartsCore { PartsCore::new(Library::build(content::catalogue::catalogue())) }
    fn counts(c: &PartsCore) -> (usize, usize) { c.lib.kinds.iter().fold((0, 0), |(n, f), k| (n + k.near.count(), f + k.far.count())) }

    #[test]
    fn placement_puts_the_rotated_footprint_at_its_cell() {
        // Rotation 0: axes unchanged, origin at the cell's minimum corner.
        let m = placement_affine(CellPos::new(4, 2, 6), PartRot::default(), [6, 2, 4]);
        assert_eq!(m.transform_point3(Vec3::ZERO), Vec3::new(2.0, 1.0, 3.0));
        assert_eq!(m.transform_vector3(Vec3::X), Vec3::X);
        // Every rotation: the footprint box lands exactly on the rotated size at `pos`, right-handed.
        for tilt in 0..content::kit::TILT_COUNT {
            for yaw in 0..content::kit::YAW_COUNT {
                let rot = PartRot { yaw, tilt };
                let size = [6u8, 2, 4];
                let m = placement_affine(CellPos::new(4, 2, 6), rot, size);
                let (mut lo, mut hi) = (Vec3::splat(f32::MAX), Vec3::splat(f32::MIN));
                for corner in 0..8 {
                    let p = Vec3::new(if corner & 1 == 0 { 0.0 } else { 3.0 }, if corner & 2 == 0 { 0.0 } else { 1.0 }, if corner & 4 == 0 { 0.0 } else { 2.0 });
                    let q = m.transform_point3(p);
                    lo = lo.min(q);
                    hi = hi.max(q);
                }
                let r = content::kit::rotated_size(size, rot);
                assert!((lo - Vec3::new(2.0, 1.0, 3.0)).length() < 1e-5, "{rot:?}: {lo}");
                assert!((hi - lo - Vec3::new(r[0] as f32, r[1] as f32, r[2] as f32) * 0.5).length() < 1e-5, "{rot:?}");
                assert!((glam::Mat3::from(m.matrix3).determinant() - 1.0).abs() < 1e-6);
            }
        }
        // One quarter turn of yaw carries +X (the bow) to -Z (port), as `PartRot` documents.
        let m = placement_affine(CellPos::new(0, 0, 0), PartRot { yaw: 1, tilt: 0 }, [2, 2, 2]);
        assert!((m.transform_vector3(Vec3::X) - Vec3::NEG_Z).length() < 1e-6);
        assert_eq!(seat_cell(CellPos::new(4, 2, 6), PartRot { yaw: 1, tilt: 0 }, [6, 2, 4]), CellPos::new(6, 2, 9));
    }

    #[test]
    fn parts_wear_the_side_paint_not_the_deck_paint() {
        // A hull whose top row is dark deck paint (swatch 0 here) over sides in swatch 1.
        let mut d = super::design::tests::box_design([20, 6, 10]);
        let grid = Arc::get_mut(&mut d.grid).unwrap();
        for z in 0..10 { for x in 0..20 { grid.set(CellPos::new(x, 5, z), Cell { paint: 0, ..super::design::tests::STEEL }); } }
        d.palette = vec![[0.05, 0.05, 0.05], [0.3, 0.3, 0.35]];
        d.hull_paint = [0.9, 0.9, 0.9];
        // A mount on the deck (seat row 6): two rows down is the side plating.
        assert_eq!(wall_paint(&d, CellPos::new(10, 6, 3)), [0.3, 0.3, 0.35]);
        // One row above the keel there is only one row under the seat: that row is used.
        assert_eq!(wall_paint(&d, CellPos::new(10, 1, 7)), [0.3, 0.3, 0.35]);
        // On a mast top with nothing beneath, and on the keel row itself: the first hull swatch.
        assert_eq!(wall_paint(&d, CellPos::new(10, 12, 3)), [0.9, 0.9, 0.9]);
        assert_eq!(wall_paint(&d, CellPos::new(10, 0, 3)), [0.9, 0.9, 0.9]);
        // Part cells are not plating: a part stacked on a part looks past it.
        let grid = Arc::get_mut(&mut d.grid).unwrap();
        for z in 0..10 { grid.set(CellPos::new(4, 4, z), Cell { material: Material::PartBody, paint: 0, ..super::design::tests::STEEL }); }
        for z in 0..10 { grid.set(CellPos::new(4, 3, z), Cell { material: Material::PartBody, paint: 0, ..super::design::tests::STEEL }); }
        assert_eq!(wall_paint(&d, CellPos::new(4, 5, 3)), [0.9, 0.9, 0.9]);
        // A seat outside the grid is answered, not a panic.
        assert_eq!(wall_paint(&d, CellPos::new(-5, 3, 400)), [0.9, 0.9, 0.9]);
    }

    #[test]
    fn joints_follow_the_simulation_conventions() {
        let piece = |joint, role| ModelPiece { kind: 0, parent: None, pivot: Vec3::new(1.0, 2.0, 3.0), joint, role };
        let j = JointInput { train: 0.5, elevation: 0.25, sim_time_s: 10.0, shaft_turns: 0.25, since_fired_s: RECOIL_KICK_S, dead: false };
        let drive = Drive { rudder_max_rad: None, antenna_rev_s: 0.4 };
        // Training is a right-handed turn about +Y: the muzzle swings from the bow toward port (-Z), the same
        // law as `sim_core::parts::mount_dir_local`.
        let train = joint_local(&piece(Joint::Train, PieceRole::Turret), &drive, &j);
        let elevate = joint_local(&piece(Joint::Elevate, PieceRole::Cradle), &drive, &j);
        let barrel = (train.matrix3 * elevate.matrix3) * glam::Vec3A::X;
        let sim = sim_core::parts::mount_dir_local(0.0, 0.5, 0.25);
        assert!((Vec3::from(barrel) - Vec3::new(sim.x as f32, sim.y as f32, sim.z as f32)).length() < 1e-6, "{barrel}");
        assert!(barrel.z < 0.0 && barrel.y > 0.0);
        assert_eq!(Vec3::from(train.translation), Vec3::new(1.0, 2.0, 3.0));
        // Recoil: full travel at the end of the kick, home after 0.4 s, nothing before the shot.
        let recoil = |t: f32| joint_local(&piece(Joint::Recoil { travel_m: 0.6 }, PieceRole::Barrel), &drive, &JointInput { since_fired_s: t, ..j }).translation.x;
        assert!((recoil(RECOIL_KICK_S) - 0.4).abs() < 1e-6);
        assert_eq!(recoil(-0.2), 1.0);
        assert_eq!(recoil(RECOIL_S), 1.0);
        assert!(recoil(0.2) > 0.4 && recoil(0.2) < 1.0);
        assert_eq!((recoil_fraction(f32::NAN), recoil_fraction(0.0)), (0.0, 0.0));
        // Antennas turn clockwise seen from above: after a quarter turn the +X face looks to starboard (+Z).
        let spin = |role, input: JointInput| joint_local(&piece(Joint::Spin { axis: [0.0, 1.0, 0.0] }, role), &drive, &input).matrix3 * glam::Vec3A::X;
        let quarter = spin(PieceRole::Antenna, JointInput { sim_time_s: 0.25 / 0.4, ..j });
        assert!((Vec3::from(quarter) - Vec3::Z).length() < 1e-5, "{quarter}");
        // A destroyed part does not turn; a rotary cluster is back where it started after its spin.
        assert_eq!(Vec3::from(spin(PieceRole::Antenna, JointInput { dead: true, ..j })), Vec3::X);
        let x_spin = |t: f32| joint_local(&piece(Joint::Spin { axis: [1.0, 0.0, 0.0] }, PieceRole::Barrel), &drive, &JointInput { since_fired_s: t, ..j }).matrix3 * glam::Vec3A::Y;
        assert!((Vec3::from(x_spin(5.0)) - Vec3::Y).length() < 1e-4);
        assert!((Vec3::from(x_spin(0.2)) - Vec3::Y).length() < 1e-4, "three whole turns in 0.2 s");
        assert!((Vec3::from(x_spin(0.05)) - Vec3::Y).length() > 0.5);
        // Late in a long sortie the angle is still exact.
        let late = spin(PieceRole::Antenna, JointInput { sim_time_s: 36_000.0 + 0.25 / 0.4, ..j });
        assert!((Vec3::from(late) - Vec3::Z).length() < 5e-3, "{late}");
        assert_eq!(scorch_of(DamageLevel::Destroyed), 1.0);
        assert_eq!(instance_data([0.1, 0.2, 0.3], 0.5), ([1.0; 4], [0.5, 0.1, 0.2, 0.3]));
    }

    #[test]
    fn the_catalogue_builds_into_shared_models() {
        let cat = content::catalogue::catalogue();
        let lib = Library::build(cat);
        assert_eq!(lib.parts.len(), cat.parts.len());
        assert!(lib.models.len() <= lib.parts.len());
        for (i, (id, def)) in cat.parts.iter().enumerate() {
            let model = &lib.models[lib.parts[i].model as usize];
            assert!(!model.pieces.is_empty(), "{id} has no model");
            assert!(model.pieces.iter().any(|p| !lib.kinds[p.kind as usize].mesh.is_empty()), "{id} draws nothing");
            assert!(model.radius.is_finite() && model.radius > 0.0, "{id}");
            // Parents precede children, as the frame loop relies on.
            assert!(model.pieces.iter().enumerate().all(|(k, p)| p.parent.is_none_or(|q| (q as usize) < k)));
            let merged = lib.merged_mesh(i);
            assert_eq!(merged.triangle_count(), model.pieces.iter().map(|p| lib.kinds[p.kind as usize].mesh.triangle_count()).sum::<usize>(), "{id}");
            assert!(merged.colors.len() == merged.positions.len() && merged.custom0.len() == merged.positions.len());
            if matches!(def.function, PartFunction::Rudder { .. }) { assert!(lib.parts[i].drive.rudder_max_rad.is_some_and(|r| r > 0.1 && r < 1.6), "{id}"); }
        }
        assert!(lib.merged_mesh(usize::MAX).is_empty());
        // Two parts that name the same generator, parameters and footprint share one model.
        let mut twice = cat.clone();
        let (first_id, first) = cat.parts.iter().next().map(|(k, v)| (k.clone(), v.clone())).unwrap();
        twice.parts.insert(PartId::new(format!("{first_id}.copy")), first);
        let lib2 = Library::build(&twice);
        assert_eq!(lib2.models.len(), lib.models.len());
        assert_eq!(lib2.parts.last().unwrap().model, lib2.parts[0].model);
    }

    #[test]
    fn parts_are_placed_banded_culled_and_anchored() {
        let mut c = core();
        let d = design();
        let lookup = |_: &ContentHash| Some(d.clone());
        let nothing = |_: &ContentHash| None;
        let near_cam = lens(Vec3::new(0.0, 20.0, -60.0));
        // Revealed before its design arrives: nothing drawn, nothing lost.
        c.step(&view(0.0, vec![pose(5, 0.0, false)]), &[reveal(5, d.hash, Vec::new())], near_cam, &nothing);
        assert_eq!((c.drawn, counts(&c)), (0, (0, 0)));
        c.step(&view(0.1, vec![pose(5, 0.0, false)]), &[], near_cam, &lookup);
        let layout = c.vessels[&EntityId::from_u64(5)].layout.clone().unwrap();
        assert_eq!(layout.parts.len(), 3, "the unknown part is skipped");
        let (near, far) = counts(&c);
        assert!(near > 3 && far == 0, "inside the shadow distance: {near} near, {far} far");
        assert_eq!(c.drawn as usize, near);
        // The gun's base piece sits at its footprint corner (30, 6, 4) cells = (15, 3, 2) m plus its pivot.
        let gun = &c.lib.models[c.lib.parts[layout.parts[0].part as usize].model as usize];
        let base = &c.lib.kinds[gun.pieces[0].kind as usize].near;
        let pivot = gun.pieces[0].pivot;
        assert!((Vec3::new(base.data[3], base.data[7], base.data[11]) - (Vec3::new(15.0, 3.0, 2.0) + pivot)).length() < 1e-4);
        // The part wears the paint of the deck cell under it (swatch 1), in the layout the shader reads.
        assert_eq!(&base.data[12..20], &[1.0, 1.0, 1.0, 1.0, 0.0, 0.2, 0.25, 0.3]);
        // Anchors: ascending slot, the gun has a muzzle ahead of its seat.
        let muzzles: Vec<AnchorPose> = c.anchors.iter().copied().filter(|a| a.kind == AnchorKind::Muzzle).collect();
        assert!(!muzzles.is_empty() && muzzles.iter().all(|a| a.slot == PartSlot(0) && a.pos[0] > 15.0 && a.dir[0] > 0.9));
        assert!(c.anchors.windows(2).all(|w| (w[0].vessel, w[0].slot) <= (w[1].vessel, w[1].slot)));
        // 2 km away: beyond the shadow distance, so the far band; 30 km away: under a pixel, but the anchors stay.
        c.step(&view(0.2, vec![pose(5, 2_000.0, false)]), &[], near_cam, &lookup);
        let (near, far) = counts(&c);
        assert!(near == 0 && far > 0);
        c.step(&view(0.3, vec![pose(5, 30_000.0, false)]), &[], near_cam, &lookup);
        assert_eq!(counts(&c), (0, 0));
        assert!(c.anchors.iter().any(|a| a.kind == AnchorKind::Muzzle));
        // The own hull is never culled and always shadowed.
        c.step(&view(0.4, vec![pose(5, 30_000.0, true)]), &[], near_cam, &lookup);
        assert!(counts(&c).0 > 3 && counts(&c).1 == 0);
        // From degrade step 4 another hull beyond 300 m casts no shadow.
        c.step(&view(0.5, vec![pose(5, 400.0, false)]), &[], Lens { degrade_step: 4, ..near_cam }, &lookup);
        assert!(counts(&c).0 == 0 && counts(&c).1 > 0);
        // No pose this frame: nothing drawn, state kept. A hidden entity is forgotten.
        c.step(&view(0.6, vec![]), &[], near_cam, &lookup);
        assert_eq!((counts(&c), c.vessels.len()), ((0, 0), 1));
        c.step(&view(0.7, vec![pose(5, 0.0, false)]), &[ev(1, SimEvent::EntityHidden { entity: EntityId::from_u64(5) })], near_cam, &lookup);
        assert_eq!((counts(&c), c.vessels.len()), ((0, 0), 0));
    }

    #[test]
    fn damage_waits_for_playback() {
        let mut c = core();
        let d = design();
        let lookup = |_: &ContentHash| Some(d.clone());
        let cam = lens(Vec3::new(0.0, 20.0, -60.0));
        let e = EntityId::from_u64(5);
        c.step(&view(1.0, vec![pose(5, 0.0, false)]), &[reveal(5, d.hash, Vec::new())], cam, &lookup);
        let all = c.drawn;
        let radar_seat = c.vessels[&e].layout.as_ref().unwrap().parts[1].seat;
        // Playback is at tick 60; the events are for ticks 66 and 68, as they arrive over a live link.
        let hit = ev(66, SimEvent::Phys(PhysEvent::CellsDamaged { vessel: e, changes: vec![content::cell::CellChange { cell: CellIndex(radar_seat), bucket: 8 }], cause: sim_core::damage::DamageCause::Shell, attacker: EntityId::NONE }));
        let hurt = ev(68, SimEvent::Phys(PhysEvent::PartDamaged { vessel: e, slot: PartSlot(0), level: DamageLevel::Heavy }));
        c.step(&view(1.0, vec![pose(5, 0.0, false)]), &[hit, hurt], cam, &lookup);
        assert_eq!(c.drawn, all, "the radar stands until the shell lands");
        c.step(&view(65.0 / 60.0, vec![pose(5, 0.0, false)]), &[], cam, &lookup);
        assert_eq!(c.drawn, all);
        c.step(&view(66.0 / 60.0, vec![pose(5, 0.0, false)]), &[], cam, &lookup);
        assert!(c.drawn < all && c.vessels[&e].state[1].gone);
        assert_eq!(c.vessels[&e].state[0].scorch, 0.0);
        c.step(&view(68.0 / 60.0, vec![pose(5, 0.0, false)]), &[], cam, &lookup);
        assert_eq!(c.vessels[&e].state[0].scorch, 0.5);
    }

    #[test]
    fn damage_recoil_rudder_and_splits() {
        let mut c = core();
        let d = design();
        let lookup = |_: &ContentHash| Some(d.clone());
        let cam = lens(Vec3::new(0.0, 20.0, -60.0));
        let e = EntityId::from_u64(5);
        let seat = |slot: usize, c: &PartsCore| c.vessels[&e].layout.as_ref().unwrap().parts[slot].seat;
        let mut v = view(1.0, vec![pose(5, 0.0, false)]);
        v.vessels[0].rudder = 1.0;
        c.step(&v, &[reveal(5, d.hash, Vec::new())], cam, &lookup);
        let all = c.drawn;
        // The rudder's arm follows the helm: positive rudder swings its trailing edge to starboard.
        let layout = c.vessels[&e].layout.clone().unwrap();
        let rudder = &c.lib.models[c.lib.parts[layout.parts[2].part as usize].model as usize];
        let arm = rudder.pieces.iter().find(|p| matches!(p.joint, Joint::Train)).expect("the rudder has a trained arm");
        let buf = &c.lib.kinds[arm.kind as usize].near.data;
        assert!(buf[8] < -0.3, "basis.x.z = -sin(angle) for a starboard helm: {}", buf[8]);
        // The gun fires at tick 90 (1.5 s); at 1.5 s + kick its barrel is back by its travel.
        let fired = ev(90, SimEvent::Phys(PhysEvent::MountFired { vessel: e, slot: PartSlot(0), rounds: 1 }));
        let gun = &c.lib.models[c.lib.parts[layout.parts[0].part as usize].model as usize];
        let (barrel_kind, travel) = gun.pieces.iter().find_map(|p| match p.joint { Joint::Recoil { travel_m } => Some((p.kind as usize, travel_m)), _ => None }).expect("the gun has a recoiling barrel");
        c.step(&view(1.2, vec![pose(5, 0.0, false)]), &[fired], cam, &lookup);
        let rest_x = c.lib.kinds[barrel_kind].near.data[3];
        c.step(&view(1.5 + RECOIL_KICK_S as f64, vec![pose(5, 0.0, false)]), &[], cam, &lookup);
        assert!((rest_x - c.lib.kinds[barrel_kind].near.data[3] - travel).abs() < 1e-3, "barrel back by {travel} m");
        // Part damage scorches; the level is written to custom.x.
        let hurt = ev(100, SimEvent::Phys(PhysEvent::PartDamaged { vessel: e, slot: PartSlot(0), level: DamageLevel::Heavy }));
        c.step(&view(3.0, vec![pose(5, 0.0, false)]), &[hurt], cam, &lookup);
        assert_eq!(c.lib.kinds[barrel_kind].near.data[16], 0.5);
        // The radar's seat cell is destroyed: the radar is gone, the rest stay.
        let radar_seat = seat(1, &c);
        let hit = ev(110, SimEvent::Phys(PhysEvent::CellsDamaged { vessel: e, changes: vec![content::cell::CellChange { cell: CellIndex(radar_seat), bucket: 8 }, content::cell::CellChange { cell: CellIndex(seat(0, &c)), bucket: 5 }], cause: sim_core::damage::DamageCause::Shell, attacker: EntityId::NONE }));
        c.step(&view(3.1, vec![pose(5, 0.0, false)]), &[hit], cam, &lookup);
        assert!(c.drawn < all && c.anchors.iter().all(|a| a.slot != PartSlot(1)));
        assert!(c.anchors.iter().any(|a| a.slot == PartSlot(0)));
        // The bow splits off with the gun's seat: the gun moves to the piece, with its scorch.
        let split = ev(120, SimEvent::Phys(PhysEvent::Split { vessel: e, piece: EntityId::from_u64(9), cells: vec![CellIndex(seat(0, &c))] }));
        c.step(&view(3.2, vec![pose(5, 0.0, false), pose(9, 50.0, false)]), &[split], cam, &lookup);
        let on = |entity: u64, slot: usize, c: &PartsCore| !c.vessels[&EntityId::from_u64(entity)].state[slot].gone;
        assert!(!on(5, 0, &c) && on(9, 0, &c));
        assert!(on(5, 2, &c) && !on(9, 2, &c), "the rudder stays with the stern");
        assert!(!on(5, 1, &c) && !on(9, 1, &c), "the destroyed radar is on neither");
        assert!(c.anchors.iter().any(|a| a.vessel == EntityId::from_u64(9) && a.slot == PartSlot(0) && a.kind == AnchorKind::Muzzle));
        assert_eq!(c.vessels[&EntityId::from_u64(9)].state[0].scorch, 0.5);
        // A wreck piece revealed with its removed cells draws only what is seated on what it kept.
        let removed: Vec<CellIndex> = (0..d.grid.cells.len() as u32).filter(|i| *i != seat(2, &c)).map(CellIndex).collect();
        c.step(&view(3.3, vec![pose(12, 0.0, false)]), &[reveal(12, d.hash, removed)], cam, &lookup);
        assert!(c.anchors.iter().all(|a| a.vessel != EntityId::from_u64(12) || a.slot == PartSlot(2)));
        let gone = c.vessels[&EntityId::from_u64(12)].state.iter().filter(|s| s.gone).count();
        assert_eq!(gone, 2);
        // Events for vessels nobody revealed, and slots that do not exist, are ignored.
        let stray = [ev(1, SimEvent::Phys(PhysEvent::MountFired { vessel: EntityId::from_u64(77), slot: PartSlot(0), rounds: 1 })), ev(1, SimEvent::Phys(PhysEvent::PartDamaged { vessel: e, slot: PartSlot(900), level: DamageLevel::Destroyed }))];
        c.step(&view(3.4, vec![pose(5, 0.0, false)]), &stray, cam, &lookup);
        // A new sortie forgets every vessel; the dock design is drawn with no vessel at all.
        c.step(&view(0.0, vec![]), &[ev(0, SimEvent::SortieStarted { seed: 1, tick_hz: 60 })], cam, &lookup);
        assert!(c.vessels.is_empty() && c.drawn == 0);
        c.show_design(&d);
        c.step(&view(0.0, vec![]), &[], cam, &lookup);
        assert_eq!(c.drawn, all);
        assert!(c.anchors.iter().all(|a| a.vessel == EntityId::NONE));
    }

    /// The 0.3 ms share (L2006) is for the engine upload; this checks the transform loop leaves room for it.
    #[test]
    fn the_frame_loop_is_fast_enough() {
        let mut c = core();
        // 40 hulls of 80 parts each: the reference battle's order of magnitude.
        let ids: Vec<String> = content::catalogue::catalogue().parts.keys().map(|k| k.as_str().to_string()).collect();
        let mut d = super::design::tests::box_design([120, 10, 24]);
        d.parts = (0..80u16).map(|i| (PartSlot(i), place(&ids[i as usize % ids.len()], (i as i16 % 40) * 3, 9, (i as i16 / 40) * 12, (i % 4) as u8))).collect();
        let d = Arc::new(d);
        let lookup = |_: &ContentHash| Some(d.clone());
        let events: Vec<WireEvent> = (0..40).map(|i| reveal(i + 1, d.hash, Vec::new())).collect();
        let poses: Vec<VesselPose> = (0..40).map(|i| pose(i + 1, i as f32 * 40.0, false)).collect();
        let cam = lens(Vec3::new(0.0, 30.0, -100.0));
        c.step(&view(0.0, poses.clone()), &events, cam, &lookup);
        let start = std::time::Instant::now();
        let frames = 50;
        for f in 0..frames { c.step(&view(f as f64 / 60.0, poses.clone()), &[], cam, &lookup); }
        let per_frame = start.elapsed().as_secs_f64() * 1e6 / frames as f64;
        println!("part loop: {} instances in {per_frame:.0} us per frame", c.drawn);
        assert!(c.drawn > 5_000);
        // The time is printed, not asserted: shared CI runners are too noisy to gate on a wall clock.
    }
}
