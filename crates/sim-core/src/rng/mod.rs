//! Unit `sim-core-rng`. Counter-based hash RNG keyed on (seed, tick, ids). DESIGN.md L441, L660, L524, L1152.
//!
//! There is no stateful generator anywhere in the simulation: every random draw is a pure function of its key,
//! so results do not depend on iteration order or thread scheduling. Same key gives the same bits on every
//! platform (integer arithmetic only; float conversion uses exact operations).
//!
//! # Frozen hash
//! [`bits`] absorbs the key words in the fixed order `seed`, `tick`, `stream | draw << 16`, `a`, `b`. Each word
//! goes through its own round `h = mix64((h + GOLDEN) ^ word)`, where `mix64` is the SplitMix64 finaliser
//! (Stafford variant 13). The starting state is `mix64(seed ^ SEED_SALT)`, and one closing `mix64` runs after the
//! last word. Because every word is mixed before the next is absorbed, swapping two fields changes the
//! output. The golden-value test in `tests.rs` pins this choice; changing it changes every golden hash.
#[cfg(test)]
mod tests;

use serde::{Deserialize, Serialize};

/// Which kind of draw; part of the key so two systems never share a stream.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[repr(u16)]
pub enum Stream {
    /// Keyed (sensor id, target id), L441.
    SensorNoise = 1,
    /// Keyed (vessel id, compartment id), L660.
    FireIgnition = 2,
    FireSpread = 3,
    /// Keyed (vessel id, mount id), one per round.
    GunDispersion = 4,
    /// Keyed (missile id, decoy id), L524.
    DecoySeduction = 5,
    WaveSet = 6,
    Weather = 7,
    AiDecision = 8,
    AimError = 9,
    Generator = 10,
    Vfx = 11,
    Theatre = 12,
    Misc = 13,
}

/// Full key of one draw. `draw` distinguishes several draws with the same ids in one tick.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Key { pub seed: u64, pub tick: u64, pub stream: Stream, pub a: u64, pub b: u64, pub draw: u32 }
impl Key {
    pub fn new(seed: u64, tick: u64, stream: Stream, a: u64, b: u64) -> Self { Self { seed, tick, stream, a, b, draw: 0 } }
    pub fn draw(self, n: u32) -> Self { Self { draw: n, ..self } }
}

/// Golden-ratio increment of SplitMix64.
const GOLDEN: u64 = 0x9E37_79B9_7F4A_7C15;
/// Salt for the starting state, so a zero seed does not start from a zero state.
const SEED_SALT: u64 = 0xD1B5_4A32_D192_ED03;
/// 2^-53, exact in f64.
const INV_2_53: f64 = 1.0 / 9_007_199_254_740_992.0;
/// Rejection rounds `below` tries before accepting a (negligibly) biased value; the chance of reaching it is
/// below 2^-64.
const BELOW_MAX_ROUNDS: u32 = 64;

/// SplitMix64 finaliser (Stafford variant 13). A bijection on u64 with full avalanche.
#[inline]
pub(crate) fn mix64(mut z: u64) -> u64 {
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// One absorption round of the key hash.
#[inline]
pub(crate) fn absorb(h: u64, word: u64) -> u64 { mix64(h.wrapping_add(GOLDEN) ^ word) }

/// 64 well-mixed bits for a key. Must be a fixed, documented integer hash (for example a SplitMix64 or
/// wyhash-style finaliser over the key words); the choice is frozen once golden hashes exist.
/// Unit: `sim-core-rng`.
pub fn bits(key: Key) -> u64 {
    let mut h = mix64(key.seed ^ SEED_SALT);
    h = absorb(h, key.tick);
    h = absorb(h, (key.stream as u16 as u64) | ((key.draw as u64) << 16));
    h = absorb(h, key.a);
    h = absorb(h, key.b);
    mix64(h)
}
/// Uniform in [0, 1) with 53 bits. Unit: `sim-core-rng`.
pub fn uniform(key: Key) -> f64 { (bits(key) >> 11) as f64 * INV_2_53 }
/// Uniform integer in [0, n). Unbiased (rejection on further `draw` values). Unit: `sim-core-rng`.
///
/// Uses Lemire's multiply-and-reject on the high 32 bits of each draw; round `k` reads draw `key.draw + k`.
/// `n == 0` returns 0.
pub fn below(key: Key, n: u32) -> u32 {
    if n <= 1 { return 0; }
    // (2^32 - n) mod n: products whose low word falls under it are the biased remainder.
    let threshold = n.wrapping_neg() % n;
    let mut m = 0u64;
    for k in 0..BELOW_MAX_ROUNDS {
        let x = (bits(key.draw(key.draw.wrapping_add(k))) >> 32) as u32;
        m = x as u64 * n as u64;
        if (m as u32) >= threshold { return (m >> 32) as u32; }
    }
    (m >> 32) as u32
}
/// True with probability `p`. Unit: `sim-core-rng`.
///
/// `p <= 0` and NaN are never true; `p >= 1` is always true (no draw is wasted either way).
pub fn chance(key: Key, p: f64) -> bool {
    if p.is_nan() || p <= 0.0 { return false; }
    if p >= 1.0 { return true; }
    uniform(key) < p
}
/// Standard normal deviate (Box-Muller with libm `log`, `sqrt`, `cos`; uses draws `draw` and `draw + 1`).
/// Unit: `sim-core-rng`.
pub fn normal(key: Key) -> f64 {
    // u1 in (0, 1] so log never sees zero; u2 in [0, 1).
    let u1 = ((bits(key) >> 11) + 1) as f64 * INV_2_53;
    let u2 = uniform(key.draw(key.draw.wrapping_add(1)));
    libm::sqrt(-2.0 * libm::log(u1)) * libm::cos(core::f64::consts::TAU * u2)
}
/// Stable 64-bit hash of an id pair, for sensor staggering `(tick + hash(sensor id)) mod p` (L412).
/// Unit: `sim-core-rng`.
///
/// The top bit is always clear, so `tick + stagger(id)` cannot overflow for any tick below 2^63.
pub fn stagger(id: u64) -> u64 { mix64(absorb(mix64(id ^ SEED_SALT), 0x5354_4147_4745_5221)) >> 1 }
