//! Shared synthesis toolkit for the patch families (unit `audio-render`).
//!
//! The split: fundsp generates noise and runs the state-variable filters (fixed and time-varying, through
//! [`lp`], [`hp`], [`bp`]); plain Rust applies sample-accurate amplitude envelopes, sums layers in a fixed
//! order, runs the modal resonator bank, phase-accumulating oscillators, grain scheduling and echo taps.
//! fundsp's `envelope` control nodes sample every 2 ms (jittered), which would smear a gun crack, so every
//! amplitude envelope is applied here per sample. All maths outside fundsp uses libm; all randomness is
//! [`crate::render::hash_unit`] keyed on derived seeds, so renders are byte-identical run to run.
//!
//! Conventions: buffers are mono `Vec<f32>` at 48 kHz; times are seconds from the buffer start; a layer is
//! usually built at unit peak ([`norm`]) and mixed with [`add`] at a relative gain, so gains in the patches
//! read as relative peak levels. `render::finish` sets the final level.
use super::{hash_unit, process_mono, run_unit, seconds_to_frames};
use fundsp::prelude32::{bandpass, bandpass_hz, brown, dc, envelope, highpass, highpass_hz, lowpass, lowpass_hz, noise, pass, pink};

/// Output rate as f32.
pub(crate) const SR: f32 = crate::SAMPLE_RATE as f32;
const TAU: f32 = core::f32::consts::TAU;

/// Frames in `sec` seconds.
pub(crate) fn frames(sec: f32) -> usize {
    seconds_to_frames(sec)
}

/// A seed derived from `seed` for layer `k` (SplitMix64 of the pair).
pub(crate) fn sub(seed: u64, k: u64) -> u64 {
    let mut z = seed.wrapping_add(k.wrapping_add(1).wrapping_mul(0x9E37_79B9_7F4A_7C15));
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// Uniform in [lo, hi) from the counter hash.
pub(crate) fn rr(seed: u64, i: u64, lo: f32, hi: f32) -> f32 {
    lo + (hi - lo) * hash_unit(seed, i)
}

// ---------------------------------------------------------------------------------------------------------
// Envelopes (pure functions of time).

/// Smoothstep of x clamped to [0, 1].
pub(crate) fn smooth(x: f32) -> f32 {
    let x = x.clamp(0.0, 1.0);
    x * x * (3.0 - 2.0 * x)
}

/// Attack then exponential decay: a smooth rise over `attack` seconds, then `exp(-(t - attack) / tau)`.
/// Zero before t = 0.
pub(crate) fn ad(t: f32, attack: f32, tau: f32) -> f32 {
    if t < 0.0 {
        0.0
    } else if t < attack {
        smooth(t / attack.max(1e-6))
    } else {
        libm::expf(-(t - attack) / tau.max(1e-6))
    }
}

/// Exponential approach from `from` to `to` with time constant `tau` (pitch drops, filter sweeps).
pub(crate) fn glide(t: f32, from: f32, to: f32, tau: f32) -> f32 {
    to + (from - to) * libm::expf(-t.max(0.0) / tau.max(1e-6))
}

/// A gate open from `t0` to `t1` with smooth edges of `edge` seconds.
pub(crate) fn gate(t: f32, t0: f32, t1: f32, edge: f32) -> f32 {
    let e = edge.max(1e-5);
    smooth((t - t0) / e) * smooth((t1 - t) / e)
}

/// A smooth bump: rises over `rise` to 1 at `peak`, then decays with `tau`.
pub(crate) fn swell(t: f32, start: f32, rise: f32, tau: f32) -> f32 {
    ad(t - start, rise, tau)
}

/// Linear interpolation.
pub(crate) fn lerp(a: f32, b: f32, x: f32) -> f32 {
    a + (b - a) * x
}

/// Round `hz` to a whole number of cycles in `period_s` (at least one), so tonal loop content seams.
pub(crate) fn snap_hz(hz: f32, period_s: f32) -> f32 {
    if !(period_s > 0.0) {
        return hz;
    }
    let k = libm::roundf(hz * period_s).max(1.0);
    k / period_s
}

// ---------------------------------------------------------------------------------------------------------
// Buffer helpers.

/// `sec` seconds of silence.
pub(crate) fn silence(sec: f32) -> Vec<f32> {
    vec![0.0; frames(sec)]
}

/// Mix `src * gain` into `dst` starting at `at` seconds (clipped to `dst`).
pub(crate) fn add(dst: &mut [f32], src: &[f32], at: f32, gain: f32) {
    let off = frames(at.max(0.0));
    if off >= dst.len() {
        return;
    }
    for (d, s) in dst[off..].iter_mut().zip(src.iter()) {
        *d += s * gain;
    }
}

/// Scale to unit peak (silence stays silence).
pub(crate) fn norm(mut x: Vec<f32>) -> Vec<f32> {
    let p = x.iter().fold(0.0f32, |m, &v| m.max(v.abs()));
    if p > 1e-12 {
        let g = 1.0 / p;
        for v in x.iter_mut() {
            *v *= g;
        }
    }
    x
}

/// Multiply by `env(t)` per sample.
pub(crate) fn shape<F: Fn(f32) -> f32>(mut x: Vec<f32>, env: F) -> Vec<f32> {
    for (i, v) in x.iter_mut().enumerate() {
        *v *= env(i as f32 / SR);
    }
    x
}

/// Multiply two buffers sample by sample (ring or amplitude modulation); the result has `a`'s length.
pub(crate) fn mul(mut a: Vec<f32>, b: &[f32]) -> Vec<f32> {
    for (i, v) in a.iter_mut().enumerate() {
        *v *= b.get(i).copied().unwrap_or(0.0);
    }
    a
}

/// Soft saturation `tanh(drive x) / tanh(drive)` of a unit-peak buffer (adds weight and harmonics).
pub(crate) fn sat(x: Vec<f32>, drive: f32) -> Vec<f32> {
    if !(drive > 0.05) {
        return x;
    }
    let x = norm(x);
    let k = 1.0 / libm::tanhf(drive);
    x.into_iter().map(|v| libm::tanhf(drive * v) * k).collect()
}

/// Asymmetric saturation for blasts: positive peaks (overpressure) pass harder than the rarefaction.
pub(crate) fn sat_asym(x: Vec<f32>, drive: f32) -> Vec<f32> {
    if !(drive > 0.05) {
        return x;
    }
    let x = norm(x);
    let kp = 1.0 / libm::tanhf(drive);
    let kn = 1.0 / libm::tanhf(drive * 1.6);
    x.into_iter().map(|v| if v >= 0.0 { libm::tanhf(drive * v) * kp } else { libm::tanhf(drive * 1.6 * v) * kn * 0.85 }).collect()
}

// ---------------------------------------------------------------------------------------------------------
// fundsp sources and filters.

/// White noise.
pub(crate) fn white(sec: f32, seed: u64) -> Vec<f32> {
    run_unit(&mut noise(), sec as f64, 1, seed)
}

/// Pink noise.
pub(crate) fn pinkn(sec: f32, seed: u64) -> Vec<f32> {
    run_unit(&mut pink(), sec as f64, 1, seed)
}

/// Brown noise with its sub-audio drift removed (high-passed at 25 Hz).
pub(crate) fn brownn(sec: f32, seed: u64) -> Vec<f32> {
    run_unit(&mut (brown() >> highpass_hz(25.0, 0.6)), sec as f64, 1, seed)
}

fn hz(f: f32) -> f32 {
    if f.is_finite() { f.clamp(10.0, 20_000.0) } else { 1_000.0 }
}

/// Time-varying 12 dB/oct low-pass (fundsp SVF), cutoff `f(t)` Hz, resonance `q`.
pub(crate) fn lp<F>(x: &[f32], seed: u64, f: F, q: f32) -> Vec<f32>
where
    F: Fn(f32) -> f32 + Clone + Send + Sync + 'static,
{
    let mut u = (pass() | envelope(move |t: f32| hz(f(t))) | dc(q.max(0.1))) >> lowpass();
    process_mono(x, &mut u, seed)
}

/// Time-varying high-pass (fundsp SVF).
pub(crate) fn hp<F>(x: &[f32], seed: u64, f: F, q: f32) -> Vec<f32>
where
    F: Fn(f32) -> f32 + Clone + Send + Sync + 'static,
{
    let mut u = (pass() | envelope(move |t: f32| hz(f(t))) | dc(q.max(0.1))) >> highpass();
    process_mono(x, &mut u, seed)
}

/// Time-varying band-pass (fundsp SVF), centre `f(t)` Hz.
pub(crate) fn bp<F>(x: &[f32], seed: u64, f: F, q: f32) -> Vec<f32>
where
    F: Fn(f32) -> f32 + Clone + Send + Sync + 'static,
{
    let mut u = (pass() | envelope(move |t: f32| hz(f(t))) | dc(q.max(0.1))) >> bandpass();
    process_mono(x, &mut u, seed)
}

/// Fixed low-pass, 24 dB/oct (two Butterworth-Q SVF stages).
pub(crate) fn lp4(x: &[f32], f: f32) -> Vec<f32> {
    let f = hz(f);
    process_mono(x, &mut (lowpass_hz(f, 0.54) >> lowpass_hz(f, 1.31)), 0)
}

/// Fixed high-pass, 12 dB/oct.
pub(crate) fn hp2(x: &[f32], f: f32) -> Vec<f32> {
    process_mono(x, &mut highpass_hz(hz(f), 0.707), 0)
}

/// Fixed band-pass.
pub(crate) fn bp2(x: &[f32], f: f32, q: f32) -> Vec<f32> {
    process_mono(x, &mut bandpass_hz(hz(f), q.max(0.1)), 0)
}

/// Fixed band-limited region: high-pass at `lo`, low-pass at `hi`.
pub(crate) fn band(x: &[f32], lo: f32, hi: f32) -> Vec<f32> {
    process_mono(x, &mut (highpass_hz(hz(lo), 0.707) >> lowpass_hz(hz(hi), 0.707)), 0)
}

/// Noise band: white noise from `lo` to `hi` Hz, `sec` long.
pub(crate) fn noise_band(sec: f32, seed: u64, lo: f32, hi: f32) -> Vec<f32> {
    band(&white(sec, seed), lo, hi)
}

/// A slow random control signal in [-1, 1] with energy below `rate` Hz.
pub(crate) fn wobble(sec: f32, seed: u64, rate: f32) -> Vec<f32> {
    let r = hz(rate.max(0.5));
    norm(process_mono(&white(sec, seed), &mut (lowpass_hz(r, 0.6) >> lowpass_hz(r, 0.6)), 0))
}

// ---------------------------------------------------------------------------------------------------------
// Rust DSP.

/// One-pole low-pass at `f` Hz, applied `passes` times (6 dB/oct each).
pub(crate) fn onepole(x: &[f32], f: f32, passes: usize) -> Vec<f32> {
    let a = 1.0 - libm::expf(-TAU * hz(f) / SR);
    let mut y = x.to_vec();
    for _ in 0..passes.max(1) {
        let mut s = 0.0f32;
        for v in y.iter_mut() {
            s += a * (*v - s);
            *v = s;
        }
    }
    y
}

/// One mode of a modal resonator bank: frequency Hz, amplitude time constant s, gain.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Mode {
    pub f: f32,
    pub tau: f32,
    pub gain: f32,
}

pub(crate) const fn mode(f: f32, tau: f32, gain: f32) -> Mode {
    Mode { f, tau, gain }
}

/// Modal resonator bank (two-pole resonators in f64). A unit impulse into a mode gives a damped sine of
/// amplitude `gain`, frequency `f` and time constant `tau`, so modes read like a physical object's partials.
pub(crate) fn modal(x: &[f32], modes: &[Mode]) -> Vec<f32> {
    let mut out = vec![0.0f32; x.len()];
    for m in modes {
        if !(m.f > 10.0 && m.f < 0.45 * SR) || !(m.tau > 0.0) || m.gain == 0.0 {
            continue;
        }
        let w = 2.0 * core::f64::consts::PI * m.f as f64 / SR as f64;
        let r = libm::exp(-1.0 / (m.tau as f64 * SR as f64));
        let a1 = 2.0 * r * libm::cos(w);
        let a2 = -r * r;
        let b0 = libm::sin(w) * m.gain as f64;
        let (mut y1, mut y2) = (0.0f64, 0.0f64);
        for (o, &v) in out.iter_mut().zip(x.iter()) {
            let y = b0 * v as f64 + a1 * y1 + a2 * y2;
            y2 = y1;
            y1 = y;
            *o += y as f32;
        }
    }
    out
}

/// Phase-accumulating additive oscillator: frequency `f(t)` Hz, partials `(ratio, amplitude)`. Each partial
/// keeps its own wrapped phase, starts at phase zero (no click), and is muted above 9 kHz.
pub(crate) fn osc<F: Fn(f32) -> f32>(sec: f32, f: F, partials: &[(f32, f32)]) -> Vec<f32> {
    let n = frames(sec);
    let mut out = vec![0.0f32; n];
    let mut ph = vec![0.0f64; partials.len()];
    for (i, o) in out.iter_mut().enumerate() {
        let t = i as f32 / SR;
        let f0 = f(t);
        let f0 = if f0.is_finite() { f0.max(0.0) } else { 0.0 };
        let mut s = 0.0f32;
        for (k, &(ratio, amp)) in partials.iter().enumerate() {
            let fk = f0 * ratio;
            if fk < 9_000.0 {
                s += amp * libm::sinf(TAU * ph[k] as f32);
            }
            ph[k] += fk as f64 / SR as f64;
            if ph[k] >= 1.0 {
                ph[k] -= libm::floor(ph[k]);
            }
        }
        *o = s;
    }
    out
}

/// Harmonic series `1..=count` with amplitudes `1 / k^tilt`.
pub(crate) fn harmonics(count: usize, tilt: f32) -> Vec<(f32, f32)> {
    (1..=count).map(|k| (k as f32, 1.0 / libm::powf(k as f32, tilt))).collect()
}

/// Read `x` through a time-varying fractional delay `d(t)` seconds (linear interpolation); zero before the
/// start. Used for ground-reflection combs and Doppler shifts of noise.
pub(crate) fn vdelay<F: Fn(f32) -> f32>(x: &[f32], d: F) -> Vec<f32> {
    let mut out = vec![0.0f32; x.len()];
    for (i, o) in out.iter_mut().enumerate() {
        let t = i as f32 / SR;
        let pos = i as f32 - d(t).max(0.0) * SR;
        if pos < 0.0 {
            continue;
        }
        let j = pos as usize;
        let fr = pos - j as f32;
        let a = x.get(j).copied().unwrap_or(0.0);
        let b = x.get(j + 1).copied().unwrap_or(0.0);
        *o = a + (b - a) * fr;
    }
    out
}

/// Combustion pulses of a piston engine: `cylinders` firings per cycle at a firing rate of `rate(t)` Hz.
/// Each cylinder has its own fixed amplitude, timing offset and burst colour (seeded per cylinder), so the
/// pattern repeats every engine cycle the way a real engine's does; `jitter` adds per-firing timing and level
/// variation on top (0 for loops, which must repeat exactly). Each firing is a pressure step plus a short
/// noise burst decaying with `pulse_tau`. Unit peak; shape it with formant filters.
pub(crate) fn engine_pulses<F: Fn(f32) -> f32>(sec: f32, seed: u64, rate: F, cylinders: usize, jitter: f32, pulse_tau: f32) -> Vec<f32> {
    let cyl = cylinders.clamp(1, 16);
    let burst_len = frames(pulse_tau * 6.0).max(8);
    let bursts: Vec<Vec<f32>> = (0..cyl)
        .map(|c| {
            let c = c as u64;
            (0..burst_len)
                .map(|k| {
                    let t = k as f32 / SR;
                    let n = 2.0 * hash_unit(sub(seed, 50 + c), k as u64) - 1.0;
                    let step = if k < 3 { 1.0 - k as f32 / 3.0 } else { 0.0 };
                    (0.6 * n + step) * libm::expf(-t / pulse_tau)
                })
                .collect()
        })
        .collect();
    let amp: Vec<f32> = (0..cyl).map(|c| rr(seed, 100 + c as u64, 0.7, 1.0)).collect();
    let off: Vec<f32> = (0..cyl).map(|c| rr(seed, 200 + c as u64, -0.04, 0.04)).collect();
    let mut out = silence(sec);
    let mut t = 0.0f32;
    let mut i = 0u64;
    while t < sec {
        let c = (i % cyl as u64) as usize;
        let r = rate(t);
        let r = if r.is_finite() { r.clamp(1.0, 2_000.0) } else { 50.0 };
        let interval = 1.0 / r;
        let j = jitter * (2.0 * hash_unit(seed, 1_000 + 2 * i) - 1.0);
        let at = t + interval * (off[c] + j * 0.5);
        let a = amp[c] * (1.0 + j);
        add(&mut out, &bursts[c], at.max(0.0), a);
        t += interval;
        i += 1;
    }
    // Low-pass the steps a little so the engine is warm rather than clicky.
    norm(onepole(&out, 6_000.0, 1))
}

/// The N-shaped pressure pulse of a blast or supersonic shock, `len` seconds, edges softened at 9 kHz.
pub(crate) fn n_wave(len: f32) -> Vec<f32> {
    let n = frames(len).max(2);
    let mut x = vec![0.0f32; n + frames(0.004)];
    for (i, v) in x.iter_mut().take(n).enumerate() {
        *v = 1.0 - 2.0 * i as f32 / (n - 1) as f32;
    }
    norm(onepole(&x, 9_000.0, 2))
}

/// Times in [t0, t1) drawn with probability proportional to `density(t)` (0..=1), `count` of them, by
/// deterministic rejection sampling. Sorted ascending.
pub(crate) fn scatter<F: Fn(f32) -> f32>(seed: u64, count: usize, t0: f32, t1: f32, density: F) -> Vec<f32> {
    let mut out = Vec::with_capacity(count);
    if !(t1 > t0) {
        return out;
    }
    let mut i = 0u64;
    let limit = (count as u64).saturating_mul(64).max(64);
    while out.len() < count && i < limit {
        let t = rr(seed, 2 * i, t0, t1);
        if hash_unit(seed, 2 * i + 1) < density(t).clamp(0.0, 1.0) {
            out.push(t);
        }
        i += 1;
    }
    out.sort_by(|a, b| a.partial_cmp(b).unwrap_or(core::cmp::Ordering::Equal));
    out
}

/// A rising bubble chirp (a drop entering water, a bubble at the surface): sine from `f0` rising by
/// `rise` (ratio) over its life, amplitude time constant `tau`. Added into `dst` at `at` seconds.
/// Synthesised with a rotating phasor whose step is refreshed every 8 samples.
pub(crate) fn drop_grain(dst: &mut [f32], at: f32, f0: f32, rise: f32, tau: f32, amp: f32) {
    let start = frames(at.max(0.0));
    if start >= dst.len() || !(tau > 0.0) {
        return;
    }
    let len = frames(tau * 6.0).max(8).min(dst.len() - start);
    let decay = libm::expf(-1.0 / (tau * SR));
    let (mut zr, mut zi) = (1.0f32, 0.0f32);
    let (mut wr, mut wi) = (1.0f32, 0.0f32);
    let mut env = 1.0f32;
    let attack = frames(0.0006).max(1) as f32;
    for k in 0..len {
        if k % 8 == 0 {
            let t = k as f32 / SR;
            let f = f0 * (1.0 + (rise - 1.0) * (1.0 - libm::expf(-t / (tau * 1.5))));
            let w = TAU * f.min(0.45 * SR) / SR;
            wr = libm::cosf(w);
            wi = libm::sinf(w);
            // Renormalise the phasor against rounding drift.
            let m = 1.0 / libm::sqrtf(zr * zr + zi * zi);
            zr *= m;
            zi *= m;
        }
        let a = if (k as f32) < attack { smooth(k as f32 / attack) } else { 1.0 };
        dst[start + k] += amp * a * env * zi;
        let nr = zr * wr - zi * wi;
        zi = zr * wi + zi * wr;
        zr = nr;
        env *= decay;
    }
}

/// A short metallic clink: inharmonic partials of `f0` (plate-like ratios), time constant `tau`, each a
/// damped rotating phasor.
pub(crate) fn clink_grain(dst: &mut [f32], at: f32, f0: f32, tau: f32, amp: f32, seed: u64) {
    const RATIOS: [f32; 4] = [1.0, 1.59, 2.14, 2.65];
    let start = frames(at.max(0.0));
    if start >= dst.len() || !(tau > 0.0) {
        return;
    }
    let len = frames(tau * 6.0).max(8).min(dst.len() - start);
    let attack = frames(0.0004).max(1);
    for (p, &r) in RATIOS.iter().enumerate() {
        let f = f0 * r * rr(seed, p as u64, 0.97, 1.03);
        if !(f < 9_000.0) {
            continue;
        }
        let g = amp / (1.0 + p as f32 * 0.6);
        let tp = tau / (1.0 + p as f32 * 0.35);
        let d = libm::expf(-1.0 / (tp * SR));
        let w = TAU * f / SR;
        let (wr, wi) = (d * libm::cosf(w), d * libm::sinf(w));
        let ph0 = TAU * hash_unit(seed, 10 + p as u64);
        let (mut zr, mut zi) = (libm::cosf(ph0), libm::sinf(ph0));
        for k in 0..len {
            let a = if k < attack { smooth(k as f32 / attack as f32) } else { 1.0 };
            dst[start + k] += g * a * zi;
            let nr = zr * wr - zi * wi;
            zi = zr * wi + zi * wr;
            zr = nr;
        }
    }
}

/// A short broadband tick (crackle, spark, gravel): hashed noise with a fast decay, smoothed by a one-pole
/// low-pass at `bright` Hz.
pub(crate) fn tick_grain(dst: &mut [f32], at: f32, tau: f32, bright: f32, amp: f32, seed: u64) {
    let start = frames(at.max(0.0));
    let len = frames(tau * 6.0).max(4);
    let a = 1.0 - libm::expf(-TAU * hz(bright) / SR);
    let mut s = 0.0f32;
    let mut prev = 0.0f32;
    for k in 0..len {
        let i = start + k;
        if i >= dst.len() {
            break;
        }
        let t = k as f32 / SR;
        let w = 2.0 * hash_unit(seed, k as u64) - 1.0;
        s += a * (w - s);
        // First difference keeps the tick free of low-end thump.
        let v = s - prev;
        prev = s;
        dst[i] += amp * 4.0 * v * libm::expf(-t / tau);
    }
}

/// Stick-slip excitation: an impulse train whose rate follows `rate(t)` Hz with relative timing jitter and
/// amplitude roughness, from `t0` to `t1` seconds. Feed it to [`modal`] for groans, creaks and tearing.
pub(crate) fn stick_slip<F: Fn(f32) -> f32>(sec: f32, seed: u64, t0: f32, t1: f32, rate: F, jitter: f32) -> Vec<f32> {
    let mut x = silence(sec);
    let mut t = t0.max(0.0);
    let mut i = 0u64;
    while t < t1.min(sec) {
        let idx = frames(t);
        if idx < x.len() {
            x[idx] += 0.5 + 0.5 * hash_unit(seed, 2 * i);
        }
        let r = rate(t);
        let r = if r.is_finite() { r.clamp(0.5, 4_000.0) } else { 20.0 };
        t += (1.0 / r) * (1.0 + jitter * (2.0 * hash_unit(seed, 2 * i + 1) - 1.0)).max(0.05);
        i += 1;
    }
    x
}

/// The first `len` seconds of `x`, faded out smoothly over the last 40% so echo taps of it end without a
/// click.
pub(crate) fn head(x: &[f32], len: f32) -> Vec<f32> {
    let n = frames(len).min(x.len());
    let fade_from = n as f32 * 0.6;
    let span = (n as f32 - fade_from).max(1.0);
    x[..n].iter().enumerate().map(|(i, &v)| if (i as f32) < fade_from { v } else { v * smooth(1.0 - (i as f32 - fade_from) / span) }).collect()
}

/// Discrete echo taps of `src` into `dst` from `at`: tap k arrives after `first + spacing * k` seconds
/// (jittered), with gain `decay^k` and a one-pole low-pass that darkens tap by tap (`lp0 * lp_mul^k`). The
/// rolling reflections of a distant report or a big burst over open water.
#[allow(clippy::too_many_arguments)]
pub(crate) fn echoes(dst: &mut [f32], src: &[f32], at: f32, seed: u64, taps: usize, first: f32, spacing: f32, decay: f32, lp0: f32, lp_mul: f32) {
    let mut d = first;
    for k in 0..taps {
        let g = libm::powf(decay, k as f32 + 1.0) * rr(seed, 3 * k as u64, 0.7, 1.0);
        let f = lp0 * libm::powf(lp_mul, k as f32);
        let y = onepole(src, f, 2);
        add(dst, &y, at + d, g);
        d += spacing * rr(seed, 3 * k as u64 + 1, 0.7, 1.4) * (1.0 + 0.15 * k as f32);
    }
}

// ---------------------------------------------------------------------------------------------------------
// Physical building blocks shared by weapons and impacts.

/// Parameters of a blast: shock pulse, crack, pitch-dropping body, sub thump and a darkening roar.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Boom {
    /// N-wave gain and length (s).
    pub nwave: f32,
    pub nwave_s: f32,
    /// Broadband crack: gain, decay time constant (s), band (Hz).
    pub crack: f32,
    pub crack_tau: f32,
    pub crack_lo: f32,
    pub crack_hi: f32,
    /// Body: gain, start and end frequency (Hz), pitch-drop time constant, amplitude time constant.
    pub body: f32,
    pub body_f0: f32,
    pub body_f1: f32,
    pub body_drop: f32,
    pub body_tau: f32,
    /// Sub thump: gain, frequency, time constant.
    pub sub: f32,
    pub sub_f: f32,
    pub sub_tau: f32,
    /// Roar: gain, amplitude time constant, low-pass sweep from `roar_lp0` to `roar_lp1` with `roar_sweep`.
    pub roar: f32,
    pub roar_tau: f32,
    pub roar_lp0: f32,
    pub roar_lp1: f32,
    pub roar_sweep: f32,
    /// Saturation drive applied to the sum.
    pub drive: f32,
}

impl Boom {
    /// A neutral starting point; patches override fields.
    pub(crate) const BASE: Boom = Boom {
        nwave: 1.0,
        nwave_s: 0.0015,
        crack: 0.8,
        crack_tau: 0.008,
        crack_lo: 900.0,
        crack_hi: 8_500.0,
        body: 1.0,
        body_f0: 160.0,
        body_f1: 50.0,
        body_drop: 0.03,
        body_tau: 0.1,
        sub: 0.8,
        sub_f: 50.0,
        sub_tau: 0.18,
        roar: 0.7,
        roar_tau: 0.15,
        roar_lp0: 6_000.0,
        roar_lp1: 350.0,
        roar_sweep: 0.05,
        drive: 2.0,
    };
}

/// Render a blast, unit peak, `sec` long.
pub(crate) fn boom(sec: f32, seed: u64, b: &Boom) -> Vec<f32> {
    let mut out = silence(sec);
    if b.nwave > 0.0 {
        add(&mut out, &n_wave(b.nwave_s), 0.0, b.nwave);
    }
    if b.crack > 0.0 {
        let len = (b.crack_tau * 9.0).min(sec);
        let c = noise_band(len, sub(seed, 1), b.crack_lo, b.crack_hi);
        let (ct,) = (b.crack_tau,);
        add(&mut out, &norm(shape(c, move |t| ad(t, 0.0003, ct))), 0.0, b.crack);
    }
    if b.body > 0.0 {
        let len = (b.body_tau * 9.0).min(sec);
        let (f0, f1, dr, bt) = (b.body_f0, b.body_f1, b.body_drop, b.body_tau);
        let x = osc(len, move |t| glide(t, f0, f1, dr), &[(1.0, 1.0), (2.0, 0.3), (3.0, 0.12)]);
        add(&mut out, &norm(shape(x, move |t| ad(t, 0.0012, bt))), 0.0, b.body);
    }
    if b.sub > 0.0 {
        let len = (b.sub_tau * 9.0).min(sec);
        let (sf, st) = (b.sub_f, b.sub_tau);
        let x = osc(len, move |t| sf * (1.0 + 0.35 * libm::expf(-t / 0.025)), &[(1.0, 1.0)]);
        add(&mut out, &norm(shape(x, move |t| ad(t, 0.004, st))), 0.0, b.sub);
    }
    if b.roar > 0.0 {
        let len = (b.roar_tau * 9.0).min(sec);
        let (l0, l1, sw, rt) = (b.roar_lp0, b.roar_lp1, b.roar_sweep, b.roar_tau);
        let n = pinkn(len, sub(seed, 2));
        let n = lp(&n, sub(seed, 3), move |t| glide(t, l0, l1, sw), 0.8);
        add(&mut out, &norm(shape(n, move |t| ad(t, 0.0025, rt))), 0.0, b.roar);
    }
    sat_asym(out, b.drive)
}

/// A low rumble bed: brown noise low-passed at `cutoff`, rising over `rise` from `start` and decaying with
/// `tau`, with slow irregular swells (`roll` 0..1 depth) like rolling thunder. Unit peak.
pub(crate) fn rumble(sec: f32, seed: u64, cutoff: f32, start: f32, rise: f32, tau: f32, roll: f32) -> Vec<f32> {
    let n = lp4(&brownn(sec, sub(seed, 1)), cutoff);
    let w = wobble(sec, sub(seed, 2), 3.0);
    let roll = roll.clamp(0.0, 1.0);
    let mut x = shape(n, move |t| swell(t, start, rise, tau));
    for (v, m) in x.iter_mut().zip(w.iter()) {
        *v *= 1.0 - roll * 0.5 + roll * 0.5 * m;
    }
    norm(x)
}

/// A metal strike on plating: a short excitation into plate modes based on `f0`, decaying with `tau` (the
/// fundamental's time constant; higher modes die faster). Unit peak.
pub(crate) fn metal_hit(sec: f32, seed: u64, f0: f32, tau: f32, partials: usize, excite_s: f32) -> Vec<f32> {
    const RATIOS: [f32; 10] = [1.0, 1.59, 2.14, 2.30, 2.65, 2.92, 3.50, 4.15, 4.62, 5.40];
    let ex = shape(noise_band(sec, sub(seed, 1), 80.0, 9_000.0), move |t| ad(t, 0.0002, excite_s.max(0.0003)));
    let modes: Vec<Mode> = RATIOS
        .iter()
        .take(partials.clamp(1, RATIOS.len()))
        .enumerate()
        .map(|(k, &r)| {
            let f = f0 * r * rr(seed, 100 + k as u64, 0.985, 1.015);
            mode(f, tau / libm::sqrtf(1.0 + k as f32 * 0.8), 1.0 / (1.0 + 0.35 * k as f32))
        })
        .collect();
    norm(modal(&ex, &modes))
}

/// Debris and sparks: `count` clinks scattered in [t0, t1) with a decaying density, partial frequency drawn
/// from [f_lo, f_hi) and time constant from [tau_lo, tau_hi). Added into `dst` at `gain`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn clatter(dst: &mut [f32], seed: u64, count: usize, t0: f32, t1: f32, f_lo: f32, f_hi: f32, tau_lo: f32, tau_hi: f32, gain: f32) {
    let span = (t1 - t0).max(1e-3);
    let times = scatter(sub(seed, 1), count, t0, t1, |t| libm::expf(-2.0 * (t - t0) / span));
    for (i, &t) in times.iter().enumerate() {
        let i = i as u64;
        let f = libm::expf(lerp(libm::logf(f_lo.max(20.0)), libm::logf(f_hi.max(f_lo + 1.0)), hash_unit(seed, 3 * i)));
        let tau = rr(seed, 3 * i + 1, tau_lo, tau_hi);
        let a = gain * rr(seed, 3 * i + 2, 0.25, 1.0) * (1.0 - 0.6 * (t - t0) / span);
        clink_grain(dst, t, f, tau, a, sub(seed, 1000 + i));
    }
}

/// Water falling back: `count` drop chirps and a patter hiss between `t0` and `t1`, density swelling over the
/// first quarter then thinning. `size` 0..1 lowers the drop pitch and thickens the patter. Added into `dst`.
pub(crate) fn rain_down(dst: &mut [f32], seed: u64, t0: f32, t1: f32, count: usize, size: f32, gain: f32) {
    let sec = dst.len() as f32 / SR;
    let t1 = t1.min(sec);
    if !(t1 > t0) {
        return;
    }
    let span = t1 - t0;
    let peak = t0 + span * 0.22;
    let dens = move |t: f32| if t < peak { smooth((t - t0) / (peak - t0).max(1e-3)) } else { libm::expf(-3.5 * (t - peak) / (t1 - peak).max(1e-3)) };
    let times = scatter(sub(seed, 1), count, t0, t1, dens);
    let s = size.clamp(0.0, 1.0);
    let (f_lo, f_hi) = (lerp(900.0, 380.0, s), lerp(4_200.0, 2_600.0, s));
    for (i, &t) in times.iter().enumerate() {
        let i = i as u64;
        let f = libm::expf(lerp(libm::logf(f_lo), libm::logf(f_hi), hash_unit(seed, 4 * i)));
        let tau = rr(seed, 4 * i + 1, 0.003, 0.009) * (1.0 + s * 0.6);
        let a = gain * rr(seed, 4 * i + 2, 0.15, 1.0) * (0.35 + 0.65 * dens(t));
        drop_grain(dst, t, f, rr(seed, 4 * i + 3, 1.15, 1.6), tau, a);
        if hash_unit(seed, 4 * i + 7) < 0.35 {
            tick_grain(dst, t, 0.0015, 5_000.0, a * 0.5, sub(seed, 5000 + i));
        }
    }
    // Patter hiss under the drops.
    let len = sec - t0;
    let h = noise_band(len, sub(seed, 2), lerp(1_400.0, 700.0, s), lerp(7_000.0, 5_000.0, s));
    let h = norm(shape(h, move |t| { let d = dens(t + t0); d * libm::sqrtf(d) }));
    add(dst, &h, t0, gain * lerp(0.25, 0.45, s));
}

/// A splash of size `s` (0 = autocannon round, 1 = heavy shell or missile), unit peak: the slap and plunk,
/// the cavity thump, the column's rising whoosh and the rain-down of the column.
pub(crate) fn splash(sec: f32, seed: u64, s: f32) -> Vec<f32> {
    let s = s.clamp(0.0, 1.0);
    let mut out = silence(sec);
    // Slap: the first contact.
    let slap_tau = lerp(0.006, 0.02, s);
    let slap = noise_band(slap_tau * 8.0, sub(seed, 1), 500.0, 6_500.0);
    add(&mut out, &norm(shape(slap, move |t| ad(t, 0.0004, slap_tau))), 0.0, 0.7);
    // Plunk: the cavity closing, a quick downward-pitched body.
    let (pf0, pf1, ptau) = (lerp(620.0, 160.0, s), lerp(230.0, 55.0, s), lerp(0.035, 0.12, s));
    let plunk = osc(ptau * 8.0, move |t| glide(t, pf0, pf1, ptau * 0.5), &[(1.0, 1.0), (2.0, 0.2)]);
    add(&mut out, &norm(shape(plunk, move |t| ad(t, 0.002, ptau))), 0.004, lerp(0.55, 0.9, s));
    // Sub thump of a big body plunging.
    if s > 0.3 {
        let st = lerp(0.12, 0.35, s);
        let sb = osc(st * 8.0, |t| 46.0 * (1.0 + 0.3 * libm::expf(-t / 0.04)), &[(1.0, 1.0)]);
        add(&mut out, &norm(shape(sb, move |t| ad(t, 0.006, st))), 0.01, (s - 0.3) * 1.3);
    }
    // Column whoosh: brightens as the column rises, then darkens.
    let (rise, wtau) = (lerp(0.03, 0.25, s), lerp(0.12, 0.6, s));
    let wlen = (rise + wtau * 7.0).min(sec);
    let w = pinkn(wlen, sub(seed, 2));
    let w = lp(&w, sub(seed, 3), move |t| 500.0 + lerp(3_000.0, 2_200.0, s) * swell(t, 0.0, rise, wtau * 0.8), 0.7);
    add(&mut out, &norm(shape(w, move |t| swell(t, 0.0, rise, wtau))), 0.01, lerp(0.45, 0.8, s));
    // Low slosh of the displaced water for medium and large splashes.
    if s > 0.2 {
        let sl = rumble(sec, sub(seed, 4), lerp(500.0, 300.0, s), 0.05, lerp(0.1, 0.35, s), lerp(0.25, 0.8, s), 0.4);
        add(&mut out, &sl, 0.0, lerp(0.15, 0.45, s));
    }
    // Rain-down of the column.
    let t0 = lerp(0.1, 0.8, s).min(sec * 0.3);
    let t1 = sec * 0.92;
    let count = (lerp(60.0, 900.0, s * s) + 120.0 * s) as usize;
    rain_down(&mut out, sub(seed, 5), t0, t1, count, s, lerp(0.35, 0.3, s));
    norm(out)
}

/// A secondary detonation (cook-off chains): the blast and a short rolling tail without debris, unit peak.
pub(crate) fn secondary_blast(sec: f32, seed: u64, s: f32) -> Vec<f32> {
    let s = s.clamp(0.0, 1.0);
    let mut b = Boom::BASE;
    b.nwave_s = lerp(0.0015, 0.004, s);
    b.crack_tau = lerp(0.007, 0.016, s);
    b.body_f0 = lerp(170.0, 110.0, s);
    b.body_f1 = lerp(55.0, 30.0, s);
    b.body_tau = lerp(0.08, 0.25, s);
    b.sub_f = lerp(58.0, 42.0, s);
    b.sub_tau = lerp(0.12, 0.4, s);
    b.roar_tau = lerp(0.1, 0.35, s);
    b.drive = lerp(1.8, 2.8, s);
    let mut out = boom(sec, sub(seed, 1), &b);
    let rb = rumble(sec, sub(seed, 2), 260.0, 0.02, 0.08, (sec / 5.0).max(0.08), 0.6);
    add(&mut out, &rb, 0.0, 0.35);
    norm(out)
}

/// An explosion of size `s` (0 = small HE shell, 1 = heavy warhead), unit peak: blast, fireball, debris with
/// splashes, rolling tail. `air` drops the debris and thins the low end (a proximity burst).
pub(crate) fn explosion(sec: f32, seed: u64, s: f32, air: bool) -> Vec<f32> {
    let s = s.clamp(0.0, 1.0);
    let mut b = Boom::BASE;
    b.nwave_s = lerp(0.002, 0.005, s);
    b.crack = 0.8;
    b.crack_tau = lerp(0.009, 0.022, s);
    b.crack_lo = lerp(900.0, 450.0, s);
    b.body_f0 = lerp(150.0, 95.0, s);
    b.body_f1 = lerp(45.0, 24.0, s);
    b.body_drop = lerp(0.035, 0.08, s);
    b.body_tau = lerp(0.13, 0.42, s);
    b.sub = lerp(0.8, 1.0, s);
    b.sub_f = lerp(52.0, 38.0, s);
    b.sub_tau = lerp(0.22, 0.75, s);
    b.roar = 0.9;
    b.roar_tau = lerp(0.18, 0.6, s);
    b.roar_lp0 = 6_500.0;
    b.roar_lp1 = lerp(380.0, 200.0, s);
    b.roar_sweep = lerp(0.07, 0.18, s);
    b.drive = lerp(2.0, 3.2, s);
    if air {
        b.crack = 1.0;
        b.sub *= 0.45;
        b.body_f0 *= 1.6;
        b.body_f1 *= 1.8;
        b.body_tau *= 0.5;
        b.roar_tau *= 0.6;
    }
    let mut out = silence(sec);
    let blast = boom(sec, sub(seed, 1), &b);
    add(&mut out, &blast, 0.0, 1.0);
    // Fireball: a low deflagration whoosh behind the shock.
    let (fr, ft) = (lerp(0.04, 0.12, s), lerp(0.25, 0.8, s));
    let fb_len = (fr + ft * 7.0).min(sec);
    let fb = lp(&pinkn(fb_len, sub(seed, 2)), sub(seed, 3), move |t| 400.0 + 1_400.0 * swell(t, 0.0, fr, ft * 0.6), 0.7);
    add(&mut out, &norm(shape(fb, move |t| swell(t, 0.005, fr, ft))), 0.0, if air { 0.2 } else { 0.4 });
    // Rolling tail over the water.
    let tail_tau = (sec / 5.5).max(0.1);
    let rb = rumble(sec, sub(seed, 4), lerp(320.0, 200.0, s), 0.04, lerp(0.08, 0.25, s), tail_tau, 0.7);
    add(&mut out, &rb, 0.0, if air { 0.18 } else { lerp(0.4, 0.6, s) });
    let head = head(&blast, 0.25);
    echoes(&mut out, &head, 0.0, sub(seed, 5), (3.0 + 4.0 * s) as usize, lerp(0.09, 0.16, s), lerp(0.12, 0.22, s), 0.5, 1_600.0, 0.7);
    if !air {
        // Debris: steel fragments and fittings landing, then their splashes.
        clatter(&mut out, sub(seed, 6), (14.0 + 40.0 * s) as usize, 0.12, lerp(0.9, 2.4, s).min(sec), 900.0, 4_200.0, 0.015, 0.06, 0.16);
        clatter(&mut out, sub(seed, 7), (3.0 + 7.0 * s) as usize, 0.2, lerp(0.9, 2.2, s).min(sec), 230.0, 650.0, 0.05, 0.14, 0.2);
        rain_down(&mut out, sub(seed, 8), lerp(0.35, 0.7, s), sec * 0.95, (40.0 + 220.0 * s) as usize, 0.3, 0.12);
    } else {
        // Fragments whizz away.
        for k in 0..8u64 {
            let at = rr(seed, 900 + k, 0.02, 0.3);
            let fc = rr(seed, 920 + k, 1_200.0, 3_200.0);
            let wz = bp(&white(0.12, sub(seed, 940 + k)), sub(seed, 960 + k), move |t| fc * (1.0 - 1.6 * t), 3.0);
            add(&mut out, &norm(shape(wz, |t| ad(t, 0.012, 0.025))), at, rr(seed, 980 + k, 0.05, 0.14));
        }
        // The sea surface returns the crack a moment later.
        let refl = onepole(&head, 4_500.0, 2);
        add(&mut out, &refl, 0.028, 0.45);
    }
    norm(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modal_unit_impulse_gives_unit_damped_sine() {
        let mut x = vec![0.0f32; 48_000];
        x[0] = 1.0;
        let y = modal(&x, &[mode(440.0, 0.1, 1.0)]);
        let peak = y[..200].iter().fold(0.0f32, |m, v| m.max(v.abs()));
        assert!((peak - 1.0).abs() < 0.02, "{peak}");
        // After one time constant the envelope is about 1/e.
        let p1 = y[4_800..4_900].iter().fold(0.0f32, |m, v| m.max(v.abs()));
        assert!((p1 - libm::expf(-1.0)).abs() < 0.03, "{p1}");
    }

    #[test]
    fn snap_gives_whole_cycles() {
        let f = snap_hz(97.3, 2.0);
        assert!((f * 2.0 - libm::roundf(f * 2.0)).abs() < 1e-4);
        assert_eq!(snap_hz(0.1, 1.0), 1.0);
    }

    #[test]
    fn scatter_is_deterministic_and_bounded() {
        let a = scatter(5, 100, 0.5, 1.5, |t| 1.5 - t);
        let b = scatter(5, 100, 0.5, 1.5, |t| 1.5 - t);
        assert_eq!(a, b);
        assert_eq!(a.len(), 100);
        assert!(a.iter().all(|&t| (0.5..1.5).contains(&t)));
        // Density weighting: more events early.
        assert!(a.iter().filter(|&&t| t < 1.0).count() > 60);
        assert!(scatter(5, 10, 1.0, 1.0, |_| 1.0).is_empty());
    }

    #[test]
    fn envelopes_behave() {
        assert_eq!(ad(-0.1, 0.01, 0.1), 0.0);
        assert!((ad(0.01, 0.01, 0.1) - 1.0).abs() < 1e-6);
        assert!((ad(0.11, 0.01, 0.1) - libm::expf(-1.0)).abs() < 1e-5);
        assert!((glide(0.0, 100.0, 50.0, 0.1) - 100.0).abs() < 1e-4);
        assert!(gate(0.5, 0.0, 1.0, 0.1) == 1.0 && gate(-0.1, 0.0, 1.0, 0.1) == 0.0);
    }
}
