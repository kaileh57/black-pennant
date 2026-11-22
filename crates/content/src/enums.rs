//! Contract-owned (no work unit). Small enums shared by content data, the simulation and the wire protocol.
use serde::{Deserialize, Serialize};

/// Campaign act. Hull tonnage caps 150 / 400 / 900 / 1,800 t (L94, L811-822).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum Act { #[default] Act1, Act2, Act3, Act4 }
impl Act {
    pub const ALL: [Act; 4] = [Act::Act1, Act::Act2, Act::Act3, Act::Act4];
    /// Per-hull tonnage cap in tonnes (L94).
    pub fn hull_cap_t(self) -> f64 { match self { Act::Act1 => 150.0, Act::Act2 => 400.0, Act::Act3 => 900.0, Act::Act4 => 1800.0 } }
    pub fn number(self) -> u8 { self as u8 + 1 }
}

/// Part tier 1 to 5. Tier 1 at campaign start, 2-3 inside Act 1, 4 in Act 2, 5 in Act 3 (L801-807).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum Tier { #[default] T1, T2, T3, T4, T5 }

/// Release phase a feature, part or data row belongs to. Lets the scheduler and the build flag later work.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum Phase {
    /// Milestone 1 vertical slice: one playable sortie, the editor loop and the tutorial.
    #[default] Slice,
    /// First public release 0.1.0 (Act 1).
    R010,
    Act2, Act3, Act4,
    /// 1.0 items (4-player co-op, 3v3 and 4v4, PvP escort, remaining night presets).
    R100,
    /// L4 Luau scripting; scheduled last.
    L4,
}

/// The three ship-wide networks (L281-283). Coolant and hydraulics are cut.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum NetworkKind { Power, Fuel, Data }

/// Track and target domain (L463, L465).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum Domain { #[default] Surface, Subsurface, Air, Weapon }

/// Track stance (L496-503).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum Stance { Friend, Neutral, #[default] Suspect, Hostile }

/// Identification rung (L470).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum IdRung { #[default] Unknown, Domain, Type, Class, Exact }

/// Track quality grade, coarsest first (L466).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum TrackGrade { #[default] Bearing, Coarse, Fine, Lock }

/// Emission control setting (L505-507).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum Emcon { Silent, #[default] Burst, Active }

/// L0 doctrine preset (L328).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum DoctrinePreset { Aggressive, #[default] Cautious, SilentRunning }

/// The five default officers (L301-311).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum OfficerRole { Helm, Engineer, Sensors, Gunnery, DamageControl }
impl OfficerRole { pub const ALL: [OfficerRole; 5] = [Self::Helm, Self::Engineer, Self::Sensors, Self::Gunnery, Self::DamageControl]; }

/// Control ladder layer (L322-332).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum ControlLayer { #[default] L0Doctrine, L1Tune, L2Rules, L3Graph, L4Script }

/// UI damage level derived from a 0..=8 bucket (L561).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum DamageLevel { #[default] Intact, Damaged, Heavy, Destroyed }
impl DamageLevel {
    /// Intact 0-1, Damaged 2-4, Heavy 5-7, Destroyed 8 (L561).
    pub fn from_bucket(bucket: u8) -> Self { match bucket { 0..=1 => Self::Intact, 2..=4 => Self::Damaged, 5..=7 => Self::Heavy, _ => Self::Destroyed } }
    /// Part output factor: Intact and Damaged 1.0, Heavy 0.5, Destroyed 0.0 (L561).
    pub fn output_factor(self) -> f64 { match self { Self::Intact | Self::Damaged => 1.0, Self::Heavy => 0.5, Self::Destroyed => 0.0 } }
}

/// Game mode of one session (L73, L957-963).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum GameMode {
    #[default] Campaign,
    Skirmish,
    Sandbox,
    PvpElimination,
    /// 1.0 item (Phase::R100).
    PvpEscort,
    /// Editor test basin (L214-229).
    Splash,
    /// The first-ten-minutes tutorial (L252-275).
    Tutorial,
    /// Headless trial battery and Fit Check runs (L227, L801).
    Trial,
}

/// Time-of-day band from sun elevation: night under 3 degrees, dusk 3 to under 25, day 25 and above (L743).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum TimeOfDay { Night, #[default] Dusk, Day }
impl TimeOfDay {
    pub fn from_sun_elevation_deg(e: f64) -> Self { if e < 3.0 { Self::Night } else if e < 25.0 { Self::Dusk } else { Self::Day } }
}

/// Principal axis in a vessel's local cell frame (see [`crate::cell`] for the convention).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum Axis { X, Y, Z }
