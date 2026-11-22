//! Contract-owned (no work unit). Tick and rate constants. DESIGN.md "Rates" (L1413-1439), L1114, L688, L843.
//! These are fixed constants, the same on every platform, changed only by commit.
use serde::{Deserialize, Serialize};

/// Simulation tick rate: physics, buoyancy, projectiles, damage (L1415).
pub const TICK_HZ: u32 = 60;
/// Tick length in seconds.
pub const DT: f64 = 1.0 / 60.0;
/// Tick length in nanoseconds for the gdclient clock thread (L1361).
pub const TICK_NS: u64 = 16_666_667;
/// Control graph and officers; missile and gun guidance (L1416, L1419).
pub const GRAPH_HZ: u32 = 30;
pub const GUIDANCE_HZ: u32 = 30;
/// Tracks and engagement (L1418).
pub const TRACK_HZ: u32 = 10;
/// Default sensor detection rate; per sensor 2..=10 Hz, staggered (L1417).
pub const DEFAULT_SENSOR_HZ: u32 = 5;
/// Snapshots per client, guided missile state and client inputs (L1420-1422).
pub const SNAPSHOT_HZ: u32 = 30;
pub const MISSILE_STATE_HZ: u32 = 5;
pub const INPUT_HZ: u32 = 30;
/// Hulls farther than this from every player-side hull tick at 30 Hz or go kinematic (L1415, L1549).
pub const DISTANT_M: f64 = 10_000.0;
pub const DISTANT_HZ: u32 = 30;
/// Cells visited per tick by splitting and compartment recompute, shared across vessels in ascending vessel id;
/// leftover work carries to the next tick in the same order (L688).
pub const SPLIT_WORK_BUDGET: u32 = 50_000;
/// Operation checkpoint interval in ticks (60 s, L843).
pub const CHECKPOINT_TICKS: u64 = 3_600;
/// Splash rewind depth in ticks (30 s, L216).
pub const REWIND_TICKS: u64 = 1_800;
/// Design-time caps of the reference battle (L1445-1447).
pub const MAX_FULL_PHYSICS_HULLS: usize = 44;
pub const MAX_LIVE_PROJECTILES: usize = 2_000;
pub const KN: f64 = 0.514_444;

/// A simulation tick count since sortie start.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Tick(pub u64);
impl Tick {
    pub fn seconds(self) -> f64 { self.0 as f64 * DT }
    pub fn next(self) -> Tick { Tick(self.0 + 1) }
    /// True on ticks where a system running at `hz` (a divisor of 60) is due, offset by `phase` ticks.
    pub fn is_due(self, hz: u32, phase: u64) -> bool { let p = (TICK_HZ / hz.max(1)) as u64; (self.0 + phase) % p.max(1) == 0 }
}
