//! Pure parts of the gauges: classification, curves, power and fuel accounting, sight predictions, ray sweeps
//! over the cell grid and hit sampling. Everything here is deterministic and reads no clock; transcendental
//! functions go through libm.
use super::{ArcSector, ComplexityGauge, EnduranceGauge, FloatGauge, FloodCase, PowerGauge, PunchGauge, ResilienceGauge, SightGauge, SpeedGauge};
use crate::doctrine::MountInfo;
use crate::editor::Check;
use crate::prelude::*;
use content::cell::{CellPos, CELL_M};
use content::graph::{MountLimits, WeaponClass};
use content::part::weapons::{BandDef, CalibreBand};
use content::part::{EngineKind, PartCategory, PartDef, PartFunction, PortRole, SensorKind};
use content::tuning::{SensorTuning, StabilityTuning};
use sim_core::compartments::CompartmentSet;
use sim_core::grid::VesselGrid;
use sim_core::hydro::Equilibrium;
use sim_core::mass::MassProperties;
use sim_core::parts::{PartRuntime, PartState};
use sim_core::rng::{Key, Stream};
use sim_core::vessel::VesselPrefab;

// ---------------------------------------------------------------------------------------------------------
// Constants for the choices the spec leaves open (documented in the module header of `gauges.rs`).

/// Endurance at cruise below this shows Caution, km.
pub(crate) const ENDURANCE_CAUTION_KM: f64 = 100.0;
/// Turning circles wider than this many hull lengths show Caution.
pub(crate) const TURN_CAUTION_LENGTHS: f64 = 8.0;
/// A flooding case survives with heel under this many degrees (the tender test's failure line, L170).
pub(crate) const SURVIVE_HEEL_DEG: f64 = 30.0;
/// Pair cases computed at most (ascending order); a 90 m hull has a few hundred adjacencies.
pub(crate) const MAX_PAIR_CASES: usize = 4_096;
/// Sight Caution when more than this many of the 36 sectors are blind.
pub(crate) const BLIND_SECTOR_CAUTION: usize = 3;
/// Random hits sampled for "share of systems surviving a random hit".
pub(crate) const RANDOM_HITS: u32 = 256;
/// Hits sampled through each outer cell for the vulnerability heatmap.
pub(crate) const HEAT_SAMPLES: u32 = 4;
/// Largest deviation of a sampled shell path from the face normal, degrees.
const HIT_JITTER_DEG: f64 = 20.0;
/// Rays sampled per 10 degree coverage sector.
const RAYS_PER_SECTOR: u32 = 5;
/// Lookout eye height above the bridge console, metres (same as the sensor pipeline).
const LOOKOUT_EYE_M: f64 = crate::sensors::LOOKOUT_EYE_M;
/// Reference targets of the Sight gauge: sea-skimming missile, a 150 t craft with a 20 m mast, aircraft.
pub(crate) const REF_HEIGHT_M: [f64; 3] = [5.0, 20.0, 1_000.0];
pub(crate) const REF_RCS_M2: [f64; 3] = [0.1, 150.0, 5.0];
/// Visual areas: the missile and the aircraft as discs of their bounding-sphere radius (0.5 m and 5 m, L597),
/// as the Signature stage gives small bodies; the 150 t craft's broadside above water.
pub(crate) const REF_VISUAL_M2: [f64; 3] = [core::f64::consts::PI * 0.25, 120.0, core::f64::consts::PI * 25.0];

fn fin(x: f64) -> f64 { if x.is_finite() { x } else { 0.0 } }
fn pos(x: f64) -> f64 { if x.is_finite() && x > 0.0 { x } else { 0.0 } }
fn cube(x: f64) -> f64 { x * x * x }

/// Catalogue definition of a runtime part; `None` for an index the catalogue does not hold.
pub(crate) fn def_of<'c>(catalogue: &'c Catalogue, p: &PartRuntime) -> Option<&'c PartDef> { catalogue.parts.get_index(p.def.index()).map(|(_, d)| d) }

/// The Helm's default cruise speed as a fraction of top speed (`helm.cruise_speed`).
pub(crate) fn cruise_fraction() -> f64 {
    crate::officers::tuning::slider("helm.cruise_speed").map(|s| s.default).filter(|v| v.is_finite() && *v > 0.0 && *v <= 1.0).unwrap_or(0.7)
}

/// Mass with fuel, ammunition, ballast and any flood water as loaded.
pub(crate) fn loaded_mass(prefab: &VesselPrefab, catalogue: &Catalogue) -> MassProperties { sim_core::mass::with_fluids(&prefab.dry_mass, &prefab.parts, &prefab.compartments, catalogue) }

/// 64-bit sampling seed from a design hash.
pub(crate) fn seed_of(hash: ContentHash) -> u64 {
    let mut b = [0u8; 8];
    b.copy_from_slice(&hash.0[..8]);
    u64::from_le_bytes(b)
}

// ---------------------------------------------------------------------------------------------------------
// Float.

/// First heel past which the righting arm stops being positive, interpolated between samples. 0 when the
/// curve never rises above zero (no positive stability), the last heel when it never falls back.
pub(crate) fn range_of_stability(gz: &[(f64, f64)]) -> f64 {
    let mut seen_positive = false;
    let mut last = 0.0;
    for w in gz.windows(2) {
        let ((h0, g0), (h1, g1)) = (w[0], w[1]);
        if !(h0.is_finite() && h1.is_finite() && g0.is_finite() && g1.is_finite()) { continue; }
        if g0 > 0.0 { seen_positive = true; }
        if seen_positive && g0 > 0.0 && g1 <= 0.0 {
            let t = if g0 - g1 > 0.0 { g0 / (g0 - g1) } else { 0.0 };
            return h0 + (h1 - h0) * t;
        }
        last = h1;
    }
    if let Some(&(h, g)) = gz.last() { if g.is_finite() && g > 0.0 { seen_positive = true; } if h.is_finite() { last = h; } }
    if seen_positive { last } else { 0.0 }
}

/// GM state (L168): Fail at or below zero, Caution under the low or over the high fraction of beam.
pub(crate) fn gm_check(gm_m: f64, beam_m: f64, t: &StabilityTuning) -> Check {
    if !(gm_m.is_finite() && gm_m > 0.0) { return Check::Fail; }
    if !(beam_m.is_finite() && beam_m > 0.0) { return Check::Caution; }
    let r = gm_m / beam_m;
    let (lo, hi) = (pos(t.gm_caution_low), if t.gm_caution_high.is_finite() && t.gm_caution_high > 0.0 { t.gm_caution_high } else { f64::INFINITY });
    if r < lo || r > hi { Check::Caution } else { Check::Pass }
}

pub(crate) fn float_from(eq: &Equilibrium, gz: Vec<(f64, f64)>, beam_m: f64, cap_t: f64, t: &StabilityTuning) -> FloatGauge {
    let gz: Vec<(f64, f64)> = gz.into_iter().filter(|(h, g)| h.is_finite() && g.is_finite()).collect();
    let disp = pos(eq.displacement_t);
    let cap = pos(cap_t);
    let gm = fin(eq.gm_m);
    let beam = pos(beam_m);
    let mut check = if eq.floats { gm_check(gm, beam, t) } else { Check::Fail };
    if check == Check::Pass && cap > 0.0 && disp > cap { check = Check::Caution; }
    FloatGauge {
        displacement_t: disp,
        cap_t: cap,
        reserve: fin(eq.reserve),
        draft_m: fin(eq.draft_m),
        heel_deg: fin(eq.heel_rad).to_degrees(),
        trim_deg: fin(eq.trim_rad).to_degrees(),
        gm_m: gm,
        gm_over_beam: if beam > 0.0 { gm / beam } else { 0.0 },
        range_of_stability_deg: if eq.floats { range_of_stability(&gz) } else { 0.0 },
        gz,
        floats: eq.floats,
        check,
    }
}

// ---------------------------------------------------------------------------------------------------------
// Speed.

pub(crate) fn speed_from(top_mps: f64, radius_m: f64, length_m: f64) -> SpeedGauge {
    let top_kn = pos(top_mps) / sim_core::time::KN;
    let circle = if radius_m.is_finite() && radius_m > 0.0 { 2.0 * radius_m } else { 0.0 };
    let check = if top_kn <= 0.0 {
        Check::Fail
    } else if circle <= 0.0 || (pos(length_m) > 0.0 && circle > TURN_CAUTION_LENGTHS * length_m) {
        Check::Caution
    } else {
        Check::Pass
    };
    SpeedGauge { top_speed_kn: top_kn, turning_circle_m: circle, check }
}

// ---------------------------------------------------------------------------------------------------------
// Power and fuel.

/// Electrical balance and fuel burners of a design.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct PowerBudget {
    pub generation_kw: f64,
    pub cruise_kw: f64,
    pub combat_kw: f64,
    pub battery_kwh: f64,
    /// (rated kW, fuel L/s at full load) of each generator.
    pub generators: Vec<(f64, f64)>,
    /// Full-throttle fuel burn of every fuel-burning engine, L/s.
    pub engine_fuel_lps: f64,
    /// Shaft power of every engine, kW.
    pub shaft_kw: f64,
    /// Electrical consumers, ascending slot.
    pub consumers: Vec<PartSlot>,
}
impl PowerBudget {
    pub fn gauge(&self) -> PowerGauge {
        let check = if self.combat_kw > 0.0 && self.generation_kw <= 0.0 {
            Check::Fail
        } else if self.combat_kw > self.generation_kw {
            Check::Caution
        } else {
            Check::Pass
        };
        PowerGauge { generation_kw: self.generation_kw, cruise_demand_kw: self.cruise_kw, combat_demand_kw: self.combat_kw, battery_kwh: self.battery_kwh, check }
    }
}

/// Power ports of a part: (supply kW, consume kW, store kWh).
fn power_ports(def: &PartDef) -> (f64, f64, f64) {
    let (mut s, mut c, mut st) = (0.0, 0.0, 0.0);
    for p in &def.ports {
        if p.network != NetworkKind::Power { continue; }
        match p.role { PortRole::Supply => s += pos(p.rate), PortRole::Consume => c += pos(p.rate), PortRole::Store => st += pos(p.capacity), PortRole::Link => {} }
    }
    (s, c, st)
}

/// Electrical draw of a part at full rating (electric motors draw their shaft power, L: `EngineDef::fuel_lps`).
pub(crate) fn rated_draw_kw(def: &PartDef) -> f64 {
    let (_, c, _) = power_ports(def);
    match &def.function { PartFunction::Engine(e) if e.kind == EngineKind::ElectricMotor => c.max(pos(e.shaft_kw)), _ => c }
}

/// Share of a consumer's rated draw it uses while cruising: weapons, fire control, special-operations gear and
/// pumps idle; electric propulsion runs at cruise power (cube of the speed fraction); everything else
/// (steering, data, consoles, search sensors, hotel load) runs.
pub(crate) fn cruise_share(def: &PartDef, cruise_fraction: f64) -> f64 {
    match &def.function {
        PartFunction::Engine(e) if e.kind == EngineKind::ElectricMotor => return cube(cruise_fraction),
        PartFunction::Sensor(s) if s.kind == SensorKind::FireControlRadar => return 0.0,
        PartFunction::Director | PartFunction::Jammer { .. } | PartFunction::Pump { .. } | PartFunction::CameraMast { .. } | PartFunction::Crane | PartFunction::RhibDavit | PartFunction::BoardingKit { .. } | PartFunction::RecoveryNet | PartFunction::MineRail { .. } => return 0.0,
        _ => {}
    }
    match def.category { PartCategory::Gun | PartCategory::Missile | PartCategory::Underwater | PartCategory::Defence | PartCategory::Offboard | PartCategory::SpecialOps => 0.0, _ => 1.0 }
}

pub(crate) fn power_budget(prefab: &VesselPrefab, catalogue: &Catalogue, cruise_fraction: f64) -> PowerBudget {
    let mut b = PowerBudget::default();
    for p in &prefab.parts {
        let Some(def) = def_of(catalogue, p) else { continue };
        if !p.works() { continue; }
        let out = p.output_factor();
        let (supply, _, store) = power_ports(def);
        match &def.function {
            PartFunction::Generator { kw, fuel_lps } => {
                let g = pos(*kw).max(supply) * out;
                b.generation_kw += g;
                b.generators.push((g, pos(*fuel_lps)));
            }
            PartFunction::Battery { kwh, .. } => b.battery_kwh += pos(*kwh).max(store),
            PartFunction::Engine(e) => {
                b.shaft_kw += pos(e.shaft_kw) * out;
                if e.kind != EngineKind::ElectricMotor { b.engine_fuel_lps += pos(e.fuel_lps); }
                b.generation_kw += supply * out;
            }
            _ => { b.generation_kw += supply * out; b.battery_kwh += store; }
        }
        let draw = rated_draw_kw(def);
        if draw > 0.0 {
            b.combat_kw += draw;
            b.cruise_kw += draw * cruise_share(def, cruise_fraction);
            b.consumers.push(p.slot);
        }
    }
    b.consumers.sort_unstable();
    b
}

/// Fuel aboard and tank capacity, litres.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct FuelAccount { pub fuel_l: f64, pub capacity_l: f64 }

pub(crate) fn fuel_account(prefab: &VesselPrefab, catalogue: &Catalogue) -> FuelAccount {
    let mut f = FuelAccount::default();
    for p in &prefab.parts {
        let Some(def) = def_of(catalogue, p) else { continue };
        let PartFunction::FuelTank { litres } = def.function else { continue };
        let cap = pos(litres);
        let now = match p.state { PartState::FuelTank { litres } => pos(litres).min(cap), _ => cap };
        f.capacity_l += cap;
        f.fuel_l += now;
    }
    f
}

/// Fuel burn at cruise in L/s: engines at the cube of the speed fraction, generators in proportion to the
/// cruise electrical load.
pub(crate) fn cruise_burn_lps(budget: &PowerBudget, cruise_fraction: f64) -> f64 {
    let p = cube(cruise_fraction.clamp(0.0, 1.0));
    let mut burn = budget.engine_fuel_lps * p;
    if budget.generation_kw > 0.0 {
        let load = (budget.cruise_kw / budget.generation_kw).clamp(0.0, 1.0);
        burn += budget.generators.iter().map(|(_, lps)| lps * load).sum::<f64>();
    }
    burn
}

pub(crate) fn endurance_from(top_mps: f64, fuel: &FuelAccount, budget: &PowerBudget, cruise_fraction: f64) -> EnduranceGauge {
    let cruise_mps = pos(top_mps) * cruise_fraction.clamp(0.0, 1.0);
    let burn = cruise_burn_lps(budget, cruise_fraction);
    let seconds = if cruise_mps <= 0.0 {
        0.0
    } else if burn > 0.0 {
        fuel.fuel_l / burn
    } else if budget.cruise_kw > 0.0 && budget.battery_kwh > 0.0 && budget.engine_fuel_lps <= 0.0 {
        // All-electric with no fuel burner: run the batteries flat.
        budget.battery_kwh / budget.cruise_kw * 3_600.0
    } else {
        0.0
    };
    let range_km = pos(cruise_mps * seconds / 1_000.0);
    let check = if range_km < ENDURANCE_CAUTION_KM { Check::Caution } else { Check::Pass };
    EnduranceGauge { cruise_kn: cruise_mps / sim_core::time::KN, range_km, fuel_l: pos(fuel.fuel_l), check }
}

// ---------------------------------------------------------------------------------------------------------
// Punch.

/// Weight of fire in the first minute of one mount, kg: a gun fires its ready rounds plus what its hoist
/// feeds, capped by its rate of fire; a launcher empties as many cells as its launch interval allows.
pub(crate) fn mount_weight_per_min(m: &MountInfo, hoist_rpm: Option<f64>, catalogue: &Catalogue) -> f64 {
    let Some((_, def)) = catalogue.parts.get_index(m.part.index()) else { return 0.0 };
    match &def.function {
        PartFunction::Gun(g) => {
            let rate = pos(g.rof_rpm) * g.barrels.max(1) as f64;
            let fed = g.ready_rounds as f64 + hoist_rpm.map(pos).unwrap_or(0.0);
            rate.min(fed) * pos(g.shell_kg)
        }
        PartFunction::Launcher(l) => {
            let Some(mass) = m.munition.and_then(|i| catalogue.munitions.get_index(i.index())).map(|(_, d)| pos(d.mass_kg)) else { return 0.0 };
            let cells = l.cells as f64;
            let launches = if l.interval_s.is_finite() && l.interval_s > 0.0 { cells.min(1.0 + libm::floor(60.0 / l.interval_s)) } else { cells };
            launches * mass
        }
        _ => 0.0,
    }
}

pub(crate) fn punch_from(mounts: &[MountInfo], prefab: &VesselPrefab, catalogue: &Catalogue) -> PunchGauge {
    // Hoists feed the gun above them: the nearest hoist within 3 m horizontally.
    let hoists: Vec<(DVec3, f64)> = prefab.parts.iter().filter_map(|p| match def_of(catalogue, p).map(|d| &d.function) { Some(PartFunction::Hoist { rounds_per_min }) if p.works() => Some((p.centre, pos(*rounds_per_min))), _ => None }).collect();
    let mut kg = 0.0;
    let mut ranges = Vec::new();
    let mut armed = false;
    for m in mounts {
        if m.weapon == WeaponClass::Decoy { continue; }
        let hoist = hoists.iter().filter(|(c, _)| { let d = DVec2::new(c.x - m.pos_local.x, c.z - m.pos_local.z); d.length() <= 3.0 }).min_by(|a, b| {
            let da = DVec2::new(a.0.x - m.pos_local.x, a.0.z - m.pos_local.z).length();
            let db = DVec2::new(b.0.x - m.pos_local.x, b.0.z - m.pos_local.z).length();
            da.partial_cmp(&db).unwrap_or(core::cmp::Ordering::Equal)
        }).map(|h| h.1);
        kg += mount_weight_per_min(m, hoist, catalogue);
        ranges.push((m.slot, pos(m.effective_range_m)));
        armed = true;
    }
    ranges.sort_by_key(|r| r.0);
    PunchGauge { weight_of_fire_kg_per_min: kg, effective_range_m: ranges, check: if armed { Check::Pass } else { Check::Caution } }
}

// ---------------------------------------------------------------------------------------------------------
// Sight.

/// One sensor as the editor sees it: where it is and how far it reaches.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct SensorSite {
    pub slot: PartSlot,
    pub kind: SensorKind,
    pub r_ref_m: f64,
    /// Vessel-local antenna position, metres, and height above the design waterline.
    pub centre: DVec3,
    pub height_m: f64,
    pub fov_half_deg: f64,
    /// Boresight bearing clockwise from the bow, degrees (staring sensors).
    pub boresight_deg: f64,
}

/// Reference range of a sensor, the spec's starting value when the part leaves it unset (as the sensor
/// pipeline does).
fn r_ref_of(kind: SensorKind, r_ref_m: f64, t: &SensorTuning) -> f64 {
    if r_ref_m.is_finite() && r_ref_m > 0.0 { return r_ref_m; }
    match kind {
        SensorKind::SearchRadar | SensorKind::NavRadar => crate::sensors::DEFAULT_SEARCH_R_REF_M,
        SensorKind::FireControlRadar => crate::sensors::DEFAULT_FIRE_CONTROL_R_REF_M,
        SensorKind::Seeker => crate::sensors::DEFAULT_SEEKER_R_REF_M,
        SensorKind::PassiveSonar => t.passive_sonar_r_ref_m,
        SensorKind::ActiveSonar => t.active_sonar_r_ref_m,
        SensorKind::Visual | SensorKind::Camera => t.visual_ref_range_m,
        SensorKind::Esm => 0.0,
    }
}

/// Bearing clockwise from the bow of a part's own +X axis after its rotation, degrees.
pub(crate) fn facing_deg(rot: content::kit::PartRot) -> f64 {
    let m = content::kit::part_rot_matrix(rot);
    let (x, z) = (m[0][0] as f64, m[0][2] as f64);
    if x == 0.0 && z == 0.0 { 0.0 } else { wrap360(libm::atan2(z, x).to_degrees()) }
}

pub(crate) fn sensor_site(p: &PartRuntime, def: &PartDef, waterline_y: f64, t: &SensorTuning) -> Option<SensorSite> {
    let (kind, r_ref, fov) = match &def.function {
        PartFunction::Sensor(s) => (s.kind, r_ref_of(s.kind, s.r_ref_m, t), if s.rpm.is_some() { 180.0 } else { s.fov_half_deg }),
        PartFunction::CameraMast { fov_deg, .. } => (SensorKind::Camera, t.visual_ref_range_m, 180.0_f64.min(pos(*fov_deg) * 0.5).max(0.0)),
        _ => return None,
    };
    // A trainable camera turns to its target; its narrow lens is not a blind arc.
    let fov = if kind == SensorKind::Camera { 180.0 } else if fov.is_finite() && fov > 0.0 { fov.min(180.0) } else { 180.0 };
    let centre = if p.centre.is_finite() { p.centre } else { DVec3::ZERO };
    Some(SensorSite { slot: p.slot, kind, r_ref_m: r_ref, centre, height_m: centre.y - fin(waterline_y), fov_half_deg: fov, boresight_deg: facing_deg(p.rot) })
}

pub(crate) fn sensor_sites(prefab: &VesselPrefab, catalogue: &Catalogue) -> Vec<SensorSite> {
    let t = &catalogue.tuning.sensors;
    prefab.parts.iter().filter(|p| p.works()).filter_map(|p| def_of(catalogue, p).and_then(|d| sensor_site(p, d, prefab.waterline_y, t))).collect()
}

/// Lookout eye: on the highest console plus eye height, else the hull top at mid-length. `None` with no crew.
pub(crate) fn lookout_height_m(prefab: &VesselPrefab, catalogue: &Catalogue) -> Option<f64> {
    if prefab.berths == 0 { return None; }
    let eye = prefab.parts.iter().filter(|p| matches!(def_of(catalogue, p).map(|d| &d.function), Some(PartFunction::Console)) && p.centre.is_finite()).map(|p| p.centre.y).fold(None, |a: Option<f64>, y| Some(a.map_or(y, |a| a.max(y))));
    let y = match eye { Some(y) => y + LOOKOUT_EYE_M, None => fin(prefab.dims[1]) };
    Some(y - fin(prefab.waterline_y))
}

fn is_search_radar(k: SensorKind) -> bool { matches!(k, SensorKind::SearchRadar | SensorKind::NavRadar) }

/// Predicted range against the reference ship, for blind-sector overlays.
fn site_range_m(s: &SensorSite, clutter: f64, visibility_m: f64, t: &SensorTuning) -> f64 {
    use crate::sensors::{optical, radar};
    match s.kind {
        SensorKind::SearchRadar | SensorKind::NavRadar | SensorKind::FireControlRadar | SensorKind::Seeker => radar::radar_ranges(s.r_ref_m, s.height_m, REF_RCS_M2[1], REF_HEIGHT_M[1], clutter, None, t).final_m,
        SensorKind::Visual | SensorKind::Camera => optical::visual_range_m(REF_VISUAL_M2[1], s.kind == SensorKind::Camera, false, false, visibility_m, radar::horizon_m(s.height_m, REF_HEIGHT_M[1]), t),
        SensorKind::Esm => radar::horizon_m(s.height_m, t.assumed_enemy_mast_m),
        SensorKind::PassiveSonar | SensorKind::ActiveSonar => pos(s.r_ref_m),
    }
}

/// Sight predictions from sensor heights alone (coverage comes from the grid; see [`sector_coverage`]).
/// `visual_km` is by eye (the lookout, L438); camera masts serve identification and photography.
pub(crate) fn sight_numbers(radars: &[SensorSite], lookout_m: Option<f64>, clutter: f64, visibility_m: f64, t: &SensorTuning) -> ([f64; 3], [f64; 3], [f64; 3], f64) {
    use crate::sensors::{optical, radar};
    let antenna = radars.iter().map(|r| r.height_m).fold(None, |a: Option<f64>, h| Some(a.map_or(h, |a| a.max(h)))).or(lookout_m).unwrap_or(0.0).max(0.0);
    let mut horizon = [0.0; 3];
    let mut radar_km = [0.0; 3];
    let mut visual_km = [0.0; 3];
    for i in 0..3 {
        horizon[i] = radar::horizon_m(antenna, REF_HEIGHT_M[i]) / 1_000.0;
        radar_km[i] = radars.iter().map(|r| radar::radar_ranges(r.r_ref_m, r.height_m, REF_RCS_M2[i], REF_HEIGHT_M[i], clutter, None, t).final_m).fold(0.0, f64::max) / 1_000.0;
        visual_km[i] = lookout_m.map(|h| optical::visual_range_m(REF_VISUAL_M2[i], false, false, false, visibility_m, radar::horizon_m(h, REF_HEIGHT_M[i]), t)).unwrap_or(0.0) / 1_000.0;
    }
    (horizon, radar_km, visual_km, antenna)
}

pub(crate) fn wrap360(d: f64) -> f64 {
    if !d.is_finite() { return 0.0; }
    let r = d - 360.0 * libm::floor(d / 360.0);
    if r >= 360.0 { 0.0 } else { r }
}

/// Smallest angle between two bearings, degrees.
fn angle_between(a: f64, b: f64) -> f64 { let d = wrap360(a - b); d.min(360.0 - d) }

/// Vessel-local unit direction of a horizontal bearing clockwise from the bow (+Z is starboard).
pub(crate) fn bearing_dir(bearing_deg: f64) -> DVec3 {
    let r = bearing_deg.to_radians();
    DVec3::new(libm::cos(r), 0.0, libm::sin(r))
}

/// True when the straight path from `from_m` along `dir` meets no solid cell other than those `own` owns.
pub(crate) fn ray_clear(grid: &VesselGrid, own: Option<PartSlot>, from_m: DVec3, dir: DVec3) -> bool {
    let s = grid.size();
    let len = DVec3::new(s[0] as f64, s[1] as f64, s[2] as f64).length() + 2.0;
    let a = from_m / CELL_M;
    let b = a + dir.normalize_or_zero() * len;
    let mut clear = true;
    grid.traverse([a.x, a.y, a.z], [b.x, b.y, b.z], &mut |p, _| {
        if grid.is_solid(p) && (own.is_none() || grid.cell_type(p).part != own) { clear = false; return false; }
        true
    });
    clear
}

/// Clear share of each of the 36 sectors for one sensor (rays at the antenna, horizontal).
pub(crate) fn sector_coverage(grid: &VesselGrid, s: &SensorSite) -> Vec<f32> {
    let structural = !matches!(s.kind, SensorKind::PassiveSonar | SensorKind::ActiveSonar);
    (0..36).map(|sec| {
        let mut clear = 0;
        for k in 0..RAYS_PER_SECTOR {
            let b = sec as f64 * 10.0 + (k as f64 + 0.5) * 10.0 / RAYS_PER_SECTOR as f64;
            if s.fov_half_deg < 180.0 && angle_between(b, s.boresight_deg) > s.fov_half_deg { continue; }
            if !structural || ray_clear(grid, Some(s.slot), s.centre, bearing_dir(b)) { clear += 1; }
        }
        clear as f32 / RAYS_PER_SECTOR as f32
    }).collect()
}

pub(crate) fn sight_check(radars: usize, coverage: &[f32]) -> Check {
    if radars == 0 || coverage.iter().filter(|c| **c < 0.5).count() > BLIND_SECTOR_CAUTION { Check::Caution } else { Check::Pass }
}

/// Beaufort 4, clear weather: the trial battery's conditions (L227).
fn reference_weather() -> WeatherState { WeatherState::default() }

pub(crate) fn sight_from(prefab: &VesselPrefab, catalogue: &Catalogue) -> SightGauge {
    let t = &catalogue.tuning.sensors;
    let weather = reference_weather();
    let clutter = crate::sensors::clutter_for(&weather, t);
    let sites = sensor_sites(prefab, catalogue);
    let radars: Vec<SensorSite> = sites.iter().copied().filter(|s| is_search_radar(s.kind)).collect();
    let lookout = lookout_height_m(prefab, catalogue);
    let (horizon_km, radar_km, visual_km, antenna_m) = sight_numbers(&radars, lookout, clutter, weather.visibility_m, t);
    let mut coverage = vec![0.0f32; 36];
    for r in &radars {
        for (c, v) in coverage.iter_mut().zip(sector_coverage(&prefab.grid, r)) { *c = c.max(v); }
    }
    let check = sight_check(radars.len(), &coverage);
    SightGauge { antenna_m, horizon_km, radar_km, visual_km, coverage, check }
}

/// Merge 36 sector states into clockwise arcs (one 0..360 arc when all agree).
pub(crate) fn merge_sectors(blocked: &[bool], range_m: f64) -> Vec<ArcSector> {
    let n = blocked.len();
    if n == 0 { return Vec::new(); }
    let width = 360.0 / n as f64;
    if blocked.iter().all(|b| *b == blocked[0]) { return vec![ArcSector { from_deg: 0.0, to_deg: 360.0, blocked: blocked[0], range_m }]; }
    // Start at a state change so no run wraps.
    let start = (0..n).find(|&i| blocked[i] != blocked[(i + n - 1) % n]).unwrap_or(0);
    let mut out = Vec::new();
    let mut i = 0;
    while i < n {
        let s = (start + i) % n;
        let state = blocked[s];
        let mut len = 1;
        while i + len < n && blocked[(start + i + len) % n] == state { len += 1; }
        out.push(ArcSector { from_deg: wrap360(s as f64 * width), to_deg: wrap360((s + len) as f64 * width), blocked: state, range_m });
        i += len;
    }
    out.sort_by(|a, b| a.from_deg.partial_cmp(&b.from_deg).unwrap_or(core::cmp::Ordering::Equal));
    out
}

pub(crate) fn blind_sectors_of(prefab: &VesselPrefab, slot: PartSlot, catalogue: &Catalogue) -> Vec<ArcSector> {
    let t = &catalogue.tuning.sensors;
    let Some(p) = prefab.parts.iter().find(|p| p.slot == slot) else { return Vec::new() };
    let Some(site) = def_of(catalogue, p).and_then(|d| sensor_site(p, d, prefab.waterline_y, t)) else { return Vec::new() };
    let weather = reference_weather();
    let range = site_range_m(&site, crate::sensors::clutter_for(&weather, t), weather.visibility_m, t);
    let cov = sector_coverage(&prefab.grid, &site);
    let blocked: Vec<bool> = cov.iter().map(|c| *c < 0.5).collect();
    merge_sectors(&blocked, range)
}

// ---------------------------------------------------------------------------------------------------------
// Weapon arcs.

/// Result of a training sweep.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct ArcSweep {
    pub sectors: Vec<ArcSector>,
    /// Blocked share of the sampled steps.
    pub blocked_fraction: f64,
    /// Widest blocked sector (clockwise from, to) in degrees.
    pub widest_blocked: Option<(f64, f64)>,
    /// Bearing at the middle of the widest blocked sector, degrees clockwise from the bow.
    pub widest_mid_deg: Option<f64>,
}

/// Clockwise bearing from the bow of a mount's training angle (training is positive to port, L: gunnery).
pub(crate) fn train_to_bearing(zero_rad: f64, train_deg: f64) -> f64 { wrap360(-(fin(zero_rad).to_degrees() + train_deg)) }

/// Whether a mount trains: guns always, launchers with a training rate; fixed launchers do not.
pub(crate) fn trainable(m: &MountInfo, catalogue: &Catalogue) -> bool {
    match catalogue.parts.get_index(m.part.index()).map(|(_, d)| &d.function) {
        Some(PartFunction::Gun(_)) => true,
        Some(PartFunction::Launcher(l)) => l.train_rate_dps.is_some(),
        _ => false,
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Step { Clear, Blocked, Omitted }

/// Sweep a mount at 1 degree steps; `clear(train_rad, elevation_rad)` tests one shell path. A trainable
/// mount is swept all round, because auto-set limits already leave out every masked bearing: masked steps
/// are blocked sectors wherever they are, clear steps outside the limits in force are left out of the
/// overlay, and the blocked fraction is over the whole circle. A fixed mount is swept over its limits only.
pub(crate) fn sweep_arcs(limits: &MountLimits, trains_all_round: bool, zero_rad: f64, range_m: f64, clear: &mut dyn FnMut(f64, f64) -> bool) -> ArcSweep {
    let range = pos(range_m);
    let (mut tmin, mut tmax) = (limits.train_min_deg, limits.train_max_deg);
    if !(tmin.is_finite() && tmax.is_finite()) { tmin = -180.0; tmax = 180.0; }
    if tmax < tmin { core::mem::swap(&mut tmin, &mut tmax); }
    let limits_full = tmax - tmin >= 359.5;
    let full = limits_full || trains_all_round;
    let (mut emin, mut emax) = (fin(limits.elevation_min_deg), fin(limits.elevation_max_deg));
    if emax < emin { core::mem::swap(&mut emin, &mut emax); }
    let elev = 0.0f64.clamp(emin, emax).to_radians();
    let zero_deg = fin(zero_rad).to_degrees();
    // Training angles of the steps, ordered clockwise in bearing.
    let trains: Vec<f64> = if full {
        (0..360).map(|b| wrap360(-(b as f64) - zero_deg + 180.0) - 180.0).collect()
    } else {
        let lo = libm::ceil(tmin);
        let n = (libm::floor(tmax) - lo).max(0.0) as usize + 1;
        let mut v: Vec<f64> = (0..n).map(|i| (lo + i as f64).min(tmax)).collect();
        if v.is_empty() { v.push(tmin); }
        v.reverse(); // training increases to port, bearings clockwise
        v
    };
    let inside = |t: f64| limits_full || (t >= tmin - 1e-9 && t <= tmax + 1e-9) || (t + 360.0 >= tmin - 1e-9 && t + 360.0 <= tmax + 1e-9) || (t - 360.0 >= tmin - 1e-9 && t - 360.0 <= tmax + 1e-9);
    let steps: Vec<(f64, Step)> = trains.iter().map(|&t| {
        let st = if !clear(t.to_radians(), elev) { Step::Blocked } else if inside(t) { Step::Clear } else { Step::Omitted };
        (train_to_bearing(zero_rad, t), st)
    }).collect();
    let blocked_n = steps.iter().filter(|s| s.1 == Step::Blocked).count();
    let mut runs: Vec<(usize, usize, Step)> = Vec::new(); // (first step, length, state)
    for (i, s) in steps.iter().enumerate() {
        match runs.last_mut() { Some(r) if r.2 == s.1 => r.1 += 1, _ => runs.push((i, 1, s.1)) }
    }
    if full && runs.len() > 1 && runs[0].2 == runs[runs.len() - 1].2 {
        let last = runs.pop().unwrap_or((0, 0, Step::Omitted));
        runs[0] = (last.0, last.1 + runs[0].1, last.2);
    }
    let n = steps.len();
    let mut sectors = Vec::new();
    let mut widest: Option<(usize, (f64, f64), f64)> = None;
    if full && runs.len() == 1 {
        if runs[0].2 != Step::Omitted { sectors.push(ArcSector { from_deg: 0.0, to_deg: 360.0, blocked: runs[0].2 == Step::Blocked, range_m: range }); }
        if runs[0].2 == Step::Blocked { widest = Some((n, (0.0, 360.0), 0.0)); }
    } else {
        for &(first, len, state) in &runs {
            if state == Step::Omitted { continue; }
            let b0 = steps[first % n].0;
            let b1 = steps[(first + len - 1) % n].0;
            let from = wrap360(b0 - 0.5);
            let to = wrap360(b1 + 0.5);
            let blocked = state == Step::Blocked;
            sectors.push(ArcSector { from_deg: from, to_deg: to, blocked, range_m: range });
            if blocked && widest.is_none_or(|w| len > w.0) { widest = Some((len, (from, to), wrap360(b0 + (len as f64 - 1.0) * 0.5))); }
        }
    }
    sectors.sort_by(|a, b| a.from_deg.partial_cmp(&b.from_deg).unwrap_or(core::cmp::Ordering::Equal));
    ArcSweep { sectors, blocked_fraction: if n == 0 { 0.0 } else { blocked_n as f64 / n as f64 }, widest_blocked: widest.map(|w| w.1), widest_mid_deg: widest.map(|w| w.2) }
}

// ---------------------------------------------------------------------------------------------------------
// Resilience.

/// Live compartments, and adjacent pairs among them, ascending.
pub(crate) fn flood_cases(cs: &CompartmentSet) -> (Vec<CompartmentId>, Vec<(CompartmentId, CompartmentId)>) {
    let live: BTreeSet<CompartmentId> = cs.compartments.iter().filter(|c| !c.is_absorbed() && !c.cells.is_empty() && c.volume_m3 > 0.0).map(|c| c.id).collect();
    let mut pairs: BTreeSet<(CompartmentId, CompartmentId)> = BTreeSet::new();
    for a in &cs.adjacency {
        if a.a == a.b || !live.contains(&a.a) || !live.contains(&a.b) { continue; }
        pairs.insert(if a.a < a.b { (a.a, a.b) } else { (a.b, a.a) });
    }
    (live.into_iter().collect(), pairs.into_iter().take(MAX_PAIR_CASES).collect())
}

pub(crate) fn flood_case(a: CompartmentId, b: Option<CompartmentId>, eq: &Equilibrium) -> FloodCase {
    let heel = fin(eq.heel_rad).to_degrees();
    let trim = fin(eq.trim_rad).to_degrees();
    FloodCase { a, b, heel_deg: heel, trim_deg: trim, survives: eq.floats && heel.abs() < SURVIVE_HEEL_DEG }
}

pub(crate) fn resilience_from(one: Vec<FloodCase>, two: Vec<FloodCase>, systems_surviving_hit: f64) -> ResilienceGauge {
    let share = |v: &[FloodCase]| if v.is_empty() { 1.0 } else { v.iter().filter(|c| c.survives).count() as f64 / v.len() as f64 };
    let (s1, s2) = (share(&one), share(&two));
    let mut all: Vec<FloodCase> = one.into_iter().chain(two).collect();
    all.sort_by(|x, y| {
        x.survives.cmp(&y.survives)
            .then_with(|| y.heel_deg.abs().partial_cmp(&x.heel_deg.abs()).unwrap_or(core::cmp::Ordering::Equal))
            .then_with(|| (x.a, x.b).cmp(&(y.a, y.b)))
    });
    all.truncate(5);
    let check = if s1 < 1.0 || s2 < 0.5 { Check::Caution } else { Check::Pass };
    ResilienceGauge { one_flooded_survival: s1, two_flooded_survival: s2, worst: all, systems_surviving_hit: fin(systems_surviving_hit).clamp(0.0, 1.0), check }
}

/// The shell band used for random hits.
pub(crate) fn hit_band(catalogue: &Catalogue) -> BandDef { catalogue.bands.iter().find(|b| b.band == CalibreBand::Medium).copied().unwrap_or_else(|| BandDef::spec_default(CalibreBand::Medium)) }

/// Hit sampling against the cells and parts of one design.
pub(crate) struct HitModel<'a> {
    grid: &'a VesselGrid,
    /// True per slot for parts that count as systems (everything but inert structure and ballast).
    system: Vec<bool>,
    n_systems: usize,
    fuse_cells: u32,
    he_r: i32,
    /// Outer cells with the inward face normal a shell enters along.
    outer: Vec<(CellPos, DVec3)>,
}

impl<'a> HitModel<'a> {
    pub fn new(prefab: &'a VesselPrefab, catalogue: &Catalogue) -> Self {
        let band = hit_band(catalogue);
        let max_slot = prefab.parts.iter().map(|p| p.slot.index() + 1).max().unwrap_or(0);
        let mut system = vec![false; max_slot];
        for p in &prefab.parts {
            let sys = def_of(catalogue, p).is_some_and(|d| !matches!(d.function, PartFunction::Structure | PartFunction::Ballast { .. }));
            if sys { system[p.slot.index()] = true; }
        }
        let n_systems = system.iter().filter(|s| **s).count();
        let wl = (libm::floor(fin(prefab.waterline_y) / CELL_M) as i32 - 1).max(0);
        Self { grid: &prefab.grid, system, n_systems, fuse_cells: band.fuse_delay_cells as u32, he_r: band.he_r_cells as i32, outer: outer_cells(&prefab.grid, wl) }
    }

    pub fn outer(&self) -> &[(CellPos, DVec3)] { &self.outer }

    /// Systems disabled by one shell entering at `entry` along `dir`.
    pub fn hit(&self, entry: CellPos, dir: DVec3) -> BTreeSet<PartSlot> {
        let mut out = BTreeSet::new();
        let dir = dir.normalize_or_zero();
        if dir == DVec3::ZERO { return out; }
        let s = self.grid.size();
        let len = DVec3::new(s[0] as f64, s[1] as f64, s[2] as f64).length() + 2.0;
        let c = DVec3::new(entry.x as f64 + 0.5, entry.y as f64 + 0.5, entry.z as f64 + 0.5);
        let a = c - dir * 0.75;
        let b = a + dir * len;
        let mut solids = 0u32;
        let mut burst: Option<CellPos> = None;
        let fuse = self.fuse_cells;
        let grid = self.grid;
        grid.traverse([a.x, a.y, a.z], [b.x, b.y, b.z], &mut |p, _| {
            if grid.is_solid(p) {
                solids += 1;
                if solids > fuse { burst = Some(p); return false; }
            }
            true
        });
        let Some(at) = burst else { return out };
        let r = self.he_r.max(0);
        for dy in -r..=r { for dz in -r..=r { for dx in -r..=r {
            if dx * dx + dy * dy + dz * dz > r * r { continue; }
            let q = CellPos::new((at.x as i32 + dx) as i16, (at.y as i32 + dy) as i16, (at.z as i32 + dz) as i16);
            if (at.x as i32 + dx) < 0 || (at.y as i32 + dy) < 0 || (at.z as i32 + dz) < 0 { continue; }
            if let Some(slot) = grid.cell_type(q).part {
                if self.system.get(slot.index()).copied().unwrap_or(false) { out.insert(slot); }
            }
        }}}
        out
    }

    fn jittered(&self, dir: DVec3, seed: u64, a: u64, b: u64) -> DVec3 {
        let (u, v) = perpendiculars(dir);
        let j = HIT_JITTER_DEG.to_radians();
        let x = (sim_core::rng::uniform(Key::new(seed, 0, Stream::Misc, a, b).draw(1)) * 2.0 - 1.0) * j;
        let y = (sim_core::rng::uniform(Key::new(seed, 0, Stream::Misc, a, b).draw(2)) * 2.0 - 1.0) * j;
        (dir + u * libm::tan(x) + v * libm::tan(y)).normalize_or_zero()
    }

    /// Share of systems that survive one random hit, averaged over `n` hits keyed on `seed`.
    pub fn systems_surviving(&self, seed: u64, n: u32) -> f64 {
        if self.n_systems == 0 || self.outer.is_empty() || n == 0 { return 1.0; }
        let count = self.outer.len().min(u32::MAX as usize) as u32;
        let mut lost = 0.0;
        for k in 0..n {
            let i = sim_core::rng::below(Key::new(seed, 0, Stream::Misc, k as u64, 0), count) as usize;
            let (cell, dir) = self.outer[i];
            let d = self.jittered(dir, seed, k as u64, 1);
            lost += self.hit(cell, d).len() as f64 / self.n_systems as f64;
        }
        (1.0 - lost / n as f64).clamp(0.0, 1.0)
    }

    /// Per outer cell, the share of sampled hits through it that disable a system, 0..=255, ascending cell.
    pub fn heatmap(&self, seed: u64) -> Vec<(CellIndex, u8)> {
        let mut acc: BTreeMap<CellIndex, (u32, u32)> = BTreeMap::new();
        if self.n_systems == 0 {
            for (cell, _) in &self.outer { if let Some(i) = self.grid.index(*cell) { acc.entry(i).or_insert((0, 0)).1 += 1; } }
        } else {
            for (k, (cell, dir)) in self.outer.iter().enumerate() {
                let Some(i) = self.grid.index(*cell) else { continue };
                let e = acc.entry(i).or_insert((0, 0));
                for j in 0..HEAT_SAMPLES {
                    let d = if j == 0 { *dir } else { self.jittered(*dir, seed, i.0 as u64, ((k as u64) << 8) | j as u64) };
                    e.1 += 1;
                    if !self.hit(*cell, d).is_empty() { e.0 += 1; }
                }
            }
        }
        acc.into_iter().map(|(i, (hit, n))| (i, if n == 0 { 0 } else { libm::round(255.0 * hit as f64 / n as f64).clamp(0.0, 255.0) as u8 })).collect()
    }
}

fn perpendiculars(d: DVec3) -> (DVec3, DVec3) {
    let a = if d.x.abs() < 0.9 { DVec3::X } else { DVec3::Y };
    let u = d.cross(a).normalize_or_zero();
    (u, d.cross(u).normalize_or_zero())
}

/// Outer cells: the first solid cell from each side of every row at or above `min_y` (entering along +Z or
/// -Z), and the top solid cell of every column (entering downward). Ascending scan order, deduplicated by
/// (cell, direction).
pub(crate) fn outer_cells(grid: &VesselGrid, min_y: i32) -> Vec<(CellPos, DVec3)> {
    let [sx, sy, sz] = grid.size();
    let (sx, sy, sz) = (sx as i32, sy as i32, sz as i32);
    let mut out = Vec::new();
    let solid = |x: i32, y: i32, z: i32| grid.is_solid(CellPos::new(x as i16, y as i16, z as i16));
    for y in min_y.max(0)..sy {
        for x in 0..sx {
            if let Some(z) = (0..sz).find(|&z| solid(x, y, z)) {
                out.push((CellPos::new(x as i16, y as i16, z as i16), DVec3::Z));
                if let Some(z2) = (0..sz).rev().find(|&z| solid(x, y, z)) { if z2 != z || sz == 1 { out.push((CellPos::new(x as i16, y as i16, z2 as i16), -DVec3::Z)); } }
            }
        }
    }
    for z in 0..sz {
        for x in 0..sx {
            if let Some(y) = (0..sy).rev().find(|&y| solid(x, y, z)) { out.push((CellPos::new(x as i16, y as i16, z as i16), -DVec3::Y)); }
        }
    }
    out
}

// ---------------------------------------------------------------------------------------------------------
// Complexity.

pub(crate) fn complexity_from(points: f64, caution_at: f64, pvp_cap: f64) -> ComplexityGauge {
    let points = pos(points);
    let caution = if caution_at.is_finite() && caution_at > 0.0 { caution_at } else { 400.0 };
    ComplexityGauge { points, caution_at: caution, pvp_cap: pos(pvp_cap), check: if points > caution { Check::Caution } else { Check::Pass } }
}
