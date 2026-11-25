//! Unit `audio-weapons`. Gun reports by calibre band (near and distant), point defence, launches, motor loops,
//! torpedo and depth-charge launches, decoys, shell pass-by, mount slew loop, drones, aircraft (Act 2),
//! helicopter (Act 2) and RHIB loops. DESIGN.md L1239-1247, L539-553 (weapon families), L565-570 (bands).
//! Reports: noise burst through a resonant body plus a low thump; larger bands lower and longer; distant
//! variants low-passed with a rolling echo tail. Motor and rotor loops must seam (see `render::finish`).
//!
//! Every patch is layered from the shared toolkit (`render::kit`): a shock pulse and band-limited crack for
//! the transient, a pitch-dropping body and a sub thump for weight, a darkening noise roar, then mechanical
//! detail (breech and recoil clanks, case ejection, latch and rail noises) and a tail scaled to the file's
//! length. Loops render [`render_seconds`] so `finish` can seam them, and their tonal parts hold a whole number
//! of cycles in the loop ([`snap_hz`]).
use crate::render::kit::*;
use crate::render::render_seconds;
use crate::{SoundSpec, WeaponSfx};

/// Unit: `audio-weapons`.
pub fn render(id: WeaponSfx, spec: &SoundSpec, seed: u64) -> Vec<f32> {
    let sec = render_seconds(spec).max(0.05);
    let period = spec.duration_s.max(0.05);
    let x = match id {
        WeaponSfx::GunAuto => gun_near(sec, seed, &AUTO),
        WeaponSfx::GunMedium => gun_near(sec, seed, &MEDIUM),
        WeaponSfx::GunLarge => gun_near(sec, seed, &LARGE),
        WeaponSfx::GunHeavy => gun_near(sec, seed, &HEAVY),
        WeaponSfx::GunDistantLight => gun_distant(sec, seed, false),
        WeaponSfx::GunDistantHeavy => gun_distant(sec, seed, true),
        WeaponSfx::Ciws => ciws(sec, seed),
        WeaponSfx::MissileLaunch => missile_launch(sec, seed, false),
        WeaponSfx::SamLaunch => missile_launch(sec, seed, true),
        WeaponSfx::MissileMotor => missile_motor(sec, seed, period),
        WeaponSfx::TorpedoLaunch => torpedo_launch(sec, seed),
        WeaponSfx::DepthChargeRoll => depth_charge_roll(sec, seed),
        WeaponSfx::MineDrop => mine_drop(sec, seed),
        WeaponSfx::DecoyLaunch => mortar(sec, seed, false),
        WeaponSfx::SmokeLaunch => mortar(sec, seed, true),
        WeaponSfx::ShellPassby => shell_passby(sec, seed),
        WeaponSfx::TurretTrain => turret_train(sec, seed, period),
        WeaponSfx::DroneLaunch => drone_launch(sec, seed),
        WeaponSfx::DroneMotor => drone_motor(sec, seed, period),
        WeaponSfx::AircraftPass => aircraft_pass(sec, seed),
        WeaponSfx::HelicopterRotor => helicopter_rotor(sec, seed, period),
        WeaponSfx::RhibOutboard => rhib_outboard(sec, seed, period),
    };
    let mut x = x;
    x.resize(frames(sec), 0.0);
    x
}

// ---------------------------------------------------------------------------------------------------------
// Gun reports.

/// One calibre band's near report.
struct GunBand {
    boom: Boom,
    /// Breech or recoil clank: delay (s), plate fundamental (Hz), time constant (s), gain.
    clank_at: f32,
    clank_f: f32,
    clank_tau: f32,
    clank: f32,
    /// Spent case striking the deck: delay (s, 0 = none) and gain.
    case_at: f32,
    case: f32,
    /// Low tail: cutoff (Hz), gain, echo taps.
    tail_lp: f32,
    tail: f32,
    taps: usize,
    /// Muzzle blast through the chest band: band-passed noise from `blast_lo` to `blast_hi` Hz decaying with
    /// `blast_tau`, at `blast` gain. It carries the report on small speakers, where the sub thump is lost,
    /// and lowers and lengthens with calibre so each band has its own voice.
    blast_lo: f32,
    blast_hi: f32,
    blast_tau: f32,
    blast: f32,
}

#[allow(clippy::too_many_arguments)]
const fn band_boom(nwave_s: f32, crack_tau: f32, crack_lo: f32, f0: f32, f1: f32, drop: f32, body_tau: f32, sub_f: f32, sub_tau: f32, roar_tau: f32, roar_lp1: f32, drive: f32) -> Boom {
    Boom {
        nwave: 1.0,
        nwave_s,
        crack: 0.9,
        crack_tau,
        crack_lo,
        crack_hi: 8_500.0,
        body: 1.0,
        body_f0: f0,
        body_f1: f1,
        body_drop: drop,
        body_tau,
        sub: 0.85,
        sub_f,
        sub_tau,
        roar: 0.75,
        roar_tau,
        roar_lp0: 6_500.0,
        roar_lp1,
        roar_sweep: roar_tau * 0.3,
        drive,
    }
}

/// 20-40 mm: a tight bark with a quick breech clack (the file repeats per round, so its tail stays short).
const AUTO: GunBand = GunBand {
    boom: band_boom(0.0008, 0.006, 1_300.0, 260.0, 85.0, 0.018, 0.045, 68.0, 0.06, 0.07, 600.0, 1.7),
    clank_at: 0.035,
    clank_f: 1_850.0,
    clank_tau: 0.025,
    clank: 0.16,
    case_at: 0.0,
    case: 0.0,
    tail_lp: 520.0,
    tail: 0.22,
    taps: 2,
    blast_lo: 320.0,
    blast_hi: 3_200.0,
    blast_tau: 0.03,
    blast: 1.0,
};
/// 57-76 mm: a hard crack and a punchy body, the slide's clank and the spent case ringing on the deck.
const MEDIUM: GunBand = GunBand {
    boom: band_boom(0.0014, 0.010, 900.0, 170.0, 52.0, 0.03, 0.11, 55.0, 0.16, 0.16, 350.0, 2.1),
    clank_at: 0.12,
    clank_f: 620.0,
    clank_tau: 0.08,
    clank: 0.12,
    case_at: 0.42,
    case: 0.07,
    tail_lp: 380.0,
    tail: 0.38,
    taps: 3,
    blast_lo: 220.0,
    blast_hi: 2_400.0,
    blast_tau: 0.05,
    blast: 1.0,
};
/// 100-127 mm: deep blast, heavy recoil clank, long low tail.
const LARGE: GunBand = GunBand {
    boom: band_boom(0.0022, 0.014, 700.0, 130.0, 40.0, 0.045, 0.18, 45.0, 0.28, 0.25, 250.0, 2.5),
    clank_at: 0.2,
    clank_f: 380.0,
    clank_tau: 0.15,
    clank: 0.1,
    case_at: 0.65,
    case: 0.05,
    tail_lp: 280.0,
    tail: 0.48,
    taps: 4,
    blast_lo: 150.0,
    blast_hi: 1_700.0,
    blast_tau: 0.075,
    blast: 0.95,
};
/// 130-155 mm (Act 3): the heaviest, slowest blast with a rolling tail.
const HEAVY: GunBand = GunBand {
    boom: band_boom(0.003, 0.018, 550.0, 110.0, 32.0, 0.06, 0.25, 40.0, 0.4, 0.35, 200.0, 2.9),
    clank_at: 0.28,
    clank_f: 260.0,
    clank_tau: 0.2,
    clank: 0.08,
    case_at: 0.0,
    case: 0.0,
    tail_lp: 220.0,
    tail: 0.55,
    taps: 5,
    blast_lo: 90.0,
    blast_hi: 1_000.0,
    blast_tau: 0.11,
    blast: 0.9,
};

fn gun_near(sec: f32, seed: u64, g: &GunBand) -> Vec<f32> {
    let mut out = silence(sec);
    let blast = boom(sec, sub(seed, 1), &g.boom);
    add(&mut out, &blast, 0.0, 1.0);
    let (bt, len) = (g.blast_tau, (g.blast_tau * 9.0).min(sec));
    let chest = sat(norm(shape(noise_band(len, sub(seed, 5), g.blast_lo, g.blast_hi), move |t| ad(t, 0.0006, bt))), 1.6);
    add(&mut out, &norm(chest), 0.0, g.blast);
    // Breech or recoil clank.
    let clank = metal_hit((g.clank_tau * 8.0).min(sec), sub(seed, 2), g.clank_f, g.clank_tau, 6, 0.0015);
    add(&mut out, &clank, g.clank_at, g.clank);
    // Spent case: two or three bright hits as it bounces.
    if g.case > 0.0 {
        let mut t = g.case_at;
        let mut a = g.case;
        for k in 0..3u64 {
            clink_grain(&mut out, t, rr(seed, 40 + k, 2_300.0, 2_700.0), 0.05, a, sub(seed, 50 + k));
            t += rr(seed, 60 + k, 0.08, 0.14) * (1.0 - 0.3 * k as f32);
            a *= 0.5;
        }
    }
    // Low tail: the blast's energy rolling off the sea, scaled to the file.
    let tail_tau = (sec / 6.5).max(0.05);
    let rb = rumble(sec, sub(seed, 3), g.tail_lp, 0.01, (g.boom.body_tau * 0.6).max(0.02), tail_tau, 0.5);
    add(&mut out, &rb, 0.0, g.tail);
    let head = head(&blast, (g.boom.body_tau * 2.0).min(sec));
    echoes(&mut out, &head, 0.0, sub(seed, 4), g.taps, 0.07 + 0.25 * g.boom.body_tau, 0.09 + 0.4 * g.boom.body_tau, 0.32, 2_000.0, 0.7);
    norm(out)
}

/// Distant reports: no bright crack, a muffled thud through the air's low-pass, then rolling echoes and a
/// rumble that swells irregularly, like far thunder. Heavy guns sit lower and roll for longer.
fn gun_distant(sec: f32, seed: u64, heavy: bool) -> Vec<f32> {
    let mut b = Boom::BASE;
    b.nwave = 0.55;
    b.nwave_s = if heavy { 0.004 } else { 0.0025 };
    b.crack = 0.25;
    b.crack_tau = 0.012;
    b.crack_lo = 300.0;
    b.crack_hi = 2_200.0;
    b.body_f0 = if heavy { 80.0 } else { 120.0 };
    b.body_f1 = if heavy { 32.0 } else { 55.0 };
    b.body_drop = 0.05;
    b.body_tau = if heavy { 0.2 } else { 0.09 };
    b.sub = if heavy { 0.9 } else { 0.55 };
    b.sub_f = if heavy { 38.0 } else { 52.0 };
    b.sub_tau = if heavy { 0.35 } else { 0.15 };
    b.roar = 0.8;
    b.roar_tau = if heavy { 0.3 } else { 0.15 };
    b.roar_lp0 = 1_600.0;
    b.roar_lp1 = 250.0;
    b.roar_sweep = 0.08;
    b.drive = 1.6;
    let cutoff = if heavy { 900.0 } else { 1_400.0 };
    let mut out = silence(sec);
    let thud = lp4(&boom(sec, sub(seed, 1), &b), cutoff);
    let thud = norm(thud);
    add(&mut out, &thud, 0.0, 1.0);
    let head = head(&thud, if heavy { 0.5 } else { 0.3 });
    let (taps, first, spacing) = if heavy { (9, 0.16, 0.2) } else { (6, 0.11, 0.14) };
    echoes(&mut out, &head, 0.0, sub(seed, 2), taps, first, spacing, 0.72, cutoff * 0.8, 0.82);
    let roll = rumble(sec, sub(seed, 3), if heavy { 190.0 } else { 300.0 }, 0.03, if heavy { 0.25 } else { 0.12 }, (sec / 4.5).max(0.1), 0.9);
    add(&mut out, &roll, 0.0, if heavy { 0.75 } else { 0.55 });
    // The report's chest band as it carries over water: a dull thump at 120-600 Hz (heavy guns lower), so a
    // far gun still reads on small speakers that cannot play its sub.
    let (lo, hi, tau): (f32, f32, f32) = if heavy { (100.0, 450.0, 0.16) } else { (140.0, 650.0, 0.09) };
    let thump = norm(shape(noise_band((tau * 9.0).min(sec), sub(seed, 6), lo, hi), move |t| ad(t, 0.004, tau)));
    add(&mut out, &thump, 0.0, if heavy { 0.8 } else { 0.7 });
    let fade_tau = sec / 6.0;
    norm(shape(out, move |t| if t < sec * 0.4 { 1.0 } else { libm::expf(-(t - sec * 0.4) / fade_tau) }))
}

/// Rotary point-defence gun: about 75 rounds a second after a short spin-up, each round a tiny crack and
/// body, over a continuous roar; then the barrels spin down.
fn ciws(sec: f32, seed: u64) -> Vec<f32> {
    let burst_end = (sec * 0.7).min(1.05);
    let mut out = silence(sec);
    // Four shot templates, rotated, so the train is not machine-identical.
    let mut shot = Boom::BASE;
    shot.nwave = 0.6;
    shot.nwave_s = 0.0005;
    shot.crack = 0.9;
    shot.crack_tau = 0.0025;
    shot.crack_lo = 1_500.0;
    shot.body = 0.8;
    shot.body_f0 = 240.0;
    shot.body_f1 = 120.0;
    shot.body_drop = 0.004;
    shot.body_tau = 0.009;
    shot.sub = 0.35;
    shot.sub_f = 75.0;
    shot.sub_tau = 0.012;
    shot.roar = 0.4;
    shot.roar_tau = 0.01;
    shot.roar_lp1 = 900.0;
    shot.roar_sweep = 0.004;
    shot.drive = 1.5;
    let templates: Vec<Vec<f32>> = (0..4).map(|k| boom(0.04, sub(seed, 10 + k), &shot)).collect();
    let mut t = 0.0f32;
    let mut i = 0u64;
    while t < burst_end {
        let rate = 75.0 * (1.0 - 0.45 * libm::expf(-t / 0.04));
        let g = rr(seed, 2 * i, 0.85, 1.0) * gate(t, 0.0, burst_end, 0.03);
        add(&mut out, &templates[(i % 4) as usize], t, g);
        t += (1.0 / rate) * rr(seed, 2 * i + 1, 0.97, 1.03);
        i += 1;
    }
    // Continuous body of the burst.
    let roar = lp4(&pinkn(sec, sub(seed, 1)), 2_500.0);
    add(&mut out, &norm(shape(roar, move |t| gate(t, 0.0, burst_end, 0.04))), 0.0, 0.3);
    let low = rumble(sec, sub(seed, 2), 320.0, 0.02, 0.15, (sec / 6.0).max(0.1), 0.3);
    add(&mut out, &norm(shape(low, move |t| if t < burst_end { 0.7 } else { libm::expf(-(t - burst_end) / 0.18) })), 0.0, 0.35);
    // Drive motor spinning the barrels down after the burst.
    let be = burst_end;
    let whine = osc(sec, move |t| if t < be { 300.0 } else { glide(t - be, 300.0, 90.0, 0.18) }, &[(1.0, 1.0), (2.0, 0.4), (3.0, 0.2)]);
    let whine = lp4(&whine, 1_500.0);
    add(&mut out, &norm(shape(whine, move |t| gate(t, 0.0, be + 0.4, 0.06))), 0.0, 0.06);
    sat(norm(out), 1.4)
}

/// Missile launch from a canister (anti-ship) or a vertical cell (`sam`): ignition pop, venting gas, the
/// booster's crackling roar, and a recession whose level and brightness fall with distance.
fn missile_launch(sec: f32, seed: u64, sam: bool) -> Vec<f32> {
    let mut out = silence(sec);
    let mut pop = Boom::BASE;
    pop.nwave = if sam { 0.8 } else { 0.6 };
    pop.nwave_s = 0.0013;
    pop.crack = if sam { 0.8 } else { 0.5 };
    pop.crack_tau = if sam { 0.008 } else { 0.011 };
    pop.body_f0 = if sam { 190.0 } else { 150.0 };
    pop.body_f1 = if sam { 70.0 } else { 60.0 };
    pop.body_tau = if sam { 0.06 } else { 0.08 };
    pop.sub_f = if sam { 60.0 } else { 55.0 };
    pop.sub_tau = if sam { 0.15 } else { 0.2 };
    pop.roar = 0.0;
    add(&mut out, &boom(0.8f32.min(sec), sub(seed, 1), &pop), 0.0, 0.75);
    // Recession law: amplitude falls roughly as 1/distance as the missile accelerates away.
    let d0 = if sam { 0.35 } else { 0.6 };
    let recede = move |t: f32| 1.0 / (1.0 + (t / d0) * (t / d0));
    let onset = if sam { 0.02 } else { 0.04 };
    // Booster roar: bright at first, darkening with range; fast irregular tearing on a VLS launch.
    let (lp0, lp_tau) = if sam { (8_000.0, 0.8) } else { (7_000.0, 1.2) };
    let lp_end = if sam { 1_200.0 } else { 900.0 };
    let src = pinkn(sec, sub(seed, 2));
    let w = white(sec, sub(seed, 3));
    let mut mixsrc = src;
    for (a, b) in mixsrc.iter_mut().zip(w.iter()) {
        *a = *a * 0.7 + *b * 0.12;
    }
    let roar = lp(&mixsrc, sub(seed, 4), move |t| glide(t, lp0, lp_end, lp_tau), 0.7);
    let tear = wobble(sec, sub(seed, 5), 40.0);
    let tear_depth = if sam { 0.4 } else { 0.2 };
    let mut roar = shape(roar, move |t| smooth(t / onset) * recede(t));
    for (v, m) in roar.iter_mut().zip(tear.iter()) {
        *v *= 1.0 + tear_depth * m;
    }
    add(&mut out, &norm(roar), 0.01, 1.0);
    // Exhaust crackle: sparse positive spikes, densest close in.
    let n_crackle = (sec * if sam { 260.0 } else { 180.0 }) as usize;
    let times = scatter(sub(seed, 6), n_crackle, 0.03, sec, recede);
    let mut crackle = silence(sec);
    for (i, &t) in times.iter().enumerate() {
        tick_grain(&mut crackle, t, rr(seed, 7_000 + i as u64, 0.0006, 0.0018), 6_000.0 * recede(t) + 1_500.0, rr(seed, 9_000 + i as u64, 0.3, 1.0) * recede(t), sub(seed, 20_000 + i as u64));
    }
    add(&mut out, &norm(crackle), 0.0, 0.22);
    // Low rumble of the motor.
    let low = rumble(sec, sub(seed, 7), 200.0, 0.0, 0.05, 10.0, 0.3);
    add(&mut out, &norm(shape(low, recede)), 0.0, 0.5);
    // Canister door and gas venting (anti-ship), or the hollow roar of the cell's gas uptake (VLS).
    if sam {
        let hollow = bp2(&pinkn(0.5, sub(seed, 8)), 600.0, 3.0);
        add(&mut out, &norm(shape(hollow, |t| ad(t, 0.01, 0.1))), 0.0, 0.3);
    } else {
        let vent = noise_band(0.6, sub(seed, 8), 2_500.0, 7_500.0);
        add(&mut out, &norm(shape(vent, |t| ad(t, 0.004, 0.08))), 0.0, 0.3);
        add(&mut out, &metal_hit(0.6, sub(seed, 9), 330.0, 0.1, 5, 0.002), 0.0, 0.25);
    }
    sat(norm(out), 1.5)
}

/// Loop on a flying missile body: turbojet whine with a periodic flutter, broadband roar and rumble.
fn missile_motor(sec: f32, seed: u64, period: f32) -> Vec<f32> {
    let mut out = silence(sec);
    let fw = snap_hz(2_400.0, period);
    let fv = snap_hz(5.0, period);
    let fc = snap_hz(900.0, period);
    let ff = snap_hz(13.0, period);
    // Whine with a slight periodic vibrato (whole cycles, so the loop seams).
    let whine = osc(sec, move |t| fw * (1.0 + 0.003 * libm::sinf(core::f32::consts::TAU * fv * t)), &[(1.0, 1.0), (2.0, 0.25)]);
    add(&mut out, &norm(whine), 0.0, 0.18);
    let comp = osc(sec, move |_| fc, &harmonics(4, 1.2));
    add(&mut out, &norm(comp), 0.0, 0.08);
    let roar = lp4(&pinkn(sec, sub(seed, 1)), 3_000.0);
    let roar = shape(roar, move |t| 1.0 + 0.15 * libm::sinf(core::f32::consts::TAU * ff * t));
    add(&mut out, &norm(roar), 0.0, 0.8);
    let low = lp4(&brownn(sec, sub(seed, 2)), 400.0);
    add(&mut out, &norm(low), 0.0, 0.5);
    norm(out)
}

/// Torpedo tube: door clank, the impulse air blast, venting hiss, the weapon striking the water and the
/// bubbles of the spent impulse air.
fn torpedo_launch(sec: f32, seed: u64) -> Vec<f32> {
    let mut out = silence(sec);
    add(&mut out, &metal_hit(0.8f32.min(sec), sub(seed, 1), 310.0, 0.12, 6, 0.002), 0.0, 0.4);
    let mut b = Boom::BASE;
    b.nwave = 0.0;
    b.crack = 0.3;
    b.crack_tau = 0.02;
    b.crack_lo = 300.0;
    b.crack_hi = 3_000.0;
    b.body_f0 = 110.0;
    b.body_f1 = 40.0;
    b.body_drop = 0.05;
    b.body_tau = 0.15;
    b.sub_f = 45.0;
    b.sub_tau = 0.25;
    b.roar = 0.7;
    b.roar_tau = 0.12;
    b.roar_lp0 = 2_500.0;
    b.roar_lp1 = 300.0;
    b.drive = 1.8;
    add(&mut out, &boom(1.2f32.min(sec), sub(seed, 2), &b), 0.05, 1.0);
    let hiss = bp2(&white(0.9, sub(seed, 3)), 1_500.0, 0.8);
    add(&mut out, &norm(shape(hiss, |t| ad(t, 0.01, 0.18))), 0.05, 0.4);
    let entry = sec * 0.22;
    add(&mut out, &splash((sec - entry).max(0.1), sub(seed, 4), 0.45), entry, 0.6);
    let mut bub = silence(sec);
    let times = scatter(sub(seed, 5), 45, entry + 0.05, sec * 0.9, |t| 1.0 - 0.5 * t / sec);
    for (i, &t) in times.iter().enumerate() {
        let i = i as u64;
        drop_grain(&mut bub, t, rr(seed, 300 + i, 400.0, 1_400.0), rr(seed, 400 + i, 1.2, 1.8), rr(seed, 500 + i, 0.006, 0.014), rr(seed, 600 + i, 0.3, 1.0));
    }
    add(&mut out, &norm(bub), 0.0, 0.22);
    let gurgle = rumble(sec, sub(seed, 6), 300.0, entry, 0.2, sec * 0.3, 0.8);
    add(&mut out, &gurgle, 0.0, 0.25);
    norm(out)
}

/// Depth charge (Act 3): the release clunk, the drum rolling and bumping along the rail, then the splash.
fn depth_charge_roll(sec: f32, seed: u64) -> Vec<f32> {
    let mut out = silence(sec);
    add(&mut out, &metal_hit(0.8f32.min(sec), sub(seed, 1), 180.0, 0.1, 6, 0.004), 0.0, 0.6);
    let roll_end = sec * 0.4;
    let ex = shape(brownn(sec, sub(seed, 2)), move |t| gate(t, 0.04, roll_end, 0.05));
    let bumps = 7.0f32;
    let rolled = modal(&ex, &[mode(220.0, 0.05, 1.0), mode(410.0, 0.04, 0.6), mode(650.0, 0.03, 0.4)]);
    let rolled = shape(rolled, move |t| 0.4 + 0.6 * libm::powf(0.5 + 0.5 * libm::sinf(core::f32::consts::TAU * bumps * t), 4.0));
    add(&mut out, &norm(rolled), 0.0, 0.5);
    clatter(&mut out, sub(seed, 3), 8, 0.05, roll_end, 500.0, 1_500.0, 0.02, 0.05, 0.2);
    let drop_at = roll_end + 0.08;
    add(&mut out, &metal_hit(0.4, sub(seed, 4), 260.0, 0.06, 4, 0.003), roll_end, 0.3);
    add(&mut out, &splash((sec - drop_at).max(0.1), sub(seed, 5), 0.5), drop_at, 0.9);
    norm(out)
}

/// Mine off the rail: latch clack, heavy clunk, a short scrape, then a heavy splash and its bubbles.
fn mine_drop(sec: f32, seed: u64) -> Vec<f32> {
    let mut out = silence(sec);
    clink_grain(&mut out, 0.0, 1_800.0, 0.03, 0.3, sub(seed, 1));
    add(&mut out, &metal_hit(0.8f32.min(sec), sub(seed, 2), 150.0, 0.15, 6, 0.004), 0.01, 0.6);
    let scrape = bp(&white(0.35, sub(seed, 3)), sub(seed, 4), |t| 900.0 - 600.0 * t, 2.0);
    let rough = wobble(0.35, sub(seed, 5), 60.0);
    let scrape = mul(shape(scrape, |t| gate(t, 0.02, 0.3, 0.03)), &rough);
    add(&mut out, &norm(scrape), 0.0, 0.3);
    let at = sec * 0.25;
    add(&mut out, &splash((sec - at).max(0.1), sub(seed, 6), 0.55), at, 1.0);
    norm(out)
}

/// Chaff and flare mortar (`smoke == false`): a sharp tube pop, a receding whoosh and the cartridge bursting
/// out with a flutter. Smoke: a softer pop and a hissing canister.
fn mortar(sec: f32, seed: u64, smoke: bool) -> Vec<f32> {
    let mut out = silence(sec);
    let mut b = Boom::BASE;
    b.nwave = if smoke { 0.5 } else { 0.7 };
    b.nwave_s = 0.001;
    b.crack = if smoke { 0.4 } else { 0.6 };
    b.crack_tau = 0.006;
    b.crack_lo = 800.0;
    b.body_f0 = if smoke { 160.0 } else { 220.0 };
    b.body_f1 = if smoke { 60.0 } else { 80.0 };
    b.body_drop = 0.015;
    b.body_tau = if smoke { 0.06 } else { 0.05 };
    b.sub = 0.6;
    b.sub_f = 70.0;
    b.sub_tau = 0.08;
    b.roar = 0.5;
    b.roar_tau = 0.05;
    b.roar_lp1 = 500.0;
    b.drive = 1.8;
    add(&mut out, &boom(0.6f32.min(sec), sub(seed, 1), &b), 0.0, 1.0);
    let whoosh = bp(&white(0.7, sub(seed, 2)), sub(seed, 3), |t| glide(t, 2_500.0, 900.0, 0.2), 1.2);
    add(&mut out, &norm(shape(whoosh, |t| ad(t, 0.01, 0.15))), 0.01, 0.35);
    if smoke {
        let at = sec * 0.33;
        let mut p = Boom::BASE;
        p.nwave = 0.3;
        p.crack = 0.2;
        p.body_f0 = 120.0;
        p.body_f1 = 50.0;
        p.body_tau = 0.05;
        p.sub = 0.3;
        p.roar = 0.0;
        add(&mut out, &boom(0.5, sub(seed, 4), &p), at, 0.4);
        let len = (sec - at).max(0.1);
        let tau = len / 4.0;
        let hiss = bp2(&white(len, sub(seed, 5)), 2_500.0, 0.6);
        add(&mut out, &norm(shape(hiss, move |t| ad(t, 0.02, tau))), at, 0.4);
        let low = bp2(&pinkn(len, sub(seed, 6)), 900.0, 0.7);
        add(&mut out, &norm(shape(low, move |t| ad(t, 0.03, tau))), at, 0.2);
    } else {
        let at = sec * 0.62;
        let mut p = Boom::BASE;
        p.nwave = 0.4;
        p.nwave_s = 0.0008;
        p.crack = 0.5;
        p.crack_tau = 0.004;
        p.crack_hi = 3_000.0;
        p.body_f0 = 300.0;
        p.body_f1 = 120.0;
        p.body_drop = 0.01;
        p.body_tau = 0.03;
        p.sub = 0.0;
        p.roar = 0.3;
        p.roar_tau = 0.03;
        add(&mut out, &boom(0.3, sub(seed, 4), &p), at, 0.35);
        let len = (sec - at).max(0.1);
        let flutter = wobble(len, sub(seed, 5), 30.0);
        let f = noise_band(len, sub(seed, 6), 2_000.0, 6_000.0);
        let f = mul(shape(f, |t| ad(t, 0.005, 0.15)), &flutter);
        add(&mut out, &norm(f), at, 0.15);
    }
    norm(out)
}

/// A shell passing overhead: the shock wave's crack, then a whistle and tearing roar that peak at closest
/// approach and drop in pitch as the shell recedes.
fn shell_passby(sec: f32, seed: u64) -> Vec<f32> {
    const C: f32 = 343.0;
    let t0 = sec * 0.36;
    let (d, v, v_pitch) = (25.0f32, 420.0f32, 120.0f32);
    let r = move |t: f32| libm::sqrtf((v * (t - t0)) * (v * (t - t0)) + d * d);
    let doppler = move |t: f32| {
        let x = v * (t - t0);
        C / (C + v_pitch * x / r(t))
    };
    // Supersonic: the approach is heard only faintly ahead of the shock.
    let amp = move |t: f32| (d / r(t)) * if t < t0 { 0.5 } else { 1.0 };
    let mut out = silence(sec);
    let tc = t0 - 0.06;
    add(&mut out, &n_wave(0.0012), tc, 0.8);
    let cr = noise_band(0.05, sub(seed, 1), 1_500.0, 8_000.0);
    add(&mut out, &norm(shape(cr, |t| ad(t, 0.0002, 0.004))), tc, 0.5);
    let whistle = osc(sec, move |t| 900.0 * doppler(t), &[(1.0, 1.0), (2.0, 0.3)]);
    add(&mut out, &norm(shape(whistle, amp)), 0.0, 0.25);
    let tear = bp(&pinkn(sec, sub(seed, 2)), sub(seed, 3), move |t| 600.0 * doppler(t), 1.2);
    add(&mut out, &norm(shape(tear, amp)), 0.0, 0.8);
    let rush = lp4(&brownn(sec, sub(seed, 4)), 500.0);
    add(&mut out, &norm(shape(rush, move |t| amp(t) * amp(t).min(1.0))), 0.0, 0.4);
    let end = sec;
    norm(shape(out, move |t| smooth((end - t) / (end * 0.15))))
}

/// Loop while a mount slews: motor hum, hydraulic whine, gear mesh and low rumble, all periodic in the loop.
fn turret_train(sec: f32, seed: u64, period: f32) -> Vec<f32> {
    let mut out = silence(sec);
    let fm = snap_hz(100.0, period);
    let fw = snap_hz(740.0, period);
    let ft = snap_hz(37.0, period);
    let hum = lp4(&osc(sec, move |_| fm, &[(1.0, 1.0), (2.0, 0.5), (3.0, 0.3), (4.0, 0.15)]), 900.0);
    add(&mut out, &norm(hum), 0.0, 0.5);
    let whine = osc(sec, move |_| fw, &[(1.0, 1.0), (2.0, 0.3)]);
    add(&mut out, &norm(whine), 0.0, 0.22);
    let mesh = bp2(&white(sec, sub(seed, 1)), 1_800.0, 3.0);
    let mesh = shape(mesh, move |t| libm::powf(0.5 + 0.5 * libm::sinf(core::f32::consts::TAU * ft * t), 3.0));
    add(&mut out, &norm(mesh), 0.0, 0.25);
    let low = lp4(&brownn(sec, sub(seed, 2)), 200.0);
    add(&mut out, &norm(low), 0.0, 0.3);
    norm(out)
}

/// Drone launch: pneumatic catapult thwack, shuttle rattle and end-stop clank, then the drone's engine buzz
/// rising as it accelerates away and fading with a Doppler drop.
fn drone_launch(sec: f32, seed: u64) -> Vec<f32> {
    let mut out = silence(sec);
    let thwack = bp2(&white(0.5, sub(seed, 1)), 1_200.0, 0.7);
    add(&mut out, &norm(shape(thwack, |t| ad(t, 0.003, 0.08))), 0.0, 0.7);
    let body = osc(0.4, |t| glide(t, 140.0, 60.0, 0.03), &[(1.0, 1.0), (2.0, 0.2)]);
    add(&mut out, &norm(shape(body, |t| ad(t, 0.002, 0.05))), 0.0, 0.6);
    clatter(&mut out, sub(seed, 2), 10, 0.0, 0.22, 700.0, 2_200.0, 0.01, 0.03, 0.25);
    add(&mut out, &metal_hit(0.6, sub(seed, 3), 400.0, 0.08, 5, 0.002), 0.22, 0.4);
    // The engine runs up as the drone leaves the rail; its pitch falls with the receding Doppler shift.
    let f = move |t: f32| (110.0 + 60.0 * (1.0 - libm::expf(-t / 0.3))) * 343.0 / (343.0 + 30.0 * (1.0 - libm::expf(-t / 0.4)));
    let fire = engine_pulses(sec, sub(seed, 4), f, 1, 0.08, 0.0015);
    let mut buzz = norm(bp2(&fire, 700.0, 1.4));
    add(&mut buzz, &norm(lp4(&fire, 450.0)), 0.0, 0.6);
    add(&mut buzz, &norm(bp2(&fire, 1_800.0, 2.0)), 0.0, 0.3);
    add(&mut out, &norm(shape(buzz, |t| smooth(t / 0.05) / (1.0 + (t / 0.6) * (t / 0.6)))), 0.05, 0.5);
    let end = sec;
    norm(shape(out, move |t| smooth((end - t) / 0.2)))
}

/// Loop on a drone body: a small two-stroke's firing buzz through exhaust formants, intake noise and the
/// propeller's blade-pass whirr.
fn drone_motor(sec: f32, seed: u64, period: f32) -> Vec<f32> {
    let mut out = silence(sec);
    // A single-cylinder two-stroke near 5,700 rpm. Successive firings differ, so the pulse pattern is drawn
    // over four firings and repeats; its rate is snapped so the pattern repeats a whole number of times.
    let pattern = snap_hz(95.0 / 4.0, period);
    let fb = snap_hz(126.0, period);
    let fire = engine_pulses(sec, sub(seed, 3), move |_| pattern * 4.0, 4, 0.0, 0.0018);
    let ex = norm(bp2(&fire, 700.0, 1.5));
    let ex2 = norm(bp2(&fire, 1_800.0, 2.0));
    add(&mut out, &ex, 0.0, 0.7);
    add(&mut out, &ex2, 0.0, 0.3);
    add(&mut out, &norm(lp4(&fire, 400.0)), 0.0, 0.4);
    let intake = bp2(&pinkn(sec, sub(seed, 1)), 1_000.0, 0.7);
    add(&mut out, &norm(intake), 0.0, 0.15);
    let whirr = lp4(&white(sec, sub(seed, 2)), 1_500.0);
    let whirr = shape(whirr, move |t| libm::powf(0.5 + 0.5 * libm::sinf(core::f32::consts::TAU * fb * t), 2.0));
    add(&mut out, &norm(whirr), 0.0, 0.2);
    norm(out)
}

/// Strike aircraft pass (Act 2): jet roar darkened by distance, a Doppler-dropping engine whine, and the
/// sea-reflection comb that sweeps as the aircraft passes.
fn aircraft_pass(sec: f32, seed: u64) -> Vec<f32> {
    const C: f32 = 343.0;
    let t0 = sec * 0.46;
    let (d, v, v_pitch) = (60.0f32, 200.0f32, 120.0f32);
    let r = move |t: f32| libm::sqrtf((v * (t - t0)) * (v * (t - t0)) + d * d);
    let near = move |t: f32| d / r(t);
    let doppler = move |t: f32| C / (C + v_pitch * v * (t - t0) / r(t));
    let mut src = pinkn(sec, sub(seed, 1));
    let w = white(sec, sub(seed, 2));
    for (a, b) in src.iter_mut().zip(w.iter()) {
        *a = *a * 0.8 + *b * 0.1;
    }
    let roar = lp(&src, sub(seed, 3), move |t| (9_000.0 * libm::sqrtf(near(t))).clamp(600.0, 9_000.0), 0.7);
    let roar = shape(roar, near);
    // Ground reflection: a delayed copy whose delay sweeps with geometry (the classic fly-by flanging).
    let refl = vdelay(&roar, move |t| 0.0008 + 0.012 * near(t));
    let mut jet = roar;
    for (a, b) in jet.iter_mut().zip(refl.iter()) {
        *a += 0.6 * b;
    }
    let mut out = silence(sec);
    add(&mut out, &norm(jet), 0.0, 1.0);
    let whine = osc(sec, move |t| 2_200.0 * doppler(t), &[(1.0, 1.0), (1.5, 0.2), (2.0, 0.1)]);
    add(&mut out, &norm(shape(whine, move |t| near(t) * near(t))), 0.0, 0.15);
    let low = lp4(&brownn(sec, sub(seed, 4)), 250.0);
    add(&mut out, &norm(shape(low, move |t| libm::powf(near(t), 0.7))), 0.0, 0.6);
    let end = sec;
    norm(shape(out, move |t| smooth(t / 0.3) * smooth((end - t) / 0.3)))
}

/// Loop on a helicopter (Act 2): four-blade main rotor slap at 20 Hz, rotor wash, turbine whine and the tail
/// rotor's buzz. Slaps fall exactly on the blade-pass grid, so the loop repeats them seamlessly.
fn helicopter_rotor(sec: f32, seed: u64, period: f32) -> Vec<f32> {
    let mut out = silence(sec);
    let bpf = snap_hz(20.0, period);
    let slap_len = 0.06;
    // Four slap variants, one per blade, so the 4-blade pattern repeats every revolution.
    let slaps: Vec<Vec<f32>> = (0..4)
        .map(|k| {
            let ex = shape(noise_band(slap_len, sub(seed, 10 + k), 150.0, 1_200.0), |t| ad(t, 0.001, 0.008));
            let m = modal(&ex, &[mode(90.0, 0.02, 1.0), mode(210.0, 0.012, 0.6), mode(420.0, 0.008, 0.3)]);
            norm(m)
        })
        .collect();
    const BLADE: [f32; 4] = [1.0, 0.85, 0.95, 0.8];
    let n = (sec * bpf) as usize + 1;
    for k in 0..n {
        add(&mut out, &slaps[k % 4], k as f32 / bpf, BLADE[k % 4]);
    }
    let wash = lp4(&brownn(sec, sub(seed, 1)), 300.0);
    let wash = shape(wash, move |t| 0.6 + 0.4 * libm::powf(0.5 + 0.5 * libm::cosf(core::f32::consts::TAU * bpf * t), 2.0));
    add(&mut out, &norm(wash), 0.0, 0.5);
    let ft = snap_hz(3_150.0, period);
    add(&mut out, &norm(osc(sec, move |_| ft, &[(1.0, 1.0)])), 0.0, 0.06);
    add(&mut out, &norm(lp4(&pinkn(sec, sub(seed, 2)), 2_500.0)), 0.0, 0.15);
    let ftr = snap_hz(92.0, period);
    add(&mut out, &norm(lp4(&osc(sec, move |_| ftr, &harmonics(6, 1.0)), 1_200.0)), 0.0, 0.15);
    norm(out)
}

/// Loop on a RHIB: an outboard's high-revving firing buzz through exhaust formants with a periodic burble,
/// hull slap on the chop and propeller hiss.
fn rhib_outboard(sec: f32, seed: u64, period: f32) -> Vec<f32> {
    let mut out = silence(sec);
    // A V6 four-stroke near 4,900 rpm: six firings per two revolutions. The cycle rate is snapped so the
    // six-cylinder pattern repeats a whole number of times in the loop.
    let cycle = snap_hz(245.0 / 6.0, period);
    let fire = engine_pulses(sec, sub(seed, 4), move |_| cycle * 6.0, 6, 0.0, 0.0012);
    let fb = snap_hz(8.5, period);
    let fire = shape(fire, move |t| 0.85 + 0.15 * libm::sinf(core::f32::consts::TAU * fb * t));
    add(&mut out, &norm(bp2(&fire, 450.0, 1.2)), 0.0, 0.6);
    add(&mut out, &norm(bp2(&fire, 1_200.0, 1.8)), 0.0, 0.35);
    add(&mut out, &norm(bp2(&fire, 2_600.0, 2.5)), 0.0, 0.15);
    // Hull slapping the chop: a few soft low thumps.
    let mut slap = silence(sec);
    let times = scatter(sub(seed, 1), (sec * 2.5) as usize, 0.0, sec, |_| 1.0);
    for (i, &t) in times.iter().enumerate() {
        let i = i as u64;
        let th = lp4(&shape(white(0.25, sub(seed, 100 + i)), |t| ad(t, 0.01, 0.04)), 500.0);
        add(&mut slap, &norm(th), t, rr(seed, 200 + i, 0.4, 1.0));
    }
    add(&mut out, &norm(slap), 0.0, 0.35);
    add(&mut out, &norm(lp4(&brownn(sec, sub(seed, 2)), 600.0)), 0.0, 0.3);
    add(&mut out, &norm(hp2(&pinkn(sec, sub(seed, 3)), 1_500.0)), 0.0, 0.1);
    norm(out)
}

#[cfg(test)]
mod tests {
    use crate::{render, SoundId, WeaponSfx};

    #[test]
    fn every_weapon_renders_clean() {
        for w in WeaponSfx::ALL {
            let id = SoundId::Weapon(w);
            let spec = id.spec();
            let pcm = render(id);
            assert_eq!(pcm.frames(), crate::render::spec_frames(&spec), "{w:?}");
            let peak = pcm.samples.iter().map(|&s| (s as i32).abs()).max().unwrap();
            // Levels are loudness targets (`analysis::target`); the peak only has to sit under the ceiling.
            let ceiling = libm::powf(10.0, spec.peak_dbfs.min(crate::render::CEILING_DBFS) / 20.0) * 32767.0;
            assert!(peak as f32 <= ceiling + 2.0 && peak as f32 > ceiling * 0.01, "{w:?} peak {peak} ceiling {ceiling}");
            // Not a near-silent file: a meaningful share of the energy above -40 dB of peak.
            let loud = pcm.samples.iter().filter(|&&s| (s as i32).abs() > peak / 100).count();
            assert!(loud > pcm.samples.len() / 20, "{w:?} mostly silent");
            if spec.playback == crate::Playback::Loop {
                let s = &pcm.samples;
                let mean_step = s.windows(2).map(|p| (p[1] as f32 - p[0] as f32).abs()).sum::<f32>() / (s.len() - 1) as f32;
                let wrap = (s[0] as f32 - s[s.len() - 1] as f32).abs();
                assert!(wrap < mean_step * 8.0 + 64.0, "{w:?} seam {wrap} vs {mean_step}");
            } else {
                // One-shots decay: the last 5% is far quieter than the peak.
                let tail = &pcm.samples[pcm.samples.len() * 95 / 100..];
                let tail_peak = tail.iter().map(|&s| (s as i32).abs()).max().unwrap();
                assert!(tail_peak < peak / 4, "{w:?} tail {tail_peak} peak {peak}");
            }
        }
    }

    #[test]
    fn weapons_are_deterministic() {
        for w in [WeaponSfx::GunAuto, WeaponSfx::Ciws, WeaponSfx::TurretTrain] {
            assert_eq!(crate::render_wav(SoundId::Weapon(w)), crate::render_wav(SoundId::Weapon(w)));
        }
    }
}
