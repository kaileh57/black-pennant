//! Unit `audio-signals`. Alert patterns, the radio effect and interface sounds.
//! DESIGN.md L1678-1685 (four MSC.302(87) priorities, each with its own synthesized sound: Emergency continuous;
//! Alarm repeats until acknowledged or rectified; Warning a chime, again at 120 s; Caution one tick),
//! L1706 (officers and aces are text with radio sounds: squelch, static, band-pass), L1659 (warning red always
//! with sound).
//! The four alert sounds must be distinguishable by rhythm and pitch alone (not just loudness), so a player who
//! hears one without looking knows its priority. UI sounds are short, quiet and dry.
//!
//! Alerts, by rhythm and pitch (each pair of dominant pitches is at least two semitones apart):
//! - Emergency: continuous, never a gap; a bright two-tone warble between 1,320 Hz and 990 Hz (a fourth),
//!   switching every 125 ms. The loop holds whole switching cycles and whole oscillator cycles, so it repeats
//!   with no seam.
//! - Alarm: three hard 110 ms pulses of a square-toned 523 Hz (C5) 170 ms apart, then a gap; the file is one
//!   whole cycle including its gap, so back-to-back repeats keep the rhythm.
//! - Warning: a two-note struck chime falling a fourth, 880 Hz then 659 Hz, ringing out.
//! - Caution: one short pitched tick at 1,568 Hz.
//!
//! Radio (L1706): everything is band-passed to the 300 Hz to 3 kHz voice band at 24 dB/oct. Squelch open is a
//! hiss burst that settles to the static floor; the static bed is band-limited hiss with crackle and a slow
//! flutter; squelch close is the carrier-drop tail (a hiss burst cut off sharply). The ace open adds a
//! heterodyne whistle gliding down and a little grit, so the enemy net sounds unlike our own.
//!
//! UI: unpitched ticks where a sound only confirms input (click, select, rotate, snap, time rate), soft mallet
//! notes where it reports a state change (place, remove, pause, resume, tutorial, objective, saved), and a low
//! dull double pulse for a refusal. Pitched UI notes come from the menu theme's D Dorian, so the interface and
//! the theme share one palette. Nothing rings longer than the file.
use super::ambience::{cyc, cyclize, emit, fold, interleave, mix, nsec, ps, sn};
use crate::render::kit::*;
use crate::render::{render_seconds, spec_frames};
use crate::{AlertSound, Playback, RadioSfx, SoundSpec, UiSfx};
use core::f32::consts::TAU;

/// Pitches (Hz) of the alert patterns.
const EMERGENCY_HI: f32 = 1_320.0;
const EMERGENCY_LO: f32 = 990.0;
/// Alternations of the Emergency warble per second (one high and one low half each).
const EMERGENCY_RATE: f32 = 4.0;
const ALARM_HZ: f32 = 523.25;
const WARNING_HZ: [f32; 2] = [880.0, 659.25];
const CAUTION_HZ: f32 = 1_568.0;

/// Onset offset of one-shots: `finish` fades the first 2 ms in, so transients start just after it.
const T0: f32 = 0.004;

/// Unit: `audio-signals`.
pub fn render_alert(id: AlertSound, spec: &SoundSpec, seed: u64) -> Vec<f32> {
    match spec.playback {
        Playback::Loop => {
            let p = spec_frames(spec);
            let x = match id {
                AlertSound::Emergency => emergency(p),
                // A pattern asked for as a loop: one cycle of it per period.
                _ => fold(alert_once(id, ps(p).max(0.05), seed), p),
            };
            emit(&[x], spec)
        }
        Playback::OneShot => {
            let sec = render_seconds(spec).max(0.05);
            let x = match id {
                AlertSound::Emergency => emergency(frames(sec).max(1)),
                _ => alert_once(id, sec, seed),
            };
            interleave(&[x], spec, frames(sec))
        }
    }
}

/// Squelch bursts and the band-passed static bed (300 Hz to 3 kHz band). Unit: `audio-signals`.
pub fn render_radio(id: RadioSfx, spec: &SoundSpec, seed: u64) -> Vec<f32> {
    match spec.playback {
        Playback::Loop => {
            let p = spec_frames(spec);
            let x = match id {
                RadioSfx::Static => static_bed(p, seed),
                _ => fold(radio_once(id, ps(p).max(0.05), seed), p),
            };
            emit(&[x], spec)
        }
        Playback::OneShot => {
            let sec = render_seconds(spec).max(0.05);
            let x = match id {
                RadioSfx::Static => {
                    let p = frames(sec).max(1);
                    static_bed(p, seed)
                }
                _ => radio_once(id, sec, seed),
            };
            interleave(&[x], spec, frames(sec))
        }
    }
}

/// Unit: `audio-signals`.
pub fn render_ui(id: UiSfx, spec: &SoundSpec, seed: u64) -> Vec<f32> {
    let sec = render_seconds(spec).max(0.02);
    let x = ui(id, sec, seed);
    match spec.playback {
        Playback::Loop => {
            let p = spec_frames(spec);
            emit(&[fold(x, p)], spec)
        }
        Playback::OneShot => interleave(&[x], spec, frames(sec)),
    }
}

// ---------------------------------------------------------------------------------------------------------
// Alerts.

/// An oscillator over exactly `p` frames whose phase is the integral of `f(t)` rescaled to a whole number of
/// cycles, so the period is cyclic for any length. `partials` are (harmonic, amplitude); partials above 8 kHz
/// are dropped.
fn cyclic_osc<F: Fn(f32) -> f32>(p: usize, f: F, partials: &[(f32, f32)]) -> Vec<f32> {
    let mut ph = Vec::with_capacity(p);
    let mut acc = 0.0f64;
    let mut fmax = 0.0f32;
    for i in 0..p {
        ph.push(acc);
        let fi = f(i as f32 / SR);
        let fi = if fi.is_finite() { fi.max(0.0) } else { 0.0 };
        fmax = fmax.max(fi);
        acc += fi as f64 / SR as f64;
    }
    let cycles = libm::round(acc).max(1.0);
    let k = if acc > 0.0 { cycles / acc } else { 0.0 };
    ph.iter()
        .map(|&q| {
            let q = libm::fmod(q * k, 1.0) as f32;
            partials.iter().filter(|(h, _)| h * fmax < 8_000.0).map(|&(h, a)| a * libm::sinf(TAU * libm::fmodf(h * q, 1.0))).sum()
        })
        .collect()
}

/// The Emergency warble over `p` frames: continuous two-tone alternation with smooth 5 ms glides and a light
/// accent at each switch, brassy partials. The switching rate is rounded to whole cycles of the period.
fn emergency(p: usize) -> Vec<f32> {
    let per = ps(p);
    let rate = snap_hz(EMERGENCY_RATE, per);
    let e = 0.02;
    // 0 on the low tone, 1 on the high tone; symmetric edges, so the mean is exactly one half.
    let s = move |t: f32| {
        let u = t * rate - libm::floorf(t * rate);
        if u < 0.5 {
            smooth(u / e)
        } else {
            1.0 - smooth((u - 0.5) / e)
        }
    };
    let f = move |t: f32| EMERGENCY_LO + (EMERGENCY_HI - EMERGENCY_LO) * s(t);
    let tone = cyclic_osc(p, f, &[(1.0, 1.0), (2.0, 0.5), (3.0, 0.35), (4.0, 0.2), (5.0, 0.12), (6.0, 0.06)]);
    let accent = move |t: f32| {
        let u = libm::fmodf(t * rate * 2.0, 1.0) / (rate * 2.0);
        0.85 + 0.15 * smooth(u / 0.0015) * libm::expf(-u / 0.03)
    };
    let x = shape(tone, accent);
    norm(cyc(&x, 0.02, |x| lp4(x, 7_000.0)))
}

/// One cycle of a non-continuous alert, `sec` long.
fn alert_once(id: AlertSound, sec: f32, seed: u64) -> Vec<f32> {
    let mut out = silence(sec);
    match id {
        AlertSound::Emergency => return emergency(frames(sec).max(1)),
        AlertSound::Alarm => {
            let partials = [(1.0, 1.0), (3.0, 0.4), (5.0, 0.22), (7.0, 0.12)];
            let pulse = osc(0.12, |_| ALARM_HZ, &partials);
            let pulse = norm(shape(pulse, |t| gate(t, 0.0, 0.11, 0.004)));
            for k in 0..3 {
                add(&mut out, &pulse, T0 + 0.17 * k as f32, 1.0);
            }
        }
        AlertSound::Warning => {
            // The first strike is damped a little so the second reads as a new strike, which then rings out.
            for (k, &f) in WARNING_HZ.iter().enumerate() {
                let note = chime(1.0, f, if k == 0 { 0.28 } else { 0.45 }, sub(seed, k as u64));
                add(&mut out, &note, T0 + 0.32 * k as f32, if k == 0 { 0.9 } else { 1.0 });
            }
        }
        AlertSound::Caution => {
            let mut ex = silence(0.14);
            if let Some(v) = ex.first_mut() {
                *v = 1.0;
            }
            let body = modal(&ex, &[mode(CAUTION_HZ, 0.018, 1.0), mode(CAUTION_HZ * 2.7, 0.006, 0.25)]);
            add(&mut out, &norm(body), T0, 1.0);
            tick_grain(&mut out, T0, 0.0005, 5_000.0, 0.15, sub(seed, 1));
        }
    }
    let end = sec;
    norm(shape(out, move |t| smooth((end - t) / 0.02)))
}

/// A struck chime at `f`: a soft mallet into bell-like partials; the fundamental rings with time constant
/// `tau` and the upper partials die sooner.
fn chime(sec: f32, f: f32, tau: f32, seed: u64) -> Vec<f32> {
    let ex = lp4(&shape(white(0.004, seed), |t| ad(t, 0.0003, 0.0007)), 4_000.0);
    let mut x = silence(sec);
    add(&mut x, &ex, 0.0, 1.0);
    norm(modal(&x, &[mode(f, tau, 1.0), mode(f * 2.0, tau * 0.55, 0.3), mode(f * 3.0, tau * 0.27, 0.12), mode(f * 4.2, tau * 0.16, 0.08)]))
}

// ---------------------------------------------------------------------------------------------------------
// Radio.

/// The voice band: 300 Hz to 3 kHz at 24 dB/oct on each side.
fn radio_band(x: &[f32]) -> Vec<f32> {
    band(&band(x, 300.0, 3_000.0), 300.0, 3_000.0)
}

/// The static bed over `p` frames (cyclic): band-limited hiss with a slow two-rate flutter and sparse
/// crackle.
fn static_bed(p: usize, seed: u64) -> Vec<f32> {
    let per = ps(p);
    let hiss = norm(cyclize(radio_band(&white(nsec(p), sub(seed, 1))), p));
    let (f1, f2) = (snap_hz(0.7, per), snap_hz(2.3, per));
    let hiss = shape(hiss, move |t| 1.0 + 0.15 * sn(f1, t) + 0.08 * sn(f2, t));
    let mut cr = vec![0.0f32; p + frames(0.02)];
    for (i, &t) in scatter(sub(seed, 2), (per * 30.0) as usize, 0.0, per, |_| 1.0).iter().enumerate() {
        let i = i as u64;
        let a = rr(seed, 3 * i, 0.2, 1.0);
        tick_grain(&mut cr, t, rr(seed, 3 * i + 1, 0.0003, 0.0015), rr(seed, 3 * i + 2, 2_000.0, 5_000.0), a * a, sub(seed, 1_000 + i));
    }
    let cr = norm(cyc(&fold(cr, p), 0.05, radio_band));
    norm(mix(p, &[(&hiss, 1.0), (&cr, 0.5)]))
}

/// 1 for `hold` seconds, then an exponential decay with time constant `tau`.
fn hold_decay(t: f32, hold: f32, tau: f32) -> f32 {
    if t < hold {
        1.0
    } else {
        libm::expf(-(t - hold) / tau.max(1e-4))
    }
}

/// A one-shot radio sound, `sec` long, band-passed.
fn radio_once(id: RadioSfx, sec: f32, seed: u64) -> Vec<f32> {
    let n = white(sec, sub(seed, 1));
    let floor = 0.12;
    let x = match id {
        RadioSfx::SquelchOpen | RadioSfx::Static => {
            let mut x = shape(n, move |t| if t < T0 { 0.0 } else { smooth((t - T0) / 0.0015) * (floor + (1.0 - floor) * hold_decay(t - T0, 0.05, 0.035)) });
            add(&mut x, &n_wave(0.0008), T0, 1.5);
            x
        }
        RadioSfx::SquelchClose => {
            let (on, off) = (T0 + 0.016, T0 + 0.14);
            let mut x = shape(n, move |t| {
                if t < T0 || t > off + 0.003 {
                    0.0
                } else if t < on {
                    lerp(floor, 1.0, smooth((t - T0) / (on - T0)))
                } else {
                    smooth((off + 0.003 - t) / 0.003)
                }
            });
            add(&mut x, &n_wave(0.0008), off, 1.2);
            x
        }
        RadioSfx::AceOpen => {
            let mut x = shape(n, move |t| if t < T0 { 0.0 } else { smooth((t - T0) / 0.0015) * (floor + (1.0 - floor) * hold_decay(t - T0, 0.04, 0.03)) * 0.7 });
            add(&mut x, &n_wave(0.0008), T0, 1.2);
            // The heterodyne: an off-tune carrier beating down into the voice band, with a slight warble.
            let het = osc(0.5f32.min(sec), |t| glide(t, 1_900.0, 650.0, 0.1) * (1.0 + 0.015 * sn(7.0, t)), &[(1.0, 1.0), (2.0, 0.15)]);
            let het = norm(shape(het, |t| smooth(t / 0.01) * smooth((0.42 - t) / 0.08)));
            add(&mut x, &het, T0 + 0.02, 0.8);
            sat(norm(x), 2.0)
        }
    };
    let y = radio_band(&x);
    let end = sec;
    norm(shape(y, move |t| smooth((end - t) / 0.015)))
}

// ---------------------------------------------------------------------------------------------------------
// Interface.

/// D Dorian pitches used by the pitched UI sounds (Hz).
const D5: f32 = 587.33;
const E5: f32 = 659.26;
const A5: f32 = 880.0;
const B5: f32 = 987.77;

/// A soft mallet note into `dst` at `at`: a 1 ms excitation into the fundamental and two quickly fading
/// upper partials.
fn mallet(dst: &mut [f32], at: f32, f: f32, tau: f32, amp: f32, seed: u64) {
    let len = (tau * 7.0).max(0.02);
    let mut x = silence(len);
    let ex = lp4(&shape(white(0.003, seed), |t| ad(t, 0.0002, 0.0005)), 3_500.0);
    add(&mut x, &ex, 0.0, 1.0);
    let y = norm(modal(&x, &[mode(f, tau, 1.0), mode(f * 2.0, tau / 2.5, 0.18), mode(f * 3.0, tau / 4.0, 0.06)]));
    add(dst, &y, at, amp);
}

/// A short dry tick into `dst` at `at`.
fn tick(dst: &mut [f32], at: f32, bright: f32, amp: f32, seed: u64) {
    tick_grain(dst, at, 0.0006, bright, amp, seed);
}

/// A short pitched body under a tick: a damped resonance excited by an impulse.
fn knock(dst: &mut [f32], at: f32, f: f32, tau: f32, amp: f32) {
    let len = (tau * 7.0).max(0.01);
    let mut x = silence(len);
    if let Some(v) = x.first_mut() {
        *v = 1.0;
    }
    let y = norm(modal(&x, &[mode(f, tau, 1.0), mode(f * 2.3, tau * 0.5, 0.3)]));
    add(dst, &y, at, amp);
}

fn ui(id: UiSfx, sec: f32, seed: u64) -> Vec<f32> {
    let mut x = silence(sec);
    let s = |k: u64| sub(seed, k);
    match id {
        UiSfx::Click => {
            tick(&mut x, T0, 4_500.0, 1.0, s(1));
            knock(&mut x, T0, 1_800.0, 0.004, 0.3);
        }
        UiSfx::Select => {
            tick(&mut x, T0, 5_500.0, 1.0, s(1));
            knock(&mut x, T0, 2_400.0, 0.008, 0.4);
        }
        UiSfx::Place => {
            let th = lp4(&shape(white(0.12, s(1)), |t| ad(t, 0.002, 0.02)), 1_200.0);
            add(&mut x, &norm(th), T0, 0.6);
            knock(&mut x, T0, 220.0, 0.03, 0.8);
            knock(&mut x, T0, 470.0, 0.015, 0.4);
            tick(&mut x, T0, 3_500.0, 0.5, s(2));
        }
        UiSfx::Remove => {
            let sw = band(&shape(white(0.1, s(1)), |t| ad(t, 0.004, 0.015)), 1_000.0, 4_000.0);
            add(&mut x, &norm(sw), T0, 0.5);
            knock(&mut x, T0 + 0.004, 520.0, 0.015, 0.7);
            tick(&mut x, T0 + 0.05, 4_000.0, 0.4, s(2));
        }
        UiSfx::Invalid => {
            let buzz = osc(0.06, |_| 150.0, &[(1.0, 1.0), (3.0, 0.45), (5.0, 0.25), (7.0, 0.12)]);
            let buzz = norm(lp4(&shape(buzz, |t| gate(t, 0.0, 0.05, 0.005)), 1_500.0));
            add(&mut x, &buzz, T0, 1.0);
            add(&mut x, &buzz, T0 + 0.085, 0.9);
        }
        UiSfx::Confirm => {
            knock(&mut x, T0, 900.0, 0.025, 0.8);
            tick(&mut x, T0, 4_000.0, 0.3, s(1));
            knock(&mut x, T0 + 0.07, 1_350.0, 0.025, 1.0);
            tick(&mut x, T0 + 0.07, 4_500.0, 0.3, s(2));
        }
        UiSfx::Rotate => {
            for k in 0..3u64 {
                tick(&mut x, T0 + 0.022 * k as f32, 5_000.0, 1.0 - 0.25 * k as f32, s(1 + k));
            }
        }
        UiSfx::Snap => {
            tick(&mut x, T0, 7_000.0, 1.0, s(1));
            clink_grain(&mut x, T0, 2_600.0, 0.012, 0.5, s(2));
        }
        UiSfx::TimeRate => {
            tick(&mut x, T0, 4_500.0, 0.9, s(1));
            knock(&mut x, T0, 1_100.0, 0.006, 0.3);
            tick(&mut x, T0 + 0.06, 4_500.0, 1.0, s(2));
            knock(&mut x, T0 + 0.06, 1_100.0, 0.006, 0.35);
        }
        UiSfx::Pause => {
            mallet(&mut x, T0, A5, 0.03, 1.0, s(1));
            mallet(&mut x, T0 + 0.09, D5, 0.03, 0.9, s(2));
        }
        UiSfx::Resume => {
            mallet(&mut x, T0, D5, 0.03, 0.9, s(1));
            mallet(&mut x, T0 + 0.09, A5, 0.03, 1.0, s(2));
        }
        UiSfx::TutorialStep => {
            mallet(&mut x, T0, E5, 0.08, 0.9, s(1));
            mallet(&mut x, T0 + 0.12, B5, 0.08, 0.8, s(2));
        }
        UiSfx::ObjectiveDone => {
            mallet(&mut x, T0, D5, 0.15, 0.9, s(1));
            mallet(&mut x, T0 + 0.11, E5, 0.15, 0.85, s(2));
            mallet(&mut x, T0 + 0.22, A5, 0.15, 1.0, s(3));
        }
        UiSfx::Saved => {
            knock(&mut x, T0, 300.0, 0.02, 0.8);
            tick(&mut x, T0, 3_500.0, 0.6, s(1));
            tick(&mut x, T0 + 0.07, 5_000.0, 0.8, s(2));
            knock(&mut x, T0 + 0.07, 600.0, 0.012, 0.4);
        }
    }
    // Ticks are first differences of noise and lean bright; keep the interface under 7 kHz (nothing shrill).
    let x = lp4(&x, 6_500.0);
    let end = sec;
    norm(shape(x, move |t| smooth((end - t) / (0.2 * end).min(0.03))))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SoundId;

    fn f(pcm: &crate::Pcm16) -> Vec<f32> {
        pcm.samples.iter().map(|&v| v as f32 / 32768.0).collect()
    }

    fn rms(x: &[f32]) -> f32 {
        libm::sqrtf(x.iter().map(|v| v * v).sum::<f32>() / x.len().max(1) as f32)
    }

    /// RMS in 10 ms windows.
    fn windows(x: &[f32]) -> Vec<f32> {
        x.chunks(frames(0.01)).map(rms).collect()
    }

    /// Onsets: a window well above everything in the 30 ms before it and above a tenth of the peak.
    fn onsets(x: &[f32]) -> usize {
        let w = windows(x);
        let pk = w.iter().fold(0.0f32, |m, v| m.max(*v));
        let mut n = 0;
        let mut last = -10i64;
        for k in 0..w.len() {
            let before = w[k.saturating_sub(3)..k].iter().fold(0.0f32, |m, v| m.max(*v));
            if w[k] > 0.1 * pk && w[k] > 1.8 * before && k as i64 - last > 3 {
                n += 1;
                last = k as i64;
            }
        }
        n
    }

    /// The strongest frequency between 300 Hz and 2.4 kHz, on a quarter-tone grid (Goertzel).
    fn dominant(x: &[f32]) -> f32 {
        let mut best = (0.0f32, 0.0f64);
        let mut k = 0;
        loop {
            let fr = 300.0 * libm::powf(2.0, k as f32 / 24.0);
            if fr > 2_400.0 {
                break;
            }
            let w = 2.0 * core::f64::consts::PI * fr as f64 / SR as f64;
            let c = 2.0 * libm::cos(w);
            let (mut s1, mut s2) = (0.0f64, 0.0f64);
            for &v in x {
                let s0 = v as f64 + c * s1 - s2;
                s2 = s1;
                s1 = s0;
            }
            let pw = s1 * s1 + s2 * s2 - c * s1 * s2;
            if pw > best.1 {
                best = (fr, pw);
            }
            k += 1;
        }
        best.0
    }

    fn check_clean(id: SoundId) -> crate::Pcm16 {
        let spec = id.spec();
        let raw = match id {
            SoundId::Alert(a) => render_alert(a, &spec, id.seed()),
            SoundId::Radio(r) => render_radio(r, &spec, id.seed()),
            SoundId::Ui(u) => render_ui(u, &spec, id.seed()),
            _ => unreachable!(),
        };
        assert_eq!(raw.len(), frames(render_seconds(&spec)), "{id:?}");
        assert!(raw.iter().all(|v| v.is_finite()), "{id:?}");
        if spec.playback == Playback::Loop {
            let p = spec_frames(&spec);
            for i in 0..raw.len() - p {
                assert!((raw[p + i] - raw[i]).abs() < 1e-6, "{id:?} overhang differs at {i}");
            }
        }
        let pcm = crate::render::finish(raw, &spec, id.seed());
        let peak = pcm.samples.iter().map(|&s| (s as i32).abs()).max().unwrap();
        // Levels are loudness targets (`analysis::target`); the peak only has to sit under the ceiling.
        let ceiling = libm::powf(10.0, spec.peak_dbfs.min(crate::render::CEILING_DBFS) / 20.0) * 32767.0;
        assert!(peak as f32 <= ceiling + 2.0 && peak as f32 > ceiling * 0.01, "{id:?} peak {peak} ceiling {ceiling}");
        let s = &pcm.samples;
        if spec.playback == Playback::Loop {
            let mean_step = s.windows(2).map(|p| (p[1] as f32 - p[0] as f32).abs()).sum::<f32>() / (s.len() - 1) as f32;
            let wrap = (s[0] as f32 - s[s.len() - 1] as f32).abs();
            assert!(wrap < mean_step * 8.0 + 64.0, "{id:?} seam {wrap} vs {mean_step}");
        } else {
            let tail = &s[s.len() * 95 / 100..];
            let tail_peak = tail.iter().map(|&v| (v as i32).abs()).max().unwrap();
            assert!(tail_peak < peak / 4 + 2, "{id:?} tail {tail_peak} peak {peak}");
        }
        pcm
    }

    #[test]
    fn alerts_differ_by_rhythm_and_pitch() {
        let mut seen: Vec<(AlertSound, usize, f32)> = Vec::new();
        for a in AlertSound::ALL {
            let pcm = check_clean(SoundId::Alert(a));
            let x = f(&pcm);
            let w = windows(&x);
            let pk = w.iter().fold(0.0f32, |m, v| m.max(*v));
            let sounding = w.iter().filter(|&&v| v > 0.3 * pk).count() as f32 / w.len() as f32;
            let rhythm = match a {
                AlertSound::Emergency => {
                    // Continuous: never a gap.
                    assert!(sounding > 0.97, "Emergency has gaps: {sounding}");
                    0
                }
                _ => onsets(&x),
            };
            seen.push((a, rhythm, dominant(&x)));
        }
        let get = |a: AlertSound| seen.iter().find(|s| s.0 == a).copied().unwrap();
        assert_eq!(get(AlertSound::Alarm).1, 3);
        assert_eq!(get(AlertSound::Warning).1, 2);
        assert_eq!(get(AlertSound::Caution).1, 1);
        for i in 0..seen.len() {
            for j in i + 1..seen.len() {
                let (a, b) = (seen[i].2, seen[j].2);
                let ratio = a.max(b) / a.min(b);
                assert!(ratio > 1.1, "{:?} {a} Hz and {:?} {b} Hz too close", seen[i].0, seen[j].0);
                assert_ne!(seen[i].1, seen[j].1, "{:?} and {:?} share a rhythm", seen[i].0, seen[j].0);
            }
        }
        // The Alarm cycle ends in a gap long enough to hear the repeat as a new group.
        let alarm = f(&check_clean(SoundId::Alert(AlertSound::Alarm)));
        let w = windows(&alarm);
        let pk = w.iter().fold(0.0f32, |m, v| m.max(*v));
        let quiet_tail = w.iter().rev().take_while(|&&v| v < 0.05 * pk).count();
        assert!(quiet_tail >= 50, "alarm gap {quiet_tail} windows");
    }

    #[test]
    fn radio_stays_in_the_voice_band() {
        for r in RadioSfx::ALL {
            let x = f(&check_clean(SoundId::Radio(r)));
            let total = rms(&x);
            let low = rms(&lp4(&lp4(&x, 150.0), 150.0));
            let high = rms(&hp2(&hp2(&x, 6_000.0), 6_000.0));
            assert!(low / total < 0.1 && high / total < 0.1, "{r:?} low {} high {}", low / total, high / total);
        }
        // Squelch open bursts at once: its loudest moment is in the first 30 ms.
        let x = f(&check_clean(SoundId::Radio(RadioSfx::SquelchOpen)));
        let w = windows(&x);
        let loudest = (0..w.len()).max_by(|&a, &b| w[a].partial_cmp(&w[b]).unwrap()).unwrap();
        assert!(loudest < 3, "squelch open peaks late, window {loudest}");
        // Squelch close ends in silence (the carrier is gone).
        let x = f(&check_clean(SoundId::Radio(RadioSfx::SquelchClose)));
        assert!(rms(&x[x.len() * 3 / 4..]) < 0.02 * rms(&x[..x.len() / 2]));
    }

    #[test]
    fn ui_sounds_are_short_and_dry() {
        for u in UiSfx::ALL {
            let pcm = check_clean(SoundId::Ui(u));
            let x = f(&pcm);
            assert!(x.len() <= frames(0.8), "{u:?} too long");
            // Energy is front-loaded: the last third holds little of it.
            let e = |s: &[f32]| s.iter().map(|v| v * v).sum::<f32>();
            assert!(e(&x[x.len() * 2 / 3..]) < 0.1 * e(&x), "{u:?} rings on");
            // Not shrill: little energy above 8 kHz.
            assert!(e(&hp2(&hp2(&x, 8_000.0), 8_000.0)) < 0.1 * e(&x), "{u:?} shrill");
        }
        // Different sounds are different files.
        let a = crate::render_wav(SoundId::Ui(UiSfx::Pause));
        let b = crate::render_wav(SoundId::Ui(UiSfx::Resume));
        assert_ne!(a, b);
        assert_eq!(a, crate::render_wav(SoundId::Ui(UiSfx::Pause)));
    }

    #[test]
    fn signals_tolerate_odd_specs() {
        let mut s = SoundId::Alert(AlertSound::Alarm).spec();
        s.playback = Playback::Loop;
        s.channels = 2;
        let x = render_alert(AlertSound::Alarm, &s, 1);
        assert_eq!(x.len(), frames(render_seconds(&s)) * 2);
        let mut s = SoundId::Alert(AlertSound::Emergency).spec();
        s.playback = Playback::OneShot;
        s.duration_s = -1.0;
        assert!(render_alert(AlertSound::Emergency, &s, 1).iter().all(|v| v.is_finite()));
        let mut s = SoundId::Radio(RadioSfx::Static).spec();
        s.duration_s = f32::INFINITY;
        assert!(render_radio(RadioSfx::Static, &s, 1).iter().all(|v| v.is_finite()));
        s.duration_s = 0.01;
        assert!(render_radio(RadioSfx::Static, &s, 1).iter().all(|v| v.is_finite()));
        let mut s = SoundId::Ui(UiSfx::Click).spec();
        s.duration_s = f32::NAN;
        assert!(render_ui(UiSfx::Click, &s, 1).iter().all(|v| v.is_finite()));
    }
}
