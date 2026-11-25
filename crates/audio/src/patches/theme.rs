//! Unit `audio-theme`. The one short menu theme (L1244), composed in code: a fixed note sequence and
//! arrangement written in this file, voiced by fundsp patches, seamless loop of `spec.duration_s` seconds,
//! stereo. Never played in sorties (L1247). Mood: dusk at sea, restrained; no generative models (L59).
//!
//! The piece: 16 bars of 4/4 at 60 bpm in D Dorian, so one bar is 4 s and the loop is exactly 64 beats (the
//! tempo scales if the spec row's length changes). Four layers, all written below:
//! - a low drone on D2 and A2, sine partials held for the whole loop with a slow breath every 16 s;
//! - a soft pad, one chord per four bars (i, IV, bVII, v: Dm add9, G, C over G, Am), band-limited fundsp saws
//!   detuned left and right, slow attack and a release that overlaps the next chord;
//! - the figure: a slow modal line on a soft additive tone with a breath of air at the onset and a late,
//!   shallow vibrato. It is a falling fourth answered by steps that lean on the Dorian major sixth (B), rises
//!   to D5 over the IV chord, rests over bVII where a distant bell strikes twice, and climbs G to A across the
//!   loop point, so the end leads into the start;
//! - a faint sea-air bed of low noise swells.
//!
//! A fundsp stereo FDN reverb (`reverb_stereo`, 25 m, 4.5 s) sits over everything. No melodic or harmonic
//! stock phrase: no fanfare intervals, no hornpipe rhythm, no I-V-vi-IV.
//!
//! Seamless loop: every layer is built as one cyclic period. Note, chord and bell events are rendered with
//! their tails and folded back onto the head; drone frequencies hold whole cycles in the period; the air bed
//! is cyclic noise under a circular envelope. The reverb runs over the period preceded by its own last 12 s,
//! so the kept period is the reverb's steady state on the looping input and its tail continues across the
//! seam.
use super::ambience::{cyclize, emit, fold, interleave, nsec, pan, ps, Bumps};
use crate::render::kit::*;
use crate::render::{prepare, render_seconds, run_unit, spec_frames};
use crate::{Playback, SoundSpec};
use fundsp::audiounit::AudioUnit;
use fundsp::buffer::BufferVec;
use fundsp::prelude32::{lowpass_hz, reverb_stereo, soft_saw_hz};
use fundsp::MAX_BUFFER_SIZE;

/// Beats in the loop (16 bars of 4/4).
const BEATS: f32 = 64.0;

/// One note of the figure: start beat, length in beats, MIDI note, velocity (0..1).
type Note = (f32, f32, u8, f32);

/// The figure, D Dorian. Bars are 4 beats; chords change at beats 0, 16, 32 and 48.
const FIGURE: [Note; 24] = [
    // i (Dm add9): a falling fourth, then steps up through the minor third to the fourth, and down to the root.
    (2.0, 2.5, 69, 0.85),  // A4
    (5.0, 1.0, 64, 0.65),  // E4
    (6.0, 1.0, 65, 0.7),   // F4
    (7.0, 3.0, 67, 0.8),   // G4
    (10.5, 1.0, 65, 0.6),  // F4
    (11.5, 1.5, 64, 0.65), // E4
    (13.0, 1.5, 60, 0.7),  // C4
    (14.5, 3.5, 62, 0.75), // D4, held into the IV chord
    // IV (G): the Dorian sixth, then up to D5.
    (18.0, 2.0, 71, 0.85), // B4
    (20.0, 1.0, 67, 0.65), // G4
    (21.0, 1.0, 69, 0.7),  // A4
    (22.0, 3.0, 74, 0.9),  // D5
    (26.0, 1.0, 72, 0.7),  // C5
    (27.0, 1.0, 71, 0.65), // B4
    (28.0, 2.0, 67, 0.7),  // G4
    (30.0, 4.0, 69, 0.75), // A4, held into bVII
    // bVII (C): low and sparse under the bell.
    (35.0, 3.0, 64, 0.55), // E4
    (38.0, 2.0, 62, 0.5),  // D4
    (41.0, 4.0, 67, 0.6),  // G4
    // v (Am): the top of the line, a step down, and the climb that leads back to the A4 at beat 2.
    (50.0, 2.0, 72, 0.8),  // C5
    (52.0, 1.0, 69, 0.65), // A4
    (53.0, 1.0, 67, 0.6),  // G4
    (54.0, 3.0, 64, 0.7),  // E4
    (58.0, 3.0, 65, 0.6),  // F4 rising to G4 below
];
/// The last note of the climb (separate so the figure table stays in bar order).
const PICKUP: Note = (61.0, 3.0, 67, 0.65); // G4 into the loop point

/// Pad chords: start beat and three MIDI notes (16 beats each).
const CHORDS: [(f32, [u8; 3]); 4] = [
    (0.0, [50, 57, 64]),  // i: D3 A3 E4
    (16.0, [55, 59, 62]), // IV: G3 B3 D4
    (32.0, [55, 60, 64]), // bVII: G3 C4 E4
    (48.0, [57, 60, 64]), // v: A3 C4 E4
];

/// Bell strikes: beat and gain.
const BELLS: [(f32, f32); 2] = [(33.0, 1.0), (43.0, 0.7)];

/// Drone notes: MIDI note and gain.
const DRONE: [(u8, f32); 2] = [(38, 1.0), (45, 0.55)];

fn hz(midi: u8) -> f32 {
    440.0 * libm::powf(2.0, (midi as f32 - 69.0) / 12.0)
}

/// Unit: `audio-theme`.
pub fn render(spec: &SoundSpec, seed: u64) -> Vec<f32> {
    let p = spec_frames(spec);
    let (l, r) = theme(p, seed);
    match spec.playback {
        Playback::Loop => emit(&[l, r], spec),
        Playback::OneShot => interleave(&[l, r], spec, frames(render_seconds(spec))),
    }
}

/// One cyclic stereo period of the theme, `p` frames per channel.
fn theme(p: usize, seed: u64) -> (Vec<f32>, Vec<f32>) {
    let per = ps(p);
    let beat = per / BEATS;
    // Event layers are written into buffers with room for their tails, then folded onto the head.
    let tail = frames(8.0 * beat + 6.0);
    let mut l = vec![0.0f32; p + tail];
    let mut r = vec![0.0f32; p + tail];
    fn put(x: &[f32], at: f32, pn: f32, g: f32, l: &mut [f32], r: &mut [f32]) {
        let (gl, gr) = pan(pn);
        add(l, x, at, g * gl);
        add(r, x, at, g * gr);
    }

    // Figure.
    for (k, n) in FIGURE.iter().chain(core::iter::once(&PICKUP)).enumerate() {
        let k = k as u64;
        let (b0, len, midi, vel) = *n;
        let at = (b0 * beat + rr(seed, 10 * k, -0.012, 0.012)).max(0.0);
        let v = vel * rr(seed, 10 * k + 1, 0.95, 1.05);
        let x = lead(hz(midi), len * beat, sub(seed, 100 + k));
        put(&x, at, 0.12, 0.55 * v, &mut l, &mut r);
    }

    // Pad: each note twice, detuned a few cents apart, one voice per side.
    for (c, (b0, notes)) in CHORDS.iter().enumerate() {
        let hold = 16.0 * beat;
        for (j, &m) in notes.iter().enumerate() {
            let s = sub(seed, 200 + 10 * c as u64 + j as u64);
            let f = hz(m);
            let vl = pad_voice(f * libm::powf(2.0, -4.0 / 1200.0), hold, beat, s);
            let vr = pad_voice(f * libm::powf(2.0, 4.0 / 1200.0), hold, beat, sub(s, 1));
            let at = b0 * beat;
            let g = 0.16 * if j == 0 { 1.0 } else { 0.8 };
            add(&mut l, &vl, at, g);
            add(&mut r, &vr, at, g);
        }
    }

    // Bell, distant and to the left.
    for (k, &(b0, g)) in BELLS.iter().enumerate() {
        let x = bell(hz(62), (10.0 * beat).max(0.2), sub(seed, 300 + k as u64));
        put(&x, b0 * beat, -0.45, 0.12 * g, &mut l, &mut r);
    }

    let mut l = fold(l, p);
    let mut r = fold(r, p);

    // Drone: whole cycles in the period, a slow breath four times per loop, a slight drift between the sides.
    let breath = snap_hz(4.0 / per.max(1e-3), per);
    for (k, &(m, g)) in DRONE.iter().enumerate() {
        let f = snap_hz(hz(m), per);
        let drift = 1.0 / per.max(1e-3);
        let parts = [(1.0, 1.0), (2.0, 0.35), (3.0, 0.12), (4.0, 0.05)];
        let ph = k as f32 * 0.25;
        let dl = osc(per, move |_| f, &parts);
        let dr = osc(per, move |_| f + drift, &parts);
        let env = move |t: f32| 0.75 + 0.25 * libm::sinf(core::f32::consts::TAU * (breath * t + ph));
        let (dl, dr) = (shape(norm(dl), env), shape(norm(dr), env));
        for i in 0..p.min(dl.len()) {
            l[i] += 0.22 * g * dl[i];
            r[i] += 0.22 * g * dr[i];
        }
    }

    // Sea air: low noise swells, independent per side.
    for (c, ch) in [&mut l, &mut r].into_iter().enumerate() {
        let sw = Bumps::new(sub(seed, 400 + c as u64), per, 6, (2.0 * beat, 3.0 * beat), (2.0 * beat, 3.0 * beat), (0.5, 1.0)).ctl();
        let n = norm(cyclize(lp4(&pinkn(nsec(p), sub(seed, 410 + c as u64)), 600.0), p));
        for (i, v) in ch.iter_mut().enumerate().take(n.len()) {
            *v += 0.05 * n[i] * (0.3 + sw.at(i as f32 / SR));
        }
    }

    // Reverb over the looping input, warmed with the period's own end.
    let (wl, wr) = reverb(&l, &r, frames(12.0).min(p), seed);
    let l: Vec<f32> = l.iter().zip(wl.iter()).map(|(d, w)| 0.8 * d + 0.5 * w).collect();
    let r: Vec<f32> = r.iter().zip(wr.iter()).map(|(d, w)| 0.8 * d + 0.5 * w).collect();
    (l, r)
}

/// The figure's voice: a soft additive tone (strong fundamental, weak second and third partials, faint
/// fourth and fifth that fade after the onset), a short breath of band noise at the attack, a gentle
/// sustain decay, a late shallow vibrato and a long release. Unit peak.
fn lead(f: f32, dur: f32, seed: u64) -> Vec<f32> {
    let release = 1.4;
    let len = dur + release * 2.2;
    let vib = 4.8;
    let pitch = move |t: f32| f * (1.0 + 0.0025 * smooth((t - 0.5) / 0.8) * libm::sinf(core::f32::consts::TAU * vib * t));
    let amp = move |t: f32| {
        let a = smooth(t / 0.12) * libm::expf(-t / 7.0);
        if t < dur {
            a
        } else {
            a * libm::expf(-(t - dur) / (release * 0.45))
        }
    };
    let body = shape(osc(len, pitch, &[(1.0, 1.0), (2.0, 0.28), (3.0, 0.1)]), amp);
    let bright = shape(osc(len, pitch, &[(4.0, 0.05), (5.0, 0.03)]), move |t| amp(t) * (0.3 + 0.7 * libm::expf(-t / 0.4)));
    let breath = band(&white(0.4, sub(seed, 1)), f, (f * 4.0).min(8_000.0));
    let breath = shape(breath, |t| ad(t, 0.03, 0.06));
    let mut x = body;
    add(&mut x, &bright, 0.0, 1.0);
    add(&mut x, &norm(breath), 0.0, 0.05);
    let end = len;
    norm(shape(x, move |t| smooth((end - t) / 0.3)))
}

/// One pad voice: a band-limited fundsp saw, low-passed, with a slow attack over three beats, held for `hold`
/// seconds and released over four beats. Unit peak.
fn pad_voice(f: f32, hold: f32, beat: f32, seed: u64) -> Vec<f32> {
    let rel = 4.0 * beat;
    let len = hold + rel;
    let mut u = soft_saw_hz(f) >> lowpass_hz(1_100.0, 0.6);
    let x = run_unit(&mut u, len as f64, 1, seed);
    let att = 3.0 * beat;
    norm(shape(x, move |t| smooth(t / att.max(1e-3)) * smooth((len - t) / rel.max(1e-3))))
}

/// A distant bell at `f` (its prime partial): a soft mallet into bell partials (hum an octave down, minor
/// third tierce, fifth, nominal), darkened by distance. Unit peak.
fn bell(f: f32, sec: f32, seed: u64) -> Vec<f32> {
    let mut x = silence(sec);
    let ex = lp4(&shape(white(0.01, seed), |t| ad(t, 0.0005, 0.0015)), 2_000.0);
    add(&mut x, &ex, 0.0, 1.0);
    let modes = [
        mode(f * 0.5, 4.0, 0.5),
        mode(f, 3.0, 1.0),
        mode(f * 1.2, 2.2, 0.6),
        mode(f * 1.5, 1.6, 0.35),
        mode(f * 2.0, 1.2, 0.4),
        mode(f * 2.5, 0.8, 0.15),
        mode(f * 3.0, 0.6, 0.1),
    ];
    let y = lp4(&modal(&x, &modes), 3_000.0);
    let end = sec;
    norm(shape(y, move |t| smooth((end - t) / (0.3 * end))))
}

/// fundsp's stereo FDN reverb over a cyclic stereo period: the period's last `warm` frames are played in
/// first, then the period, and the wet output over the period is returned.
fn reverb(l: &[f32], r: &[f32], warm: usize, seed: u64) -> (Vec<f32>, Vec<f32>) {
    let p = l.len().min(r.len());
    if p == 0 {
        return (Vec::new(), Vec::new());
    }
    let w = warm.min(p);
    let src = |x: &[f32], i: usize| if i < w { x[p - w + i] } else { x[i - w] };
    // Darken what feeds the room, so the tail sits low.
    let mut unit = (lowpass_hz(4_000.0, 0.6) | lowpass_hz(4_000.0, 0.6)) >> reverb_stereo(25.0, 4.5, 0.6);
    prepare(&mut unit, seed);
    let n = p + w;
    let mut inb = BufferVec::new(2);
    let mut outb = BufferVec::new(2);
    let (mut ol, mut or) = (Vec::with_capacity(p), Vec::with_capacity(p));
    let mut start = 0;
    while start < n {
        let len = (n - start).min(MAX_BUFFER_SIZE);
        for i in 0..len {
            inb.set_f32(0, i, src(l, start + i));
            inb.set_f32(1, i, src(r, start + i));
        }
        unit.process(len, &inb.buffer_ref(), &mut outb.buffer_mut());
        for i in 0..len {
            if start + i >= w {
                ol.push(outb.at_f32(0, i));
                or.push(outb.at_f32(1, i));
            }
        }
        start += len;
    }
    (ol, or)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SoundId;

    #[test]
    fn score_is_in_d_dorian_and_in_range() {
        // D Dorian pitch classes: D E F G A B C.
        let dorian = [2u8, 4, 5, 7, 9, 11, 0];
        for n in FIGURE.iter().chain(core::iter::once(&PICKUP)) {
            assert!(dorian.contains(&(n.2 % 12)), "{n:?}");
            assert!(n.0 >= 0.0 && n.0 + n.1 <= BEATS + 3.0, "{n:?}");
            assert!((60..=76).contains(&n.2), "{n:?}");
        }
        for (_, c) in CHORDS {
            assert!(c.iter().all(|m| dorian.contains(&(m % 12))));
        }
        // Notes of the figure never overlap (a single line).
        let mut v: Vec<Note> = FIGURE.to_vec();
        v.push(PICKUP);
        for w in v.windows(2) {
            assert!(w[0].0 + w[0].1 <= w[1].0 + 1e-3, "{:?} overlaps {:?}", w[0], w[1]);
        }
    }

    #[test]
    fn theme_loops_seamlessly_and_is_deterministic() {
        let id = SoundId::MenuTheme;
        let spec = id.spec();
        let raw = render(&spec, id.seed());
        let p = spec_frames(&spec);
        assert_eq!(raw.len(), frames(render_seconds(&spec)) * 2);
        assert!(raw.iter().all(|v| v.is_finite()));
        // The overhang repeats the head, so `finish` passes the loop through unchanged.
        for i in 0..raw.len() - 2 * p {
            assert!((raw[2 * p + i] - raw[i]).abs() < 1e-6, "overhang differs at {i}");
        }
        let pcm = crate::render::finish(raw, &spec, id.seed());
        assert_eq!(pcm.frames(), p);
        assert_eq!(pcm.channels, 2);
        assert_eq!(pcm.loop_frames, Some((0, p as u32)));
        let peak = pcm.samples.iter().map(|&s| (s as i32).abs()).max().unwrap();
        // Levels are loudness targets (`analysis::target`); the peak only has to sit under the ceiling.
        let ceiling = libm::powf(10.0, spec.peak_dbfs.min(crate::render::CEILING_DBFS) / 20.0) * 32767.0;
        assert!(peak as f32 <= ceiling + 2.0 && peak as f32 > ceiling * 0.01);
        for c in 0..2 {
            let s: Vec<f32> = pcm.samples.iter().skip(c).step_by(2).map(|&v| v as f32).collect();
            let mean_step = s.windows(2).map(|w| (w[1] - w[0]).abs()).sum::<f32>() / (s.len() - 1) as f32;
            let wrap = (s[0] - s[s.len() - 1]).abs();
            assert!(wrap < mean_step * 6.0 + 16.0, "ch {c} seam {wrap} vs {mean_step}");
            // Never silent and never a wall: every 4 s bar sounds, within a 20 dB range.
            let bar = s.len() / 16;
            let levels: Vec<f32> = (0..16).map(|k| libm::sqrtf(s[k * bar..(k + 1) * bar].iter().map(|v| v * v).sum::<f32>() / bar as f32)).collect();
            let (lo, hi) = levels.iter().fold((f32::MAX, 0.0f32), |(a, b), &v| (a.min(v), b.max(v)));
            assert!(lo > 0.0 && hi / lo < 10.0, "bar levels {levels:?}");
            // Restrained: nothing much above 8 kHz.
            let x: Vec<f32> = s.iter().map(|v| v / 32768.0).collect();
            let hi8 = hp2(&hp2(&x, 8_000.0), 8_000.0);
            let e = |y: &[f32]| y.iter().map(|v| v * v).sum::<f32>();
            assert!(e(&hi8) < 1e-3 * e(&x), "bright");
        }
        // Wide: the sides differ.
        let (l, r): (Vec<f32>, Vec<f32>) = pcm.samples.chunks(2).map(|f| (f[0] as f32, f[1] as f32)).unzip();
        assert!(l != r);
    }

    #[test]
    fn theme_is_deterministic_and_scales_with_the_spec() {
        // A short spec row runs every layer and the reverb at a quicker tempo; two renders match byte for
        // byte (the full-length render costs several seconds, so the short one stands in for it).
        let mut spec = SoundId::MenuTheme.spec();
        spec.duration_s = 4.0;
        let a = render(&spec, 11);
        let b = render(&spec, 11);
        assert_eq!(a, b);
        assert_eq!(a.len(), frames(render_seconds(&spec)) * 2);
        assert!(a.iter().any(|v| v.abs() > 1e-4));
        assert_ne!(a, render(&spec, 12));
        // Degenerate rows do not panic.
        spec.duration_s = f32::NAN;
        assert!(render(&spec, 1).iter().all(|v| v.is_finite()));
        spec.duration_s = 0.5;
        spec.playback = Playback::OneShot;
        spec.channels = 1;
        assert_eq!(render(&spec, 1).len(), frames(0.5));
    }
}
