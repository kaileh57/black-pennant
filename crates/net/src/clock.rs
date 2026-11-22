//! Contract-owned (no work unit). The one place net reads time. Sessions take a `Box<dyn Clock>` so tests,
//! soak runs and the ladder gate with synthetic tick cost (L1027) drive time by hand; the binaries and gdclient
//! pass [`SystemClock`]. Time is a `Duration` since an arbitrary epoch fixed at clock creation.
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

pub trait Clock: Send + Sync {
    /// Monotonic time since the clock's epoch.
    fn now(&self) -> Duration;
    /// Wall time since the Unix epoch, for netcode connect-token timestamps only (renetcode2 needs it).
    fn unix_now(&self) -> Duration;
}

/// Real monotonic time. Real code.
#[derive(Debug)]
pub struct SystemClock { start: Instant }
impl SystemClock { pub fn new() -> Self { Self { start: Instant::now() } } }
impl Default for SystemClock { fn default() -> Self { Self::new() } }
impl Clock for SystemClock {
    fn now(&self) -> Duration { self.start.elapsed() }
    fn unix_now(&self) -> Duration { std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default() }
}

/// Hand-driven time shared between a test and the sessions it drives. Real code.
#[derive(Clone, Debug, Default)]
pub struct ManualClock { nanos: Arc<AtomicU64> }
impl ManualClock {
    pub fn new() -> Self { Self::default() }
    pub fn advance(&self, d: Duration) { self.nanos.fetch_add(d.as_nanos() as u64, Ordering::SeqCst); }
    pub fn set(&self, t: Duration) { self.nanos.store(t.as_nanos() as u64, Ordering::SeqCst); }
}
impl Clock for ManualClock {
    fn now(&self) -> Duration { Duration::from_nanos(self.nanos.load(Ordering::SeqCst)) }
    /// A fixed plausible Unix time plus the manual offset, so connect tokens stay valid in tests.
    fn unix_now(&self) -> Duration { Duration::from_secs(1_800_000_000) + self.now() }
}
