//! Unit `sim-core-munitions`. Missiles, torpedoes, mines, depth charges, drones, aircraft, helicopters, RHIBs
//! and decoy objects as flat arrays of small bodies. DESIGN.md L584-604, L597 (small bodies),
//! L516-525 (what guidance needs from the body), L786-797 (RHIB, mines).
//!
//! sim-core integrates bodies and resolves their fuses and hits. Guidance (proportional navigation, seekers,
//! decoy seduction, aircraft behaviour) lives in sim-systems, which writes a [`Steer`] per body at
//! 30 Hz. Later-phase kinds use the same arrays: aircraft and helicopter (Act 2), depth charge (Act 3); they need
//! data and behaviour, not new storage. AI submarines (Act 3) are cell vessels, not bodies.
//!
//! # Row lifetime
//! A flight ends on a hull hit, a burst (fuse or [`Steer::Detonate`]), the sea or the land, the end of the
//! type's range or endurance, recovery by the launcher, the loss of every hit point, or [`Steer::Ditch`] from
//! guidance that has nothing left to steer by.
//! A body that ends in a tick (hull hit, burst, expiry, recovery, or killed by another body's air burst) is
//! marked [`BodyPhase::Dead`] with 0 hit points and stays in the arrays until the start of the next
//! [`Bodies::step`], so the world can read its kind, launcher and position while resolving outcomes. Dead rows
//! never move and never produce a second outcome. The world despawns their entities; calling
//! [`Bodies::remove`] early is allowed and idempotent. Decoys follow the same pattern: after a step,
//! [`Decoys::expired`] lists decoys whose life is over (the world queues `DecoyExpired` and despawns them), and
//! the next step drops those rows.
//!
//! # Kinematics
//! Powered bodies keep their cruise speed (after a fixed boost ramp) and turn with lateral acceleration capped
//! at the type's `max_accel`. Anti-ship missiles, drones, aircraft and helicopters hold `cruise_alt_m` above the
//! local wave height (the higher of the sea under them and a quarter second ahead; from 50 m up they hold above
//! mean sea level), and under a [`Steer::Accel`] demand (terminal homing) they follow its full 3D direction
//! instead. These bodies keep at least 1 m above the sea where each step lands. Torpedoes hold their running
//! depth below the local surface and steer only in the horizontal plane; RHIBs ride the surface. Surface-to-air and
//! point-defence missiles fly a free 3D path under their acceleration demand. Mines and depth charges fall
//! ballistically until they enter the water; mines then sit moored at [`MINE_DEPTH_M`], depth charges sink at
//! their fuse's rate.
//!
//! # Hull geometry
//! A [`HullTarget`] carries the hull's world bounds and pose. Because the vessel frame has its origin at the
//! grid's minimum corner, the local box `[0, d]` is recovered from the bounds' centre: `d = 2 R^T (centre -
//! pos)`; it is accepted when it reproduces the bounds within 1 m, otherwise the world bounds are used as is.
//! Contact fuses test the swept segment against that box; the [`BodyOutcome::HullHit`] entry is where the
//! segment enters it, in the vessel's cell frame, and `armour::resolve_warhead` then steps to the first solid
//! cell. A ray that crosses the box but no solid cell is a fly-through with no damage.
use crate::entity::EntityId;
use crate::math::{Aabb, Pose};
use crate::projectiles::{BodyGraze, HullTarget};
use content::cell::CELL_M;
use content::ids::{MunitionIdx, PartSlot, SideId};
use content::part::weapons::{Fuse, MunitionDef, MunitionKind};
use content::part::DecoyKind;
use glam::DVec3;
use serde::{Deserialize, Serialize};

#[cfg(test)]
mod tests;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum BodyPhase { #[default] Boost, Cruise, Terminal, /// Mine laid, or depth charge sinking.
    Passive, /// Drone, aircraft, helicopter or RHIB holding or returning.
    Loiter, Returning, Dead }

/// Steering demand written by sim-systems guidance.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub enum Steer {
    /// Keep the current velocity (ballistic or straight-running).
    #[default] Hold,
    /// Lateral acceleration demand in world m/s^2 (proportional navigation output); clamped to the type's limit.
    Accel(DVec3),
    /// Fly or run toward a world point at cruise speed and cruise altitude or depth.
    Waypoint(DVec3),
    /// Orbit a point at a radius (drones, aircraft, helicopters).
    Orbit { centre: DVec3, radius_m: f64 },
    /// Detonate now (command or seeker-decided proximity burst).
    Detonate,
    /// End the flight now with no burst: a weapon with no guidance left drops into the sea. The body ends as
    /// [`BodyOutcome::Expired`], the outcome of running out its range.
    Ditch,
}

/// Structure-of-arrays storage; rows ascending by entity slot.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Bodies {
    pub entity: Vec<EntityId>,
    pub kind: Vec<MunitionIdx>,
    pub side: Vec<SideId>,
    pub pos: Vec<DVec3>,
    pub vel: Vec<DVec3>,
    pub hp: Vec<u8>,
    pub phase: Vec<BodyPhase>,
    pub steer: Vec<Steer>,
    pub launcher: Vec<EntityId>,
    pub launcher_slot: Vec<PartSlot>,
    pub spawn_tick: Vec<u64>,
    /// Metres of range or seconds of endurance left, by kind.
    pub remaining: Vec<f64>,
    /// Set depth for depth charges in metres; unused otherwise.
    pub set_depth_m: Vec<f64>,
    /// Seeker radiating (active radar seeker on, active sonar pinging); written by sim-systems, read by ESM.
    pub radiating: Vec<bool>,
    /// The body has left its launcher's box (or the mine its layer's influence radius); until then the
    /// launcher cannot fuse it.
    #[serde(default)]
    pub(crate) cleared: Vec<bool>,
}

/// Chaff, flares, smoke and towed decoys: sim objects identical on every quality tier (L599-604).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Decoys {
    pub entity: Vec<EntityId>,
    pub kind: Vec<DecoyKind>,
    pub owner: Vec<EntityId>,
    pub pos: Vec<DVec3>,
    pub age_s: Vec<f64>,
    /// Towed decoy whose owner is gone.
    #[serde(default)]
    pub(crate) lost: Vec<bool>,
}
impl Decoys {
    /// Radar cross-section now in m^2: chaff blooms over 3 s to 1,000 m^2 then falls linearly to 0 over 60 s;
    /// towed decoy = 2x owner RCS supplied by the caller (L524, L601-603). Unit: `sim-core-munitions`.
    ///
    /// Flares and smoke have no radar cross-section (smoke does not block radar, L604). Out-of-range rows
    /// return 0.
    pub fn rcs_m2(&self, row: usize, owner_rcs: f64, tuning: &content::tuning::DecoyTuning) -> f64 {
        let (Some(kind), Some(age)) = (self.kind.get(row), self.age_s.get(row)) else { return 0.0 };
        let age = clean_age(*age);
        match kind {
            DecoyKind::Chaff => {
                let peak = tuning.chaff_rcs_m2.max(0.0);
                let bloom = tuning.chaff_bloom_s.max(0.0);
                let life = tuning.chaff_life_s.max(0.0);
                if age < bloom { peak * age / bloom }
                else if age < bloom + life { peak * (1.0 - (age - bloom) / life) }
                else { 0.0 }
            }
            DecoyKind::TowedDecoy => {
                if self.lost.get(row).copied().unwrap_or(false) { return 0.0; }
                let r = owner_rcs * tuning.towed_rcs_factor;
                if r.is_finite() { r.max(0.0) } else { 0.0 }
            }
            DecoyKind::Flare | DecoyKind::Smoke => 0.0,
        }
    }
    /// Infrared power in MW (flares 20 MW for 5 s). Unit: `sim-core-munitions`.
    pub fn ir_mw(&self, row: usize, tuning: &content::tuning::DecoyTuning) -> f64 {
        match (self.kind.get(row), self.age_s.get(row)) {
            (Some(DecoyKind::Flare), Some(age)) if clean_age(*age) < tuning.flare_life_s => tuning.flare_mw.max(0.0),
            _ => 0.0,
        }
    }
    /// Smoke cylinder (centre, radius, height) now, or `None`: radius 60 m and height 30 m at full size after
    /// 10 s, lasting 120 s (L604). Unit: `sim-core-munitions`.
    ///
    /// The cylinder stands on mean sea level (y = 0) under the drifting puff; `centre` is its geometric centre
    /// (base plus half the height). Radius and height grow linearly from 0 over the growth time.
    pub fn smoke_cylinder(&self, row: usize, tuning: &content::tuning::DecoyTuning) -> Option<(DVec3, f64, f64)> {
        let (Some(DecoyKind::Smoke), Some(age), Some(pos)) = (self.kind.get(row), self.age_s.get(row), self.pos.get(row)) else { return None };
        let age = clean_age(*age);
        if age >= tuning.smoke_life_s { return None; }
        let grow = if tuning.smoke_grow_s > 0.0 { (age / tuning.smoke_grow_s).min(1.0) } else { 1.0 };
        let r = tuning.smoke_radius_m.max(0.0) * grow;
        let h = tuning.smoke_height_m.max(0.0) * grow;
        if !(pos.x.is_finite() && pos.z.is_finite()) || r <= 0.0 || h <= 0.0 { return None; }
        Some((DVec3::new(pos.x, 0.5 * h, pos.z), r, h))
    }

    pub fn len(&self) -> usize { self.entity.len() }
    pub fn is_empty(&self) -> bool { self.entity.is_empty() }
    pub fn row_of(&self, id: EntityId) -> Option<usize> { self.entity.binary_search(&id).ok() }
    /// Insert a freshly deployed decoy (age 0) keeping ascending entity order; an existing row is replaced.
    pub fn insert(&mut self, entity: EntityId, kind: DecoyKind, owner: EntityId, pos: DVec3) {
        self.repair();
        match self.entity.binary_search(&entity) {
            Ok(i) => { self.kind[i] = kind; self.owner[i] = owner; self.pos[i] = pos; self.age_s[i] = 0.0; self.lost[i] = false; }
            Err(i) => {
                self.entity.insert(i, entity);
                self.kind.insert(i, kind);
                self.owner.insert(i, owner);
                self.pos.insert(i, pos);
                self.age_s.insert(i, 0.0);
                self.lost.insert(i, false);
            }
        }
    }
    /// Remove a decoy; unknown ids are ignored.
    pub fn remove(&mut self, id: EntityId) {
        self.repair();
        if let Ok(i) = self.entity.binary_search(&id) {
            self.entity.remove(i);
            self.kind.remove(i);
            self.owner.remove(i);
            self.pos.remove(i);
            self.age_s.remove(i);
            self.lost.remove(i);
        }
    }
    /// True when the decoy's life is over: chaff after bloom plus life, flares after 5 s, smoke after 120 s, a
    /// towed decoy once its owner is gone. Out-of-range rows count as expired.
    pub fn is_expired(&self, row: usize, tuning: &content::tuning::DecoyTuning) -> bool {
        let (Some(kind), Some(age)) = (self.kind.get(row), self.age_s.get(row)) else { return true };
        if !age.is_finite() { return true; }
        let age = clean_age(*age);
        match kind {
            DecoyKind::Chaff => age >= tuning.chaff_bloom_s.max(0.0) + tuning.chaff_life_s.max(0.0),
            DecoyKind::Flare => age >= tuning.flare_life_s,
            DecoyKind::Smoke => age >= tuning.smoke_life_s,
            DecoyKind::TowedDecoy => self.lost.get(row).copied().unwrap_or(false),
        }
    }
    /// Expired decoys in ascending entity order (see the module docs on row lifetime).
    pub fn expired(&self, tuning: &content::tuning::DecoyTuning) -> Vec<EntityId> {
        (0..self.len().min(self.kind.len()).min(self.age_s.len())).filter(|&i| self.is_expired(i, tuning)).map(|i| self.entity[i]).collect()
    }

    /// Make every column the same length (rows past the shortest public column are dropped; the private
    /// column is padded), so data pushed directly or deserialised cannot cause out-of-bounds access.
    fn repair(&mut self) {
        let n = self.entity.len().min(self.kind.len()).min(self.owner.len()).min(self.pos.len()).min(self.age_s.len());
        self.entity.truncate(n);
        self.kind.truncate(n);
        self.owner.truncate(n);
        self.pos.truncate(n);
        self.age_s.truncate(n);
        self.lost.resize(n, false);
    }

    /// Age, drift and tow every decoy one tick. Called from [`Bodies::step`].
    fn step(&mut self, ctx: &crate::world::StepContext<'_>, dt: f64, hulls: &[HullBox]) {
        let tuning = &ctx.tuning.decoys;
        // Drop the rows that expired last tick.
        let mut i = 0;
        while i < self.len() {
            if self.is_expired(i, tuning) { let id = self.entity[i]; self.remove(id); } else { i += 1; }
        }
        let wind = wind_vector(ctx.weather);
        for i in 0..self.len() {
            self.age_s[i] = clean_age(self.age_s[i]) + dt;
            match self.kind[i] {
                DecoyKind::Chaff | DecoyKind::Smoke => self.pos[i] += wind * dt,
                DecoyKind::Flare => {}
                DecoyKind::TowedDecoy => match find_hull(hulls, self.owner[i]) {
                    Some(h) => {
                        let stern = h.stern_point();
                        let fwd = h.forward_flat();
                        let p = stern - fwd * tuning.towed_distance_m.max(0.0);
                        let y = ctx.waves.height_at(p.x, p.z, ctx.time_s + dt);
                        self.pos[i] = DVec3::new(p.x, y, p.z);
                    }
                    None => self.lost[i] = true,
                },
            }
        }
    }
}

/// What ended or struck this tick.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum BodyOutcome {
    /// Struck a hull: burst at this cell with the munition's fuse, B and r (L586-595).
    HullHit { body: EntityId, vessel: EntityId, entry: [f64; 3], dir: [f64; 3], world_point: DVec3 },
    /// Proximity burst that removes all hit points of bodies within the lethal radius (L597).
    AirBurst { body: EntityId, at: DVec3, killed: Vec<EntityId> },
    /// Underwater burst near a hull (mine influence, depth charge at set depth): HE at the nearest hull cell.
    UnderwaterBurst { body: EntityId, at: DVec3, vessel: Option<EntityId> },
    /// Ran out of range, fuel or hit points, hit the sea or land.
    Expired { body: EntityId, at: DVec3, shot_down: bool },
    /// Offboard body returned to its bay.
    Recovered { body: EntityId, vessel: EntityId },
}
impl BodyOutcome {
    /// The body this outcome belongs to.
    pub fn body(&self) -> EntityId {
        match self {
            Self::HullHit { body, .. } | Self::AirBurst { body, .. } | Self::UnderwaterBurst { body, .. } | Self::Expired { body, .. } | Self::Recovered { body, .. } => *body,
        }
    }
}

/// Moored depth of a laid mine below mean sea level, metres.
pub const MINE_DEPTH_M: f64 = 2.0;
/// Seconds after laying before a mine's influence fuse arms.
pub const MINE_ARM_S: f64 = 30.0;
/// Reach of a depth charge or commanded underwater burst: the hull must be within this distance for the burst
/// to strike it (the mine influence radius of L594).
pub const UNDERWATER_REACH_M: f64 = 10.0;
/// Horizontal distance from the launcher's hull at which a returning drone, helicopter or RHIB is recovered.
pub const RECOVER_M: f64 = 30.0;
/// A waypoint this close to the launcher's hull recalls a reusable body.
pub const RECALL_M: f64 = 60.0;
/// Boost ramp to cruise speed: missiles, drones and aircraft; torpedoes.
const BOOST_S: f64 = 2.5;
const TORPEDO_BOOST_S: f64 = 1.0;
/// Default depth-charge set depth and its allowed range, metres (L595).
const DEPTH_CHARGE_DEFAULT_M: f64 = 30.0;
const DEPTH_CHARGE_MIN_M: f64 = 10.0;
const DEPTH_CHARGE_MAX_M: f64 = 150.0;
const DEPTH_CHARGE_SINK_MPS: f64 = 3.0;
/// Gravity for unpowered bodies, m/s^2.
const G: f64 = 9.81;
/// Time constant of the altitude and depth hold, seconds.
const HOLD_TAU_S: f64 = 1.0;
/// Pursuit time constant for waypoint and orbit steering, seconds.
const PURSUIT_TAU_S: f64 = 0.5;
/// Endurance margin for the automatic return: fly home when endurance is below 1.2x the time home plus this.
const BINGO_MARGIN_S: f64 = 15.0;
/// Torpedoes keep at least this depth below the local surface.
const TORPEDO_MIN_DEPTH_M: f64 = 1.0;
/// Air bodies hold at least this height above the local surface.
const AIR_MIN_ALT_M: f64 = 1.0;
/// Look-ahead of the altitude hold over the waves, seconds of flight.
const HOLD_LOOKAHEAD_S: f64 = 0.25;
/// Bodies holding this altitude or more hold it above mean sea level.
const HIGH_ALT_M: f64 = 50.0;

impl Bodies {
    pub fn len(&self) -> usize { self.entity.len() }
    pub fn is_empty(&self) -> bool { self.entity.is_empty() }
    pub fn row_of(&self, id: EntityId) -> Option<usize> { self.entity.binary_search(&id).ok() }
    /// Insert keeping ascending entity order. Unit: `sim-core-munitions`.
    ///
    /// Hit points, range or endurance (seconds when the type has `endurance_s > 0`, else metres of
    /// `range_m`), the start phase and the depth-charge set depth (the type's negative `cruise_alt_m`, else
    /// 30 m, clamped to 10..=150 m) come from the catalogue. An unknown kind is inserted with 0 hit points and
    /// expires on the next step. An existing row with the same entity is replaced.
    pub fn insert(&mut self, entity: EntityId, kind: MunitionIdx, side: SideId, pos: DVec3, vel: DVec3, launcher: EntityId, slot: PartSlot, tick: u64, catalogue: &content::catalogue::Catalogue) {
        self.repair();
        let def = munition_def(catalogue, kind);
        let (hp, remaining, phase, set_depth) = match def {
            Some(d) => {
                let remaining = if d.endurance_s > 0.0 { d.endurance_s } else { d.range_m.max(0.0) };
                let phase = match d.kind {
                    MunitionKind::Mine | MunitionKind::DepthCharge => BodyPhase::Passive,
                    MunitionKind::Rhib | MunitionKind::Aircraft | MunitionKind::Helicopter => BodyPhase::Cruise,
                    _ => BodyPhase::Boost,
                };
                let set_depth = if d.kind == MunitionKind::DepthCharge {
                    let want = if d.cruise_alt_m < 0.0 { -d.cruise_alt_m } else { DEPTH_CHARGE_DEFAULT_M };
                    if want.is_finite() { want.clamp(DEPTH_CHARGE_MIN_M, DEPTH_CHARGE_MAX_M) } else { DEPTH_CHARGE_DEFAULT_M }
                } else { 0.0 };
                (d.hp.max(1), if remaining.is_finite() { remaining } else { 0.0 }, phase, set_depth)
            }
            None => (0, 0.0, BodyPhase::Boost, 0.0),
        };
        let i = match self.entity.binary_search(&entity) {
            Ok(i) => i,
            Err(i) => {
                self.entity.insert(i, entity);
                self.kind.insert(i, kind);
                self.side.insert(i, side);
                self.pos.insert(i, pos);
                self.vel.insert(i, vel);
                self.hp.insert(i, hp);
                self.phase.insert(i, phase);
                self.steer.insert(i, Steer::Hold);
                self.launcher.insert(i, launcher);
                self.launcher_slot.insert(i, slot);
                self.spawn_tick.insert(i, tick);
                self.remaining.insert(i, remaining);
                self.set_depth_m.insert(i, set_depth);
                self.radiating.insert(i, false);
                self.cleared.insert(i, launcher.is_none());
                return;
            }
        };
        self.kind[i] = kind;
        self.side[i] = side;
        self.pos[i] = pos;
        self.vel[i] = vel;
        self.hp[i] = hp;
        self.phase[i] = phase;
        self.steer[i] = Steer::Hold;
        self.launcher[i] = launcher;
        self.launcher_slot[i] = slot;
        self.spawn_tick[i] = tick;
        self.remaining[i] = remaining;
        self.set_depth_m[i] = set_depth;
        self.radiating[i] = false;
        self.cleared[i] = launcher.is_none();
    }
    /// Remove a body; unknown ids are ignored (idempotent).
    pub fn remove(&mut self, id: EntityId) {
        self.repair();
        if let Ok(i) = self.entity.binary_search(&id) { self.remove_row(i); }
    }
    /// Integrate all bodies one tick under their [`Steer`], hold cruise altitude or depth over the wave field,
    /// run contact, proximity, influence and depth fuses against hulls and other bodies, apply shell grazes
    /// (-1 hit point each; point-defence rounds also count within the proximity distance), and report outcomes
    /// in ascending entity order. Wind drifts chaff and smoke. Unit: `sim-core-munitions`.
    ///
    /// Order: drop rows that ended last tick (see the module docs), apply `grazes`, integrate every live body
    /// from the same start state, then resolve fuses in ascending entity order against the integrated state,
    /// then age the decoys. Each body produces at most one outcome per tick; bodies killed by another body's
    /// air burst appear only in that burst's `killed` list. The point-defence proximity distance is applied by
    /// the projectile step that produced the grazes. A body ignores its own launcher's hull until it has left
    /// it, and a mine its layer until it is outside its influence radius; mines arm [`MINE_ARM_S`] after laying.
    /// Proximity fuses fire on, and air bursts kill, only air bodies (missiles, drones, aircraft, helicopters) of
    /// another side.
    pub fn step(&mut self, decoys: &mut Decoys, ctx: &crate::world::StepContext<'_>, hulls: &[crate::projectiles::HullTarget], grazes: &[crate::projectiles::BodyGraze], out: &mut Vec<BodyOutcome>) {
        let dt = if ctx.dt.is_finite() && ctx.dt > 0.0 { ctx.dt } else { crate::time::DT };
        self.repair();
        let mut i = 0;
        while i < self.len() {
            if self.phase[i] == BodyPhase::Dead { self.remove_row(i); } else { i += 1; }
        }
        let boxes: Vec<HullBox> = hulls.iter().map(HullBox::new).collect();
        let first_out = out.len();
        let t_end = ctx.time_s + dt;
        let sim = Sim { ctx, dt, t_end, boxes: &boxes, wave_bound: wave_bound(ctx.waves) };

        // Shell grazes from last tick: one hit point each.
        let mut grazed = vec![false; self.len()];
        for g in grazes {
            if let Some(r) = self.row_of(g.body) {
                if self.phase[r] != BodyPhase::Dead && self.hp[r] > 0 {
                    self.hp[r] -= 1;
                    grazed[r] = true;
                }
            }
        }
        // Bodies with no hit points left, an unknown kind or a corrupt state end before moving.
        for r in 0..self.len() {
            if self.phase[r] == BodyPhase::Dead { continue; }
            let valid = munition_def(ctx.catalogue, self.kind[r]).is_some() && self.pos[r].is_finite() && self.vel[r].is_finite();
            if self.hp[r] == 0 || !valid {
                out.push(BodyOutcome::Expired { body: self.entity[r], at: finite_or_zero(self.pos[r]), shot_down: grazed[r] && self.hp[r] == 0 });
                self.kill(r);
            }
        }

        // Integrate every live body from the same start state.
        let prev: Vec<DVec3> = self.pos.clone();
        for r in 0..self.len() {
            if self.phase[r] == BodyPhase::Dead { continue; }
            let Some(def) = munition_def(ctx.catalogue, self.kind[r]) else { continue };
            sim.integrate(self, r, def);
        }

        // Fuses and endings in ascending entity order.
        for r in 0..self.len() {
            if self.phase[r] == BodyPhase::Dead { continue; }
            let Some(def) = munition_def(ctx.catalogue, self.kind[r]) else { continue };
            if let Some(o) = sim.resolve(self, r, def, &prev) {
                out.push(o);
                self.kill(r);
            }
        }
        // Launcher clearance for next tick.
        for r in 0..self.len() {
            if self.phase[r] == BodyPhase::Dead || self.cleared[r] { continue; }
            let Some(def) = munition_def(ctx.catalogue, self.kind[r]) else { continue };
            self.cleared[r] = match find_hull(&boxes, self.launcher[r]) {
                None => true,
                Some(h) => {
                    let margin = match def.fuse { Fuse::Influence { radius_m } => radius_m.max(0.0) + 1.0, _ => def.radius_m.max(0.0) + 1.0 };
                    h.distance(self.pos[r]) > margin
                }
            };
        }
        out[first_out..].sort_by_key(|o| o.body());
        decoys.repair();
        decoys.step(ctx, dt, &boxes);
    }

    fn kill(&mut self, r: usize) {
        self.phase[r] = BodyPhase::Dead;
        self.hp[r] = 0;
        self.steer[r] = Steer::Hold;
        self.radiating[r] = false;
    }

    fn remove_row(&mut self, i: usize) {
        self.entity.remove(i);
        self.kind.remove(i);
        self.side.remove(i);
        self.pos.remove(i);
        self.vel.remove(i);
        self.hp.remove(i);
        self.phase.remove(i);
        self.steer.remove(i);
        self.launcher.remove(i);
        self.launcher_slot.remove(i);
        self.spawn_tick.remove(i);
        self.remaining.remove(i);
        self.set_depth_m.remove(i);
        self.radiating.remove(i);
        self.cleared.remove(i);
    }

    /// Make every column the same length (see [`Decoys::repair`]).
    fn repair(&mut self) {
        let n = [self.entity.len(), self.kind.len(), self.side.len(), self.pos.len(), self.vel.len(), self.hp.len(), self.phase.len(), self.steer.len(), self.launcher.len(), self.launcher_slot.len(), self.spawn_tick.len(), self.remaining.len(), self.set_depth_m.len(), self.radiating.len()]
            .into_iter().min().unwrap_or(0);
        self.entity.truncate(n);
        self.kind.truncate(n);
        self.side.truncate(n);
        self.pos.truncate(n);
        self.vel.truncate(n);
        self.hp.truncate(n);
        self.phase.truncate(n);
        self.steer.truncate(n);
        self.launcher.truncate(n);
        self.launcher_slot.truncate(n);
        self.spawn_tick.truncate(n);
        self.remaining.truncate(n);
        self.set_depth_m.truncate(n);
        self.radiating.truncate(n);
        // Rows pushed without a clearance flag: treat as cleared only when there is no launcher.
        while self.cleared.len() < n { let i = self.cleared.len(); self.cleared.push(self.launcher[i].is_none()); }
        self.cleared.truncate(n);
    }
}

// ---- internals ----

fn munition_def(catalogue: &content::catalogue::Catalogue, kind: MunitionIdx) -> Option<&MunitionDef> {
    catalogue.munitions.get_index(kind.index()).map(|(_, d)| d)
}

fn clean_age(a: f64) -> f64 { if a.is_finite() && a > 0.0 { a } else { 0.0 } }

fn finite_or_zero(v: DVec3) -> DVec3 { if v.is_finite() { v } else { DVec3::ZERO } }

/// Wind velocity in world m/s from the weather (compass direction the wind blows toward).
fn wind_vector(w: &content::weather::WeatherState) -> DVec3 {
    let (h, s) = (w.wind_dir_rad, w.wind_speed_mps);
    if !(h.is_finite() && s.is_finite()) { return DVec3::ZERO; }
    DVec3::new(libm::sin(h), 0.0, -libm::cos(h)) * s.max(0.0)
}

/// Upper bound on the wave surface height (sum of amplitudes), so high flyers skip the surface solve.
fn wave_bound(w: &crate::waves::WaveSet) -> f64 {
    w.waves[..(w.count as usize).min(crate::waves::MAX_WAVES)].iter().map(|c| c.amplitude.abs()).sum()
}

/// How a kind moves.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Motion {
    /// Holds altitude above the wave surface.
    AirHold,
    /// Free 3D flight (SAM, PD missile).
    Air3d,
    /// Holds depth below the wave surface.
    Underwater,
    /// Rides the surface.
    Surface,
    /// Falls then moors (mine).
    Mine,
    /// Falls then sinks (depth charge).
    Sinker,
}
fn motion(kind: MunitionKind) -> Motion {
    match kind {
        MunitionKind::AntiShipMissile | MunitionKind::Drone | MunitionKind::Aircraft | MunitionKind::Helicopter => Motion::AirHold,
        MunitionKind::Sam | MunitionKind::PdMissile => Motion::Air3d,
        MunitionKind::Torpedo => Motion::Underwater,
        MunitionKind::Rhib => Motion::Surface,
        MunitionKind::Mine => Motion::Mine,
        MunitionKind::DepthCharge => Motion::Sinker,
    }
}
/// Bodies a proximity fuse can fire on.
fn is_air_target(kind: MunitionKind) -> bool { matches!(motion(kind), Motion::AirHold | Motion::Air3d) }
/// Drones, helicopters, aircraft and RHIBs come back when they can.
fn is_reusable(def: &MunitionDef) -> bool {
    def.endurance_s > 0.0 && matches!(def.kind, MunitionKind::Drone | MunitionKind::Helicopter | MunitionKind::Aircraft | MunitionKind::Rhib)
}
/// Bodies that can stop and hover at a point.
fn can_hover(kind: MunitionKind) -> bool { matches!(kind, MunitionKind::Helicopter | MunitionKind::Rhib) }

/// A hull's box in its own frame (`[0, size]`, vessel-local metres) or, when that cannot be recovered, its
/// world bounds.
#[derive(Clone, Copy)]
struct HullBox { vessel: EntityId, pose: Pose, size: DVec3, world: Aabb, oriented: bool }
impl HullBox {
    fn new(h: &HullTarget) -> Self {
        let world = h.bounds;
        let pose = if h.pose.pos.is_finite() && h.pose.rot.is_finite() && (h.pose.rot.length() - 1.0).abs() < 1e-3 { Pose::new(h.pose.pos, h.pose.rot.normalize()) } else { Pose::default() };
        let centre = (world.min + world.max) * 0.5;
        let half = (world.max - world.min) * 0.5;
        let size = pose.to_local(centre) * 2.0;
        let mut oriented = size.is_finite() && size.min_element() > 0.0 && half.min_element() >= 0.0;
        if oriented {
            // The recovered box must reproduce the bounds: |R| (size / 2) == half extents.
            let m = glam::DMat3::from_quat(pose.rot);
            let abs = glam::DMat3::from_cols(m.x_axis.abs(), m.y_axis.abs(), m.z_axis.abs());
            let e = abs * (size * 0.5);
            oriented = (e - half).abs().max_element() <= 1.0;
        }
        Self { vessel: h.vessel, pose, size, world, oriented }
    }
    /// Distance from a world point to the box (0 inside).
    fn distance(&self, p: DVec3) -> f64 {
        if self.oriented {
            let l = self.pose.to_local(p);
            (l - l.clamp(DVec3::ZERO, self.size)).length()
        } else {
            (p - p.clamp(self.world.min, self.world.max)).length()
        }
    }
    /// Horizontal distance from a world point to the box's footprint.
    fn distance_flat(&self, p: DVec3) -> f64 {
        let c = self.closest(p);
        let d = p - c;
        (d.x * d.x + d.z * d.z).sqrt()
    }
    fn closest(&self, p: DVec3) -> DVec3 {
        if self.oriented { self.pose.to_world(self.pose.to_local(p).clamp(DVec3::ZERO, self.size)) } else { p.clamp(self.world.min, self.world.max) }
    }
    fn centre(&self) -> DVec3 { if self.oriented { self.pose.to_world(self.size * 0.5) } else { (self.world.min + self.world.max) * 0.5 } }
    /// Bow direction flattened onto the sea plane.
    fn forward_flat(&self) -> DVec3 {
        let f = self.pose.rot * DVec3::X;
        let flat = DVec3::new(f.x, 0.0, f.z);
        if flat.length_squared() > 1e-12 { flat.normalize() } else { DVec3::X }
    }
    /// Stern centre at the keel-to-deck midline.
    fn stern_point(&self) -> DVec3 {
        if self.oriented { self.pose.to_world(DVec3::new(0.0, 0.5 * self.size.y, 0.5 * self.size.z)) } else { self.centre() - self.forward_flat() * (0.5 * (self.world.max - self.world.min).length()) }
    }
    /// First entry of the segment a -> b into the box: (parameter in 0..=1, world point, local point).
    fn segment_entry(&self, a: DVec3, b: DVec3) -> Option<(f64, DVec3, DVec3)> {
        let (la, lb, lo, hi) = if self.oriented { (self.pose.to_local(a), self.pose.to_local(b), DVec3::ZERO, self.size) } else { (a, b, self.world.min, self.world.max) };
        let d = lb - la;
        let (mut t0, mut t1) = (0.0f64, 1.0f64);
        for k in 0..3 {
            let (o, dk, l, h) = (la[k], d[k], lo[k], hi[k]);
            if dk.abs() < 1e-12 {
                if o < l || o > h { return None; }
            } else {
                let (mut ta, mut tb) = ((l - o) / dk, (h - o) / dk);
                if ta > tb { core::mem::swap(&mut ta, &mut tb); }
                t0 = t0.max(ta);
                t1 = t1.min(tb);
                if t0 > t1 { return None; }
            }
        }
        let world = a + (b - a) * t0;
        let local = if self.oriented { la + d * t0 } else { self.pose.to_local(world) };
        Some((t0, world, local))
    }
}

/// Hulls arrive in ascending vessel slot order, so a binary search finds the launcher; fall back to a scan if
/// the caller's order differs.
fn find_hull(hulls: &[HullBox], id: EntityId) -> Option<&HullBox> {
    if id.is_none() { return None; }
    match hulls.binary_search_by(|h| h.vessel.cmp(&id)) {
        Ok(i) => Some(&hulls[i]),
        Err(_) => hulls.iter().find(|h| h.vessel == id),
    }
}

struct Sim<'a, 'c> { ctx: &'a crate::world::StepContext<'c>, dt: f64, t_end: f64, boxes: &'a [HullBox], wave_bound: f64 }

impl Sim<'_, '_> {
    fn surface(&self, p: DVec3) -> f64 { self.ctx.waves.height_at(p.x, p.z, self.t_end) }

    /// Surface an altitude-holding body flies over: the higher of the sea under it and the sea
    /// [`HOLD_LOOKAHEAD_S`] ahead, so it climbs before a crest rather than after. Bodies holding
    /// [`HIGH_ALT_M`] or more hold above mean sea level, as a barometric hold would.
    fn hold_surface(&self, pos: DVec3, vel: DVec3, clear_alt: f64) -> f64 {
        if clear_alt >= HIGH_ALT_M || pos.y > self.wave_bound + clear_alt + HIGH_ALT_M { return 0.0; }
        let ahead = pos + DVec3::new(vel.x, 0.0, vel.z) * HOLD_LOOKAHEAD_S;
        self.surface(pos).max(self.surface(ahead))
    }

    /// Keep an air body at least [`AIR_MIN_ALT_M`] above the sea where this tick's step lands: the vertical
    /// speed is raised just enough (at most half the speed), and the horizontal speed shrinks to keep the speed.
    fn air_floor(&self, pos: DVec3, v: DVec3, speed: f64) -> DVec3 {
        let next = pos + v * self.dt;
        if next.y >= self.wave_bound + AIR_MIN_ALT_M { return v; }
        let floor = self.surface(next) + AIR_MIN_ALT_M;
        if next.y >= floor { return v; }
        let vy = ((floor - pos.y) / self.dt).min(0.5 * speed.max(1.0)).max(v.y);
        with_vertical(v, vy, speed)
    }

    /// Move one body under its steer.
    fn integrate(&self, b: &mut Bodies, r: usize, def: &MunitionDef) {
        let dt = self.dt;
        let mut pos = b.pos[r];
        let mut vel = b.vel[r];
        let steer = sanitise(b.steer[r]);
        let age = b.spawn_tick.get(r).map(|s| self.ctx.tick.0.saturating_sub(*s) as f64 * dt).unwrap_or(0.0);
        let m = motion(def.kind);
        let cruise = if def.speed_mps.is_finite() { def.speed_mps.max(0.0) } else { 0.0 };
        let max_accel = if def.max_accel.is_finite() { def.max_accel.max(0.0) } else { 0.0 };
        let before = pos;

        match m {
            Motion::Mine if pos.y <= -MINE_DEPTH_M + 1e-9 => {
                // Moored.
                vel = DVec3::ZERO;
                b.phase[r] = BodyPhase::Passive;
            }
            Motion::Mine | Motion::Sinker => {
                let water = if pos.y > -self.wave_bound - 1.0 { self.surface(pos) } else { f64::INFINITY };
                if pos.y > water {
                    vel.y -= G * dt;
                    pos += vel * dt;
                } else if m == Motion::Mine {
                    pos.y = -MINE_DEPTH_M;
                    vel = DVec3::ZERO;
                } else {
                    let sink = match def.fuse { Fuse::Depth { sink_mps } if sink_mps.is_finite() && sink_mps > 0.0 => sink_mps, _ => DEPTH_CHARGE_SINK_MPS };
                    let damp = (1.0 - 2.0 * dt).max(0.0);
                    vel = DVec3::new(vel.x * damp, -sink, vel.z * damp);
                    pos += vel * dt;
                }
                b.phase[r] = BodyPhase::Passive;
            }
            _ => {
                // Phase from steer: Boost ends on speed or time; Returning is sticky.
                let boost_s = if m == Motion::Underwater { TORPEDO_BOOST_S } else { BOOST_S };
                let speed = vel.length();
                let mut phase = b.phase[r];
                if phase == BodyPhase::Boost && (speed >= cruise * 0.999 || age >= boost_s * 2.0) { phase = BodyPhase::Cruise; }
                let launcher = find_hull(self.boxes, b.launcher[r]).copied();
                let mut steer = steer;
                if is_reusable(def) && phase != BodyPhase::Boost {
                    if let (Some(h), Steer::Waypoint(p)) = (launcher, steer) {
                        if h.distance_flat(p) <= RECALL_M { phase = BodyPhase::Returning; }
                    }
                    if phase != BodyPhase::Returning {
                        if let Some(h) = launcher {
                            let home = h.distance_flat(pos) / cruise.max(1.0);
                            if b.remaining[r] <= home * 1.2 + BINGO_MARGIN_S { phase = BodyPhase::Returning; }
                        }
                    }
                }
                if phase == BodyPhase::Returning {
                    steer = match launcher { Some(h) => Steer::Waypoint(h.centre()), None => Steer::Hold };
                } else if phase != BodyPhase::Boost {
                    phase = match steer {
                        Steer::Accel(_) => BodyPhase::Terminal,
                        Steer::Orbit { .. } => BodyPhase::Loiter,
                        Steer::Hold | Steer::Waypoint(_) | Steer::Detonate | Steer::Ditch => if phase == BodyPhase::Terminal || phase == BodyPhase::Loiter { BodyPhase::Cruise } else { phase },
                    };
                }

                // Terminal homing of an altitude-holding body follows the full 3D demand (guidance aims at the
                // target's height), bounded below by the wave floor.
                let terminal_3d = m == Motion::AirHold && matches!(steer, Steer::Accel(_));
                let alt = if def.cruise_alt_m.is_finite() { def.cruise_alt_m } else { 0.0 };
                // Reference surface for the holds (free 3D flight and terminal homing need none).
                let surf = match m {
                    Motion::Air3d => 0.0,
                    Motion::AirHold if terminal_3d => 0.0,
                    Motion::AirHold => self.hold_surface(pos, vel, alt.max(AIR_MIN_ALT_M)),
                    _ => self.surface(pos),
                };
                // Underwater bodies launched above the sea fall until they enter it.
                if m == Motion::Underwater && pos.y > surf {
                    vel.y -= G * dt;
                    pos += vel * dt;
                    b.pos[r] = pos;
                    b.vel[r] = vel;
                    b.phase[r] = phase;
                    return;
                }

                // Direction of travel; a body at rest picks one.
                let mut dir = if speed > 1e-6 { vel / speed } else { match m { Motion::Air3d | Motion::AirHold => DVec3::Y, _ => DVec3::NEG_Z } };
                if matches!(m, Motion::AirHold | Motion::Underwater | Motion::Surface) && dir.x * dir.x + dir.z * dir.z < 1e-9 {
                    // Vertical launch: pick the waypoint's bearing, else north.
                    let to = match steer { Steer::Waypoint(p) | Steer::Orbit { centre: p, .. } => p - pos, _ => DVec3::NEG_Z };
                    let flat = DVec3::new(to.x, 0.0, to.z);
                    let flat = if flat.length_squared() > 1e-9 { flat.normalize() } else { DVec3::NEG_Z };
                    dir = (dir + flat * 1e-3).normalize();
                }
                // A surface or underwater body at rest (a RHIB holding station, a hovering helicopter) sets off
                // along the bearing of its waypoint or orbit centre.
                let start_from_rest = speed <= 1e-6 && matches!(steer, Steer::Waypoint(_) | Steer::Orbit { .. });
                if start_from_rest && matches!(m, Motion::Underwater | Motion::Surface) {
                    if let Steer::Waypoint(p) | Steer::Orbit { centre: p, .. } = steer {
                        let flat = DVec3::new(p.x - pos.x, 0.0, p.z - pos.z);
                        if flat.length_squared() > 1e-9 { dir = flat.normalize(); }
                    }
                }

                // Speed command.
                let mut speed_cmd = cruise;
                let mut accel_lim = (0.25 * max_accel).max(3.0);
                if phase == BodyPhase::Boost {
                    speed_cmd = cruise;
                    accel_lim = (cruise / boost_s).max(10.0);
                }
                // Lateral demand.
                let flat_only = matches!(m, Motion::AirHold | Motion::Underwater | Motion::Surface) && !terminal_3d;
                let desired_dir = |target: DVec3| -> Option<DVec3> {
                    let mut d = target - pos;
                    if flat_only { d.y = 0.0; }
                    if d.length_squared() > 1e-9 { Some(d.normalize()) } else { None }
                };
                let mut lateral = DVec3::ZERO;
                match steer {
                    Steer::Hold | Steer::Detonate | Steer::Ditch => {}
                    Steer::Accel(a) => lateral = a,
                    Steer::Waypoint(p) => {
                        let mut d = p - pos;
                        if flat_only { d.y = 0.0; }
                        let dist = d.length();
                        let turn_r = if max_accel > 0.0 { cruise * cruise / max_accel } else { f64::INFINITY };
                        if can_hover(def.kind) {
                            speed_cmd = speed_cmd.min(0.3 * dist);
                            if let Some(dd) = desired_dir(p) { lateral = pursuit(dd, dir, speed, vel, max_accel); }
                        } else if !flat_only || dist > 1.5 * turn_r || phase == BodyPhase::Returning || matches!(def.kind, MunitionKind::AntiShipMissile) || m == Motion::Underwater {
                            if let Some(dd) = desired_dir(p) { lateral = pursuit(dd, dir, speed, vel, max_accel); }
                        } else {
                            lateral = orbit_demand(pos, vel, p, 1.5 * turn_r);
                        }
                    }
                    Steer::Orbit { centre, radius_m } => {
                        if def.kind == MunitionKind::Rhib {
                            speed_cmd = speed_cmd.min(0.3 * DVec3::new(centre.x - pos.x, 0.0, centre.z - pos.z).length());
                            if let Some(dd) = desired_dir(centre) { lateral = pursuit(dd, dir, speed, vel, max_accel); }
                        } else {
                            let min_r = if max_accel > 0.0 { 1.05 * cruise * cruise / max_accel } else { 0.0 };
                            let rad = if radius_m.is_finite() { radius_m.max(min_r) } else { min_r };
                            lateral = orbit_demand(pos, vel, centre, rad);
                        }
                    }
                }
                // Keep only the part perpendicular to travel, capped by the type's limit.
                lateral -= dir * lateral.dot(dir);
                if flat_only { lateral.y = 0.0; }
                let lat = lateral.length();
                if lat > max_accel && lat > 0.0 { lateral *= max_accel / lat; }
                if !lateral.is_finite() { lateral = DVec3::ZERO; }

                let new_speed = approach(speed, speed_cmd, accel_lim * dt);
                let mut v = dir * speed + lateral * dt;
                if start_from_rest && v.length_squared() < 1e-18 { v = dir * 1e-6; }
                // Vertical hold.
                match m {
                    Motion::Surface => {
                        let flat = DVec3::new(v.x, 0.0, v.z);
                        let fl = flat.length();
                        v = if fl > 1e-9 { flat / fl * new_speed } else { flat };
                        pos += v * dt;
                        pos.y = self.surface(pos);
                        v.y = 0.0;
                    }
                    Motion::AirHold | Motion::Underwater if !terminal_3d => {
                        let target_y = if m == Motion::AirHold { surf + alt.max(AIR_MIN_ALT_M) } else { surf + alt.min(-TORPEDO_MIN_DEPTH_M) };
                        let max_vy = 0.5 * new_speed.max(1.0);
                        let vy_cmd = ((target_y - pos.y) / HOLD_TAU_S).clamp(-max_vy, max_vy);
                        let vy = approach(v.y, vy_cmd, max_accel.max(G) * dt);
                        v = with_vertical(v, vy, new_speed);
                        if m == Motion::AirHold { v = self.air_floor(pos, v, new_speed); }
                        pos += v * dt;
                    }
                    _ => {
                        let l = v.length();
                        v = if l > 1e-9 { v / l * new_speed } else { dir * new_speed };
                        if m == Motion::AirHold { v = self.air_floor(pos, v, new_speed); }
                        pos += v * dt;
                    }
                }
                vel = v;
                b.phase[r] = phase;
            }
        }
        if def.endurance_s > 0.0 {
            b.remaining[r] -= dt;
        } else if def.range_m > 0.0 && !matches!(m, Motion::Mine | Motion::Sinker) {
            b.remaining[r] -= (pos - before).length();
        }
        // Grounded RHIBs stop at the shore rather than climb it.
        if m == Motion::Surface && self.ctx.terrain.height_at(pos.x, pos.z) > pos.y {
            pos = before;
            vel = DVec3::ZERO;
        }
        b.pos[r] = pos;
        b.vel[r] = vel;
    }

    /// Fuses and endings for one integrated body; `None` while it flies on.
    fn resolve(&self, b: &mut Bodies, r: usize, def: &MunitionDef, prev: &[DVec3]) -> Option<BodyOutcome> {
        let id = b.entity[r];
        let (a, p) = (prev[r], b.pos[r]);
        if !p.is_finite() || !b.vel[r].is_finite() { return Some(BodyOutcome::Expired { body: id, at: finite_or_zero(a), shot_down: false }); }
        let m = motion(def.kind);
        let age = self.ctx.tick.0.saturating_sub(b.spawn_tick[r]) as f64 * self.dt + self.dt;

        // Commanded burst.
        if sanitise(b.steer[r]) == Steer::Detonate {
            return Some(self.burst(b, r, def, prev, 1.0));
        }
        // Commanded end of flight: no burst, no fuse. Mines and depth charges are never guided and ignore it.
        if b.steer[r] == Steer::Ditch && !matches!(m, Motion::Mine | Motion::Sinker) {
            return Some(BodyOutcome::Expired { body: id, at: p, shot_down: false });
        }
        // Contact with a hull along the swept segment.
        let contact = matches!(def.fuse, Fuse::Penetrate { .. } | Fuse::Contact | Fuse::Proximity { .. });
        if contact {
            let mut best: Option<(f64, &HullBox, DVec3, DVec3)> = None;
            for h in self.boxes {
                if h.vessel == b.launcher[r] && !b.cleared[r] { continue; }
                if let Some((t, w, l)) = h.segment_entry(a, p) {
                    if best.as_ref().is_none_or(|(bt, ..)| t < *bt) { best = Some((t, h, w, l)); }
                }
            }
            if let Some((_, h, w, l)) = best {
                let travel = p - a;
                let wdir = if travel.length_squared() > 1e-18 { travel.normalize() } else if b.vel[r].length_squared() > 1e-18 { b.vel[r].normalize() } else { DVec3::NEG_Y };
                let ldir = h.pose.rot.inverse() * wdir;
                let entry = l / CELL_M;
                return Some(BodyOutcome::HullHit { body: id, vessel: h.vessel, entry: entry.to_array(), dir: ldir.to_array(), world_point: w });
            }
        }
        // Proximity fuse against hostile air bodies (closest approach within the tick).
        if let Fuse::Proximity { radius_m } = def.fuse {
            let radius = if radius_m.is_finite() { radius_m.max(0.0) } else { 0.0 };
            let mut best: Option<(f64, f64)> = None;
            for o in 0..b.len() {
                if o == r || b.phase[o] == BodyPhase::Dead || b.side[o] == b.side[r] { continue; }
                let Some(od) = munition_def(self.ctx.catalogue, b.kind[o]) else { continue };
                if !is_air_target(od.kind) { continue; }
                let (d, t) = closest_approach(a, p, prev[o], b.pos[o]);
                if d <= radius + od.radius_m.max(0.0) && best.is_none_or(|(_, bt)| t < bt) { best = Some((d, t)); }
            }
            if let Some((_, t)) = best {
                return Some(self.burst(b, r, def, prev, t));
            }
        }
        // Influence fuse (mines).
        if let Fuse::Influence { radius_m } = def.fuse {
            if age >= MINE_ARM_S && p.y < 0.0 {
                let radius = if radius_m.is_finite() { radius_m.max(0.0) } else { 0.0 };
                if let Some(v) = self.nearest_hull(b, r, p, radius) {
                    return Some(BodyOutcome::UnderwaterBurst { body: id, at: p, vessel: Some(v) });
                }
            }
        }
        // Depth fuse (depth charges): set depth below mean sea level, or the seabed.
        if m == Motion::Sinker {
            let floor = self.ctx.terrain.height_at(p.x, p.z);
            if p.y <= -b.set_depth_m[r] || p.y <= floor {
                let at = DVec3::new(p.x, p.y.max(floor), p.z);
                return Some(BodyOutcome::UnderwaterBurst { body: id, at, vessel: self.nearest_hull(b, r, at, UNDERWATER_REACH_M) });
            }
            return None;
        }
        if m == Motion::Mine { return None; }

        // Recovery of returning bodies.
        if b.phase[r] == BodyPhase::Returning {
            if let Some(h) = find_hull(self.boxes, b.launcher[r]) {
                if h.distance_flat(p) <= RECOVER_M { return Some(BodyOutcome::Recovered { body: id, vessel: h.vessel }); }
            }
        }
        // Sea and land.
        let ground = self.ctx.terrain.height_at(p.x, p.z);
        match m {
            Motion::AirHold | Motion::Air3d => {
                if p.y <= ground || (p.y < self.wave_bound + 0.5 && p.y < self.surface(p)) {
                    return Some(BodyOutcome::Expired { body: id, at: p, shot_down: false });
                }
            }
            Motion::Underwater => {
                if p.y <= ground { return Some(BodyOutcome::Expired { body: id, at: p, shot_down: false }); }
            }
            _ => {}
        }
        // Range or endurance.
        let limited = def.endurance_s > 0.0 || def.range_m > 0.0;
        if limited && b.remaining[r] <= 0.0 {
            return Some(BodyOutcome::Expired { body: id, at: p, shot_down: false });
        }
        None
    }

    /// Burst at `at`: underwater against the nearest hull in reach, in the air against hostile bodies in the
    /// lethal radius (the proximity distance, else the HE radius), plus their sphere radius.
    /// `t` is the fraction of the tick at which it bursts; other bodies are taken at the same instant.
    fn burst(&self, b: &mut Bodies, r: usize, def: &MunitionDef, prev: &[DVec3], t: f64) -> BodyOutcome {
        let id = b.entity[r];
        let at = prev[r] + (b.pos[r] - prev[r]) * t;
        let water = if at.y < self.wave_bound + 0.5 { self.surface(at) } else { f64::NEG_INFINITY };
        if at.y < water {
            let reach = match def.fuse { Fuse::Influence { radius_m } if radius_m.is_finite() => radius_m.max(0.0), _ => UNDERWATER_REACH_M };
            return BodyOutcome::UnderwaterBurst { body: id, at, vessel: self.nearest_hull(b, r, at, reach) };
        }
        let lethal = match def.fuse { Fuse::Proximity { radius_m } if radius_m.is_finite() => radius_m.max(0.0), _ => def.he_r_cells as f64 * CELL_M };
        let mut killed = Vec::new();
        for o in 0..b.len() {
            if o == r || b.phase[o] == BodyPhase::Dead || b.side[o] == b.side[r] { continue; }
            let Some(od) = munition_def(self.ctx.catalogue, b.kind[o]) else { continue };
            if !is_air_target(od.kind) { continue; }
            let there = prev[o] + (b.pos[o] - prev[o]) * t;
            if (there - at).length() <= lethal + od.radius_m.max(0.0) {
                killed.push(b.entity[o]);
                b.kill(o);
            }
        }
        BodyOutcome::AirBurst { body: id, at, killed }
    }

    /// Nearest hull within `reach` of `at`, skipping the launcher until the body has cleared it; ties go to
    /// the lower vessel id.
    fn nearest_hull(&self, b: &Bodies, r: usize, at: DVec3, reach: f64) -> Option<EntityId> {
        let mut best: Option<(f64, EntityId)> = None;
        for h in self.boxes {
            if h.vessel == b.launcher[r] && !b.cleared[r] { continue; }
            let d = h.distance(at);
            if d <= reach && best.is_none_or(|(bd, bid)| d < bd || (d == bd && h.vessel < bid)) { best = Some((d, h.vessel)); }
        }
        best.map(|(_, v)| v)
    }
}

/// Non-finite demands are treated as `Hold`.
fn sanitise(s: Steer) -> Steer {
    match s {
        Steer::Accel(a) if !a.is_finite() => Steer::Hold,
        Steer::Waypoint(p) if !p.is_finite() => Steer::Hold,
        Steer::Orbit { centre, radius_m } if !centre.is_finite() || !radius_m.is_finite() => Steer::Hold,
        other => other,
    }
}

/// Velocity with vertical component `vy` and the horizontal direction of `v`, rescaled so the total speed is
/// `speed` where it can be (a vertical body keeps no horizontal component).
fn with_vertical(v: DVec3, vy: f64, speed: f64) -> DVec3 {
    let flat = DVec3::new(v.x, 0.0, v.z);
    let fl = flat.length();
    let h = (speed * speed - vy * vy).max(0.0).sqrt();
    if fl > 1e-9 { flat / fl * h + DVec3::Y * vy } else { DVec3::Y * vy }
}

/// Move `x` toward `target` by at most `step`.
fn approach(x: f64, target: f64, step: f64) -> f64 {
    if x < target { (x + step).min(target) } else { (x - step).max(target) }
}

/// Lateral demand to turn the velocity toward unit direction `want`. A target dead astern turns to starboard
/// at the full limit rather than stalling on a zero cross-track demand.
fn pursuit(want: DVec3, dir: DVec3, speed: f64, vel: DVec3, max_accel: f64) -> DVec3 {
    let mut a = (want * speed.max(1.0) - vel) / PURSUIT_TAU_S;
    a -= dir * a.dot(dir);
    if a.length_squared() < 1e-6 && want.dot(dir) < 0.0 {
        let right = DVec3::new(-dir.z, 0.0, dir.x);
        a = if right.length_squared() > 1e-12 { right.normalize() * max_accel } else { DVec3::X * max_accel };
    }
    a
}

/// Lateral demand to fly a clockwise (seen from above) circle of `radius` around `centre`.
fn orbit_demand(pos: DVec3, vel: DVec3, centre: DVec3, radius: f64) -> DVec3 {
    let rel = DVec3::new(pos.x - centre.x, 0.0, pos.z - centre.z);
    let dist = rel.length();
    let speed = DVec3::new(vel.x, 0.0, vel.z).length().max(1.0);
    let (radial, tangent) = if dist > 1e-6 {
        let out = rel / dist;
        (out, DVec3::new(-out.z, 0.0, out.x))
    } else {
        (DVec3::X, DVec3::Z)
    };
    let err = if radius > 0.0 { ((dist - radius) / radius).clamp(-1.0, 1.0) } else { 0.0 };
    let want = (tangent - radial * err).normalize_or_zero();
    // Centripetal term keeps the circle; pursuit term corrects onto it.
    let centripetal = if dist > 1e-6 && radius > 0.0 { -radial * (speed * speed / radius) } else { DVec3::ZERO };
    centripetal + (want * speed - DVec3::new(vel.x, 0.0, vel.z)) / PURSUIT_TAU_S
}

/// Closest approach of two points moving linearly over the tick: (distance, parameter in 0..=1).
fn closest_approach(a0: DVec3, a1: DVec3, b0: DVec3, b1: DVec3) -> (f64, f64) {
    let r0 = a0 - b0;
    let dv = (a1 - b1) - r0;
    let dd = dv.length_squared();
    let t = if dd > 1e-18 { (-r0.dot(dv) / dd).clamp(0.0, 1.0) } else { 0.0 };
    ((r0 + dv * t).length(), t)
}
