//! The five default officers (DESIGN.md L301-311). Each officer is a set of graph stages built from the same
//! nodes players use; this directory holds the stage bodies that are not part of the sensor pipeline, plus
//! the shared officer services. This file is contract-owned and holds nothing else.
//!
//! | Officer | Stages | Where |
//! | Helm | `HelmAutopilot`, `StationKeeping`, `Evasion` | `helm.rs` (unit `sys-officer-helm`) |
//! | Engineer | `LoadShedder` | `engineer.rs` (unit `sys-officer-engineer`) |
//! | Sensors | `TrackFusion`, `EmissionController` | `crate::tracks`, `crate::emcon` |
//! | Gunnery | `ThreatRanking`, `MountAssignment`, `BallisticSolver`, `PnGuidance` | `crate::engage`, `crate::guidance` |
//! | Damage Control | `DamageControlPlanner` | `damage_control.rs` (unit `sys-officer-dc`) |
//!
//! `stages.rs` (unit `sys-officer-stages`) marshals graph ports to those typed functions; `tuning.rs` (unit
//! `sys-officer-tuning`) is the L0 preset table, the L1 slider registry and override bookkeeping; `board.rs`
//! (unit `sys-officer-board`) writes the Bridge Board, officer messages and "Why?" chains.
//! Deliberate limits are documented, not bugs (L313): Helm has no terrain-aware pathing beyond the theatre
//! route; Engineer uses a fixed priority order with no prediction; Sensors has no multi-hypothesis tracking;
//! Gunnery takes one target per mount with fixed salvo rules; Damage Control repairs in a fixed priority list.
pub mod board;
pub mod damage_control;
pub mod engineer;
pub mod helm;
pub mod stages;
pub mod tuning;
