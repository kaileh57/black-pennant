//! Contract-owned (no work unit). The read-only per-tick context handed to every system function, and the
//! deterministic work counters. DESIGN.md L1150-1158 (determinism), L416 (tick split), L441 (RNG keys).
//!
//! Determinism rules for every module in this crate (same as sim-core):
//! - transcendentals through `libm` only; never std `sin`, `cos`, `tan`, `exp`, `ln`, `powf`, `powi`, `atan2`;
//! - never iterate std `HashMap` / `HashSet`; use `BTreeMap`, `IndexMap` or ascending-id vectors;
//! - parallel work fans out per vessel and merges serially in ascending entity slot; no parallel float sums;
//! - budgets are work units (pair checks, node evaluations, A* expansions, interrupts), never elapsed time;
//! - randomness only through `sim_core::rng` keyed on (seed, tick, ids). sim-systems adds no `Stream` values:
//!   it uses `SensorNoise` (sensor, target), `DecoySeduction` (missile, decoy), `AimError` (vessel, mount),
//!   `AiDecision` (entity, decision code), `Generator` and `Theatre` (ids as documented at each call site),
//!   separating draws with `a`, `b` and `draw`;
//! - no function here reads a clock. Wall-clock measurements (tick time for the overload ladder, the script
//!   backstop, Quartermaster limits) are taken by the driver and passed in as plain numbers.
use content::campaign::DifficultySliders;
use content::catalogue::Catalogue;
use content::enums::TimeOfDay;
use content::tuning::TuningTable;
use content::weather::WeatherState;
use serde::{Deserialize, Serialize};
use sim_core::rng::{Key, Stream};
use sim_core::Tick;

#[derive(Clone, Copy)]
pub struct SimCtx<'a> {
    pub seed: u64,
    pub tick: Tick,
    pub catalogue: &'a Catalogue,
    pub tuning: &'a TuningTable,
    pub weather: &'a WeatherState,
    pub time_of_day: TimeOfDay,
    /// Region sonar layer depth in metres (L455).
    pub layer_depth_m: f64,
    pub difficulty: &'a DifficultySliders,
}
impl SimCtx<'_> {
    /// RNG key for this tick.
    pub fn key(&self, stream: Stream, a: u64, b: u64) -> Key { Key::new(self.seed, self.tick.0, stream, a, b) }
    pub fn time_s(&self) -> f64 { self.tick.seconds() }
}

/// Deterministic work counts of the last tick for sim-systems' rows of the tick split (L416): sensors 1.0 ms,
/// control 1.0 ms, enemy AI and steering 0.5 ms, replication 0.5 ms. Feeds tracing spans, the nightly
/// work-unit regression check (fails on a rise over 10%, L418) and Complexity points (L973).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SystemsWork {
    pub sensor_pairs: u32,
    pub detections: u32,
    pub track_updates: u32,
    pub engagement_pairs: u32,
    pub graph_node_evals: u32,
    pub luau_interrupts: u32,
    pub gun_iterations: u32,
    pub guidance_steps: u32,
    pub ai_decisions: u32,
    pub astar_expansions: u32,
    pub snapshot_items: u32,
}

/// Per-tick budgets in work units. Same on every platform; changed only by commit or a gate fallback.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Budgets {
    /// A* node expansions per 60 Hz tick across all hulls, first in first out (L768; fallback 4,000).
    pub astar_expansions: u32,
    /// Luau interrupts per vehicle per graph tick, shared by its script nodes (L366).
    pub luau_interrupts: u32,
    /// Graph node evaluations per vessel per graph tick; a graph over budget stops and its remaining sinks fall
    /// back to officer stages for that tick (L324). PLACEHOLDER value (the spec sets no number): refit once
    /// Complexity coefficients are fitted.
    pub graph_node_evals: u32,
    /// Sensor pair checks per tick before the scheduler defers sensors one tick. PLACEHOLDER derived from the
    /// sizing case at L414 (450,000 pair checks per second = 7,500 per tick before culling).
    pub sensor_pairs: u32,
}
impl Default for Budgets { fn default() -> Self { Self { astar_expansions: 2_000, luau_interrupts: 5_000, graph_node_evals: 4_096, sensor_pairs: 8_192 } } }
