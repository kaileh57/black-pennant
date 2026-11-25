//! Unit `audio-impacts`. Splashes by size, ricochet, penetration, autocannon strikes, explosions, cook-off,
//! underwater and air bursts, collision, grounding, hull groan and break, debris, fire loop and ignition,
//! flooding loop, breach rush, pump loop and steam hiss. DESIGN.md L1239-1247, L557 (splash), L578-597 (fuses
//! and bursts), L630-686 (flooding, fire, cook-off, splitting).
//!
//! Layering follows the physics of each event: splashes are a slap, a cavity plunk, the column's whoosh and
//! the rain-down of the column; hits are a crack into plate modes with a crunch; bursts are the shared blast
//! (`kit::explosion`) with debris and a rolling tail; structure sounds are stick-slip excitation into low
//! hull modes. Loops (fire, flooding, pump) render [`render_seconds`] so `finish` can seam them, and the
//! pump's tones hold whole cycles in the loop.
use crate::render::kit::*;
use crate::render::render_seconds;
use crate::{ImpactSfx, SoundSpec};

/// Unit: `audio-impacts`.
pub fn render(id: ImpactSfx, spec: &SoundSpec, seed: u64) -> Vec<f32> {
    let sec = render_seconds(spec).max(0.05);
    let period = spec.duration_s.max(0.05);
    let x = match id {
        ImpactSfx::SplashSmall => splash(sec, sub(seed, 1), 0.1),
        ImpactSfx::SplashMedium => splash(sec, sub(seed, 1), 0.45),
        ImpactSfx::SplashLarge => splash(sec, sub(seed, 1), 1.0),
        ImpactSfx::Ricochet => ricochet(sec, seed),
        ImpactSfx::PenetrateHit => plate_hit(sec, seed, false),
        ImpactSfx::AutocannonHit => plate_hit(sec, seed, true),
        ImpactSfx::ExplosionSmall => explosion(sec, sub(seed, 1), 0.35, false),
        ImpactSfx::ExplosionLarge => explosion(sec, sub(seed, 1), 1.0, false),
        ImpactSfx::CookOff => cook_off(sec, seed),
        ImpactSfx::UnderwaterBurst => underwater_burst(sec, seed),
        ImpactSfx::AirBurst => explosion(sec, sub(seed, 1), 0.3, true),
        ImpactSfx::Collision => collision(sec, seed),
        ImpactSfx::Grounding => grounding(sec, seed),
        ImpactSfx::HullGroan => hull_groan(sec, seed),
        ImpactSfx::HullBreak => hull_break(sec, seed),
        ImpactSfx::DebrisFall => debris_fall(sec, seed),
        ImpactSfx::FireLoop => fire_loop(sec, seed),
        ImpactSfx::FireIgnite => fire_ignite(sec, seed),
        ImpactSfx::FloodingLoop => flooding_loop(sec, seed),
        ImpactSfx::BreachRush => breach_rush(sec, seed),
        ImpactSfx::PumpLoop => pump_loop(sec, seed, period),
        ImpactSfx::SteamHiss => steam_hiss(sec, seed),
    };
    let mut x = x;
    x.resize(frames(sec), 0.0);
    x
}

/// Ricochet: a bright glancing strike, then the tumbling round's whine dropping away with a flutter.
fn ricochet(sec: f32, seed: u64) -> Vec<f32> {
    let mut out = silence(sec);
    add(&mut out, &metal_hit(0.4f32.min(sec), sub(seed, 1), 900.0, 0.04, 4, 0.0008), 0.0, 0.5);
    let cr = noise_band(0.03, sub(seed, 2), 2_000.0, 8_000.0);
    add(&mut out, &norm(shape(cr, |t| ad(t, 0.0002, 0.003))), 0.0, 0.6);
    let f0 = rr(seed, 3, 2_400.0, 2_800.0);
    let flutter = rr(seed, 4, 28.0, 40.0);
    let fr = move |t: f32| f0 * (0.62 + 0.38 * libm::expf(-t / 0.25));
    let tau = (sec / 3.5).max(0.05);
    let whine = osc(sec, fr, &[(1.0, 1.0), (2.0, 0.12)]);
    let whine = shape(whine, move |t| ad(t - 0.01, 0.01, tau) * (0.65 + 0.35 * libm::sinf(core::f32::consts::TAU * flutter * t)));
    add(&mut out, &norm(whine), 0.0, 0.45);
    let air = bp(&white(sec, sub(seed, 5)), sub(seed, 6), fr, 4.0);
    add(&mut out, &norm(shape(air, move |t| ad(t - 0.01, 0.01, tau))), 0.0, 0.2);
    norm(out)
}

/// A round striking plating. Penetration: crack, a heavy plate clang, a tearing crunch, a thud and falling
/// fragments. Autocannon: a smaller, higher smack with sparks.
fn plate_hit(sec: f32, seed: u64, small: bool) -> Vec<f32> {
    let mut out = silence(sec);
    add(&mut out, &n_wave(if small { 0.0004 } else { 0.0006 }), 0.0, if small { 0.4 } else { 0.7 });
    let ct = if small { 0.0025 } else { 0.004 };
    let cr = noise_band(0.04, sub(seed, 1), if small { 1_800.0 } else { 1_200.0 }, 8_500.0);
    add(&mut out, &norm(shape(cr, move |t| ad(t, 0.0002, ct))), 0.0, 0.8);
    // Plating on frames is well damped: a clang, not a bell.
    let (f0, tau) = if small { (rr(seed, 2, 440.0, 520.0), sec / 6.0) } else { (rr(seed, 2, 195.0, 225.0), sec / 6.0) };
    add(&mut out, &metal_hit(sec, sub(seed, 3), f0, tau, if small { 5 } else { 10 }, 0.003), 0.0, if small { 0.6 } else { 0.7 });
    let (b0, b1, bt) = if small { (180.0, 90.0, 0.025) } else { (110.0, 55.0, 0.06) };
    let th = osc(bt * 8.0, move |t| glide(t, b0, b1, bt * 0.5), &[(1.0, 1.0), (2.0, 0.2)]);
    add(&mut out, &norm(shape(th, move |t| ad(t, 0.001, bt))), 0.0, if small { 0.35 } else { 0.6 });
    if !small {
        // Tearing crunch of the plate giving way.
        let ex = white(0.3, sub(seed, 4));
        let rough = wobble(0.3, sub(seed, 5), 90.0);
        let ex = mul(shape(ex, |t| ad(t, 0.002, 0.06)), &rough);
        let crunch = modal(&ex, &[mode(700.0, 0.02, 1.0), mode(1_250.0, 0.018, 0.8), mode(1_900.0, 0.015, 0.6), mode(2_700.0, 0.012, 0.4)]);
        add(&mut out, &norm(crunch), 0.003, 0.5);
        clatter(&mut out, sub(seed, 6), 20, 0.08, sec * 0.7, 1_800.0, 4_500.0, 0.03, 0.08, 0.15);
    } else {
        clatter(&mut out, sub(seed, 6), 5, 0.01, sec * 0.5, 2_000.0, 5_000.0, 0.01, 0.03, 0.1);
    }
    sat(norm(out), 1.3)
}

/// Magazine cook-off: a catastrophic primary blast, then secondary detonations thinning out, cartridges
/// popping in the fire, and the roar of burning propellant.
fn cook_off(sec: f32, seed: u64) -> Vec<f32> {
    let mut out = silence(sec);
    add(&mut out, &explosion(sec.min(4.0), sub(seed, 1), 1.0, false), 0.0, 1.0);
    let times = scatter(sub(seed, 2), 9, 0.6, sec * 0.82, |t| libm::expf(-(t - 0.6) / 2.0));
    for (i, &t) in times.iter().enumerate() {
        let i = i as u64;
        let size = rr(seed, 100 + i, 0.12, 0.45) * (1.0 - 0.5 * t / sec);
        let len = (sec - t).clamp(0.2, 2.0);
        add(&mut out, &secondary_blast(len, sub(seed, 200 + i), size), t, rr(seed, 300 + i, 0.25, 0.55) * (1.0 - 0.4 * t / sec));
    }
    // Cartridges cooking off: sharp pops with a tiny body.
    let pops = scatter(sub(seed, 3), 110, 0.3, sec * 0.95, |t| libm::expf(-t / 2.5));
    let mut p = silence(sec);
    for (i, &t) in pops.iter().enumerate() {
        let i = i as u64;
        let a = rr(seed, 400 + i, 0.2, 1.0);
        tick_grain(&mut p, t, rr(seed, 500 + i, 0.0008, 0.003), 7_000.0, a, sub(seed, 600 + i));
        drop_grain(&mut p, t, rr(seed, 700 + i, 500.0, 1_200.0), 0.7, 0.008, a * 0.4);
    }
    add(&mut out, &norm(p), 0.0, 0.3);
    // Burning propellant roar.
    let roar = lp4(&pinkn(sec, sub(seed, 4)), 700.0);
    let fl = wobble(sec, sub(seed, 5), 3.0);
    let roar = mul(shape(roar, move |t| swell(t, 0.2, 0.8, sec / 3.0)), &fl.iter().map(|m| 0.7 + 0.3 * m).collect::<Vec<f32>>());
    add(&mut out, &norm(roar), 0.0, 0.45);
    let low = rumble(sec, sub(seed, 6), 180.0, 0.1, 0.6, sec / 3.5, 0.8);
    add(&mut out, &low, 0.0, 0.45);
    norm(out)
}

/// Torpedo, mine or depth charge heard from the surface: a muffled deep thud, the shock knocking the hull,
/// the gas bubble's second pulse, then the plume erupting and raining down.
fn underwater_burst(sec: f32, seed: u64) -> Vec<f32> {
    let mut out = silence(sec);
    let mut b = Boom::BASE;
    b.nwave = 0.0;
    b.crack = 0.0;
    b.body_f0 = 70.0;
    b.body_f1 = 25.0;
    b.body_drop = 0.04;
    b.body_tau = 0.25;
    b.sub = 1.0;
    b.sub_f = 38.0;
    b.sub_tau = 0.4;
    b.roar = 0.6;
    b.roar_tau = 0.2;
    b.roar_lp0 = 800.0;
    b.roar_lp1 = 150.0;
    b.drive = 2.4;
    let thud = norm(lp4(&boom(sec.min(2.5), sub(seed, 1), &b), 400.0));
    add(&mut out, &thud, 0.0, 1.0);
    // Shock wave on the hull: a resonant knock.
    let ex = shape(noise_band(0.6, sub(seed, 2), 40.0, 1_500.0), |t| ad(t, 0.0005, 0.004));
    let knock = modal(&ex, &[mode(95.0, 0.2, 1.0), mode(160.0, 0.15, 0.7), mode(240.0, 0.1, 0.5), mode(380.0, 0.07, 0.3)]);
    add(&mut out, &norm(knock), 0.0, 0.35);
    // Bubble pulse.
    let bub = osc(1.0, |t| glide(t, 50.0, 28.0, 0.05), &[(1.0, 1.0), (2.0, 0.15)]);
    add(&mut out, &norm(lp4(&shape(bub, |t| ad(t, 0.01, 0.12)), 300.0)), 0.3, 0.5);
    // Plume eruption.
    let at = 0.4f32;
    let len = (sec - at).max(0.1);
    let plume = lp(&pinkn(len, sub(seed, 3)), sub(seed, 4), |t| 600.0 + 2_900.0 * swell(t, 0.0, 0.3, 0.5), 0.7);
    add(&mut out, &norm(shape(plume, move |t| swell(t, 0.0, 0.3, len / 3.5))), at, 0.7);
    add(&mut out, &rumble(sec, sub(seed, 5), 250.0, at, 0.3, sec / 4.0, 0.6), 0.0, 0.5);
    rain_down(&mut out, sub(seed, 6), 1.1f32.min(sec * 0.4), sec * 0.95, 900, 1.0, 0.35);
    norm(out)
}

/// Hulls colliding: a deep impact and crunch, the hull ringing, grinding and scraping as the hulls slide,
/// a low groan and fittings falling.
fn collision(sec: f32, seed: u64) -> Vec<f32> {
    let mut out = silence(sec);
    let th = osc(1.2f32.min(sec), |t| glide(t, 70.0, 35.0, 0.08), &[(1.0, 1.0), (2.0, 0.25), (3.0, 0.1)]);
    add(&mut out, &norm(shape(th, |t| ad(t, 0.003, 0.15))), 0.0, 1.0);
    let cr = bp2(&white(0.4, sub(seed, 1)), 600.0, 0.8);
    add(&mut out, &norm(shape(cr, |t| ad(t, 0.001, 0.04))), 0.0, 0.6);
    let hull_tau = sec / 4.0;
    let ex = shape(noise_band(sec, sub(seed, 2), 40.0, 3_000.0), |t| ad(t, 0.001, 0.01));
    let ring = modal(&ex, &[mode(85.0, hull_tau, 1.0), mode(140.0, hull_tau * 0.8, 0.8), mode(205.0, hull_tau * 0.6, 0.6), mode(290.0, hull_tau * 0.5, 0.5), mode(410.0, hull_tau * 0.4, 0.35), mode(560.0, hull_tau * 0.3, 0.25)]);
    add(&mut out, &norm(ring), 0.0, 0.6);
    // Grinding: stick-slip into mid modes, roughened.
    let g_end = sec * 0.8;
    let ss = stick_slip(sec, sub(seed, 3), 0.05, g_end, |t| 60.0 + 80.0 * libm::sinf(t * 2.3) * libm::sinf(t * 2.3), 0.35);
    let grind = modal(&ss, &[mode(320.0, 0.03, 1.0), mode(510.0, 0.03, 0.8), mode(790.0, 0.025, 0.6), mode(1_150.0, 0.02, 0.45), mode(1_600.0, 0.015, 0.3)]);
    let scrape = band(&white(sec, sub(seed, 4)), 400.0, 3_500.0);
    let rough = wobble(sec, sub(seed, 5), 12.0);
    let mut gr = norm(grind);
    let sc = norm(scrape);
    for ((g, s), r) in gr.iter_mut().zip(sc.iter()).zip(rough.iter()) {
        *g = (*g + 0.35 * s) * (0.6 + 0.4 * r);
    }
    let gr = shape(gr, move |t| gate(t, 0.05, g_end, 0.15) * (1.0 - 0.5 * t / sec));
    add(&mut out, &norm(gr), 0.0, 0.5);
    let gs = stick_slip(sec, sub(seed, 6), 0.2, sec * 0.9, |t| 25.0 + 20.0 * t, 0.2);
    let groan = modal(&gs, &[mode(110.0, 0.15, 1.0), mode(170.0, 0.12, 0.7), mode(260.0, 0.1, 0.5)]);
    add(&mut out, &norm(shape(groan, move |t| gate(t, 0.2, sec * 0.9, 0.3))), 0.0, 0.35);
    clatter(&mut out, sub(seed, 7), 15, 0.1, sec * 0.8, 900.0, 3_500.0, 0.02, 0.06, 0.18);
    sat(norm(out), 1.6)
}

/// Grounding: the keel striking and dragging over the bottom: jolts, a long rock scrape, gravel, and the
/// hull straining.
fn grounding(sec: f32, seed: u64) -> Vec<f32> {
    let mut out = silence(sec);
    let jolts = [0.0f32, rr(seed, 1, 0.4, 0.6), rr(seed, 2, 0.95, 1.25), rr(seed, 3, 1.6, 2.0)];
    for (k, &t) in jolts.iter().enumerate() {
        let g = 1.0 / (1.0 + k as f32 * 0.6);
        let th = osc(1.0, |t| glide(t, 60.0, 30.0, 0.06), &[(1.0, 1.0), (2.0, 0.2)]);
        add(&mut out, &norm(shape(th, |t| ad(t, 0.004, 0.12))), t, g);
        let n = lp4(&shape(white(0.6, sub(seed, 10 + k as u64)), |t| ad(t, 0.002, 0.05)), 400.0);
        add(&mut out, &norm(n), t, g * 0.5);
    }
    let src = brownn(sec, sub(seed, 4));
    let pk = pinkn(sec, sub(seed, 5));
    let mut mixsrc = src;
    for (a, b) in mixsrc.iter_mut().zip(pk.iter()) {
        *a = *a * 0.6 + *b * 0.4;
    }
    let scrape = modal(&lp4(&mixsrc, 900.0), &[mode(140.0, 0.04, 1.0), mode(230.0, 0.04, 0.8), mode(380.0, 0.03, 0.6), mode(600.0, 0.025, 0.4)]);
    let rough = wobble(sec, sub(seed, 6), 8.0);
    let scrape = mul(norm(scrape), &rough.iter().map(|r| 0.5 + 0.5 * r.abs()).collect::<Vec<f32>>());
    let tau = sec / 3.3;
    add(&mut out, &norm(shape(scrape, move |t| swell(t, 0.0, 0.6, tau))), 0.0, 0.8);
    let mut gravel = silence(sec);
    let times = scatter(sub(seed, 7), 60, 0.1, sec * 0.75, |t| libm::expf(-t / (sec * 0.4)));
    for (i, &t) in times.iter().enumerate() {
        let i = i as u64;
        clink_grain(&mut gravel, t, rr(seed, 100 + i, 300.0, 900.0), rr(seed, 200 + i, 0.01, 0.02), rr(seed, 300 + i, 0.3, 1.0), sub(seed, 400 + i));
    }
    add(&mut out, &norm(gravel), 0.0, 0.2);
    let gs = stick_slip(sec, sub(seed, 8), 0.3, sec * 0.93, |t| 20.0 + 15.0 * t, 0.25);
    let groan = modal(&gs, &[mode(75.0, 0.15, 1.0), mode(120.0, 0.12, 0.7), mode(190.0, 0.1, 0.5)]);
    add(&mut out, &norm(shape(groan, move |t| gate(t, 0.3, sec * 0.93, 0.4))), 0.0, 0.35);
    sat(norm(out), 1.5)
}

/// Structural stress: a slow bowed-steel groan whose creak rate swells and sags, with short high creaks.
fn hull_groan(sec: f32, seed: u64) -> Vec<f32> {
    let mut out = silence(sec);
    let (a, b) = (rr(seed, 1, 16.0, 22.0), rr(seed, 2, 22.0, 34.0));
    let mid = sec * 0.45;
    let ss = stick_slip(sec, sub(seed, 3), 0.0, sec * 0.95, move |t| a + b * swell(t, 0.0, mid, sec * 0.5), 0.15);
    let groan = modal(&ss, &[mode(95.0, 0.3, 1.0), mode(148.0, 0.25, 0.8), mode(232.0, 0.2, 0.55), mode(355.0, 0.15, 0.35), mode(520.0, 0.1, 0.2)]);
    let env = move |t: f32| smooth(t / 0.6) * smooth((sec * 0.97 - t) / (sec * 0.35));
    add(&mut out, &norm(shape(groan, env)), 0.0, 1.0);
    let f0 = rr(seed, 4, 58.0, 66.0);
    let moan = lp4(&osc(sec, move |t| f0 * (1.0 + 0.08 * libm::sinf(t * 1.7)), &[(1.0, 1.0), (2.0, 0.5), (3.0, 0.25)]), 400.0);
    add(&mut out, &norm(shape(moan, env)), 0.0, 0.3);
    for k in 0..3u64 {
        let t0 = rr(seed, 10 + k, 0.2, sec * 0.75);
        let len = rr(seed, 20 + k, 0.1, 0.25);
        let rate = rr(seed, 30 + k, 150.0, 300.0);
        let cs = stick_slip(sec, sub(seed, 40 + k), t0, t0 + len, move |_| rate, 0.3);
        let creak = modal(&cs, &[mode(420.0, 0.02, 1.0), mode(690.0, 0.015, 0.7), mode(1_100.0, 0.01, 0.4)]);
        add(&mut out, &norm(creak), 0.0, rr(seed, 50 + k, 0.15, 0.3));
    }
    add(&mut out, &rumble(sec, sub(seed, 5), 120.0, 0.0, 0.8, sec * 0.4, 0.5), 0.0, 0.2);
    norm(out)
}

/// A piece splitting off: a bang, steel tearing faster and higher as the crack runs, a deep groan, sharp
/// secondary snaps, and the displaced water heaving.
fn hull_break(sec: f32, seed: u64) -> Vec<f32> {
    let mut out = silence(sec);
    let mut b = Boom::BASE;
    b.nwave = 0.8;
    b.crack = 0.7;
    b.crack_tau = 0.012;
    b.crack_lo = 600.0;
    b.body_f0 = 120.0;
    b.body_f1 = 40.0;
    b.body_tau = 0.15;
    b.sub_f = 45.0;
    b.sub_tau = 0.3;
    b.roar = 0.4;
    b.roar_lp0 = 3_000.0;
    b.roar_lp1 = 300.0;
    b.roar_tau = 0.15;
    add(&mut out, &boom(1.5f32.min(sec), sub(seed, 1), &b), 0.0, 1.0);
    let tear_end = sec * 0.5;
    let ss = stick_slip(sec, sub(seed, 2), 0.08, tear_end, move |t| 180.0 + 240.0 * (t / tear_end), 0.25);
    let tear = modal(&ss, &[mode(640.0, 0.03, 1.0), mode(980.0, 0.025, 0.8), mode(1_420.0, 0.02, 0.6), mode(2_100.0, 0.015, 0.4), mode(2_900.0, 0.01, 0.25)]);
    let rough = wobble(sec, sub(seed, 3), 25.0);
    let tear = mul(shape(tear, move |t| gate(t, 0.08, tear_end, 0.05)), &rough.iter().map(|r| 0.4 + 0.6 * r.abs()).collect::<Vec<f32>>());
    add(&mut out, &norm(tear), 0.0, 0.6);
    let gs = stick_slip(sec, sub(seed, 4), 0.1, sec * 0.8, |t| 30.0 + 20.0 * libm::sinf(t * 2.0), 0.2);
    let groan = modal(&gs, &[mode(70.0, 0.2, 1.0), mode(115.0, 0.15, 0.7), mode(180.0, 0.12, 0.5), mode(260.0, 0.1, 0.3)]);
    add(&mut out, &norm(shape(groan, move |t| gate(t, 0.1, sec * 0.8, 0.3))), 0.0, 0.5);
    for k in 0..3u64 {
        let t = rr(seed, 10 + k, 0.4, tear_end + 0.2);
        let c = noise_band(0.08, sub(seed, 20 + k), 700.0, 7_000.0);
        add(&mut out, &norm(shape(c, |t| ad(t, 0.0003, 0.008))), t, 0.4);
        add(&mut out, &metal_hit(0.5, sub(seed, 30 + k), rr(seed, 40 + k, 250.0, 500.0), 0.08, 5, 0.002), t, 0.25);
    }
    let heave = rumble(sec, sub(seed, 5), 400.0, sec * 0.45, 0.4, sec * 0.18, 0.6);
    add(&mut out, &heave, 0.0, 0.5);
    rain_down(&mut out, sub(seed, 6), sec * 0.55, sec * 0.95, 160, 0.6, 0.2);
    sat(norm(out), 1.5)
}

/// Debris clattering on the deck (some pieces bouncing) and splashing into the sea.
fn debris_fall(sec: f32, seed: u64) -> Vec<f32> {
    let mut out = silence(sec);
    let end = sec * 0.8;
    let starts = scatter(sub(seed, 1), 24, 0.0, end * 0.7, |t| libm::expf(-t / 0.5));
    for (i, &t0) in starts.iter().enumerate() {
        let i = i as u64;
        let heavy = hash_unit_k(seed, 100 + i) < 0.3;
        let (f, tau) = if heavy { (rr(seed, 200 + i, 250.0, 700.0), rr(seed, 300 + i, 0.06, 0.15)) } else { (rr(seed, 200 + i, 1_000.0, 4_000.0), rr(seed, 300 + i, 0.02, 0.08)) };
        let mut a = rr(seed, 400 + i, 0.3, 1.0);
        let mut t = t0;
        let mut gap = rr(seed, 500 + i, 0.08, 0.2);
        let bounces = if hash_unit_k(seed, 600 + i) < 0.5 { 3 } else { 1 };
        for k in 0..bounces {
            if t >= end {
                break;
            }
            clink_grain(&mut out, t, f, tau, a, sub(seed, 700 + i * 4 + k));
            t += gap;
            gap *= 0.6;
            a *= 0.45;
        }
    }
    for k in 0..5u64 {
        let t = rr(seed, 800 + k, 0.3, sec * 0.85);
        let sp = splash((sec - t).clamp(0.1, 0.6), sub(seed, 900 + k), 0.05);
        add(&mut out, &sp, t, rr(seed, 950 + k, 0.2, 0.45));
    }
    norm(out)
}

fn hash_unit_k(seed: u64, i: u64) -> f32 {
    crate::render::hash_unit(seed, i)
}

/// Loop for a burning compartment: a low flickering roar with crackle and occasional pops.
fn fire_loop(sec: f32, seed: u64) -> Vec<f32> {
    let mut out = silence(sec);
    let roar = lp4(&brownn(sec, sub(seed, 1)), 450.0);
    let air = lp4(&pinkn(sec, sub(seed, 2)), 1_500.0);
    let fl = wobble(sec, sub(seed, 3), 2.5);
    let mut r = norm(roar);
    let a = norm(air);
    for ((v, b), m) in r.iter_mut().zip(a.iter()).zip(fl.iter()) {
        *v = (*v + 0.4 * b) * (0.75 + 0.25 * m);
    }
    add(&mut out, &norm(r), 0.0, 0.8);
    let mut cr = silence(sec);
    let times = scatter(sub(seed, 4), (sec * 40.0) as usize, 0.0, sec, |_| 1.0);
    for (i, &t) in times.iter().enumerate() {
        let i = i as u64;
        tick_grain(&mut cr, t, rr(seed, 100 + 2 * i, 0.0004, 0.002), rr(seed, 101 + 2 * i, 2_500.0, 6_000.0), rr(seed, 5_000 + i, 0.1, 1.0), sub(seed, 10_000 + i));
    }
    let pops = scatter(sub(seed, 5), (sec * 2.5) as usize, 0.0, sec, |_| 1.0);
    for (i, &t) in pops.iter().enumerate() {
        let i = i as u64;
        tick_grain(&mut cr, t, 0.003, 4_000.0, 1.8, sub(seed, 20_000 + i));
        drop_grain(&mut cr, t, rr(seed, 30_000 + i, 600.0, 1_200.0), 0.8, 0.008, 0.6);
    }
    add(&mut out, &norm(cr), 0.0, 0.35);
    norm(out)
}

/// Ignition: the whoomp of a fuel flash, a burst of crackle, and the fire taking hold then settling.
fn fire_ignite(sec: f32, seed: u64) -> Vec<f32> {
    let mut out = silence(sec);
    let wh = lp(&pinkn(sec, sub(seed, 1)), sub(seed, 2), |t| 300.0 + 2_200.0 * swell(t, 0.0, 0.05, 0.2), 0.8);
    add(&mut out, &norm(shape(wh, |t| swell(t, 0.0, 0.04, 0.25))), 0.0, 0.9);
    let th = osc(0.8f32.min(sec), |t| glide(t, 80.0, 55.0, 0.04), &[(1.0, 1.0)]);
    add(&mut out, &norm(shape(th, |t| ad(t, 0.01, 0.15))), 0.0, 0.5);
    let mut cr = silence(sec);
    let times = scatter(sub(seed, 3), 70, 0.03, sec * 0.8, |t| libm::expf(-t / 0.25));
    for (i, &t) in times.iter().enumerate() {
        let i = i as u64;
        tick_grain(&mut cr, t, rr(seed, 100 + i, 0.0005, 0.002), rr(seed, 200 + i, 2_500.0, 6_000.0), rr(seed, 300 + i, 0.2, 1.0), sub(seed, 400 + i));
    }
    add(&mut out, &norm(cr), 0.0, 0.35);
    let tau = sec / 3.5;
    let roar = lp4(&brownn(sec, sub(seed, 4)), 450.0);
    add(&mut out, &norm(shape(roar, move |t| swell(t, 0.05, 0.25, tau))), 0.0, 0.4);
    norm(out)
}

/// Loop of water entering a breached compartment: turbulent rush and spray, a churning low end and a
/// steady stream of bubbles.
fn flooding_loop(sec: f32, seed: u64) -> Vec<f32> {
    let mut out = silence(sec);
    let rush = bp2(&pinkn(sec, sub(seed, 1)), 700.0, 0.5);
    add(&mut out, &norm(rush), 0.0, 0.6);
    let spray = bp2(&white(sec, sub(seed, 2)), 2_200.0, 0.7);
    add(&mut out, &norm(spray), 0.0, 0.18);
    let churn = lp4(&brownn(sec, sub(seed, 3)), 250.0);
    let sl = wobble(sec, sub(seed, 4), 1.5);
    add(&mut out, &norm(mul(churn, &sl.iter().map(|m| 0.7 + 0.3 * m).collect::<Vec<f32>>())), 0.0, 0.6);
    let mut bub = silence(sec);
    let times = scatter(sub(seed, 5), (sec * 60.0) as usize, 0.0, sec, |_| 1.0);
    for (i, &t) in times.iter().enumerate() {
        let i = i as u64;
        drop_grain(&mut bub, t, rr(seed, 100 + i, 200.0, 900.0), rr(seed, 200 + i, 1.2, 1.7), rr(seed, 300 + i, 0.006, 0.016), rr(seed, 400 + i, 0.2, 1.0));
    }
    add(&mut out, &norm(bub), 0.0, 0.3);
    norm(out)
}

/// The first rush of a new breach: a thud, the high-pressure jet's hiss surging then settling, and churn.
fn breach_rush(sec: f32, seed: u64) -> Vec<f32> {
    let mut out = silence(sec);
    let th = osc(0.6f32.min(sec), |t| glide(t, 80.0, 40.0, 0.05), &[(1.0, 1.0), (2.0, 0.2)]);
    add(&mut out, &norm(shape(th, |t| ad(t, 0.002, 0.08))), 0.0, 0.7);
    let cr = noise_band(0.05, sub(seed, 1), 800.0, 6_000.0);
    add(&mut out, &norm(shape(cr, |t| ad(t, 0.0003, 0.006))), 0.0, 0.3);
    let tau = sec / 2.5;
    let jet_env = move |t: f32| smooth(t / 0.02) * (0.5 + 0.5 * libm::expf(-t / 0.15)) * libm::expf(-t / tau);
    let jet = bp2(&white(sec, sub(seed, 2)), 1_600.0, 0.6);
    add(&mut out, &norm(shape(jet, jet_env)), 0.0, 0.7);
    let body = lp4(&pinkn(sec, sub(seed, 3)), 3_000.0);
    add(&mut out, &norm(shape(body, jet_env)), 0.0, 0.4);
    add(&mut out, &rumble(sec, sub(seed, 4), 300.0, 0.0, 0.1, tau, 0.6), 0.0, 0.6);
    let mut bub = silence(sec);
    let times = scatter(sub(seed, 5), 80, 0.05, sec * 0.9, |t| libm::expf(-t / tau));
    for (i, &t) in times.iter().enumerate() {
        let i = i as u64;
        drop_grain(&mut bub, t, rr(seed, 100 + i, 250.0, 1_000.0), rr(seed, 200 + i, 1.2, 1.7), rr(seed, 300 + i, 0.006, 0.014), rr(seed, 400 + i, 0.2, 1.0));
    }
    add(&mut out, &norm(bub), 0.0, 0.25);
    norm(out)
}

/// Loop of a running pump: shaft rumble, the impeller's vane-pass tone, motor hum, flow noise and a light
/// mechanical rattle at shaft rate, all periodic in the loop.
fn pump_loop(sec: f32, seed: u64, period: f32) -> Vec<f32> {
    let mut out = silence(sec);
    let shaft = snap_hz(29.5, period);
    let vane = snap_hz(shaft * 6.0, period);
    let hum = snap_hz(120.0, period);
    add(&mut out, &norm(osc(sec, move |_| shaft, &[(1.0, 1.0), (2.0, 0.3)])), 0.0, 0.35);
    add(&mut out, &norm(osc(sec, move |_| vane, &[(1.0, 1.0), (2.0, 0.25)])), 0.0, 0.4);
    add(&mut out, &norm(osc(sec, move |_| hum, &[(1.0, 1.0), (2.0, 0.3), (3.0, 0.15)])), 0.0, 0.3);
    add(&mut out, &norm(bp2(&pinkn(sec, sub(seed, 1)), 500.0, 0.5)), 0.0, 0.3);
    let rattle = bp2(&white(sec, sub(seed, 2)), 2_000.0, 2.0);
    let rattle = shape(rattle, move |t| libm::powf(0.5 + 0.5 * libm::sinf(core::f32::consts::TAU * shaft * t), 6.0));
    add(&mut out, &norm(rattle), 0.0, 0.1);
    norm(out)
}

/// Fire put out by water: a surging hiss and sizzle that sputters out, over a low steam whoosh.
fn steam_hiss(sec: f32, seed: u64) -> Vec<f32> {
    let mut out = silence(sec);
    let tau = sec / 3.3;
    let env = move |t: f32| smooth(t / 0.03) * (0.6 + 0.4 * libm::expf(-t / 0.15)) * libm::expf(-t / tau);
    let sputter = wobble(sec, sub(seed, 1), 18.0);
    let sp: Vec<f32> = sputter.iter().map(|m| 0.6 + 0.4 * m).collect();
    let hiss = lp4(&bp2(&white(sec, sub(seed, 2)), 3_500.0, 0.5), 7_500.0);
    add(&mut out, &norm(mul(shape(hiss, env), &sp)), 0.0, 0.7);
    let sizzle = lp4(&bp2(&white(sec, sub(seed, 3)), 5_500.0, 0.7), 7_500.0);
    add(&mut out, &norm(mul(shape(sizzle, env), &sp)), 0.0, 0.3);
    let mut cr = silence(sec);
    let times = scatter(sub(seed, 4), 60, 0.02, sec * 0.8, |t| libm::expf(-t / tau));
    for (i, &t) in times.iter().enumerate() {
        let i = i as u64;
        tick_grain(&mut cr, t, rr(seed, 100 + i, 0.0004, 0.0015), 6_000.0, rr(seed, 200 + i, 0.2, 1.0), sub(seed, 300 + i));
    }
    add(&mut out, &norm(cr), 0.0, 0.2);
    let wh = lp4(&pinkn(sec, sub(seed, 5)), 800.0);
    add(&mut out, &norm(shape(wh, move |t| swell(t, 0.0, 0.1, tau))), 0.0, 0.35);
    norm(out)
}

#[cfg(test)]
mod tests {
    use crate::{render, ImpactSfx, Playback, SoundId};

    #[test]
    fn every_impact_renders_clean() {
        for w in ImpactSfx::ALL {
            let id = SoundId::Impact(w);
            let spec = id.spec();
            let pcm = render(id);
            assert_eq!(pcm.frames(), crate::render::spec_frames(&spec), "{w:?}");
            let peak = pcm.samples.iter().map(|&s| (s as i32).abs()).max().unwrap();
            // Levels are loudness targets (`analysis::target`); the peak only has to sit under the ceiling.
            let ceiling = libm::powf(10.0, spec.peak_dbfs.min(crate::render::CEILING_DBFS) / 20.0) * 32767.0;
            assert!(peak as f32 <= ceiling + 2.0 && peak as f32 > ceiling * 0.01, "{w:?} peak {peak} ceiling {ceiling}");
            let loud = pcm.samples.iter().filter(|&&s| (s as i32).abs() > peak / 100).count();
            assert!(loud > pcm.samples.len() / 20, "{w:?} mostly silent");
            if spec.playback == Playback::Loop {
                assert_eq!(pcm.loop_frames, Some((0, pcm.frames() as u32)));
                let s = &pcm.samples;
                let mean_step = s.windows(2).map(|p| (p[1] as f32 - p[0] as f32).abs()).sum::<f32>() / (s.len() - 1) as f32;
                let wrap = (s[0] as f32 - s[s.len() - 1] as f32).abs();
                assert!(wrap < mean_step * 8.0 + 64.0, "{w:?} seam {wrap} vs {mean_step}");
            } else {
                let tail = &pcm.samples[pcm.samples.len() * 95 / 100..];
                let tail_peak = tail.iter().map(|&s| (s as i32).abs()).max().unwrap();
                assert!(tail_peak < peak / 4, "{w:?} tail {tail_peak} peak {peak}");
            }
        }
    }

    #[test]
    fn impacts_are_deterministic() {
        for w in [ImpactSfx::SplashSmall, ImpactSfx::AutocannonHit, ImpactSfx::PumpLoop] {
            assert_eq!(crate::render_wav(SoundId::Impact(w)), crate::render_wav(SoundId::Impact(w)));
        }
    }
}
