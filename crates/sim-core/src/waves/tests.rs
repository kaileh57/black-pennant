use super::*;
use content::weather::SeaState;

fn weather(code: u8, wind_dir_rad: f64) -> WeatherState { WeatherState { sea_state: SeaState(code), wind_dir_rad, ..WeatherState::default() } }
fn tuning(count: u8) -> WaveTuning { WaveTuning { component_count: count, ..WaveTuning::default() } }

/// Fixed pseudo-random points for sweeps (no RNG dependency on test order).
fn lcg(state: &mut u64) -> f64 {
    *state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
    (*state >> 11) as f64 / (1u64 << 53) as f64
}

#[test]
fn hs_matches_wmo_codes_and_count_is_fixed() {
    for code in 0..=8u8 {
        for count in [8u8, 12, 16] {
            for seed in [1u64, 7, 99] {
                let w = WaveSet::generate(seed, &weather(code, 0.7), &tuning(count));
                assert_eq!(w.count, count);
                let target = SeaState(code).significant_height_m();
                let hs = w.significant_height();
                assert!((hs - target).abs() <= 0.05 * target + 1e-12, "code {code} count {count}: hs {hs} vs {target}");
                for c in &w.waves[..count as usize] {
                    let lambda = TAU / c.k;
                    assert!((8.0 - 1e-9..=400.0 + 1e-9).contains(&lambda), "lambda {lambda}");
                    assert!((c.omega - libm::sqrt(G * c.k)).abs() < 1e-12);
                    assert!((c.dir.length() - 1.0).abs() < 1e-12);
                    assert!((0.0..=1.0).contains(&c.q));
                }
                assert!(steepness_sum(&w.waves[..count as usize]) <= 0.85 + 1e-12);
            }
        }
    }
}

#[test]
fn generation_is_deterministic_and_seeded() {
    let a = WaveSet::generate(42, &weather(5, 1.0), &WaveTuning::default());
    let b = WaveSet::generate(42, &weather(5, 1.0), &WaveTuning::default());
    let c = WaveSet::generate(43, &weather(5, 1.0), &WaveTuning::default());
    assert_eq!(a, b);
    assert_ne!(a.waves, c.waves);
}

#[test]
fn reduced_set_is_a_prefix_of_the_full_set() {
    for code in [3u8, 6] {
        let full = WaveSet::generate(5, &weather(code, 0.0), &tuning(16));
        let small = WaveSet::generate(5, &weather(code, 0.0), &tuning(8));
        for i in 0..8 {
            assert_eq!(full.waves[i].k, small.waves[i].k);
            assert_eq!(full.waves[i].dir, small.waves[i].dir);
            assert!(small.waves[i].amplitude >= full.waves[i].amplitude);
        }
        // Descending energy.
        for i in 1..16 { assert!(full.waves[i].amplitude <= full.waves[i - 1].amplitude); }
    }
}

#[test]
fn bad_tuning_is_clamped() {
    let t = WaveTuning { component_count: 0, min_wavelength_m: f64::NAN, max_wavelength_m: -3.0, max_steepness_sum: f64::INFINITY, swell_count: 200 };
    let w = WaveSet::generate(1, &weather(6, f64::NAN), &t);
    assert_eq!(w.count, 1);
    assert!(w.significant_height().is_finite());
    let t = WaveTuning { component_count: 99, ..WaveTuning::default() };
    assert_eq!(WaveSet::generate(1, &weather(6, 0.0), &t).count, 16);
}

#[test]
fn reference_spectrum_has_32_waves_and_same_hs() {
    for code in [3u8, 6] {
        let r = WaveSet::reference_spectrum(9, &weather(code, 0.3));
        assert_eq!(r.len(), REFERENCE_WAVES);
        let hs = significant_height_of(&r);
        assert!((hs - SeaState(code).significant_height_m()).abs() < 1e-9);
        assert!(steepness_sum(&r) <= 0.85 + 1e-12);
        // Swells are drawn identically in every discretisation.
        let cfg = GenConfig::from_tuning(&WaveTuning::default());
        let a = base_components(9, &weather(code, 0.3), &cfg, MAX_WAVES - cfg.swells, 0);
        let b = base_components(9, &weather(code, 0.3), &cfg, REFERENCE_WAVES - cfg.swells, 0);
        for s in 0..cfg.swells {
            let (x, y) = (a[a.len() - 1 - s], b[b.len() - 1 - s]);
            assert_eq!(x.wave, y.wave);
            assert_eq!(x.energy, y.energy);
        }
    }
}

#[test]
fn compass_convention_and_crest_travel() {
    // Wind blowing toward east (90 degrees) sends waves toward +X.
    let w = WaveSet::generate(3, &weather(5, core::f64::consts::FRAC_PI_2), &WaveTuning::default());
    let mean: DVec2 = w.waves[..12].iter().map(|c| c.dir * c.amplitude * c.amplitude).sum();
    assert!(mean.x > 0.0 && mean.x.abs() > 3.0 * mean.y.abs(), "mean dir {mean:?}");
    // Toward north (0) sends waves toward -Z.
    let w = WaveSet::generate(3, &weather(5, 0.0), &WaveTuning::default());
    let mean: DVec2 = w.waves[..12].iter().map(|c| c.dir * c.amplitude * c.amplitude).sum();
    assert!(mean.y < 0.0 && mean.y.abs() > 3.0 * mean.x.abs());

    // One wave along +X: the crest moves along +X at c = omega / k, and is the sharp side.
    let k = TAU / 50.0;
    let mut set = WaveSet::calm();
    set.count = 1;
    set.waves[0] = GerstnerWave { dir: DVec2::X, k, omega: libm::sqrt(G * k), amplitude: 1.0, q: 0.8, phase: 0.0 };
    assert!((set.height_at(0.0, 0.0, 0.0) - 1.0).abs() < 1e-9);
    let c = set.waves[0].omega / k;
    assert!((set.height_at(c * 3.0, 0.0, 3.0) - 1.0).abs() < 1e-9);
    // Sharp crests: the surface is above mean level over less than half the wavelength.
    let above = (0..1000).filter(|i| set.height_at(*i as f64 * 0.05, 0.0, 0.0) > 0.0).count();
    assert!(above < 490 && above > 400, "crest fraction {above}");
}

#[test]
fn newton_height_reaches_1mm_in_4_steps() {
    let mut st = 11u64;
    for code in 0..=8u8 {
        for count in [8u8, 12, 16] {
            for seed in [2u64, 17, 1234] {
                let w = WaveSet::generate(seed, &weather(code, 2.0), &tuning(count));
                for _ in 0..60 {
                    let x = (lcg(&mut st) - 0.5) * 20_000.0;
                    let z = (lcg(&mut st) - 0.5) * 20_000.0;
                    let t = lcg(&mut st) * 10_800.0;
                    let h = w.height_at(x, z, t);
                    // Converged reference: many steps of the same Newton.
                    let (mut x0, mut z0) = (x, z);
                    for _ in 0..300 {
                        let p = w.displace(x0, z0, t);
                        x0 -= p.x - x;
                        z0 -= p.z - z;
                    }
                    let p = w.displace(x0, z0, t);
                    assert!((p.x - x).abs() < 1e-6 && (p.z - z).abs() < 1e-6);
                    assert!((h - p.y).abs() < 1e-3, "code {code} count {count}: {h} vs {}", p.y);
                }
            }
        }
    }
}

#[test]
fn calm_and_non_finite_inputs() {
    let c = WaveSet::calm();
    assert_eq!(c.height_at(10.0, 5.0, 3.0), 0.0);
    assert_eq!(c.significant_height(), 0.0);
    let w = WaveSet::generate(1, &weather(0, 0.0), &WaveTuning::default());
    assert_eq!(w.count, 12);
    assert_eq!(w.height_at(10.0, 5.0, 3.0), 0.0);
    let w = WaveSet::generate(1, &weather(6, 0.0), &WaveTuning::default());
    assert_eq!(w.height_at(f64::NAN, 0.0, 0.0), 0.0);
    assert_eq!(w.sample(0.0, f64::INFINITY, 0.0).normal, DVec3::Y);
    let u = w.pack_uniforms(DVec3::new(f64::NAN, 0.0, 0.0), f64::NAN);
    assert!(u.mats.iter().flatten().all(|v| v.is_finite()));
}

#[test]
fn sample_velocity_and_normal_are_consistent() {
    let w = WaveSet::generate(8, &weather(5, 0.4), &WaveTuning::default());
    let (x, z, t) = (130.0, -55.0, 400.0);
    let s = w.sample(x, z, t);
    assert!((s.height - w.height_at(x, z, t)).abs() < 1e-12);
    assert!((s.normal.length() - 1.0).abs() < 1e-9 && s.normal.y > 0.5);
    // Normal matches the finite-difference slope of height_at.
    let e = 0.01;
    let dhdx = (w.height_at(x + e, z, t) - w.height_at(x - e, z, t)) / (2.0 * e);
    let dhdz = (w.height_at(x, z + e, t) - w.height_at(x, z - e, t)) / (2.0 * e);
    let n = DVec3::new(-dhdx, 1.0, -dhdz).normalize();
    assert!((n - s.normal).length() < 1e-3, "{n:?} vs {:?}", s.normal);
    // Vertical velocity is the time derivative of the Lagrangian particle's height.
    let (x0, z0) = w.invert(x, z, t);
    let vy = (w.displace(x0, z0, t + 1e-3).y - w.displace(x0, z0, t - 1e-3).y) / 2e-3;
    assert!((vy - s.velocity.y).abs() < 1e-3);
}

#[test]
fn uniform_layout() {
    let w = WaveSet::generate(4, &weather(4, 0.0), &WaveTuning::default());
    let u = w.pack_uniforms(DVec3::ZERO, 0.0);
    assert_eq!(u.count, 12);
    for i in 0..12 {
        let m = &u.mats[i / 2];
        let b = (i % 2) * 8;
        assert_eq!(m[b], w.waves[i].dir.x as f32);
        assert_eq!(m[b + 1], w.waves[i].dir.y as f32);
        assert_eq!(m[b + 2], w.waves[i].k as f32);
        assert_eq!(m[b + 3], w.waves[i].omega as f32);
        assert_eq!(m[b + 4], w.waves[i].amplitude as f32);
        assert_eq!(m[b + 5], w.waves[i].q as f32);
        assert!((m[b + 6] as f64 - wrap_pi(w.waves[i].phase)).abs() < 1e-6);
    }
    assert!(u.mats[6].iter().chain(u.mats[7].iter()).all(|v| *v == 0.0));
    assert_eq!(w.pack_uniforms(DVec3::ZERO, 5000.0).t_ref, (5000.0f64 - 4096.0) as f32);
}

/// L1893: the f32 port matches the f64 sim within 2 mm (t to 3 h, origin offsets to 50 km, |p| to 12 km).
#[test]
fn shader_port_matches_sim_within_2mm() {
    let mut st = 5u64;
    let mut worst = 0.0f64;
    for code in [3u8, 6, 8] {
        let w = WaveSet::generate(77, &weather(code, 1.3), &tuning(16));
        for case in 0..400 {
            let ox = (lcg(&mut st) * 2.0 - 1.0) * 50_000.0;
            let oz = (lcg(&mut st) * 2.0 - 1.0) * 50_000.0;
            let origin = DVec3::new(ox, 0.0, oz);
            let t_r = lcg(&mut st) * 10_800.0;
            let u = w.pack_uniforms(origin, t_r);
            // Shader TIME some way after t_ref, including across the 2048 s rollover.
            let delta = match case % 4 { 0 => 0.0, 1 => lcg(&mut st) * 0.1, 2 => lcg(&mut st) * 30.0, _ => -lcg(&mut st) * 0.05 };
            let mut time = u.t_ref as f64 + delta;
            if case % 8 == 2 { time -= TIME_ROLLOVER_S; }
            let t = t_r + delta;
            let r = 12_000.0 * libm::sqrt(lcg(&mut st));
            let a = lcg(&mut st) * TAU;
            let (px, pz) = (r * libm::cos(a), r * libm::sin(a));
            let p32 = [px as f32, 0.0, pz as f32];
            let got = shader_port_displace(&u, p32, time as f32);
            // Reference at the f32 grid point actually used.
            let want = w.displace(ox + p32[0] as f64, oz + p32[2] as f64, t) - origin;
            let err = DVec3::new(got[0] as f64 - want.x, got[1] as f64 - want.y, got[2] as f64 - want.z).length();
            worst = worst.max(err);
        }
    }
    assert!(worst < 2e-3, "worst port error {worst} m");
}

#[test]
fn include_is_deterministic_and_complete() {
    let a = emit_wave_shader_include(MAX_WAVES);
    assert_eq!(a, emit_wave_shader_include(MAX_WAVES));
    for name in WAVE_MAT_UNIFORMS { assert!(a.contains(&format!("global uniform mat4 {name};"))); }
    assert!(a.contains(&format!("global uniform float {WAVE_T_REF_UNIFORM};")));
    for f in ["vec3 wave_displace(vec2 p, float time)", "float wave_height(vec2 p, float time)", "vec3 wave_normal(vec2 p, float time)"] {
        assert!(a.contains(f));
    }
    assert!(a.contains("bp_waves_7[3]"));
    let b = emit_wave_shader_include(12);
    assert!(!b.contains("bp_waves_7[3]") && b.contains("bp_waves_5[3]"));
    // Braces balance.
    assert_eq!(a.matches('{').count(), a.matches('}').count());
}
