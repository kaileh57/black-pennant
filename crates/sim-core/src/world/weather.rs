//! Part of unit `sim-core-world`: the sea's change from one weather to the next.
//!
//! # Implementation decisions (binding for callers)
//! - A weather turn does not swap the wave set in one tick. Before this, the surface under a hull jumped by
//!   up to 4.7 m in one tick (stock fast attack craft, sea 7 to sea 6), and a dry, undamaged hull whose deck
//!   the new sea covered was declared sunk on that tick.
//! - The sea moves to the new set over [`WEATHER_BLEND_S`] (60 s, the top of L1871's 20 to 60 s weather blend),
//!   counted in ticks. The component count stays fixed (L1116): component `i` of the old set fades out and
//!   component `i` of the new set fades in through zero amplitude, one window per component, staggered so at
//!   most two components are fading at any moment. Wavenumber, frequency, direction and phase are never
//!   interpolated (that would sweep the phase `k d.x - omega t` across the map), so the surface height changes
//!   continuously in time everywhere.
//! - Component `i` (0-based, of `n` = the larger of the two counts) fades out over
//!   `[i h, (i + 1) h]` and in over `[(i + 1) h, (i + 2) h]` seconds, with `h = WEATHER_BLEND_S / (n + 1)`.
//!   Both ramps are linear in amplitude. If the mixed set's `sum(Q k A)` passes the tuning's ceiling, every Q
//!   is scaled down to it, so the height inversion stays well posed; the scale is continuous in time.
//! - A turn during a blend starts a new blend from the set in force at that tick.
//! - Renderers call [`blended_waves`] with the same inputs so the drawn sea stays the floated one.
use crate::waves::{GerstnerWave, WaveSet, MAX_WAVES};
use content::tuning::WaveTuning;
use serde::{Deserialize, Serialize};

/// Seconds the sea takes to change from one weather's wave set to the next (L1871: 20 to 60 s).
pub const WEATHER_BLEND_S: f64 = 60.0;
/// [`WEATHER_BLEND_S`] in ticks.
pub const WEATHER_BLEND_TICKS: u64 = (WEATHER_BLEND_S as u64) * crate::time::TICK_HZ as u64;

/// A weather turn in progress: the set in force when it began, the new weather's set and the first tick
/// that blends.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct WaveBlend { pub from: WaveSet, pub to: WaveSet, pub start_tick: u64 }

impl WaveBlend {
    /// The set in force at `tick`, and whether the blend has finished.
    pub fn at(&self, tick: u64, tuning: &WaveTuning) -> (WaveSet, bool) {
        let n = tick.saturating_sub(self.start_tick);
        if n >= WEATHER_BLEND_TICKS { return (self.to, true); }
        (blended_waves(&self.from, &self.to, n as f64 * crate::time::DT, tuning), false)
    }
}

/// The wave set `elapsed_s` seconds into a turn from `from` to `to` (see the module docs). `elapsed_s` at or
/// below 0 gives `from`; at or past [`WEATHER_BLEND_S`] it gives `to`. Unit: `sim-core-world`.
pub fn blended_waves(from: &WaveSet, to: &WaveSet, elapsed_s: f64, tuning: &WaveTuning) -> WaveSet {
    if !elapsed_s.is_finite() || elapsed_s >= WEATHER_BLEND_S { return *to; }
    if elapsed_s <= 0.0 { return *from; }
    let n = (from.count.max(to.count) as usize).min(MAX_WAVES);
    let h = WEATHER_BLEND_S / (n as f64 + 1.0);
    let pick = |s: &WaveSet, i: usize| if i < s.count as usize { s.waves[i] } else { GerstnerWave::default() };
    let mut out = WaveSet { seed: to.seed, count: n as u8, waves: [GerstnerWave::default(); MAX_WAVES] };
    for i in 0..n {
        let u = elapsed_s / h - i as f64;
        out.waves[i] = if u <= 0.0 {
            pick(from, i)
        } else if u < 1.0 {
            let mut w = pick(from, i);
            w.amplitude *= 1.0 - u;
            w
        } else if u < 2.0 {
            let mut w = pick(to, i);
            w.amplitude *= u - 1.0;
            w
        } else {
            pick(to, i)
        };
    }
    let ceiling = if tuning.max_steepness_sum.is_finite() && tuning.max_steepness_sum > 0.0 { tuning.max_steepness_sum.min(1.0) } else { 0.85 };
    let mut sum = 0.0;
    for w in &out.waves[..n] { sum += w.q * w.k * w.amplitude; }
    if sum > ceiling && sum.is_finite() {
        let f = ceiling / sum * (1.0 - 1e-9);
        for w in &mut out.waves[..n] { w.q *= f; }
    }
    out
}
