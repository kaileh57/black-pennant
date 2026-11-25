//! The smoothed playback clock of [`super::SnapshotBuffer`] (unit `net-interp`): arrival times to sim ticks,
//! jitter, and the playback tick that steers toward `newest estimate - delay` without jumping (L914-916).
//! See the design decisions in the module header of `interp`.
use super::{nominal_rate, InterpTuning, MAX_EXTRAPOLATION_S};
use sim_systems::protocol::snapshot::Pacing;
use std::collections::VecDeque;

/// Frames whose mean arrival defines the newest-tick estimate: those that arrived in the last two seconds.
const LEAD_WINDOW_S: f64 = 2.0;
/// A pacing epoch measures its own tick rate once it holds this much time and this many frames.
const RATE_MIN_SPAN_S: f64 = 1.0;
const RATE_MIN_FRAMES: usize = 8;
/// Highest tick rate believed (4x compression with a margin), ticks per second.
const RATE_MAX: f64 = 60.0 * 4.0 * 1.5;
/// Time constant of the playback steering, seconds.
const STEER_S: f64 = 1.0;
/// The playback tick snaps when it is more than this far (seconds of ticks) from its target.
const SNAP_S: f64 = 1.0;
/// ... and never for less than this many ticks (a paused or very slow clock).
const SNAP_MIN_TICKS: f64 = 30.0;

/// The pacing fields that set the tick rate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct PaceKey { paused: bool, factor: u32, scale_pct: i32 }
impl PaceKey {
    fn of(p: &Pacing) -> PaceKey {
        let s = if p.time_scale.is_finite() && p.time_scale > 0.0 { (p.time_scale.min(1.0) * 100.0).round() as i32 } else { 100 };
        PaceKey { paused: p.paused, factor: p.effective.factor(), scale_pct: s }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct Playback {
    key: Option<PaceKey>,
    /// A pacing change heard from an event, applied at the next frame or sample (which know the time).
    pending: Option<Pacing>,
    /// Rate the pacing asks for, ticks per second.
    nominal: f64,
    /// (arrival s, tick) of frames that advanced the tick in the current pacing epoch, within the window.
    /// An epoch opens with one point stamped at the change carrying the estimate of that moment.
    arrivals: VecDeque<(f64, f64)>,
    /// Measured tick rate, ticks per second.
    rate: f64,
    /// mean(k - rate x t) over the arrivals of the lead window; the estimate is `lead_base + rate x now`.
    lead_base: Option<f64>,
    /// (arrival s, lateness s) of recent frames.
    lateness: VecDeque<(f64, f64)>,
    jitter_p95: f64,
    newest: Option<u64>,
    /// Playback tick and the time it was last advanced.
    play: Option<f64>,
    last_t: f64,
    speed: f64,
}

impl Default for Playback {
    fn default() -> Self {
        Self { key: None, pending: None, nominal: 60.0, arrivals: VecDeque::new(), rate: 60.0, lead_base: None, lateness: VecDeque::new(), jitter_p95: 0.0, newest: None, play: None, last_t: 0.0, speed: 1.0 }
    }
}

impl Playback {
    /// A pacing change heard on the reliable event lane; it takes effect at the next frame or sample.
    pub(crate) fn on_pacing(&mut self, p: &Pacing) {
        if self.key != Some(PaceKey::of(p)) { self.pending = Some(p.clone()); }
    }

    /// Start a new epoch at time `t` if the pacing changed: the rate falls back to the nominal one and the
    /// arrivals restart from one point at `t` carrying the estimate of that moment (never past the newest
    /// tick received, since a pause or slow-down announced now means no more ticks at the old rate).
    fn set_pacing(&mut self, t: f64, p: &Pacing) {
        let key = PaceKey::of(p);
        if self.key == Some(key) { return; }
        let estimate = match (self.lead(t), self.newest) {
            (Some(l), Some(n)) => Some(l.min(n as f64)),
            (None, Some(n)) => Some(n as f64),
            _ => None,
        };
        self.key = Some(key);
        self.nominal = nominal_rate(p);
        self.rate = self.nominal;
        self.arrivals.clear();
        if let Some(k) = estimate { self.arrivals.push_back((t, k)); }
        self.recompute_lead();
    }

    fn take_pending(&mut self, t: f64) {
        if let Some(p) = self.pending.take() { self.set_pacing(t, &p); }
    }

    pub(crate) fn on_frame(&mut self, t: f64, tick: u64, pacing: &Pacing, tuning: &InterpTuning) {
        if !t.is_finite() { return; }
        self.take_pending(t);
        self.set_pacing(t, pacing);
        if self.newest.is_some_and(|n| tick <= n) { return; }
        // Lateness behind the estimate from earlier frames.
        if let Some(lead) = self.lead(t) {
            if self.rate > 0.0 {
                let late = ((lead - tick as f64) / self.rate).max(0.0);
                if late.is_finite() { self.lateness.push_back((t, late)); }
            }
        }
        self.newest = Some(tick);
        self.arrivals.push_back((t, tick as f64));
        let window = tuning.window_s();
        while self.arrivals.len() > 1 && self.arrivals.front().is_some_and(|a| t - a.0 > window) { self.arrivals.pop_front(); }
        while self.lateness.front().is_some_and(|a| t - a.0 > window) { self.lateness.pop_front(); }
        self.recompute_rate();
        self.recompute_lead();
        self.recompute_jitter();
    }

    fn recompute_rate(&mut self) {
        let n = self.arrivals.len();
        if self.nominal <= 0.0 { self.rate = 0.0; return; }
        let (Some(first), Some(last)) = (self.arrivals.front(), self.arrivals.back()) else { self.rate = self.nominal; return };
        if n < RATE_MIN_FRAMES || last.0 - first.0 < RATE_MIN_SPAN_S { self.rate = self.nominal; return; }
        // Least-squares slope of tick over arrival time.
        let (t0, k0) = *first;
        let nf = n as f64;
        let (mut sx, mut sy) = (0.0, 0.0);
        for &(t, k) in &self.arrivals { sx += t - t0; sy += k - k0; }
        let (mx, my) = (sx / nf, sy / nf);
        let (mut sxx, mut sxy) = (0.0, 0.0);
        for &(t, k) in &self.arrivals {
            let dx = t - t0 - mx;
            sxx += dx * dx;
            sxy += dx * (k - k0 - my);
        }
        let slope = if sxx > 0.0 { sxy / sxx } else { self.nominal };
        self.rate = if slope.is_finite() { slope.clamp(0.0, RATE_MAX) } else { self.nominal };
    }

    fn recompute_lead(&mut self) {
        let Some(&(t_new, _)) = self.arrivals.back() else { self.lead_base = None; return };
        let rate = self.rate;
        let (mut sum, mut n) = (0.0, 0usize);
        for &(t, k) in self.arrivals.iter().filter(|a| t_new - a.0 <= LEAD_WINDOW_S) {
            sum += k - rate * t;
            n += 1;
        }
        self.lead_base = (n > 0).then(|| sum / n as f64);
    }

    fn recompute_jitter(&mut self) {
        let mut v: Vec<f64> = self.lateness.iter().map(|l| l.1).collect();
        if v.is_empty() { self.jitter_p95 = 0.0; return; }
        v.sort_by(f64::total_cmp);
        let rank = ((v.len() as f64 * 0.95).ceil() as usize).clamp(1, v.len());
        self.jitter_p95 = v[rank - 1];
    }

    /// The newest-tick estimate at `t`.
    fn lead(&self, t: f64) -> Option<f64> { self.lead_base.map(|b| b + self.rate * t) }

    pub(crate) fn delay_s(&self, tuning: &InterpTuning) -> f64 { tuning.delay_s(self.jitter_p95) }
    pub(crate) fn jitter_p95_s(&self) -> f64 { self.jitter_p95 }
    pub(crate) fn speed(&self) -> f64 { self.speed }

    /// Advance the playback tick to local time `t` and return it; `None` before the first frame.
    pub(crate) fn advance(&mut self, t: f64, newest: u64, tuning: &InterpTuning) -> Option<f64> {
        if !t.is_finite() { return self.play.map(|p| p.max(0.0)); }
        self.take_pending(t);
        let lead = self.lead(t)?;
        let rate = self.rate;
        // The target may lie before tick 0 at the start of a sortie: playback then holds the first frame
        // until the delay has built up, instead of starting with no buffer.
        let desired = lead - tuning.delay_s(self.jitter_p95) * rate;
        let ceiling = newest as f64 + rate * MAX_EXTRAPOLATION_S;
        let snap = (rate * SNAP_S).max(SNAP_MIN_TICKS);
        let play = match self.play {
            None => {
                self.speed = 1.0;
                desired.min(ceiling)
            }
            Some(p) => {
                let dt = (t - self.last_t).max(0.0);
                let predicted = p + rate * dt;
                let err = desired - predicted;
                if err.abs() > snap {
                    self.speed = 1.0;
                    desired.min(ceiling)
                } else {
                    let m = tuning.max_change();
                    self.speed = if rate > 0.0 { 1.0 + (err / rate / STEER_S).clamp(-m, m) } else { 1.0 };
                    // Never backwards: hold where it is when the ceiling dropped below it.
                    (p + rate * dt * self.speed).min(ceiling).max(p)
                }
            }
        };
        self.last_t = t;
        self.play = Some(play);
        Some(play.max(0.0))
    }
}
