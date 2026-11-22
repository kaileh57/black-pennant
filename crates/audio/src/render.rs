//! Unit `audio-render`. The fundsp seam, post-processing and WAV encoding. DESIGN.md L1246 (offline, 48 kHz,
//! 16-bit WAV), L1078 (deterministic: CI renders twice and compares bytes).
//!
//! fundsp 0.23.0 seam (checked against the crate source): patches import `fundsp::prelude32::*` (f32 graph
//! nodes: `sine_hz`, `saw_hz`, `square_hz`, `noise()`, `pink::<f32>()`, `brown::<f32>()`, `lowpass_hz`,
//! `highpass_hz`, `bandpass_hz`, `resonator_hz`, `envelope`, `lfo`, `reverb_stereo`, ...), build an `An<...>`
//! graph or a `Net`, and hand it to [`run_unit`] as `&mut dyn AudioUnit`. `AudioUnit::set_sample_rate`,
//! `reset`, `set_hash` (seed for noise determinism), `get_mono` / `tick` are the methods used. Offline
//! rendering may also use `fundsp::wave::Wave::render(sample_rate, duration, &mut unit)`.
//!
//! Seeding: fundsp calls `set_hash` on leaf nodes only through `AudioUnit::ping`; the combinators (`Pipe`,
//! `Stack`, `Bus`, `Net`) recurse through `ping` and ignore a direct `set_hash`. [`run_unit`] therefore seeds
//! the whole graph with `ping(false, AttoHash::new(seed))`, which calls `set_hash` on every leaf with a hash
//! that also depends on the leaf's position, so two `noise()` nodes in one graph still differ.
//!
//! WAV writing: RIFF/WAVE, PCM format 1, 16-bit little-endian, plus a `smpl` chunk with one forward loop for
//! `Playback::Loop` sounds so Godot's WAV importer detects loop points. Written by hand (hound cannot write
//! `smpl`); hound may be used in tests to read files back.
//!
//! Loops: a loop patch renders [`render_seconds`] (`duration_s` plus [`LOOP_XFADE_S`]). [`finish`] crossfades
//! the overhang into the head of the file, so the sample after the last loop frame continues exactly where
//! the overhang began. Tonal loop content should hold a whole number of cycles in `duration_s`
//! (`kit::snap_hz`); the crossfade is correlation-compensated, so identical (periodic) material passes
//! through unchanged and uncorrelated noise keeps its power.
//!
//! `kit` (crate-private) holds the shared synthesis toolkit: sample-accurate envelopes, layer mixing, fundsp
//! filters over buffers, grain scheduling, echo tails and the physical building blocks weapons and impacts
//! share.
use crate::{Pcm16, Playback, SoundSpec, SAMPLE_RATE};
use fundsp::audiounit::AudioUnit;
use fundsp::buffer::BufferVec;
use fundsp::math::AttoHash;
use fundsp::MAX_BUFFER_SIZE;

pub(crate) mod kit;

/// Extra seconds a loop patch renders past `spec.duration_s`; [`finish`] crossfades them into the loop head.
pub const LOOP_XFADE_S: f32 = 0.25;
/// One-shot fade-in, seconds.
const FADE_IN_S: f32 = 0.002;
/// One-shot fade-out: a cosine fade over this fraction of the file, between the two bounds (seconds), so a
/// tail still ringing at the end of the file dies away instead of stopping.
const FADE_OUT_FRAC: f32 = 0.12;
const FADE_OUT_MIN_S: f32 = 0.010;
const FADE_OUT_MAX_S: f32 = 0.25;
/// Highest sample peak of any file, dBFS: the 16-bit encoding and the bus mixing keep 1 dB of headroom.
pub const CEILING_DBFS: f32 = -1.0;
/// Peak limiter for one-shots raised to their loudness target: lookahead and release, seconds.
const LIMIT_LOOKAHEAD_S: f32 = 0.0015;
const LIMIT_RELEASE_S: f32 = 0.08;
/// DC-blocking high-pass corner for one-shots, Hz. Well under the lowest sub thump (about 30 Hz).
const DC_CORNER_HZ: f64 = 12.0;

/// Seconds a patch should render for `spec`: loops get the crossfade overhang.
pub fn render_seconds(spec: &SoundSpec) -> f32 {
    let d = if spec.duration_s.is_finite() { spec.duration_s.max(0.0) } else { 0.0 };
    match spec.playback {
        Playback::Loop => d + LOOP_XFADE_S,
        Playback::OneShot => d,
    }
}

/// Frames in `spec.duration_s` at the output rate (at least 1).
pub fn spec_frames(spec: &SoundSpec) -> usize {
    seconds_to_frames(spec.duration_s).max(1)
}

pub(crate) fn seconds_to_frames(seconds: f32) -> usize {
    if !seconds.is_finite() || seconds <= 0.0 {
        return 0;
    }
    // Round to the nearest frame; f64 keeps 64 s exact.
    (seconds as f64 * SAMPLE_RATE as f64 + 0.5) as usize
}

/// Run a fundsp unit for `seconds` at 48 kHz and return interleaved f32 frames with `channels` channels
/// (mono units are duplicated to stereo when `channels == 2`; stereo units are downmixed for mono). Calls
/// `set_sample_rate`, `set_hash(seed)` and `reset` first. Unit: `audio-render`.
///
/// Units with inputs are fed silence. Units with more than two outputs use their first two. A unit with no
/// outputs renders silence. `channels` other than 2 are treated as 1.
pub fn run_unit(unit: &mut dyn fundsp::audiounit::AudioUnit, seconds: f64, channels: u16, seed: u64) -> Vec<f32> {
    let channels = if channels == 2 { 2usize } else { 1usize };
    let frames = if seconds.is_finite() && seconds > 0.0 { (seconds * SAMPLE_RATE as f64 + 0.5) as usize } else { 0 };
    let mut out = Vec::with_capacity(frames * channels);
    prepare(unit, seed);
    let outputs = unit.outputs();
    if outputs == 0 {
        out.resize(frames * channels, 0.0);
        return out;
    }
    let input = BufferVec::new(unit.inputs());
    let mut output = BufferVec::new(outputs);
    let mut done = 0;
    while done < frames {
        let n = (frames - done).min(MAX_BUFFER_SIZE);
        unit.process(n, &input.buffer_ref(), &mut output.buffer_mut());
        for i in 0..n {
            let l = output.at_f32(0, i);
            let r = if outputs > 1 { output.at_f32(1, i) } else { l };
            if channels == 2 {
                out.push(l);
                out.push(r);
            } else if outputs > 1 {
                out.push((l + r) * 0.5);
            } else {
                out.push(l);
            }
        }
        done += n;
    }
    out
}

/// Seed and reset a unit: sample rate, deterministic hash through `ping`, then reset.
pub(crate) fn prepare(unit: &mut dyn AudioUnit, seed: u64) {
    unit.set_sample_rate(SAMPLE_RATE as f64);
    unit.ping(false, AttoHash::new(seed));
    unit.allocate();
    unit.reset();
}

/// Run a unit over a mono buffer (a filter chain): the buffer feeds input 0, further inputs get silence, and
/// the first output (or the mean of the first two) is returned. A unit without inputs ignores `input` and
/// renders `input.len()` frames.
pub(crate) fn process_mono(input: &[f32], unit: &mut dyn AudioUnit, seed: u64) -> Vec<f32> {
    prepare(unit, seed);
    let ins = unit.inputs();
    let outs = unit.outputs();
    let mut out = Vec::with_capacity(input.len());
    if outs == 0 {
        out.resize(input.len(), 0.0);
        return out;
    }
    let mut inb = BufferVec::new(ins);
    let mut outb = BufferVec::new(outs);
    for chunk in input.chunks(MAX_BUFFER_SIZE) {
        if ins > 0 {
            for (i, &x) in chunk.iter().enumerate() {
                inb.set_f32(0, i, if x.is_finite() { x } else { 0.0 });
            }
        }
        unit.process(chunk.len(), &inb.buffer_ref(), &mut outb.buffer_mut());
        for i in 0..chunk.len() {
            let v = if outs > 1 { (outb.at_f32(0, i) + outb.at_f32(1, i)) * 0.5 } else { outb.at_f32(0, i) };
            out.push(v);
        }
    }
    out
}

/// Finish a raw render (interleaved f32 at 48 kHz, `spec.channels` channels, about `spec.duration_s` long):
/// trim or pad to exactly `duration_s`; remove DC; for loops crossfade the tail into the head so the loop is
/// seamless and set `loop_frames` to the whole file; for one-shots apply a 2 ms fade-in and a fade-out over the
/// last 12% (10 to 250 ms); set the level ([`level`]); quantise to i16 with TPDF dither drawn from a counter hash of
/// (seed, sample index). Unit: `audio-render`.
///
/// Loops: overhang past `duration_s` (up to [`LOOP_XFADE_S`], at most half the loop) is crossfaded into the
/// head. A loop rendered without overhang crossfades its own last quarter (up to [`LOOP_XFADE_S`]) into its
/// head instead; the loop then covers the shortened body, the file is filled to `duration_s` by continuing
/// the loop, and `loop_frames` is `(0, body)`. Non-finite samples become silence; a silent render stays
/// silent.
pub fn finish(raw: Vec<f32>, spec: &SoundSpec, seed: u64) -> Pcm16 {
    let ch = if spec.channels == 2 { 2usize } else { 1usize };
    let n = spec_frames(spec);
    let mut raw = raw;
    // A ragged tail (not a whole frame) is dropped.
    raw.truncate((raw.len() / ch) * ch);
    for x in raw.iter_mut() {
        if !x.is_finite() {
            *x = 0.0;
        }
    }
    let raw_frames = raw.len() / ch;

    let (mut buf, loop_frames) = match spec.playback {
        Playback::OneShot => {
            let mut b = raw;
            b.resize(n * ch, 0.0);
            dc_block(&mut b, ch);
            fade_one_shot(&mut b, ch);
            (b, None)
        }
        Playback::Loop => {
            let max_x = seconds_to_frames(LOOP_XFADE_S).min(n / 2);
            let (mut b, body) = if raw_frames > n && max_x > 0 {
                let x = (raw_frames - n).min(max_x);
                let mut b = raw;
                crossfade_into_head(&mut b, ch, n, x);
                b.truncate(n * ch);
                (b, n)
            } else if raw_frames >= 8 {
                // No overhang: seam the render onto itself.
                let x = (raw_frames / 4).min(seconds_to_frames(LOOP_XFADE_S)).max(1);
                let body = raw_frames - x;
                let mut b = raw;
                crossfade_into_head(&mut b, ch, body, x);
                b.truncate(body * ch);
                // Continue the loop to fill the file.
                let mut full = Vec::with_capacity(n * ch);
                while full.len() < n * ch {
                    let take = (n * ch - full.len()).min(b.len());
                    full.extend_from_slice(&b[..take]);
                }
                (full, body.min(n))
            } else {
                (vec![0.0; n * ch], n)
            };
            remove_mean(&mut b, ch, body);
            (b, Some((0u32, body as u32)))
        }
    };

    level(&mut buf, ch, spec);
    let samples = quantise(&buf, seed);
    Pcm16 { sample_rate: SAMPLE_RATE, channels: ch as u16, samples, loop_frames }
}

/// Crossfade frames `[n, n + x)` (the overhang) into `[0, x)`, correlation-compensated: the gains satisfy
/// `a^2 + b^2 + 2 rho a b = 1` for the measured correlation `rho` of the two segments, so periodic material
/// (rho near 1) is untouched and uncorrelated noise keeps constant power.
fn crossfade_into_head(b: &mut [f32], ch: usize, n: usize, x: usize) {
    for c in 0..ch {
        let mut sab = 0.0f64;
        let mut saa = 0.0f64;
        let mut sbb = 0.0f64;
        for i in 0..x {
            let a = b[i * ch + c] as f64;
            let o = b[(n + i) * ch + c] as f64;
            sab += a * o;
            saa += a * a;
            sbb += o * o;
        }
        let rho = if saa > 0.0 && sbb > 0.0 { (sab / libm::sqrt(saa * sbb)).clamp(0.0, 1.0) } else { 0.0 };
        for i in 0..x {
            // s rises 0 -> 1 across the fade (head weight); the overhang weight is 1 - s.
            let s = (i as f64 + 0.5) / x as f64;
            let o = 1.0 - s;
            let k = 1.0 / libm::sqrt((s * s + o * o + 2.0 * rho * s * o).max(1e-9));
            let head = b[i * ch + c] as f64;
            let over = b[(n + i) * ch + c] as f64;
            b[i * ch + c] = ((head * s + over * o) * k) as f32;
        }
    }
}

/// First-order DC-blocking high-pass, per channel, in f64.
fn dc_block(b: &mut [f32], ch: usize) {
    let r = libm::exp(-2.0 * core::f64::consts::PI * DC_CORNER_HZ / SAMPLE_RATE as f64);
    let frames = b.len() / ch;
    for c in 0..ch {
        let mut x1 = 0.0f64;
        let mut y1 = 0.0f64;
        for i in 0..frames {
            let x = b[i * ch + c] as f64;
            let y = x - x1 + r * y1;
            x1 = x;
            y1 = y;
            b[i * ch + c] = y as f32;
        }
    }
}

/// Subtract each channel's mean over the loop body (a constant shift keeps the seam intact).
fn remove_mean(b: &mut [f32], ch: usize, body: usize) {
    let all = b.len() / ch;
    let frames = all.min(body);
    if frames == 0 {
        return;
    }
    for c in 0..ch {
        let mut s = 0.0f64;
        for i in 0..frames {
            s += b[i * ch + c] as f64;
        }
        let m = (s / frames as f64) as f32;
        for i in 0..all {
            b[i * ch + c] -= m;
        }
    }
}

fn fade_one_shot(b: &mut [f32], ch: usize) {
    let frames = b.len() / ch;
    let fi = seconds_to_frames(FADE_IN_S).min(frames / 2).max(1);
    let fo_s = (FADE_OUT_FRAC * frames as f32 / SAMPLE_RATE as f32).clamp(FADE_OUT_MIN_S, FADE_OUT_MAX_S);
    let fo = seconds_to_frames(fo_s).min(frames / 2).max(1);
    for i in 0..fi.min(frames) {
        let g = i as f32 / fi as f32;
        for c in 0..ch {
            b[i * ch + c] *= g;
        }
    }
    for k in 0..fo.min(frames) {
        let i = frames - 1 - k;
        // Raised cosine, 0 at the last frame.
        let g = 0.5 - 0.5 * libm::cosf(core::f32::consts::PI * k as f32 / fo as f32);
        for c in 0..ch {
            b[i * ch + c] *= g;
        }
    }
}

/// Set a sound's level: gain to the middle of its loudness target (`analysis::target`, judged as
/// `analysis::judged_lufs`), with the sample peak held at or under `spec.peak_dbfs` and [`CEILING_DBFS`]. A
/// one-shot whose peak would pass the ceiling goes through a lookahead peak limiter (1.5 ms lookahead, 80 ms
/// release), as a gun report would through a recording chain; a loop is scaled down instead, since a limiter's
/// gain would not seam. A silent render stays silent. Unit: `audio-levels`.
pub fn level(b: &mut [f32], ch: usize, spec: &SoundSpec) {
    let ceiling_db = if spec.peak_dbfs.is_finite() { spec.peak_dbfs.min(CEILING_DBFS) } else { CEILING_DBFS };
    normalise(b, ceiling_db);
    let peak = b.iter().fold(0.0f32, |m, &x| m.max(x.abs()));
    if !(peak > 1e-9) {
        return;
    }
    let (lo, hi) = crate::analysis::target(spec.id);
    let aim = 0.5 * (lo + hi);
    let ceiling = libm::powf(10.0, ceiling_db / 20.0);
    let src = b.to_vec();
    let mut gain_db = aim - crate::analysis::judged_lufs(&src, ch, spec.playback);
    // Limiting lowers loudness a little; correct the gain against the limited result a few times.
    for _ in 0..4 {
        let g = libm::powf(10.0, gain_db / 20.0);
        for (o, &x) in b.iter_mut().zip(src.iter()) {
            *o = x * g;
        }
        match spec.playback {
            Playback::OneShot => limit(b, ch, ceiling),
            Playback::Loop => normalise_down(b, ceiling),
        }
        let err = aim - crate::analysis::judged_lufs(b, ch, spec.playback);
        if err.abs() < 0.1 || spec.playback == Playback::Loop {
            break;
        }
        gain_db += err;
    }
    normalise_down(b, ceiling);
}

/// Scale down so the peak is at most `ceiling` (linear).
fn normalise_down(b: &mut [f32], ceiling: f32) {
    let peak = b.iter().fold(0.0f32, |m, &x| m.max(x.abs()));
    if peak > ceiling {
        let g = ceiling / peak;
        for x in b.iter_mut() {
            *x *= g;
        }
    }
}

/// Lookahead peak limiter to `ceiling` (linear), all channels linked: the gain is the running minimum of the
/// required gain over the lookahead, averaged over the lookahead (so it never exceeds the required gain at any
/// sample and has no step), then released with a one-pole.
fn limit(b: &mut [f32], ch: usize, ceiling: f32) {
    let frames = b.len() / ch;
    if frames == 0 {
        return;
    }
    let need: Vec<f64> = (0..frames)
        .map(|i| {
            let p = (0..ch).fold(0.0f32, |m, c| m.max(b[i * ch + c].abs())) as f64;
            if p > ceiling as f64 { ceiling as f64 / p } else { 1.0 }
        })
        .collect();
    if need.iter().all(|&g| g >= 1.0) {
        return;
    }
    let la = seconds_to_frames(LIMIT_LOOKAHEAD_S).max(1);
    // Forward running minimum over [i, i + la].
    let mut gmin = vec![1.0f64; frames];
    for i in 0..frames {
        let mut m = 1.0f64;
        for &g in &need[i..(i + la + 1).min(frames)] {
            m = m.min(g);
        }
        gmin[i] = m;
    }
    // Box average over [i - la, i]: each term includes need[i], so the average never exceeds it.
    let rel = libm::exp(-1.0 / (LIMIT_RELEASE_S as f64 * SAMPLE_RATE as f64));
    let mut acc = 0.0f64;
    let mut env = 1.0f64;
    for i in 0..frames {
        acc += gmin[i];
        if i > la {
            acc -= gmin[i - la - 1];
        }
        let n = (i.min(la) + 1) as f64;
        // Frames before the start count as unity gain.
        let avg = (acc + (la + 1) as f64 - n) / (la + 1) as f64;
        let avg = avg.min(need[i]);
        env = if avg < env { avg } else { avg.min(1.0 - (1.0 - env) * rel) };
        for c in 0..ch {
            b[i * ch + c] = (b[i * ch + c] as f64 * env) as f32;
        }
    }
}

fn normalise(b: &mut [f32], peak_dbfs: f32) {
    let peak = b.iter().fold(0.0f32, |m, &x| m.max(x.abs()));
    if !(peak > 1e-9) {
        b.fill(0.0);
        return;
    }
    let db = if peak_dbfs.is_finite() { peak_dbfs.min(0.0) } else { -1.0 };
    let g = libm::powf(10.0, db / 20.0) / peak;
    for x in b.iter_mut() {
        *x *= g;
    }
}

/// Quantise to i16 with TPDF dither of +-1 LSB from the counter hash. Exact digital silence stays silent, so
/// fade ends and gaps carry no hiss.
fn quantise(b: &[f32], seed: u64) -> Vec<i16> {
    let dseed = seed ^ 0xD17E_D17E_D17E_D17E;
    b.iter()
        .enumerate()
        .map(|(i, &x)| {
            if x == 0.0 {
                return 0;
            }
            let i = i as u64;
            let d = hash_unit(dseed, 2 * i) - hash_unit(dseed, 2 * i + 1);
            libm::floorf(x * 32767.0 + d + 0.5).clamp(-32768.0, 32767.0) as i16
        })
        .collect()
}

/// Encode as a 16-bit PCM WAV (with a `smpl` loop chunk when `loop_frames` is set). Byte-stable. Unit: `audio-render`.
///
/// The `smpl` loop end is written as the exclusive end frame from `loop_frames` (clamped to the data), the
/// convention Godot's importer uses for `AudioStreamWAV.loop_end`. The data chunk always holds whole frames.
pub fn encode_wav(pcm: &Pcm16) -> Vec<u8> {
    let ch = pcm.channels.max(1);
    let frames = pcm.samples.len() / ch as usize;
    let data_len = (frames * ch as usize * 2) as u32;
    let block_align = ch.wrapping_mul(2);
    let byte_rate = pcm.sample_rate.wrapping_mul(block_align as u32);
    let smpl_len: u32 = if pcm.loop_frames.is_some() { 36 + 24 } else { 0 };
    let riff_len = 4 + (8 + 16) + if smpl_len > 0 { 8 + smpl_len } else { 0 } + 8 + data_len;

    let mut w = Vec::with_capacity(riff_len as usize + 8);
    let put = |w: &mut Vec<u8>, v: u32| w.extend_from_slice(&v.to_le_bytes());
    w.extend_from_slice(b"RIFF");
    put(&mut w, riff_len);
    w.extend_from_slice(b"WAVE");

    w.extend_from_slice(b"fmt ");
    put(&mut w, 16);
    w.extend_from_slice(&1u16.to_le_bytes());
    w.extend_from_slice(&ch.to_le_bytes());
    put(&mut w, pcm.sample_rate);
    put(&mut w, byte_rate);
    w.extend_from_slice(&block_align.to_le_bytes());
    w.extend_from_slice(&16u16.to_le_bytes());

    if let Some((start, end)) = pcm.loop_frames {
        let end = end.min(frames as u32);
        let start = start.min(end);
        let period_ns = if pcm.sample_rate > 0 { 1_000_000_000u32 / pcm.sample_rate } else { 0 };
        w.extend_from_slice(b"smpl");
        put(&mut w, smpl_len);
        // Manufacturer, product, sample period, MIDI unity note, pitch fraction, SMPTE format, SMPTE offset,
        // number of loops, sampler data length.
        for v in [0u32, 0, period_ns, 60, 0, 0, 0, 1, 0] {
            put(&mut w, v);
        }
        // Cue point id, type 0 (forward), start, end, fraction, play count 0 (infinite).
        for v in [0u32, 0, start, end, 0, 0] {
            put(&mut w, v);
        }
    }

    w.extend_from_slice(b"data");
    put(&mut w, data_len);
    for s in &pcm.samples[..frames * ch as usize] {
        w.extend_from_slice(&s.to_le_bytes());
    }
    w
}

/// Counter-based uniform in [0, 1) for patch code that needs randomness outside fundsp (e.g. scheduling
/// grains); keyed on (seed, index). Unit: `audio-render`.
pub fn hash_unit(seed: u64, index: u64) -> f32 {
    // SplitMix64 finaliser over the combined key; the 24 high bits give an exact f32 in [0, 1).
    let mut z = seed ^ index.wrapping_mul(0x9E37_79B9_7F4A_7C15).wrapping_add(0x6A09_E667_F3BC_C909);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^= z >> 31;
    (z >> 40) as f32 / (1u64 << 24) as f32
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Bus, SoundId, WeaponSfx};
    use fundsp::prelude32::*;

    fn spec(playback: Playback, channels: u16, duration_s: f32, peak_dbfs: f32) -> SoundSpec {
        SoundSpec { id: SoundId::Weapon(WeaponSfx::GunAuto), stem: "t", playback, channels, duration_s, bus: Bus::Sfx, positional: true, peak_dbfs }
    }

    #[test]
    fn hash_unit_range_and_spread() {
        let mut sum = 0.0f64;
        for i in 0..10_000u64 {
            let u = hash_unit(7, i);
            assert!((0.0..1.0).contains(&u));
            sum += u as f64;
        }
        assert!((sum / 10_000.0 - 0.5).abs() < 0.02);
        assert_ne!(hash_unit(1, 0), hash_unit(2, 0));
        assert_eq!(hash_unit(3, 9), hash_unit(3, 9));
    }

    #[test]
    fn run_unit_seeds_noise() {
        let a = run_unit(&mut (noise() >> lowpass_hz(2000.0, 0.7)), 0.05, 1, 1);
        let b = run_unit(&mut (noise() >> lowpass_hz(2000.0, 0.7)), 0.05, 1, 1);
        let c = run_unit(&mut (noise() >> lowpass_hz(2000.0, 0.7)), 0.05, 1, 2);
        assert_eq!(a.len(), 2400);
        assert_eq!(a, b);
        assert_ne!(a, c);
        // Two noise leaves in one graph are not identical.
        let s = run_unit(&mut (noise() | noise()), 0.01, 2, 5);
        assert!(s.chunks(2).any(|f| f[0] != f[1]));
        // Mono to stereo duplicates; stereo to mono averages.
        let st = run_unit(&mut dc(0.5), 0.001, 2, 0);
        assert!(st.iter().all(|&x| x == 0.5));
        let mo = run_unit(&mut dc((0.2, 0.6)), 0.001, 1, 0);
        assert!(mo.iter().all(|&x| (x - 0.4).abs() < 1e-6));
    }

    #[test]
    fn one_shot_finish_lengths_fades_and_peak() {
        let sp = spec(Playback::OneShot, 1, 0.5, -6.0);
        let raw = vec![0.3f32; 10_000];
        let pcm = finish(raw, &sp, 1);
        assert_eq!(pcm.frames(), 24_000);
        assert_eq!(pcm.samples[0], 0);
        assert_eq!(*pcm.samples.last().unwrap(), 0);
        assert!(pcm.loop_frames.is_none());
        let peak = pcm.samples.iter().map(|&s| (s as i32).abs()).max().unwrap();
        let target = (libm::powf(10.0, -6.0 / 20.0) * 32767.0) as i32;
        assert!((peak - target).abs() <= 2, "peak {peak} target {target}");
        // Non-finite and silent input does not panic and stays silent.
        let pcm = finish(vec![f32::NAN, f32::INFINITY, 0.0], &sp, 1);
        assert!(pcm.samples.iter().all(|&s| s == 0));
    }

    #[test]
    fn loop_seam_is_continuous() {
        let sp = spec(Playback::Loop, 1, 1.0, -3.0);
        // Noise plus a tone that is not periodic in 1 s, to stress the crossfade.
        let raw = run_unit(&mut ((noise() >> lowpass_hz(800.0, 0.7)) * 0.5 + sine_hz(97.3) * 0.5), render_seconds(&sp) as f64, 1, 9);
        let pcm = finish(raw, &sp, 9);
        assert_eq!(pcm.frames(), 48_000);
        assert_eq!(pcm.loop_frames, Some((0, 48_000)));
        let s: Vec<f32> = pcm.samples.iter().map(|&v| v as f32).collect();
        let mean_step = s.windows(2).map(|w| (w[1] - w[0]).abs()).sum::<f32>() / (s.len() - 1) as f32;
        let wrap = (s[0] - s[s.len() - 1]).abs();
        assert!(wrap < mean_step * 6.0, "wrap {wrap} mean step {mean_step}");
        // Periodic material passes the crossfade unchanged in level.
        let sp2 = spec(Playback::Loop, 2, 1.0, -3.0);
        let raw = run_unit(&mut sine_hz(100.0), render_seconds(&sp2) as f64, 2, 1);
        let pcm = finish(raw, &sp2, 1);
        assert_eq!(pcm.channels, 2);
        let head_peak = pcm.samples[..2 * 4800].iter().map(|&v| (v as i32).abs()).max().unwrap();
        let mid_peak = pcm.samples[2 * 24_000..2 * 28_800].iter().map(|&v| (v as i32).abs()).max().unwrap();
        assert!((head_peak - mid_peak).abs() < 200, "{head_peak} {mid_peak}");
    }

    #[test]
    fn loop_without_overhang_still_loops() {
        let sp = spec(Playback::Loop, 1, 1.0, -3.0);
        let raw = run_unit(&mut (noise() >> lowpass_hz(500.0, 0.7)), 1.0, 1, 3);
        let pcm = finish(raw, &sp, 3);
        assert_eq!(pcm.frames(), 48_000);
        let (a, b) = pcm.loop_frames.unwrap();
        assert_eq!(a, 0);
        assert!(b > 30_000 && b <= 48_000);
        let pcm = finish(Vec::new(), &sp, 3);
        assert_eq!(pcm.frames(), 48_000);
    }

    #[test]
    fn wav_layout_round_trips() {
        let pcm = Pcm16 { sample_rate: 48_000, channels: 2, samples: vec![1, -2, 3, -4, 32767, -32768], loop_frames: Some((0, 3)) };
        let w = encode_wav(&pcm);
        assert_eq!(&w[0..4], b"RIFF");
        assert_eq!(u32::from_le_bytes(w[4..8].try_into().unwrap()) as usize, w.len() - 8);
        let smpl = w.windows(4).position(|x| x == b"smpl").unwrap();
        let rd = |o: usize| u32::from_le_bytes(w[o..o + 4].try_into().unwrap());
        assert_eq!(rd(smpl + 4), 60);
        assert_eq!(rd(smpl + 8 + 28), 1); // one loop
        assert_eq!(rd(smpl + 8 + 36 + 4), 0); // forward
        assert_eq!(rd(smpl + 8 + 36 + 8), 0);
        assert_eq!(rd(smpl + 8 + 36 + 12), 3);
        let r = hound::WavReader::new(std::io::Cursor::new(w.clone())).unwrap();
        assert_eq!(r.spec().channels, 2);
        assert_eq!(r.spec().sample_rate, 48_000);
        assert_eq!(r.spec().bits_per_sample, 16);
        let back: Vec<i16> = r.into_samples::<i16>().map(|s| s.unwrap()).collect();
        assert_eq!(back, pcm.samples);
        assert_eq!(encode_wav(&pcm), w);
        let one = Pcm16 { sample_rate: 48_000, channels: 1, samples: vec![5; 10], loop_frames: None };
        let w1 = encode_wav(&one);
        assert_eq!(w1.len(), 44 + 20);
        assert!(!w1.windows(4).any(|x| x == b"smpl"));
    }
}
