//! Contract-owned module root for Rust-made environment meshes. Units: `hm-ocean` (ocean.rs), `hm-scenery`
//! (scenery.rs). These are Godot-free mesh generators the client uploads once per tier or per sortie.
//! DESIGN.md L1895-1901 (ocean clipmap), L1915 (clouds), L1917 (islands), L1831 (floating origin).
pub mod ocean;
pub mod scenery;
