//! Stage 5 of the pipeline at 30 Hz. `gun.rs` is unit `sys-gunnery` (hard), `missile.rs` is unit
//! `sys-guidance` (hard). This file is contract-owned and holds nothing else.
//! DESIGN.md L516-527.
pub mod gun;
pub mod missile;
