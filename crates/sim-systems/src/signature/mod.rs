//! Unit `sys-signature`. Stage 1 of the sensor pipeline: what every entity presents to sensors.
//! DESIGN.md L404 (signature stage, per tick), L427 (aspect bins and material factors), L432 (reference RCS),
//! L436 (visual area, lights, fired, burning), L453-454 (acoustic noise, submerged area), L524 (infrared),
//! L599-604 (decoy objects).
//!
//! Projected areas come from `sim_core::vessel::projected_areas`, which is costly: call it at spawn and when
//! a vessel's solid cell count or draft band changes, never per tick. Everything else is refreshed every tick.
use crate::prelude::*;
use crate::ship::ShipTable;
use content::part::DecoyKind;
use content::part::weapons::{MunitionDef, MunitionKind, SeekerKind};
use content::part::{PartFunction, SensorKind};
use content::tuning::SensorTuning;
use sim_core::munitions::{BodyPhase, Bodies};
use sim_core::parts::PartState;
use sim_core::vessel::{ProjectedAreas, Vessel, VesselStatus};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum EmitterKind { SearchRadar, NavRadar, FireControlRadar, Seeker, ActiveSonar, Jammer }

/// One emitter radiating this tick (ESM and passive-sonar input, L434, L454).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Emission {
    pub source: SensorRef,
    pub kind: EmitterKind,
    /// Index into `crate::ident::IdentLibrary::emitters`; `u16::MAX` = not in any library.
    pub emitter: u16,
    /// Emitter's own reference range in metres: ESM hears it to 2.0 x this, horizon-capped (L434).
    pub r_ref_m: f64,
    /// Emitter height above the sea in metres (negative for sonar).
    pub height_m: f64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SigKind { Vessel, Wreck, Body(MunitionIdx), Decoy(DecoyKind) }

/// Everything a sensor law needs to know about one target this tick.
#[derive(Clone, Debug, PartialEq)]
pub struct Signature {
    pub entity: EntityId,
    pub side: SideId,
    pub kind: SigKind,
    pub domain: Domain,
    pub class: Option<ClassId>,
    pub pos: DVec3,
    pub vel: DVec3,
    pub heading_rad: f64,
    /// Aspect tables for vessels and wrecks. Small bodies carry a uniform table (bounding-sphere disc for the
    /// visual and submerged silhouettes, catalogue RCS for radar); decoys carry `None` and use the flat values
    /// below. Radar on bodies and decoys always reads `rcs_flat_m2`.
    pub areas: Option<Arc<ProjectedAreas>>,
    /// Flat radar cross-section in m^2 for bodies (missile 0.1, drone 0.5, aircraft 5) and decoys (chaff bloom
    /// curve, towed decoy 2x owner).
    pub rcs_flat_m2: f64,
    /// Highest point above the sea in metres (horizon cull target height, clutter fade, L425, L427).
    pub top_m: f64,
    /// Depth of the keel or body below the surface in metres; 0 when surfaced. Sonar layer side (L455).
    pub depth_m: f64,
    /// Acoustic noise N: propulsion power / 2 MW, doubled above cavitation speed; torpedo 1.0; mines 0 (L453).
    pub noise: f64,
    /// Infrared power in MW: propulsion power for hulls, 20 MW for a flare (L524).
    pub ir_mw: f64,
    pub emissions: Vec<Emission>,
    /// Self-screening jammer burn-through rating in metres while radiating (L428).
    pub jammer_r_bt0_m: Option<f64>,
    /// Night visual exceptions: fired in the last 10 s, burning, or lights on (Active EMCON only), L436.
    pub fired_recently: bool,
    pub burning: bool,
    pub lights: bool,
}
impl Signature {
    /// Radar cross-section toward an observer at `from`, m^2: aspect bin (10 degree bins from the bow) for
    /// vessels, the flat value otherwise. Unit: `sys-signature`.
    pub fn rcs_toward(&self, from: DVec3) -> f64 {
        let v = match (&self.kind, &self.areas) {
            (SigKind::Vessel | SigKind::Wreck, Some(a)) => a.rcs_m2[self.aspect_bin(from)] as f64,
            _ => self.rcs_flat_m2,
        };
        finite_nonneg(v)
    }
    /// Projected above-water area toward `from`, m^2 (visual law, L436). Unit: `sys-signature`.
    pub fn visual_area_toward(&self, from: DVec3) -> f64 {
        let v = match (&self.kind, &self.areas) {
            (SigKind::Vessel | SigKind::Wreck, Some(a)) => a.above_m2[self.aspect_bin(from)] as f64,
            // A body under the surface shows nothing above it; the table is its silhouette.
            (SigKind::Body(_), Some(a)) if !self.is_submerged() => a.above_m2[self.aspect_bin(from)] as f64,
            _ => 0.0,
        };
        finite_nonneg(v)
    }
    /// Projected submerged area toward `from`, m^2 (active sonar law, L454). Unit: `sys-signature`.
    pub fn submerged_area_toward(&self, from: DVec3) -> f64 {
        let v = match (&self.kind, &self.areas) {
            (SigKind::Vessel | SigKind::Wreck, Some(a)) => a.submerged_m2[self.aspect_bin(from)] as f64,
            (SigKind::Body(_), Some(a)) if self.depth_m > 0.0 => a.submerged_m2[self.aspect_bin(from)] as f64,
            _ => 0.0,
        };
        finite_nonneg(v)
    }
    /// Aspect bin 0..36 of an observer position. Unit: `sys-signature`.
    ///
    /// Bin `i` holds observers whose relative bearing, measured clockwise from the target's bow on the
    /// horizontal plane, lies in `[10 i, 10 (i + 1))` degrees: bin 0 is just off the bow to starboard, bin 9
    /// the starboard beam, bin 18 dead astern and bin 27 the port beam. An observer straight above or below
    /// (no horizontal offset) reads bin 0.
    pub fn aspect_bin(&self, from: DVec3) -> usize {
        let d = from - self.pos;
        if !(d.x.is_finite() && d.z.is_finite()) || (d.x == 0.0 && d.z == 0.0) { return 0; }
        let rel = wrap_tau(compass_bearing(d) - self.heading_rad);
        let bin = (rel / BIN_RAD).floor();
        if bin.is_finite() && bin > 0.0 { (bin as usize).min(35) } else { 0 }
    }
    /// Fully under the surface: nothing shows to radar or the eye (torpedoes, moored mines, a dived submarine).
    pub fn is_submerged(&self) -> bool { self.depth_m > 0.0 && self.top_m <= 0.0 }
}

/// Signatures of every live entity, ascending entity slot, rebuilt each tick.
#[derive(Clone, Debug, Default)]
pub struct SignatureTable {
    pub rows: Vec<Signature>,
    /// Cached area tables per vessel with the key they were computed at (solid-cell count and draft band), and
    /// per small body (keyed by munition index). Pruned to live entities every update.
    pub(crate) areas: BTreeMap<EntityId, (u32, Arc<ProjectedAreas>)>,
}
impl SignatureTable {
    pub fn get(&self, id: EntityId) -> Option<&Signature> { self.rows.binary_search_by(|s| s.entity.cmp(&id)).ok().map(|i| &self.rows[i]) }
    /// Refresh every row from the world: vessels and wrecks (areas from cache, noise from
    /// `Vessel::propulsion_kw` and speed against each propulsor's cavitation speed, emissions from parts whose
    /// `PartState` says radiating, lights only in Active EMCON), bodies (catalogue values, seeker emission when
    /// `Bodies::radiating`), decoys (`Decoys::rcs_m2`, `ir_mw`). Recomputes a vessel's projected areas only
    /// when its solid-cell count changed since the cached copy. Unit: `sys-signature`.
    pub fn update(&mut self, world: &World, ships: &ShipTable, ctx: &SimCtx<'_>) {
        let cat = ctx.catalogue;
        let tuning = &ctx.tuning.sensors;
        let decoy_tuning = &ctx.tuning.decoys;
        let now = ctx.tick.0;
        let emitters = EmitterKeys::new(cat);
        let mut rows: Vec<Signature> = Vec::with_capacity(self.rows.len());
        let mut live: BTreeSet<EntityId> = BTreeSet::new();

        // ---- Vessels and wreck pieces ----
        for (_, v) in world.vessels() {
            live.insert(v.id);
            let key = area_key(v);
            let areas = match self.areas.get(&v.id) {
                Some((k, a)) if *k == key => a.clone(),
                _ => {
                    let a = Arc::new(sim_core::vessel::projected_areas(v, cat));
                    self.areas.insert(v.id, (key, a.clone()));
                    a
                }
            };
            rows.push(vessel_row(v, areas, ships, cat, tuning, now, &emitters));
        }

        // ---- Small bodies ----
        let b = world.bodies();
        for row in 0..b.len() {
            if b.phase.get(row).copied() == Some(BodyPhase::Dead) { continue; }
            let (Some(&id), Some(&kind), Some(&pos)) = (b.entity.get(row), b.kind.get(row), b.pos.get(row)) else { continue };
            if !pos.is_finite() { continue; }
            let Some((_, def)) = cat.munitions.get_index(kind.index()) else { continue };
            live.insert(id);
            let key = kind.0 as u32;
            let areas = match self.areas.get(&id) {
                Some((k, a)) if *k == key => a.clone(),
                _ => {
                    let a = Arc::new(body_areas(def));
                    self.areas.insert(id, (key, a.clone()));
                    a
                }
            };
            rows.push(body_row(b, row, id, pos, kind, def, areas, tuning, &emitters));
        }

        // ---- Decoys (chaff, flares, smoke, towed decoys) ----
        let d = world.decoys();
        for row in 0..d.entity.len() {
            let (Some(&id), Some(&kind), Some(&owner), Some(&pos)) = (d.entity.get(row), d.kind.get(row), d.owner.get(row), d.pos.get(row)) else { continue };
            if !pos.is_finite() { continue; }
            // Owner RCS for the towed decoy: the owner's mean over all aspects (the decoy is a point source).
            let owner_rcs = rows.iter().find(|r| r.entity == owner).map(Signature::mean_rcs).unwrap_or(0.0);
            let (side, vel) = match world.vessel(owner) {
                Some(v) => (v.side, if kind == DecoyKind::TowedDecoy { v.kin.lin_vel } else { DVec3::ZERO }),
                None => match b.row_of(owner) { Some(r) => (b.side.get(r).copied().unwrap_or_default(), DVec3::ZERO), None => (world.locate(id).map(|l| l.2).unwrap_or_default(), DVec3::ZERO) },
            };
            let rcs = finite_nonneg(d.rcs_m2(row, owner_rcs, decoy_tuning));
            let ir = finite_nonneg(d.ir_mw(row, decoy_tuning));
            let noise = if kind == DecoyKind::TowedDecoy { finite_nonneg(decoy_tuning.towed_noise) } else { 0.0 };
            let domain = match kind { DecoyKind::Chaff | DecoyKind::Flare => Domain::Air, DecoyKind::Smoke | DecoyKind::TowedDecoy => Domain::Surface };
            rows.push(Signature {
                entity: id, side, kind: SigKind::Decoy(kind), domain, class: None, pos, vel, heading_rad: heading_of(vel),
                areas: None, rcs_flat_m2: rcs, top_m: pos.y.max(0.0), depth_m: (-pos.y).max(0.0), noise, ir_mw: ir,
                emissions: Vec::new(), jammer_r_bt0_m: None, fired_recently: false, burning: false, lights: false,
            });
        }

        rows.sort_unstable_by_key(|r| r.entity);
        rows.dedup_by_key(|r| r.entity);
        self.areas.retain(|id, _| live.contains(id));
        self.rows = rows;
    }
}

/// Bin width: 10 degrees.
const BIN_RAD: f64 = core::f64::consts::TAU / 36.0;
/// Seconds after firing during which a hull stays visible at night (L436).
const FIRED_VISIBLE_S: f64 = 10.0;
/// ESM reference range given to a radiating jammer: large enough that only the radar horizon limits it
/// ("every ESM receiver within the radar horizon gets its bearing", L428). Finite so it serialises.
pub const JAMMER_ESM_R_REF_M: f64 = 1.0e9;
/// Flood-mass step of the draft band that triggers an area recompute: every 5% of dry mass taken on.
const DRAFT_BAND_MASS_STEP: f64 = 0.05;
/// Emergence step for submersible hulls: a band per 2 m of top height.
const DRAFT_BAND_TOP_STEP_M: f64 = 2.0;

impl Signature {
    /// Mean radar cross-section over the 36 aspects, m^2 (towed decoy, editor summary).
    pub fn mean_rcs(&self) -> f64 {
        match (&self.kind, &self.areas) {
            (SigKind::Vessel | SigKind::Wreck, Some(a)) => finite_nonneg(a.rcs_m2.iter().map(|&x| x as f64).sum::<f64>() / 36.0),
            _ => finite_nonneg(self.rcs_flat_m2),
        }
    }
}

/// Compass bearing (0 = north = world -Z, clockwise) of a world-frame offset, radians in `[0, 2 pi)`.
pub(crate) fn compass_bearing(d: DVec3) -> f64 { wrap_tau(libm::atan2(d.x, -d.z)) }
/// Wrap an angle into `[0, 2 pi)`; non-finite input gives 0.
pub(crate) fn wrap_tau(a: f64) -> f64 {
    if !a.is_finite() { return 0.0; }
    let t = core::f64::consts::TAU;
    let r = a - t * (a / t).floor();
    if r >= t || r < 0.0 { 0.0 } else { r }
}
/// Heading of a velocity on the horizontal plane; 0 when still.
fn heading_of(vel: DVec3) -> f64 { if vel.x == 0.0 && vel.z == 0.0 { 0.0 } else { compass_bearing(vel) } }
fn finite_nonneg(x: f64) -> f64 { if x.is_finite() && x > 0.0 { x } else { 0.0 } }

/// Cache key of a vessel's area table: live solid-cell count in the high 24 bits and a draft band in the low
/// 8 (flood mass in 5% steps of dry mass; for submersible hulls also emergence in 2 m steps of top height).
fn area_key(v: &Vessel) -> u32 {
    let dry = v.dry_mass.mass_kg;
    let ratio = if dry.is_finite() && dry > 0.0 && v.mass.mass_kg.is_finite() { v.mass.mass_kg / dry } else { 1.0 };
    let flood = ((ratio - 1.0) / DRAFT_BAND_MASS_STEP).floor().clamp(0.0, 15.0) as u32;
    let emerge = if v.submersible {
        let top = v.top_height_m();
        if top.is_finite() { ((top / DRAFT_BAND_TOP_STEP_M).floor() + 8.0).clamp(0.0, 15.0) as u32 } else { 0 }
    } else { 0 };
    (v.grid.solid_count.min(0x00FF_FFFF) << 8) | (emerge << 4) | flood
}

/// Uniform aspect table for a small body: its bounding-sphere disc (pi r^2) for the visual and submerged
/// silhouettes and the catalogue reference RCS for radar.
fn body_areas(def: &MunitionDef) -> ProjectedAreas {
    let r = finite_nonneg(def.radius_m);
    let disc = (core::f64::consts::PI * r * r) as f32;
    let rcs = finite_nonneg(def.rcs_m2) as f32;
    ProjectedAreas { rcs_m2: [rcs; 36], above_m2: [disc; 36], submerged_m2: [disc; 36], top_height_m: r as f32 }
}

/// Infrared power of a small body in MW. The spec fixes hulls (propulsion power) and flares (20 MW) only;
/// small bodies get engine-scale values so infrared seekers and flares behave sensibly against them.
fn body_ir_mw(kind: MunitionKind, phase: BodyPhase) -> f64 {
    let flying = !matches!(phase, BodyPhase::Passive | BodyPhase::Dead);
    if !flying { return 0.0; }
    match kind {
        MunitionKind::Aircraft => 5.0,
        MunitionKind::Helicopter => 1.5,
        MunitionKind::AntiShipMissile | MunitionKind::Sam | MunitionKind::PdMissile => 0.5,
        MunitionKind::Rhib => 0.2,
        MunitionKind::Drone => 0.05,
        MunitionKind::Torpedo | MunitionKind::Mine | MunitionKind::DepthCharge => 0.0,
    }
}

/// Sorted, de-duplicated emitter model keys of the whole catalogue (`SensorDef::emitter` on parts and on
/// small-body sensor lists). The index of a key in this list is the emitter id carried by [`Emission`]; it is
/// the same list [`emitter_keys`] returns.
struct EmitterKeys<'a> { keys: Vec<&'a str> }
impl<'a> EmitterKeys<'a> {
    fn new(cat: &'a Catalogue) -> Self {
        let mut keys: Vec<&str> = Vec::new();
        for def in cat.parts.values() {
            if let PartFunction::Sensor(s) = &def.function { if !s.emitter.is_empty() { keys.push(s.emitter.as_str()); } }
        }
        for m in cat.munitions.values() {
            for s in &m.sensors { if !s.emitter.is_empty() { keys.push(s.emitter.as_str()); } }
        }
        keys.sort_unstable();
        keys.dedup();
        Self { keys }
    }
    fn index(&self, key: &str) -> u16 {
        if key.is_empty() { return u16::MAX; }
        match self.keys.binary_search(&key) { Ok(i) if i < u16::MAX as usize => i as u16, _ => u16::MAX }
    }
}

/// The emitter index space used by the Signature stage: every non-empty `SensorDef::emitter` key in the
/// catalogue (parts and small-body sensors), sorted ascending and de-duplicated. `Emission::emitter` is an
/// index into this list, so `crate::ident::IdentLibrary::emitters` must be built as this same list (or a list
/// that agrees with it index for index) for ESM matches to resolve.
pub fn emitter_keys(catalogue: &Catalogue) -> Vec<String> { EmitterKeys::new(catalogue).keys.into_iter().map(str::to_owned).collect() }

/// Emitter index of one key in [`emitter_keys`] order, `u16::MAX` when absent or empty.
pub fn emitter_index(catalogue: &Catalogue, key: &str) -> u16 { EmitterKeys::new(catalogue).index(key) }

fn sensor_emitter_kind(kind: SensorKind) -> Option<EmitterKind> {
    match kind {
        SensorKind::SearchRadar => Some(EmitterKind::SearchRadar),
        SensorKind::NavRadar => Some(EmitterKind::NavRadar),
        SensorKind::FireControlRadar => Some(EmitterKind::FireControlRadar),
        SensorKind::Seeker => Some(EmitterKind::Seeker),
        SensorKind::ActiveSonar => Some(EmitterKind::ActiveSonar),
        SensorKind::Esm | SensorKind::PassiveSonar | SensorKind::Visual | SensorKind::Camera => None,
    }
}

#[allow(clippy::too_many_arguments)]
fn vessel_row(v: &Vessel, areas: Arc<ProjectedAreas>, ships: &ShipTable, cat: &Catalogue, tuning: &SensorTuning, now: u64, emitters: &EmitterKeys<'_>) -> Signature {
    let wreck = matches!(v.status, VesselStatus::Lost(_) | VesselStatus::Wreck);
    let top = v.top_height_m();
    let top_m = if top.is_finite() { top } else { areas.top_height_m as f64 };
    let depth_m = if v.submersible && top_m < 0.0 { (v.dims[1] - top_m).max(0.0) } else { 0.0 };
    let speed = v.forward_speed();
    let speed_kn = if speed.is_finite() { speed.abs() / sim_core::time::KN } else { 0.0 };

    let mut emissions = Vec::new();
    let mut jammer: Option<f64> = None;
    let mut cavitation_kn = f64::INFINITY;
    for part in &v.parts {
        if !part.works() { continue; }
        let Some((_, def)) = cat.parts.get_index(part.def.index()) else { continue };
        match (&part.state, &def.function) {
            (PartState::Propulsor { shaft_kw, .. }, PartFunction::Propulsor(p)) if *shaft_kw > 0.0 && p.cavitation_kn.is_finite() => {
                cavitation_kn = cavitation_kn.min(p.cavitation_kn);
            }
            (PartState::Sensor { radiating: true, .. }, PartFunction::Sensor(s)) if !wreck => {
                if let Some(kind) = sensor_emitter_kind(s.kind) {
                    let h = v.to_world(part.centre).y;
                    emissions.push(Emission {
                        source: SensorRef::part(v.id, part.slot), kind, emitter: emitters.index(&s.emitter),
                        r_ref_m: finite_nonneg(s.r_ref_m) * part.output_factor(), height_m: if h.is_finite() { h } else { 0.0 },
                    });
                }
            }
            (PartState::Jammer { radiating: true }, PartFunction::Jammer { r_bt0_m }) if !wreck => {
                let rated = if r_bt0_m.is_finite() && *r_bt0_m > 0.0 { r_bt0_m.max(tuning.jammer_r_bt0_m) } else { tuning.jammer_r_bt0_m };
                // Burn-through grows as 1/sqrt(jammer power): a jammer at half output is burnt through sqrt(2) farther.
                let out = part.output_factor();
                if out > 0.0 && rated.is_finite() && rated > 0.0 {
                    let r = rated / out.sqrt();
                    jammer = Some(jammer.map_or(r, |j: f64| j.min(r)));
                    let h = v.to_world(part.centre).y;
                    emissions.push(Emission { source: SensorRef::part(v.id, part.slot), kind: EmitterKind::Jammer, emitter: u16::MAX, r_ref_m: JAMMER_ESM_R_REF_M, height_m: if h.is_finite() { h } else { 0.0 } });
                }
            }
            _ => {}
        }
    }
    let kw = v.propulsion_kw();
    let kw = if kw.is_finite() { kw.max(0.0) } else { 0.0 };
    let noise = hull_noise(kw, speed_kn, cavitation_kn);
    let fired_recently = v.last_fired_tick != 0 && v.last_fired_tick <= now && ((now - v.last_fired_tick) as f64) < FIRED_VISIBLE_S * sim_core::TICK_HZ as f64;
    let lights = !wreck && v.status == VesselStatus::Active && ships.get(v.id).is_some_and(|s| s.settings.emcon == Emcon::Active);
    let pos = v.to_world(v.mass.com);
    let pos = if pos.is_finite() { pos } else { v.kin.pose.pos };
    Signature {
        entity: v.id, side: v.side, kind: if wreck { SigKind::Wreck } else { SigKind::Vessel },
        domain: if v.submersible { Domain::Subsurface } else { Domain::Surface }, class: v.class.clone(),
        pos, vel: v.kin.lin_vel, heading_rad: v.kin.pose.heading_rad(), areas: Some(areas), rcs_flat_m2: 0.0,
        top_m, depth_m, noise, ir_mw: kw / 1000.0, emissions, jammer_r_bt0_m: jammer,
        fired_recently, burning: v.is_burning(), lights,
    }
}

#[allow(clippy::too_many_arguments)]
fn body_row(b: &Bodies, row: usize, id: EntityId, pos: DVec3, kind: MunitionIdx, def: &MunitionDef, areas: Arc<ProjectedAreas>, tuning: &SensorTuning, emitters: &EmitterKeys<'_>) -> Signature {
    let vel = b.vel.get(row).copied().unwrap_or(DVec3::ZERO);
    let phase = b.phase.get(row).copied().unwrap_or_default();
    let radiating = b.radiating.get(row).copied().unwrap_or(false);
    let running = !matches!(phase, BodyPhase::Passive | BodyPhase::Dead);
    let noise = match def.kind {
        MunitionKind::Torpedo => if running { finite_nonneg(tuning.torpedo_noise) } else { 0.0 },
        MunitionKind::Mine | MunitionKind::DepthCharge => 0.0,
        _ => if running { finite_nonneg(def.noise) } else { 0.0 },
    };
    let mut emissions = Vec::new();
    if radiating {
        let seeker_kind = match def.seeker { SeekerKind::ActiveRadar => Some(EmitterKind::Seeker), SeekerKind::ActiveAcoustic => Some(EmitterKind::ActiveSonar), SeekerKind::Infrared | SeekerKind::None => None };
        if let Some(k) = seeker_kind {
            emissions.push(Emission { source: SensorRef::seeker(id), kind: k, emitter: u16::MAX, r_ref_m: finite_nonneg(def.seeker_r_ref_m), height_m: pos.y });
        }
        for (i, s) in def.sensors.iter().enumerate() {
            if i >= SensorRef::SEEKER as usize { break; }
            if let Some(k) = sensor_emitter_kind(s.kind) {
                emissions.push(Emission { source: SensorRef { carrier: id, index: i as u16 }, kind: k, emitter: emitters.index(&s.emitter), r_ref_m: finite_nonneg(s.r_ref_m), height_m: pos.y });
            }
        }
    }
    Signature {
        entity: id, side: b.side.get(row).copied().unwrap_or_default(), kind: SigKind::Body(kind), domain: def.domain, class: None,
        pos, vel, heading_rad: heading_of(vel), areas: Some(areas), rcs_flat_m2: finite_nonneg(def.rcs_m2),
        top_m: pos.y.max(0.0), depth_m: (-pos.y).max(0.0), noise, ir_mw: body_ir_mw(def.kind, phase), emissions,
        jammer_r_bt0_m: None, fired_recently: false, burning: false, lights: false,
    }
}

/// Acoustic noise N of a hull (L453): `propulsion_kw / 2000`, doubled while any propeller runs above its
/// cavitation speed. Shared with the editor's signature view. Unit: `sys-signature`.
pub fn hull_noise(propulsion_kw: f64, speed_kn: f64, cavitation_kn: f64) -> f64 {
    let base = finite_nonneg(propulsion_kw) / 2_000.0;
    if speed_kn.is_finite() && cavitation_kn.is_finite() && speed_kn > cavitation_kn { base * 2.0 } else { base }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A hand-built signature at the origin heading north, for law tests.
    pub(crate) fn sig(kind: SigKind) -> Signature {
        Signature {
            entity: EntityId { index: 7, generation: 0 }, side: SideId(1), kind, domain: Domain::Surface, class: None,
            pos: DVec3::ZERO, vel: DVec3::ZERO, heading_rad: 0.0, areas: None, rcs_flat_m2: 0.0, top_m: 10.0, depth_m: 0.0,
            noise: 0.0, ir_mw: 0.0, emissions: Vec::new(), jammer_r_bt0_m: None, fired_recently: false, burning: false, lights: false,
        }
    }

    fn table() -> Arc<ProjectedAreas> {
        let mut a = ProjectedAreas { rcs_m2: [0.0; 36], above_m2: [0.0; 36], submerged_m2: [0.0; 36], top_height_m: 10.0 };
        for i in 0..36 { a.rcs_m2[i] = i as f32 * 10.0; a.above_m2[i] = i as f32; a.submerged_m2[i] = 100.0 + i as f32; }
        Arc::new(a)
    }

    /// World offset of an observer at compass bearing `deg` from the target.
    fn at(deg: f64, r: f64) -> DVec3 { let a = deg.to_radians(); DVec3::new(r * libm::sin(a), 0.0, -r * libm::cos(a)) }

    #[test]
    fn aspect_bins_clockwise_from_bow() {
        let mut s = sig(SigKind::Vessel);
        assert_eq!(s.aspect_bin(at(5.0, 1_000.0)), 0);
        assert_eq!(s.aspect_bin(at(95.0, 1_000.0)), 9);
        assert_eq!(s.aspect_bin(at(185.0, 1_000.0)), 18);
        assert_eq!(s.aspect_bin(at(275.0, 1_000.0)), 27);
        assert_eq!(s.aspect_bin(at(359.9, 1_000.0)), 35);
        // Heading 090: an observer due east is dead ahead, one due north is on the port beam.
        s.heading_rad = 90f64.to_radians();
        assert_eq!(s.aspect_bin(at(92.0, 500.0)), 0);
        assert_eq!(s.aspect_bin(at(1.0, 500.0)), 27);
        // Directly overhead and bad input read bin 0.
        assert_eq!(s.aspect_bin(DVec3::new(0.0, 300.0, 0.0)), 0);
        assert_eq!(s.aspect_bin(DVec3::new(f64::NAN, 0.0, 1.0)), 0);
        s.heading_rad = f64::NAN;
        assert!(s.aspect_bin(at(45.0, 10.0)) < 36);
    }

    #[test]
    fn areas_by_kind() {
        let mut v = sig(SigKind::Vessel);
        v.areas = Some(table());
        assert_eq!(v.rcs_toward(at(95.0, 1e3)), 90.0);
        assert_eq!(v.visual_area_toward(at(95.0, 1e3)), 9.0);
        assert_eq!(v.submerged_area_toward(at(95.0, 1e3)), 109.0);
        // Bodies: flat RCS, disc above water when surfaced, below when submerged.
        let def_areas = Arc::new(ProjectedAreas { rcs_m2: [0.1; 36], above_m2: [0.785; 36], submerged_m2: [0.785; 36], top_height_m: 0.5 });
        let mut b = sig(SigKind::Body(MunitionIdx(0)));
        b.areas = Some(def_areas);
        b.rcs_flat_m2 = 0.1;
        b.top_m = 5.0;
        assert_eq!(b.rcs_toward(at(10.0, 1e3)), 0.1);
        assert!((b.visual_area_toward(at(10.0, 1e3)) - 0.785).abs() < 1e-6);
        assert_eq!(b.submerged_area_toward(at(10.0, 1e3)), 0.0);
        b.top_m = 0.0;
        b.depth_m = 20.0;
        assert!(b.is_submerged());
        assert_eq!(b.visual_area_toward(at(10.0, 1e3)), 0.0);
        assert!((b.submerged_area_toward(at(10.0, 1e3)) - 0.785).abs() < 1e-6);
        // Decoys: radar only.
        let mut d = sig(SigKind::Decoy(DecoyKind::Chaff));
        d.rcs_flat_m2 = 1_000.0;
        assert_eq!(d.rcs_toward(at(10.0, 1e3)), 1_000.0);
        assert_eq!(d.visual_area_toward(at(10.0, 1e3)), 0.0);
        d.rcs_flat_m2 = f64::NAN;
        assert_eq!(d.rcs_toward(at(10.0, 1e3)), 0.0);
    }

    #[test]
    fn hull_noise_law() {
        // 2 MW of propulsion is N = 1; doubled above cavitation speed.
        assert_eq!(hull_noise(2_000.0, 10.0, 20.0), 1.0);
        assert_eq!(hull_noise(2_000.0, 20.0, 20.0), 1.0);
        assert_eq!(hull_noise(2_000.0, 20.5, 20.0), 2.0);
        assert_eq!(hull_noise(0.0, 30.0, 20.0), 0.0);
        assert_eq!(hull_noise(f64::NAN, 30.0, 20.0), 0.0);
        assert_eq!(hull_noise(4_000.0, 30.0, f64::INFINITY), 2.0);
    }

    #[test]
    fn body_tables_and_ir() {
        assert_eq!(body_ir_mw(MunitionKind::Torpedo, BodyPhase::Cruise), 0.0);
        assert_eq!(body_ir_mw(MunitionKind::Aircraft, BodyPhase::Dead), 0.0);
        assert!(body_ir_mw(MunitionKind::Aircraft, BodyPhase::Cruise) > 0.0);
        assert!((wrap_tau(-0.5) - (core::f64::consts::TAU - 0.5)).abs() < 1e-12);
        assert_eq!(wrap_tau(f64::NAN), 0.0);
        assert!((compass_bearing(DVec3::new(1.0, 0.0, 0.0)) - core::f64::consts::FRAC_PI_2).abs() < 1e-12);
        assert!((compass_bearing(DVec3::new(0.0, 0.0, 1.0)) - core::f64::consts::PI).abs() < 1e-12);
    }
}
