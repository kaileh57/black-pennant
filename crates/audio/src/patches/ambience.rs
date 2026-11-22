//! Unit `audio-ambience`. Engine loops (diesel, gas turbine, electric) and cavitation, hull wash, sea beds by
//! sea state, wind, rain, thunder, and sonar pings. DESIGN.md L1246 (Rust sets pitch and volume from sim state
//! for engines, sea and weather), L1857-1866 (weather), L453 (cavitation), L1875 (lightning pulses; thunder
//! follows by distance).
//! Engine loops are rendered at a reference RPM; `mix::engine_params` sets pitch from shaft power, so loops must
//! stay clean when pitched 0.6x to 1.6x. Sea and weather beds are stereo and seamless.
//!
//! Loop construction (shared with `signals` and `theme` through the `pub(crate)` helpers below): every loop is
//! built as exactly one period of `spec_frames(spec)` frames per channel that is cyclic on its own, and
//! [`emit`] appends the head of that period as the overhang `render::finish` expects. The overhang then equals
//! the head, the measured correlation is 1 and `finish` passes the loop through unchanged, so there is no level
//! pulse at the loop rate. Three tools make a period cyclic:
//! - tonal layers hold whole cycles (`kit::snap_hz`), and stationary periodic processes (engine firing
//!   patterns, filters over them) are rendered past one period and the last period kept ([`last_period`],
//!   [`cyc`]), which is their exact steady state;
//! - event layers (drops, crackle, waves, slaps) are scheduled inside the period, rendered with their tails,
//!   and the tails folded back onto the head ([`fold`]);
//! - noise layers are rendered one crossfade longer than the period and the extra crossfaded into the head
//!   with equal power ([`cyclize`]), which keeps the power of uncorrelated noise constant across the seam.
//!
//! Slow envelopes (gusts, swells) are circular sums of bumps ([`Bumps`]), so they are periodic too.
//!
//! Character: engines are tonal with their noise low-passed at 6 kHz, so a 1.6x pitch keeps them under about
//! 10 kHz and a 0.6x pitch keeps the firing rate audible, and the 2 s loops hold no single distinctive event
//! that would reveal the repeat when slowed. The beds are tamed to a common 12 dB crest factor ([`tame`])
//! because `mix` crossfades neighbouring beds as if equal peaks meant equal loudness.
use crate::render::kit::*;
use crate::render::{render_seconds, spec_frames, LOOP_XFADE_S};
use crate::{AmbienceSfx, Playback, SoundSpec};
use core::f32::consts::{FRAC_PI_2, TAU};

/// Unit: `audio-ambience`.
pub fn render(id: AmbienceSfx, spec: &SoundSpec, seed: u64) -> Vec<f32> {
    use AmbienceSfx::*;
    match spec.playback {
        Playback::Loop => {
            let p = spec_frames(spec);
            let chs: Vec<Vec<f32>> = match id {
                EngineDiesel => vec![diesel(p, seed)],
                EngineGasTurbine => vec![gas_turbine(p, seed)],
                EngineElectric => vec![electric(p, seed)],
                Cavitation => vec![cavitation(p, seed)],
                HullWashSlow => wash(p, seed, false),
                HullWashFast => wash(p, seed, true),
                SeaCalm => sea(p, seed, &CALM),
                SeaModerate => sea(p, seed, &MODERATE),
                SeaRough => sea(p, seed, &ROUGH),
                SeaStorm => sea(p, seed, &STORM),
                Wind => wind(p, seed, false),
                WindGale => wind(p, seed, true),
                Rain => rain(p, seed, false),
                RainHeavy => rain(p, seed, true),
                // A one-shot whose spec row was changed to a loop: one period of it, folded so it still seams.
                ThunderNear | ThunderFar | SonarPing | SonarPingHeard => vec![fold(one_shot(id, ps(p).max(0.05), seed), p)],
            };
            emit(&chs, spec)
        }
        Playback::OneShot => {
            let sec = render_seconds(spec).max(0.05);
            let chs: Vec<Vec<f32>> = match id {
                ThunderNear => thunder(sec, seed, true),
                ThunderFar => thunder(sec, seed, false),
                SonarPing | SonarPingHeard => vec![one_shot(id, sec, seed)],
                // A loop patch asked for as a one-shot: one period of the loop.
                _ => {
                    let mut s = *spec;
                    s.playback = Playback::Loop;
                    let x = render(id, &s, seed);
                    return fit(x, spec);
                }
            };
            interleave(&chs, spec, frames(sec))
        }
    }
}

fn one_shot(id: AmbienceSfx, sec: f32, seed: u64) -> Vec<f32> {
    match id {
        AmbienceSfx::ThunderNear => thunder(sec, seed, true).swap_remove(0),
        AmbienceSfx::ThunderFar => thunder(sec, seed, false).swap_remove(0),
        AmbienceSfx::SonarPingHeard => sonar(sec, seed, true),
        _ => sonar(sec, seed, false),
    }
}

// ---------------------------------------------------------------------------------------------------------
// Loop construction helpers (crate-private; also used by `signals` and `theme`).

/// Seconds in `p` frames.
pub(crate) fn ps(p: usize) -> f32 {
    p as f32 / SR
}

/// Crossfade frames used by [`cyclize`] for a period of `p` frames.
pub(crate) fn xf(p: usize) -> usize {
    frames(LOOP_XFADE_S).min(p / 2)
}

/// Seconds to render a noise layer that [`cyclize`] turns into one period of `p` frames.
pub(crate) fn nsec(p: usize) -> f32 {
    (p + xf(p)) as f32 / SR
}

/// Fold everything past frame `p` back onto the start (modulo `p`), so event tails that run past the end of
/// the period sound at its head. Shorter input is padded with silence.
pub(crate) fn fold(mut x: Vec<f32>, p: usize) -> Vec<f32> {
    if p == 0 {
        return Vec::new();
    }
    if x.len() <= p {
        x.resize(p, 0.0);
        return x;
    }
    for i in p..x.len() {
        let v = x[i];
        x[i % p] += v;
    }
    x.truncate(p);
    x
}

/// Equal-power crossfade of the frames past `p` into the head (for uncorrelated material such as noise),
/// giving one cyclic period of `p` frames.
pub(crate) fn cyclize(mut x: Vec<f32>, p: usize) -> Vec<f32> {
    let n = xf(p).min(x.len().saturating_sub(p));
    if x.len() < p {
        x.resize(p, 0.0);
    }
    for i in 0..n {
        let s = (i as f32 + 0.5) / n as f32 * FRAC_PI_2;
        x[i] = x[i] * libm::sinf(s) + x[p + i] * libm::cosf(s);
    }
    x.truncate(p);
    x
}

/// The last `p` frames of a render of a stationary periodic process (its steady state).
pub(crate) fn last_period(x: Vec<f32>, p: usize) -> Vec<f32> {
    if x.len() <= p {
        let mut x = x;
        x.resize(p, 0.0);
        return x;
    }
    x[x.len() - p..].to_vec()
}

/// Run a time-invariant process `f` over a cyclic period: prepend the period's last `warm_s` seconds (at most
/// one period), run, and drop them, so filter state at the head is the steady state of the loop.
pub(crate) fn cyc<F: FnOnce(&[f32]) -> Vec<f32>>(x: &[f32], warm_s: f32, f: F) -> Vec<f32> {
    let p = x.len();
    if p == 0 {
        return Vec::new();
    }
    let w = frames(warm_s).min(p);
    let mut input = Vec::with_capacity(p + w);
    input.extend_from_slice(&x[p - w..]);
    input.extend_from_slice(x);
    let mut y = f(&input);
    y.resize(p + w, 0.0);
    y[w..w + p].to_vec()
}

/// Equal-power pan gains for `pan` in [-1, 1] (left, right).
pub(crate) fn pan(pan: f32) -> (f32, f32) {
    let a = (pan.clamp(-1.0, 1.0) + 1.0) * core::f32::consts::FRAC_PI_4;
    (libm::cosf(a), libm::sinf(a))
}

/// Soft-limit a group of channels to a crest factor of `crest_db` over their joint RMS (memoryless, so loop
/// periods stay cyclic). Beds that `mix` crossfades share one crest, so equal peaks mean equal loudness.
pub(crate) fn tame(chs: &mut [Vec<f32>], crest_db: f32) {
    let (mut s, mut n) = (0.0f64, 0usize);
    for c in chs.iter() {
        for &v in c {
            s += (v as f64) * (v as f64);
        }
        n += c.len();
    }
    if n == 0 || !(s > 1e-20) {
        return;
    }
    let rms = libm::sqrt(s / n as f64) as f32;
    let t = libm::powf(10.0, crest_db / 20.0) * rms;
    for c in chs.iter_mut() {
        for v in c.iter_mut() {
            *v = t * libm::tanhf(*v / t);
        }
    }
}

/// Interleave cyclic periods (one per channel; a mono period feeds both channels of a stereo spec, a stereo
/// pair is averaged for a mono spec) and tile them to `render_seconds(spec)`, so the overhang repeats the head
/// exactly.
pub(crate) fn emit(chs: &[Vec<f32>], spec: &SoundSpec) -> Vec<f32> {
    let n = frames(render_seconds(spec));
    let ch = if spec.channels == 2 { 2 } else { 1 };
    let mut out = vec![0.0f32; n * ch];
    let p = chs.iter().map(|c| c.len()).min().unwrap_or(0);
    if p == 0 {
        return out;
    }
    for i in 0..n {
        let j = i % p;
        for c in 0..ch {
            out[i * ch + c] = if ch == 1 && chs.len() > 1 { 0.5 * (chs[0][j] + chs[1][j]) } else { chs[c.min(chs.len() - 1)][j] };
        }
    }
    out
}

/// Interleave one-shot channels to `n` frames (pad or trim), with the same channel mapping as [`emit`].
pub(crate) fn interleave(chs: &[Vec<f32>], spec: &SoundSpec, n: usize) -> Vec<f32> {
    let ch = if spec.channels == 2 { 2 } else { 1 };
    let mut out = vec![0.0f32; n * ch];
    if chs.is_empty() {
        return out;
    }
    let get = |c: usize, i: usize| chs[c].get(i).copied().unwrap_or(0.0);
    for i in 0..n {
        for c in 0..ch {
            out[i * ch + c] = if ch == 1 && chs.len() > 1 { 0.5 * (get(0, i) + get(1, i)) } else { get(c.min(chs.len() - 1), i) };
        }
    }
    out
}

/// Trim or pad an interleaved render to the frames and channels of `spec`.
fn fit(mut x: Vec<f32>, spec: &SoundSpec) -> Vec<f32> {
    let ch = if spec.channels == 2 { 2 } else { 1 };
    x.resize(frames(render_seconds(spec)) * ch, 0.0);
    x
}

/// A slow periodic envelope: `count` smooth bumps placed around a period of `period` seconds, each evaluated
/// at its circular distance, so the envelope is periodic. Each bump is zero at its start and at one period.
#[derive(Clone, Debug)]
pub(crate) struct Bumps {
    period: f32,
    t: Vec<f32>,
    a: Vec<f32>,
    rise: Vec<f32>,
    tau: Vec<f32>,
}

impl Bumps {
    /// `count` bumps spread evenly (with seeded jitter) over the period; rise, decay time constant and
    /// amplitude drawn from the given ranges.
    pub(crate) fn new(seed: u64, period: f32, count: usize, rise: (f32, f32), tau: (f32, f32), amp: (f32, f32)) -> Bumps {
        let period = if period.is_finite() && period > 0.01 { period } else { 1.0 };
        let mut b = Bumps { period, t: Vec::new(), a: Vec::new(), rise: Vec::new(), tau: Vec::new() };
        for k in 0..count as u64 {
            b.t.push((k as f32 + rr(seed, 5 * k, 0.1, 0.9)) * period / count.max(1) as f32);
            b.rise.push(rr(seed, 5 * k + 1, rise.0, rise.1).max(1e-3));
            b.tau.push(rr(seed, 5 * k + 2, tau.0, tau.1).max(1e-3));
            b.a.push(rr(seed, 5 * k + 3, amp.0, amp.1));
        }
        b
    }

    /// Bump start times (seconds) and amplitudes.
    pub(crate) fn events(&self) -> impl Iterator<Item = (f32, f32)> + '_ {
        self.t.iter().copied().zip(self.a.iter().copied())
    }

    /// The envelope at `t` seconds.
    pub(crate) fn at(&self, t: f32) -> f32 {
        let mut s = 0.0;
        let w = 0.25 * self.period;
        for k in 0..self.t.len() {
            let d = (t - self.t[k]).rem_euclid(self.period);
            s += self.a[k] * ad(d, self.rise[k], self.tau[k]) * smooth((self.period - d) / w);
        }
        s
    }
}

/// A periodic control signal stored at about 500 Hz and read with linear interpolation (cheap to evaluate per
/// sample and inside filter closures).
#[derive(Clone, Debug)]
pub(crate) struct Ctl {
    rate: f32,
    v: Vec<f32>,
}

impl Ctl {
    /// Sample `f` over one period of `period` seconds.
    pub(crate) fn new<F: Fn(f32) -> f32>(period: f32, f: F) -> Ctl {
        let period = if period.is_finite() && period > 0.01 { period } else { 1.0 };
        let n = (libm::roundf(period * 500.0) as usize).max(4);
        let rate = n as f32 / period;
        Ctl { rate, v: (0..n).map(|i| f(i as f32 / rate)).collect() }
    }

    /// The control value at `t` seconds (periodic).
    pub(crate) fn at(&self, t: f32) -> f32 {
        let n = self.v.len();
        let x = (t * self.rate).rem_euclid(n as f32);
        let i = (x as usize).min(n - 1);
        let fr = x - i as f32;
        self.v[i] + (self.v[(i + 1) % n] - self.v[i]) * fr
    }
}

impl Bumps {
    /// The envelope as a control table.
    pub(crate) fn ctl(&self) -> Ctl {
        Ctl::new(self.period, |t| self.at(t))
    }
}

/// Common end of the stereo beds: keep them under 8 kHz (nothing shrill outside the radio and alerts), then
/// tame them to the shared crest factor. Both steps keep each period cyclic.
fn bed_finish(chs: &mut [Vec<f32>]) {
    for c in chs.iter_mut() {
        *c = cyc(c, 0.05, |x| lp4(x, 7_500.0));
    }
    tame(chs, 12.0);
}

/// `base + depth * bumps(t)` sampled over `p` frames.
fn env_buf(b: &Bumps, p: usize, base: f32, depth: f32) -> Vec<f32> {
    let c = b.ctl();
    (0..p).map(|i| base + depth * c.at(i as f32 / SR)).collect()
}

/// Sum `layers` (buffer, gain) into one buffer of `p` frames.
pub(crate) fn mix(p: usize, layers: &[(&[f32], f32)]) -> Vec<f32> {
    let mut out = vec![0.0f32; p];
    for (x, g) in layers {
        for (o, v) in out.iter_mut().zip(x.iter()) {
            *o += v * g;
        }
    }
    out
}

/// A cyclic noise layer at unit peak: `make(seconds, seed)` renders noise one crossfade longer than the
/// period.
fn noise_layer<F: FnOnce(f32, u64) -> Vec<f32>>(p: usize, seed: u64, make: F) -> Vec<f32> {
    norm(cyclize(make(nsec(p), seed), p))
}

/// Sine of `TAU * f * t`.
pub(crate) fn sn(f: f32, t: f32) -> f32 {
    libm::sinf(TAU * f * t)
}

// ---------------------------------------------------------------------------------------------------------
// Engines and the propulsor.

/// Medium-speed marine diesel at its reference speed: 12 cylinders, four-stroke, 900 rpm, so 90 firings a
/// second in a 7.5 Hz engine-cycle pattern (each cylinder has its own strength and colour). Body thrum, exhaust
/// bark, combustion knock and valve-train clatter are formants of the same firing pulses; a second-order crank
/// imbalance hum, a quiet turbocharger whine and broadband machinery noise sit under them.
fn diesel(p: usize, seed: u64) -> Vec<f32> {
    let per = ps(p);
    let cycle = snap_hz(7.5, per);
    let fire = last_period(engine_pulses(2.0 * per, sub(seed, 1), move |_| cycle * 12.0, 12, 0.0, 0.0022), p);
    let body = norm(cyc(&fire, 0.3, |x| lp4(x, 220.0)));
    let bark = norm(cyc(&fire, 0.3, |x| bp2(x, 520.0, 1.4)));
    let knock = norm(cyc(&fire, 0.3, |x| bp2(x, 1_500.0, 2.2)));
    let clatter = norm(cyc(&fire, 0.3, |x| hp2(x, 2_800.0)));
    let crank = snap_hz(30.0, per);
    let hum = norm(osc(per, move |_| crank, &[(1.0, 1.0), (2.0, 0.3)]));
    let turbo_f = snap_hz(3_100.0, per);
    let am = snap_hz(1.5, per);
    let turbo = norm(osc(per, move |t| turbo_f * (1.0 + 0.002 * sn(am, t)), &[(1.0, 1.0)]));
    let mach = noise_layer(p, sub(seed, 2), |s, k| lp4(&pinkn(s, k), 2_500.0));
    let x = mix(p, &[(&body, 1.0), (&bark, 0.5), (&knock, 0.22), (&clatter, 0.07), (&hum, 0.22), (&turbo, 0.03), (&mach, 0.12)]);
    norm(cyc(&x, 0.05, |x| lp4(x, 6_000.0)))
}

/// Gas turbine at its reference spool speed: the compressor's blade-pass whine (a beating pair with a slight
/// periodic waver), the high-pressure spool's thinner whine, the reduction gear mesh, exhaust roar and hiss,
/// and the low combustion rumble.
fn gas_turbine(p: usize, seed: u64) -> Vec<f32> {
    let per = ps(p);
    let f1 = snap_hz(2_350.0, per);
    let beat = 3.0 / per;
    let wv = snap_hz(0.5, per);
    let whine = norm(osc(per, move |t| f1 * (1.0 + 0.0015 * sn(wv, t)), &[(1.0, 1.0), (2.0, 0.25)]));
    let whine2 = norm(osc(per, move |_| f1 + beat, &[(1.0, 1.0)]));
    let f2 = snap_hz(3_650.0, per);
    let hp_whine = norm(osc(per, move |_| f2, &[(1.0, 1.0)]));
    let fg = snap_hz(410.0, per);
    let shaft = snap_hz(30.0, per);
    let gear = norm(osc(per, move |_| fg, &harmonics(3, 1.2)));
    let gear = shape(gear, move |t| 0.8 + 0.2 * sn(shaft, t));
    let roar = noise_layer(p, sub(seed, 1), |s, k| lp4(&pinkn(s, k), 1_800.0));
    let hiss = noise_layer(p, sub(seed, 2), |s, k| band(&white(s, k), 1_500.0, 5_500.0));
    let rumble = noise_layer(p, sub(seed, 3), |s, k| lp4(&brownn(s, k), 180.0));
    let x = mix(p, &[(&whine, 0.16), (&whine2, 0.09), (&hp_whine, 0.05), (&gear, 0.08), (&roar, 0.9), (&hiss, 0.22), (&rumble, 0.5)]);
    norm(cyc(&x, 0.05, |x| lp4(x, 6_000.0)))
}

/// Electric propulsion motor: magnetostrictive hum at twice the electrical frequency with slot harmonics, the
/// inverter's switching whine with its sidebands, the cooling fan, and shaft and bearing rumble. Quiet.
fn electric(p: usize, seed: u64) -> Vec<f32> {
    let per = ps(p);
    let f0 = snap_hz(100.0, per);
    let hum = norm(osc(per, move |_| f0, &[(1.0, 1.0), (2.0, 0.5), (3.0, 0.35), (4.0, 0.12), (6.0, 0.2), (12.0, 0.05)]));
    let pwm = norm(osc(per, move |_| f0, &[(20.0, 1.0), (18.0, 0.4), (22.0, 0.4), (40.0, 0.12)]));
    let fan_f = snap_hz(240.0, per);
    let fan_tone = norm(osc(per, move |_| fan_f, &[(1.0, 1.0), (2.0, 0.3)]));
    let fan = noise_layer(p, sub(seed, 1), |s, k| bp2(&pinkn(s, k), 900.0, 0.8));
    let shaft = noise_layer(p, sub(seed, 2), |s, k| lp4(&brownn(s, k), 250.0));
    let x = mix(p, &[(&hum, 0.5), (&pwm, 0.12), (&fan_tone, 0.04), (&fan, 0.2), (&shaft, 0.3)]);
    norm(cyc(&x, 0.05, |x| lp4(x, 6_000.0)))
}

/// Propeller cavitation: bubble-collapse crackle and small bubbles whose density peaks as each blade passes
/// the wake (16 Hz blade rate), over a modulated hiss and a low growl.
fn cavitation(p: usize, seed: u64) -> Vec<f32> {
    let per = ps(p);
    let fb = snap_hz(16.0, per);
    let dens = move |t: f32| {
        let c = 0.5 + 0.5 * libm::cosf(TAU * fb * t);
        0.25 + 0.75 * c * c * c
    };
    let tail = frames(0.03);
    let mut crackle = vec![0.0f32; p + tail];
    let times = scatter(sub(seed, 1), (per * 1_300.0) as usize, 0.0, per, dens);
    for (i, &t) in times.iter().enumerate() {
        let i = i as u64;
        let a = rr(seed, 3 * i, 0.15, 1.0);
        tick_grain(&mut crackle, t, rr(seed, 3 * i + 1, 0.0003, 0.0012), rr(seed, 3 * i + 2, 1_800.0, 4_500.0), a * a, sub(seed, 10_000 + i));
    }
    let crackle = norm(fold(crackle, p));
    let mut bubbles = vec![0.0f32; p + tail];
    let times = scatter(sub(seed, 2), (per * 150.0) as usize, 0.0, per, dens);
    for (i, &t) in times.iter().enumerate() {
        let i = i as u64;
        drop_grain(&mut bubbles, t, rr(seed, 50_000 + 3 * i, 1_500.0, 4_000.0), rr(seed, 50_001 + 3 * i, 1.1, 1.3), rr(seed, 50_002 + 3 * i, 0.001, 0.003), rr(seed, 60_000 + i, 0.2, 1.0));
    }
    let bubbles = norm(fold(bubbles, p));
    let d: Vec<f32> = (0..p).map(|i| dens(i as f32 / SR)).collect();
    let hiss = mul(noise_layer(p, sub(seed, 3), |s, k| band(&white(s, k), 900.0, 5_000.0)), &d);
    let growl = mul(noise_layer(p, sub(seed, 4), |s, k| lp4(&brownn(s, k), 350.0)), &d);
    let x = mix(p, &[(&crackle, 0.8), (&bubbles, 0.25), (&hiss, 0.4), (&growl, 0.45)]);
    norm(cyc(&x, 0.05, |x| lp4(x, 5_000.0)))
}

// ---------------------------------------------------------------------------------------------------------
// Hull wash.

/// Water moving along the hull, heard on deck (stereo). Slow: low gurgling flow that swells as small waves
/// pass, bubbly gurgles and soft slaps on the plating. Fast: the bow wave's roar and a broadband rush, spray
/// bursts, foam fizz and entrained bubbles.
fn wash(p: usize, seed: u64, fast: bool) -> Vec<Vec<f32>> {
    let per = ps(p);
    // The low component is shared by both sides (it anchors the image); the rest is decorrelated per side.
    let low = noise_layer(p, sub(seed, 1), |s, k| lp4(&brownn(s, k), if fast { 450.0 } else { 160.0 }));
    let mut chs: Vec<Vec<f32>> = (0..2u64)
        .map(|c| {
            let sd = sub(seed, 10 + c);
            let tail = frames(0.6);
            if fast {
                let b = Bumps::new(sub(sd, 1), per, 9, (0.3, 0.8), (0.4, 0.9), (0.3, 0.6));
                let e = env_buf(&b, p, 0.75, 0.5);
                let rush = mul(noise_layer(p, sub(sd, 2), |s, k| band(&pinkn(s, k), 300.0, 7_000.0)), &e);
                let sb = Bumps::new(sub(sd, 3), per, 14, (0.03, 0.08), (0.12, 0.3), (0.3, 1.0));
                let se = env_buf(&sb, p, 0.05, 1.0);
                let spray = mul(noise_layer(p, sub(sd, 4), |s, k| hp2(&white(s, k), 2_500.0)), &se);
                let mut fizz = vec![0.0f32; p + tail];
                for (i, &t) in scatter(sub(sd, 5), (per * 150.0) as usize, 0.0, per, |_| 1.0).iter().enumerate() {
                    let i = i as u64;
                    tick_grain(&mut fizz, t, 0.0004, 6_000.0, rr(sd, 3 * i, 0.1, 1.0), sub(sd, 1_000 + i));
                }
                let fizz = norm(fold(fizz, p));
                let mut bub = vec![0.0f32; p + tail];
                for (i, &t) in scatter(sub(sd, 6), (per * 60.0) as usize, 0.0, per, |_| 1.0).iter().enumerate() {
                    let i = i as u64;
                    drop_grain(&mut bub, t, rr(sd, 90_000 + 4 * i, 400.0, 2_500.0), rr(sd, 90_001 + 4 * i, 1.1, 1.6), rr(sd, 90_002 + 4 * i, 0.003, 0.01), rr(sd, 90_003 + 4 * i, 0.2, 1.0));
                }
                let bub = norm(fold(bub, p));
                let lowm = mul(low.clone(), &e);
                mix(p, &[(&rush, 1.0), (&lowm, 0.6), (&spray, 0.3), (&fizz, 0.15), (&bub, 0.25)])
            } else {
                let b = Bumps::new(sub(sd, 1), per, 5, (0.6, 1.0), (0.8, 1.4), (0.5, 1.0));
                let e = env_buf(&b, p, 0.35, 1.0);
                let b2 = b.ctl();
                let flow = cyclize(bp(&pinkn(nsec(p), sub(sd, 2)), sub(sd, 3), move |t| 450.0 + 450.0 * b2.at(t), 0.7), p);
                let flow = mul(norm(flow), &e);
                let mut gurgle = vec![0.0f32; p + tail];
                let eb = b.ctl();
                for (i, &t) in scatter(sub(sd, 4), (per * 25.0) as usize, 0.0, per, |t| 0.3 + 0.7 * eb.at(t).min(1.0)).iter().enumerate() {
                    let i = i as u64;
                    let a = rr(sd, 4 * i + 3, 0.2, 1.0) * (0.35 + eb.at(t));
                    drop_grain(&mut gurgle, t, rr(sd, 4 * i, 250.0, 900.0), rr(sd, 4 * i + 1, 1.2, 1.8), rr(sd, 4 * i + 2, 0.004, 0.012), a);
                }
                let gurgle = norm(fold(gurgle, p));
                let mut slaps = vec![0.0f32; p + tail];
                for (k, (t, a)) in b.events().enumerate() {
                    let k = k as u64;
                    let sl = shape(band(&white(0.4, sub(sd, 200 + k)), 200.0, 1_800.0), |t| ad(t, 0.008, 0.06));
                    let sl = lp4(&sl, 2_000.0);
                    add(&mut slaps, &norm(sl), t + rr(sd, 300 + k, 0.4, 0.8), a);
                }
                let slaps = norm(fold(slaps, p));
                let lowm = mul(low.clone(), &e);
                mix(p, &[(&flow, 0.8), (&lowm, 0.5), (&gurgle, 0.35), (&slaps, 0.3)])
            }
        })
        .collect();
    bed_finish(&mut chs);
    chs
}

// ---------------------------------------------------------------------------------------------------------
// Sea beds.

/// One sea bed: waves per period, their size range, how hard they break, hull lapping, distant breakers,
/// and the continuous low heave, mid surf and high hiss under them, bubbly gurgles and spray.
struct Sea {
    waves: usize,
    size: (f32, f32),
    crash: f32,
    lap: f32,
    minor: usize,
    body_lp: f32,
    low: f32,
    mid: f32,
    hiss: f32,
    drops: usize,
    spray: f32,
}

const CALM: Sea = Sea { waves: 7, size: (0.15, 0.45), crash: 0.0, lap: 1.0, minor: 0, body_lp: 220.0, low: 0.45, mid: 0.15, hiss: 0.0, drops: 160, spray: 0.0 };
const MODERATE: Sea = Sea { waves: 5, size: (0.4, 0.75), crash: 0.45, lap: 0.35, minor: 4, body_lp: 280.0, low: 0.5, mid: 0.3, hiss: 0.06, drops: 90, spray: 0.0 };
const ROUGH: Sea = Sea { waves: 5, size: (0.6, 0.95), crash: 0.8, lap: 0.1, minor: 6, body_lp: 320.0, low: 0.6, mid: 0.45, hiss: 0.12, drops: 40, spray: 0.15 };
const STORM: Sea = Sea { waves: 6, size: (0.8, 1.0), crash: 1.0, lap: 0.0, minor: 8, body_lp: 380.0, low: 0.8, mid: 0.75, hiss: 0.25, drops: 0, spray: 0.35 };

/// One passing wave (mono, unit peak): the low heave of the swell, the wash riding it, and when it breaks the
/// crest's roar with a brightening then darkening band, foam fizz and the thump of the lip landing; on calm
/// water a few slaps and plunks against the hull instead.
fn wave_event(seed: u64, size: f32, crash: f32, lap: f32, body_lp: f32) -> Vec<f32> {
    let rise = lerp(0.9, 1.8, size);
    let tau = lerp(0.9, 2.2, size);
    let len = rise + tau * 5.0;
    let mut ev = silence(len);
    let sw = shape(lp4(&brownn(len, sub(seed, 1)), body_lp), move |t| swell(t, 0.0, rise, tau));
    add(&mut ev, &norm(sw), 0.0, 0.55);
    let wlen = rise * 1.1 + tau * 4.0;
    let wash = shape(band(&pinkn(wlen, sub(seed, 2)), 250.0, 2_200.0), move |t| swell(t, 0.0, rise * 1.1, tau * 0.8));
    add(&mut ev, &norm(wash), 0.0, 0.35);
    if crash > 0.0 {
        let crest = rise * 0.85;
        let br_tau = lerp(0.35, 1.1, crash) * lerp(0.7, 1.2, size);
        let br_rise = lerp(0.08, 0.2, crash);
        let br_len = br_rise + br_tau * 6.0;
        let top = 1_800.0 + 3_800.0 * crash;
        // The band opens as the lip breaks and closes as the foam settles: a crossfade between a bright and a
        // dark low-pass of the same noise.
        let src = pinkn(br_len, sub(seed, 3));
        let bright = norm(lp4(&src, top));
        let dark = norm(lp4(&src, 500.0));
        let mut n = silence(br_len);
        for (i, v) in n.iter_mut().enumerate() {
            let t = i as f32 / SR;
            let w = swell(t, 0.0, 0.12, br_tau * 0.7);
            *v = (bright[i] * w + dark[i] * (1.0 - w)) * swell(t, 0.0, br_rise, br_tau);
        }
        add(&mut ev, &norm(n), crest, crash * 0.9);
        let fz_len = 0.3 + br_tau * 9.0;
        let fz = shape(band(&white(fz_len, sub(seed, 5)), 2_000.0, 7_000.0), move |t| swell(t, 0.1, 0.2, br_tau * 1.5));
        add(&mut ev, &norm(fz), crest, crash * 0.16);
        let th = shape(lp4(&brownn(1.2, sub(seed, 6)), 180.0), |t| ad(t, 0.02, 0.18));
        add(&mut ev, &norm(th), crest + 0.05, crash * 0.5 * size);
    }
    if lap > 0.0 {
        for j in 0..3u64 {
            let at = rise * (0.6 + 0.35 * j as f32) + rr(seed, 30 + j, -0.08, 0.08);
            let sl = shape(band(&white(0.3, sub(seed, 40 + j)), 280.0, 1_600.0), |t| ad(t, 0.006, 0.05));
            add(&mut ev, &norm(sl), at, lap * 0.3 * rr(seed, 50 + j, 0.5, 1.0));
            drop_grain(&mut ev, at + 0.01, rr(seed, 60 + j, 220.0, 480.0), 1.3, 0.02, lap * 0.22);
        }
    }
    let end = len;
    shape(ev, move |t| smooth((end - t) / (0.15 * end)))
}

/// A sea bed (stereo): waves panned around the listener and folded into the period, a continuous bed per side
/// whose surf swells with the waves, and gurgles; tamed to the common crest factor.
fn sea(p: usize, seed: u64, cfg: &Sea) -> Vec<Vec<f32>> {
    let per = ps(p);
    let tail = frames(14.0);
    let mut ev_l = vec![0.0f32; p + tail];
    let mut ev_r = vec![0.0f32; p + tail];
    let mut place = |e: &[f32], at: f32, pn: f32, g: f32| {
        let (l, r) = pan(pn);
        add(&mut ev_l, e, at, g * l);
        add(&mut ev_r, e, at, g * r);
    };
    let n = cfg.waves.max(1);
    for k in 0..n as u64 {
        let at = (k as f32 + rr(seed, 10 * k, 0.1, 0.9)) * per / n as f32;
        let size = rr(seed, 10 * k + 1, cfg.size.0, cfg.size.1);
        let crash = cfg.crash * rr(seed, 10 * k + 2, 0.7, 1.0);
        let e = wave_event(sub(seed, 100 + k), size, crash, cfg.lap, cfg.body_lp);
        place(&e, at, rr(seed, 10 * k + 3, -0.85, 0.85), lerp(0.6, 1.0, size));
    }
    for k in 0..cfg.minor as u64 {
        let at = (k as f32 + rr(seed, 2_000 + 10 * k, 0.0, 1.0)) * per / cfg.minor.max(1) as f32;
        let e = wave_event(sub(seed, 500 + k), rr(seed, 2_001 + 10 * k, 0.2, 0.5), cfg.crash * 0.6, 0.0, cfg.body_lp);
        let e = lp4(&e, 2_000.0);
        place(&e, at, rr(seed, 2_002 + 10 * k, -1.0, 1.0), 0.3);
    }
    let evs = [fold(ev_l, p), fold(ev_r, p)];
    let surf = Bumps::new(sub(seed, 3), per, cfg.waves.max(1) * 2, (1.0, 2.0), (1.0, 2.0), (0.5, 1.0));
    let se = env_buf(&surf, p, 0.6, 0.5);
    let ev_peak = evs.iter().flat_map(|e| e.iter()).fold(0.0f32, |m, v| m.max(v.abs())).max(1e-9);
    let mut chs: Vec<Vec<f32>> = (0..2usize)
        .map(|c| {
            let sd = sub(seed, 20 + c as u64);
            let low = noise_layer(p, sub(sd, 1), |s, k| lp4(&brownn(s, k), cfg.body_lp));
            let mid = mul(noise_layer(p, sub(sd, 2), |s, k| band(&pinkn(s, k), 300.0, 3_500.0)), &se);
            let mut layers: Vec<(Vec<f32>, f32)> = vec![(low, cfg.low), (mid, cfg.mid)];
            if cfg.hiss > 0.0 {
                layers.push((noise_layer(p, sub(sd, 3), |s, k| band(&white(s, k), 2_500.0, 7_500.0)), cfg.hiss));
            }
            if cfg.spray > 0.0 {
                let sb = Bumps::new(sub(sd, 4), per, 10, (0.05, 0.15), (0.2, 0.5), (0.4, 1.0));
                let e = env_buf(&sb, p, 0.0, 1.0);
                layers.push((mul(noise_layer(p, sub(sd, 5), |s, k| hp2(&white(s, k), 3_000.0)), &e), cfg.spray));
            }
            if cfg.drops > 0 {
                let mut g = vec![0.0f32; p + frames(0.2)];
                for (i, &t) in scatter(sub(sd, 6), cfg.drops, 0.0, per, |_| 1.0).iter().enumerate() {
                    let i = i as u64;
                    let a = rr(sd, 4 * i + 3, 0.1, 1.0);
                    drop_grain(&mut g, t, rr(sd, 4 * i, 300.0, 1_100.0), rr(sd, 4 * i + 1, 1.2, 1.7), rr(sd, 4 * i + 2, 0.004, 0.014), a * a);
                }
                layers.push((norm(fold(g, p)), 0.12));
            }
            // Both sides share one scale for the panned waves, so the image keeps its balance.
            let waves: Vec<f32> = evs[c].iter().map(|v| v / ev_peak).collect();
            layers.push((waves, 1.0));
            let refs: Vec<(&[f32], f32)> = layers.iter().map(|(x, g)| (x.as_slice(), *g)).collect();
            mix(p, &refs)
        })
        .collect();
    bed_finish(&mut chs);
    chs
}

// ---------------------------------------------------------------------------------------------------------
// Weather.

/// Wind over the deck (stereo): a band of moving air whose centre rises in the gusts, a whistle in the rigging
/// that sings in the gusts, and low buffeting. The gale adds a second, higher howl, spray hiss and harder
/// gusts.
fn wind(p: usize, seed: u64, gale: bool) -> Vec<Vec<f32>> {
    let per = ps(p);
    let gusts = if gale {
        Bumps::new(sub(seed, 1), per, 6, (0.6, 1.6), (1.0, 1.8), (0.6, 1.0))
    } else {
        Bumps::new(sub(seed, 1), per, 4, (1.5, 3.0), (1.5, 2.4), (0.5, 1.0))
    };
    let base = if gale { 0.7 } else { 0.45 };
    let mut chs: Vec<Vec<f32>> = (0..2u64)
        .map(|c| {
            let sd = sub(seed, 10 + c);
            // The far side hears the same gusts a moment later.
            let lag = 0.35 * c as f32;
            let g = gusts.ctl();
            let gat = move |t: f32| g.at(t - lag);
            let env: Vec<f32> = (0..p).map(|i| base + gat(i as f32 / SR)).collect();
            let env2: Vec<f32> = env.iter().map(|e| e * e).collect();
            let (c0, c1) = if gale { (600.0, 900.0) } else { (350.0, 500.0) };
            let gb = gat.clone();
            let body = cyclize(bp(&pinkn(nsec(p), sub(sd, 1)), sub(sd, 2), move |t| c0 + c1 * gb(t), if gale { 0.5 } else { 0.6 }), p);
            let body = mul(norm(body), &env);
            let w0 = rr(sd, 1, 850.0, 1_000.0) * if gale { 0.75 } else { 1.0 };
            let gw = gat.clone();
            let whistle = cyclize(bp(&white(nsec(p), sub(sd, 3)), sub(sd, 4), move |t| w0 + 700.0 * gw(t), 12.0), p);
            let whistle = norm(mul(norm(whistle), &env2));
            let buffet = mul(noise_layer(p, sub(sd, 5), |s, k| lp4(&brownn(s, k), 120.0)), &env);
            let mut layers: Vec<(Vec<f32>, f32)> = vec![(body, 1.0), (whistle, if gale { 0.3 } else { 0.18 }), (buffet, if gale { 0.6 } else { 0.3 })];
            if gale {
                let gh = gat.clone();
                let w1 = w0 * 2.0;
                let howl = cyclize(bp(&white(nsec(p), sub(sd, 6)), sub(sd, 7), move |t| w1 + 900.0 * gh(t), 15.0), p);
                layers.push((norm(mul(norm(howl), &env2)), 0.2));
                let spray = mul(noise_layer(p, sub(sd, 8), |s, k| hp2(&white(s, k), 3_000.0)), &env);
                layers.push((spray, 0.25));
            }
            let refs: Vec<(&[f32], f32)> = layers.iter().map(|(x, g)| (x.as_slice(), *g)).collect();
            mix(p, &refs)
        })
        .collect();
    bed_finish(&mut chs);
    chs
}

/// Rain (stereo): drops on the water (bubble chirps), ticks on steel decks and fittings, the sizzle of the
/// surface, and low patter. Heavy rain triples the density, adds drumming and runoff from the scuppers.
fn rain(p: usize, seed: u64, heavy: bool) -> Vec<Vec<f32>> {
    let per = ps(p);
    let k = if heavy { 3.0 } else { 1.0 };
    let mut chs: Vec<Vec<f32>> = (0..2u64)
        .map(|c| {
            let sd = sub(seed, 10 + c);
            let tail = frames(0.05);
            let mut drops = vec![0.0f32; p + tail];
            for (i, &t) in scatter(sub(sd, 1), (per * 220.0 * k) as usize, 0.0, per, |_| 1.0).iter().enumerate() {
                let i = i as u64;
                let a = rr(sd, 4 * i + 3, 0.1, 1.0);
                drop_grain(&mut drops, t, rr(sd, 4 * i, 1_000.0, 4_000.0), rr(sd, 4 * i + 1, 1.1, 1.5), rr(sd, 4 * i + 2, 0.002, 0.006), a * a);
            }
            let drops = norm(fold(drops, p));
            let mut ticks = vec![0.0f32; p + tail];
            for (i, &t) in scatter(sub(sd, 2), (per * 150.0 * k) as usize, 0.0, per, |_| 1.0).iter().enumerate() {
                let i = i as u64;
                let a = rr(sd, 200_000 + 3 * i, 0.1, 1.0);
                tick_grain(&mut ticks, t, rr(sd, 200_001 + 3 * i, 0.0005, 0.0015), rr(sd, 200_002 + 3 * i, 3_000.0, 6_000.0), a * a, sub(sd, 300_000 + i));
            }
            let ticks = norm(fold(ticks, p));
            let hiss = noise_layer(p, sub(sd, 3), |s, k| band(&pinkn(s, k), 800.0, 7_000.0));
            let low = noise_layer(p, sub(sd, 4), |s, k| lp4(&brownn(s, k), 300.0));
            let mut layers: Vec<(Vec<f32>, f32)> = vec![(drops, 0.5), (ticks, 0.3), (hiss, if heavy { 1.0 } else { 0.6 }), (low, 0.15)];
            if heavy {
                let drum = noise_layer(p, sub(sd, 5), |s, k| bp2(&brownn(s, k), 160.0, 0.8));
                layers.push((drum, 0.35));
                let rb = Bumps::new(sub(sd, 6), per, 4, (0.5, 1.2), (0.6, 1.2), (0.4, 1.0));
                let re = env_buf(&rb, p, 0.4, 0.6);
                let runoff = mul(noise_layer(p, sub(sd, 7), |s, k| band(&pinkn(s, k), 400.0, 1_200.0)), &re);
                layers.push((runoff, 0.25));
            }
            let refs: Vec<(&[f32], f32)> = layers.iter().map(|(x, g)| (x.as_slice(), *g)).collect();
            mix(p, &refs)
        })
        .collect();
    bed_finish(&mut chs);
    chs
}

/// Thunder (stereo one-shot). Near: the tearing crackle of the channel and a few sharp sub-cracks, the boom,
/// then a heavy rolling rumble and its reflections. Far: no crack, a slow low rise and several rolls, heavily
/// low-passed by distance.
fn thunder(sec: f32, seed: u64, near: bool) -> Vec<Vec<f32>> {
    let mut common = silence(sec);
    if near {
        let mut crack = silence(sec.min(1.0));
        let t0 = 0.05;
        let times = scatter(sub(seed, 1), 420, t0, t0 + 0.35, move |t| libm::expf(-(t - t0) / 0.12));
        for (i, &t) in times.iter().enumerate() {
            let i = i as u64;
            let a = rr(seed, 3 * i, 0.2, 1.0) * libm::expf(-(t - t0) / 0.2);
            tick_grain(&mut crack, t, rr(seed, 3 * i + 1, 0.0005, 0.002), rr(seed, 3 * i + 2, 3_000.0, 7_500.0), a, sub(seed, 10_000 + i));
        }
        let burst = shape(noise_band(0.6, sub(seed, 2), 300.0, 6_000.0), |t| ad(t, 0.002, 0.06));
        add(&mut crack, &norm(burst), t0, 0.8);
        for j in 0..4u64 {
            let at = t0 + rr(seed, 500 + j, 0.0, 0.35);
            add(&mut crack, &n_wave(rr(seed, 510 + j, 0.002, 0.005)), at, rr(seed, 520 + j, 0.4, 0.9));
        }
        add(&mut common, &norm(lp4(&crack, 7_500.0)), 0.0, 0.8);
        let boom = osc(2.5f32.min(sec), |t| glide(t, 90.0, 35.0, 0.25), &[(1.0, 1.0), (2.0, 0.2)]);
        add(&mut common, &norm(shape(boom, |t| ad(t, 0.01, 0.4))), 0.08, 0.7);
    }
    (0..2u64)
        .map(|c| {
            let sd = sub(seed, 20 + c);
            let mut x = silence(sec);
            if near {
                // The crack reaches the far ear a fraction of a millisecond later.
                add(&mut x, &common, 0.0004 * c as f32, 1.0);
                let r = rumble(sec, sub(sd, 1), 220.0, 0.15, 0.4, (sec / 4.3).max(0.2), 0.9);
                add(&mut x, &r, 0.0, 0.9);
                let hd = head(&common, 0.4);
                echoes(&mut x, &hd, 0.0, sub(sd, 2), 6, 0.3, 0.4, 0.6, 1_200.0, 0.7);
            } else {
                let tau = (sec / 4.0).max(0.2);
                let r = rumble(sec, sub(sd, 1), 150.0, 0.2, 1.2, tau, 1.0);
                add(&mut x, &r, 0.0, 1.0);
                for j in 0..2u64 {
                    let at = rr(sd, 10 + j, 1.2, 3.2).min(sec * 0.4);
                    let r2 = rumble(sec, sub(sd, 2 + j), 180.0, 0.0, 0.6, tau * 0.6, 0.8);
                    add(&mut x, &r2, at, rr(sd, 20 + j, 0.4, 0.7));
                }
                let gr = lp4(&band(&brownn(sec, sub(sd, 5)), 80.0, 400.0), 300.0);
                let gr = shape(gr, move |t| swell(t, 0.4, 1.5, tau));
                add(&mut x, &norm(gr), 0.0, 0.3);
            }
            let end = sec;
            norm(shape(x, move |t| smooth((end - t) / (0.12 * end))))
        })
        .collect()
}

/// Sonar (mono one-shot). Own ping: a 2.1 kHz continuous-wave pulse with a quick attack, the transducer's
/// ring, ocean reverberation around the ping frequency and late bottom and surface returns. Heard ping:
/// another unit's 1.65 kHz pulse through the hull, softened by dispersion, doubled by a surface bounce,
/// muffled by the plating, with a longer diffuse tail.
fn sonar(sec: f32, seed: u64, heard: bool) -> Vec<f32> {
    let mut out = silence(sec);
    let (f, hold, attack) = if heard { (1_650.0f32, 0.35f32, 0.02f32) } else { (2_100.0, 0.28, 0.004) };
    let len = hold + 0.1;
    let pulse = osc(len, move |_| f, &[(1.0, 1.0), (2.0, 0.04)]);
    let pulse = norm(shape(pulse, move |t| smooth(t / attack) * smooth((hold - t) / 0.04 + 1.0)));
    let at = 0.004;
    add(&mut out, &pulse, at, 1.0);
    if heard {
        add(&mut out, &onepole(&pulse, 2_500.0, 2), at + 0.035, 0.6);
        add(&mut out, &onepole(&pulse, 1_800.0, 2), at + 0.09, 0.3);
    } else {
        let ring = modal(&pulse, &[mode(f, 0.35, 1.0), mode(f * 1.003, 0.25, 0.6)]);
        add(&mut out, &norm(ring), at, 0.12);
        echoes(&mut out, &head(&pulse, len), at, sub(seed, 1), 3, 0.45, 0.5, 0.45, 3_500.0, 0.8);
    }
    let tau = if heard { 0.8 } else { 0.55 };
    let rv = shape(bp2(&white(sec, sub(seed, 2)), f, if heard { 5.0 } else { 6.0 }), move |t| swell(t, at + 0.05, 0.15, tau));
    add(&mut out, &norm(rv), 0.0, if heard { 0.25 } else { 0.18 });
    let out = if heard { lp4(&out, 2_800.0) } else { out };
    let end = sec;
    norm(shape(out, move |t| smooth((end - t) / (0.1 * end))))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SoundId;

    /// One channel of a rendered file as f32.
    fn channel(s: &[i16], ch: usize, c: usize) -> Vec<f32> {
        s.iter().skip(c).step_by(ch).map(|&v| v as f32).collect()
    }

    fn rms(x: &[f32]) -> f32 {
        libm::sqrtf(x.iter().map(|v| v * v).sum::<f32>() / x.len().max(1) as f32)
    }

    #[test]
    fn helpers_make_cyclic_periods() {
        let mut x = vec![0.0f32; 12];
        x[11] = 1.0;
        x[10] = 2.0;
        let f = fold(x, 8);
        assert_eq!(f.len(), 8);
        assert_eq!((f[2], f[3]), (2.0, 1.0));
        assert_eq!(cyclize(vec![1.0; 3], 8).len(), 8);
        // A filter run through `cyc` is in steady state at the head: no step at the wrap.
        let p = 4_800;
        let s: Vec<f32> = (0..p).map(|i| libm::sinf(TAU * 100.0 * i as f32 / SR)).collect();
        let y = cyc(&s, 0.05, |x| lp4(x, 400.0));
        let step = (y[0] - y[p - 1]).abs();
        let typical = (y[1] - y[0]).abs().max((y[p - 1] - y[p - 2]).abs());
        assert!(step < typical * 2.0 + 1e-4, "{step} {typical}");
        let b = Bumps::new(3, 2.0, 3, (0.1, 0.2), (0.1, 0.2), (1.0, 1.0));
        assert!((b.at(0.0) - b.at(2.0)).abs() < 1e-5 && (b.at(0.37) - b.at(4.37)).abs() < 1e-4);
        let mut t = vec![vec![0.0f32, 3.0, -0.1, 0.2]];
        tame(&mut t, 6.0);
        assert!(t[0][1] < 3.0 && t[0][1] > 0.0);
    }

    #[test]
    fn every_ambience_renders_clean() {
        let mut crest: Vec<(AmbienceSfx, f32)> = Vec::new();
        for a in AmbienceSfx::ALL {
            let id = SoundId::Ambience(a);
            let spec = id.spec();
            let raw = render(a, &spec, id.seed());
            let ch = spec.channels as usize;
            assert_eq!(raw.len(), frames(render_seconds(&spec)) * ch, "{a:?}");
            assert!(raw.iter().all(|v| v.is_finite()), "{a:?}");
            if spec.playback == Playback::Loop {
                // The overhang repeats the head exactly, so `finish` leaves the loop untouched.
                let p = spec_frames(&spec);
                for i in 0..xf(p) * ch {
                    assert!((raw[p * ch + i] - raw[i]).abs() < 1e-6, "{a:?} overhang differs at {i}");
                }
            }
            let pcm = crate::render::finish(raw, &spec, id.seed());
            assert_eq!(pcm.frames(), spec_frames(&spec), "{a:?}");
            let peak = pcm.samples.iter().map(|&s| (s as i32).abs()).max().unwrap();
            // Levels are loudness targets (`analysis::target`); the peak only has to sit under the ceiling.
            let ceiling = libm::powf(10.0, spec.peak_dbfs.min(crate::render::CEILING_DBFS) / 20.0) * 32767.0;
            assert!(peak as f32 <= ceiling + 2.0 && peak as f32 > ceiling * 0.01, "{a:?} peak {peak} ceiling {ceiling}");
            for c in 0..ch {
                let s = channel(&pcm.samples, ch, c);
                let loud = s.iter().filter(|v| v.abs() > peak as f32 / 100.0).count();
                assert!(loud > s.len() / 20, "{a:?} mostly silent");
                if spec.playback == Playback::Loop {
                    let mean_step = s.windows(2).map(|w| (w[1] - w[0]).abs()).sum::<f32>() / (s.len() - 1) as f32;
                    let wrap = (s[0] - s[s.len() - 1]).abs();
                    assert!(wrap < mean_step * 8.0 + 64.0, "{a:?} ch {c} seam {wrap} vs {mean_step}");
                } else {
                    let tail = &s[s.len() * 95 / 100..];
                    let tail_peak = tail.iter().fold(0.0f32, |m, v| m.max(v.abs()));
                    assert!(tail_peak < peak as f32 / 4.0, "{a:?} tail {tail_peak} peak {peak}");
                }
            }
            if ch == 2 {
                // Beds are wide: the two sides are far from identical.
                let (l, r) = (channel(&pcm.samples, 2, 0), channel(&pcm.samples, 2, 1));
                let (mut lr, mut ll, mut rq) = (0.0f64, 0.0f64, 0.0f64);
                for (x, y) in l.iter().zip(r.iter()) {
                    lr += (*x as f64) * (*y as f64);
                    ll += (*x as f64) * (*x as f64);
                    rq += (*y as f64) * (*y as f64);
                }
                let corr = lr / libm::sqrt(ll * rq).max(1e-9);
                assert!(corr < 0.97, "{a:?} channels correlate {corr}");
                // Not shrill: little energy above 8 kHz.
                let hi = hp2(&hp2(&l, 8_500.0), 8_500.0);
                eprintln!("{a:?} corr {corr:.3} hi8k {:.4}", rms(&hi) / rms(&l));
                assert!(rms(&hi) / rms(&l) < 0.1, "{a:?} shrill");
                if spec.playback == Playback::Loop {
                    let all: Vec<f32> = pcm.samples.iter().map(|&v| v as f32).collect();
                    crest.push((a, 20.0 * libm::log10f(peak as f32 / rms(&all))));
                }
            }
        }
        // Beds that crossfade share one crest factor, so their peaks mean equal loudness.
        assert_eq!(crest.len(), 10);
        for (a, c) in crest {
            eprintln!("{a:?} crest {c:.2} dB");
            assert!((9.0..=13.5).contains(&c), "{a:?} crest {c} dB");
        }
    }

    #[test]
    fn engines_stay_clean_when_pitched() {
        // At 1.6x the content must stay under about 10 kHz: little energy above 6.5 kHz as rendered.
        for a in [AmbienceSfx::EngineDiesel, AmbienceSfx::EngineGasTurbine, AmbienceSfx::EngineElectric, AmbienceSfx::Cavitation] {
            let spec = SoundId::Ambience(a).spec();
            let x = render(a, &spec, 7);
            let hi = hp2(&hp2(&x, 6_500.0), 6_500.0);
            let ratio = rms(&hi) / rms(&x);
            let limit = if a == AmbienceSfx::Cavitation { 0.12 } else { 0.05 };
            eprintln!("{a:?} high band {ratio:.4} (limit {limit})");
            assert!(ratio < limit, "{a:?} high band {ratio}");
            // A real low end, so 0.6x keeps the engine's weight rather than turning to hiss.
            if a != AmbienceSfx::Cavitation {
                let lo = lp4(&x, 400.0);
                eprintln!("{a:?} low share {:.3}", rms(&lo) / rms(&x));
                assert!(rms(&lo) / rms(&x) > 0.2, "{a:?} thin");
            }
        }
    }

    #[test]
    fn ambience_is_deterministic_and_robust() {
        for a in [AmbienceSfx::EngineDiesel, AmbienceSfx::Cavitation, AmbienceSfx::SonarPing] {
            assert_eq!(crate::render_wav(SoundId::Ambience(a)), crate::render_wav(SoundId::Ambience(a)));
        }
        // Degenerate spec rows neither panic nor produce non-finite samples.
        let mut s = SoundId::Ambience(AmbienceSfx::Cavitation).spec();
        s.duration_s = f32::NAN;
        assert!(render(AmbienceSfx::Cavitation, &s, 1).iter().all(|v| v.is_finite()));
        s.duration_s = 0.0;
        assert!(render(AmbienceSfx::Cavitation, &s, 1).iter().all(|v| v.is_finite()));
        let mut s = SoundId::Ambience(AmbienceSfx::SonarPing).spec();
        s.playback = Playback::Loop;
        s.channels = 3;
        let x = render(AmbienceSfx::SonarPing, &s, 1);
        assert_eq!(x.len(), frames(render_seconds(&s)));
        let mut s = SoundId::Ambience(AmbienceSfx::EngineElectric).spec();
        s.playback = Playback::OneShot;
        assert_eq!(render(AmbienceSfx::EngineElectric, &s, 1).len(), frames(render_seconds(&s)));
    }
}
