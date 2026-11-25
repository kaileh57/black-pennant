//! content: pure data formats and the part catalogue. Leaf crate: depends on no other workspace crate.
//! DESIGN.md "Crate layout" (L1060-1074), "Blueprint format" (L2067-2087), "Campaign" (L729-896).
//!
//! Module map (one module = one work unit unless marked contract-owned):
//! - contract-owned: [`ids`], [`enums`], [`weather`], [`error`], [`part`], [`graph`], [`network`],
//!   [`ruleset`], [`tuning`], [`replay`], [`settings`], [`data`], the types in [`blueprint`], [`campaign`], [`save`]
//! - `content-cell` [`cell`], `content-hull` [`hull`], `content-catalogue` [`catalogue`], `content-kit` [`kit`],
//!   `content-blueprint` [`blueprint::io`], `content-share` [`share`], `content-migrate` [`migrate`],
//!   `content-validate` [`validate`], `content-stats` [`stats`], `content-campaign` [`campaign::load`],
//!   `content-save` [`save`] functions
//!
//! Every player-facing string is a [`ids::TextKey`]; English lives in `game/i18n/*.csv`.
#![allow(dead_code, unused_variables)]

pub mod blueprint;
pub mod campaign;
pub mod catalogue;
pub mod cell;
pub mod data;
pub mod enums;
pub mod error;
pub mod graph;
pub mod hull;
pub mod ids;
pub mod kit;
pub mod migrate;
pub mod network;
pub mod part;
pub mod replay;
pub mod ruleset;
pub mod save;
pub mod settings;
pub mod share;
pub mod stats;
pub mod tuning;
pub mod validate;
pub mod weather;

pub use enums::*;
pub use error::{ContentError, ContentResult};
pub use ids::*;
pub use weather::{SeaState, WeatherKind, WeatherState};
