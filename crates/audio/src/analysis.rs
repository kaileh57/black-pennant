//! Level and quality analysis of rendered sounds: what a mixing engineer would check by ear and meter, done by
//! numbers because nobody can listen in CI. Used by the `levels` test (every sound against its bus target) and
//! by `cargo run -p audio --example report -- <dir>` (a table of the WAVs `cargo xtask gen` wrote).
//!
//! Loudness is ITU-R BS.1770-4: K-weighting (the 48 kHz pre-filter and RLB high-pass), 400 ms momentary
//! blocks on a 100 ms hop, channels summed with unit weight. One-shots are judged by their loudest momentary
//! block (`m_max`, the usual game-SFX meter), loops by gated integrated loudness (`integrated`). Spectra are a
//! Welch average of 4096-point Hann FFTs.
//!
//! DECISION (the spec sets no levels): loudness targets per bus and class are in [`target`]; the file levels
//! are set so a sound played at 0 dB on its bus at the reference distance sits at that loudness, and the
//! runtime gains (`mix`) then only move it for distance and sim state.
use crate::{Bus, Pcm16, Playback, SoundId, SoundSpec};

/// Frequency edges (Hz) of the five report bands: sub, low, mid, presence, air.
pub const BANDS_HZ: [f32; 6] = [0.0, 120.0, 500.0, 2_000.0, 6_000.0, 24_000.0];
const FFT_N: usize = 4096;
/// Bands of the similarity fingerprint (log-spaced, 40 Hz to 16 kHz).
const PRINT_BANDS: usize = 24;
/// Points of the loudness-envelope part of the fingerprint.
const PRINT_ENV: usize = 16;

/// Everything measured on one file.
#[derive(Clone, Debug, PartialEq)]
pub struct Report {
    pub duration_s: f32,
    pub channels: u16,
    /// Sample peak, dBFS.
    pub peak_dbfs: f32,
    /// Samples at the 16-bit rails.
    pub clipped: usize,
    /// Whole-file RMS, dBFS (all channels).
    pub rms_dbfs: f32,
    /// Largest channel mean, as a fraction of full scale.
    pub dc: f32,
    /// Loudest 400 ms block, LUFS.
    pub m_max: f32,
    /// Gated integrated loudness, LUFS.
    pub integrated: f32,
    /// Loudest 100 ms block, LUFS: how loud a short tick or pulse is heard, which the 400 ms meter under-reads.
    pub s100_max: f32,
    /// Loudest block through a 200 Hz high-pass (a laptop speaker) minus `m_max`, LU: how much of the sound
    /// survives on small speakers.
    pub small_lu: f32,
    /// Seconds before the signal first reaches -40 dB under its peak.
    pub lead_s: f32,
    /// Seconds until the momentary loudness falls for good 40 LU under its maximum (the audible length).
    pub audible_s: f32,
    /// Momentary loudness of the last 100 ms before the end, relative to `m_max` (one-shots: a cut tail is
    /// louder than -30 LU).
    pub tail_lu: f32,
    /// Loops: sample step across the wrap divided by the median absolute step (1 is seamless).
    pub seam_step: f32,
    /// Loops: momentary loudness of the last 200 ms minus the first 200 ms, LU.
    pub seam_lu: f32,
    /// Fraction of power in each of [`BANDS_HZ`]'s five bands.
    pub bands: [f32; 5],
    /// Power-weighted mean frequency, Hz.
    pub centroid_hz: f32,
    /// Power fraction between 2 and 5 kHz, where the ear is most sensitive (harshness).
    pub harsh: f32,
    /// Similarity fingerprint: [`PRINT_BANDS`] log-band levels in dB, then [`PRINT_ENV`] points of the 10 ms
    /// RMS envelope across the audible length, relative to its maximum.
    pub print: Vec<f32>,
}

/// Loudness target of a sound: (low, high) in LUFS, measured as `m_max` for one-shots and `integrated` for
/// loops. Unit: `audio-levels`.
///
/// Reference: the loudest near events (a large gun at the mount, a magazine cook-off) sit at -13 to -9 LUFS
/// momentary, so a single report peaks near the -1 dBFS ceiling after light limiting and the master limiter
/// only touches stacks. Lighter weapons and hits step down about 2 LU a class, so calibre reads as loudness.
/// Ambience beds play at 0 dB and are not distance-scaled, so they sit 20 to 25 LU under the near guns: the
/// summed bus (sea, wind, rain, wash and the own engine) stays about 10 LU under the own gun, level with a gun
/// at 1 km and within 6 LU of a distant report at 5 km, whose file is as loud as a large near report and is
/// taken down by the distance law (the `ambience_bus_leaves_room_for_reports_at_range` test). Alerts sit under the guns but over the beds; their
/// 0.5-1.5 kHz tones carry over the low-heavy battle. Radio clicks are quiet and the static quieter; interface
/// ticks are quietest, state-change notes a little louder. The menu theme plays alone, at a streaming level.
pub fn target(id: SoundId) -> (f32, f32) {
    use crate::{AmbienceSfx as A, ImpactSfx as I, WeaponSfx as W};
    match id {
        SoundId::Weapon(w) => match w {
            W::GunHeavy | W::GunLarge => (-13.0, -9.0),
            W::GunMedium | W::MissileLaunch | W::AircraftPass => (-15.0, -11.0),
            W::GunAuto | W::Ciws | W::SamLaunch | W::TorpedoLaunch => (-17.0, -13.0),
            W::GunDistantHeavy | W::GunDistantLight => (-13.0, -9.0),
            W::DecoyLaunch | W::SmokeLaunch | W::DroneLaunch | W::ShellPassby => (-20.0, -15.0),
            W::DepthChargeRoll | W::MineDrop => (-23.0, -18.0),
            W::MissileMotor | W::HelicopterRotor => (-22.0, -17.0),
            W::DroneMotor | W::RhibOutboard => (-25.0, -20.0),
            W::TurretTrain => (-31.0, -26.0),
        },
        SoundId::Impact(i) => match i {
            I::ExplosionLarge | I::CookOff | I::UnderwaterBurst => (-13.0, -9.0),
            I::ExplosionSmall | I::HullBreak | I::Collision | I::Grounding | I::PenetrateHit => (-16.0, -12.0),
            I::SplashLarge | I::AirBurst => (-17.0, -13.0),
            I::SplashMedium | I::Ricochet | I::AutocannonHit | I::BreachRush | I::FireIgnite => (-20.0, -15.0),
            I::SplashSmall | I::DebrisFall | I::HullGroan | I::SteamHiss => (-23.0, -18.0),
            I::FireLoop | I::FloodingLoop => (-25.0, -20.0),
            I::PumpLoop => (-31.0, -26.0),
        },
        SoundId::Ambience(a) => match a {
            A::EngineDiesel | A::EngineGasTurbine => (-30.0, -26.0),
            A::EngineElectric | A::Cavitation => (-37.0, -32.0),
            A::HullWashSlow | A::HullWashFast => (-38.0, -33.0),
            A::SeaCalm => (-37.0, -33.0),
            A::SeaModerate => (-35.0, -31.0),
            A::SeaRough => (-33.0, -29.0),
            A::SeaStorm => (-31.0, -27.0),
            A::Wind | A::Rain => (-38.0, -33.0),
            A::WindGale | A::RainHeavy => (-34.0, -29.0),
            A::ThunderNear => (-15.0, -11.0),
            A::ThunderFar => (-22.0, -17.0),
            A::SonarPing => (-26.0, -21.0),
            A::SonarPingHeard => (-29.0, -24.0),
        },
        // Alerts sit over the radio and the radio over the interface (the levels test checks it on 100 ms
        // blocks); the Caution tick and the clicks are judged on 100 ms blocks (`SHORT_S`).
        SoundId::Alert(a) => match a {
            crate::AlertSound::Emergency => (-16.0, -13.0),
            crate::AlertSound::Alarm => (-18.0, -15.0),
            crate::AlertSound::Warning => (-20.0, -17.0),
            crate::AlertSound::Caution => (-21.0, -18.0),
        },
        SoundId::Radio(r) => match r {
            crate::RadioSfx::Static => (-38.0, -33.0),
            crate::RadioSfx::AceOpen => (-29.0, -25.0),
            _ => (-25.0, -21.0),
        },
        SoundId::Ui(u) => match u {
            crate::UiSfx::Click | crate::UiSfx::Select | crate::UiSfx::Rotate | crate::UiSfx::Snap => (-32.0, -28.0),
            crate::UiSfx::TutorialStep | crate::UiSfx::ObjectiveDone => (-33.0, -29.0),
            _ => (-29.0, -25.0),
        },
        SoundId::MenuTheme => (-18.0, -15.0),
    }
}

/// The loudness of `r` that [`target`] is measured against: integrated for a loop, the loudest 100 ms block
/// for a one-shot of [`SHORT_S`] or less, else the loudest 400 ms block.
pub fn judged_loudness(spec: &SoundSpec, r: &Report) -> f32 {
    match spec.playback {
        Playback::Loop => r.integrated,
        Playback::OneShot if r.duration_s <= SHORT_S + 1e-3 => r.s100_max,
        Playback::OneShot => r.m_max,
    }
}

/// Bus of a spec, for grouping.
pub fn bus_name(b: Bus) -> &'static str { b.name() }

fn db(p: f64) -> f32 { if p > 1e-20 { (10.0 * libm::log10(p)) as f32 } else { -200.0 } }

/// One biquad, direct form I, in f64.
#[derive(Clone, Copy)]
struct Biquad { b: [f64; 3], a: [f64; 2], x: [f64; 2], y: [f64; 2] }
impl Biquad {
    fn new(b: [f64; 3], a: [f64; 2]) -> Self { Self { b, a, x: [0.0; 2], y: [0.0; 2] } }
    fn tick(&mut self, x: f64) -> f64 {
        let y = self.b[0] * x + self.b[1] * self.x[0] + self.b[2] * self.x[1] - self.a[0] * self.y[0] - self.a[1] * self.y[1];
        self.x = [x, self.x[0]];
        self.y = [y, self.y[0]];
        y
    }
}

/// K-weighted squares per frame, summed over channels (BS.1770 at 48 kHz).
fn k_weighted_power(ch: &[Vec<f64>]) -> Vec<f64> {
    let frames = ch.first().map_or(0, |c| c.len());
    let mut out = vec![0.0f64; frames];
    for c in ch {
        let mut shelf = Biquad::new([1.535_124_859_586_97, -2.691_696_189_406_38, 1.198_392_810_852_85], [-1.690_659_293_182_41, 0.732_480_774_215_85]);
        let mut hp = Biquad::new([1.0, -2.0, 1.0], [-1.990_047_454_833_98, 0.990_072_250_366_21]);
        for (o, &x) in out.iter_mut().zip(c.iter()) {
            let y = hp.tick(shelf.tick(x));
            *o += y * y;
        }
    }
    out
}

/// Momentary loudness (LUFS) of 400 ms blocks every 100 ms. Files shorter than a block are measured as one
/// block padded with silence (a 50 ms click is judged as a 400 ms window, as a meter would).
fn momentary(kp: &[f64], sr: usize) -> Vec<f32> {
    let block = sr * 4 / 10;
    let hop = sr / 10;
    let mut out = Vec::new();
    let n = kp.len();
    let mut start = 0usize;
    loop {
        let end = (start + block).min(n);
        let s: f64 = kp[start.min(n)..end].iter().sum();
        out.push(-0.691 + db(s / block as f64));
        if start + block >= n { break; }
        start += hop;
    }
    out
}

/// Loudest 100 ms block (10 ms hop), LUFS; a file shorter than a block is one block padded with silence.
fn s100(kp: &[f64], sr: usize) -> f32 {
    let block = (sr / 10).max(1);
    let hop = (sr / 100).max(1);
    let mut best = -200.0f32;
    let mut start = 0usize;
    while start < kp.len().max(1) {
        let end = (start + block).min(kp.len());
        best = best.max(-0.691 + db(kp[start..end].iter().sum::<f64>() / block as f64));
        if end >= kp.len() { break; }
        start += hop;
    }
    best
}

/// One-shots this short are judged on 100 ms blocks, the ear's window for a tick or a click, since the
/// 400 ms meter reads them several LU under how loud they are heard.
pub const SHORT_S: f32 = 0.4;

/// Gated integrated loudness (absolute -70 LUFS gate, relative -10 LU gate).
fn integrated(m: &[f32]) -> f32 {
    let pw = |l: f32| libm::pow(10.0, ((l + 0.691) / 10.0) as f64);
    let abs: Vec<f32> = m.iter().copied().filter(|&l| l > -70.0).collect();
    if abs.is_empty() { return -200.0; }
    let mean = abs.iter().map(|&l| pw(l)).sum::<f64>() / abs.len() as f64;
    let gate = -0.691 + db(mean) - 10.0;
    let rel: Vec<f32> = abs.into_iter().filter(|&l| l > gate).collect();
    if rel.is_empty() { return -200.0; }
    -0.691 + db(rel.iter().map(|&l| pw(l)).sum::<f64>() / rel.len() as f64)
}

/// In-place radix-2 FFT over (re, im).
fn fft(re: &mut [f64], im: &mut [f64]) {
    let n = re.len();
    let mut j = 0usize;
    for i in 1..n {
        let mut bit = n >> 1;
        while j & bit != 0 { j ^= bit; bit >>= 1; }
        j |= bit;
        if i < j { re.swap(i, j); im.swap(i, j); }
    }
    let mut len = 2;
    while len <= n {
        let ang = -2.0 * core::f64::consts::PI / len as f64;
        let (wr, wi) = (libm::cos(ang), libm::sin(ang));
        for s in (0..n).step_by(len) {
            let (mut cr, mut ci) = (1.0f64, 0.0f64);
            for k in 0..len / 2 {
                let (a, b) = (s + k, s + k + len / 2);
                let tr = re[b] * cr - im[b] * ci;
                let ti = re[b] * ci + im[b] * cr;
                re[b] = re[a] - tr; im[b] = im[a] - ti;
                re[a] += tr; im[a] += ti;
                let ncr = cr * wr - ci * wi;
                ci = cr * wi + ci * wr;
                cr = ncr;
            }
        }
        len <<= 1;
    }
}

/// Welch power spectrum of a mono signal: `FFT_N / 2` bins.
fn spectrum(x: &[f64]) -> Vec<f64> {
    let half = FFT_N / 2;
    let mut acc = vec![0.0f64; half];
    let win: Vec<f64> = (0..FFT_N).map(|i| 0.5 - 0.5 * libm::cos(2.0 * core::f64::consts::PI * i as f64 / FFT_N as f64)).collect();
    let mut start = 0usize;
    loop {
        let mut re: Vec<f64> = (0..FFT_N).map(|i| x.get(start + i).copied().unwrap_or(0.0) * win[i]).collect();
        let mut im = vec![0.0f64; FFT_N];
        fft(&mut re, &mut im);
        for k in 0..half { acc[k] += re[k] * re[k] + im[k] * im[k]; }
        if start + FFT_N >= x.len() { break; }
        start += FFT_N / 2;
    }
    acc
}

fn deinterleave_f32(buf: &[f32], ch: usize) -> Vec<Vec<f64>> {
    let ch = ch.max(1);
    let frames = buf.len() / ch;
    (0..ch).map(|c| (0..frames).map(|i| buf[i * ch + c] as f64).collect()).collect()
}

/// The loudness [`target`] judges, of an interleaved float buffer at 48 kHz: the loudest momentary block for a
/// one-shot, gated integrated loudness for a loop. Used by `render::finish` to set file levels.
pub fn judged_lufs(buf: &[f32], ch: usize, playback: Playback) -> f32 {
    let sr = crate::SAMPLE_RATE as usize;
    let kp = k_weighted_power(&deinterleave_f32(buf, ch));
    let m = momentary(&kp, sr);
    match playback {
        Playback::Loop => integrated(&m),
        Playback::OneShot if (kp.len() as f32) <= SHORT_S * sr as f32 => s100(&kp, sr),
        Playback::OneShot => m.iter().copied().fold(-200.0f32, f32::max),
    }
}

/// Second-order Butterworth high-pass at `f` Hz (RBJ), 48 kHz.
fn highpass(f: f64) -> Biquad {
    let w = 2.0 * core::f64::consts::PI * f / crate::SAMPLE_RATE as f64;
    let (cw, sw) = (libm::cos(w), libm::sin(w));
    let alpha = sw / (2.0 * core::f64::consts::FRAC_1_SQRT_2);
    let a0 = 1.0 + alpha;
    Biquad::new([(1.0 + cw) / 2.0 / a0, -(1.0 + cw) / a0, (1.0 + cw) / 2.0 / a0], [-2.0 * cw / a0, (1.0 - alpha) / a0])
}

/// Measure one rendered sound.
pub fn analyse(pcm: &Pcm16, playback: Playback) -> Report {
    let ch = pcm.channels.max(1) as usize;
    let frames = pcm.samples.len() / ch;
    let chans: Vec<Vec<f64>> = (0..ch).map(|c| (0..frames).map(|i| pcm.samples[i * ch + c] as f64 / 32768.0).collect()).collect();
    let clipped = pcm.samples.iter().filter(|&&s| s == i16::MAX || s == i16::MIN).count();
    measure(&chans, pcm.sample_rate.max(1) as usize, playback, clipped)
}

fn measure(chans: &[Vec<f64>], sr: usize, playback: Playback, clipped: usize) -> Report {
    let ch = chans.len().max(1);
    let frames = chans.first().map_or(0, |c| c.len());
    let peak = chans.iter().flat_map(|c| c.iter()).fold(0.0f64, |m, &x| m.max(x.abs()));
    let ms = chans.iter().flat_map(|c| c.iter()).map(|v| v * v).sum::<f64>() / (frames * ch).max(1) as f64;
    let dc = chans.iter().map(|c| (c.iter().sum::<f64>() / frames.max(1) as f64).abs()).fold(0.0f64, f64::max) as f32;
    let kp = k_weighted_power(chans);
    let m = momentary(&kp, sr);
    let m_max = m.iter().copied().fold(-200.0f32, f32::max);
    let s100_max = s100(&kp, sr);
    // Small speakers: the loudest block again after a 4th-order 200 Hz high-pass (a laptop or phone).
    let small: Vec<Vec<f64>> = chans.iter().map(|c| { let (mut a, mut b) = (highpass(200.0), highpass(200.0)); c.iter().map(|&x| b.tick(a.tick(x))).collect() }).collect();
    let small_max = momentary(&k_weighted_power(&small), sr).iter().copied().fold(-200.0f32, f32::max);
    let integ = integrated(&m);
    // Lead: first frame at -40 dB under the peak.
    let thr = peak * 0.01;
    let lead = (0..frames).find(|&i| chans.iter().any(|c| c[i].abs() >= thr)).unwrap_or(frames);
    // Audible length: end of the last 100 ms hop whose block is within 40 LU of the maximum.
    let last_loud = m.iter().rposition(|&l| l > m_max - 40.0).unwrap_or(0);
    let audible_s = ((last_loud as f32 * 0.1 + 0.4).min(frames as f32 / sr as f32)).max(0.0);
    // Tail: the 100 ms before the fade-out.
    let tail_n = (sr / 10).min(frames);
    let tail_p: f64 = kp[frames - tail_n..].iter().sum::<f64>() / tail_n.max(1) as f64;
    let tail_lu = -0.691 + db(tail_p) - m_max;
    // Seam (loops): wrap step against the median step, and the loudness of both ends.
    let (mut seam_step, mut seam_lu) = (0.0f32, 0.0f32);
    if playback == Playback::Loop && frames > 2 {
        let mut worst = 0.0f32;
        for c in chans {
            let mut steps: Vec<f64> = c.windows(2).map(|w| (w[1] - w[0]).abs()).collect();
            let mid = steps.len() / 2;
            steps.select_nth_unstable_by(mid, |a, b| a.total_cmp(b));
            let med = steps[mid].max(1e-6);
            let wrap = (c[0] - c[frames - 1]).abs();
            worst = worst.max((wrap / med) as f32);
        }
        seam_step = worst;
        let w = (sr / 5).min(frames / 2);
        let head: f64 = kp[..w].iter().sum();
        let tail: f64 = kp[frames - w..].iter().sum();
        seam_lu = db(tail.max(1e-30)) - db(head.max(1e-30));
    }
    // Spectrum of the mid (sum) signal.
    let mid: Vec<f64> = (0..frames).map(|i| chans.iter().map(|c| c[i]).sum::<f64>() / ch as f64).collect();
    let spec = spectrum(&mid);
    let bin_hz = sr as f64 / FFT_N as f64;
    let total: f64 = spec.iter().skip(1).sum::<f64>().max(1e-30);
    let mut bands = [0.0f32; 5];
    let mut centroid = 0.0f64;
    let mut harsh = 0.0f64;
    for (k, &p) in spec.iter().enumerate().skip(1) {
        let f = k as f64 * bin_hz;
        let b = (0..5).find(|&b| (f as f32) < BANDS_HZ[b + 1]).unwrap_or(4);
        bands[b] += (p / total) as f32;
        centroid += f * p;
        if (2_000.0..5_000.0).contains(&f) { harsh += p; }
    }
    centroid /= total;
    // Fingerprint: log-band spectrum in dB (floored 60 dB under its top), then the momentary envelope.
    let mut lb = vec![0.0f64; PRINT_BANDS];
    let (lo, hi) = (libm::log(40.0), libm::log(16_000.0));
    for (k, &p) in spec.iter().enumerate().skip(1) {
        let f = k as f64 * bin_hz;
        if !(40.0..16_000.0).contains(&f) { continue; }
        let b = (((libm::log(f) - lo) / (hi - lo)) * PRINT_BANDS as f64) as usize;
        lb[b.min(PRINT_BANDS - 1)] += p;
    }
    let mut print: Vec<f32> = lb.iter().map(|&p| db(p / total).max(-90.0)).collect();
    // Envelope: 10 ms RMS frames across the audible length, resampled to PRINT_ENV points.
    let win = (sr / 100).max(1);
    let env: Vec<f32> = (0..frames.div_ceil(win))
        .map(|w| {
            let (a, b) = (w * win, ((w + 1) * win).min(frames));
            let p: f64 = chans.iter().map(|c| c[a..b].iter().map(|v| v * v).sum::<f64>()).sum::<f64>() / ((b - a).max(1) * ch) as f64;
            db(p)
        })
        .collect();
    let top = env.iter().copied().fold(-200.0f32, f32::max);
    let used = ((audible_s * sr as f32) as usize).div_ceil(win).clamp(1, env.len().max(1));
    for i in 0..PRINT_ENV {
        let j = (i * used) / PRINT_ENV;
        print.push((env.get(j).copied().unwrap_or(-200.0) - top).max(-60.0));
    }
    Report {
        duration_s: frames as f32 / sr as f32,
        channels: ch as u16,
        peak_dbfs: if peak > 0.0 { (20.0 * libm::log10(peak)) as f32 } else { -200.0 },
        clipped,
        rms_dbfs: db(ms),
        dc,
        m_max,
        integrated: integ,
        small_lu: small_max - m_max,
        s100_max,
        lead_s: lead as f32 / sr as f32,
        audible_s,
        tail_lu,
        seam_step,
        seam_lu,
        bands,
        centroid_hz: centroid as f32,
        harsh: (harsh / total) as f32,
        print,
    }
}

fn pearson(a: &[f32], b: &[f32]) -> f32 {
    let n = a.len().min(b.len()).max(1) as f32;
    let (ma, mb) = (a.iter().sum::<f32>() / n, b.iter().sum::<f32>() / n);
    let (mut sab, mut saa, mut sbb) = (0.0f32, 0.0f32, 0.0f32);
    for (x, y) in a.iter().zip(b.iter()) {
        sab += (x - ma) * (y - mb);
        saa += (x - ma) * (x - ma);
        sbb += (y - mb) * (y - mb);
    }
    if saa < 1e-9 || sbb < 1e-9 { return if saa < 1e-9 && sbb < 1e-9 { 1.0 } else { 0.0 }; }
    sab / libm::sqrtf(saa * sbb)
}

/// How alike two sounds are, at most 1: the mean of the correlations of their spectral shapes and of their
/// 10 ms envelopes over their audible lengths, less a penalty for different audible lengths (0.1 per
/// doubling). Above 0.95 a player would hear the same sound.
pub fn similarity(a: &Report, b: &Report) -> f32 {
    let s = pearson(&a.print[..PRINT_BANDS], &b.print[..PRINT_BANDS]);
    let e = pearson(&a.print[PRINT_BANDS..], &b.print[PRINT_BANDS..]);
    let len = libm::fabsf(libm::log2f(a.audible_s.max(0.01) / b.audible_s.max(0.01)));
    0.5 * (s + e) - 0.1 * len
}

/// Defects that fail the build: silence, clipping, a peak over the ceiling, DC, a level outside its target, a
/// loop seam that clicks or jumps in level.
pub fn problems(spec: &SoundSpec, r: &Report) -> Vec<String> {
    let mut p = Vec::new();
    let loud = judged_loudness(spec, r);
    let (lo, hi) = target(spec.id);
    if r.peak_dbfs < -60.0 { p.push("silent".into()); }
    if r.clipped > 0 { p.push(format!("{} clipped samples", r.clipped)); }
    if r.peak_dbfs > -0.9 { p.push(format!("peak {:.1} dBFS over -1", r.peak_dbfs)); }
    if r.dc > 0.002 { p.push(format!("DC {:.4}", r.dc)); }
    if loud < lo - 0.05 || loud > hi + 0.05 { p.push(format!("loudness {loud:.1} outside {lo:.0}..{hi:.0}")); }
    if spec.playback == Playback::Loop {
        if r.seam_step > 12.0 { p.push(format!("seam step {:.1}x", r.seam_step)); }
        if r.seam_lu.abs() > 3.0 { p.push(format!("seam level {:+.1} LU", r.seam_lu)); }
    }
    p
}

/// What a mixing engineer would also flag, printed for review but not failing: a slow onset, a tail the file
/// cuts, a file much longer than what is heard, weight only under 200 Hz, harshness.
pub fn advisories(spec: &SoundSpec, r: &Report) -> Vec<String> {
    let mut p = Vec::new();
    if spec.playback == Playback::OneShot {
        if r.lead_s > 0.03 { p.push(format!("{:.0} ms of lead silence", r.lead_s * 1000.0)); }
        if r.tail_lu > -30.0 && r.duration_s > 0.45 { p.push(format!("tail cut ({:.0} LU at the end)", r.tail_lu)); }
        if r.duration_s > 0.5 && r.audible_s < 0.5 * r.duration_s { p.push(format!("audible {:.2} s of {:.2} s", r.audible_s, r.duration_s)); }
    }
    if r.small_lu < -12.0 && spec.bus != Bus::Ui { p.push(format!("thin on small speakers ({:.0} LU)", r.small_lu)); }
    if r.harsh > 0.55 && !matches!(spec.bus, Bus::Alert) { p.push(format!("harsh ({:.0}% at 2-5 kHz)", r.harsh * 100.0)); }
    p
}
