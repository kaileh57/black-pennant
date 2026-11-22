//! Unit `sim-core-waves` (hard: numerically subtle, shared bit-for-bit with the ocean shader).
//! Gerstner wave set, replicated weather, sampling, uniform packing and the shader include emitter.
//! DESIGN.md L1116-1127 (waves and gates), L1885-1893 (ocean shader contract), L1857-1868 (weather).
//!
//! The set is a sum of 12 Gerstner components (8 or 16 if a gate picks them; never above 16), wavelengths 8 to
//! 400 m, seeded by the server and evaluated with libm. The count is fixed in every sea state: sea state only
//! changes amplitudes, wavelengths and directions drawn from a Pierson-Moskowitz spectrum plus 2 swells, with
//! Hs per WMO code 2..=8 of 0.3, 0.9, 1.9, 3.2, 5.0, 7.5 and 11 m within 5%. Sets with sum(Q k A) > 0.85 are
//! rejected. A reduced set keeps the highest-energy components. The shader always draws the same set.
//!
//! # Conventions
//! A component's phase is `theta = k (d . x0) - omega t + phase`. The undisplaced point `x0` (world x, z) moves
//! to `(x0 - Q A d sin theta, A cos theta)`: crests (`theta = 0`) are the sharp side and travel along `+d`.
//! Directions come from the compass convention of the weather (heading `h` maps to world `(sin h, -cos h)`).
//!
//! # Generation
//! A base set of [`MAX_WAVES`] components is drawn from the counter-based RNG keyed on (seed, attempt,
//! [`Stream::WaveSet`], component role): the Pierson-Moskowitz wind sea is split into geometric frequency bins
//! between 0.6 and 3.5 times the peak frequency (band-limited to the tuning's wavelength range), each bin
//! energy integrated exactly from the spectrum's closed-form integral, frequency jittered inside its bin,
//! directions stratified over +-55 degrees around the wind. Swells carry 15% of the energy at long wavelengths
//! from up to 75 degrees off the wind. The set in use keeps the `component_count` highest-energy components,
//! ordered by descending energy (so a reduced set is a prefix of the full one), and is renormalised so
//! `4 sqrt(sum A^2 / 2)` equals the sea state's Hs exactly. If `sum(Q k A)` exceeds the ceiling, the set is
//! redrawn with the next attempt index; after the last attempt the steepnesses Q are scaled down to fit.
//!
//! # Shader packing
//! [`WaveSet::pack_uniforms`] computes per-component phases at the render origin in f64; the emitted include
//! adds `k d . p - omega wrap(TIME - t_ref)` in f32 with origin-relative `p`. Global parameter names are
//! [`WAVE_MAT_UNIFORMS`] and [`WAVE_T_REF_UNIFORM`]; gd-render-ocean sets them and the project registers them
//! as global shader parameters (8 `mat4` and 1 `float`).
#[cfg(test)]
mod tests;

use crate::math::{DVec2, DVec3};
use crate::rng::{self, Key, Stream};
use content::tuning::WaveTuning;
use content::weather::WeatherState;
use core::f64::consts::{PI, TAU};
use serde::{Deserialize, Serialize};
use std::fmt::Write as _;

pub const MAX_WAVES: usize = 16;
/// Shader time wraps at this many seconds (L1891).
pub const TIME_ROLLOVER_S: f64 = 2048.0;
/// Newton steps of [`WaveSet::height_at`] and of the shader's `wave_height` (L1893).
pub const NEWTON_STEPS: usize = 4;
/// Component count of [`WaveSet::reference_spectrum`] (the accuracy gate's reference, L1121).
pub const REFERENCE_WAVES: usize = 32;
/// Global shader parameter names of the 8 packed `mat4`s, in order (component i lives in entry i / 2).
pub const WAVE_MAT_UNIFORMS: [&str; 8] =
    ["bp_waves_0", "bp_waves_1", "bp_waves_2", "bp_waves_3", "bp_waves_4", "bp_waves_5", "bp_waves_6", "bp_waves_7"];
/// Global shader parameter name of the reference time `t_ref` (float).
pub const WAVE_T_REF_UNIFORM: &str = "bp_waves_t_ref";

/// Gravity, m/s^2 (the same value the projectile integrator uses).
const G: f64 = 9.81;
/// Pierson-Moskowitz constants: S(w) = ALPHA g^2 w^-5 exp(-BETA (w0 / w)^4), w0 = g / U19.5.
const PM_ALPHA: f64 = 8.1e-3;
const PM_BETA: f64 = 0.74;
/// Hs = PM_HS_K U19.5^2 / g for a fully developed Pierson-Moskowitz sea.
const PM_HS_K: f64 = 0.21;
/// Peak frequency over w0: (4 BETA / 5)^(1/4).
const PM_PEAK: f64 = 0.877;
/// Wind-sea band around the peak frequency (holds about 97% of the spectrum's energy).
const BAND_LO: f64 = 0.6;
const BAND_HI: f64 = 3.5;
/// Smallest Hs used to shape the spectrum (codes 0 and 1 borrow code 2's shape, then scale down).
const SHAPE_MIN_HS: f64 = 0.3;
/// Directional half-spread of the wind sea, radians (55 degrees).
const WIND_SPREAD: f64 = 55.0 * PI / 180.0;
/// Largest swell offset from the wind, radians (75 degrees).
const SWELL_SPREAD: f64 = 75.0 * PI / 180.0;
/// Share of the total energy carried by the swells.
const SWELL_SHARE: f64 = 0.15;
/// Largest number of swells (tuning asks for 2).
const MAX_SWELLS: usize = 4;
/// Generation attempts before the steepness fallback scales Q down.
const MAX_ATTEMPTS: u64 = 8;
/// RNG field ids (`b` of the key).
const F_FREQ: u64 = 0;
const F_DIR: u64 = 1;
const F_PHASE: u64 = 2;
const F_Q: u64 = 3;
const F_LAMBDA: u64 = 4;
const F_PERM: u64 = 5;
/// RNG component-role offsets (`a` of the key): wind bins use their bin index, swells this plus their index,
/// the direction permutation its own role, so the swells are identical in every discretisation.
const ROLE_SWELL: u64 = 1_000;
const ROLE_PERM: u64 = 2_000;

/// One Gerstner component.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct GerstnerWave {
    /// Unit travel direction in world (x, z).
    pub dir: DVec2,
    /// Wavenumber k = 2 pi / wavelength, rad/m.
    pub k: f64,
    /// Angular frequency omega = sqrt(g k), rad/s (deep water).
    pub omega: f64,
    /// Amplitude in metres.
    pub amplitude: f64,
    /// Steepness Q in 0..=1 (horizontal displacement = Q * A).
    pub q: f64,
    /// Phase offset at the world origin at t = 0, radians.
    pub phase: f64,
}

/// The replicated wave set. Clients rebuild it from (seed, weather) with [`WaveSet::generate`]; the server
/// sends both reliably on change (L920).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct WaveSet {
    pub seed: u64,
    pub count: u8,
    pub waves: [GerstnerWave; MAX_WAVES],
}
impl WaveSet {
    /// Flat calm: every amplitude zero.
    pub fn calm() -> Self { Self { seed: 0, count: 0, waves: [GerstnerWave::default(); MAX_WAVES] } }
    /// Deterministic generation from seed and weather (see module docs). Unit: `sim-core-waves`.
    ///
    /// Bad tuning is clamped: a count outside 1..=16 is clamped into it, a non-finite or inverted wavelength
    /// range falls back to 8..400 m, and a non-finite steepness ceiling uses 0.85.
    pub fn generate(seed: u64, weather: &WeatherState, tuning: &WaveTuning) -> WaveSet {
        let cfg = GenConfig::from_tuning(tuning);
        let count = (tuning.component_count as usize).clamp(1, MAX_WAVES);
        let n_wind = MAX_WAVES - cfg.swells;
        let mut last: Vec<GerstnerWave> = Vec::new();
        for attempt in 0..MAX_ATTEMPTS {
            let base = base_components(seed, weather, &cfg, n_wind, attempt);
            let set = select_and_normalise(base, count, weather);
            if steepness_sum(&set) <= cfg.max_steepness {
                return pack_set(seed, &set);
            }
            last = set;
        }
        limit_steepness(&mut last, cfg.max_steepness);
        pack_set(seed, &last)
    }
    /// The 32-wave reference spectrum for the accuracy gate (L1121); returned as plain components.
    /// Unit: `sim-core-waves`.
    ///
    /// Same spectrum, swells and normalisation as [`WaveSet::generate`] with default tuning (attempt 0), but
    /// with 30 wind-sea bins and no reduction. Steepness is scaled to the 0.85 ceiling if it would exceed it.
    pub fn reference_spectrum(seed: u64, weather: &WeatherState) -> Vec<GerstnerWave> {
        let cfg = GenConfig::from_tuning(&WaveTuning::default());
        let n_wind = REFERENCE_WAVES - cfg.swells;
        let base = base_components(seed, weather, &cfg, n_wind, 0);
        let mut set = select_and_normalise(base, REFERENCE_WAVES, weather);
        limit_steepness(&mut set, cfg.max_steepness);
        set
    }
    /// Displaced surface point for the undisplaced grid point (x0, z0) at time t: returns world position.
    /// Unit: `sim-core-waves`.
    pub fn displace(&self, x0: f64, z0: f64, t: f64) -> DVec3 {
        let mut d = DVec3::ZERO;
        for w in self.active() {
            let (s, c) = libm::sincos(theta(w, x0, z0, t));
            let qa = w.q * w.amplitude;
            d.x -= qa * w.dir.x * s;
            d.y += w.amplitude * c;
            d.z -= qa * w.dir.y * s;
        }
        DVec3::new(x0 + d.x, d.y, z0 + d.z)
    }
    /// Water surface height at world (x, z) and time t: Newton iteration on the horizontal displacement, 4 steps,
    /// reaching 1 mm (L1893). This is the one height every system uses. Unit: `sim-core-waves`.
    ///
    /// Non-finite input returns 0 (mean sea level) rather than poisoning physics with NaN.
    pub fn height_at(&self, x: f64, z: f64, t: f64) -> f64 {
        if !(x.is_finite() && z.is_finite() && t.is_finite()) { return 0.0; }
        let (x0, z0) = self.invert(x, z, t);
        let mut y = 0.0;
        for w in self.active() { y += w.amplitude * libm::cos(theta(w, x0, z0, t)); }
        y
    }
    /// Height and orbital velocity of the water at world (x, z), for drag on moving hulls. Unit: `sim-core-waves`.
    ///
    /// The velocity is the linear-theory particle velocity at the surface point (horizontal `A omega cos theta`
    /// along d, vertical `A omega sin theta`), independent of the visual steepness Q. The normal is the unit
    /// normal of the displaced surface there.
    pub fn sample(&self, x: f64, z: f64, t: f64) -> WaveSample {
        if !(x.is_finite() && z.is_finite() && t.is_finite()) {
            return WaveSample { height: 0.0, velocity: DVec3::ZERO, normal: DVec3::Y };
        }
        let (x0, z0) = self.invert(x, z, t);
        let mut height = 0.0;
        let mut vel = DVec3::ZERO;
        // Jacobian of the horizontal map (jx = dPx/dx0, jy = dPx/dz0 = dPz/dx0, jz = dPz/dz0) and slopes.
        let (mut jx, mut jy, mut jz) = (1.0, 0.0, 1.0);
        let (mut slx, mut slz) = (0.0, 0.0);
        for w in self.active() {
            let (s, c) = libm::sincos(theta(w, x0, z0, t));
            let a = w.amplitude;
            height += a * c;
            let aw = a * w.omega;
            vel.x += aw * c * w.dir.x;
            vel.z += aw * c * w.dir.y;
            vel.y += aw * s;
            let qak = w.q * a * w.k * c;
            jx -= qak * w.dir.x * w.dir.x;
            jy -= qak * w.dir.x * w.dir.y;
            jz -= qak * w.dir.y * w.dir.y;
            let ak = a * w.k * s;
            slx += ak * w.dir.x;
            slz += ak * w.dir.y;
        }
        // dP/dx0 = (jx, -slx, jy), dP/dz0 = (jy, -slz, jz); normal = dP/dz0 x dP/dx0 (points up).
        let n = DVec3::new(jz * slx - jy * slz, jz * jx - jy * jy, jx * slz - jy * slx);
        let len = n.length();
        let normal = if len.is_finite() && len > 1e-12 && n.y > 0.0 { n / len } else { DVec3::Y };
        WaveSample { height, velocity: vel, normal }
    }
    /// Significant wave height of the set (4 sqrt(m0)). Unit: `sim-core-waves`.
    pub fn significant_height(&self) -> f64 { significant_height_of(self.active()) }
    /// Pack this frame's uniforms: per component phi_i = wrap(k_i d_i . O_xz - omega_i t_r + phase_i) computed in
    /// f64 relative to the render origin O, then narrowed to f32 (L1889). 8 mat4s plus t_ref, 600 bytes per frame.
    /// Unit: `sim-core-waves`.
    ///
    /// `t_ref` is filled with `t_render` wrapped to [0, 2048) s, which is right when the shader's `TIME` tracks
    /// sim time modulo the rollover. gd-render-ocean overwrites `t_ref` with its `TIME` replica value for the
    /// frame (the shader's TIME at which `t_render` is shown); phases stay valid either way, because the shader
    /// only uses `TIME - t_ref`. Components at or above `count` are packed as zero (they draw nothing).
    pub fn pack_uniforms(&self, render_origin: DVec3, t_render: f64) -> WaveUniforms {
        let ox = if render_origin.x.is_finite() { render_origin.x } else { 0.0 };
        let oz = if render_origin.z.is_finite() { render_origin.z } else { 0.0 };
        let tr = if t_render.is_finite() { t_render } else { 0.0 };
        let mut mats = [[0.0f32; 16]; 8];
        let active = self.active();
        for (i, w) in active.iter().enumerate() {
            let phi = wrap_pi(theta(w, ox, oz, tr));
            let m = &mut mats[i / 2];
            let b = (i % 2) * 8;
            m[b] = w.dir.x as f32;
            m[b + 1] = w.dir.y as f32;
            m[b + 2] = w.k as f32;
            m[b + 3] = w.omega as f32;
            m[b + 4] = w.amplitude as f32;
            m[b + 5] = w.q as f32;
            m[b + 6] = phi as f32;
            m[b + 7] = 0.0;
        }
        WaveUniforms { mats, t_ref: tr.rem_euclid(TIME_ROLLOVER_S) as f32, count: active.len() as u32 }
    }

    /// Components in use (`count` clamped to [`MAX_WAVES`]).
    fn active(&self) -> &[GerstnerWave] { &self.waves[..(self.count as usize).min(MAX_WAVES)] }

    /// Undisplaced point whose horizontal displacement lands on (x, z): Newton on the 2x2 Jacobian, at most
    /// [`NEWTON_STEPS`] steps, stopping early below 0.1 micrometre. The Jacobian is I - M with the eigenvalues of
    /// M bounded by sum(Q k A) <= 0.85, so it stays invertible.
    fn invert(&self, x: f64, z: f64, t: f64) -> (f64, f64) {
        let (mut x0, mut z0) = (x, z);
        for _ in 0..NEWTON_STEPS {
            let (mut hx, mut hz) = (x0, z0);
            let (mut a, mut b, mut c) = (1.0, 0.0, 1.0);
            for w in self.active() {
                let (s, co) = libm::sincos(theta(w, x0, z0, t));
                let qa = w.q * w.amplitude;
                hx -= qa * w.dir.x * s;
                hz -= qa * w.dir.y * s;
                let qak = qa * w.k * co;
                a -= qak * w.dir.x * w.dir.x;
                b -= qak * w.dir.x * w.dir.y;
                c -= qak * w.dir.y * w.dir.y;
            }
            let (rx, rz) = (hx - x, hz - z);
            if rx * rx + rz * rz < 1e-14 { break; }
            let det = a * c - b * b;
            let (dx, dz) = if det.is_finite() && det > 1e-6 { ((c * rx - b * rz) / det, (a * rz - b * rx) / det) } else { (rx, rz) };
            x0 -= dx;
            z0 -= dz;
        }
        (x0, z0)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct WaveSample { pub height: f64, pub velocity: DVec3, pub normal: DVec3 }

/// Per-frame shader globals: 8 mat4s (column-major f32) plus the reference time (L1891).
/// Layout per component i: mats[i / 2] holds two components of 8 floats each:
/// (dir.x, dir.z, k, omega, amplitude, q, phi, 0). `emit_wave_shader_include` writes the matching GLSL reads.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WaveUniforms { pub mats: [[f32; 16]; 8], pub t_ref: f32, pub count: u32 }

/// Emit the text of `res://shaders/gen/waves.gdshaderinc` from this packing: the uniform declarations, the
/// unpack code and `wave_displace` / `wave_height` functions computing
/// theta_i = k_i d_i . p_xz - omega_i wrap(TIME - t_ref) + phi_i with origin-relative p (L1889-1891).
/// Deterministic: `cargo xtask gen` runs it twice and fails on any byte difference (L1078).
/// Unit: `sim-core-waves`.
///
/// Functions (all positions relative to the render origin, `time` is the shader's `TIME`):
/// `vec3 wave_displace(vec2 p, float time)` (displaced position of undisplaced point p = (x, z)),
/// `float wave_height(vec2 p, float time)` (surface height at p, 4 Newton steps) and
/// `vec3 wave_normal(vec2 p, float time)` (unit normal at the displaced image of undisplaced point p).
/// `max_components` is clamped to 1..=16; all 8 `mat4` globals are always declared.
pub fn emit_wave_shader_include(max_components: usize) -> String {
    let n = max_components.clamp(1, MAX_WAVES);
    let mut s = String::new();
    s.push_str("// Generated by `cargo xtask gen` from sim_core::waves::emit_wave_shader_include. Never edit by hand.\n");
    s.push_str("// The Gerstner wave set shared with the simulation (DESIGN.md L1885-1893).\n");
    s.push_str("// Component i lives in bp_waves_(i / 2), columns 2 (i % 2) and 2 (i % 2) + 1:\n");
    s.push_str("// (dir.x, dir.z, k, omega) and (amplitude, q, phi, unused); phi is the phase at the render origin\n");
    s.push_str("// at t_ref. Positions are relative to the render origin; pass TIME as `time`.\n\n");
    for name in WAVE_MAT_UNIFORMS { let _ = writeln!(s, "global uniform mat4 {name};"); }
    let _ = writeln!(s, "global uniform float {WAVE_T_REF_UNIFORM};\n");
    let _ = writeln!(s, "const float BP_WAVES_ROLLOVER = {:.1};", TIME_ROLLOVER_S);
    let _ = writeln!(s, "const int BP_WAVES_COUNT = {n};\n");

    s.push_str("float bp_waves_dt(float time) {\n");
    let _ = writeln!(s, "\tfloat d = time - {WAVE_T_REF_UNIFORM};");
    s.push_str("\treturn d - BP_WAVES_ROLLOVER * floor(d / BP_WAVES_ROLLOVER + 0.5);\n}\n\n");

    s.push_str("void bp_waves_add(vec4 a, vec4 b, vec2 p, float dt, inout vec3 o) {\n");
    s.push_str("\tfloat th = a.z * (a.x * p.x + a.y * p.y) - a.w * dt + b.z;\n");
    s.push_str("\tfloat sn = sin(th);\n");
    s.push_str("\tfloat cs = cos(th);\n");
    s.push_str("\tfloat qa = b.y * b.x;\n");
    s.push_str("\to.x -= qa * a.x * sn;\n");
    s.push_str("\to.y += b.x * cs;\n");
    s.push_str("\to.z -= qa * a.y * sn;\n}\n\n");

    s.push_str("void bp_waves_add_j(vec4 a, vec4 b, vec2 p, float dt, inout vec2 h, inout vec3 j) {\n");
    s.push_str("\tfloat th = a.z * (a.x * p.x + a.y * p.y) - a.w * dt + b.z;\n");
    s.push_str("\tfloat sn = sin(th);\n");
    s.push_str("\tfloat qa = b.y * b.x;\n");
    s.push_str("\th -= qa * sn * a.xy;\n");
    s.push_str("\tfloat qak = qa * a.z * cos(th);\n");
    s.push_str("\tj.x -= qak * a.x * a.x;\n");
    s.push_str("\tj.y -= qak * a.x * a.y;\n");
    s.push_str("\tj.z -= qak * a.y * a.y;\n}\n\n");

    s.push_str("void bp_waves_add_n(vec4 a, vec4 b, vec2 p, float dt, inout vec3 j, inout vec2 sl) {\n");
    s.push_str("\tfloat th = a.z * (a.x * p.x + a.y * p.y) - a.w * dt + b.z;\n");
    s.push_str("\tfloat sn = sin(th);\n");
    s.push_str("\tfloat qak = b.y * b.x * a.z * cos(th);\n");
    s.push_str("\tj.x -= qak * a.x * a.x;\n");
    s.push_str("\tj.y -= qak * a.x * a.y;\n");
    s.push_str("\tj.z -= qak * a.y * a.y;\n");
    s.push_str("\tsl += b.x * a.z * sn * a.xy;\n}\n\n");

    s.push_str("float bp_waves_h(vec4 a, vec4 b, vec2 p, float dt) {\n");
    s.push_str("\treturn b.x * cos(a.z * (a.x * p.x + a.y * p.y) - a.w * dt + b.z);\n}\n\n");

    let each = |s: &mut String, call: &str| {
        for i in 0..n {
            let m = WAVE_MAT_UNIFORMS[i / 2];
            let c = (i % 2) * 2;
            let _ = writeln!(s, "\t{}", call.replace("$A", &format!("{m}[{c}]")).replace("$B", &format!("{m}[{}]", c + 1)));
        }
    };

    s.push_str("vec3 wave_displace(vec2 p, float time) {\n");
    s.push_str("\tfloat dt = bp_waves_dt(time);\n");
    s.push_str("\tvec3 o;\n\to.x = 0.0;\n\to.y = 0.0;\n\to.z = 0.0;\n");
    each(&mut s, "bp_waves_add($A, $B, p, dt, o);");
    s.push_str("\treturn vec3(p.x + o.x, o.y, p.y + o.z);\n}\n\n");

    s.push_str("float wave_height(vec2 p, float time) {\n");
    s.push_str("\tfloat dt = bp_waves_dt(time);\n");
    s.push_str("\tvec2 q = p;\n");
    let _ = writeln!(s, "\tfor (int it = 0; it < {NEWTON_STEPS}; it++) {{");
    s.push_str("\t\tvec2 h = q;\n");
    s.push_str("\t\tvec3 j;\n\t\tj.x = 1.0;\n\t\tj.y = 0.0;\n\t\tj.z = 1.0;\n");
    for i in 0..n {
        let m = WAVE_MAT_UNIFORMS[i / 2];
        let c = (i % 2) * 2;
        let _ = writeln!(s, "\t\tbp_waves_add_j({m}[{c}], {m}[{}], q, dt, h, j);", c + 1);
    }
    s.push_str("\t\tvec2 r = h - p;\n");
    s.push_str("\t\tfloat det = j.x * j.z - j.y * j.y;\n");
    s.push_str("\t\tq -= vec2(j.z * r.x - j.y * r.y, j.x * r.y - j.y * r.x) / det;\n");
    s.push_str("\t}\n");
    s.push_str("\tfloat y = 0.0;\n");
    each(&mut s, "y += bp_waves_h($A, $B, q, dt);");
    s.push_str("\treturn y;\n}\n\n");

    s.push_str("vec3 wave_normal(vec2 p, float time) {\n");
    s.push_str("\tfloat dt = bp_waves_dt(time);\n");
    s.push_str("\tvec3 j;\n\tj.x = 1.0;\n\tj.y = 0.0;\n\tj.z = 1.0;\n");
    s.push_str("\tvec2 sl = vec2(0.0);\n");
    each(&mut s, "bp_waves_add_n($A, $B, p, dt, j, sl);");
    s.push_str("\treturn normalize(vec3(j.z * sl.x - j.y * sl.y, j.z * j.x - j.y * j.y, j.x * sl.y - j.y * sl.x));\n}\n");
    s
}

/// Rust f32 port of the emitted include, used by the golden test: must match [`WaveSet::displace`] within 2 mm
/// for t up to 3 h, origin offsets up to 50 km and |p| up to 12 km (L1893). Unit: `sim-core-waves`.
///
/// Mirrors `wave_displace` operation for operation (f32, libm `sinf` / `cosf`), over all 16 packed components
/// as the include does with `MAX_WAVES`; `p_rel[1]` is added to the height. Returns the displaced position
/// relative to the render origin.
pub fn shader_port_displace(u: &WaveUniforms, p_rel: [f32; 3], time: f32) -> [f32; 3] {
    let d = time - u.t_ref;
    let dt = d - (TIME_ROLLOVER_S as f32) * libm::floorf(d / (TIME_ROLLOVER_S as f32) + 0.5);
    let (px, pz) = (p_rel[0], p_rel[2]);
    let (mut ox, mut oy, mut oz) = (0.0f32, 0.0f32, 0.0f32);
    for i in 0..MAX_WAVES {
        let m = &u.mats[i / 2];
        let b = (i % 2) * 8;
        let (dx, dz, k, w, a, q, phi) = (m[b], m[b + 1], m[b + 2], m[b + 3], m[b + 4], m[b + 5], m[b + 6]);
        let th = k * (dx * px + dz * pz) - w * dt + phi;
        let sn = libm::sinf(th);
        let cs = libm::cosf(th);
        let qa = q * a;
        ox -= qa * dx * sn;
        oy += a * cs;
        oz -= qa * dz * sn;
    }
    [px + ox, p_rel[1] + oy, pz + oz]
}

// ---- generation internals ----

/// Sanitised generation parameters.
struct GenConfig { lambda_min: f64, lambda_max: f64, max_steepness: f64, swells: usize }
impl GenConfig {
    fn from_tuning(t: &WaveTuning) -> Self {
        let mut lo = if t.min_wavelength_m.is_finite() && t.min_wavelength_m > 0.0 { t.min_wavelength_m } else { 8.0 };
        let mut hi = if t.max_wavelength_m.is_finite() && t.max_wavelength_m > 0.0 { t.max_wavelength_m } else { 400.0 };
        if lo > hi { core::mem::swap(&mut lo, &mut hi); }
        if hi < lo * 2.0 { hi = lo * 2.0; }
        let max_steepness = if t.max_steepness_sum.is_finite() && t.max_steepness_sum > 0.0 { t.max_steepness_sum.min(1.0) } else { 0.85 };
        Self { lambda_min: lo, lambda_max: hi, max_steepness, swells: (t.swell_count as usize).min(MAX_SWELLS) }
    }
}

/// One drawn component with its un-normalised energy (A^2 / 2 share of the total).
#[derive(Clone, Copy)]
struct Drawn { wave: GerstnerWave, energy: f64 }

fn key(seed: u64, attempt: u64, role: u64, field: u64) -> Key { Key::new(seed, attempt, Stream::WaveSet, role, field) }

/// Compass heading (radians, 0 = north = -Z, clockwise) to a world (x, z) unit vector.
fn compass_dir(h: f64) -> DVec2 { DVec2::new(libm::sin(h), -libm::cos(h)) }

fn wave_from(omega: f64, dir: DVec2, q: f64, phase: f64) -> GerstnerWave {
    GerstnerWave { dir, k: omega * omega / G, omega, amplitude: 0.0, q: q.clamp(0.0, 1.0), phase }
}

/// Draw `n_wind` wind-sea bins plus the configured swells. Energies are shares of the total: the wind sea sums
/// to `1 - SWELL_SHARE` and the swells to `SWELL_SHARE` (all to 1 without swells).
fn base_components(seed: u64, weather: &WeatherState, cfg: &GenConfig, n_wind: usize, attempt: u64) -> Vec<Drawn> {
    let code = weather.sea_state.0.min(8);
    let hs_shape = weather.sea_state.significant_height_m().max(SHAPE_MIN_HS);
    let wind = if weather.wind_dir_rad.is_finite() { weather.wind_dir_rad } else { 0.0 };
    let u195 = libm::sqrt(hs_shape * G / PM_HS_K);
    let w0 = G / u195;
    let wp = PM_PEAK * w0;
    // Frequencies allowed by the wavelength range (deep water: omega^2 = g k = 2 pi g / lambda).
    let w_lo = libm::sqrt(G * TAU / cfg.lambda_max);
    let w_hi = libm::sqrt(G * TAU / cfg.lambda_min);
    let band_lo = (BAND_LO * wp).max(w_lo).min(w_hi / 1.5);
    let band_hi = (BAND_HI * wp).min(w_hi).max(band_lo * 1.5);
    // Cumulative PM energy (up to a constant): exp(-BETA (w0 / w)^4).
    let cdf = |w: f64| { let r = w0 / w; let r2 = r * r; libm::exp(-PM_BETA * r2 * r2) };
    let swell_share = if cfg.swells > 0 { SWELL_SHARE } else { 0.0 };
    // Wind sea gets choppier with sea state.
    let chop = 0.45 + 0.06 * code as f64;

    let mut out = Vec::with_capacity(n_wind + cfg.swells);
    // Stratified directions: slot j of a keyed permutation.
    let mut slots: Vec<usize> = (0..n_wind).collect();
    for i in (1..n_wind).rev() {
        let j = rng::below(key(seed, attempt, ROLE_PERM, F_PERM).draw(i as u32 * 64), (i + 1) as u32) as usize;
        slots.swap(i, j);
    }
    let ratio = band_hi / band_lo;
    let mut energies = Vec::with_capacity(n_wind);
    for j in 0..n_wind {
        let e0 = band_lo * libm::pow(ratio, j as f64 / n_wind as f64);
        let e1 = band_lo * libm::pow(ratio, (j + 1) as f64 / n_wind as f64);
        let role = j as u64;
        let omega = e0 + rng::uniform(key(seed, attempt, role, F_FREQ)) * (e1 - e0);
        let off = WIND_SPREAD * (((slots[j] as f64 + rng::uniform(key(seed, attempt, role, F_DIR))) / n_wind as f64) * 2.0 - 1.0);
        let phase = TAU * rng::uniform(key(seed, attempt, role, F_PHASE));
        let q = chop * (0.7 + 0.3 * rng::uniform(key(seed, attempt, role, F_Q)));
        energies.push((cdf(e1) - cdf(e0)).max(0.0));
        out.push(Drawn { wave: wave_from(omega, compass_dir(wind + off), q, phase), energy: 0.0 });
    }
    let total: f64 = energies.iter().sum();
    for (d, e) in out.iter_mut().zip(&energies) {
        d.energy = if total > 0.0 && total.is_finite() { e / total } else { 1.0 / n_wind as f64 } * (1.0 - swell_share);
    }
    // Swells: long, smooth, from a distant storm. Weights n, n-1, ..., 1 (60/40 for two).
    let weight_sum = (cfg.swells * (cfg.swells + 1) / 2) as f64;
    for s in 0..cfg.swells {
        let role = ROLE_SWELL + s as u64;
        let lambda = (cfg.lambda_max * (0.55 + 0.45 * rng::uniform(key(seed, attempt, role, F_LAMBDA)))).max(cfg.lambda_min);
        let omega = libm::sqrt(G * TAU / lambda);
        let off = SWELL_SPREAD * (rng::uniform(key(seed, attempt, role, F_DIR)) * 2.0 - 1.0);
        let phase = TAU * rng::uniform(key(seed, attempt, role, F_PHASE));
        let q = 0.2 + 0.2 * rng::uniform(key(seed, attempt, role, F_Q));
        let energy = swell_share * (cfg.swells - s) as f64 / weight_sum;
        out.push(Drawn { wave: wave_from(omega, compass_dir(wind + off), q, phase), energy });
    }
    out
}

/// Keep the `count` highest-energy components (ties by draw order), ordered by descending energy, with
/// amplitudes scaled so the set's Hs equals the sea state's.
fn select_and_normalise(mut base: Vec<Drawn>, count: usize, weather: &WeatherState) -> Vec<GerstnerWave> {
    // Stable sort keeps draw order among equal energies.
    base.sort_by(|a, b| b.energy.total_cmp(&a.energy));
    base.truncate(count);
    let hs = weather.sea_state.significant_height_m();
    let m0 = (hs / 4.0) * (hs / 4.0);
    let total: f64 = base.iter().map(|d| d.energy).sum();
    base.iter()
        .map(|d| {
            let e = if total > 0.0 && total.is_finite() { d.energy / total } else { 1.0 / count as f64 };
            GerstnerWave { amplitude: libm::sqrt(2.0 * e * m0), ..d.wave }
        })
        .collect()
}

fn steepness_sum(set: &[GerstnerWave]) -> f64 { set.iter().map(|w| w.q * w.k * w.amplitude).sum() }

/// Scale every Q so sum(Q k A) is at most `max`.
fn limit_steepness(set: &mut [GerstnerWave], max: f64) {
    let s = steepness_sum(set);
    if s > max && s > 0.0 {
        let f = max / s * (1.0 - 1e-9);
        for w in set.iter_mut() { w.q *= f; }
    }
}

fn significant_height_of(set: &[GerstnerWave]) -> f64 {
    let m0: f64 = set.iter().map(|w| 0.5 * w.amplitude * w.amplitude).sum();
    4.0 * libm::sqrt(m0)
}

fn pack_set(seed: u64, set: &[GerstnerWave]) -> WaveSet {
    let mut waves = [GerstnerWave::default(); MAX_WAVES];
    let n = set.len().min(MAX_WAVES);
    waves[..n].copy_from_slice(&set[..n]);
    WaveSet { seed, count: n as u8, waves }
}

#[inline]
fn theta(w: &GerstnerWave, x: f64, z: f64, t: f64) -> f64 { w.k * (w.dir.x * x + w.dir.y * z) - w.omega * t + w.phase }

/// Wrap an angle into [-pi, pi).
fn wrap_pi(a: f64) -> f64 { (a + PI).rem_euclid(TAU) - PI }
