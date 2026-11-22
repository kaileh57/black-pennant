//! Unit `net-quality`. Connection quality, adaptive bandwidth, host saturation, host load and the local
//! connection log. DESIGN.md L937 (adaptive cap 128 to 768 kbps, start 256, +10% every 10 s while loss under
//! 1%, -25% when loss over a 2 s window exceeds 5%), L1031 (upload saturation: over 10 s, median client loss
//! over 5% and median RTT up 50 ms or more over the first-minute baseline: every cap -25% with floor 128,
//! upload figure set to the measured send rate, new joins refused; recovery +10% every 30 s while loss under
//! 2%), L1033-1037 (drop rule: RTT over 500 ms, loss over 20%, or more than 30 snapshots behind,
//! continuously for 15 s; warning after 5 s; fallback hold 30 s and loss 25%), L1019-1025 (the driver
//! measures tick p95 over 2 s and the backlog for `sim_systems::lod::HostLoad`), L1039 (every drop, warning,
//! ladder step and port-mapping result goes to a local connection log, included in the diagnostics bundle).
//!
//! Every monitor takes `now: Duration` from the session's clock and a [`LinkSample`] the session reads from
//! the transport; nothing here reads time or sockets itself.
use crate::error::DropReason;
use crate::msg::{DropWarning, LinkQuality};
use serde::{Deserialize, Serialize};
use sim_systems::lod::HostLoad;
use std::time::Duration;

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct QualityTuning {
    pub drop_rtt_ms: f32,
    pub drop_loss: f32,
    pub drop_frames_behind: u32,
    pub hold_s: f32,
    pub warn_s: f32,
    pub cap_min_kbps: u32,
    pub cap_max_kbps: u32,
    pub cap_start_kbps: u32,
    pub cap_raise: f32,
    pub cap_raise_every_s: f32,
    pub cap_raise_below_loss: f32,
    pub cap_cut: f32,
    pub cap_cut_window_s: f32,
    pub cap_cut_above_loss: f32,
    pub sat_window_s: f32,
    pub sat_loss: f32,
    pub sat_rtt_rise_ms: f32,
    pub sat_baseline_s: f32,
    pub sat_recover: f32,
    pub sat_recover_every_s: f32,
    pub sat_recover_below_loss: f32,
}
impl Default for QualityTuning {
    fn default() -> Self {
        Self { drop_rtt_ms: 500.0, drop_loss: 0.20, drop_frames_behind: 30, hold_s: 15.0, warn_s: 5.0,
            cap_min_kbps: 128, cap_max_kbps: 768, cap_start_kbps: 256, cap_raise: 0.10, cap_raise_every_s: 10.0, cap_raise_below_loss: 0.01,
            cap_cut: 0.25, cap_cut_window_s: 2.0, cap_cut_above_loss: 0.05,
            sat_window_s: 10.0, sat_loss: 0.05, sat_rtt_rise_ms: 50.0, sat_baseline_s: 60.0, sat_recover: 0.10, sat_recover_every_s: 30.0, sat_recover_below_loss: 0.02 }
    }
}
impl QualityTuning {
    /// Fallback of L1037: hold 30 s, loss threshold 25%.
    pub fn drop_fallback() -> Self { Self { hold_s: 30.0, drop_loss: 0.25, ..Self::default() } }
}

/// One reading of a client's link, taken by the session each update.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct LinkSample { pub rtt_ms: f32, pub loss: f32, pub frames_behind: u32, pub sent_kbps: f32, pub recv_kbps: f32 }

/// Per-client adaptive cap (L937).
#[derive(Clone, Debug, PartialEq)]
pub struct BandwidthCap {
    pub(crate) kbps: f32,
    /// Start of the current "loss under 1%" run (or the last raise).
    pub(crate) last_raise: Duration,
    /// Loss samples of the cut window.
    pub(crate) losses: std::collections::VecDeque<(Duration, f32)>,
    /// Start of the current cut window: creation or the last cut. A cut needs a full window of samples.
    pub(crate) window_start: Duration,
}
impl BandwidthCap {
    pub fn new(tuning: &QualityTuning, now: Duration) -> Self { Self { kbps: tuning.cap_start_kbps as f32, last_raise: now, losses: Default::default(), window_start: now } }
    pub fn kbps(&self) -> f32 { self.kbps }
    /// Apply the raise and cut rules for one sample. Unit: `net-quality`.
    ///
    /// Cut: when the mean loss over the last `cap_cut_window_s` (with at least that long observed since the
    /// connection started or the previous cut) exceeds `cap_cut_above_loss`, the cap drops by `cap_cut` and
    /// the window restarts, so a sustained bad link cuts once per window, not once per sample. Raise: while
    /// every sample stays under `cap_raise_below_loss`, the cap rises by `cap_raise` each `cap_raise_every_s`;
    /// any sample at or above it restarts that run.
    pub fn update(&mut self, now: Duration, sample: &LinkSample, tuning: &QualityTuning) {
        let loss = if sample.loss.is_finite() { sample.loss.clamp(0.0, 1.0) } else { 1.0 };
        let window = Duration::from_secs_f32(tuning.cap_cut_window_s.max(0.0));
        self.losses.push_back((now, loss));
        while let Some(&(t, _)) = self.losses.front() {
            if now.saturating_sub(t) > window { self.losses.pop_front(); } else { break; }
        }
        if now.saturating_sub(self.window_start) >= window && !self.losses.is_empty() {
            let mean = self.losses.iter().map(|&(_, l)| l).sum::<f32>() / self.losses.len() as f32;
            if mean > tuning.cap_cut_above_loss {
                self.scale(1.0 - tuning.cap_cut, tuning);
                self.losses.clear();
                self.window_start = now;
                self.last_raise = now;
                return;
            }
        }
        if loss >= tuning.cap_raise_below_loss {
            self.last_raise = now;
        } else if now.saturating_sub(self.last_raise).as_secs_f32() >= tuning.cap_raise_every_s {
            self.scale(1.0 + tuning.cap_raise, tuning);
            self.last_raise = now;
        }
    }
    /// Multiply by `factor`, clamped to the tuning range (saturation cut and recovery). Unit: `net-quality`.
    pub fn scale(&mut self, factor: f32, tuning: &QualityTuning) {
        let lo = tuning.cap_min_kbps as f32;
        let hi = (tuning.cap_max_kbps as f32).max(lo);
        let f = if factor.is_finite() && factor > 0.0 { factor } else { 1.0 };
        self.kbps = (self.kbps * f).clamp(lo, hi);
    }
    /// Bytes available for one snapshot interval at `hz`, before reliable traffic is subtracted. Real code.
    pub fn bytes_per_interval(&self, hz: u32) -> usize { ((self.kbps * 1000.0 / 8.0) / hz.max(1) as f32) as usize }
}

/// The drop rule's verdict for one client.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Verdict { Fine, Warn(DropWarning), Drop(DropReason) }

/// Per-client drop monitor: each condition must hold continuously.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DropMonitor {
    pub(crate) rtt_since: Option<Duration>,
    pub(crate) loss_since: Option<Duration>,
    pub(crate) behind_since: Option<Duration>,
    /// Last time each condition (rtt, loss, behind) was seen holding: a condition clears only after
    /// [`CLEAR_GRACE`] below threshold, so one lucky loss reading does not restart a 15 s hold.
    pub(crate) last_bad: [Option<Duration>; 3],
}

/// How long a drop condition must stay clear before its hold timer restarts. Measured loss and RTT are noisy:
/// a 25% loss link reads under 20% now and then, and the gate wants it dropped within 20 s (L1035). A link
/// that is truly fine never holds a condition for 15 s with gaps this short.
pub const CLEAR_GRACE: Duration = Duration::from_millis(1000);
impl DropMonitor {
    pub fn new() -> Self { Self::default() }
    /// Unit: `net-quality`.
    ///
    /// Drop after any condition held for `hold_s`; warn after `warn_s`. When several hold, the one held
    /// longest is reported (ties: ping, then loss, then falling behind).
    pub fn update(&mut self, now: Duration, sample: &LinkSample, tuning: &QualityTuning) -> Verdict {
        let rtt_bad = !sample.rtt_ms.is_finite() || sample.rtt_ms > tuning.drop_rtt_ms;
        let loss_bad = !sample.loss.is_finite() || sample.loss > tuning.drop_loss;
        let behind_bad = sample.frames_behind > tuning.drop_frames_behind;
        let since = [&mut self.rtt_since, &mut self.loss_since, &mut self.behind_since];
        let mut held = [None; 3];
        for (i, (s, bad)) in since.into_iter().zip([rtt_bad, loss_bad, behind_bad]).enumerate() {
            if bad {
                self.last_bad[i] = Some(now);
                s.get_or_insert(now);
            } else if self.last_bad[i].is_none_or(|t| now.saturating_sub(t) >= CLEAR_GRACE) {
                *s = None;
                self.last_bad[i] = None;
            }
            held[i] = s.map(|t| now.saturating_sub(t).as_secs_f32());
        }
        let mut best: Option<(usize, f32)> = None;
        for (i, h) in held.iter().enumerate() {
            if let Some(h) = *h { if best.is_none_or(|(_, b)| h > b) { best = Some((i, h)); } }
        }
        match best {
            Some((i, h)) if h >= tuning.hold_s => Verdict::Drop([DropReason::HighPing, DropReason::PacketLoss, DropReason::FallingBehind][i]),
            Some((i, h)) if h >= tuning.warn_s => Verdict::Warn([DropWarning::HighPing, DropWarning::PacketLoss, DropWarning::FallingBehind][i]),
            _ => Verdict::Fine,
        }
    }
}

/// Panel quality bucket for a sample. Unit: `net-quality`.
///
/// Poor: a drop condition holds right now, or the cap sits at its floor while loss is still over the cut
/// threshold. Fair: ping over half the drop threshold, loss over the cut threshold, more than a third of the
/// drop rule's frames behind, or a cap below the 256 kbps start (the client gets less than the design
/// budget). Good otherwise.
pub fn quality_of(sample: &LinkSample, cap_kbps: f32, tuning: &QualityTuning) -> LinkQuality {
    let rtt = if sample.rtt_ms.is_finite() { sample.rtt_ms } else { f32::MAX };
    let loss = if sample.loss.is_finite() { sample.loss } else { 1.0 };
    let at_floor = cap_kbps <= tuning.cap_min_kbps as f32 + 0.5;
    if rtt > tuning.drop_rtt_ms || loss > tuning.drop_loss || sample.frames_behind > tuning.drop_frames_behind || (at_floor && loss > tuning.cap_cut_above_loss) {
        LinkQuality::Poor
    } else if rtt > tuning.drop_rtt_ms * 0.5 || loss > tuning.cap_cut_above_loss || sample.frames_behind > tuning.drop_frames_behind / 3 || cap_kbps < tuning.cap_start_kbps as f32 - 0.5 {
        LinkQuality::Fair
    } else {
        LinkQuality::Good
    }
}

/// Host upload saturation (L1031).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SaturationDetector {
    pub(crate) started: Option<Duration>,
    pub(crate) baseline_rtt_ms: Option<f32>,
    pub(crate) window: std::collections::VecDeque<(Duration, f32, f32)>,
    pub(crate) saturated: bool,
    pub(crate) last_recover: Duration,
    /// Sum and count of median RTT readings during the baseline minute.
    pub(crate) baseline_sum: f64,
    pub(crate) baseline_n: u32,
    /// Fraction of the pre-saturation caps restored so far: `1 - cap_cut` on saturation, times
    /// `1 + sat_recover` per recovery step.
    pub(crate) level: f32,
    /// Caps are back: `Cleared` goes out on the next update.
    pub(crate) clear_pending: bool,
}
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub enum SaturationChange {
    /// Cut every client cap by `cut` and set the upload figure to `upload_kbps`; refuse joins.
    Saturated { upload_kbps: u32 },
    /// Raise every cap by the recovery step.
    Recover,
    /// Recovered fully: joins allowed again.
    Cleared,
}
impl SaturationDetector {
    pub fn new() -> Self { Self::default() }
    pub fn saturated(&self) -> bool { self.saturated }
    /// Feed all clients' samples and the measured total send rate. Unit: `net-quality`.
    ///
    /// Each update records the median loss and median RTT across `samples`. The RTT baseline is the mean of
    /// those medians over the first `sat_baseline_s` after the first client sample (the lobby's first
    /// minute); no saturation is declared before it exists. Saturated: over a full `sat_window_s`, the median
    /// of the recorded loss medians exceeds `sat_loss` and the median of the RTT medians is at least
    /// `sat_rtt_rise_ms` over the baseline. Then `Recover` every `sat_recover_every_s` while the median loss
    /// stays under `sat_recover_below_loss` (a reading at or over it restarts the wait), and `Cleared` on the
    /// update after the recovery steps have restored the caps to their pre-cut level. A new full window
    /// meeting the condition while recovering saturates again with a fresh upload figure.
    pub fn update(&mut self, now: Duration, samples: &[LinkSample], measured_send_kbps: f32, tuning: &QualityTuning) -> Option<SaturationChange> {
        if self.clear_pending {
            self.clear_pending = false;
            self.saturated = false;
            return Some(SaturationChange::Cleared);
        }
        if samples.is_empty() { return None; }
        let loss = median(samples.iter().map(|s| if s.loss.is_finite() { s.loss.clamp(0.0, 1.0) } else { 1.0 }));
        let rtt = median(samples.iter().map(|s| if s.rtt_ms.is_finite() { s.rtt_ms.max(0.0) } else { 10_000.0 }));
        let started = *self.started.get_or_insert(now);
        if self.baseline_rtt_ms.is_none() {
            if now.saturating_sub(started).as_secs_f32() < tuning.sat_baseline_s {
                self.baseline_sum += rtt as f64;
                self.baseline_n += 1;
            } else {
                self.baseline_rtt_ms = Some(if self.baseline_n > 0 { (self.baseline_sum / self.baseline_n as f64) as f32 } else { rtt });
            }
        }
        let window = Duration::from_secs_f32(tuning.sat_window_s.max(0.0));
        self.window.push_back((now, loss, rtt));
        while let Some(&(t, _, _)) = self.window.front() {
            if now.saturating_sub(t) > window { self.window.pop_front(); } else { break; }
        }
        let full = self.window.front().is_some_and(|&(t, _, _)| now.saturating_sub(t).as_secs_f32() >= tuning.sat_window_s * 0.9);
        if let (Some(base), true) = (self.baseline_rtt_ms, full) {
            let w_loss = median(self.window.iter().map(|w| w.1));
            let w_rtt = median(self.window.iter().map(|w| w.2));
            if w_loss > tuning.sat_loss && w_rtt - base >= tuning.sat_rtt_rise_ms {
                self.saturated = true;
                self.level = 1.0 - tuning.cap_cut;
                self.last_recover = now;
                self.window.clear();
                let upload = if measured_send_kbps.is_finite() { measured_send_kbps.max(0.0).round() as u32 } else { 0 };
                return Some(SaturationChange::Saturated { upload_kbps: upload });
            }
        }
        if !self.saturated { return None; }
        if loss >= tuning.sat_recover_below_loss {
            self.last_recover = now;
            return None;
        }
        if now.saturating_sub(self.last_recover).as_secs_f32() >= tuning.sat_recover_every_s {
            self.last_recover = now;
            self.level *= 1.0 + tuning.sat_recover;
            if self.level >= 0.99 { self.clear_pending = true; }
            return Some(SaturationChange::Recover);
        }
        None
    }
}

/// The driver's tick-time meter: p95 of sim tick wall time over the last 2 s and the achieved tick rate
/// (shown to every player, L1025). The ladder gate injects synthetic cost by adding it to each recorded tick.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct HostLoadMeter { pub(crate) ticks: std::collections::VecDeque<(Duration, f32)>, pub(crate) synthetic_ms: f32 }
impl HostLoadMeter {
    pub fn new() -> Self { Self::default() }
    pub fn set_synthetic_ms(&mut self, ms: f32) { self.synthetic_ms = ms; }
    /// Record one tick's wall time. Unit: `net-quality`.
    ///
    /// The synthetic cost is added to every recorded tick. Ticks older than the 2 s p95 window (relative to
    /// this one) are pruned here, because [`HostLoadMeter::load`] has no clock.
    pub fn record(&mut self, now: Duration, tick_ms: f32) {
        let ms = if tick_ms.is_finite() { tick_ms.max(0.0) } else { 0.0 } + self.synthetic_ms.max(0.0);
        self.ticks.push_back((now, ms));
        while let Some(&(t, _)) = self.ticks.front() {
            if now.saturating_sub(t) > LOAD_WINDOW { self.ticks.pop_front(); } else { break; }
        }
        // A runaway caller cannot grow the window without bound (2 s at 60 Hz is 120 entries).
        while self.ticks.len() > 8192 { self.ticks.pop_front(); }
    }
    /// Load for `Sim::set_host_load`. Unit: `net-quality`.
    pub fn load(&self, backlog_ticks: u32) -> HostLoad {
        let mut v: Vec<f32> = self.ticks.iter().map(|&(_, ms)| ms).collect();
        if v.is_empty() { return HostLoad { tick_p95_ms: 0.0, backlog_ticks }; }
        v.sort_by(f32::total_cmp);
        // Nearest rank: the smallest value with at least 95% of samples at or below it.
        let rank = ((v.len() as f64 * 0.95).ceil() as usize).clamp(1, v.len());
        HostLoad { tick_p95_ms: v[rank - 1], backlog_ticks }
    }
    /// Ticks per second over the last second. Unit: `net-quality`.
    pub fn tick_hz(&self, now: Duration) -> f32 {
        self.ticks.iter().filter(|&&(t, _)| t <= now && now.saturating_sub(t) < Duration::from_secs(1)).count() as f32
    }
}

/// Flushed entries kept in memory for the diagnostics view.
pub const LOG_KEEP: usize = 1024;
/// p95 window of the host load meter (L1021).
const LOAD_WINDOW: Duration = Duration::from_secs(2);

fn median(it: impl Iterator<Item = f32>) -> f32 {
    let mut v: Vec<f32> = it.collect();
    if v.is_empty() { return 0.0; }
    v.sort_by(f32::total_cmp);
    let n = v.len();
    if n % 2 == 1 { v[n / 2] } else { (v[n / 2 - 1] + v[n / 2]) * 0.5 }
}

/// One line of the local connection log (L1039). Developer-facing diagnostics, stored as RON lines; never
/// uploaded anywhere.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LogEntry { pub at_s: f64, pub kind: LogKind }
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum LogKind {
    Joined { client_id: u64, slot: u8 },
    Refused { client_id: u64, reason: crate::error::JoinRefusal },
    Left { client_id: u64 },
    Warning { client_id: u64, warning: DropWarning, sample: LinkSample },
    Dropped { client_id: u64, reason: DropReason, sample: LinkSample },
    Saturation(SaturationChange),
    LadderStep { rung: u8, sub_step: u8, up: bool, tick_p95_ms: f32 },
    PortMapping(crate::portmap::MappingOutcome),
    HostLost { last_packet_s: f64 },
    Rehosted { from_tick: u64 },
}

/// Bounded in-memory log; the app appends it to the file in the user data directory.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ConnectionLog { pub entries: Vec<LogEntry>, pub flushed: usize }
impl ConnectionLog {
    pub fn push(&mut self, at: Duration, kind: LogKind) { self.entries.push(LogEntry { at_s: at.as_secs_f64(), kind }); }
    /// Entries not yet written out, as RON lines; marks them flushed. Unit: `net-quality`.
    ///
    /// One entry per line (compact RON, so strings with newlines are escaped). Once flushed, all but the
    /// newest [`LOG_KEEP`] entries are dropped from memory, which keeps a long session's log bounded.
    pub fn take_unflushed_ron(&mut self) -> String {
        let start = self.flushed.min(self.entries.len());
        let mut out = String::new();
        for e in &self.entries[start..] {
            if let Ok(line) = ron::to_string(e) {
                out.push_str(&line);
                out.push('\n');
            }
        }
        self.flushed = self.entries.len();
        if self.entries.len() > LOG_KEEP {
            let drop = self.entries.len() - LOG_KEEP;
            self.entries.drain(..drop);
            self.flushed -= drop;
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn secs(s: f32) -> Duration { Duration::from_secs_f32(s) }
    fn sample(rtt_ms: f32, loss: f32) -> LinkSample { LinkSample { rtt_ms, loss, ..Default::default() } }

    #[test]
    fn cap_rises_ten_percent_per_ten_clean_seconds_and_cuts_once_per_window() {
        let t = QualityTuning::default();
        let mut c = BandwidthCap::new(&t, secs(0.0));
        assert_eq!(c.kbps(), 256.0);
        let mut now = 0.0;
        while now < 10.05 { c.update(secs(now), &sample(50.0, 0.0), &t); now += 0.1; }
        assert!((c.kbps() - 281.6).abs() < 0.01, "{}", c.kbps());
        // Loss of 1% or more stops the raise clock.
        while now < 19.0 { c.update(secs(now), &sample(50.0, 0.0), &t); now += 0.1; }
        c.update(secs(now), &sample(50.0, 0.02), &t);
        let before = c.kbps();
        for _ in 0..50 { now += 0.1; c.update(secs(now), &sample(50.0, 0.0), &t); }
        assert_eq!(c.kbps(), before);
        // Sustained 10% loss: one cut per 2 s window, never below the floor.
        let start = c.kbps();
        let t0 = now;
        while now < t0 + 2.05 { now += 0.1; c.update(secs(now), &sample(50.0, 0.10), &t); }
        assert!((c.kbps() - start * 0.75).abs() < 0.01);
        for _ in 0..400 { now += 0.1; c.update(secs(now), &sample(50.0, 0.10), &t); }
        assert_eq!(c.kbps(), 128.0);
        c.scale(100.0, &t);
        assert_eq!(c.kbps(), 768.0);
        c.scale(f32::NAN, &t);
        assert_eq!(c.kbps(), 768.0);
        assert_eq!(c.bytes_per_interval(30), 3200);
    }

    #[test]
    fn drop_rule_warns_at_5s_drops_at_15s_and_tolerates_noise() {
        let t = QualityTuning::default();
        let mut m = DropMonitor::new();
        let mut first_warn = None;
        let mut dropped = None;
        let mut i = 0u32;
        while i < 300 && dropped.is_none() {
            // 25% loss reads under 20% every fourth sample (samples every 100 ms, the first one clean).
            let now = i as f32 * 0.1;
            let loss = if i.is_multiple_of(4) { 0.18 } else { 0.25 };
            match m.update(secs(now), &sample(100.0, loss), &t) {
                Verdict::Warn(DropWarning::PacketLoss) if first_warn.is_none() => first_warn = Some(now),
                Verdict::Drop(r) => { assert_eq!(r, DropReason::PacketLoss); dropped = Some(now); }
                _ => {}
            }
            i += 1;
        }
        // The condition starts at the 0.1 s sample; f32 time may put each verdict one sample later.
        assert!((5.1..5.25).contains(&first_warn.unwrap()), "{first_warn:?}");
        assert!((15.1..15.25).contains(&dropped.unwrap()), "{dropped:?}");
        // 300 ms, 10% loss, 50 ms jitter for a long time: never dropped or even warned.
        let mut m = DropMonitor::new();
        for i in 0..36_000 {
            let jitter = ((i * 7919) % 101) as f32 - 50.0;
            let loss = if i % 37 == 0 { 0.22 } else { 0.10 };
            assert_eq!(m.update(secs(i as f32 * 0.1), &sample(300.0 + jitter, loss), &t), Verdict::Fine);
        }
        // 600 ms is dropped within 20 s; a recovered link clears after the grace.
        let mut m = DropMonitor::new();
        let mut v = Verdict::Fine;
        for i in 0..=151 { v = m.update(secs(i as f32 * 0.1), &sample(600.0, 0.0), &t); }
        assert_eq!(v, Verdict::Drop(DropReason::HighPing));
        let mut m = DropMonitor::new();
        for i in 0..100 { m.update(secs(i as f32 * 0.1), &LinkSample { frames_behind: 31, ..sample(50.0, 0.0) }, &t); }
        for i in 100..120 { m.update(secs(i as f32 * 0.1), &sample(50.0, 0.0), &t); }
        assert_eq!(m.update(secs(12.0), &LinkSample { frames_behind: 31, ..sample(50.0, 0.0) }, &t), Verdict::Fine);
        let f = QualityTuning::drop_fallback();
        assert_eq!((f.hold_s, f.drop_loss), (30.0, 0.25));
    }

    #[test]
    fn quality_buckets() {
        let t = QualityTuning::default();
        assert_eq!(quality_of(&sample(80.0, 0.0), 256.0, &t), LinkQuality::Good);
        assert_eq!(quality_of(&sample(300.0, 0.0), 256.0, &t), LinkQuality::Fair);
        assert_eq!(quality_of(&sample(80.0, 0.0), 192.0, &t), LinkQuality::Fair);
        assert_eq!(quality_of(&sample(80.0, 0.3), 256.0, &t), LinkQuality::Poor);
        assert_eq!(quality_of(&sample(80.0, 0.08), 128.0, &t), LinkQuality::Poor);
        assert_eq!(quality_of(&sample(f32::NAN, 0.0), 256.0, &t), LinkQuality::Poor);
    }

    #[test]
    fn saturation_detects_recovers_and_clears() {
        let t = QualityTuning::default();
        let mut d = SaturationDetector::new();
        let mut now = 0.0f32;
        let clients = |rtt, loss| vec![sample(rtt, loss); 7];
        // First minute: 60 ms baseline; no detection before it is set even with bad readings.
        while now < 60.0 { assert_eq!(d.update(secs(now), &clients(60.0, if now > 40.0 { 0.1 } else { 0.0 }), 900.0, &t), None); now += 0.5; }
        // RTT up 40 ms only: not saturated.
        let t0 = now;
        while now < t0 + 12.0 { assert_eq!(d.update(secs(now), &clients(100.0, 0.08), 900.0, &t), None); now += 0.5; }
        // Up 80 ms with 8% loss: saturated once the 10 s window is full.
        let t0 = now;
        let mut hit = None;
        while now < t0 + 12.0 {
            if let Some(c) = d.update(secs(now), &clients(140.0, 0.08), 812.4, &t) { hit = Some((now - t0, c)); break; }
            now += 0.5;
        }
        let (after, change) = hit.unwrap();
        // The window median turns once most of the last 10 s reads saturated.
        assert!(after <= 10.5, "{after}");
        assert_eq!(change, SaturationChange::Saturated { upload_kbps: 812 });
        assert!(d.saturated());
        // Recovery: +10% each 30 s under 2% loss; a bad reading restarts the wait; cleared when back.
        let mut recovers = 0;
        let mut cleared_at = None;
        let t0 = now;
        while now < t0 + 200.0 {
            now += 0.5;
            let loss = if (now - t0 - 15.0).abs() < 0.1 { 0.03 } else { 0.0 };
            match d.update(secs(now), &clients(70.0, loss), 700.0, &t) {
                Some(SaturationChange::Recover) => recovers += 1,
                Some(SaturationChange::Cleared) => { cleared_at = Some(now - t0); break; }
                Some(other) => panic!("{other:?}"),
                None => {}
            }
        }
        assert_eq!(recovers, 3);
        let c = cleared_at.unwrap();
        assert!(c > 105.0 && c < 106.0, "{c}");
        assert!(!d.saturated());
    }

    #[test]
    fn load_meter_p95_and_rate() {
        let mut m = HostLoadMeter::new();
        for i in 0..240 { m.record(Duration::from_micros(i * 16_667), if i % 10 == 0 { 9.0 } else { 3.0 }); }
        let l = m.load(4);
        assert_eq!(l.backlog_ticks, 4);
        assert_eq!(l.tick_p95_ms, 9.0);
        assert!((m.tick_hz(Duration::from_micros(239 * 16_667)) - 60.0).abs() <= 1.0);
        m.set_synthetic_ms(5.0);
        for i in 240..400 { m.record(Duration::from_micros(i * 16_667), 3.0); }
        assert_eq!(m.load(0).tick_p95_ms, 8.0);
        assert_eq!(HostLoadMeter::new().load(0).tick_p95_ms, 0.0);
    }

    #[test]
    fn log_lines_are_ron_and_bounded() {
        let mut log = ConnectionLog::default();
        log.push(secs(1.5), LogKind::Joined { client_id: 7, slot: 1 });
        log.push(secs(2.0), LogKind::Dropped { client_id: 7, reason: DropReason::HighPing, sample: sample(600.0, 0.0) });
        let text = log.take_unflushed_ron();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 2);
        let back: LogEntry = ron::from_str(lines[1]).unwrap();
        assert_eq!(back, log.entries[1]);
        assert_eq!(log.take_unflushed_ron(), "");
        for i in 0..3000 { log.push(secs(i as f32), LogKind::Left { client_id: i }); }
        assert_eq!(log.take_unflushed_ron().lines().count(), 3000);
        assert_eq!(log.entries.len(), LOG_KEEP);
        assert_eq!(log.flushed, LOG_KEEP);
    }
}
