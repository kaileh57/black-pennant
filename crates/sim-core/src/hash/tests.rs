//! Tests of the state hash: the canonical serde stream, the subsystem split and the libm golden vector.
use super::*;
use crate::world::{World, WorldConfig};
use content::weather::WeatherState;
use std::sync::Arc;

fn of<T: Serialize + ?Sized>(v: &T) -> StateHash {
    let mut h = StateHasher::new();
    h.write_serde(v);
    h.finish()
}

#[test]
fn the_serde_stream_is_delimited_and_canonical() {
    // Lengths are part of the stream, so moving a boundary changes the hash.
    assert_ne!(of(&("ab", "c")), of(&("a", "bc")));
    assert_ne!(of(&(vec![1u8, 2], vec![3u8])), of(&(vec![1u8], vec![2u8, 3])));
    assert_ne!(of(&Some(0u8)), of(&None::<u8>));
    assert_ne!(of(&vec![0u16; 2]), of(&vec![0u8; 4]));
    // Floats go in by bit pattern: the zeros differ, and an f32 hashes as the f64 it widens to.
    assert_ne!(of(&0.0f64), of(&-0.0f64));
    assert_eq!(of(&1.5f32), of(&1.5f64));
    // The serde form and the direct writers agree, so sim-systems can mix them.
    let v = glam::DVec3::new(1.0, -2.5, 1e300);
    let mut direct = StateHasher::new();
    direct.write_vec3(v);
    assert_eq!(of(&v), direct.finish());
    // Enum variants are tagged, and a large value straddles the internal buffer without loss.
    assert_ne!(of(&crate::vessel::Fidelity::Full), of(&crate::vessel::Fidelity::Full30));
    let big: Vec<u64> = (0..50_000).collect();
    let mut by_hand = StateHasher::new();
    by_hand.write_u64(big.len() as u64);
    for x in &big { by_hand.write_u64(*x); }
    assert_eq!(of(&big), by_hand.finish());
}

#[test]
fn subsystem_hashes_name_what_changed() {
    let cat = Arc::new(content::catalogue::catalogue().clone());
    let config = |seed: u64| WorldConfig { seed, weather: WeatherState::default(), map: Default::default(), catalogue: cat.clone() };
    let (a, b) = (World::new(config(7)), World::new(config(7)));
    assert_eq!(subsystem_hashes(&a), subsystem_hashes(&b));
    assert_eq!(state_hash(&a), state_hash(&b));
    // Another seed: only the environment (seed and the wave set drawn from it) differs.
    let (h7, h8) = (subsystem_hashes(&a), subsystem_hashes(&World::new(config(8))));
    assert_ne!(h7.environment, h8.environment);
    assert_eq!((h7.entities, h7.bodies, h7.cells, h7.projectiles, h7.munitions), (h8.entities, h8.bodies, h8.cells, h8.projectiles, h8.munitions));
    assert_ne!(state_hash(&a), state_hash(&World::new(config(8))));
    // A projectile in flight touches the projectile hash alone; a tick moves the environment too.
    let mut c = World::new(config(7));
    c.projectiles.spawn(0, glam::DVec3::new(0.0, 50.0, 0.0), glam::DVec3::X * 800.0, 76, content::part::weapons::CalibreBand::Medium, crate::entity::EntityId::NONE, content::ids::PartSlot(0), false, 2_000);
    let hc = subsystem_hashes(&c);
    assert_ne!(hc.projectiles, h7.projectiles);
    assert_eq!((hc.entities, hc.munitions, hc.environment), (h7.entities, h7.munitions, h7.environment));
    c.step();
    let hd = subsystem_hashes(&c);
    assert!(hd.projectiles != hc.projectiles && hd.environment != hc.environment && hd.entities == hc.entities);
    assert_eq!(SubsystemHashes::NAMES.len(), 9);
}

#[test]
fn libm_matches_its_golden_vector() {
    let h = libm_golden_hash();
    assert_eq!(h, libm_golden_hash(), "the vector is a pure function");
    assert_eq!(h, LIBM_GOLDEN, "libm results changed: an upgrade or a platform difference breaks replays (L447)");
}
