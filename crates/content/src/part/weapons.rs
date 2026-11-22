//! Contract-owned types (no work unit). Weapon tables: calibre bands, munitions, small bodies.
//! DESIGN.md L557-597 (projectiles, penetration, missiles and underwater weapons, small bodies).
//!
//! Numbers are data (`crates/content/data/weapons.ron`) because gate fallbacks rewrite them by script commit
//! (L620). [`BandDef::spec_default`] and the example file carry the spec's starting values.
use crate::enums::{Domain, Phase};
use crate::ids::{IconId, ModelId, MunitionId, TextKey};
use crate::part::SensorDef;
use serde::{Deserialize, Serialize};

/// Gravity in m/s^2 (L557).
pub const GRAVITY: f64 = 9.81;
/// Penetration velocity exponent: P(v) = P_1km * (v / v_1km)^1.4 (L563).
pub const PEN_EXPONENT: f64 = 1.4;
/// Obliquity above which a shell ricochets and is deleted, degrees (L563).
pub const RICOCHET_DEG: f64 = 70.0;
/// Penetration cost of one unarmoured solid cell in millimetres (L576).
pub const UNARMOURED_CELL_MM: f64 = 2.0;

/// Calibre band; one drag coefficient and one damage row each (L565-570).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum CalibreBand {
    /// 20-40 mm autocannon.
    Auto,
    /// 57-76 mm.
    Medium,
    /// 100-127 mm.
    Large,
    /// 130-155 mm heavy, Act 3 (L551).
    Heavy,
}
impl CalibreBand { pub const ALL: [CalibreBand; 4] = [Self::Auto, Self::Medium, Self::Large, Self::Heavy]; }

/// One row of the penetration table (L565-570).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BandDef {
    pub band: CalibreBand,
    /// Penetration at 1 km in mm (input).
    pub p_1km_mm: f64,
    /// Penetration at the cap range in mm (fit target; CI fails if the fitted drag misses by over 5%).
    pub p_cap_mm: f64,
    /// Effective-range cap in metres (also the engagement cap, L489).
    pub cap_range_m: f64,
    /// Fuse delay in solid cells after the first solid cell hit (L578).
    pub fuse_delay_cells: u8,
    /// Path damage K in buckets per solid cell crossed (L577).
    pub path_k: u8,
    /// HE damage B in buckets at the burst centre (L580).
    pub he_b: u8,
    /// HE radius r in cells (L580).
    pub he_r_cells: u8,
    /// Reference muzzle velocity used by the drag fit, m/s.
    pub ref_muzzle_mps: f64,
    /// Quadratic drag k in 1/m (a = -k |v| v), fitted by `cargo xtask fit-drag` from the rows above (L563).
    pub drag_k: f64,
}
impl BandDef {
    /// The spec's starting row for a band (L565-570). `ref_muzzle_mps` and `drag_k` are placeholders until the
    /// fit runs; `data-weapons` commits the fitted values.
    pub fn spec_default(band: CalibreBand) -> BandDef {
        let (p1, pc, cap, fuse, k, b, r, v0) = match band {
            CalibreBand::Auto => (20.0, 8.0, 3_000.0, 1, 2, 4, 1, 1_050.0),
            CalibreBand::Medium => (45.0, 25.0, 8_000.0, 2, 3, 6, 2, 925.0),
            CalibreBand::Large => (90.0, 55.0, 12_000.0, 4, 4, 8, 3, 850.0),
            CalibreBand::Heavy => (150.0, 100.0, 18_000.0, 6, 4, 8, 5, 880.0),
        };
        BandDef { band, p_1km_mm: p1, p_cap_mm: pc, cap_range_m: cap, fuse_delay_cells: fuse, path_k: k, he_b: b, he_r_cells: r, ref_muzzle_mps: v0, drag_k: 0.0 }
    }
}

/// Kind of munition or small body. All are flat-array bodies in sim-core, never cell vessels.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum MunitionKind {
    AntiShipMissile,
    Sam,
    PdMissile,
    Torpedo,
    Mine,
    /// Act 3 (L551).
    DepthCharge,
    Drone,
    /// Enemy strike aircraft, Act 2 (L866-875).
    Aircraft,
    /// Stock helicopter flown by an officer, Act 2.
    Helicopter,
    /// RHIB for insert, extract, boarding and recovery (L786-797).
    Rhib,
}

/// Fuse behaviour (table at L586-595).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub enum Fuse {
    /// Penetrates up to `armour_mm`, then bursts after `delay_cells` solid cells.
    Penetrate { armour_mm: f64, delay_cells: u8 },
    /// Bursts within `radius_m` of an air target; against hulls uses the autocannon band values.
    Proximity { radius_m: f64 },
    Contact,
    /// Mine: fires when a hull enters `radius_m` (10 m, L594).
    Influence { radius_m: f64 },
    /// Depth charge: sinks at `sink_mps` (3 m/s) to a set depth of 10..=150 m (L595).
    Depth { sink_mps: f64 },
    None,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SeekerKind { None, ActiveRadar, Infrared, ActiveAcoustic }

/// One munition or small-body type (L586-597).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MunitionDef {
    pub id: MunitionId,
    pub name: TextKey,
    pub kind: MunitionKind,
    #[serde(default)]
    pub phase: Phase,
    pub domain: Domain,
    /// Cruise speed in m/s (torpedo 40 kn = 20.6, 45 kn = 23.2).
    pub speed_mps: f64,
    /// Kinematic range in metres; engagement uses 80% of it (L584). Drone and aircraft: combat radius.
    pub range_m: f64,
    /// Largest lateral acceleration in m/s^2.
    pub max_accel: f64,
    /// Cruise altitude (positive) or running depth (negative) in metres. Sea-skimmers fly at 5 m.
    pub cruise_alt_m: f64,
    pub fuse: Fuse,
    /// HE B (buckets) and r (cells), L586-595.
    pub he_b: u8,
    pub he_r_cells: u8,
    pub seeker: SeekerKind,
    /// Seeker reference range in metres vs 100 m^2 (12 km for missiles, L432) and half field of view (30 deg, L523).
    #[serde(default)]
    pub seeker_r_ref_m: f64,
    #[serde(default)]
    pub seeker_fov_half_deg: f64,
    /// Proportional-navigation constant: 3 anti-ship and torpedo, 4 SAM and PD missile (L520).
    #[serde(default)]
    pub pn_n: f64,
    /// Torpedoes: the launcher steers the run-out with 1 Hz wire updates of the predicted position (L525).
    #[serde(default)]
    pub wire_guided: bool,
    /// Bounding-sphere radius in metres and hit points (L597): missile 0.5 / 1, drone 1 / 2, aircraft 5 / 6.
    pub radius_m: f64,
    pub hp: u8,
    /// Reference radar cross-section in m^2 (missile 0.1, drone 0.5, aircraft 5, L432) and acoustic noise N.
    pub rcs_m2: f64,
    #[serde(default)]
    pub noise: f64,
    pub mass_kg: f64,
    pub cost: u32,
    /// Sensors a drone, aircraft or helicopter carries.
    #[serde(default)]
    pub sensors: Vec<SensorDef>,
    /// Weapons an aircraft or helicopter carries: (munition, count).
    #[serde(default)]
    pub payload: Vec<(MunitionId, u8)>,
    /// Endurance in seconds for drones, aircraft and helicopters; 0 = one-way.
    #[serde(default)]
    pub endurance_s: f64,
    #[serde(default)]
    pub model: ModelId,
    #[serde(default)]
    pub icon: IconId,
}

/// The whole weapons table file.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WeaponTables {
    pub bands: Vec<BandDef>,
    pub munitions: Vec<MunitionDef>,
}
