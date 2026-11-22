//! Unit `sys-gauges`. The gauge strip and editor overlays. DESIGN.md L153-187.
//!
//! | Gauge | Shows |
//! | Float | displacement against the current tonnage cap, reserve buoyancy, GM, GZ curve, range of stability |
//! | Speed | top speed in knots, turning circle |
//! | Power | cruise and combat power balance |
//! | Endurance | range at cruise on current fuel |
//! | Punch | weight of fire per minute; effective range per weapon |
//! | Sight | radar horizon against three reference targets (sea-skimmer 5 m, ship mast 20 m, aircraft 1,000 m) with `4.12 (sqrt(h_antenna) + sqrt(h_target))` km; coverage by sector |
//! | Resilience | survival with 1 and 2 compartments flooded; share of systems surviving a random hit |
//! | Complexity | points from the trial battery's 10 s headless trial; Caution above 400; PvP cap 600 marked |
//! Gauges recompute within 100 ms of each edit (p95, 90 m stress hull); Complexity updates after each save.
//! Split-strip fallback (L183): Float, Speed, Power and Punch stay on the fast path; Resilience, Sight
//! coverage and Endurance move to the background and draw stale until refreshed. GM states: Fail at GM <= 0;
//! Caution at GM under 4% of beam or over 15% (`StabilityTuning`); else Pass. Every prediction here is held to
//! within 10% of the simulation by the trial battery (L229); where a gate says "gauge adopts the simulation's
//! method" the function here is replaced, not the simulation.
//!
//! # Method
//! Every physical number comes from the simulation's own laws: hydrostatics from `sim_core::hydro`
//! (`equilibrium`, `gz_curve`, `predict_speed_turn`), mass with fluids from `sim_core::mass`, detection from
//! `crate::sensors::{radar, optical}`, effective range from the solver-derived value `ShipDesign` stores per
//! mount (the same number doctrine uses, L489), shell paths from `World::mount_path_clear`. The pure parts
//! (classification, curves, power and fuel accounting, ray sweeps over the cell grid, hit sampling) live in
//! `gauges/calc.rs` so they are testable without the physics.
//!
//! # Choices the spec leaves open
//! - Cruise is the Helm's default cruise speed (`helm.cruise_speed`, 0.7 of top speed) and shaft power
//!   follows the cube of speed, so cruise burns 0.343 of full-power engine fuel.
//! - Endurance shows Caution under 100 km at cruise: such a hull cannot reach a sortie area. Briefing
//!   `Range` tags set the real per-operation limits through Fit Check.
//! - Speed shows Fail at zero top speed and Caution when the hull cannot turn (no steering) or its turning
//!   circle is wider than 8 hull lengths. A turning circle of 0 means "cannot turn".
//! - Power shows Fail at zero generation with electrical consumers fitted, Caution when combat demand exceeds
//!   generation.
//! - Punch shows Caution when the ship carries no weapon other than decoys.
//! - Sight shows Caution with no search or navigation radar, or with more than 3 of the 36 sectors blind.
//! - Resilience: a flooding case survives when the hull floats with heel under 30 degrees (the tender test's
//!   failure line, L170). Two-compartment cases are the adjacent pairs (damage across one bulkhead). Caution
//!   when any single case is lost or fewer than half the pairs survive.
//! - Sight reference targets: sea-skimmer 0.1 m^2 at 5 m, a 150 t craft of about 150 m^2 with a 20 m mast,
//!   an aircraft of 5 m^2 at 1,000 m (all L432). Visual areas follow the Signature stage: the missile and the
//!   aircraft as discs of their 0.5 m and 5 m bounding radii (L597), the craft 120 m^2 broadside above
//!   water. Detection is predicted at Beaufort 4 in clear weather, the trial battery's sea (L227); `visual_km`
//!   is the lookout's (by eye), not a camera mast's.
//! - A random hit is a 57-76 mm HE shell (the Medium band) striking an outer cell at or above the
//!   boot-topping from the side, or the deck from above; it disables every system with a footprint cell on its
//!   path or within the band's HE radius of the burst.
use super::Check;
use crate::prelude::*;
use crate::ship::ShipDesign;
use content::blueprint::Blueprint;
use sim_core::vessel::VesselPrefab;

pub(crate) mod calc;
#[cfg(test)]
mod tests;
#[cfg(test)]
pub(crate) mod testkit;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct FloatGauge { pub displacement_t: f64, pub cap_t: f64, pub reserve: f64, pub draft_m: f64, pub heel_deg: f64, pub trim_deg: f64, pub gm_m: f64, pub gm_over_beam: f64, /// (heel in degrees, righting arm in metres) at 2 degree steps (10 degree steps while a handle is dragged).
    pub gz: Vec<(f64, f64)>, pub range_of_stability_deg: f64, pub floats: bool, pub check: Check }
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SpeedGauge { pub top_speed_kn: f64, pub turning_circle_m: f64, pub check: Check }
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct PowerGauge { pub generation_kw: f64, pub cruise_demand_kw: f64, pub combat_demand_kw: f64, pub battery_kwh: f64, pub check: Check }
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct EnduranceGauge { pub cruise_kn: f64, pub range_km: f64, pub fuel_l: f64, pub check: Check }
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct PunchGauge { pub weight_of_fire_kg_per_min: f64, /// (mount, effective range in metres).
    pub effective_range_m: Vec<(PartSlot, f64)>, pub check: Check }
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SightGauge {
    /// Best antenna height in metres and the horizon in km against 5 m, 20 m and 1,000 m targets.
    pub antenna_m: f64,
    pub horizon_km: [f64; 3],
    /// Predicted first-detection range in km of the three reference targets by radar and by eye; the trial
    /// battery holds these within 10% of simulation (L438).
    pub radar_km: [f64; 3],
    pub visual_km: [f64; 3],
    /// Radar coverage per 10 degree sector from the bow: 1.0 clear, 0.0 blind.
    pub coverage: Vec<f32>,
    pub check: Check,
}
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct FloodCase { pub a: CompartmentId, pub b: Option<CompartmentId>, pub heel_deg: f64, pub trim_deg: f64, pub survives: bool }
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ResilienceGauge { pub one_flooded_survival: f64, pub two_flooded_survival: f64, pub worst: Vec<FloodCase>, pub systems_surviving_hit: f64, pub check: Check }
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ComplexityGauge { pub points: f64, pub caution_at: f64, pub pvp_cap: f64, pub check: Check }

/// Gauges on the fast path. Unit: `sys-gauges`.
///
/// Mass with fluids (fuel, ammunition and ballast as loaded), calm-water equilibrium and the GZ curve at
/// `gz_step_deg` (2 normally, 10 while a handle is dragged).
pub fn float_gauge(prefab: &VesselPrefab, cap_t: f64, gz_step_deg: f64, catalogue: &Catalogue) -> FloatGauge {
    let mass = calc::loaded_mass(prefab, catalogue);
    let tuning = &catalogue.tuning.buoyancy;
    let eq = sim_core::hydro::equilibrium(&prefab.blocks, &mass, &[], &prefab.compartments, tuning);
    let step = if gz_step_deg.is_finite() && gz_step_deg > 0.0 { gz_step_deg.clamp(0.5, 30.0) } else { 2.0 };
    let gz = sim_core::hydro::gz_curve(&prefab.blocks, &mass, step, tuning);
    calc::float_from(&eq, gz, prefab.dims[2], cap_t, &catalogue.tuning.stability)
}

/// Top speed and turning circle at full power from the hydro unit's analytic prediction.
pub fn speed_gauge(prefab: &VesselPrefab, catalogue: &Catalogue) -> SpeedGauge {
    let (top, radius) = sim_core::hydro::predict_speed_turn(prefab, catalogue, &catalogue.tuning.buoyancy);
    calc::speed_from(top, radius, prefab.dims[0])
}

/// Generation against cruise and combat demand of the fitted parts.
pub fn power_gauge(prefab: &VesselPrefab, catalogue: &Catalogue) -> PowerGauge {
    calc::power_budget(prefab, catalogue, calc::cruise_fraction()).gauge()
}

/// Weight of fire in the first minute of action and the effective range of each weapon.
pub fn punch_gauge(design: &ShipDesign, catalogue: &Catalogue) -> PunchGauge { calc::punch_from(&design.mounts, &design.prefab, catalogue) }

/// Gauges that may run on the background worker under the split strip. Unit: `sys-gauges`.
///
/// Range at cruise on the fuel now loaded: engine burn at cruise power plus generator burn for the cruise
/// electrical load; an all-electric hull with no fuel runs on its batteries.
pub fn endurance_gauge(prefab: &VesselPrefab, catalogue: &Catalogue) -> EnduranceGauge {
    let (top, _) = sim_core::hydro::predict_speed_turn(prefab, catalogue, &catalogue.tuning.buoyancy);
    let frac = calc::cruise_fraction();
    let budget = calc::power_budget(prefab, catalogue, frac);
    calc::endurance_from(top, &calc::fuel_account(prefab, catalogue), &budget, frac)
}

/// Radar horizon, predicted first detection by radar and by eye of the three reference targets, and radar
/// coverage per 10 degree sector.
pub fn sight_gauge(design: &ShipDesign, catalogue: &Catalogue) -> SightGauge { calc::sight_from(&design.prefab, catalogue) }

/// Enumerates every 1- and 2-compartment flooding case with `sim_core::hydro::equilibrium`. Unit: `sys-gauges`.
///
/// Single cases are every live compartment; pairs are the compartments that share a deck or bulkhead.
pub fn resilience_gauge(prefab: &VesselPrefab, catalogue: &Catalogue) -> ResilienceGauge {
    let mass = calc::loaded_mass(prefab, catalogue);
    let tuning = &catalogue.tuning.buoyancy;
    let (singles, pairs) = calc::flood_cases(&prefab.compartments);
    let solve = |open: &[CompartmentId]| sim_core::hydro::equilibrium(&prefab.blocks, &mass, open, &prefab.compartments, tuning);
    let one: Vec<FloodCase> = singles.iter().map(|&a| calc::flood_case(a, None, &solve(&[a]))).collect();
    let two: Vec<FloodCase> = pairs.iter().map(|&(a, b)| calc::flood_case(a, Some(b), &solve(&[a, b]))).collect();
    let hits = calc::HitModel::new(prefab, catalogue);
    let systems = hits.systems_surviving(calc::seed_of(prefab.hash), calc::RANDOM_HITS);
    calc::resilience_from(one, two, systems)
}

/// Complexity from the last trial's work counts (`trials::ComplexityCounts`). Unit: `sys-gauges`.
pub fn complexity_gauge(points: f64, catalogue: &Catalogue) -> ComplexityGauge {
    calc::complexity_from(points, catalogue.tuning.complexity.caution_points, content::ruleset::Ruleset::default().complexity_per_vessel)
}

/// The whole strip. `stale` marks gauges whose value predates the last edit (split strip: drawn in
/// `text.secondary` with the word "Updating").
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct GaugeStrip { pub float: FloatGauge, pub speed: SpeedGauge, pub power: PowerGauge, pub endurance: EnduranceGauge, pub punch: PunchGauge, pub sight: SightGauge, pub resilience: ResilienceGauge, pub complexity: ComplexityGauge, pub stale: Vec<String> }

/// Compute every gauge for a design in one call (CI, Quartermaster screening). Unit: `sys-gauges`.
///
/// The gauges read only the design built from `bp`, so the strip of a design and of its blueprint always
/// agree. Nothing is stale after a full recompute.
pub fn all_gauges(bp: &Blueprint, design: &ShipDesign, cap_t: f64, complexity_points: f64, catalogue: &Catalogue) -> GaugeStrip {
    let prefab = &design.prefab;
    GaugeStrip {
        float: float_gauge(prefab, cap_t, 2.0, catalogue),
        speed: speed_gauge(prefab, catalogue),
        power: power_gauge(prefab, catalogue),
        endurance: endurance_gauge(prefab, catalogue),
        punch: punch_gauge(design, catalogue),
        sight: sight_gauge(design, catalogue),
        resilience: resilience_gauge(prefab, catalogue),
        complexity: complexity_gauge(complexity_points, catalogue),
        stale: Vec::new(),
    }
}

/// One arc of a weapon-arc overlay: clear, or blocked by own structure (hatched `alert.warning`, L168).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct ArcSector { pub from_deg: f64, pub to_deg: f64, pub blocked: bool, pub range_m: f64 }

/// Weapon arcs on the water for one mount. Unit: `sys-gauges`.
///
/// Bearings are degrees clockwise from the bow in 0..360, and each sector runs clockwise from `from_deg` to
/// `to_deg`. The sweep runs in 1 degree steps with flat fire (elevation 0, raised to the mount's minimum
/// elevation when that is positive). A trainable mount is swept all round: bearings own structure masks are
/// blocked sectors, clear bearings inside the limits in force are clear sectors, and clear bearings outside
/// them are not drawn. A fixed launcher is swept over its limits. Empty when the slot is not a mount.
pub fn weapon_arcs(design: &ShipDesign, slot: PartSlot, catalogue: &Catalogue) -> Vec<ArcSector> {
    let Some(m) = design.mounts.iter().find(|m| m.slot == slot) else { return Vec::new() };
    let prefab = &design.prefab;
    let mut clear = |t: f64, e: f64| sim_core::World::mount_path_clear(prefab, slot, t, e, catalogue);
    calc::sweep_arcs(&m.limits, calc::trainable(m, catalogue), m.zero_rad, m.effective_range_m, &mut clear).sectors
}

/// Sensor blind sectors for one sensor. Unit: `sys-gauges`.
///
/// 36 sectors of 10 degrees clockwise from the bow, merged where neighbours agree. A sector is blocked when
/// own structure (or the edge of a staring sensor's field of view) cuts half or more of its sampled
/// horizontal rays. `range_m` is the sensor's predicted range against the reference ship. Empty when the slot
/// is not a sensor.
pub fn blind_sectors(design: &ShipDesign, slot: PartSlot, catalogue: &Catalogue) -> Vec<ArcSector> { calc::blind_sectors_of(&design.prefab, slot, catalogue) }

/// Vulnerability heatmap: per outer cell 0..=255, the share of sampled hits through that cell that disable a
/// system. Deterministic sampling keyed on the design hash. Unit: `sys-gauges`.
pub fn vulnerability_heatmap(design: &ShipDesign, catalogue: &Catalogue) -> Vec<(CellIndex, u8)> {
    calc::HitModel::new(&design.prefab, catalogue).heatmap(calc::seed_of(design.hash))
}

/// Radar cross-section, visual area and submerged area per aspect for the editor's signature view (L404).
/// Unit: `sys-gauges`.
pub fn signature_view(prefab: &VesselPrefab, catalogue: &Catalogue) -> sim_core::vessel::ProjectedAreas { sim_core::vessel::projected_areas_prefab(prefab, catalogue) }
