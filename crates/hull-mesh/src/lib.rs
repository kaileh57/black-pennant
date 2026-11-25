//! hull-mesh: Godot-free conversion of cells and parameters into render data. DESIGN.md "Crate layout"
//! (L1069: cell-to-mesh conversion, greedy plus shaped cells, LODs, procedural part models, Godot surface bytes),
//! "Ship meshing and materials" (L1919-1945), "VFX" (L1947-1973), "Ocean and sky" (L1885-1917), "Art and audio"
//! (L1226-1238). Depends on content only; input is the plain `content::cell::CellGridView`, so the same code
//! meshes editor designs and damaged live vessels.
//!
//! Output is plain Vecs and bytes the gdclient adapter hands to `RenderingServer` unchanged
//! ([`types::MeshData`], [`types::SurfaceBytes`], [`types::VolumeTexture`], [`vfx::VfxInstance`]).
//!
//! Module map (one module = one work unit unless marked contract-owned):
//! - contract-owned: [`types`], [`gen_files`], [`parts`] (mod.rs registry), [`vfx`] (mod.rs), [`env`] (mod.rs),
//!   this file
//! - `hm-chunk` [`chunk`], `hm-greedy` [`greedy`], `hm-shaped` [`shaped`], `hm-surface` [`surface`],
//!   `hm-pipeline` [`pipeline`], `hm-lod` [`lod`], `hm-mask` [`mask`], `hm-prim` [`prim`],
//!   `hm-parts-weapons` `parts/weapons.rs`, `hm-parts-sensors` `parts/sensors.rs`,
//!   `hm-parts-fittings` `parts/fittings.rs`, `hm-parts-bodies` `parts/bodies.rs`,
//!   `hm-vfx-meshes` `vfx/meshes.rs`, `hm-vfx-runtime` `vfx/runtime.rs`, `hm-ocean` `env/ocean.rs`,
//!   `hm-scenery` `env/scenery.rs`
//!
//! Determinism: libm for trig, no std HashMap iteration, seeded hashes for every random choice, so
//! `cargo xtask gen` output and shared chunk meshes are byte-stable.
#![allow(dead_code, unused_variables, unused_imports)]

pub mod chunk;
pub mod env;
pub mod gen_files;
pub mod greedy;
pub mod lod;
pub mod mask;
pub mod parts;
pub mod pipeline;
pub mod prim;
pub mod shaped;
pub mod surface;
pub mod types;
pub mod vfx;

pub use types::*;

const _: () = { fn assert_send<T: Send>() {} fn check() { assert_send::<pipeline::ChunkMesher>(); } };
