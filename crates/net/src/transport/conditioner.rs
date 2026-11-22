//! Unit `net-conditioner`. The link conditioner (L990): latency, jitter, Gilbert-Elliott burst loss and an
//! upstream limit, wrapping any renet2_netcode socket in tests, soak runs and nightly bot matches (L901: all
//! on one hosted runner over loopback UDP). Gate profiles: L915 (150 ms, 5% loss, 30 ms jitter), L1031 (host
//! upload limited to 1 Mbps with 7 bot clients), L1036 (300 ms, 10% loss, 50 ms jitter never dropped; 600 ms
//! or 25% loss dropped within 20 s).
//!
//! Delay applies to outgoing packets (each end conditions its own sends; a profile's latency is one way, so
//! RTT is twice it when both ends use it). Randomness is a small seeded generator private to this file, so a
//! run is repeatable for its seed. Time comes from the shared [`crate::clock::Clock`].
use crate::clock::Clock;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// Gilbert-Elliott two-state loss model.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct BurstLoss { pub p_good_to_bad: f32, pub p_bad_to_good: f32, pub loss_good: f32, pub loss_bad: f32 }
impl BurstLoss {
    pub const NONE: BurstLoss = BurstLoss { p_good_to_bad: 0.0, p_bad_to_good: 1.0, loss_good: 0.0, loss_bad: 0.0 };
    /// A burst model whose long-run mean loss is `mean` with mean burst length of 3 packets. Unit: `net-conditioner`.
    pub fn with_mean(mean: f32) -> BurstLoss {
        if mean.is_nan() || mean <= 0.0 {
            return BurstLoss::NONE;
        }
        if mean >= 1.0 {
            return BurstLoss { p_good_to_bad: 1.0, p_bad_to_good: 0.0, loss_good: 1.0, loss_bad: 1.0 };
        }
        // Simple Gilbert model: the bad state loses every packet, the good state none. The stationary share of
        // the bad state is p_gb / (p_gb + p_bg) = mean; bursts last 1 / p_bg = 3 packets on average. Above 75%
        // mean loss p_gb would pass 1, so it saturates and the bursts lengthen instead, keeping the mean exact.
        let p_bad_to_good = 1.0 / MEAN_BURST_PACKETS;
        let p_good_to_bad = mean * p_bad_to_good / (1.0 - mean);
        if p_good_to_bad <= 1.0 {
            BurstLoss { p_good_to_bad, p_bad_to_good, loss_good: 0.0, loss_bad: 1.0 }
        } else {
            BurstLoss { p_good_to_bad: 1.0, p_bad_to_good: (1.0 - mean) / mean, loss_good: 0.0, loss_bad: 1.0 }
        }
    }
    /// Long-run fraction of packets this model loses.
    pub fn mean(&self) -> f32 {
        let total = self.p_good_to_bad + self.p_bad_to_good;
        if total <= 0.0 {
            return self.loss_good;
        }
        let bad = self.p_good_to_bad / total;
        bad * self.loss_bad + (1.0 - bad) * self.loss_good
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct ConditionerProfile { pub latency_ms: f32, pub jitter_ms: f32, pub loss: BurstLoss, /// 0 = unlimited.
    pub upstream_kbps: u32 }
impl ConditionerProfile {
    pub const CLEAN: ConditionerProfile = ConditionerProfile { latency_ms: 0.0, jitter_ms: 0.0, loss: BurstLoss::NONE, upstream_kbps: 0 };
    /// One-way profile for a target RTT, mean loss and jitter (latency = rtt/2). Unit: `net-conditioner`.
    pub fn round_trip(rtt_ms: f32, mean_loss: f32, jitter_ms: f32) -> ConditionerProfile {
        ConditionerProfile {
            latency_ms: finite_non_negative(rtt_ms) * 0.5,
            jitter_ms: finite_non_negative(jitter_ms),
            loss: BurstLoss::with_mean(mean_loss),
            upstream_kbps: 0,
        }
    }
    /// The same profile with an upstream limit.
    pub fn with_upstream(mut self, kbps: u32) -> ConditionerProfile {
        self.upstream_kbps = kbps;
        self
    }
}

/// What a backend constructor needs to wrap its socket.
#[derive(Clone)]
pub struct Conditioning { pub profile: ConditionerProfile, pub seed: u64, pub clock: Arc<dyn Clock> }

/// Wraps a server socket. Unit: `net-conditioner`.
#[derive(Debug)]
pub struct ConditionedServerSocket<S: renet2_netcode::ServerSocket> { pub(crate) inner: S, pub(crate) state: ConditionerState }
/// Wraps a client socket. Unit: `net-conditioner`.
#[derive(Debug)]
pub struct ConditionedClientSocket<S: renet2_netcode::ClientSocket> { pub(crate) inner: S, pub(crate) state: ConditionerState }

/// Delay queue, loss state, token bucket and RNG.
pub struct ConditionerState {
    pub(crate) profile: ConditionerProfile,
    pub(crate) clock: Arc<dyn Clock>,
    pub(crate) rng: u64,
    pub(crate) bad: bool,
    pub(crate) queue: std::collections::BinaryHeap<std::cmp::Reverse<(u64, u64, std::net::SocketAddr, Vec<u8>)>>,
    pub(crate) bucket_bytes: f64,
    pub(crate) last_refill_ns: u64,
    /// Tie-break for packets due at the same nanosecond: send order.
    pub(crate) seq: u64,
    pub(crate) counters: ConditionerCounters,
}

/// What the conditioner did with the packets it was given.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ConditionerCounters { pub offered: u64, pub lost: u64, pub tail_dropped: u64, pub delivered: u64 }

/// Mean burst length of [`BurstLoss::with_mean`], packets.
const MEAN_BURST_PACKETS: f32 = 3.0;
/// IPv4 plus UDP header bytes, so the upstream limit counts what the wire carries.
const UDP_IP_OVERHEAD_BYTES: usize = 28;
/// The bottleneck queue holds this much time at the line rate before it tail-drops (a typical home router
/// buffer, so a saturated upload shows both added delay and loss, as the saturation rule expects, L1031).
const MAX_QUEUE_S: f64 = 0.25;

/// renetcode's largest datagram.
const MAX_PACKET_BYTES: f64 = 1400.0;

/// Token bucket depth: 10 ms at the line rate, at least two full packets.
fn burst_bytes(kbps: u32) -> f64 {
    let rate = f64::from(kbps) * 1000.0 / 8.0;
    (rate * 0.010).max(2.0 * MAX_PACKET_BYTES)
}

fn finite_non_negative(v: f32) -> f32 { if v.is_finite() && v > 0.0 { v } else { 0.0 } }

/// A delayed packet's send error has no caller left to report to; on a lossy test link it is one more loss.
fn forward(r: Result<(), renet2_netcode::NetcodeTransportError>) {
    let _ = r;
}
impl std::fmt::Debug for ConditionerState { fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { f.debug_struct("ConditionerState").field("profile", &self.profile).finish() } }

impl ConditionerState {
    /// Unit: `net-conditioner`.
    pub fn new(c: &Conditioning) -> Self {
        let now = c.clock.now().as_nanos() as u64;
        let mut profile = c.profile;
        profile.latency_ms = finite_non_negative(profile.latency_ms);
        profile.jitter_ms = finite_non_negative(profile.jitter_ms);
        ConditionerState {
            profile,
            clock: c.clock.clone(),
            rng: c.seed ^ 0x6C69_6E6B_636F_6E64, // "linkcond"
            bad: false,
            queue: std::collections::BinaryHeap::new(),
            bucket_bytes: burst_bytes(profile.upstream_kbps),
            last_refill_ns: now,
            seq: 0,
            counters: ConditionerCounters::default(),
        }
    }

    /// Packets offered, dropped by the loss model, dropped by the full upstream queue, and delivered so far.
    pub fn counters(&self) -> ConditionerCounters { self.counters }

    fn now_ns(&self) -> u64 { self.clock.now().as_nanos() as u64 }

    /// splitmix64: small, fast and fully determined by the seed.
    fn next_u64(&mut self) -> u64 {
        self.rng = self.rng.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.rng;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Uniform in [0, 1).
    fn unit(&mut self) -> f64 { (self.next_u64() >> 11) as f64 * (1.0 / (1u64 << 53) as f64) }

    /// Step the Gilbert-Elliott chain one packet and decide whether this packet is lost.
    fn lose(&mut self) -> bool {
        let l = self.profile.loss;
        if self.bad {
            if self.unit() < f64::from(l.p_bad_to_good) {
                self.bad = false;
            }
        } else if self.unit() < f64::from(l.p_good_to_bad) {
            self.bad = true;
        }
        let p = if self.bad { l.loss_bad } else { l.loss_good };
        p > 0.0 && self.unit() < f64::from(p)
    }

    /// Admit a packet of `len` bytes to the upstream token bucket at `now`. Returns the queueing delay in ns, or
    /// `None` when the bottleneck queue is full and the packet is tail-dropped.
    fn shape(&mut self, now: u64, len: usize) -> Option<u64> {
        let kbps = self.profile.upstream_kbps;
        if kbps == 0 {
            return Some(0);
        }
        let rate = f64::from(kbps) * 1000.0 / 8.0; // bytes per second
        let elapsed = now.saturating_sub(self.last_refill_ns) as f64 * 1e-9;
        self.last_refill_ns = now;
        self.bucket_bytes = (self.bucket_bytes + elapsed * rate).min(burst_bytes(kbps));
        let wire = (len + UDP_IP_OVERHEAD_BYTES) as f64;
        let after = self.bucket_bytes - wire;
        if -after > rate * MAX_QUEUE_S {
            return None;
        }
        self.bucket_bytes = after;
        Some(if after < 0.0 { (-after / rate * 1e9) as u64 } else { 0 })
    }

    /// One-way delay for the next packet: latency plus uniform jitter in [-jitter, +jitter], never negative.
    fn delay_ns(&mut self) -> u64 {
        let base = f64::from(self.profile.latency_ms);
        let j = f64::from(self.profile.jitter_ms);
        let ms = if j > 0.0 { base + j * (2.0 * self.unit() - 1.0) } else { base };
        (ms.max(0.0) * 1e6) as u64
    }

    /// Decide the fate of one outgoing packet: `Some(due_ns)` to deliver at `due_ns`, `None` if dropped.
    fn admit(&mut self, len: usize) -> Option<u64> {
        let now = self.now_ns();
        self.counters.offered += 1;
        if self.lose() {
            self.counters.lost += 1;
            return None;
        }
        let Some(queued) = self.shape(now, len) else {
            self.counters.tail_dropped += 1;
            return None;
        };
        Some(now + queued + self.delay_ns())
    }

    /// Queue or pass through one packet. `send_now` sends it on the inner socket.
    fn handle(&mut self, addr: std::net::SocketAddr, packet: &[u8], send_now: impl FnOnce(std::net::SocketAddr, &[u8])) {
        let Some(due) = self.admit(packet.len()) else { return };
        if self.queue.is_empty() && due <= self.now_ns() {
            self.counters.delivered += 1;
            send_now(addr, packet);
            return;
        }
        self.seq += 1;
        self.queue.push(std::cmp::Reverse((due, self.seq, addr, packet.to_vec())));
    }

    /// Hand every packet whose time has come to `send`, in due order (ties in send order).
    fn release(&mut self, mut send: impl FnMut(std::net::SocketAddr, &[u8])) {
        let now = self.now_ns();
        while let Some(std::cmp::Reverse((due, ..))) = self.queue.peek() {
            if *due > now {
                break;
            }
            let Some(std::cmp::Reverse((_, _, addr, bytes))) = self.queue.pop() else { break };
            self.counters.delivered += 1;
            send(addr, &bytes);
        }
    }
}

impl<S: renet2_netcode::ServerSocket> ConditionedServerSocket<S> {
    pub fn new(inner: S, c: &Conditioning) -> Self { Self { inner, state: ConditionerState::new(c) } }
}
impl<S: renet2_netcode::ClientSocket> ConditionedClientSocket<S> {
    pub fn new(inner: S, c: &Conditioning) -> Self { Self { inner, state: ConditionerState::new(c) } }
}

impl<S: renet2_netcode::ServerSocket> renet2_netcode::ServerSocket for ConditionedServerSocket<S> {
    fn is_encrypted(&self) -> bool { self.inner.is_encrypted() }
    fn is_reliable(&self) -> bool { self.inner.is_reliable() }
    fn addr(&self) -> std::io::Result<std::net::SocketAddr> { self.inner.addr() }
    fn is_closed(&mut self) -> bool { self.inner.is_closed() }
    fn close(&mut self) { self.inner.close() }
    fn connection_denied(&mut self, addr: std::net::SocketAddr) { self.inner.connection_denied(addr) }
    fn connection_accepted(&mut self, client_id: u64, addr: std::net::SocketAddr) { self.inner.connection_accepted(client_id, addr) }
    fn disconnect(&mut self, addr: std::net::SocketAddr) { self.inner.disconnect(addr) }
    /// Release due packets from the delay queue to the inner socket. Unit: `net-conditioner`.
    fn preupdate(&mut self) {
        let inner = &mut self.inner;
        self.state.release(|a, p| forward(inner.send(a, p)));
        self.inner.preupdate();
    }
    fn try_recv(&mut self, buffer: &mut [u8]) -> std::io::Result<(usize, std::net::SocketAddr)> { self.inner.try_recv(buffer) }
    fn postupdate(&mut self) {
        self.inner.postupdate();
        let inner = &mut self.inner;
        self.state.release(|a, p| forward(inner.send(a, p)));
    }
    /// Drop, delay or rate-limit the packet. Unit: `net-conditioner`.
    fn send(&mut self, addr: std::net::SocketAddr, packet: &[u8]) -> Result<(), renet2_netcode::NetcodeTransportError> {
        let inner = &mut self.inner;
        let mut result = Ok(());
        self.state.handle(addr, packet, |a, p| result = inner.send(a, p));
        result
    }
}

impl<S: renet2_netcode::ClientSocket> renet2_netcode::ClientSocket for ConditionedClientSocket<S> {
    fn is_encrypted(&self) -> bool { self.inner.is_encrypted() }
    fn is_reliable(&self) -> bool { self.inner.is_reliable() }
    fn addr(&self) -> std::io::Result<std::net::SocketAddr> { self.inner.addr() }
    fn is_closed(&mut self) -> bool { self.inner.is_closed() }
    fn close(&mut self) { self.inner.close() }
    fn preupdate(&mut self) {
        let inner = &mut self.inner;
        self.state.release(|a, p| forward(inner.send(a, p)));
        self.inner.preupdate();
    }
    fn try_recv(&mut self, buffer: &mut [u8]) -> std::io::Result<(usize, std::net::SocketAddr)> { self.inner.try_recv(buffer) }
    fn postupdate(&mut self) {
        self.inner.postupdate();
        let inner = &mut self.inner;
        self.state.release(|a, p| forward(inner.send(a, p)));
    }
    fn send(&mut self, addr: std::net::SocketAddr, packet: &[u8]) -> Result<(), renet2_netcode::NetcodeTransportError> {
        let inner = &mut self.inner;
        let mut result = Ok(());
        self.state.handle(addr, packet, |a, p| result = inner.send(a, p));
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::ManualClock;
    use renet2_netcode::{ClientSocket, NetcodeTransportError};
    use std::net::SocketAddr;
    use std::sync::Mutex;
    use std::time::Duration;

    /// Records what reaches the wire and when.
    #[derive(Debug)]
    struct Wire { clock: ManualClock, sent: Arc<Mutex<Vec<(u64, usize, u8)>>> }
    impl ClientSocket for Wire {
        fn is_encrypted(&self) -> bool { false }
        fn is_reliable(&self) -> bool { false }
        fn addr(&self) -> std::io::Result<SocketAddr> { Ok("127.0.0.1:1".parse().unwrap()) }
        fn is_closed(&mut self) -> bool { false }
        fn close(&mut self) {}
        fn preupdate(&mut self) {}
        fn try_recv(&mut self, _: &mut [u8]) -> std::io::Result<(usize, SocketAddr)> { Err(std::io::ErrorKind::WouldBlock.into()) }
        fn postupdate(&mut self) {}
        fn send(&mut self, _: SocketAddr, packet: &[u8]) -> Result<(), NetcodeTransportError> {
            let t = self.clock.now().as_nanos() as u64;
            self.sent.lock().unwrap().push((t, packet.len(), packet[0]));
            Ok(())
        }
    }

    fn socket(profile: ConditionerProfile, seed: u64) -> (ConditionedClientSocket<Wire>, ManualClock, Arc<Mutex<Vec<(u64, usize, u8)>>>) {
        let clock = ManualClock::new();
        let sent = Arc::new(Mutex::new(Vec::new()));
        let wire = Wire { clock: clock.clone(), sent: sent.clone() };
        let c = Conditioning { profile, seed, clock: Arc::new(clock.clone()) };
        (ConditionedClientSocket::new(wire, &c), clock, sent)
    }

    fn to() -> SocketAddr { "127.0.0.1:2".parse().unwrap() }

    #[test]
    fn clean_profile_passes_straight_through() {
        let (mut s, _clock, sent) = socket(ConditionerProfile::CLEAN, 1);
        for i in 0..100u8 {
            s.send(to(), &[i; 100]).unwrap();
        }
        let sent = sent.lock().unwrap();
        assert_eq!(sent.len(), 100);
        assert!(sent.iter().enumerate().all(|(i, p)| p.2 == i as u8 && p.0 == 0));
    }

    #[test]
    fn latency_holds_packets_until_due() {
        let p = ConditionerProfile::round_trip(150.0, 0.0, 0.0);
        assert_eq!(p.latency_ms, 75.0);
        let (mut s, clock, sent) = socket(p, 1);
        s.send(to(), &[7; 10]).unwrap();
        clock.advance(Duration::from_millis(74));
        s.preupdate();
        assert!(sent.lock().unwrap().is_empty());
        clock.advance(Duration::from_millis(1));
        s.preupdate();
        assert_eq!(sent.lock().unwrap().as_slice(), &[(75_000_000, 10, 7)]);
    }

    #[test]
    fn jitter_stays_in_band_and_averages_to_latency() {
        let p = ConditionerProfile { latency_ms: 75.0, jitter_ms: 30.0, loss: BurstLoss::NONE, upstream_kbps: 0 };
        let (mut s, clock, sent) = socket(p, 9);
        let n = 4000u64;
        for i in 0..n {
            clock.set(Duration::from_millis(i));
            s.send(to(), &[0; 10]).unwrap();
            s.preupdate();
        }
        for i in n..n + 200 {
            clock.set(Duration::from_millis(i));
            s.preupdate();
        }
        let sent = sent.lock().unwrap();
        assert_eq!(sent.len() as u64, n);
        // Packets are sent 1 ms apart; recover each one's delay from its arrival order is not possible after
        // reordering, so check the arrival-time envelope and the mean shift instead.
        let mean_arrival: f64 = sent.iter().map(|p| p.0 as f64 * 1e-6).sum::<f64>() / n as f64;
        let mean_send = (n - 1) as f64 / 2.0;
        assert!((mean_arrival - mean_send - 75.0).abs() < 2.0, "mean delay {}", mean_arrival - mean_send);
        let first = sent.iter().map(|p| p.0).min().unwrap();
        assert!(first >= 45_000_000);
    }

    #[test]
    fn burst_loss_hits_its_mean_with_three_packet_bursts() {
        for mean in [0.05f32, 0.10, 0.25] {
            let loss = BurstLoss::with_mean(mean);
            assert!((loss.mean() - mean).abs() < 1e-6);
            let p = ConditionerProfile { latency_ms: 0.0, jitter_ms: 0.0, loss, upstream_kbps: 0 };
            let (mut s, _clock, sent) = socket(p, 42);
            let n = 200_000usize;
            for i in 0..n {
                s.send(to(), &[(i % 251) as u8; 8]).unwrap();
            }
            let got = sent.lock().unwrap();
            let lost = n - got.len();
            let rate = lost as f32 / n as f32;
            assert!((rate - mean).abs() < 0.01, "mean {mean}: measured {rate}");
            // Count bursts: runs of consecutive missing sequence numbers.
            let mut delivered = vec![false; n];
            let mut k = 0;
            for i in 0..n {
                if k < got.len() && got[k].2 == (i % 251) as u8 {
                    delivered[i] = true;
                    k += 1;
                }
            }
            let bursts = (0..n).filter(|&i| !delivered[i] && (i == 0 || delivered[i - 1])).count();
            let mean_burst = lost as f32 / bursts as f32;
            assert!((mean_burst - 3.0).abs() < 0.25, "mean {mean}: burst {mean_burst}");
        }
        assert_eq!(BurstLoss::with_mean(0.0), BurstLoss::NONE);
        assert_eq!(BurstLoss::with_mean(f32::NAN), BurstLoss::NONE);
        assert!((BurstLoss::with_mean(0.9).mean() - 0.9).abs() < 1e-6);
    }

    #[test]
    fn upstream_limit_holds_the_line_rate_and_tail_drops() {
        let p = ConditionerProfile::CLEAN.with_upstream(1000);
        let (mut s, clock, sent) = socket(p, 3);
        // Offer 2 Mbps for 10 s: 1,222 + 28 bytes every 5 ms.
        for i in 0..2000u64 {
            clock.set(Duration::from_millis(i * 5));
            s.send(to(), &[0; 1222]).unwrap();
            s.preupdate();
        }
        let delivered_by_10s = sent.lock().unwrap().iter().filter(|p| p.0 <= 10_000_000_000).count();
        let kbps = delivered_by_10s as f64 * 1250.0 * 8.0 / 10.0 / 1000.0;
        assert!((kbps - 1000.0).abs() < 30.0, "throughput {kbps} kbps");
        let c = s.state.counters();
        assert!(c.tail_dropped > 800, "{c:?}");
        // Queueing delay never passes the buffer.
        clock.advance(Duration::from_secs(2));
        s.postupdate();
        assert_eq!(s.state.counters().delivered, c.offered - c.tail_dropped);
    }

    #[test]
    fn same_seed_same_run() {
        let p = ConditionerProfile::round_trip(300.0, 0.10, 50.0);
        let run = |seed| {
            let (mut s, clock, sent) = socket(p, seed);
            for i in 0..2000u64 {
                clock.set(Duration::from_millis(i));
                s.send(to(), &[(i % 256) as u8; 20]).unwrap();
                s.preupdate();
            }
            clock.advance(Duration::from_secs(1));
            s.preupdate();
            let v = sent.lock().unwrap().clone();
            v
        };
        assert_eq!(run(5), run(5));
        assert_ne!(run(5), run(6));
    }
}
