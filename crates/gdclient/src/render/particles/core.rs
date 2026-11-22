//! Part of unit `gd-render-particles`. Layer B decisions with no Godot in it: which emitter each particle goes
//! to, where the emitters stand, the per-frame `emit_particle` budget, the live cap split and the degrade
//! steps. `gpu.rs` turns [`ParticleCore::emits`] into `particles_emit` calls. Tests run headless.
//!
//! # Emitters (L1951)
//! At most [`MAX_EMITTERS`] (20): one seed emitter per kind for one-shot bursts, and up to
//! [`ANCHOR_EMITTERS`] sub-emitters that follow an anchor (a burning hull's embers, a fast hull's bow spray).
//! Emitters are translation-only with `y = 0` and `local_coords` true, so a particle's local `y` is its
//! height above the calm sea, which the process shader's analytic water contact reads from `custom.w`.
//! Seed emitters stay put and shift with the render origin; an idle one returns to the origin. An anchor
//! emitter that loses its anchor keeps its place until its particles have died, then serves another.
//!
//! # Sources
//! Events, held until playback reaches their tick like every other effect: `Splash` (spray, and mist for big
//! shells), `ShellImpact` (sparks, embers when the shell burst inside), `BodyImpact` and `CookOff` (sparks
//! and embers), `BodyBurst` (flak and sparks in the air, spray and mist under water), `BodyDestroyed`
//! (sparks and embers; spray when it fell into the sea). State, every frame: embers over burning hulls
//! (`VesselFlags::BURNING`) and bow spray on hulls above [`BOW_SPRAY_MIN_KN`], nearest the camera first.
// Client rendering, not simulation (clippy.toml): std floats are allowed here. Nothing drawn from them feeds
// back into the sim, and a last-bit difference between platforms moves nothing on screen.
#![allow(clippy::disallowed_methods)]
use super::super::hull::design::DrawDesign;
use super::super::hull::gpu::pose_affine;
use crate::interp::ViewFrame;
use crate::seams::WireEvent;
use content::ids::ContentHash;
use glam::Vec3;
use sim_core::events::PhysEvent;
use sim_core::EntityId;
use sim_systems::protocol::{SimEvent, VesselFlags};
use std::collections::BTreeMap;
use std::sync::Arc;

/// Layer B kinds, in process-shader order (`particle_kind` 0..=4).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ParticleKind { Spray, Spark, Ember, Flak, Mist }
impl ParticleKind {
    pub const ALL: [ParticleKind; 5] = [Self::Spray, Self::Spark, Self::Ember, Self::Flak, Self::Mist];
    pub fn index(self) -> usize { self as usize }
    /// `vfx_kind` of the draw pass (`inc/vfx_common.gdshaderinc`: 32 spray .. 36 mist).
    pub fn draw_kind(self) -> i64 { 32 + self as i64 }
    /// Mist is the one blended Layer B kind (L1953).
    pub fn blended(self) -> bool { self == Self::Mist }
    /// Ramp the draw pass samples.
    pub fn ramp(self) -> hull_mesh::vfx::RampId {
        use hull_mesh::vfx::RampId;
        match self { Self::Spray => RampId::Foam, Self::Spark => RampId::Flash, Self::Ember => RampId::Fire, Self::Flak => RampId::Flak, Self::Mist => RampId::Smoke }
    }
    /// The Layer A mesh drawn for each particle: a puff for spray and mist, a box (stretched along its
    /// velocity by the process shader) for sparks, a cube for embers and flak fragments.
    pub fn mesh(self) -> hull_mesh::vfx::VfxKind {
        use hull_mesh::vfx::VfxKind;
        match self { Self::Spray => VfxKind::HitPuff, Self::Spark => VfxKind::Tracer, Self::Ember | Self::Flak => VfxKind::Debris, Self::Mist => VfxKind::SmokePuff }
    }
    /// Longest life the process shader gives this kind, seconds (`kind_life` upper bound).
    pub fn max_life_s(self) -> f32 { match self { Self::Spray => 2.4, Self::Spark => 0.8, Self::Ember => 4.5, Self::Flak => 1.5, Self::Mist => 6.0 } }
    /// Off from degrade step 2 (L2028: sparks, embers and spray go first).
    pub fn degradable(self) -> bool { matches!(self, Self::Spray | Self::Spark | Self::Ember) }
    /// Share of the live cap the kind's seed emitter holds.
    fn seed_share(self) -> f32 { match self { Self::Spray => 0.22, Self::Spark => 0.14, Self::Ember => 0.08, Self::Flak => 0.12, Self::Mist => 0.04 } }
}

pub const MAX_EMITTERS: usize = 20;
pub const SEED_EMITTERS: usize = ParticleKind::ALL.len();
pub const ANCHOR_EMITTERS: usize = MAX_EMITTERS - SEED_EMITTERS;
/// Share of the live cap the anchor emitters hold together.
pub const ANCHOR_SHARE: f32 = 0.40;
/// `emit_particle` calls per frame (L1951).
pub const MAX_EMITS_PER_FRAME: usize = 128;
/// Every emitter's system lifetime, seconds: above every kind's own life so the engine never ends a
/// particle before the process shader does.
pub const SYSTEM_LIFETIME_S: f64 = 8.0;
/// Fixed process rates (L1951, L2029).
pub const FIXED_FPS: i32 = 30;
pub const DEGRADED_FPS: i32 = 15;
pub const DEGRADE_OFF: u8 = 2;
pub const DEGRADE_SLOW: u8 = 3;
/// An idle seed emitter farther than this from the render origin returns to it.
pub const SEED_RECENTRE_M: f32 = 2_000.0;
/// Bow spray (L1951 "spray"): from this speed, at this many particles per second per knot above it.
pub const BOW_SPRAY_MIN_KN: f32 = 12.0;
pub const BOW_SPRAY_PER_KN_S: f32 = 0.8;
/// Embers per second over a burning hull.
pub const EMBER_HZ: f32 = 6.0;
/// Anchors farther than this from the camera get no emitter.
pub const ANCHOR_RANGE_M: f32 = 2_500.0;
/// Box margin around an emitter's particles, metres (throw distance of the fastest kind).
pub const AABB_PAD_M: f32 = 80.0;

/// One `particles_emit` request: emitter, local position, optional velocity, lifetime (0 = the kind's
/// own), seed, and the emission point's height above the sea (`custom.w`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Emit { pub emitter: u8, pub local: Vec3, pub velocity: Option<Vec3>, pub life_s: f32, pub seed: f32 }

/// What an anchor emitter follows.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum AnchorKey { Fire(EntityId), Bow(EntityId) }
impl AnchorKey { fn kind(self) -> ParticleKind { match self { Self::Fire(_) => ParticleKind::Ember, Self::Bow(_) => ParticleKind::Spray } } }

#[derive(Clone, Debug)]
pub struct Emitter {
    pub kind: ParticleKind,
    /// Render-space translation (y always 0).
    pub at: Vec3,
    pub owner: Option<AnchorKey>,
    /// Simulation time of its last emission.
    pub last_emit_s: f64,
    /// Particle capacity (`particles_set_amount`).
    pub amount: u32,
    /// Local box around its live particles.
    pub min: Vec3,
    pub max: Vec3,
    /// Fractional emissions carried between frames (steady anchors).
    carry: f32,
    counter: u32,
}
impl Emitter {
    fn new(kind: ParticleKind) -> Self { Self { kind, at: Vec3::ZERO, owner: None, last_emit_s: f64::NEG_INFINITY, amount: 0, min: Vec3::ZERO, max: Vec3::ZERO, carry: 0.0, counter: 0 } }
    pub fn idle(&self, now: f64) -> bool { now - self.last_emit_s > self.kind.max_life_s() as f64 + 0.25 || now < self.last_emit_s }
    fn grow(&mut self, p: Vec3, now: f64) {
        if self.idle(now) { self.min = p; self.max = p; } else { self.min = self.min.min(p); self.max = self.max.max(p); }
        self.last_emit_s = now;
    }
}

/// A burst asked for this frame, before the budget.
#[derive(Clone, Copy, Debug)]
struct Burst { kind: ParticleKind, at: Vec3, count: u32, priority: u8, seed: u32, velocity: Option<Vec3>, spread: f32 }

pub struct ParticleCore {
    pub emitters: Vec<Emitter>,
    /// This frame's emit requests, at most [`MAX_EMITS_PER_FRAME`].
    pub emits: Vec<Emit>,
    /// Live cap after the tier, thermal scale and halvings; 0 builds nothing.
    pub cap: u32,
    pub fps: i32,
    pub enabled: [bool; 5],
    /// Particles alive now (estimated from what was emitted and the kinds' lives).
    pub live: u32,
    /// Draw one particle into every emitter on the next frame and ask for a 0.1 s pre-process (L1973).
    pub warm: bool,
    queue: Vec<WireEvent>,
    sizes: BTreeMap<ContentHash, Vec3>,
    hashes: BTreeMap<EntityId, ContentHash>,
    deaths: std::collections::VecDeque<f64>,
    /// Wind in the render frame, m/s (the process materials' `wind`).
    pub wind: Vec3,
    last_s: Option<f64>,
    counter: u32,
}

fn hash32(a: u32, b: u32) -> u32 { hull_mesh::vfx::runtime::effect_seed(a, b) }
fn unit(h: u32) -> f32 { (h >> 8) as f32 / (1u32 << 24) as f32 }

impl ParticleCore {
    pub fn new(cap: u32) -> Self {
        let mut emitters: Vec<Emitter> = ParticleKind::ALL.iter().map(|k| Emitter::new(*k)).collect();
        emitters.extend((0..ANCHOR_EMITTERS).map(|_| Emitter::new(ParticleKind::Ember)));
        let mut c = Self { emitters, emits: Vec::new(), cap: 0, fps: FIXED_FPS, enabled: [true; 5], live: 0, warm: false, queue: Vec::new(), sizes: BTreeMap::new(), hashes: BTreeMap::new(), deaths: Default::default(), wind: Vec3::ZERO, last_s: None, counter: 0 };
        c.set_cap(cap);
        c
    }

    /// Split a live cap over the emitters: seeds by kind share, anchors evenly. Returns whether any amount
    /// changed (the engine reallocates those emitters).
    pub fn set_cap(&mut self, cap: u32) -> bool {
        let before: Vec<u32> = self.emitters.iter().map(|e| e.amount).collect();
        self.cap = cap;
        let c = cap as f32;
        for (i, e) in self.emitters.iter_mut().enumerate() {
            let share = if i < SEED_EMITTERS { e.kind.seed_share() } else { ANCHOR_SHARE / ANCHOR_EMITTERS as f32 };
            e.amount = if cap == 0 { 0 } else { ((c * share) as u32).max(16) };
        }
        // Rounding up to 16 may not push the total over the cap.
        while cap > 0 && self.emitters.iter().map(|e| e.amount).sum::<u32>() > cap {
            let Some(e) = self.emitters.iter_mut().max_by_key(|e| e.amount) else { break };
            if e.amount <= 1 { break; }
            e.amount -= 1;
        }
        before.iter().zip(&self.emitters).any(|(b, e)| *b != e.amount)
    }

    /// Drop pending bursts and anchors; a new sortie (`sortie`) also forgets the reveals, a rewound timeline
    /// keeps them (they are not sent again).
    fn reset(&mut self, sortie: bool) {
        for e in &mut self.emitters { e.owner = None; e.last_emit_s = f64::NEG_INFINITY; e.carry = 0.0; e.at = Vec3::ZERO; }
        self.queue.clear();
        if sortie { self.hashes.clear(); }
        self.deaths.clear();
        self.last_s = None;
    }

    fn next_seed(&mut self, tick: u64) -> u32 { self.counter = self.counter.wrapping_add(1); hash32(tick as u32 ^ (tick >> 32) as u32, self.counter) }

    /// One frame. `camera` is render space and `forward` its unit view direction; `degrade_step` is rung 1's
    /// step (L2026-2031).
    pub fn step(&mut self, view: &ViewFrame, events: &[WireEvent], camera: Vec3, forward: Vec3, degrade_step: u8, lookup: &dyn Fn(&ContentHash) -> Option<Arc<DrawDesign>>) {
        self.emits.clear();
        let now = view.sim_time_s;
        let tick = view.render_tick;
        if self.last_s.is_some_and(|l| now + 1e-6 < l) { self.reset(false); }
        let t0 = self.last_s.unwrap_or(now);
        self.last_s = Some(now);
        let dt = (now - t0).clamp(0.0, 0.25) as f32;
        for k in ParticleKind::ALL { self.enabled[k.index()] = !(k.degradable() && degrade_step >= DEGRADE_OFF); }
        self.fps = if degrade_step >= DEGRADE_SLOW { DEGRADED_FPS } else { FIXED_FPS };
        if let Some(d) = view.origin_shift.map(Vec3::from).filter(|d| d.is_finite()) {
            for e in &mut self.emitters { e.at += Vec3::new(d.x, 0.0, d.z); }
        }
        for e in self.emitters.iter_mut().take(SEED_EMITTERS) {
            if e.idle(now) && e.at.length() > SEED_RECENTRE_M { e.at = Vec3::ZERO; }
        }
        if self.cap == 0 { self.queue.clear(); return; }

        let mut bursts: Vec<Burst> = Vec::new();
        for ev in events {
            match &ev.event {
                SimEvent::SortieStarted { .. } => self.reset(true),
                SimEvent::EntityRevealed(intro) => { self.hashes.insert(intro.entity, intro.blueprint); }
                SimEvent::EntityHidden { entity } => { self.hashes.remove(entity); }
                SimEvent::Phys(PhysEvent::WeatherChanged { weather, .. }) => self.wind = super::super::vfx::core::wind_vector(weather),
                SimEvent::Phys(PhysEvent::Splash { .. } | PhysEvent::ShellImpact { .. } | PhysEvent::BodyImpact { .. } | PhysEvent::CookOff { .. } | PhysEvent::BodyBurst { .. } | PhysEvent::BodyDestroyed { .. }) => self.queue.push(ev.clone()),
                _ => {}
            }
        }
        let mut due = Vec::new();
        self.queue.retain(|e| {
            let ahead = e.tick as f64 - tick;
            if ahead <= 0.0 || ahead > super::super::vfx::core::EVENT_HOLD_MAX_TICKS { due.push(e.clone()); false } else { true }
        });
        for ev in &due { self.burst_of(ev, view, &mut bursts); }

        // Anchors: fires first (fire presence reads at range), then bow spray; nearest first.
        let mut wanted: Vec<(f32, AnchorKey, Vec3, f32, Vec3)> = Vec::new();
        for p in &view.vessels {
            let xf = pose_affine(p.pos, p.rot);
            let size = self.size_of(p.entity, lookup);
            let burning = p.flags.contains(VesselFlags::BURNING) && !p.flags.contains(VesselFlags::SUBMERGED);
            if burning {
                let at = xf.transform_point3(Vec3::new(size.x * 0.5, size.y * 0.75, size.z * 0.5));
                let d = (at - camera).length();
                if at.is_finite() && d < ANCHOR_RANGE_M { wanted.push((d, AnchorKey::Fire(p.entity), at, EMBER_HZ, Vec3::ZERO)); }
            }
            let kn = Vec3::new(p.vel[0], 0.0, p.vel[2]).length() * 1.943_844_5;
            if kn > BOW_SPRAY_MIN_KN && !p.flags.contains(VesselFlags::SUBMERGED) {
                let at = xf.transform_point3(Vec3::new(size.x * 0.97, (size.y * 0.3).min(2.0), size.z * 0.5));
                let at = Vec3::new(at.x, at.y.clamp(0.3, 3.0), at.z);
                let d = (at - camera).length();
                let vel = Vec3::from(p.vel);
                if at.is_finite() && d < ANCHOR_RANGE_M { wanted.push((d + 1.0e5, AnchorKey::Bow(p.entity), at, (kn - BOW_SPRAY_MIN_KN) * BOW_SPRAY_PER_KN_S, vel)); }
            }
        }
        wanted.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
        wanted.truncate(ANCHOR_EMITTERS);
        // Release emitters whose anchor is gone; place the kept ones.
        for e in self.emitters.iter_mut().skip(SEED_EMITTERS) {
            if let Some(owner) = e.owner { if !wanted.iter().any(|w| w.1 == owner) { e.owner = None; } }
        }
        let mut steady: Vec<(usize, f32, Vec3, Vec3)> = Vec::new();
        for (_, key, at, rate, vel) in &wanted {
            if !self.enabled[key.kind().index()] { continue; }
            let slot = self.emitters.iter().skip(SEED_EMITTERS).position(|e| e.owner == Some(*key)).map(|i| i + SEED_EMITTERS)
                .or_else(|| self.emitters.iter().skip(SEED_EMITTERS).position(|e| e.owner.is_none() && e.idle(now)).map(|i| i + SEED_EMITTERS));
            let Some(i) = slot else { continue };
            let e = &mut self.emitters[i];
            if e.owner != Some(*key) { e.owner = Some(*key); e.kind = key.kind(); e.carry = 0.0; }
            e.at = Vec3::new(at.x, 0.0, at.z);
            steady.push((i, *rate, Vec3::new(0.0, at.y, 0.0), *vel));
        }

        // Spend the call budget: bursts by priority, then steady anchors.
        bursts.sort_by_key(|b| b.priority);
        let mut budget = MAX_EMITS_PER_FRAME;
        for b in bursts {
            if !self.enabled[b.kind.index()] { continue; }
            let i = b.kind.index();
            for k in 0..b.count {
                if budget == 0 { break; }
                let h = hash32(b.seed, k);
                let jitter = Vec3::new(unit(hash32(h, 1)) - 0.5, unit(hash32(h, 2)) * 0.5, unit(hash32(h, 3)) - 0.5) * b.spread;
                let world = b.at + jitter;
                let e = &mut self.emitters[i];
                let local = world - e.at;
                e.grow(local, now);
                self.emits.push(Emit { emitter: i as u8, local, velocity: b.velocity, life_s: 0.0, seed: unit(hash32(h, 4)) });
                budget -= 1;
                let life = b.kind.max_life_s() as f64;
                self.deaths.push_back(now + life);
            }
        }
        for (i, rate, local, vel) in steady {
            let e = &mut self.emitters[i];
            e.carry += rate * dt;
            while e.carry >= 1.0 && budget > 0 {
                e.carry -= 1.0;
                e.counter = e.counter.wrapping_add(1);
                let h = hash32(i as u32 * 7919 + 13, e.counter);
                let side = if e.counter.is_multiple_of(2) { 1.0 } else { -1.0 };
                let (pos, velocity) = match e.kind {
                    // Bow spray thrown out to both sides and up, against the hull's own motion.
                    ParticleKind::Spray => {
                        let v = Vec3::new(vel.x, 0.0, vel.z);
                        let across = Vec3::new(-v.z, 0.0, v.x).normalize_or(Vec3::X) * side;
                        let speed = v.length();
                        (local + across * 1.5, Some(across * (2.0 + 0.25 * speed) + Vec3::Y * (3.0 + 0.3 * speed) * (0.7 + 0.6 * unit(h))))
                    }
                    _ => (local + Vec3::new(unit(hash32(h, 1)) - 0.5, 0.0, unit(hash32(h, 2)) - 0.5) * 3.0, None),
                };
                e.grow(pos, now);
                self.emits.push(Emit { emitter: i as u8, local: pos, velocity, life_s: 0.0, seed: unit(hash32(h, 3)) });
                budget -= 1;
                self.deaths.push_back(now + e.kind.max_life_s() as f64);
            }
            if budget == 0 { e.carry = e.carry.min(1.0); }
        }
        if self.warm {
            self.warm = false;
            for (i, e) in self.emitters.iter_mut().enumerate() {
                let local = camera + forward.normalize_or(Vec3::NEG_Z) * 40.0 - e.at;
                e.grow(local, now);
                self.emits.push(Emit { emitter: i as u8, local, velocity: Some(Vec3::ZERO), life_s: 0.05, seed: 0.5 });
            }
        }
        while self.deaths.front().is_some_and(|d| *d <= now) { self.deaths.pop_front(); }
        self.live = (self.deaths.len() as u32).min(self.cap);
    }

    fn size_of(&mut self, e: EntityId, lookup: &dyn Fn(&ContentHash) -> Option<Arc<DrawDesign>>) -> Vec3 {
        let fallback = Vec3::new(40.0, 6.0, 8.0);
        let Some(hash) = self.hashes.get(&e).copied() else { return fallback };
        if let Some(s) = self.sizes.get(&hash) { return *s; }
        match lookup(&hash) {
            Some(d) => { let s = Vec3::from(d.size_m()); self.sizes.insert(hash, s); s }
            None => fallback,
        }
    }

    fn burst_of(&mut self, ev: &WireEvent, view: &ViewFrame, out: &mut Vec<Burst>) {
        let world = |p: glam::DVec3| Vec3::from(view.origin.to_render(p));
        let seed = self.next_seed(ev.tick);
        let mut push = |kind, at: Vec3, count: u32, priority: u8, velocity: Option<Vec3>, spread: f32, k: u32| {
            if at.is_finite() && count > 0 { out.push(Burst { kind, at, count, priority, seed: hash32(seed, k), velocity, spread }); }
        };
        match &ev.event {
            SimEvent::Phys(PhysEvent::Splash { pos, calibre_mm, .. }) => {
                let p = world(*pos);
                let at = Vec3::new(p.x, 0.2, p.z);
                let cal = *calibre_mm as u32;
                push(ParticleKind::Spray, at, (4 + cal / 16).min(24), 2, None, 2.0 + 0.01 * cal as f32, 1);
                if cal >= 150 { push(ParticleKind::Mist, at + Vec3::Y * 3.0, 2 + cal / 200, 4, None, 6.0, 2); }
            }
            SimEvent::Phys(PhysEvent::ShellImpact { world_point, burst, .. }) => {
                let p = world(*world_point);
                push(ParticleKind::Spark, p, if *burst { 20 } else { 10 }, 1, None, 0.5, 1);
                if *burst { push(ParticleKind::Ember, p, 6, 3, None, 1.0, 2); }
            }
            SimEvent::Phys(PhysEvent::BodyImpact { world_point, .. }) => {
                let p = world(*world_point);
                push(ParticleKind::Spark, p, 24, 1, None, 1.0, 1);
                push(ParticleKind::Ember, p, 10, 3, None, 2.0, 2);
            }
            SimEvent::Phys(PhysEvent::CookOff { pos, radius_cells, .. }) => {
                let p = world(*pos);
                let r = (*radius_cells).max(1) as u32;
                push(ParticleKind::Spark, p, (16 + 4 * r).min(40), 1, None, 1.5, 1);
                push(ParticleKind::Ember, p, (8 + 2 * r).min(24), 3, None, 3.0, 2);
            }
            SimEvent::Phys(PhysEvent::BodyBurst { pos, underwater, .. }) => {
                let p = world(*pos);
                if *underwater {
                    let at = Vec3::new(p.x, 0.2, p.z);
                    push(ParticleKind::Spray, at, 24, 2, None, 6.0, 1);
                    push(ParticleKind::Mist, at + Vec3::Y * 6.0, 4, 4, None, 10.0, 2);
                } else {
                    push(ParticleKind::Flak, p, 18, 0, None, 1.0, 1);
                    push(ParticleKind::Spark, p, 8, 1, None, 0.5, 2);
                }
            }
            SimEvent::Phys(PhysEvent::BodyDestroyed { pos, .. }) => {
                let p = world(*pos);
                if p.y < 0.5 { push(ParticleKind::Spray, Vec3::new(p.x, 0.2, p.z), 10, 2, None, 2.0, 1); } else {
                    push(ParticleKind::Flak, p, 10, 0, None, 1.0, 1);
                    push(ParticleKind::Spark, p, 8, 1, None, 0.5, 2);
                    push(ParticleKind::Ember, p, 4, 3, None, 1.0, 3);
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::interp::VesselPose;
    use glam::DVec3;
    use sim_core::projectiles::ProjectileId;

    const HZ: f64 = sim_core::TICK_HZ as f64;
    fn ev(tick: u64, event: SimEvent) -> WireEvent { WireEvent { tick, event } }
    fn frame(tick: f64, vessels: Vec<VesselPose>) -> ViewFrame { ViewFrame { render_tick: tick, sim_time_s: tick / HZ, vessels, ..Default::default() } }
    fn splash(tick: u64, x: f64) -> WireEvent { ev(tick, SimEvent::Phys(PhysEvent::Splash { projectile: ProjectileId(1), pos: DVec3::new(x, 0.0, 0.0), calibre_mm: 203, shooter: EntityId::NONE })) }
    fn none(_: &ContentHash) -> Option<Arc<DrawDesign>> { None }

    #[test]
    fn cap_split_fits_the_cap() {
        for cap in [32_768u32, 8_192, 4_096, 512] {
            let c = ParticleCore::new(cap);
            let total: u32 = c.emitters.iter().map(|e| e.amount).sum();
            assert!(total <= cap, "{cap}: {total}");
            assert!(c.emitters.iter().all(|e| e.amount >= 1));
            assert_eq!(c.emitters.len(), MAX_EMITTERS);
        }
        let mut c = ParticleCore::new(32_768);
        assert!(c.set_cap(16_384), "halving reallocates");
        assert!(!c.set_cap(16_384));
        assert!(c.emitters[ParticleKind::Spray.index()].amount > c.emitters[ParticleKind::Mist.index()].amount);
    }

    #[test]
    fn bursts_wait_for_playback_and_keep_the_call_budget() {
        let mut c = ParticleCore::new(32_768);
        c.step(&frame(100.0, vec![]), &[splash(105, 50.0)], Vec3::ZERO, Vec3::NEG_Z, 0, &none);
        assert!(c.emits.is_empty());
        c.step(&frame(105.0, vec![]), &[], Vec3::ZERO, Vec3::NEG_Z, 0, &none);
        let spray = c.emits.iter().filter(|e| e.emitter as usize == ParticleKind::Spray.index()).count();
        let mist = c.emits.iter().filter(|e| e.emitter as usize == ParticleKind::Mist.index()).count();
        assert!(spray >= 4 && mist >= 2, "{spray} spray, {mist} mist");
        // Water contact reads the emission height: every spray particle starts above the sea.
        assert!(c.emits.iter().all(|e| e.local.y >= 0.0 && e.local.is_finite()));
        // Forty splashes in one frame: at most 128 calls.
        let many: Vec<WireEvent> = (0..40).map(|k| splash(106, k as f64 * 10.0)).collect();
        c.step(&frame(106.0, vec![]), &many, Vec3::ZERO, Vec3::NEG_Z, 0, &none);
        assert_eq!(c.emits.len(), MAX_EMITS_PER_FRAME);
        assert!(c.live > 0 && c.live <= c.cap);
        // Degrade step 2: spray, sparks and embers off; mist still shows; step 3 halves the rate.
        c.step(&frame(107.0, vec![]), &[splash(107, 0.0)], Vec3::ZERO, Vec3::NEG_Z, 3, &none);
        assert!(c.emits.iter().all(|e| e.emitter as usize == ParticleKind::Mist.index()));
        assert_eq!(c.fps, DEGRADED_FPS);
        assert!(!c.enabled[ParticleKind::Spark.index()]);
    }

    #[test]
    fn anchors_follow_hulls_and_the_origin() {
        let mut c = ParticleCore::new(8_192);
        let fast = |x: f32| VesselPose { entity: EntityId::from_u64(3), pos: [x, -1.0, 0.0], rot: [0.0, 0.0, 0.0, 1.0], vel: [12.0, 0.0, 0.0], ..Default::default() };
        let mut emitted = 0;
        for k in 0..60 {
            c.step(&frame(k as f64, vec![fast(k as f32 * 0.2)]), &[], Vec3::ZERO, Vec3::NEG_Z, 0, &none);
            emitted += c.emits.len();
        }
        let owner = c.emitters.iter().position(|e| e.owner == Some(AnchorKey::Bow(EntityId::from_u64(3)))).expect("bow spray has an emitter");
        assert!(owner >= SEED_EMITTERS);
        assert!(emitted >= 5, "about 9 a second at 23 kn: {emitted}");
        // The emitter stands at the bow, on the sea.
        assert_eq!(c.emitters[owner].at.y, 0.0);
        assert!((c.emitters[owner].at.x - (11.8 + 40.0 * 0.97)).abs() < 1.0);
        // A rebase moves every emitter.
        let mut v = frame(60.0, vec![fast(12.0 - 1000.0)]);
        v.origin_shift = Some([-1000.0, 0.0, 0.0]);
        let before = c.emitters[0].at.x;
        c.step(&v, &[], Vec3::ZERO, Vec3::NEG_Z, 0, &none);
        assert!((c.emitters[0].at.x - (before - 1000.0)).abs() < 1e-3);
        // The hull stops: the emitter is released, but stays where its particles are until they die.
        c.step(&frame(61.0, vec![]), &[], Vec3::ZERO, Vec3::NEG_Z, 0, &none);
        assert!(c.emitters[owner].owner.is_none());
        assert!(!c.emitters[owner].idle(61.0 / HZ));
        // Fallback tier: nothing at all.
        let mut off = ParticleCore::new(0);
        off.step(&frame(1.0, vec![]), &[splash(1, 0.0)], Vec3::ZERO, Vec3::NEG_Z, 0, &none);
        assert!(off.emits.is_empty());
    }
}
