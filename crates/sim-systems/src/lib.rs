//! sim-systems: the brains on top of sim-core's physical world, and the campaign layer. No Godot dependency.
//! DESIGN.md "Crate layout" (L1060-1074): sensor pipeline, fire control, graph VM and officers, Luau host,
//! enemy AI, theatre ledger. Depends on `content` and `sim-core`; `net` and `gdclient` depend on it.
//!
//! Top-level objects:
//! - [`sim::Sim`]: one sortie. Owns a `sim_core::World` plus every system; exposes `new(config, deps)`,
//!   `apply(commands)`, `set_input`, `set_host_load`, `step()`, `snapshot(viewer)`, `drain_events()`. The same
//!   `Sim` runs in the headless server, in single-player, on a listen host, in Splash, in trials and under bots.
//! - [`theatre::Theatre`]: the campaign layer. Creates sortie configs and consumes sortie results; it does
//!   not live inside `Sim`.
//! - [`protocol`]: every type that crosses to net, the client, replays and saves.
//!
//! Module map. One file or directory is one work unit; the unit id is in each module's `//!` header.
//! Contract-owned files (no unit; change only through the orchestrator): this file, `prelude.rs`, `ctx.rs`,
//! `ids.rs`, `protocol/*`, `graph/mod.rs`, `guidance/mod.rs`, `officers/mod.rs`, `ship/mod.rs`,
//! `editor/mod.rs`, `replay/mod.rs`, `theatre/types.rs`. In unit-owned files the public types and
//! signatures are frozen contract: owners fill `todo!()` bodies and add private helpers or extra files in
//! their own directory.
//!
//! | Area | Modules |
//! | Sensor pipeline (L398-530) | `signature`, `sensors` (+ `radar`, `optical`, `sonar`), `tracks`, `ident`, `emcon`, `doctrine`, `engage`, `guidance::{gun, missile}`, `datalink`, `offboard` |
//! | Control (L277-397) | `graph::{library, generate, compile, vm}`, `officers::{helm, engineer, damage_control, stages, tuning, board}`, `script`, `input`, `ship::{design, controls}` |
//! | Enemy (L756-774) | `ai` (+ `commander`, `group`, `unit`, `path`, `ace`, `air`, `sub`) |
//! | Sortie (L104-116, L778-797, L1500-1549, L1676-1687) | `sim` (+ `setup`, `events`, `interest`, `snapshot`, `keyframe`), `specops`, `objectives`, `alerts`, `lod`, `timeflow`, `modes`, `text` |
//! | Editor harness (L118-276, L801-807) | `editor::{gauges, notes, suggest, trials, fitcheck, quartermaster, splash}`, `tutorial` |
//! | Campaign (L729-896) | `theatre` (+ `logistics`, `alert`, `admiral`, `adaptation`, `progress`, `opgen`, `economy`, `saves`, `abstract_combat`, `bot`) |
//! | Replays (L1043-1050) | `replay::{record, play}` |
//!
//! Later-phase units, to schedule last: `sys-ai-air` (Act 2), `sys-ai-sub` (Act 3), `sys-script-luau`
//! (L4, not yet a file), the escort builder in `sys-modes` (1.0). Everything else is needed for the first
//! playable sortie or for 0.1.0. Determinism rules are in [`ctx`].
#![allow(dead_code, unused_variables, unused_imports)]

pub mod ai;
pub mod alerts;
pub mod ctx;
pub mod datalink;
pub mod doctrine;
pub mod editor;
pub mod emcon;
pub mod engage;
pub mod graph;
pub mod guidance;
pub mod ident;
pub mod ids;
pub mod input;
pub mod lod;
pub mod modes;
pub mod objectives;
pub mod offboard;
pub mod officers;
pub mod prelude;
pub mod protocol;
pub mod replay;
pub mod script;
pub mod sensors;
pub mod ship;
pub mod signature;
pub mod sim;
pub mod specops;
pub mod text;
pub mod theatre;
pub mod timeflow;
pub mod tracks;
pub mod tutorial;

pub use protocol::PROTOCOL_VERSION;
pub use sim::{Sim, SimDeps, SimError};
pub use theatre::Theatre;
