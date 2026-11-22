//! Target 2 of DESIGN.md L2130: raw bytes compiled as Luau source and run in a production vehicle VM,
//! guided by `fuzz/host_api.dict`; every oracle of L2132 is checked by `luau::fuzz::check_bytes`.
#![no_main]
use libfuzzer_sys::fuzz_target;
use sim_systems::script::luau::{fuzz::check_bytes, MAX_SOURCE_BYTES};

fuzz_target!(|data: &[u8]| {
    if data.len() > MAX_SOURCE_BYTES { return; }
    if let Err(e) = check_bytes(data) { panic!("oracle failed: {e}"); }
});
