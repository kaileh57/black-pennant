//! Contract-owned types (no work unit; values are maintained by `data-tuning`). The versioned tuning table:
//! every number a gate fallback may rewrite by script commit, as data in `crates/content/data/tuning.ron`.
//! `Default` impls carry the spec's starting values with the governing DESIGN.md line in each doc comment.
//! Fixed constants the spec never retunes (tick rate, split work budget) are Rust consts in sim-core.
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct TuningTable {
    /// Bumped by every fallback commit; recorded in replays and saves.
    pub version: u32,
    pub stability: StabilityTuning,
    pub flooding: FloodingTuning,
    pub fire: FireTuning,
    pub crew: CrewTuning,
    pub structure: StructureTuning,
    pub buoyancy: BuoyancyTuning,
    pub projectiles: ProjectileTuning,
    pub decoys: DecoyTuning,
    pub waves: WaveTuning,
    pub networks: NetworkTuning,
    pub sensors: SensorTuning,
    pub tracks: TrackTuning,
    pub guidance: GuidanceTuning,
    pub doctrine: DoctrineTuning,
    pub complexity: ComplexityCoefficients,
    pub special_ops: SpecialOpsTuning,
}

/// GM gauge thresholds as fractions of beam (L168-175).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct StabilityTuning { pub gm_caution_low: f64, pub gm_caution_high: f64, pub capsize_heel_deg: f64, pub capsize_hold_s: f64 }
impl Default for StabilityTuning { fn default() -> Self { Self { gm_caution_low: 0.04, gm_caution_high: 0.15, capsize_heel_deg: 90.0, capsize_hold_s: 10.0 } } }

/// Flooding (L622-648).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct FloodingTuning {
    /// Orifice discharge coefficient Cd = 0.6; never retuned, pacing uses `flood_rate_scale` (L624, L632).
    pub cd: f64,
    pub flood_rate_scale: f64,
    /// Flood valve area per below-waterline compartment, m^2 (L648).
    pub valve_m2: f64,
    /// Seconds for the fire main to fill a magazine (L648).
    pub magazine_flood_s: f64,
    /// Bisection steps for a compartment's water plane (L634).
    pub level_bisection_steps: u8,
    /// A compartment over this fill fraction cannot burn, and its berths do not count (L664, L704).
    pub flooded_fraction: f64,
    /// Replication rate of compartment water levels in Hz, as u8 fill fractions (L640).
    pub replication_hz: f64,
}
impl Default for FloodingTuning { fn default() -> Self { Self { cd: 0.6, flood_rate_scale: 1.0, valve_m2: 0.1, magazine_flood_s: 10.0, level_bisection_steps: 16, flooded_fraction: 0.5, replication_hz: 2.0 } } }

/// Fire (L660-669).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct FireTuning {
    pub ignite_near_fuel: f64,
    pub ignite_near_ammo: f64,
    /// Cells within which a fuel or ammunition cell counts as near (L662).
    pub ignite_radius_cells: u8,
    /// Chance per second to ignite each adjacent compartment; halved across a closed bulkhead (L663).
    pub spread_per_s: f64,
    pub start_intensity: f64,
    /// Part health lost per second in a burning compartment (L665).
    pub burn_health_per_s: f64,
    pub cookoff_delay_s: f64,
    pub cookoff_r_min_cells: u8,
    pub cookoff_r_max_cells: u8,
    /// Intensity removed per second per assigned team (L667).
    pub suppression_per_team: f64,
    /// Default Damage Control floods a burning magazine when this many seconds of delay remain (L669).
    pub autoflood_remaining_s: f64,
}
impl Default for FireTuning { fn default() -> Self { Self { ignite_near_fuel: 0.15, ignite_near_ammo: 0.25, ignite_radius_cells: 2, spread_per_s: 0.01, start_intensity: 100.0, burn_health_per_s: 0.01, cookoff_delay_s: 120.0, cookoff_r_min_cells: 4, cookoff_r_max_cells: 24, suppression_per_team: 2.0, autoflood_remaining_s: 60.0 } } }

/// Crew capacity and damage-control jobs (L701-727).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct CrewTuning {
    pub berths_per_team: u8,
    pub min_teams: u8,
    pub max_teams: u8,
    pub jury_rig_s: f64,
    pub jury_rig_capacity: f64,
    pub patch_face_s: f64,
    pub repair_job_s: f64,
    /// Health added per repair job (25 percentage points) and the cap (75%, bucket 2), L714.
    pub repair_gain: f64,
    pub repair_cap: f64,
    /// Crew veterancy cap (+15%, L811-822). The gain rule and the affected stats need an owner decision.
    pub veterancy_cap: f64,
}
impl Default for CrewTuning { fn default() -> Self { Self { berths_per_team: 4, min_teams: 1, max_teams: 6, jury_rig_s: 20.0, jury_rig_capacity: 0.5, patch_face_s: 10.0, repair_job_s: 30.0, repair_gain: 0.25, repair_cap: 0.75, veterancy_cap: 0.15 } } }

/// Splitting and wrecks (L682-699). The 50,000-cell work budget is a sim-core const, changed only by commit.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct StructureTuning { pub min_piece_cells: u32, pub max_wrecks: u8, pub wreck_ttl_s: f64, pub wreck_despawn_depth_m: f64 }
impl Default for StructureTuning { fn default() -> Self { Self { min_piece_cells: 64, max_wrecks: 16, wreck_ttl_s: 600.0, wreck_despawn_depth_m: 20.0 } } }

/// Buoyancy sampling (L650, L658).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct BuoyancyTuning {
    /// Block edge in cells: 4 = 2 m cubes.
    pub block_cells: u8,
    /// Fallback: hulls farther than `far_distance_m` from every player hull use `far_block_cells`. Off by default.
    pub far_blocks_enabled: bool,
    pub far_block_cells: u8,
    pub far_distance_m: f64,
    pub water_density: f64,
    /// Hull drag coefficients used by the hydro unit (form, skin friction, lateral, angular damping).
    pub drag_form: f64,
    pub drag_skin: f64,
    pub drag_lateral: f64,
    pub drag_angular: f64,
}
impl Default for BuoyancyTuning { fn default() -> Self { Self { block_cells: 4, far_blocks_enabled: false, far_block_cells: 8, far_distance_m: 2_000.0, water_density: 1_025.0, drag_form: 0.35, drag_skin: 0.003, drag_lateral: 1.1, drag_angular: 0.8 } } }

/// Projectile caps and point-defence rounds (L557, L597).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct ProjectileTuning {
    /// Live caps by degradation rung: 2,000 then 1,500 then 1,000.
    pub live_caps: [u32; 3],
    pub pd_self_destruct_s: f64,
    /// Point-defence rounds hit small bodies within this distance of the sphere surface, metres (0.5..=3.0).
    pub pd_proximity_m: f64,
    /// Broadphase hash grid cell in metres (L559).
    pub broadphase_cell_m: f64,
}
impl Default for ProjectileTuning { fn default() -> Self { Self { live_caps: [2_000, 1_500, 1_000], pd_self_destruct_s: 4.0, pd_proximity_m: 1.5, broadphase_cell_m: 128.0 } } }

/// Defence objects (L599-604, L524).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct DecoyTuning {
    pub chaff_rcs_m2: f64, pub chaff_bloom_s: f64, pub chaff_life_s: f64,
    pub flare_mw: f64, pub flare_life_s: f64,
    pub towed_distance_m: f64, pub towed_rcs_factor: f64, pub towed_noise: f64,
    pub smoke_radius_m: f64, pub smoke_height_m: f64, pub smoke_grow_s: f64, pub smoke_life_s: f64,
    /// Seeker resolution cell: a decoy within this distance of the locked return competes (L524).
    pub resolution_cell_m: f64,
}
impl Default for DecoyTuning { fn default() -> Self { Self { chaff_rcs_m2: 1_000.0, chaff_bloom_s: 3.0, chaff_life_s: 60.0, flare_mw: 20.0, flare_life_s: 5.0, towed_distance_m: 200.0, towed_rcs_factor: 2.0, towed_noise: 4.0, smoke_radius_m: 60.0, smoke_height_m: 30.0, smoke_grow_s: 10.0, smoke_life_s: 120.0, resolution_cell_m: 300.0 } } }

/// Wave set (L1116-1127, L1887).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct WaveTuning {
    /// 12 by default; the wave gates may commit 8 or 16. Never above 16.
    pub component_count: u8,
    pub min_wavelength_m: f64,
    pub max_wavelength_m: f64,
    /// Sets with sum(Q k A) above this are rejected (L1887).
    pub max_steepness_sum: f64,
    pub swell_count: u8,
}
impl Default for WaveTuning { fn default() -> Self { Self { component_count: 12, min_wavelength_m: 8.0, max_wavelength_m: 400.0, max_steepness_sum: 0.85, swell_count: 2 } } }

/// Network buffers (L394).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct NetworkTuning { pub battery_s: f64, pub day_tank_s: f64, pub local_control_error_factor: f64 }
impl Default for NetworkTuning { fn default() -> Self { Self { battery_s: 30.0, day_tank_s: 60.0, local_control_error_factor: 3.0 } } }

/// Detection model constants designers may not retune by hand but gate fallbacks may (L410-456).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct SensorTuning {
    pub search_radar_hz: f64,
    pub sigma_ref_m2: f64,
    /// Clutter C at calm, Beaufort 4 and Beaufort 7 (L427).
    pub clutter: [f64; 3],
    pub clutter_full_below_m: f64,
    pub clutter_zero_above_m: f64,
    pub jammer_r_bt0_m: f64,
    pub esm_range_factor: f64,
    pub visual_ref_range_m: f64,
    pub night_visual_factor: f64,
    pub camera_range_factor: f64,
    pub passive_sonar_r_ref_m: f64,
    pub active_sonar_r_ref_m: f64,
    pub torpedo_noise: f64,
    pub layer_cross_factor: f64,
    pub shadow_zone_m: f64,
    pub spatial_grid_cell_m: f64,
    /// Assumed enemy mast height for the ESM warning, metres (L510).
    pub assumed_enemy_mast_m: f64,
}
impl Default for SensorTuning { fn default() -> Self { Self { search_radar_hz: 5.0, sigma_ref_m2: 100.0, clutter: [0.0, 0.5, 2.0], clutter_full_below_m: 5.0, clutter_zero_above_m: 15.0, jammer_r_bt0_m: 10_000.0, esm_range_factor: 2.0, visual_ref_range_m: 10_000.0, night_visual_factor: 0.3, camera_range_factor: 2.0, passive_sonar_r_ref_m: 8_000.0, active_sonar_r_ref_m: 4_000.0, torpedo_noise: 1.0, layer_cross_factor: 0.3, shadow_zone_m: 5_000.0, spatial_grid_cell_m: 5_000.0, assumed_enemy_mast_m: 20.0 } } }

/// Track filter constants (L458-468). Order of `sigma_a`: surface or subsurface, aircraft, missiles, bearing-only.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct TrackTuning { pub sigma_a: [f64; 4], pub gate_sigma: f64, pub drop_surface_s: f64, pub drop_air_s: f64, pub last_known_s: f64, pub bearing_only_min_range_fraction: f64 }
impl Default for TrackTuning { fn default() -> Self { Self { sigma_a: [0.3, 20.0, 50.0, 0.5], gate_sigma: 3.03, drop_surface_s: 120.0, drop_air_s: 20.0, last_known_s: 600.0, bearing_only_min_range_fraction: 0.10 } } }

/// Guidance constants (L516-527).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct GuidanceTuning { pub gun_iteration_cap: u8, pub gun_converge_m: f64, pub pn_antiship: f64, pub pn_sam: f64, pub seeker_fov_half_deg: f64, pub midcourse_hz: f64 }
impl Default for GuidanceTuning { fn default() -> Self { Self { gun_iteration_cap: 6, gun_converge_m: 0.25, pn_antiship: 3.0, pn_sam: 4.0, seeker_fov_half_deg: 30.0, midcourse_hz: 1.0 } } }

/// Engagement doctrine defaults (L479-492, L505-512).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct DoctrineTuning {
    pub effective_range_pk: f64,
    pub self_defence_range_m: f64,
    pub pd_gun_cap_m: f64,
    pub pd_missile_cap_m: f64,
    pub missile_range_fraction: f64,
    pub inbound_cpa_m: f64,
    pub inbound_horizon_s: f64,
    pub decoy_trigger_m: f64,
    pub burst_trigger_m: f64,
    pub burst_rotations: u8,
    pub burst_repeat_s: f64,
    pub burst_repeat_engaged_s: f64,
    pub silent_pd_radar_m: f64,
}
impl Default for DoctrineTuning { fn default() -> Self { Self { effective_range_pk: 0.10, self_defence_range_m: 4_000.0, pd_gun_cap_m: 2_000.0, pd_missile_cap_m: 8_000.0, missile_range_fraction: 0.8, inbound_cpa_m: 500.0, inbound_horizon_s: 60.0, decoy_trigger_m: 6_000.0, burst_trigger_m: 20_000.0, burst_rotations: 3, burst_repeat_s: 30.0, burst_repeat_engaged_s: 10.0, silent_pd_radar_m: 8_000.0 } } }

/// Complexity point coefficients per unit of work (L973-976). Fitted on the owner's Mac; the coefficients are zeros until then.
/// The PvP caps (600 per vessel, 3,600 per lobby) live in `content::ruleset::Ruleset`.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct ComplexityCoefficients { pub cuboid: f64, pub buoyancy_sample: f64, pub conduit_segment: f64, pub compartment: f64, pub graph_node_eval: f64, pub luau_interrupt: f64, /// Editor gauge shows Caution above this many points (400).
    pub caution_points: f64 }
impl Default for ComplexityCoefficients { fn default() -> Self { Self { cuboid: 0.0, buoyancy_sample: 0.0, conduit_segment: 0.0, compartment: 0.0, graph_node_eval: 0.0, luau_interrupt: 0.0, caution_points: 400.0 } } }

/// Special-operations action rules (L786-797).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct SpecialOpsTuning {
    pub board_range_m: f64, pub board_max_speed_kn: f64, pub board_s: f64,
    pub hold_max_speed_kn: f64, pub hold_range_m: f64, pub hold_s: f64,
    pub mine_spacing_m: f64, pub mine_course_tolerance_deg: f64, pub mine_min_speed_kn: f64,
    pub photo_hold_s: f64,
}
impl Default for SpecialOpsTuning { fn default() -> Self { Self { board_range_m: 200.0, board_max_speed_kn: 5.0, board_s: 90.0, hold_max_speed_kn: 3.0, hold_range_m: 100.0, hold_s: 60.0, mine_spacing_m: 50.0, mine_course_tolerance_deg: 5.0, mine_min_speed_kn: 6.0, photo_hold_s: 20.0 } } }
