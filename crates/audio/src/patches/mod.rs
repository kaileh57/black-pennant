//! Patch families, one unit per file. Each `render*` returns interleaved f32 samples at 48 kHz with
//! `spec.channels` channels and about `spec.duration_s` seconds; `render::finish` does levels, fades, loop seams
//! and quantisation, so patches only shape sound. Use `fundsp::prelude32::*` and `crate::render::run_unit`.
//!
//! Shared character (all families): physical and restrained, matching the restrained post stack; no music cues
//! in sorties; loud events have a short transient plus a low tail that sits under the sea bed; nothing shrill
//! above 8 kHz except the radio static and alert attack.
pub mod ambience;
pub mod impacts;
pub mod signals;
pub mod theme;
pub mod weapons;
