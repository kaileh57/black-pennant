//! sim-core: one sortie's physical world. No Godot dependency. DESIGN.md "Simulation core" (L1098-1129),
//! "Coordinates" (L1131-1146), "Determinism" (L1150-1168), "Combat and damage model" (L531-728).
//!
//! Determinism rules for every module here:
//! - never call std `sin`, `cos`, `tan`, `exp`, `ln`, `powf`, `powi`, `atan2` or other transcendentals; use
//!   `libm` (sqrt, abs, floor, ceil, round, trunc, mul_add are allowed);
//! - never iterate std `HashMap` / `HashSet`; use `BTreeMap`, `IndexMap` or slot-ordered columns;
//! - never sum floats in parallel; fan out per vessel and merge serially in ascending slot order;
//! - budgets are work units, never elapsed time;
//! - randomness only through [`rng`], keyed on (seed, tick, ids).
//!
//! Module map (one directory = one work unit; `time`, `math`, `bits`, `events` and this file are
//! contract-owned): `sim-core-rng`, `sim-core-entity`, `sim-core-waves`, `sim-core-terrain`, `sim-core-grid`,
//! `sim-core-mass`, `sim-core-compartments`, `sim-core-networks`, `sim-core-parts`, `sim-core-vessel`,
//! `sim-core-physics`, `sim-core-hydro`, `sim-core-projectiles`, `sim-core-munitions`, `sim-core-armour`,
//! `sim-core-damage`, `sim-core-flooding`, `sim-core-fire`, `sim-core-structure`, `sim-core-crew`,
//! `sim-core-world` (`world/mod.rs`), `sim-core-world-query` (`world/query.rs`), `sim-core-keyframe`
//! (`world/keyframe.rs`), `sim-core-hash`.
#![allow(dead_code, unused_variables, unused_imports)]

pub mod armour;
pub mod bits;
pub mod compartments;
pub mod crew;
pub mod damage;
pub mod entity;
pub mod events;
pub mod fire;
pub mod flooding;
pub mod grid;
pub mod hash;
pub mod hydro;
pub mod mass;
pub mod math;
pub mod munitions;
pub mod networks;
pub mod parts;
pub mod physics;
pub mod projectiles;
pub mod rng;
pub mod spans;
pub mod structure;
pub mod terrain;
pub mod time;
pub mod vessel;
pub mod waves;
pub mod world;

pub use entity::{EntityId, EntityKind};
pub use events::PhysEvent;
pub use math::{Kinematics, Pose};
pub use time::{Tick, DT, TICK_HZ};
pub use world::{World, WorldConfig};
