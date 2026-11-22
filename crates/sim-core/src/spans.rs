//! Per-phase wall-clock spans of a tick, for the tick gate's per-subsystem split (DESIGN.md "Performance
//! budgets": "A 10-minute reference battle runs headless with per-subsystem tracing spans", and the tick split
//! table). Reporting only: nothing in the simulation reads these numbers, they are never saved, hashed or
//! replicated, and budgets stay in work units. They are off unless the driver turns them on for its thread
//! ([`enable`]), so a game or replay pays one thread-local read per phase.
//!
//! The tick runs its phases on the thread that calls `step` (parallel phases fan out inside one phase and
//! join before the next), so one thread-local accumulator per stepping thread sees every phase. The driver
//! calls [`take`] after each step to read and clear the tick's figures.
use std::cell::Cell;
use std::time::Instant;

/// One timed phase, in tick order: the sim-systems phases, then sim-core's `World::step` sections.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u8)]
pub enum Span {
    /// Commands, arrivals, the load ladder and fidelity assignment.
    Schedule,
    Sensors,
    Tracks,
    Ai,
    /// Graph VM, officers and stages (including the gun solver), Luau.
    Control,
    Guidance,
    SpecOps,
    /// sim-core phase 3: networks and parts.
    Parts,
    /// sim-core phase 5: mass, buoyancy, drag and propulsion.
    Forces,
    /// sim-core phase 6: Rapier step, kinematics back, contacts.
    Physics,
    /// sim-core phase 7: small bodies and decoys.
    Bodies,
    /// sim-core phase 8: the projectile array.
    Projectiles,
    /// sim-core phase 9: shell hits and damage.
    Hits,
    /// sim-core phase 10: crew, flooding and fire.
    Systems,
    /// sim-core phase 11: splitting, compartment recompute, collider rebuilds.
    Structure,
    /// sim-core phase 12: status, losses and wrecks.
    Status,
    /// sim-systems events, reports and the snapshot bookkeeping after the world step.
    Events,
}

/// The rows of DESIGN.md's tick split table that the spans add up to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Budget { RigidBodies, Buoyancy, Projectiles, Damage, Sensors, Control, Ai, Other }

pub const COUNT: usize = 17;

impl Span {
    pub const ALL: [Span; COUNT] = [
        Span::Schedule, Span::Sensors, Span::Tracks, Span::Ai, Span::Control, Span::Guidance, Span::SpecOps, Span::Parts, Span::Forces,
        Span::Physics, Span::Bodies, Span::Projectiles, Span::Hits, Span::Systems, Span::Structure, Span::Status, Span::Events,
    ];
    pub fn name(self) -> &'static str {
        match self {
            Span::Schedule => "schedule",
            Span::Sensors => "sensors",
            Span::Tracks => "tracks",
            Span::Ai => "ai",
            Span::Control => "control",
            Span::Guidance => "guidance",
            Span::SpecOps => "specops",
            Span::Parts => "parts",
            Span::Forces => "forces",
            Span::Physics => "physics",
            Span::Bodies => "bodies",
            Span::Projectiles => "projectiles",
            Span::Hits => "hits",
            Span::Systems => "crew_flood_fire",
            Span::Structure => "structure",
            Span::Status => "status",
            Span::Events => "events",
        }
    }
    /// The tick split row this span counts toward.
    pub fn budget(self) -> Budget {
        match self {
            Span::Physics | Span::Structure => Budget::RigidBodies,
            Span::Forces => Budget::Buoyancy,
            Span::Bodies | Span::Projectiles => Budget::Projectiles,
            Span::Hits | Span::Systems | Span::Status => Budget::Damage,
            Span::Sensors | Span::Tracks => Budget::Sensors,
            Span::Control | Span::Guidance | Span::Parts => Budget::Control,
            Span::Ai => Budget::Ai,
            Span::Schedule | Span::SpecOps | Span::Events => Budget::Other,
        }
    }
}

impl Budget {
    pub const ALL: [Budget; 8] = [Budget::RigidBodies, Budget::Buoyancy, Budget::Projectiles, Budget::Damage, Budget::Sensors, Budget::Control, Budget::Ai, Budget::Other];
    pub fn name(self) -> &'static str {
        match self {
            Budget::RigidBodies => "rigid_bodies",
            Budget::Buoyancy => "buoyancy_waves",
            Budget::Projectiles => "projectiles",
            Budget::Damage => "damage_flooding",
            Budget::Sensors => "sensors",
            Budget::Control => "control",
            Budget::Ai => "ai",
            Budget::Other => "other",
        }
    }
    /// The row's share of the 8.0 ms tick, ms (replication and the reserve fall under `Other`).
    pub fn budget_ms(self) -> f64 {
        match self {
            Budget::RigidBodies => 1.5,
            Budget::Buoyancy => 1.25,
            Budget::Projectiles | Budget::Damage | Budget::Sensors | Budget::Control => 1.0,
            Budget::Ai => 0.5,
            Budget::Other => 0.75,
        }
    }
}

/// A clock reading in nanoseconds from an arbitrary origin.
pub type Clock = fn() -> u64;

/// The default clock: wall time since the first reading in this process.
pub fn wall_ns() -> u64 {
    static EPOCH: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    u64::try_from(EPOCH.get_or_init(Instant::now).elapsed().as_nanos()).unwrap_or(u64::MAX)
}

thread_local! {
    static ON: Cell<Option<Clock>> = const { Cell::new(None) };
    static NS: Cell<[u64; COUNT]> = const { Cell::new([0; COUNT]) };
}

/// Turn span timing on (wall clock) or off for the calling thread.
pub fn enable(on: bool) { ON.with(|c| c.set(on.then_some(wall_ns as Clock))); }

/// Turn span timing on with another clock, such as the thread's CPU time when the sim pool has one thread
/// (a driver on a loaded machine; sim-core has no portable thread clock of its own).
pub fn enable_with(clock: Clock) { ON.with(|c| c.set(Some(clock))); }

pub fn enabled() -> bool { ON.with(|c| c.get()).is_some() }

/// Read and clear the calling thread's span totals, nanoseconds, indexed like [`Span::ALL`].
pub fn take() -> [u64; COUNT] { NS.with(|c| c.replace([0; COUNT])) }

/// A lap timer: each [`Lap::mark`] charges the time since the previous mark (or the start) to one span.
/// Inert when spans are off.
pub struct Lap(Option<(Clock, u64)>);

impl Lap {
    pub fn start() -> Lap { Lap(ON.with(|c| c.get()).map(|clock| (clock, clock()))) }
    pub fn mark(&mut self, span: Span) {
        let Some((clock, t0)) = self.0 else { return };
        let now = clock();
        let ns = now.saturating_sub(t0);
        NS.with(|c| {
            let mut a = c.get();
            let i = span as usize;
            a[i] = a[i].saturating_add(ns);
            c.set(a);
        });
        self.0 = Some((clock, now));
    }
    /// Restart the lap without charging anything (time another timer already counted).
    pub fn skip(&mut self) { if let Some((clock, _)) = self.0 { self.0 = Some((clock, clock())); } }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spans_are_off_by_default_and_per_thread() {
        let mut lap = Lap::start();
        lap.mark(Span::Forces);
        assert_eq!(take(), [0; COUNT]);
        enable(true);
        let mut lap = Lap::start();
        std::thread::sleep(std::time::Duration::from_millis(1));
        lap.mark(Span::Forces);
        lap.skip();
        lap.mark(Span::Physics);
        let t = take();
        assert!(t[Span::Forces as usize] >= 1_000_000);
        assert!(t[Span::Physics as usize] < t[Span::Forces as usize]);
        assert_eq!(take(), [0; COUNT], "take clears");
        std::thread::spawn(|| assert!(!enabled())).join().unwrap();
        fn fake() -> u64 { 7 }
        enable_with(fake);
        let mut lap = Lap::start();
        lap.mark(Span::Ai);
        assert_eq!(take()[Span::Ai as usize], 0, "a stopped clock charges nothing");
        enable(false);
        assert!(!enabled());
    }

    #[test]
    fn every_span_has_a_budget_row() {
        for (i, s) in Span::ALL.iter().enumerate() { assert_eq!(*s as usize, i); assert!(!s.name().is_empty()); let _ = s.budget(); }
        let total: f64 = Budget::ALL.iter().map(|b| b.budget_ms()).sum();
        assert!((total - 8.0).abs() < 1e-9);
    }
}
