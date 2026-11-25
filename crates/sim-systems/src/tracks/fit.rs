//! Fire-control target motion analysis: the estimate the gun solver leads with, and the error it really has.
//! DESIGN.md L462 (the track filter's gains are fixed: alpha 0.5 and beta 0.167 for fire control at 10 Hz),
//! L518 (the solver aims at the track estimate), L489 (effective range from the predicted single-round hit
//! probability). Unit: `sys-tracks`.
//!
//! The spec's fixed-gain alpha-beta filter is the track: it follows a contact closely, but its velocity carries
//! about 2 m/s of noise per metre of fix noise (about 10 m/s rms under a 5 m fire-control lock at 10 Hz). Led
//! over a 6 s time of flight that alone scatters shells over 60 m. A fire-control computer instead fits the
//! target's course and speed over a longer baseline of fixes. [`MotionFit`] does that for surface and
//! subsurface tracks without touching the filter: two exponentially weighted least-squares line fits of the
//! ranged fixes the track took, one over a long baseline ([`FIT_LONG_S`]) and one over a short one
//! ([`FIT_SHORT_S`]). Each fix is weighted by the inverse of its fused measurement variance, so a 5 m lock fix
//! outweighs a 50 m search-radar fix a hundred times.
//!
//! - A steady target: the two fits agree within their statistical error, and the long fit is the estimate.
//!   Its velocity error under a 10 Hz lock is a few centimetres a second.
//! - A manoeuvring target: the fits disagree by more than [`MANOEUVRE_CHI2`] in their combined variance. The
//!   short fit is the estimate, its errors are widened by the bias a line fit has against the acceleration the
//!   disagreement implies, and that acceleration is reported for the hit model.
//!
//! The error the estimate reports is honest: the exact variance of an exponentially weighted least-squares fit
//! under the fixes' own variances (kept as three extra sums), plus the manoeuvre bias. Nothing here is tuned to
//! a hit rate; the hit model in `crate::guidance::gun` reads these numbers as they are.
//!
//! State is O(1) per track (two sets of ten sums), updated in O(1) per ranged fix, carried in keyframes and the
//! state hash with the track. Sums are kept about the latest fix in space and time, so they stay small and
//! exact to rounding wherever the track is.
use crate::prelude::*;

/// Time constant of the long fit, seconds: 6 s. Under a 10 Hz lock with 5 m fixes its velocity error is
/// 5 / (2 sqrt(10 x 6^3)) = 0.05 m/s per axis, and a steady 0.3 m/s^2 manoeuvre (L463's surface sigma_a)
/// biases it by 2 x 0.3 x 6 = 3.6 m/s, which the short fit then detects.
pub const FIT_LONG_S: f64 = 6.0;
/// Time constant of the short fit, seconds: 1.5 s (velocity error 0.43 m/s per axis under a 10 Hz lock).
pub const FIT_SHORT_S: f64 = 1.5;
/// A gap without fixes longer than this restarts the fit (two long time constants: older fixes then weigh
/// under 14% and describe a course the target may well have left).
pub const FIT_RESET_GAP_S: f64 = 2.0 * FIT_LONG_S;
/// Squared Mahalanobis distance between the two fits (2 degrees of freedom, per quantity) above which the
/// target is taken to be manoeuvring: 11.8, the 99.7% point of chi-squared with 2 degrees of freedom.
pub const MANOEUVRE_CHI2: f64 = 11.8;
/// Horizon of the fit's check on its own predictions, seconds: a medium gun's time of flight at about 6 km, where
/// open-fire decisions against small craft fall. A weaving hull's miss grows faster than t^2 while the fits
/// trail its turns, so a shorter check under-reads it at the ranges that matter.
pub const CHECK_S: f64 = 8.0;
/// Memory of the measured manoeuvre acceleration, seconds (several checks, and longer than a zigzag leg's turn).
pub const MANOEUVRE_MEMORY_S: f64 = 30.0;
/// Manoeuvre acceleration a fit assumes of a target before its checks have watched it, m/s^2: what they read of
/// the Combine hulls hunting and weaving in the k2 calibration runs (0.4 to 0.6). Not having watched a contact
/// is no evidence that it holds its course; a fight's first rounds go out while its hulls work up to speed and
/// turn toward the enemy.
pub const MANOEUVRE_PRIOR_MPS2: f64 = 0.5;
/// The estimate is usable once the long fit holds at least this many fixes over at least [`MIN_SPAN_S`].
pub const MIN_FIXES: u32 = 3;
/// Shortest baseline the estimate is given on, seconds. A fit of a second of fixes on a hull working up to
/// speed reads a course that is not yet the one it will hold: in the 1-against-3 trials the first rounds laid
/// on such fits hit one time in forty where their formal error promised one in eight (k2). Until then the
/// track's filter speaks for the target, and its velocity error keeps the guns waiting.
pub const MIN_SPAN_S: f64 = 2.0;
const TICK_S: f64 = 1.0 / sim_core::TICK_HZ as f64;

/// One exponentially weighted least-squares line fit, per horizontal axis x(u) = a + b u, with u the time in
/// seconds from the fit's tick and positions about the fit's origin. Weights w = lambda / var, with lambda the
/// exponential forgetting factor of the sample's age.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct LineFit {
    /// Sum of w, w u, w u^2.
    pub s0: f64,
    pub s1: f64,
    pub s2: f64,
    /// Sum of w x, w z, w u x, w u z.
    pub px: f64,
    pub pz: f64,
    pub qx: f64,
    pub qz: f64,
    /// Sum of lambda^2 / var, lambda^2 u / var, lambda^2 u^2 / var: the variance of an exponentially weighted
    /// estimate is not the inverse of its weighted normal matrix, so these give the exact one.
    pub t0: f64,
    pub t1: f64,
    pub t2: f64,
}

/// Fit of one axis pair: intercept and slope with their per-axis variances and covariance.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Line { pub a: DVec2, pub b: DVec2, pub var_a: f64, pub var_b: f64, pub cov_ab: f64 }

impl LineFit {
    /// Age every sum by `dt` seconds under time constant `tau`, and move the time origin to now.
    fn advance(&mut self, dt: f64, tau: f64) {
        let l = libm::exp(-dt / tau);
        let l2 = l * l;
        let (s0, s1, s2) = (self.s0 * l, self.s1 * l, self.s2 * l);
        let (px, pz, qx, qz) = (self.px * l, self.pz * l, self.qx * l, self.qz * l);
        let (t0, t1, t2) = (self.t0 * l2, self.t1 * l2, self.t2 * l2);
        // u_old = u_new + dt... with u measured back from the new tick: u -> u - dt.
        self.s0 = s0;
        self.s1 = s1 - dt * s0;
        self.s2 = s2 - 2.0 * dt * s1 + dt * dt * s0;
        self.px = px;
        self.pz = pz;
        self.qx = qx - dt * px;
        self.qz = qz - dt * pz;
        self.t0 = t0;
        self.t1 = t1 - dt * t0;
        self.t2 = t2 - 2.0 * dt * t1 + dt * dt * t0;
    }
    /// Move the spatial origin by `d` (new origin = old origin + d).
    fn shift(&mut self, d: DVec2) {
        self.px -= d.x * self.s0;
        self.pz -= d.y * self.s0;
        self.qx -= d.x * self.s1;
        self.qz -= d.y * self.s1;
    }
    /// Add a sample at the time and spatial origin with variance `var`.
    fn add_at_origin(&mut self, var: f64) {
        let w = 1.0 / var;
        self.s0 += w;
        self.t0 += w;
    }
    /// The fitted line, or `None` while the samples cannot fix a slope.
    pub fn line(&self) -> Option<Line> {
        let d = self.s0 * self.s2 - self.s1 * self.s1;
        if !(d.is_finite() && d > 1e-12 * self.s0 * self.s2.max(1e-300)) || !(self.s0 > 0.0) { return None; }
        let b = DVec2::new(self.s0 * self.qx - self.s1 * self.px, self.s0 * self.qz - self.s1 * self.pz) / d;
        let a = DVec2::new(self.s2 * self.px - self.s1 * self.qx, self.s2 * self.pz - self.s1 * self.qz) / d;
        let d2 = d * d;
        let var_b = (self.s0 * self.s0 * self.t2 - 2.0 * self.s0 * self.s1 * self.t1 + self.s1 * self.s1 * self.t0) / d2;
        let var_a = (self.s2 * self.s2 * self.t0 - 2.0 * self.s1 * self.s2 * self.t1 + self.s1 * self.s1 * self.t2) / d2;
        let cov_ab = (self.s0 * self.s2 * self.t1 + self.s1 * self.s1 * self.t1 - self.s1 * self.s2 * self.t0 - self.s0 * self.s1 * self.t2) / d2;
        if !(a.is_finite() && b.is_finite() && var_a.is_finite() && var_b.is_finite() && cov_ab.is_finite()) { return None; }
        Some(Line { a, b, var_a: var_a.max(0.0), var_b: var_b.max(0.0), cov_ab })
    }
}

/// The fire-control motion fit of one track (see the module doc).
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct MotionFit {
    /// Tick of the latest fix: the fits' time origin.
    pub tick: u64,
    /// World (x, z) of the latest fix: the fits' spatial origin.
    pub origin: DVec2,
    /// Fixes taken since the fit last restarted, and the tick of the first of them.
    pub fixes: u32,
    #[serde(default)]
    pub since: u64,
    /// The long fit, then the short one.
    pub fits: [LineFit; 2],
    /// The fit's check on itself: a prediction made at `check_from` (position and velocity) is compared with
    /// the first fix at least [`CHECK_S`] later. The miss beyond the statistical error is a manoeuvre the
    /// target flew in that time; `accel2` is the mean of the squared accelerations those misses imply,
    /// remembered over [`MANOEUVRE_MEMORY_S`]. A target that weaves or turns under fire shows here.
    pub check_from: u64,
    pub check_pos: DVec2,
    pub check_vel: DVec2,
    pub check_var: f64,
    pub accel2: f64,
    /// Geometry of the fixes, weighted like the long fit: the sum of w x (unit line of sight from the sensor),
    /// and of w x (variance along that line / the fix's isotropic variance) and w x (across it / isotropic).
    /// The fit weighs fixes by their isotropic equivalent, so its errors along and across the sensors' line of
    /// sight are these fractions of the isotropic ones ([`MotionEstimate::along_frac`]). A search radar fix is
    /// 50 m in range but a few metres in bearing at gun ranges: across the line of fire it is good for guns.
    #[serde(default)]
    pub los_sum: DVec2,
    #[serde(default)]
    pub along_sum: f64,
    #[serde(default)]
    pub cross_sum: f64,
    /// The sensor whose fix weighed most in the latest fused fix (the default until the track filter names one,
    /// [`MotionFit::seen_by`]). The filter's initiation pass reads it: single-fix tracks are one object's only
    /// when one sensor reported them all.
    #[serde(default)]
    pub sensor: SensorRef,
    /// World (x, z) and per-axis variance of the first fix since the fit last restarted (at tick `since`). The
    /// track filter compares the latest fix with it to tell how far the object has really moved in that time.
    #[serde(default)]
    pub first: DVec2,
    #[serde(default)]
    pub first_var: f64,
}

/// How one fix's error is shaped: the unit line of sight from its sensor (world x, z) and its variances along
/// and across that line, m^2.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct FixShape { pub los: DVec2, pub along_var: f64, pub cross_var: f64 }

/// What the fit says about a target now: horizontal position and velocity with their 1-sigma errors per axis,
/// and the acceleration of a detected manoeuvre (0 while the target holds its course).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct MotionEstimate {
    /// Tick the estimate is for, world (x, z) position and velocity there.
    pub tick: u64,
    pub pos: DVec2,
    pub vel: DVec2,
    /// Per-axis variances of position and velocity at `tick`, and their covariance.
    pub var_pos: f64,
    pub var_vel: f64,
    pub cov: f64,
    /// Magnitude of the acceleration implied by the two fits' disagreement, m/s^2 (0 when steady).
    pub accel_mps2: f64,
    pub manoeuvring: bool,
    /// The target's measured manoeuvre acceleration, magnitude (rms over [`MANOEUVRE_MEMORY_S`], from the fit's
    /// checks on its own predictions), m/s^2.
    pub manoeuvre_mps2: f64,
    /// The fixes' mean line of sight (unit, world x, z) and the shares of the isotropic variances above that lie
    /// along it and across it (both 1 for round fixes; see [`MotionFit::los_sum`]).
    pub fix_los: DVec2,
    pub along_frac: f64,
    pub cross_frac: f64,
}
impl MotionEstimate {
    /// Per-axis position variance of the constant-velocity prediction `t` seconds after `tick` (the fit's own
    /// statistical error; the manoeuvre allowance is the hit model's).
    pub fn var_at(&self, t: f64) -> f64 { (self.var_pos + 2.0 * t * self.cov + t * t * self.var_vel).max(0.0) }
    /// Predicted world (x, z) at `tick` (constant velocity from the estimate).
    pub fn predict(&self, tick: u64) -> DVec2 { self.pos + self.vel * ((tick as f64 - self.tick as f64) * TICK_S) }
}

impl MotionFit {
    /// A fit started from one fix.
    pub fn start(x: f64, z: f64, var: f64, tick: u64, shape: Option<FixShape>) -> Option<Self> {
        if !(x.is_finite() && z.is_finite() && var.is_finite() && var > 0.0) { return None; }
        let mut f = MotionFit { tick, origin: DVec2::new(x, z), fixes: 1, since: tick, fits: [LineFit::default(); 2], check_from: 0, check_pos: DVec2::ZERO, check_vel: DVec2::ZERO, check_var: 0.0, accel2: MANOEUVRE_PRIOR_MPS2 * MANOEUVRE_PRIOR_MPS2, los_sum: DVec2::ZERO, along_sum: 0.0, cross_sum: 0.0, sensor: SensorRef::default(), first: DVec2::new(x, z), first_var: var };
        for l in &mut f.fits { l.add_at_origin(var); }
        f.add_shape(var, shape);
        Some(f)
    }
    /// The same fit with its latest fix credited to `sensor`.
    pub fn seen_by(mut self, sensor: SensorRef) -> Self { self.sensor = sensor; self }
    /// Seconds from the first fix since the last restart to the latest, the distance between the two, and the
    /// 1-sigma error of that distance per axis given the latest fix's variance `var`.
    pub fn moved(&self, var: f64) -> (f64, f64, f64) {
        (self.tick.saturating_sub(self.since) as f64 * TICK_S, (self.origin - self.first).length(), libm::sqrt((self.first_var + var).max(0.0)))
    }
    /// Take one fused ranged fix at world (x, z) with per-axis variance `var` at `tick`. A fix older than the
    /// latest is ignored; a gap over [`FIT_RESET_GAP_S`] restarts the fit. Returns the updated fit (`None` for
    /// an unusable first fix).
    pub fn update(fit: Option<Self>, x: f64, z: f64, var: f64, tick: u64, shape: Option<FixShape>) -> Option<Self> {
        if !(x.is_finite() && z.is_finite() && var.is_finite() && var > 0.0) { return fit; }
        let Some(mut f) = fit else { return Self::start(x, z, var, tick, shape) };
        if tick < f.tick { return Some(f); }
        let dt = (tick - f.tick) as f64 * TICK_S;
        if dt > FIT_RESET_GAP_S { return Self::start(x, z, var, tick, shape); }
        let d = DVec2::new(x, z) - f.origin;
        for (l, tau) in f.fits.iter_mut().zip([FIT_LONG_S, FIT_SHORT_S]) {
            if dt > 0.0 { l.advance(dt, tau); }
            l.shift(d);
            l.add_at_origin(var);
        }
        if dt > 0.0 {
            let l = libm::exp(-dt / FIT_LONG_S);
            f.los_sum *= l;
            f.along_sum *= l;
            f.cross_sum *= l;
        }
        f.add_shape(var, shape);
        f.origin = DVec2::new(x, z);
        f.tick = tick;
        f.fixes = f.fixes.saturating_add(1);
        if !f.fits.iter().all(|l| l.s0.is_finite() && l.t2.is_finite()) { return Self::start(x, z, var, tick, shape); }
        f.check(x, z, var, tick);
        Some(f)
    }
    /// Weigh one fix's shape into the geometry sums (a fix of unknown shape counts as round).
    fn add_shape(&mut self, var: f64, shape: Option<FixShape>) {
        let w = 1.0 / var;
        let (los, a, c) = match shape.filter(|s| s.los.is_finite() && s.los.length_squared() > 0.5 && s.along_var.is_finite() && s.cross_var.is_finite()) {
            // The line of sight is a direction, not a sense: fold it into one half-plane so opposite sensors add.
            Some(s) => (if s.los.x < 0.0 || (s.los.x == 0.0 && s.los.y < 0.0) { -s.los } else { s.los }, (s.along_var / var).max(0.0), (s.cross_var / var).max(0.0)),
            None => (DVec2::ZERO, 1.0, 1.0),
        };
        self.los_sum += los * w;
        self.along_sum += a * w;
        self.cross_sum += c * w;
    }
    /// Close a due check against the target's position now and open the next one (see
    /// [`MotionFit::check_from`]). The position now is the short fit's (a fraction of a fix's noise, and
    /// trailing a turn by only a tau^2 = 2.25 m per m/s^2).
    fn check(&mut self, x: f64, z: f64, var: f64, tick: u64) {
        let span = |from: u64| (tick.saturating_sub(from)) as f64 * TICK_S;
        let open = self.check_from > 0 && self.check_var.is_finite();
        if open && span(self.check_from) < CHECK_S { return; }
        if open {
            let t = span(self.check_from);
            let (now, var_now) = match self.fits[1].line() { Some(l) => (self.origin + l.a, l.var_a), None => (DVec2::new(x, z), var) };
            let miss = now - (self.check_pos + self.check_vel * t);
            // Squared miss beyond what the prediction's own error and the observation's explain (both axes); kept
            // signed so that noise averages out instead of piling up. A miss of 0.5 a t^2 is an acceleration a.
            let a2 = 4.0 * (miss.length_squared() - 2.0 * (self.check_var + var_now)) / (t * t * t * t);
            if a2.is_finite() {
                let w = 1.0 - libm::exp(-t / MANOEUVRE_MEMORY_S);
                self.accel2 += (a2 - self.accel2) * w;
            }
        }
        match self.estimate_core() {
            Some(e) => { self.check_from = tick.max(1); self.check_pos = e.pos; self.check_vel = e.vel; self.check_var = e.var_at(CHECK_S); }
            None => { self.check_from = 0; }
        }
    }
    /// The estimate at the latest fix, or `None` before [`MIN_FIXES`] fixes span any time.
    pub fn estimate(&self) -> Option<MotionEstimate> {
        let mut e = self.estimate_core()?;
        e.manoeuvre_mps2 = libm::sqrt(self.accel2.max(0.0));
        let s0 = self.fits[0].s0;
        if s0 > 0.0 && self.along_sum.is_finite() && self.cross_sum.is_finite() {
            let los = self.los_sum / s0;
            // A spread of lines of sight (several sensors) shortens the mean vector: the fractions then even out
            // toward round, as the fixes' combined error does.
            let k = los.length().clamp(0.0, 1.0);
            let (a, c) = ((self.along_sum / s0).max(0.0), (self.cross_sum / s0).max(0.0));
            let m = 0.5 * (a + c);
            e.fix_los = if k > 1e-6 { los / los.length() } else { DVec2::X };
            e.along_frac = m + (a - m) * k;
            e.cross_frac = m + (c - m) * k;
        }
        Some(e)
    }
    fn estimate_core(&self) -> Option<MotionEstimate> {
        if self.fixes < MIN_FIXES { return None; }
        if ((self.tick.saturating_sub(self.since)) as f64 * TICK_S) < MIN_SPAN_S { return None; }
        let long = self.fits[0].line()?;
        let at = |l: &Line, accel: f64, manoeuvring: bool| MotionEstimate {
            tick: self.tick, pos: self.origin + l.a, vel: l.b, var_pos: l.var_a, var_vel: l.var_b, cov: l.cov_ab, accel_mps2: accel, manoeuvring, manoeuvre_mps2: 0.0,
            fix_los: DVec2::X, along_frac: 1.0, cross_frac: 1.0,
        };
        let Some(short) = self.fits[1].line() else { return Some(at(&long, 0.0, false)) };
        // The fits differ by noise alone when the course is steady; their errors are positively correlated, so
        // the sum of their variances bounds the variance of the difference.
        let dv = short.b - long.b;
        let dp = short.a - long.a;
        let sv = (short.var_b + long.var_b).max(1e-12);
        let sp = (short.var_a + long.var_a).max(1e-12);
        let steady = dv.length_squared() / sv <= MANOEUVRE_CHI2 && dp.length_squared() / sp <= MANOEUVRE_CHI2;
        if steady { return Some(at(&long, 0.0, false)); }
        // A line fit with forgetting time tau trails a steady acceleration a by 2 a tau in velocity and a tau^2
        // in position, so the fits differ by 2 a (tau_long - tau_short) in velocity.
        let span = FIT_LONG_S - FIT_SHORT_S;
        let accel = dv.length() / (2.0 * span);
        let bias_v = 2.0 * accel * FIT_SHORT_S;
        let bias_p = accel * FIT_SHORT_S * FIT_SHORT_S;
        let mut e = at(&short, accel, true);
        e.var_vel += bias_v * bias_v;
        e.var_pos += bias_p * bias_p;
        Some(e)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Deterministic standard normal draws for the tests.
    struct Lcg(u64);
    impl Lcg {
        fn uniform(&mut self) -> f64 { self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407); ((self.0 >> 11) as f64 + 0.5) / (1u64 << 53) as f64 }
        fn normal(&mut self) -> f64 { let (u, v) = (self.uniform(), self.uniform()); libm::sqrt(-2.0 * libm::log(u)) * libm::cos(core::f64::consts::TAU * v) }
    }

    /// Fixes of a target at `vel` with acceleration `acc` from t = 0, every `every` ticks, with sigma `s`.
    fn run(vel: DVec2, acc: DVec2, s: f64, every: u64, ticks: u64, seed: u64) -> (MotionFit, DVec2, DVec2) {
        let mut r = Lcg(seed);
        let mut f: Option<MotionFit> = None;
        let mut truth = (DVec2::new(5_000.0, -3_000.0), vel);
        for k in 0..=ticks / every {
            let tick = 1_000 + k * every;
            let t = (k * every) as f64 * TICK_S;
            truth = (DVec2::new(5_000.0, -3_000.0) + vel * t + acc * (0.5 * t * t), vel + acc * t);
            f = MotionFit::update(f, truth.0.x + s * r.normal(), truth.0.y + s * r.normal(), s * s, tick, None);
        }
        (f.expect("fit"), truth.0, truth.1)
    }

    #[test]
    fn steady_target_velocity_is_honest_and_small() {
        // 30 s of a 10 Hz lock with 5 m fixes on a 15 m/s target, over 300 seeds.
        let (mut z2, mut n, mut sig2, mut alarms) = (0.0, 0.0, 0.0, 0);
        for seed in 0..300u64 {
            let (f, _, v) = run(DVec2::new(12.0, -9.0), DVec2::ZERO, 5.0, 6, 1_800, seed);
            let e = f.estimate().expect("estimate");
            if e.manoeuvring { alarms += 1; continue; }
            let d = e.vel - v;
            z2 += (d.x * d.x + d.y * d.y) / e.var_vel;
            sig2 += e.var_vel;
            n += 2.0;
        }
        assert!(alarms <= 9, "{alarms} of 300 steady targets read as manoeuvring");
        let sigma = (sig2 / n * 2.0).sqrt();
        // The asymptote 5 / (2 sqrt(10 x 216)) = 0.054 m/s.
        assert!((sigma - 0.054).abs() < 0.01, "sigma {sigma}");
        // Normalised errors average 1 per axis: the reported sigma is the real one.
        let chi = z2 / n;
        assert!((0.8..1.25).contains(&chi), "normalised squared error {chi}");
    }

    #[test]
    fn a_turning_target_is_detected_and_its_error_widens() {
        let (f, p, v) = run(DVec2::new(12.0, 0.0), DVec2::new(0.0, 1.0), 5.0, 6, 1_200, 7);
        let e = f.estimate().expect("estimate");
        assert!(e.manoeuvring, "{e:?}");
        assert!((e.accel_mps2 - 1.0).abs() < 0.5, "{e:?}");
        let dv = (e.vel - v).length();
        assert!(dv * dv <= 9.0 * 2.0 * e.var_vel, "velocity off by {dv} against sigma {}", e.var_vel.sqrt());
        assert!((e.pos - p).length() < 20.0);
    }

    #[test]
    fn a_weaving_target_shows_its_manoeuvres_in_the_checks() {
        // 12 m/s, swinging 30 degrees either side of north with a 20 s period: about 1 m/s^2 across.
        let mut r = Lcg(5);
        let mut f: Option<MotionFit> = None;
        let (mut p, w) = (DVec2::ZERO, core::f64::consts::TAU / 20.0);
        for k in 0..900u64 {
            let t = k as f64 * 0.1;
            let h = core::f64::consts::FRAC_PI_6 * libm::sin(w * t);
            p += DVec2::new(libm::sin(h), -libm::cos(h)) * (12.0 * 0.1);
            f = MotionFit::update(f, p.x + 5.0 * r.normal(), p.y + 5.0 * r.normal(), 25.0, 1 + k * 6, None);
        }
        let e = f.unwrap().estimate().unwrap();
        // What the checks measure is the acceleration that explains their misses over the check horizon, which
        // a weave half reverses inside it: well under the swing's 1.4 m/s^2 rms, well over a steady course's 0.
        assert!(e.manoeuvre_mps2 > 0.15 && e.manoeuvre_mps2 < 3.0, "{e:?}");
        // A steady course works the prior off: after 90 s of checks it reads a fraction of it.
        let (s, _, _) = run(DVec2::new(12.0, -9.0), DVec2::ZERO, 5.0, 6, 5_400, 1);
        assert!(s.estimate().unwrap().manoeuvre_mps2 < 0.4 * MANOEUVRE_PRIOR_MPS2, "{:?}", s.estimate());
        let (y, _, _) = run(DVec2::new(12.0, -9.0), DVec2::ZERO, 5.0, 6, 150, 1);
        assert_eq!(y.estimate().unwrap().manoeuvre_mps2, MANOEUVRE_PRIOR_MPS2, "an unwatched contact is assumed to manoeuvre");
    }

    #[test]
    fn young_fits_say_they_know_little_and_gaps_restart() {
        let (f, _, _) = run(DVec2::new(10.0, 0.0), DVec2::ZERO, 5.0, 6, 12, 3);
        assert!(f.estimate().is_none(), "two tenths of a second of fixes give no estimate");
        let (f, _, _) = run(DVec2::new(10.0, 0.0), DVec2::ZERO, 5.0, 6, 120, 3);
        let e = f.estimate().expect("two seconds of fixes");
        assert!(e.var_vel.sqrt() > 1.0, "two seconds of fixes leave a speed error of metres a second: {e:?}");
        let g = MotionFit::update(Some(f), 0.0, 0.0, 25.0, f.tick + 60 * 13, None).expect("restart");
        assert_eq!(g.fixes, 1);
        assert!(g.estimate().is_none());
        assert_eq!(MotionFit::update(None, f64::NAN, 0.0, 25.0, 0, None), None);
        assert_eq!(MotionFit::update(Some(f), 1.0, 1.0, -1.0, f.tick + 6, None), Some(f));
    }

    #[test]
    fn weights_follow_the_fix_variance() {
        // Alternate 5 m lock fixes with 200 m search fixes: the coarse ones barely move the answer.
        let mut r = Lcg(11);
        let mut f: Option<MotionFit> = None;
        for k in 0..300u64 {
            let t = k as f64 * 0.1;
            let s = if k % 2 == 0 { 5.0 } else { 200.0 };
            f = MotionFit::update(f, 8.0 * t + s * r.normal(), s * r.normal(), s * s, k * 6, None);
        }
        let e = f.unwrap().estimate().unwrap();
        assert!((e.vel - DVec2::new(8.0, 0.0)).length() < 0.5, "{e:?}");
    }
}
