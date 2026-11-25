//! Unit `hm-vfx-runtime`. Godot-free Layer A bookkeeping the client adapter drives each frame: ring-slot pools
//! with tier caps and protected slots, per-effect instance builders, back-to-front sorting and the flash limiter.
//! DESIGN.md L1949 (ring slots; reused slot draws scale 0 for a frame; sort blended back to front),
//! L1957 (accessibility floor 6), L1959-1969 (effect rules), L1972 (caps), L2026-2033 (degrade order and the
//! never-degraded list), L1831 (floating origin: anchors live 30 s past a rebase; positions are render-origin
//! relative f32).
//!
//! The client owns time (render-interpolated sim time) and the render origin; everything here is deterministic
//! given its inputs so it tests headless.
//!
//! # Frame order
//! Per kind per frame: `spawn` / `update` / `retire` as events arrive, then `tick(now)`, then
//! `write_buffer(camera, ..)` and one `multimesh_set_buffer` of `instances.len()` instances
//! (`visible_instance_count` may be set to [`RingPool::visible_count`]). `tick` before the spawns works too.
//! An instance always draws at least one visible frame: its age starts at its first visible frame, so a 2-frame
//! hit flash spawned into a just-recycled slot still shows twice.
//!
//! # Instance data written by the builders
//! The transform maps the kind's unit mesh (`vfx::meshes`) into render-origin-relative metres. Colour is the
//! tint (white unless noted) multiplied into the ramp. `custom.x` age s (the pool writes it), `custom.y`
//! lifetime s (the builder's value is kept; 0 means "the lifetime passed to `spawn`"), `custom.z` seed 0..1,
//! `custom.w` per kind:
//!
//! | Kind | Transform | `custom.w` |
//! | --- | --- | --- |
//! | Tracer | X = velocity direction x length, centred half a length behind the shell | emission 4..8 |
//! | SplashColumn | unit column scaled to its diameter and height, base 0.5 m under the surface | calibre factor 0..1 (height / 40 m) |
//! | SplashFoam | ring scaled to its final diameter | calibre factor |
//! | HitFlash | star scaled to its size | emission scale before the flash limiter (1) |
//! | HitPuff | local +Y = hit normal | drift speed m/s along normalised local +Y |
//! | Debris | 0.5 m cube, local +Y = launch direction | launch speed m/s along normalised local +Y; the shader adds gravity [`GRAVITY`] and tumbles by seed |
//! | WakeSegment | X = b - a, Z = width across | speed factor 0..1 (0 kn .. 30 kn) |
//! | KelvinArm | X = one emission interval of arm, Z = arm width | signed outward drift m/s along normalised local +Z |
//! | BowWave | X = forward x length (mesh runs aft), Z = starboard x width, Y = height | speed factor |
//! | MissileTrail | X = b - a, Y and Z = trail diameter | 0 |
//! | FlamePrism | X = downwind x base width / 0.9, Y = up x height sheared downwind | emission 2..4 |
//! | SmokePuff, MuzzleHaze | uniform diameter, local +Y = drift direction | drift speed m/s along normalised local +Y |
//! | Fireball | uniform diameter | emission 2..4 |
//! | MuzzleStar | X = bore direction x star length | emission scale before the flash limiter (1) |
//! | FlakBurst | uniform diameter | 0 |
//! | ChaffCloud | diameter (Y flattened to 0.6), slow tumble by age | bloom 0..1 |
//! | FlareBurn | uniform diameter, spun by age | emission 6 |
//! | DefenceSmoke | puff diameter, Y flattened | churn rate rad/s |
//! | PumpArc | X = outward x throw / 0.9, Y = up x drop / 0.85 | flow 0..1 |
//! | BoilRing | ring diameter, Y = diameter x intensity | intensity 0..1 |
//! | FoamBand | X = b - a along the waterline, Z = outward x width | freeboard lost 0..1 |
//!
//! Attached effects (bow wave, defence smoke, pump arcs, boil rings, foam bands) are spawned with
//! `f32::INFINITY` and `update`d each frame; chaff and flares are spawned with their decoy life and `update`d
//! from the decoy's age. Continuous emitters (fire smoke, flame tongues) are paced by [`emissions_between`].
//!
//! Splash columns animate over their own lifetime with [`splash_column_phases`]: rise, hold 0.4 s, collapse.
use super::{CapGroup, VfxInstance, VfxKind, PROTECTED_OWN_SPLASHES};
use crate::types::QualityTier;
use std::collections::{BTreeSet, VecDeque};

/// Handle to a live instance: kind plus slot index; `generation` guards against stale handles after recycling.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct VfxHandle { pub kind: VfxKind, pub slot: u32, pub generation: u32 }

/// Why an instance must survive recycling (never-degraded cues, L2033; own-side splashes, L1962).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Protection { #[default] None, OwnSplash, IncomingMissileHead, Fire, Flooding, DefenceSmoke }

/// `custom.y` written for an instance spawned with an infinite lifetime (kept finite for the GPU; age / life
/// stays near 0, the start of the ramp).
pub const OPEN_ENDED_S: f32 = 1.0e9;

/// Slots a growable (`CapGroup::Protected`) pool adds at a time, so the client resizes its MultiMesh rarely.
pub const GROW_SLOTS: usize = 64;

/// Fixed ring-slot pool for one kind. Oldest unprotected slot recycles when full; a recycled slot is written as
/// `VfxInstance::HIDDEN` for one frame before its new contents (L1949).
#[derive(Clone, Debug)]
pub struct RingPool {
    pub kind: VfxKind,
    pub cap: usize,
    pub instances: Vec<VfxInstance>,
    pub born_s: Vec<f64>,
    pub lifetime_s: Vec<f32>,
    pub protection: Vec<Protection>,
    pub generation: Vec<u32>,
    /// Slots reused this frame (drawn hidden for one frame).
    pub fresh: Vec<u32>,
    live: Vec<bool>,
    /// Written visible in the last buffer.
    shown: Vec<bool>,
    /// Spawned but not yet written visible: its age is held at 0 and it cannot expire.
    pending: Vec<bool>,
    /// Drawn hidden in the next buffer (mirror of `fresh`).
    hide: Vec<bool>,
    free: BTreeSet<u32>,
    /// Live slots in spawn order (entries go stale when a slot retires or regenerates).
    order: VecDeque<(u32, u32)>,
    /// Live own-side splash slots in spawn order, for the `PROTECTED_OWN_SPLASHES` limit.
    own: VecDeque<(u32, u32)>,
    now_s: f64,
    visible: usize,
    scratch: Vec<(f32, u32)>,
}
impl RingPool {
    /// Pool sized to `kind.cap_group().cap(tier)`. `Protected` kinds grow on demand. Unit: `hm-vfx-runtime`.
    pub fn new(kind: VfxKind, tier: QualityTier) -> Self {
        let cap = kind.cap_group().cap(tier);
        let mut p = RingPool {
            kind, cap,
            instances: Vec::new(), born_s: Vec::new(), lifetime_s: Vec::new(), protection: Vec::new(), generation: Vec::new(),
            fresh: Vec::new(), live: Vec::new(), shown: Vec::new(), pending: Vec::new(), hide: Vec::new(),
            free: BTreeSet::new(), order: VecDeque::new(), own: VecDeque::new(), now_s: 0.0, visible: 0, scratch: Vec::new(),
        };
        p.grow(if cap == usize::MAX { GROW_SLOTS } else { cap });
        p
    }

    fn grow(&mut self, by: usize) {
        let start = self.instances.len();
        let n = start + by;
        self.instances.resize(n, VfxInstance::HIDDEN);
        self.born_s.resize(n, 0.0);
        self.lifetime_s.resize(n, 0.0);
        self.protection.resize(n, Protection::None);
        self.generation.resize(n, 0);
        self.live.resize(n, false);
        self.shown.resize(n, false);
        self.pending.resize(n, false);
        self.hide.resize(n, false);
        self.free.extend(start as u32..n as u32);
    }

    fn valid(&self, h: VfxHandle) -> bool {
        h.kind == self.kind && (h.slot as usize) < self.instances.len() && self.live[h.slot as usize] && self.generation[h.slot as usize] == h.generation
    }

    /// Whether slot `s` may not be recycled at `now`. A missile trail segment is protected only while it is part
    /// of the 2 s head (L1966).
    fn protected_now(&self, s: usize, now: f64) -> bool {
        match self.protection[s] {
            Protection::None => false,
            Protection::IncomingMissileHead => now - self.born_s[s] < TRAIL_HEAD_S as f64,
            _ => true,
        }
    }

    /// Own-side splash slots this pool keeps protected at once: 64 splashes' worth (L1962), five columns each.
    fn own_limit(&self) -> usize {
        match self.kind { VfxKind::SplashColumn => PROTECTED_OWN_SPLASHES * 5, _ => PROTECTED_OWN_SPLASHES }
    }

    fn retire_slot(&mut self, s: usize) {
        self.live[s] = false;
        self.pending[s] = false;
        self.protection[s] = Protection::None;
        self.instances[s] = VfxInstance::HIDDEN;
        self.free.insert(s as u32);
    }

    /// Claim a slot at time `now_s` for `lifetime_s`; recycles the oldest unprotected slot when full and returns
    /// `None` only when every slot is protected. Unit: `hm-vfx-runtime`.
    ///
    /// Free slots are taken lowest first (preferring ones hidden last frame, which need no hidden frame), so live
    /// slots stay packed at the front. A builder-set `custom.y` caps the lifetime. A NaN or negative lifetime is
    /// 0 (the instance still draws one frame); `f32::INFINITY` lives until [`RingPool::retire`]. Past
    /// `PROTECTED_OWN_SPLASHES` splashes, the oldest own-side splash loses its protection so the newest fall of
    /// shot always gets a slot.
    pub fn spawn(&mut self, now_s: f64, lifetime_s: f32, protection: Protection, instance: VfxInstance) -> Option<VfxHandle> {
        let now = if now_s.is_finite() { now_s } else { self.now_s };
        let life = if lifetime_s.is_nan() || lifetime_s < 0.0 { 0.0 } else { lifetime_s };
        if self.free.is_empty() && self.cap == usize::MAX { self.grow(GROW_SLOTS); }
        if protection == Protection::OwnSplash {
            // Release the oldest own-side splash past the limit first, so it can be the slot recycled below.
            let (live, gen, prot) = (&self.live, &self.generation, &self.protection);
            self.own.retain(|&(s, g)| live[s as usize] && gen[s as usize] == g && prot[s as usize] == Protection::OwnSplash);
            while self.own.len() >= self.own_limit() {
                let Some((s, _)) = self.own.pop_front() else { break };
                self.protection[s as usize] = Protection::None;
            }
        }
        // Prefer a free slot that was hidden last frame, so the new instance draws at once (a slot retired this
        // frame would need a hidden frame first).
        let pick = self.free.iter().copied().find(|&s| !self.shown[s as usize]).or_else(|| self.free.first().copied());
        let slot = match pick {
            Some(s) => { self.free.remove(&s); s as usize }
            None => {
                let mut victim = None;
                for _ in 0..self.order.len() {
                    let Some((s, g)) = self.order.pop_front() else { break };
                    let su = s as usize;
                    if !self.live[su] || self.generation[su] != g { continue; }
                    if self.protected_now(su, now) { self.order.push_back((s, g)); continue; }
                    victim = Some(su);
                    break;
                }
                let s = victim?;
                self.retire_slot(s);
                self.free.remove(&(s as u32));
                s
            }
        };
        let g = self.generation[slot].wrapping_add(1);
        self.generation[slot] = g;
        let mut inst = instance;
        inst.custom[0] = 0.0;
        // A builder-set lifetime (custom.y) is the instance's own timeline: the pool never keeps it longer.
        let life = if inst.custom[1] > 0.0 && inst.custom[1].is_finite() { life.min(inst.custom[1]) } else {
            inst.custom[1] = if life.is_finite() { life } else { OPEN_ENDED_S };
            life
        };
        self.instances[slot] = inst;
        self.born_s[slot] = now;
        self.lifetime_s[slot] = life;
        self.protection[slot] = protection;
        self.live[slot] = true;
        self.pending[slot] = true;
        if self.shown[slot] && !self.kind.blended() && !self.hide[slot] {
            self.hide[slot] = true;
            self.fresh.push(slot as u32);
        }
        self.order.push_back((slot as u32, g));
        if protection == Protection::OwnSplash { self.own.push_back((slot as u32, g)); }
        if self.order.len() > 4 * self.instances.len() + 16 { self.compact_queues(); }
        Some(VfxHandle { kind: self.kind, slot: slot as u32, generation: g })
    }

    /// Drop stale queue entries (bounded memory under long sessions of retire-heavy use).
    fn compact_queues(&mut self) {
        let (live, gen) = (&self.live, &self.generation);
        self.order.retain(|&(s, g)| live[s as usize] && gen[s as usize] == g);
        let prot = &self.protection;
        self.own.retain(|&(s, g)| live[s as usize] && gen[s as usize] == g && prot[s as usize] == Protection::OwnSplash);
    }

    /// Update one live instance (stale handles are ignored). Unit: `hm-vfx-runtime`.
    ///
    /// The pool keeps owning `custom.x` (age) and `custom.y` (lifetime); everything else is replaced.
    pub fn update(&mut self, handle: VfxHandle, instance: VfxInstance) {
        if !self.valid(handle) { return; }
        let s = handle.slot as usize;
        let (age, life) = (self.instances[s].custom[0], self.instances[s].custom[1]);
        let mut inst = instance;
        inst.custom[0] = age;
        inst.custom[1] = life;
        self.instances[s] = inst;
    }

    /// Retire one live instance now (a projectile that ended, a fire put out). Stale handles are ignored.
    pub fn retire(&mut self, handle: VfxHandle) {
        if self.valid(handle) { self.retire_slot(handle.slot as usize); }
    }

    /// Whether `handle` still names its instance.
    pub fn is_live(&self, handle: VfxHandle) -> bool { self.valid(handle) }

    /// Live instances.
    pub fn live_count(&self) -> usize { self.live.iter().filter(|l| **l).count() }

    /// Instances the last `write_buffer` needs drawn (everything after them is hidden): a value for the
    /// MultiMesh's `visible_instance_count`.
    pub fn visible_count(&self) -> usize { self.visible }

    /// Retire every instance older than `max_age_s` that is not protected at `now_s`: the degrade ladder's "trails
    /// trimmed to the head" (L2029) with `TRAIL_HEAD_S`. Returns how many retired.
    pub fn trim_older_than(&mut self, now_s: f64, max_age_s: f32) -> usize {
        if !now_s.is_finite() || max_age_s.is_nan() { return 0; }
        let mut n = 0;
        for s in 0..self.instances.len() {
            if self.live[s] && !self.pending[s] && now_s - self.born_s[s] > max_age_s as f64 && !self.protected_now(s, now_s) {
                self.retire_slot(s);
                n += 1;
            }
        }
        n
    }

    /// Age every instance to `now_s` (writes `custom.x`), retire expired ones as hidden. Unit: `hm-vfx-runtime`.
    ///
    /// Time running backwards (a replay seek) clamps ages at 0. An instance not yet drawn keeps age 0.
    pub fn tick(&mut self, now_s: f64) {
        if !now_s.is_finite() { return; }
        self.now_s = now_s;
        for s in 0..self.instances.len() {
            if !self.live[s] { continue; }
            if self.pending[s] {
                self.born_s[s] = now_s;
                self.instances[s].custom[0] = 0.0;
                continue;
            }
            let age = (now_s - self.born_s[s]).max(0.0);
            if age >= self.lifetime_s[s] as f64 {
                self.retire_slot(s);
            } else {
                self.instances[s].custom[0] = age as f32;
            }
        }
    }

    /// Write the whole buffer for `multimesh_set_buffer` (cap x 20 floats; fresh slots hidden this frame), sorting
    /// blended kinds back to front from `camera` (render-origin-relative). Unit: `hm-vfx-runtime`.
    ///
    /// The buffer holds `instances.len()` instances (the cap, or the grown size of a `Protected` pool). Opaque
    /// kinds keep slot order; blended kinds write their live instances farthest first, then hidden ones.
    /// An instance with any non-finite value is retired and written hidden.
    pub fn write_buffer(&mut self, camera: [f32; 3], out: &mut Vec<f32>) {
        let n = self.instances.len();
        out.clear();
        out.resize(n * super::INSTANCE_FLOATS, 0.0);
        let ok = |i: &VfxInstance| i.xform.iter().chain(i.color.iter()).chain(i.custom.iter()).all(|v| v.is_finite());
        self.visible = 0;
        if self.kind.blended() {
            self.scratch.clear();
            for s in 0..n {
                if self.live[s] && !ok(&self.instances[s]) { self.retire_slot(s); }
                if self.live[s] {
                    let x = &self.instances[s].xform;
                    let d = [x[3] - camera[0], x[7] - camera[1], x[11] - camera[2]];
                    let d2 = d[0] * d[0] + d[1] * d[1] + d[2] * d[2];
                    self.scratch.push((if d2.is_finite() { d2 } else { f32::MAX }, s as u32));
                }
            }
            self.scratch.sort_unstable_by(|a, b| b.0.total_cmp(&a.0).then(a.1.cmp(&b.1)));
            for (k, &(_, s)) in self.scratch.iter().enumerate() {
                let s = s as usize;
                self.instances[s].write(&mut out[k * 20..k * 20 + 20]);
                self.pending[s] = false;
            }
            self.visible = self.scratch.len();
            for s in 0..n { self.shown[s] = self.live[s]; }
        } else {
            for s in 0..n {
                if self.live[s] && !ok(&self.instances[s]) { self.retire_slot(s); }
                let draw = self.live[s] && !self.hide[s];
                if draw {
                    self.instances[s].write(&mut out[s * 20..s * 20 + 20]);
                    self.pending[s] = false;
                    self.visible = s + 1;
                }
                self.shown[s] = draw;
            }
        }
        for &s in &self.fresh { self.hide[s as usize] = false; }
        self.fresh.clear();
    }

    /// Shift every live instance by `delta` after a floating-origin rebase (L1831). Unit: `hm-vfx-runtime`.
    ///
    /// `delta` = old render origin - new render origin. A non-finite delta is ignored.
    pub fn rebase(&mut self, delta: [f32; 3]) {
        if !delta.iter().all(|v| v.is_finite()) { return; }
        for s in 0..self.instances.len() {
            if !self.live[s] { continue; }
            let x = &mut self.instances[s].xform;
            x[3] += delta[0];
            x[7] += delta[1];
            x[11] += delta[2];
        }
    }
    pub fn group(&self) -> CapGroup { self.kind.cap_group() }

    /// Resize the pool to another tier's cap (a quality change mid-sortie), keeping instances with their age,
    /// lifetime and protection. Returns `(old, new)` for every handle that moved slot; handles not listed
    /// either kept their slot or were retired (check with [`RingPool::is_live`]).
    ///
    /// Growing only adds free slots. Shrinking keeps at most the new cap: instances protected at `now_s`
    /// first, then the newest, and packs them into the low slots. The buffer length changes, so the client
    /// rebuilds the MultiMesh with `instances.len()` instances; every kept instance draws on the next frame
    /// (no hidden frame, since the old buffer is discarded). `Protected` pools never shrink.
    pub fn retier(&mut self, now_s: f64, tier: QualityTier) -> Vec<(VfxHandle, VfxHandle)> {
        let now = if now_s.is_finite() { now_s } else { self.now_s };
        let new_cap = self.kind.cap_group().cap(tier);
        if new_cap == usize::MAX || new_cap >= self.instances.len() {
            if new_cap != usize::MAX { let by = new_cap - self.instances.len(); self.grow(by); }
            self.cap = new_cap;
            return Vec::new();
        }
        // Rank live slots: protected first, then newest first, slot as the tie-break.
        let mut ranked: Vec<usize> = (0..self.instances.len()).filter(|&s| self.live[s]).collect();
        ranked.sort_by(|&a, &b| {
            self.protected_now(b, now).cmp(&self.protected_now(a, now))
                .then(self.born_s[b].total_cmp(&self.born_s[a]))
                .then(a.cmp(&b))
        });
        for &s in ranked.iter().skip(new_cap) { self.retire_slot(s); }
        ranked.truncate(new_cap);
        // Keep spawn order for the recycle queues.
        ranked.sort_by(|&a, &b| self.born_s[a].total_cmp(&self.born_s[b]).then(a.cmp(&b)));
        let fresh_pool = RingPool::new(self.kind, tier);
        let old = std::mem::replace(self, fresh_pool);
        let mut moved = Vec::new();
        for (dst, &src) in ranked.iter().enumerate() {
            self.free.remove(&(dst as u32));
            // A slot's generation only rises, so `old + 1` can match no earlier handle of slot `dst`.
            let g = if dst == src { old.generation[src] } else { old.generation[dst].wrapping_add(1) };
            self.instances[dst] = old.instances[src];
            self.born_s[dst] = old.born_s[src];
            self.lifetime_s[dst] = old.lifetime_s[src];
            self.protection[dst] = old.protection[src];
            self.generation[dst] = g;
            self.live[dst] = true;
            self.pending[dst] = old.pending[src];
            self.order.push_back((dst as u32, g));
            if old.protection[src] == Protection::OwnSplash { self.own.push_back((dst as u32, g)); }
            if dst != src {
                moved.push((VfxHandle { kind: self.kind, slot: src as u32, generation: old.generation[src] }, VfxHandle { kind: self.kind, slot: dst as u32, generation: g }));
            }
        }
        // Keep generations of untouched slots monotonic so no stale pre-retier handle can match a later spawn.
        for s in ranked.len()..self.instances.len() { self.generation[s] = old.generation[s]; }
        self.now_s = old.now_s;
        moved
    }
}

// ---------------------------------------------------------------------------------------------------------
// Instance builders.

/// Gravity the shaders apply to debris, m/s^2.
pub const GRAVITY: f32 = 9.81;
/// Hit and muzzle flash lifetime: two frames at 30 and three at 60 fps (L1955 "2 to 3 frames").
pub const FLASH_LIFETIME_S: f32 = 0.045;
/// Hit puffs ramp white to grey over 1.2 s (L1963).
pub const HIT_PUFF_LIFETIME_S: f32 = 1.2;
/// Most debris cubes per hit (L1963).
pub const MAX_DEBRIS: u32 = 64;
/// Debris cube edge, metres (L1963).
pub const DEBRIS_EDGE_M: f32 = 0.5;
/// Splash timing (L1962): rise 0.6 to 1.0 s, hold 0.4 s, collapse 2 to 3 s, foam ring 6 s.
pub const SPLASH_RISE_S: (f32, f32) = (0.6, 1.0);
pub const SPLASH_HOLD_S: f32 = 0.4;
pub const SPLASH_COLLAPSE_S: (f32, f32) = (2.0, 3.0);
pub const SPLASH_FOAM_S: f32 = 6.0;
/// Column height per mm of calibre and its cap (L1962).
pub const SPLASH_M_PER_MM: f32 = 0.25;
pub const SPLASH_MAX_M: f32 = 40.0;
/// Wake ribbon life and width range in beams (L1967); full width at this speed.
pub const WAKE_LIFETIME_S: f32 = 30.0;
pub const WAKE_BEAMS: (f32, f32) = (1.0, 2.5);
pub const WAKE_FULL_KN: f32 = 30.0;
/// Kelvin arms (L1967): half-angle, threshold speed, the emission interval [`kelvin_arm_instances`] assumes,
/// and their life.
pub const KELVIN_ANGLE_DEG: f32 = 19.47;
pub const KELVIN_MIN_KN: f32 = 8.0;
pub const KELVIN_EMIT_S: f32 = 1.0;
pub const KELVIN_LIFETIME_S: f32 = 10.0;
/// Bow wave: the sheet's spread across both sides at its aft end, in beams.
pub const BOW_WAVE_SPREAD_BEAMS: f32 = 0.9;
/// Tracer length is speed x this (L1961), and at least this many pixels wide.
pub const TRACER_LENGTH_S: f32 = 1.0 / 40.0;
pub const TRACER_MIN_PX: f32 = 1.5;
/// Missile trail diameter at emission, metres.
pub const TRAIL_DIAMETER_M: f32 = 2.4;
const KN_TO_MS: f32 = 0.514_444;
const UP: [f32; 3] = [0.0, 1.0, 0.0];

/// Splash column timeline from its lifetime (custom.y): `(rise_end_s, hold_end_s)`. The layout ties collapse to
/// rise (`collapse = 2 + 2.5 (rise - 0.6)`), so `lifetime = 0.9 + 3.5 rise` and the shader can recover both.
pub fn splash_column_phases(lifetime_s: f32) -> (f32, f32) {
    let rise = ((lifetime_s - 0.9) / 3.5).clamp(SPLASH_RISE_S.0, SPLASH_RISE_S.1);
    (rise, rise + SPLASH_HOLD_S)
}

fn hash(a: u32, b: u32) -> u32 {
    let mut h = a.wrapping_mul(0x9E37_79B9) ^ b.wrapping_add(0x7F4A_7C15).rotate_left(13);
    h ^= h >> 16;
    h = h.wrapping_mul(0x85EB_CA6B);
    h ^= h >> 13;
    h = h.wrapping_mul(0xC2B2_AE35);
    h ^ (h >> 16)
}
fn unit(h: u32) -> f32 { (h >> 8) as f32 / (1u32 << 24) as f32 }
/// Seeded uniform 0..1 for stream `k` of `seed`.
fn rnd(seed: u32, k: u32) -> f32 { unit(hash(seed, k)) }
fn lerp(a: f32, b: f32, t: f32) -> f32 { a + (b - a) * t }
fn fin(v: f32, or: f32) -> f32 { if v.is_finite() { v } else { or } }
fn fin3(p: [f32; 3]) -> [f32; 3] { [fin(p[0], 0.0), fin(p[1], 0.0), fin(p[2], 0.0)] }
fn add(a: [f32; 3], b: [f32; 3]) -> [f32; 3] { [a[0] + b[0], a[1] + b[1], a[2] + b[2]] }
fn sub(a: [f32; 3], b: [f32; 3]) -> [f32; 3] { [a[0] - b[0], a[1] - b[1], a[2] - b[2]] }
fn mul(a: [f32; 3], s: f32) -> [f32; 3] { [a[0] * s, a[1] * s, a[2] * s] }
fn dot(a: [f32; 3], b: [f32; 3]) -> f32 { a[0] * b[0] + a[1] * b[1] + a[2] * b[2] }
fn cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] { [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]] }
fn len(a: [f32; 3]) -> f32 { dot(a, a).sqrt() }
fn norm_or(a: [f32; 3], or: [f32; 3]) -> [f32; 3] { let l = len(a); if l > 1e-12 && l.is_finite() { mul(a, 1.0 / l) } else { or } }
/// Seed hash of a position (for builders without a seed argument).
fn pos_seed(p: [f32; 3]) -> u32 { hash(hash(p[0].to_bits(), p[1].to_bits()), p[2].to_bits()) }

/// Godot MultiMesh row-major 3x4 from basis columns (images of local X, Y, Z) and origin.
pub fn xform(x: [f32; 3], y: [f32; 3], z: [f32; 3], o: [f32; 3]) -> [f32; 12] {
    [x[0], y[0], z[0], o[0], x[1], y[1], z[1], o[1], x[2], y[2], z[2], o[2]]
}
/// Apply a row-major 3x4 to a local point.
pub fn apply_xform(m: &[f32; 12], p: [f32; 3]) -> [f32; 3] {
    [m[0] * p[0] + m[1] * p[1] + m[2] * p[2] + m[3],
     m[4] * p[0] + m[5] * p[1] + m[6] * p[2] + m[7],
     m[8] * p[0] + m[9] * p[1] + m[10] * p[2] + m[11]]
}
/// Right-handed orthonormal frame with `axis` as its second (Y) vector, rolled by `roll` about it.
fn frame_y(axis: [f32; 3], roll: f32) -> ([f32; 3], [f32; 3], [f32; 3]) {
    let y = norm_or(axis, UP);
    let r = if y[1].abs() > 0.9 { [1.0, 0.0, 0.0] } else { UP };
    let x0 = norm_or(cross(y, r), [1.0, 0.0, 0.0]);
    let z0 = cross(x0, y);
    let (c, s) = (libm::cosf(roll), libm::sinf(roll));
    let x = add(mul(x0, c), mul(z0, s));
    let z = cross(x, y);
    (x, y, z)
}
/// Frame for a segment along `d` (X, unnormalised, kept as is) with unit Y and Z perpendicular to it, Y as
/// close to world up as possible, right-handed (X x Y = Z direction).
fn frame_x(d: [f32; 3]) -> ([f32; 3], [f32; 3]) {
    let dir = norm_or(d, [1.0, 0.0, 0.0]);
    let r = if dir[1].abs() > 0.99 { [1.0, 0.0, 0.0] } else { UP };
    let z = norm_or(cross(dir, r), [0.0, 0.0, 1.0]);
    let y = cross(z, dir);
    (y, z)
}

/// Splash layout for one shell landing (L1962): 3 to 5 hex columns of height 0.25 m per mm of calibre capped at
/// 40 m, rise 0.6 to 1.0 s, hold 0.4 s, collapse 2 to 3 s, foam ring 6 s. Columns and timings vary by `seed`.
#[derive(Clone, Debug, PartialEq)]
pub struct SplashLayout { pub columns: Vec<VfxInstance>, pub foam: VfxInstance, pub lifetime_s: f32 }
/// Unit: `hm-vfx-runtime`.
///
/// Each column carries its own lifetime in `custom.y` (spawn it with that value); `lifetime_s` is the longest
/// of all, the foam ring's 6 s. Bigger shells rise and collapse slower. Column 0 is the central, tallest one
/// (a degrade step drawing distant splashes as one prism keeps only it, L2029).
pub fn splash_layout(pos: [f32; 3], calibre_mm: u16, seed: u32) -> SplashLayout {
    let pos = fin3(pos);
    let h = (calibre_mm as f32 * SPLASH_M_PER_MM).clamp(SPLASH_M_PER_MM, SPLASH_MAX_M);
    let cf = h / SPLASH_MAX_M;
    let r_main = (0.12 * h).max(0.3);
    let count = 3 + (hash(seed, 0) % 3) as usize;
    let seed01 = rnd(seed, 1);
    let mut columns = Vec::with_capacity(count);
    for i in 0..count {
        let k = 16 * (i as u32 + 1);
        let (height, radius, offset) = if i == 0 {
            (h * lerp(0.9, 1.0, rnd(seed, k)), r_main, [0.0, 0.0])
        } else {
            let a = core::f32::consts::TAU * (i as f32 - 1.0 + 0.35 * rnd(seed, k + 1)) / (count - 1) as f32;
            let d = r_main * lerp(0.7, 1.3, rnd(seed, k + 2));
            (h * lerp(0.45, 0.8, rnd(seed, k)), r_main * lerp(0.55, 0.8, rnd(seed, k + 3)), [d * libm::cosf(a), d * libm::sinf(a)])
        };
        let t = (0.7 * cf + 0.3 * rnd(seed, k + 4)).clamp(0.0, 1.0);
        let mut rise = lerp(SPLASH_RISE_S.0, SPLASH_RISE_S.1, t);
        if i > 0 { rise = (rise * 0.9).max(SPLASH_RISE_S.0); }
        let collapse = SPLASH_COLLAPSE_S.0 + 2.5 * (rise - SPLASH_RISE_S.0);
        let life = rise + SPLASH_HOLD_S + collapse;
        // Spin about Y and a lean of at most 4 degrees.
        let spin = core::f32::consts::TAU * rnd(seed, k + 5);
        let lean = 0.07 * rnd(seed, k + 6);
        let lean_dir = core::f32::consts::TAU * rnd(seed, k + 7);
        let axis = [libm::sinf(lean) * libm::cosf(lean_dir), libm::cosf(lean), libm::sinf(lean) * libm::sinf(lean_dir)];
        let (bx, by, bz) = frame_y(axis, spin);
        let base = [pos[0] + offset[0], pos[1] - 0.5, pos[2] + offset[1]];
        columns.push(VfxInstance {
            xform: xform(mul(bx, 2.0 * radius), mul(by, height + 0.5), mul(bz, 2.0 * radius), base),
            color: [1.0; 4],
            custom: [0.0, life, rnd(seed, k + 8), cf],
        });
    }
    let (fx, fy, fz) = frame_y(UP, core::f32::consts::TAU * rnd(seed, 2));
    let foam_d = 2.0 * 2.5 * r_main;
    let foam = VfxInstance { xform: xform(mul(fx, foam_d), fy, mul(fz, foam_d), pos), color: [1.0; 4], custom: [0.0, SPLASH_FOAM_S, seed01, cf] };
    let longest = columns.iter().map(|c| c.custom[1]).fold(SPLASH_FOAM_S, f32::max);
    SplashLayout { columns, foam, lifetime_s: longest }
}

/// Hit effect: one flash, 3 to 7 puffs and up to `debris_cells.min(64)` debris cubes with seeded velocities
/// (L1963). Returns (kind, instance, lifetime) triples. Unit: `hm-vfx-runtime`.
///
/// Order: the flash, then the puffs, then the debris, so a degrade step can truncate debris from the end
/// (L2028: halved, then 16 per event). Debris lifetimes run until the cube falls back to sea level (y = 0 in
/// render space). A zero or non-finite `normal` is taken as up.
pub fn hit_layout(pos: [f32; 3], normal: [f32; 3], debris_cells: u32, seed: u32) -> Vec<(VfxKind, VfxInstance, f32)> {
    let pos = fin3(pos);
    let n = norm_or(fin3(normal), UP);
    let cells = debris_cells.min(MAX_DEBRIS);
    let puffs = (3 + cells / 16 + (hash(seed, 0) & 1)).min(7) as usize;
    let mut out = Vec::with_capacity(1 + puffs + cells as usize);
    // Flash: size grows with how much of the ship came apart.
    let size = 2.0 + 0.05 * cells as f32;
    let (fx, fy, fz) = frame_y(n, core::f32::consts::TAU * rnd(seed, 1));
    out.push((VfxKind::HitFlash, VfxInstance {
        xform: xform(mul(fx, size), mul(fy, size), mul(fz, size), add(pos, mul(n, 0.3))),
        color: [1.0; 4],
        custom: [0.0, FLASH_LIFETIME_S, rnd(seed, 2), 1.0],
    }, FLASH_LIFETIME_S));
    for i in 0..puffs {
        let k = 32 + 8 * i as u32;
        let (tx, _, tz) = frame_y(n, core::f32::consts::TAU * rnd(seed, k));
        let along = lerp(0.4, 1.8, rnd(seed, k + 1));
        let side = lerp(-1.0, 1.0, rnd(seed, k + 2));
        let p = add(add(pos, mul(n, along)), add(mul(tx, side), mul(tz, 0.5 * side)));
        let d = 2.0 * lerp(0.6, 1.4, rnd(seed, k + 3));
        let (bx, by, bz) = frame_y(n, core::f32::consts::TAU * rnd(seed, k + 4));
        out.push((VfxKind::HitPuff, VfxInstance {
            xform: xform(mul(bx, d), mul(by, d), mul(bz, d), p),
            color: [1.0; 4],
            custom: [0.0, HIT_PUFF_LIFETIME_S, rnd(seed, k + 5), lerp(1.5, 3.0, rnd(seed, k + 6))],
        }, HIT_PUFF_LIFETIME_S));
    }
    let height = pos[1].max(0.0) + DEBRIS_EDGE_M * 0.5;
    for i in 0..cells {
        let k = 128 + 8 * i;
        let jitter = [rnd(seed, k) * 2.0 - 1.0, rnd(seed, k + 1) * 2.0 - 1.0, rnd(seed, k + 2) * 2.0 - 1.0];
        let dir = norm_or(add(add(mul(n, lerp(0.6, 1.0, rnd(seed, k + 3))), mul(jitter, 0.7)), mul(UP, 0.4)), UP);
        let speed = lerp(6.0, 22.0, rnd(seed, k + 4));
        let vy = dir[1] * speed;
        let fall = (vy + (vy * vy + 2.0 * GRAVITY * height).sqrt()) / GRAVITY;
        let life = fall.clamp(0.6, 5.0);
        let (bx, by, bz) = frame_y(dir, core::f32::consts::TAU * rnd(seed, k + 5));
        out.push((VfxKind::Debris, VfxInstance {
            xform: xform(mul(bx, DEBRIS_EDGE_M), mul(by, DEBRIS_EDGE_M), mul(bz, DEBRIS_EDGE_M), pos),
            color: [1.0; 4],
            custom: [0.0, life, rnd(seed, k + 6), speed],
        }, life));
    }
    out
}

/// Wake width multiplier of the beam and the 0..1 speed factor.
fn wake_factor(speed_kn: f32) -> f32 { (fin(speed_kn, 0.0) / WAKE_FULL_KN).clamp(0.0, 1.0) }

/// Wake ribbon segment between two stern positions: width 1.0 to 2.5 beams by speed, 30 s life (L1967). Kelvin
/// arms at 19.47 degrees above 8 kn are separate instances from [`kelvin_arm_instances`]. Unit: `hm-vfx-runtime`.
///
/// Mesh x = 0 maps to `a`, x = 1 to `b`. Width reaches 2.5 beams at 30 kn.
pub fn wake_segment(a: [f32; 3], b: [f32; 3], beam_m: f32, speed_kn: f32) -> VfxInstance {
    let (a, b) = (fin3(a), fin3(b));
    let sf = wake_factor(speed_kn);
    let width = fin(beam_m, 0.0).abs().max(0.5) * lerp(WAKE_BEAMS.0, WAKE_BEAMS.1, sf);
    let x = sub(b, a);
    // Z = X x up keeps the frame right-handed so the up-facing ribbon is not mirrored.
    let z = norm_or(cross(x, UP), [0.0, 0.0, 1.0]);
    VfxInstance { xform: xform(x, UP, mul(z, width), a), color: [1.0; 4], custom: [0.0, WAKE_LIFETIME_S, rnd(pos_seed(a), 0), sf] }
}

/// Unit: `hm-vfx-runtime`.
///
/// Call once per [`KELVIN_EMIT_S`] while the ship makes more than [`KELVIN_MIN_KN`]; returns nothing slower.
/// Heading is compass radians (0 = north = -Z, clockwise, as `sim_core::math`). Each call emits one segment
/// per arm, from the stern back along the arm by one emission interval; each segment then drifts outward at
/// `v tan(19.47 deg)` (custom.w), so successive segments chain into the arms' V moving with the ship.
pub fn kelvin_arm_instances(stern: [f32; 3], heading_rad: f32, speed_kn: f32) -> Vec<VfxInstance> {
    let speed_kn = fin(speed_kn, 0.0);
    if speed_kn <= KELVIN_MIN_KN { return Vec::new(); }
    let stern = fin3(stern);
    let h = fin(heading_rad, 0.0);
    let v = speed_kn * KN_TO_MS;
    let fwd = [libm::sinf(h), 0.0, -libm::cosf(h)];
    let right = [libm::cosf(h), 0.0, libm::sinf(h)];
    let tan = libm::tanf(KELVIN_ANGLE_DEG.to_radians());
    let step = v * KELVIN_EMIT_S;
    let width = (0.25 * v).max(2.0);
    let height = (0.2 * v).clamp(1.0, 4.0);
    let seed = pos_seed(stern);
    [1.0f32, -1.0].iter().enumerate().map(|(k, &side)| {
        let out_dir = mul(right, side);
        let x = add(mul(fwd, -step), mul(out_dir, step * tan));
        // Pick the Z sign that keeps the frame right-handed; the drift sign follows so it is always outward.
        let mut z = right;
        if dot(x, cross(UP, z)) < 0.0 { z = mul(z, -1.0); }
        let drift = v * tan * dot(z, out_dir).signum();
        VfxInstance { xform: xform(x, mul(UP, height), mul(z, width), stern), color: [1.0; 4], custom: [0.0, KELVIN_LIFETIME_S, rnd(seed, k as u32), drift] }
    }).collect()
}

/// Bow wave sheet at the stem, scaled by speed (L1967): length and height grow with speed; zero scale when
/// stopped. Attach it to the ship (spawn with `f32::INFINITY` and `update` it each frame). Heading as in
/// [`kelvin_arm_instances`].
///
/// The sheet hugs the entry: it spreads to [`BOW_WAVE_SPREAD_BEAMS`] of the beam at its aft end, so on a fine
/// bow it stands a metre or two off the plating and peels away from it, never out across open water.
pub fn bow_wave_instance(bow: [f32; 3], heading_rad: f32, beam_m: f32, speed_kn: f32) -> VfxInstance {
    let bow = fin3(bow);
    let kn = fin(speed_kn, 0.0).max(0.0);
    let h = fin(heading_rad, 0.0);
    let beam = fin(beam_m, 0.0).abs().max(1.0);
    let fwd = [libm::sinf(h), 0.0, -libm::cosf(h)];
    let right = [libm::cosf(h), 0.0, libm::sinf(h)];
    let on = if kn > 0.5 { 1.0 } else { 0.0 };
    let length = on * beam * (0.45 + 0.04 * kn).min(2.0);
    let width = on * beam * BOW_WAVE_SPREAD_BEAMS;
    let height = on * (0.2 + 0.07 * kn).min(3.5);
    VfxInstance { xform: xform(mul(fwd, length), mul(UP, height), mul(right, width), bow), color: [1.0; 4], custom: [0.0, 0.0, rnd(pos_seed(bow), 0), wake_factor(kn)] }
}

/// Tracer box for a projectile: speed x 1/40 s long, at least 1.5 px wide at `distance_m` given `px_per_m_at_1m`
/// (L1961). Unit: `hm-vfx-runtime`.
///
/// The box's front face sits on `pos` and it trails back along the velocity. Its natural width is twice the
/// calibre; `px_per_m_at_1m` is the viewport's pixels per metre at 1 m (`viewport_height / (2 tan(fov_y / 2))`).
/// Emission (custom.w) runs 4 to 8 with calibre (L1955).
pub fn tracer_instance(pos: [f32; 3], vel: [f32; 3], calibre_mm: u16, distance_m: f32, px_per_m_at_1m: f32) -> VfxInstance {
    let pos = fin3(pos);
    let vel = fin3(vel);
    let speed = len(vel);
    let dist = fin(distance_m, 0.0).max(0.0);
    let px = fin(px_per_m_at_1m, 0.0);
    let min_w = if px > 0.0 { TRACER_MIN_PX * dist / px } else { 0.0 };
    let width = (2.0 * calibre_mm as f32 / 1000.0).max(min_w).max(0.01);
    let length = (speed * TRACER_LENGTH_S).max(width);
    let dir = norm_or(vel, [1.0, 0.0, 0.0]);
    let (y, z) = frame_x(dir);
    let centre = sub(pos, mul(dir, 0.5 * length));
    let t = ((calibre_mm as f32 - 20.0) / 180.0).clamp(0.0, 1.0);
    VfxInstance { xform: xform(mul(dir, length), mul(y, width), mul(z, width), centre), color: [1.0; 4], custom: [0.0, 0.0, 0.5, lerp(4.0, 8.0, t)] }
}

/// Missile trail cross-quad emitted every 0.1 s; trail length 12 s (6 s on Low); the 2 s head is never trimmed
/// (L1966). Unit: `hm-vfx-runtime`.
///
/// Mesh x = 0 maps to `a` (the older point), x = 1 to `b`. Spawn with `trail_length_s(tier)`; give incoming
/// missiles' segments `Protection::IncomingMissileHead`, which protects each segment for its first
/// `TRAIL_HEAD_S`.
pub fn trail_segment(a: [f32; 3], b: [f32; 3]) -> VfxInstance {
    let (a, b) = (fin3(a), fin3(b));
    let x = sub(b, a);
    let (y, z) = frame_x(x);
    VfxInstance { xform: xform(x, mul(y, TRAIL_DIAMETER_M), mul(z, TRAIL_DIAMETER_M), a), color: [1.0; 4], custom: [0.0, 0.0, rnd(pos_seed(a), 0), 0.0] }
}
pub const TRAIL_EMIT_S: f32 = 0.1;
pub fn trail_length_s(tier: QualityTier) -> f32 { if matches!(tier, QualityTier::Low | QualityTier::Fallback) { 6.0 } else { 12.0 } }
pub const TRAIL_HEAD_S: f32 = 2.0;

/// Global flash limiter (accessibility floor 6, L1957): call once per flash candidate with its screen fraction;
/// returns the emission multiplier (1.0, or 0.25 with no light for extras inside the same third of a second
/// beyond 3 per second) and whether it may cast a light.
///
/// A flash blooming over 2.5% of the screen is admitted at full strength only when no full flash was admitted in
/// the same third of a second (fixed thirds, `floor(3 t)`) and fewer than 3 in the last second; extras draw at
/// 25% with no light and do not count. Smaller flashes are never limited. With `reduce_flashing` every flash is
/// further scaled by `4 / 16` so the brightest flash (16) emits at most the setting's cap of 4 (L1957).
/// `recent` holds the admission times of full flashes within the last second.
#[derive(Clone, Debug, Default)]
pub struct FlashLimiter { pub recent: Vec<f64>, pub reduce_flashing: bool }
impl FlashLimiter {
    /// Unit: `hm-vfx-runtime`.
    pub fn admit(&mut self, now_s: f64, screen_fraction: f32) -> (f32, bool) {
        let cap = if self.reduce_flashing { super::emission::REDUCE_FLASHING_CAP / super::emission::FLASH.1 } else { 1.0 };
        let dim = (super::emission::EXTRA_FLASH_EMISSION * cap, false);
        if !now_s.is_finite() { return dim; }
        // Prune the window; entries after `now` belong to a timeline that was rewound.
        self.recent.retain(|&t| t > now_s - 1.0 + 1e-6 && t <= now_s);
        if screen_fraction <= super::emission::FLASH_SCREEN_FRACTION { return (cap, true); }
        let third = libm::floor(now_s * 3.0);
        let same_third = self.recent.iter().any(|&t| libm::floor(t * 3.0) == third);
        if !same_third && self.recent.len() < super::emission::FLASHES_PER_S as usize {
            self.recent.push(now_s);
            (cap, true)
        } else {
            dim
        }
    }
    /// Emission after the Reduce flashing cap (L1957): `emission` itself, or at most 4 with the setting on.
    pub fn cap_emission(&self, emission: f32) -> f32 {
        if self.reduce_flashing { emission.min(super::emission::REDUCE_FLASHING_CAP) } else { emission }
    }
}

/// Screen fraction a flash of `radius_m` covers at `distance_m`, for [`FlashLimiter::admit`]: the projected
/// disc's area over the viewport's. `px_per_m_at_1m` as in [`tracer_instance`]; a camera inside the flash
/// counts the whole screen. Bad input gives 0.
pub fn flash_screen_fraction(radius_m: f32, distance_m: f32, px_per_m_at_1m: f32, viewport_px: [f32; 2]) -> f32 {
    let (r, d, k) = (fin(radius_m, 0.0).abs(), fin(distance_m, 0.0).abs(), fin(px_per_m_at_1m, 0.0));
    let area = fin(viewport_px[0], 0.0) * fin(viewport_px[1], 0.0);
    if r <= 0.0 || k <= 0.0 || area <= 0.0 { return 0.0; }
    if d <= r { return 1.0; }
    let rp = r * k / d;
    (core::f32::consts::PI * rp * rp / area).clamp(0.0, 1.0)
}

/// Seed for one emission of a continuous effect: `a` names the emitter (fire, mount, decoy), `b` counts its
/// emissions. Deterministic, so replays draw the same puffs.
pub fn effect_seed(a: u32, b: u32) -> u32 { hash(a, b) }

/// How many emissions a steady emitter of `rate_hz` makes in `(t0_s, t1_s]`, with phase `phase01` (0..1, from
/// the emitter's seed) so emitters do not pulse in step. Stateless: the client calls it with last frame's and
/// this frame's time. At most [`MAX_EMISSIONS_PER_CALL`] (a long frame or a seek must not flood the pool); 0
/// when time did not advance or any input is bad.
pub fn emissions_between(t0_s: f64, t1_s: f64, rate_hz: f32, phase01: f32) -> u32 {
    if !(t0_s.is_finite() && t1_s.is_finite() && rate_hz.is_finite() && rate_hz > 0.0) || t1_s <= t0_s { return 0; }
    let ph = fin(phase01, 0.0).rem_euclid(1.0) as f64;
    let n = libm::floor(t1_s * rate_hz as f64 + ph) - libm::floor(t0_s * rate_hz as f64 + ph);
    n.clamp(0.0, MAX_EMISSIONS_PER_CALL as f64) as u32
}
/// Cap on [`emissions_between`] per call.
pub const MAX_EMISSIONS_PER_CALL: u32 = 8;

// ---------------------------------------------------------------------------------------------------------
// Fire, fireballs, muzzle, flak, decoys, defence smoke and flooding (L1964-1969, L599-604).

/// Fire smoke per burning compartment, puffs per second (L1964), from fire intensity 0 to 1.
pub const FIRE_SMOKE_HZ: (f32, f32) = (2.0, 4.0);
/// Fire smoke puff life, seconds; puffs rise at [`SMOKE_RISE_MS`] plus the wind.
pub const FIRE_SMOKE_LIFETIME_S: f32 = 9.0;
pub const SMOKE_RISE_MS: f32 = 2.5;
/// Flame tongues per second per burning compartment, from intensity 0 to 1. Each tongue peaks early and
/// scales to 0 over its own life (L1953), so a fire is a steady stream of tongues rather than one looping mesh.
pub const FLAME_HZ: (f32, f32) = (3.0, 6.0);
pub const FLAME_LIFETIME_S: (f32, f32) = (0.7, 1.3);
/// Fireball life range, seconds (grows with radius).
pub const FIREBALL_LIFETIME_S: (f32, f32) = (0.8, 3.0);
/// Muzzle light (L1968): unshadowed, range at most 30 m, at most 0.15 s.
pub const MUZZLE_LIGHT_MAX_M: f32 = 30.0;
pub const MUZZLE_LIGHT_MAX_S: f32 = 0.15;
/// Muzzle haze and muzzle smoke life, seconds.
pub const MUZZLE_HAZE_LIFETIME_S: f32 = 1.6;
pub const MUZZLE_SMOKE_LIFETIME_S: f32 = 5.0;
/// Chaff cloud diameter at full bloom, metres (the sim's cloud is a radar cross-section with no size).
pub const CHAFF_DIAMETER_M: f32 = 40.0;
/// Flare burn diameter, metres.
pub const FLARE_DIAMETER_M: f32 = 2.0;
/// Opaque puffs drawing one defence smoke cylinder (L1965).
pub const DEFENCE_SMOKE_PUFFS: u32 = 24;

/// One VFX light for the client's pooled OmniLights (counted against `vfx_light_budget`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct VfxLight { pub pos: [f32; 3], pub range_m: f32, pub duration_s: f32 }

fn smoothstep(e0: f32, e1: f32, x: f32) -> f32 {
    let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}
/// Horizontal part of a vector.
fn flat(v: [f32; 3]) -> [f32; 3] { [v[0], 0.0, v[2]] }
/// Uniform-scale puff of diameter `d` drifting along `drift` (local +Y = its direction, custom.w = its speed).
fn drifting_puff(pos: [f32; 3], drift: [f32; 3], d: f32, grey: f32, life: f32, seed: u32) -> VfxInstance {
    let speed = len(drift);
    let (bx, by, bz) = frame_y(norm_or(drift, UP), core::f32::consts::TAU * rnd(seed, 101));
    VfxInstance { xform: xform(mul(bx, d), mul(by, d), mul(bz, d), pos), color: [grey, grey, grey, 1.0], custom: [0.0, life, rnd(seed, 102), fin(speed, 0.0)] }
}

/// Fire smoke rate for a burning compartment of `intensity` 0..1 (L1964: 2 to 4 puffs per second).
pub fn fire_smoke_hz(intensity: f32) -> f32 { lerp(FIRE_SMOKE_HZ.0, FIRE_SMOKE_HZ.1, fin(intensity, 0.0).clamp(0.0, 1.0)) }
/// Flame tongue rate for a burning compartment of `intensity` 0..1.
pub fn flame_hz(intensity: f32) -> f32 { lerp(FLAME_HZ.0, FLAME_HZ.1, fin(intensity, 0.0).clamp(0.0, 1.0)) }

/// One flame tongue over a burning compartment (L1964): an emissive `FlamePrism` standing on `base` (the
/// burning deck or the breach, render-origin-relative), `width_m` across its base, leaning downwind.
/// Returns the instance and its lifetime; spawn it with [`Protection::Fire`] (fire presence never degrades,
/// L2033) at [`flame_hz`] using [`emissions_between`] and a fresh [`effect_seed`] per tongue.
///
/// Flames stand on world up whatever the ship's heel. `custom.w` is the emission, 2 to 4 by intensity (L1955).
pub fn flame_tongue(base: [f32; 3], width_m: f32, intensity: f32, wind: [f32; 3], seed: u32) -> (VfxInstance, f32) {
    let base = fin3(base);
    let i = fin(intensity, 0.0).clamp(0.0, 1.0);
    let w = fin(width_m, 0.0).abs().clamp(0.3, 20.0) * lerp(0.7, 1.1, rnd(seed, 1));
    let h = w * lerp(1.2, 2.0, i) * lerp(0.8, 1.2, rnd(seed, 2));
    let wind = flat(fin3(wind));
    let lean = (len(wind) / 15.0).clamp(0.0, 0.6);
    // The mesh leans toward local +X: point it downwind (or a seeded direction in still air).
    let a = core::f32::consts::TAU * rnd(seed, 3);
    let xdir = norm_or(wind, [libm::cosf(a), 0.0, libm::sinf(a)]);
    let zdir = cross(xdir, UP);
    let jitter = mul(add(mul(xdir, rnd(seed, 4) - 0.5), mul(zdir, rnd(seed, 5) - 0.5)), 0.5 * w);
    // Shear the column downwind: volume (and the determinant's sign) is kept.
    let y = add(mul(UP, h), mul(xdir, lean * h));
    let s = w / 0.9;
    let life = lerp(FLAME_LIFETIME_S.0, FLAME_LIFETIME_S.1, rnd(seed, 6));
    let e = lerp(super::emission::FIRE.0, super::emission::FIRE.1, i);
    (VfxInstance { xform: xform(mul(xdir, s), y, mul(zdir, s), add(base, jitter)), color: [1.0; 4], custom: [0.0, life, rnd(seed, 7), e] }, life)
}

/// One fire smoke puff (L1964): opaque faceted `SmokePuff` leaving `top` (the top of the flames), rising and
/// drifting with `wind` (m/s, render frame). Returns the instance and its lifetime; spawn at
/// [`fire_smoke_hz`]. Smoke is not fire presence, so it is unprotected and may be halved beyond 3 km by the
/// degrade ladder (L2028). `custom.w` = drift speed m/s along normalised local +Y.
pub fn fire_smoke_puff(top: [f32; 3], intensity: f32, wind: [f32; 3], seed: u32) -> (VfxInstance, f32) {
    let i = fin(intensity, 0.0).clamp(0.0, 1.0);
    let d = lerp(3.0, 6.0, i) * lerp(0.8, 1.25, rnd(seed, 1));
    let drift = add(mul(UP, SMOKE_RISE_MS * lerp(0.7, 1.3, rnd(seed, 2))), flat(fin3(wind)));
    let p = add(fin3(top), [(rnd(seed, 3) - 0.5) * d * 0.5, 0.0, (rnd(seed, 4) - 0.5) * d * 0.5]);
    let life = FIRE_SMOKE_LIFETIME_S * lerp(0.85, 1.15, rnd(seed, 5));
    (drifting_puff(p, drift, d, lerp(0.5, 0.35, i), life, seed), life)
}

/// Explosion of `radius_m` (a cook-off, a warhead, a magazine): a flash, 1 to 3 fireballs and 2 to 4 dark
/// smoke puffs. Returns (kind, instance, lifetime) triples, flash first. Fireballs carry emission 2 to 4 in
/// `custom.w` (L1955); give the flash's light to the flash limiter like a hit flash. Add `hit_layout` for
/// debris when cells were lost.
pub fn fireball_layout(pos: [f32; 3], radius_m: f32, wind: [f32; 3], seed: u32) -> Vec<(VfxKind, VfxInstance, f32)> {
    let pos = fin3(pos);
    let r = fin(radius_m, 0.0).abs().clamp(0.5, 40.0);
    let mut out = Vec::with_capacity(8);
    let (fx, fy, fz) = frame_y(UP, core::f32::consts::TAU * rnd(seed, 1));
    let fs = 2.2 * r;
    out.push((VfxKind::HitFlash, VfxInstance { xform: xform(mul(fx, fs), mul(fy, fs), mul(fz, fs), pos), color: [1.0; 4], custom: [0.0, FLASH_LIFETIME_S, rnd(seed, 2), 1.0] }, FLASH_LIFETIME_S));
    let balls = 1 + (hash(seed, 3) % 3) as usize;
    let life = (FIREBALL_LIFETIME_S.0 + 0.08 * r).min(FIREBALL_LIFETIME_S.1);
    for k in 0..balls {
        let ks = 16 + 8 * k as u32;
        let d = 2.0 * r * lerp(0.75, 1.0, rnd(seed, ks)) * if k == 0 { 1.0 } else { 0.7 };
        let off = if k == 0 { [0.0; 3] } else { [(rnd(seed, ks + 1) - 0.5) * r, rnd(seed, ks + 2) * 0.6 * r, (rnd(seed, ks + 3) - 0.5) * r] };
        let (bx, by, bz) = frame_y(UP, core::f32::consts::TAU * rnd(seed, ks + 4));
        let l = life * lerp(0.85, 1.0, rnd(seed, ks + 5));
        let e = lerp(super::emission::FIRE.0, super::emission::FIRE.1, rnd(seed, ks + 6));
        out.push((VfxKind::Fireball, VfxInstance { xform: xform(mul(bx, d), mul(by, d), mul(bz, d), add(pos, off)), color: [1.0; 4], custom: [0.0, l, rnd(seed, ks + 7), e] }, l));
    }
    let puffs = 2 + (hash(seed, 4) % 3) as usize;
    for k in 0..puffs {
        let ks = 64 + 8 * k as u32;
        let p = add(pos, [(rnd(seed, ks) - 0.5) * 1.5 * r, r * lerp(0.4, 1.2, rnd(seed, ks + 1)), (rnd(seed, ks + 2) - 0.5) * 1.5 * r]);
        let drift = add(mul(UP, SMOKE_RISE_MS * lerp(1.0, 1.8, rnd(seed, ks + 3))), flat(fin3(wind)));
        let l = 10.0 * lerp(0.8, 1.2, rnd(seed, ks + 4));
        out.push((VfxKind::SmokePuff, drifting_puff(p, drift, 2.0 * r * lerp(0.8, 1.2, rnd(seed, ks + 5)), 0.3, l, hash(seed, ks + 6)), l));
    }
    out
}

/// Muzzle effect for one round leaving `muzzle` along `dir` (L1968): an emissive `MuzzleStar` for 2 to 3
/// frames, a blended `MuzzleHaze`, and 1 to 3 light smoke puffs (more for bigger guns). Returns (kind,
/// instance, lifetime) triples, star first. The star carries emission scale 1 in `custom.w` (multiply in the
/// flash limiter's result, as for hit flashes); haze and smoke carry drift speed along normalised local +Y.
/// Pair it with [`muzzle_light`].
pub fn muzzle_layout(muzzle: [f32; 3], dir: [f32; 3], calibre_mm: u16, wind: [f32; 3], seed: u32) -> Vec<(VfxKind, VfxInstance, f32)> {
    let p = fin3(muzzle);
    let dir = norm_or(fin3(dir), [1.0, 0.0, 0.0]);
    let cal = calibre_mm.clamp(5, 510) as f32;
    let wind = flat(fin3(wind));
    let length = 1.0 + 0.04 * cal;
    let width = 0.5 * length;
    let (y, z) = frame_x(dir);
    let mut out = Vec::with_capacity(5);
    out.push((VfxKind::MuzzleStar, VfxInstance { xform: xform(mul(dir, length), mul(y, width), mul(z, width), p), color: [1.0; 4], custom: [0.0, FLASH_LIFETIME_S, rnd(seed, 1), 1.0] }, FLASH_LIFETIME_S));
    let hd = 1.0 + 0.05 * cal;
    let haze_drift = add(add(mul(dir, 2.0), mul(UP, 0.6)), wind);
    out.push((VfxKind::MuzzleHaze, drifting_puff(add(p, mul(dir, 0.6 * length)), haze_drift, hd, 1.0, MUZZLE_HAZE_LIFETIME_S, hash(seed, 2)), MUZZLE_HAZE_LIFETIME_S));
    let puffs = 1 + (cal >= 100.0) as usize + (cal >= 250.0) as usize;
    for k in 0..puffs {
        let ks = 16 + 8 * k as u32;
        let along = length * lerp(0.5, 1.5, (k as f32 + rnd(seed, ks)) / puffs as f32);
        let d = (0.5 + 0.022 * cal) * lerp(0.8, 1.2, rnd(seed, ks + 1));
        let drift = add(add(mul(dir, lerp(1.0, 4.0, rnd(seed, ks + 2))), mul(UP, 0.8)), wind);
        let l = MUZZLE_SMOKE_LIFETIME_S * lerp(0.8, 1.2, rnd(seed, ks + 3));
        out.push((VfxKind::SmokePuff, drifting_puff(add(p, mul(dir, along)), drift, d, 0.85, l, hash(seed, ks + 4)), l));
    }
    out
}

/// Muzzle light (L1968): unshadowed, at most 30 m range and 0.15 s, growing with calibre, a little ahead of
/// the muzzle. Ask [`FlashLimiter::admit`] first; extras get no light.
pub fn muzzle_light(muzzle: [f32; 3], dir: [f32; 3], calibre_mm: u16) -> VfxLight {
    let dir = norm_or(fin3(dir), [1.0, 0.0, 0.0]);
    let cal = calibre_mm.clamp(5, 510) as f32;
    VfxLight {
        pos: add(fin3(muzzle), mul(dir, 0.3 * (1.0 + 0.04 * cal))),
        range_m: (6.0 + 0.06 * cal).min(MUZZLE_LIGHT_MAX_M),
        duration_s: (0.06 + 0.0002 * cal).min(MUZZLE_LIGHT_MAX_S),
    }
}

/// Flak burst of a shell of `calibre_mm` bursting at `pos` in the air: one dark faceted `FlakBurst` that
/// hangs, sized and lived by calibre. Returns the instance and its lifetime. `custom.w` = 0 (Layer B adds
/// the sparks).
pub fn flak_burst(pos: [f32; 3], calibre_mm: u16, seed: u32) -> (VfxInstance, f32) {
    let cal = calibre_mm.clamp(5, 510) as f32;
    let d = (2.0 + 0.06 * cal).min(14.0) * lerp(0.85, 1.15, rnd(seed, 1));
    let life = (2.5 + 0.01 * cal).min(5.0) * lerp(0.9, 1.1, rnd(seed, 2));
    let axis = norm_or([rnd(seed, 3) - 0.5, 1.0, rnd(seed, 4) - 0.5], UP);
    let (bx, by, bz) = frame_y(axis, core::f32::consts::TAU * rnd(seed, 5));
    (VfxInstance { xform: xform(mul(bx, d), mul(by, d), mul(bz, d), fin3(pos)), color: [1.0; 4], custom: [0.0, life, rnd(seed, 6), 0.0] }, life)
}

/// Chaff cloud at a decoy's position and age (L601): blooms over `chaff_bloom_s` to [`CHAFF_DIAMETER_M`],
/// thins toward the end of `chaff_life_s` and scales to 0 at its end. Spawn on `DecoyDeployed` with
/// `chaff_life_s` and `update` it every frame from the sim's drifting position and age. `custom.w` = bloom
/// 0..1. Timings come from the sim's tuning so the picture matches the radar cloud.
pub fn chaff_cloud(pos: [f32; 3], age_s: f32, tuning: &content::tuning::DecoyTuning, seed: u32) -> VfxInstance {
    let age = fin(age_s, 0.0).max(0.0);
    let (bloom_s, life) = ((tuning.chaff_bloom_s as f32).max(0.1), (tuning.chaff_life_s as f32).max(1.0));
    let bloom = smoothstep(0.0, bloom_s, age);
    let end = 1.0 - smoothstep(life * 0.9, life, age);
    let d = CHAFF_DIAMETER_M * (0.1 + 0.9 * bloom) * end;
    // Continuous slow tumble so the facets glint.
    let (bx, by, bz) = frame_y(UP, core::f32::consts::TAU * rnd(seed, 1) + 0.15 * age);
    VfxInstance { xform: xform(mul(bx, d), mul(by, 0.6 * d), mul(bz, d), fin3(pos)), color: [1.0; 4], custom: [0.0, life, rnd(seed, 2), bloom] }
}

/// Flare burn at a decoy's position and age (L602): an emissive star of [`FLARE_DIAMETER_M`] lasting
/// `flare_life_s`, scaling to 0 over its last 0.5 s. Spawn on `DecoyDeployed`, `update` every frame.
/// `custom.w` = emission 6 (between tracers and flashes, L1955; cap it with [`FlashLimiter::cap_emission`]).
pub fn flare_burn(pos: [f32; 3], age_s: f32, tuning: &content::tuning::DecoyTuning, seed: u32) -> VfxInstance {
    let age = fin(age_s, 0.0).max(0.0);
    let life = (tuning.flare_life_s as f32).max(0.5);
    let d = FLARE_DIAMETER_M * (1.0 - smoothstep(life - 0.5, life, age));
    let (bx, by, bz) = frame_y(UP, core::f32::consts::TAU * rnd(seed, 1) + 3.0 * age);
    VfxInstance { xform: xform(mul(bx, d), mul(by, d), mul(bz, d), fin3(pos)), color: [1.0; 4], custom: [0.0, life, rnd(seed, 2), 6.0] }
}

/// Puff `k` (0..[`DEFENCE_SMOKE_PUFFS`]) of a defence smoke cylinder (L1965) whose bottom centre is `base`
/// (at the sea surface) with the sim's current `radius_m` and `height_m` (`Decoys::smoke_cylinder`; pass
/// the centre minus half the height if the sim's centre is mid-height). The puffs fill the cylinder: 12
/// round the outside low, 4 inside and 8 higher up, overlapping so the screen stays opaque, and nothing
/// draws past the cylinder by more than a puff's lumps. Spawn all of them with [`Protection::DefenceSmoke`]
/// and `f32::INFINITY`, `update` every frame (the cylinder grows and drifts) and retire them on
/// `DecoyExpired`: the picture follows the sim exactly, the same on every tier. `custom.w` = churn rate
/// rad/s for the shader's slow roll.
pub fn defence_smoke_puff(base: [f32; 3], radius_m: f32, height_m: f32, seed: u32, k: u32) -> VfxInstance {
    let base = fin3(base);
    let r = fin(radius_m, 0.0).abs().min(1_000.0);
    let h = fin(height_m, 0.0).abs().min(1_000.0);
    let k = k % DEFENCE_SMOKE_PUFFS;
    // (ring radius fraction, height fraction of the puff centre, count in the ring, index in the ring)
    let (rf, yf, n, i) = match k { 0..=11 => (0.56, 0.3, 12, k), 12..=15 => (0.22, 0.35, 4, k - 12), _ => (0.45, 0.7, 8, k - 16) };
    let a = core::f32::consts::TAU * (i as f32 + 0.3 * rnd(seed, 8 * k)) / n as f32 + rnd(seed, 1);
    let rr = r * rf * lerp(0.9, 1.1, rnd(seed, 8 * k + 1));
    // Diameter wide enough to close the ring (12 puffs 0.29 r apart round 0.56 r) and the height. The outer
    // ring then reaches r (0.56 x 1.1 + 0.4 x 1.05) = 1.04 r at most while 0.8 r >= h (the sim's 60 by 30 m
    // shape), so the picture hides no more than the sim's cylinder does.
    let d = (0.8 * r).max(h) * lerp(0.85, 1.05, rnd(seed, 8 * k + 2));
    // Flattened a little so the top reads flat rather than as a row of domes; tops stay near the sim's height.
    let dy = 0.75 * d.min(1.4 * h);
    let y = (h * yf).min(h - 0.4 * dy).max(0.0);
    let p = [base[0] + rr * libm::cosf(a), base[1] + y, base[2] + rr * libm::sinf(a)];
    let (bx, by, bz) = frame_y(UP, core::f32::consts::TAU * rnd(seed, 8 * k + 3));
    VfxInstance { xform: xform(mul(bx, d), mul(by, dy), mul(bz, d), p), color: [1.0; 4], custom: [0.0, 0.0, rnd(seed, 8 * k + 4), lerp(0.03, 0.1, rnd(seed, 8 * k + 5))] }
}

/// Pump discharge (L1969) from a hull outlet at `outlet`, thrown out along the horizontal part of `outward`
/// and falling `height_m` to the sea, with `flow` 0..1 (pump rate over its rating). Attach it: spawn with
/// [`Protection::Flooding`] and `f32::INFINITY`, `update` every frame, retire when the pump stops.
/// `custom.w` = flow (the shader scrolls the jet along `custom0.x` by it).
pub fn pump_arc(outlet: [f32; 3], outward: [f32; 3], height_m: f32, flow: f32, seed: u32) -> VfxInstance {
    let o = norm_or(flat(fin3(outward)), [1.0, 0.0, 0.0]);
    let f = fin(flow, 0.0).clamp(0.0, 1.0);
    let drop = fin(height_m, 0.0).clamp(0.3, 20.0);
    // Jet speed 4 to 8 m/s: horizontal throw over the fall time, plus a little for the initial rise.
    let v = 4.0 + 4.0 * f;
    let throw = (v * (2.0 * drop / GRAVITY).sqrt() + 0.5).clamp(1.0, 15.0);
    let w = (0.12 + 0.18 * f) / 0.085;
    let z = cross(o, UP);
    VfxInstance { xform: xform(mul(o, throw / 0.9), mul(UP, drop / 0.85), mul(z, w), fin3(outlet)), color: [1.0; 4], custom: [0.0, 0.0, rnd(seed, 1), f] }
}

/// Boil ring (L1969) on the surface above a hole `depth_m` below the waterline, for a hole `hole_m` across:
/// bigger holes boil wider, deeper ones weaker. `pos` is the surface point above the hole. Attach it like
/// [`pump_arc`] ([`Protection::Flooding`]) while the compartment takes water. `custom.w` = intensity 0..1.
pub fn boil_ring(pos: [f32; 3], hole_m: f32, depth_m: f32, seed: u32) -> VfxInstance {
    let intensity = (1.0 - fin(depth_m, 0.0).max(0.0) / 6.0).clamp(0.15, 1.0);
    let d = ((1.5 + 2.5 * fin(hole_m, 0.0).abs()) * (0.6 + 0.4 * intensity)).clamp(0.5, 12.0);
    let (bx, by, bz) = frame_y(UP, core::f32::consts::TAU * rnd(seed, 1));
    VfxInstance { xform: xform(mul(bx, d), mul(by, d * intensity), mul(bz, d), fin3(pos)), color: [1.0; 4], custom: [0.0, 0.0, rnd(seed, 2), intensity] }
}

/// Foam band (L1969) along the hull's waterline from `a` to `b`, reaching out to the `outward` side, wider as
/// freeboard drops: `freeboard_lost` 0..1 (lost over the design freeboard) gives 0.6 m to 5 m. Attach it per
/// side ([`Protection::Flooding`]; flooding and list cues never degrade, L2033). `custom.w` = freeboard lost.
pub fn foam_band(a: [f32; 3], b: [f32; 3], outward: [f32; 3], freeboard_lost: f32) -> VfxInstance {
    let (mut a, mut b) = (fin3(a), fin3(b));
    let loss = fin(freeboard_lost, 0.0).clamp(0.0, 1.0);
    let width = 0.6 + 4.4 * loss;
    let x0 = sub(b, a);
    // Outward, perpendicular to the band in the horizontal plane.
    let along = norm_or(flat(x0), [1.0, 0.0, 0.0]);
    let o = flat(fin3(outward));
    let mut z = norm_or(sub(o, mul(along, dot(o, along))), cross(along, UP));
    if dot(z, o) < 0.0 && len(o) > 0.0 { z = mul(z, -1.0); }
    // Keep the frame right-handed (an up-facing band must not mirror): reverse the band if needed.
    if dot(x0, cross(UP, z)) < 0.0 { core::mem::swap(&mut a, &mut b); }
    let x = sub(b, a);
    VfxInstance { xform: xform(x, UP, mul(z, width), a), color: [1.0; 4], custom: [0.0, 0.0, rnd(pos_seed(a), 0), loss] }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inst(x: f32) -> VfxInstance {
        VfxInstance { xform: xform([1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0], [x, 0.0, 0.0]), color: [1.0; 4], custom: [0.0; 4] }
    }
    fn det(m: &[f32; 12]) -> f32 {
        let (x, y, z) = ([m[0], m[4], m[8]], [m[1], m[5], m[9]], [m[2], m[6], m[10]]);
        dot(x, cross(y, z))
    }
    fn origin(m: &[f32; 12]) -> [f32; 3] { [m[3], m[7], m[11]] }
    fn close(a: [f32; 3], b: [f32; 3], eps: f32) -> bool { len(sub(a, b)) <= eps }

    #[test]
    fn row_major_layout() {
        let m = xform([1.0, 2.0, 3.0], [4.0, 5.0, 6.0], [7.0, 8.0, 9.0], [10.0, 11.0, 12.0]);
        assert_eq!(m, [1.0, 4.0, 7.0, 10.0, 2.0, 5.0, 8.0, 11.0, 3.0, 6.0, 9.0, 12.0]);
        assert_eq!(origin(&m), [10.0, 11.0, 12.0]);
        let mut out = [0.0; 20];
        VfxInstance { xform: m, color: [0.1, 0.2, 0.3, 0.4], custom: [5.0, 6.0, 7.0, 8.0] }.write(&mut out);
        assert_eq!(&out[12..], &[0.1, 0.2, 0.3, 0.4, 5.0, 6.0, 7.0, 8.0]);
    }

    #[test]
    fn caps_and_growth() {
        assert_eq!(RingPool::new(VfxKind::SmokePuff, QualityTier::Medium).instances.len(), 4_096);
        assert_eq!(RingPool::new(VfxKind::SmokePuff, QualityTier::Low).instances.len(), 1_536);
        assert_eq!(RingPool::new(VfxKind::Debris, QualityTier::High).instances.len(), 2_048);
        let mut p = RingPool::new(VfxKind::DefenceSmoke, QualityTier::Fallback);
        for i in 0..200 { assert!(p.spawn(0.0, 100.0, Protection::DefenceSmoke, inst(i as f32)).is_some()); }
        assert_eq!(p.live_count(), 200);
        assert_eq!(p.instances.len() % GROW_SLOTS, 0);
        let mut buf = Vec::new();
        p.write_buffer([0.0; 3], &mut buf);
        assert_eq!(buf.len(), p.instances.len() * 20);
    }

    #[test]
    fn oldest_unprotected_recycles_with_a_hidden_frame() {
        let mut p = RingPool::new(VfxKind::Fireball, QualityTier::Low); // cap 32
        let mut hs = Vec::new();
        for i in 0..32 {
            let prot = if i == 0 { Protection::Fire } else { Protection::None };
            hs.push(p.spawn(i as f64 * 0.01, 10.0, prot, inst(i as f32)).unwrap());
        }
        let mut buf = Vec::new();
        p.tick(0.5);
        p.write_buffer([0.0; 3], &mut buf);
        assert_eq!(p.visible_count(), 32);
        // Full: slot 0 is protected, so slot 1 (the oldest unprotected) recycles.
        let h = p.spawn(0.6, 10.0, Protection::None, inst(99.0)).unwrap();
        assert_eq!(h.slot, hs[1].slot);
        assert_ne!(h.generation, hs[1].generation);
        assert!(!p.is_live(hs[1]));
        assert!(p.is_live(hs[0]));
        p.update(hs[1], inst(-5.0)); // stale handle: ignored
        assert_eq!(origin(&p.instances[h.slot as usize].xform)[0], 99.0);
        p.tick(0.6);
        p.write_buffer([0.0; 3], &mut buf);
        let s = h.slot as usize * 20;
        assert!(buf[s..s + 20].iter().all(|v| *v == 0.0), "recycled slot must draw hidden for a frame");
        assert!(p.fresh.is_empty());
        p.tick(0.62);
        p.write_buffer([0.0; 3], &mut buf);
        assert_eq!(buf[s + 3], 99.0);
        // Every slot protected: no slot to give.
        let mut q = RingPool::new(VfxKind::Fireball, QualityTier::Low);
        for _ in 0..32 { q.spawn(0.0, 10.0, Protection::Flooding, inst(0.0)).unwrap(); }
        assert!(q.spawn(0.0, 1.0, Protection::None, inst(0.0)).is_none());
    }

    #[test]
    fn two_frame_flash_survives_a_recycled_slot_at_30_fps() {
        let mut p = RingPool::new(VfxKind::HitFlash, QualityTier::Low);
        let mut buf = Vec::new();
        for i in 0..p.cap { p.spawn(0.0, 100.0, Protection::None, inst(i as f32)); }
        p.tick(0.0);
        p.write_buffer([0.0; 3], &mut buf);
        let dt = 1.0 / 30.0;
        let mut t = dt;
        let h = p.spawn(t, FLASH_LIFETIME_S, Protection::None, inst(500.0)).unwrap();
        let mut visible_frames = 0;
        for _ in 0..6 {
            p.tick(t);
            p.write_buffer([0.0; 3], &mut buf);
            if buf[h.slot as usize * 20 + 3] == 500.0 { visible_frames += 1; }
            t += dt;
        }
        assert_eq!(visible_frames, 2);
        // A free slot shows at once and also lasts two frames.
        let mut q = RingPool::new(VfxKind::HitFlash, QualityTier::Low);
        let h = q.spawn(0.0, FLASH_LIFETIME_S, Protection::None, inst(7.0)).unwrap();
        let mut frames = 0;
        let mut t = 0.0;
        for _ in 0..6 {
            q.tick(t);
            q.write_buffer([0.0; 3], &mut buf);
            if buf[h.slot as usize * 20 + 3] == 7.0 { frames += 1; }
            t += dt;
        }
        assert_eq!(frames, 2);
    }

    #[test]
    fn tick_ages_and_retires() {
        let mut p = RingPool::new(VfxKind::SmokePuff, QualityTier::Low);
        let mut buf = Vec::new();
        let mut i = inst(1.0);
        i.custom[1] = 0.0;
        let h = p.spawn(10.0, 2.0, Protection::None, i).unwrap();
        assert_eq!(p.instances[h.slot as usize].custom[1], 2.0, "lifetime fills an unset custom.y");
        p.tick(10.0);
        p.write_buffer([0.0; 3], &mut buf);
        p.tick(11.5);
        assert_eq!(p.instances[h.slot as usize].custom[0], 1.5);
        p.tick(9.0); // rewound
        assert_eq!(p.instances[h.slot as usize].custom[0], 0.0);
        p.tick(12.0);
        assert!(!p.is_live(h));
        p.write_buffer([0.0; 3], &mut buf);
        assert!(buf.iter().all(|v| *v == 0.0));
        assert_eq!(p.visible_count(), 0);
        // NaN lifetime still draws once, then retires.
        let h = p.spawn(20.0, f32::NAN, Protection::None, inst(3.0)).unwrap();
        p.tick(20.0);
        p.write_buffer([0.0; 3], &mut buf);
        assert_eq!(buf[h.slot as usize * 20 + 3], 3.0);
        p.tick(20.01);
        assert!(!p.is_live(h));
    }

    #[test]
    fn builder_lifetime_caps_the_pool_and_bad_instances_retire() {
        let mut p = RingPool::new(VfxKind::SplashColumn, QualityTier::Low);
        let l = splash_layout([0.0; 3], 152, 3);
        let c = l.columns[0];
        let h = p.spawn(0.0, l.lifetime_s, Protection::OwnSplash, c).unwrap();
        assert_eq!(p.lifetime_s[h.slot as usize], c.custom[1]);
        assert_eq!(p.instances[h.slot as usize].custom[1], c.custom[1]);
        let mut bad = inst(0.0);
        bad.xform[5] = f32::NAN;
        let b = p.spawn(0.0, 5.0, Protection::Fire, bad).unwrap();
        let mut buf = Vec::new();
        p.tick(0.0);
        p.write_buffer([0.0; 3], &mut buf);
        assert!(!p.is_live(b), "non-finite instance retired");
        assert!(buf.iter().all(|v| v.is_finite()));
        p.tick(c.custom[1] as f64 + 0.01);
        assert!(!p.is_live(h));
    }

    #[test]
    fn free_slots_hidden_last_frame_are_preferred() {
        let mut p = RingPool::new(VfxKind::HitFlash, QualityTier::Low);
        let mut buf = Vec::new();
        let a = p.spawn(0.0, 0.01, Protection::None, inst(1.0)).unwrap();
        p.tick(0.0);
        p.write_buffer([0.0; 3], &mut buf);
        p.tick(0.02); // a retires this frame; its slot was shown
        let b = p.spawn(0.02, 1.0, Protection::None, inst(2.0)).unwrap();
        assert_ne!(a.slot, b.slot);
        p.write_buffer([0.0; 3], &mut buf);
        assert_eq!(buf[b.slot as usize * 20 + 3], 2.0, "drawn on its first frame");
    }

    #[test]
    fn blended_sorts_back_to_front() {
        let mut p = RingPool::new(VfxKind::MissileTrail, QualityTier::Medium);
        for x in [5.0f32, 50.0, -20.0, 1.0] { p.spawn(0.0, 10.0, Protection::None, inst(x)); }
        let mut buf = Vec::new();
        p.tick(0.0);
        p.write_buffer([0.0; 3], &mut buf);
        let xs: Vec<f32> = (0..4).map(|k| buf[k * 20 + 3]).collect();
        assert_eq!(xs, vec![50.0, -20.0, 5.0, 1.0]);
        assert_eq!(p.visible_count(), 4);
        assert!(buf[4 * 20..].iter().all(|v| *v == 0.0));
    }

    #[test]
    fn rebase_shifts_live_instances() {
        let mut p = RingPool::new(VfxKind::WakeSegment, QualityTier::Low);
        let h = p.spawn(0.0, 30.0, Protection::None, inst(1000.0)).unwrap();
        p.rebase([-1000.0, 0.0, 250.0]);
        assert_eq!(origin(&p.instances[h.slot as usize].xform), [0.0, 0.0, 250.0]);
        assert_eq!(p.instances[h.slot as usize].xform[0], 1.0, "basis untouched");
        p.rebase([f32::NAN, 0.0, 0.0]);
        assert_eq!(origin(&p.instances[h.slot as usize].xform), [0.0, 0.0, 250.0]);
    }

    #[test]
    fn own_splash_protection_is_bounded() {
        let mut p = RingPool::new(VfxKind::SplashFoam, QualityTier::Low); // 640 slots
        let first = p.spawn(0.0, 6.0, Protection::OwnSplash, inst(0.0)).unwrap();
        for i in 1..PROTECTED_OWN_SPLASHES { p.spawn(i as f64 * 0.01, 6.0, Protection::OwnSplash, inst(0.0)).unwrap(); }
        assert_eq!(p.protection[first.slot as usize], Protection::OwnSplash);
        p.spawn(1.0, 6.0, Protection::OwnSplash, inst(0.0)).unwrap();
        assert_eq!(p.protection[first.slot as usize], Protection::None, "oldest own splash released");
        let own = p.protection.iter().filter(|x| **x == Protection::OwnSplash).count();
        assert_eq!(own, PROTECTED_OWN_SPLASHES);
        // Fill the rest; the newest own splash still gets a slot.
        for _ in 0..p.cap { p.spawn(2.0, 6.0, Protection::None, inst(0.0)); }
        assert!(p.spawn(2.5, 6.0, Protection::OwnSplash, inst(0.0)).is_some());
    }

    #[test]
    fn missile_head_protection_expires_and_trims() {
        let mut p = RingPool::new(VfxKind::MissileTrail, QualityTier::Low);
        let head = p.spawn(0.0, 6.0, Protection::IncomingMissileHead, inst(0.0)).unwrap();
        let tail = p.spawn(0.0, 6.0, Protection::None, inst(0.0)).unwrap();
        p.tick(0.0);
        let mut buf = Vec::new();
        p.write_buffer([0.0; 3], &mut buf);
        assert_eq!(p.trim_older_than(1.0, 0.5), 1);
        assert!(p.is_live(head) && !p.is_live(tail));
        assert_eq!(p.trim_older_than(2.5, TRAIL_HEAD_S), 1);
        assert!(!p.is_live(head));
    }

    #[test]
    fn splash_matches_the_spec() {
        for (cal, seed) in [(20u16, 1u32), (127, 2), (152, 3), (406, 4), (460, 5), (0, 6)] {
            let l = splash_layout([10.0, 0.0, -4.0], cal, seed);
            assert!((3..=5).contains(&l.columns.len()));
            let h = (cal as f32 * 0.25).clamp(0.25, 40.0);
            let tallest = l.columns.iter().map(|c| apply_xform(&c.xform, [0.0, 1.0, 0.0])[1]).fold(f32::MIN, f32::max);
            assert!(tallest <= h && tallest >= 0.85 * h - 0.5, "cal {cal}: {tallest} vs {h}");
            for c in &l.columns {
                let life = c.custom[1];
                let (rise, hold_end) = splash_column_phases(life);
                assert!((0.6..=1.0).contains(&rise));
                assert!((hold_end - rise - 0.4).abs() < 1e-5);
                let collapse = life - hold_end;
                assert!((2.0 - 1e-4..=3.0 + 1e-4).contains(&collapse), "collapse {collapse}");
                assert!((0.0..=1.0).contains(&c.custom[3]));
                assert!(det(&c.xform) > 0.0);
            }
            assert_eq!(l.foam.custom[1], 6.0);
            assert_eq!(l.lifetime_s, 6.0);
            assert_eq!(l, splash_layout([10.0, 0.0, -4.0], cal, seed), "deterministic");
        }
        assert!(splash_layout([0.0; 3], 460, 9).columns.iter().map(|c| apply_xform(&c.xform, [0.0, 1.0, 0.0])[1]).fold(0.0, f32::max) <= 40.0);
    }

    #[test]
    fn hit_matches_the_spec() {
        let v = hit_layout([0.0, 5.0, 0.0], [0.0, 0.0, 1.0], 500, 7);
        assert_eq!(v[0].0, VfxKind::HitFlash);
        let puffs = v.iter().filter(|e| e.0 == VfxKind::HitPuff).count();
        assert!((3..=7).contains(&puffs));
        assert_eq!(v.iter().filter(|e| e.0 == VfxKind::Debris).count(), 64);
        assert_eq!(v.last().unwrap().0, VfxKind::Debris);
        for (k, i, life) in &v {
            assert!(det(&i.xform) > 0.0);
            assert_eq!(i.custom[1], *life);
            if *k == VfxKind::HitPuff { assert_eq!(*life, 1.2); }
            if *k == VfxKind::Debris {
                let e = len([i.xform[0], i.xform[4], i.xform[8]]);
                assert!((e - 0.5).abs() < 1e-4);
                assert!(i.custom[3] >= 6.0 && i.custom[3] <= 22.0);
            }
        }
        let few = hit_layout([0.0; 3], [0.0; 3], 0, 1);
        assert!(few.iter().all(|e| e.0 != VfxKind::Debris));
        assert!(hit_layout([f32::NAN; 3], [f32::NAN; 3], 3, 2).iter().all(|e| e.1.xform.iter().all(|x| x.is_finite())));
    }

    #[test]
    fn wake_and_kelvin_frames() {
        let a = [0.0, 0.0, 0.0];
        let b = [0.0, 0.0, -10.0];
        let w = wake_segment(a, b, 12.0, 30.0);
        assert!(close(apply_xform(&w.xform, [0.0; 3]), a, 1e-5) && close(apply_xform(&w.xform, [1.0, 0.0, 0.0]), b, 1e-5));
        let width = len(sub(apply_xform(&w.xform, [0.0, 0.0, 0.5]), apply_xform(&w.xform, [0.0, 0.0, -0.5])));
        assert!((width - 30.0).abs() < 1e-3, "2.5 beams at 30 kn: {width}");
        let slow = wake_segment(a, b, 12.0, 0.0);
        assert!((len([slow.xform[2], slow.xform[6], slow.xform[10]]) - 12.0).abs() < 1e-3, "1 beam when slow");
        assert!(det(&w.xform) > 0.0);
        assert_eq!(w.custom[1], 30.0);

        assert!(kelvin_arm_instances(a, 0.0, 8.0).is_empty());
        // Heading north at 20 kn: arms trail south (+Z), spread east and west, outward drift = v tan 19.47.
        let arms = kelvin_arm_instances(a, 0.0, 20.0);
        assert_eq!(arms.len(), 2);
        let v = 20.0 * KN_TO_MS;
        for arm in &arms {
            assert!(det(&arm.xform) > 0.0);
            let end = apply_xform(&arm.xform, [1.0, 0.0, 0.0]);
            assert!(end[2] > 0.0, "arm runs aft");
            let angle = libm::atanf(end[0].abs() / end[2]).to_degrees();
            assert!((angle - 19.47).abs() < 0.05, "{angle}");
            // Drift along the normalised local Z, signed: moves away from the track.
            let z = norm_or([arm.xform[2], arm.xform[6], arm.xform[10]], UP);
            let drift = mul(z, arm.custom[3]);
            assert!(drift[0] * end[0] > 0.0, "drift is outward");
            assert!((len(drift) - v * libm::tanf(19.47f32.to_radians())).abs() < 1e-3);
        }
        let bw = bow_wave_instance(a, 1.0, 10.0, 25.0);
        assert!(det(&bw.xform) > 0.0);
        assert_eq!(bow_wave_instance(a, 1.0, 10.0, 0.0).xform[0], 0.0);
    }

    #[test]
    fn tracer_length_and_width() {
        let t = tracer_instance([100.0, 10.0, 0.0], [800.0, 0.0, 0.0], 127, 10.0, 935.0);
        let head = apply_xform(&t.xform, [0.5, 0.0, 0.0]);
        let tail = apply_xform(&t.xform, [-0.5, 0.0, 0.0]);
        assert!(close(head, [100.0, 10.0, 0.0], 1e-3));
        assert!((len(sub(head, tail)) - 20.0).abs() < 1e-3, "800 m/s x 1/40 s");
        assert!(det(&t.xform) > 0.0);
        // Far away the width is held at 1.5 px.
        let far = tracer_instance([0.0; 3], [0.0, 0.0, 800.0], 127, 20_000.0, 935.0);
        let w = len([far.xform[1], far.xform[5], far.xform[9]]);
        assert!((w * 935.0 / 20_000.0 - 1.5).abs() < 1e-3, "{w}");
        assert!((4.0..=8.0).contains(&far.custom[3]));
        let bad = tracer_instance([f32::NAN; 3], [0.0; 3], 0, f32::NAN, -1.0);
        assert!(bad.xform.iter().all(|v| v.is_finite()));
    }

    #[test]
    fn trail_spans_its_points() {
        let (a, b) = ([1.0, 2.0, 3.0], [1.0, 40.0, 3.0]);
        let s = trail_segment(a, b);
        assert!(close(apply_xform(&s.xform, [0.0; 3]), a, 1e-4) && close(apply_xform(&s.xform, [1.0, 0.0, 0.0]), b, 1e-4));
        assert!(det(&s.xform) > 0.0);
        assert_eq!(trail_length_s(QualityTier::Low), 6.0);
    }

    #[test]
    fn flash_limiter_floor_6() {
        let mut f = FlashLimiter::default();
        // 10 Hz for 3 s of big flashes: exactly 3 full per second, the rest dim with no light.
        let mut full = [0u32; 3];
        for k in 0..30 {
            let t = k as f64 * 0.1;
            let (e, light) = f.admit(t, 0.1);
            if e == 1.0 { assert!(light); full[(t + 1e-9) as usize] += 1; } else { assert_eq!((e, light), (0.25, false)); }
        }
        assert_eq!(full, [3, 3, 3]);
        // Small flashes are never limited.
        assert_eq!(f.admit(3.0, 0.01), (1.0, true));
        // Rewinding drops future entries.
        assert_eq!(f.admit(0.05, 0.5), (1.0, true));
        assert_eq!(f.recent, vec![0.05]);
        // Reduce flashing caps the brightest flash at 4.
        let mut r = FlashLimiter { reduce_flashing: true, ..Default::default() };
        let (e, _) = r.admit(0.0, 0.5);
        assert!(e * super::super::emission::FLASH.1 <= 4.0 + 1e-6);
        assert_eq!(r.cap_emission(16.0), 4.0);
        assert_eq!(f.cap_emission(16.0), 16.0);
        assert_eq!(f.admit(f64::NAN, 0.5).1, false);
    }

    #[test]
    fn retier_keeps_protected_and_newest_and_remaps_handles() {
        let mut p = RingPool::new(VfxKind::Fireball, QualityTier::Medium); // 64
        let mut hs = Vec::new();
        let mut buf = Vec::new();
        for i in 0..64 {
            let prot = if i % 16 == 0 { Protection::Fire } else { Protection::None };
            hs.push(p.spawn(i as f64, 1_000.0, prot, inst(i as f32)).unwrap());
            p.tick(i as f64);
            p.write_buffer([0.0; 3], &mut buf);
        }
        p.tick(64.0);
        let moved = p.retier(64.0, QualityTier::Low); // 32
        assert_eq!(p.instances.len(), 32);
        assert_eq!(p.live_count(), 32);
        // The 4 protected ones survive wherever they were; the newest unprotected fill the rest.
        for (i, h) in hs.iter().enumerate() {
            let now = moved.iter().find(|m| m.0 == *h).map(|m| m.1).unwrap_or(*h);
            let alive = p.is_live(now) && origin(&p.instances[now.slot as usize].xform)[0] == i as f32;
            let expect = i % 16 == 0 || i >= 35;
            assert_eq!(alive, expect, "instance {i}");
            if alive {
                assert_eq!(p.born_s[now.slot as usize], i as f64, "age kept");
                assert_eq!(p.instances[now.slot as usize].custom[0], (64 - i) as f32, "age kept");
            }
        }
        for (old, new) in &moved { assert!(!p.is_live(*old) || old == new); }
        // A full pool still recycles its oldest unprotected slot, and the buffer matches the new size.
        let h = p.spawn(70.0, 1.0, Protection::None, inst(500.0)).unwrap();
        assert!((h.slot as usize) < 32);
        p.tick(70.0);
        p.write_buffer([0.0; 3], &mut buf);
        assert_eq!(buf.len(), 32 * 20);
        // Growing back only adds slots.
        assert!(p.retier(70.0, QualityTier::High).is_empty());
        assert_eq!(p.instances.len(), 64);
        assert_eq!(p.live_count(), 32);
        let mut d = RingPool::new(VfxKind::DefenceSmoke, QualityTier::High);
        d.spawn(0.0, 10.0, Protection::DefenceSmoke, inst(0.0));
        assert!(d.retier(0.0, QualityTier::Low).is_empty());
        assert_eq!(d.live_count(), 1);
    }

    #[test]
    fn emitters_pace_by_rate() {
        // 3 Hz for 10 s at 60 fps: 30 emissions however the frames fall.
        let mut n = 0;
        for k in 0..600 { n += emissions_between(k as f64 / 60.0, (k + 1) as f64 / 60.0, 3.0, 0.37); }
        assert_eq!(n, 30);
        assert_eq!(emissions_between(0.0, 100.0, 4.0, 0.0), MAX_EMISSIONS_PER_CALL);
        assert_eq!(emissions_between(5.0, 4.0, 4.0, 0.0), 0);
        assert_eq!(emissions_between(0.0, 1.0, f32::NAN, 0.0), 0);
        assert_eq!((fire_smoke_hz(0.0), fire_smoke_hz(1.0), fire_smoke_hz(f32::NAN)), (2.0, 4.0, 2.0));
        assert_ne!(effect_seed(1, 2), effect_seed(1, 3));
        let f = flash_screen_fraction(5.0, 100.0, 935.0, [1920.0, 1080.0]);
        assert!((f - core::f32::consts::PI * 46.75 * 46.75 / (1920.0 * 1080.0)).abs() < 1e-6);
        assert_eq!(flash_screen_fraction(5.0, 1.0, 935.0, [1920.0, 1080.0]), 1.0);
        assert_eq!(flash_screen_fraction(f32::NAN, 1.0, 935.0, [0.0, 1080.0]), 0.0);
    }

    #[test]
    fn fire_muzzle_flak_builders() {
        let tune = content::tuning::DecoyTuning::default();
        let nan = [f32::NAN; 3];
        let finite = |i: &VfxInstance| i.xform.iter().chain(&i.color).chain(&i.custom).all(|v| v.is_finite());
        for seed in 0..20u32 {
            let (fl, life) = flame_tongue([0.0, 5.0, 0.0], 3.0, 1.0, [8.0, 0.0, 0.0], seed);
            assert!(det(&fl.xform) > 0.0 && (FLAME_LIFETIME_S.0..=FLAME_LIFETIME_S.1).contains(&life));
            assert!((2.0..=4.0).contains(&fl.custom[3]));
            let tip = apply_xform(&fl.xform, [0.0, 1.0, 0.0]);
            assert!(tip[1] > 5.0 && tip[0] > apply_xform(&fl.xform, [0.0; 3])[0], "rises and leans downwind");
            let (sm, sl) = fire_smoke_puff([0.0, 9.0, 0.0], 0.5, [5.0, 0.0, 0.0], seed);
            assert!(det(&sm.xform) > 0.0 && sl > 5.0);
            assert!(sm.xform[5] > 0.0 && sm.custom[3] > SMOKE_RISE_MS * 0.5, "drifts up");
            for (k, i, l) in fireball_layout([0.0; 3], 6.0, [0.0; 3], seed) {
                assert!(det(&i.xform) > 0.0 && i.custom[1] == l);
                if k == VfxKind::Fireball { assert!((2.0..=4.0).contains(&i.custom[3])); }
            }
            let m = muzzle_layout([0.0; 3], [0.0, 0.1, -1.0], 152, [3.0, 0.0, 0.0], seed);
            assert_eq!(m[0].0, VfxKind::MuzzleStar);
            assert_eq!(m[0].2, FLASH_LIFETIME_S);
            assert_eq!(m[1].0, VfxKind::MuzzleHaze);
            assert_eq!(m.iter().filter(|e| e.0 == VfxKind::SmokePuff).count(), 2);
            assert!(m.iter().all(|e| det(&e.1.xform) > 0.0 && e.1.custom[1] == e.2));
            let tip = apply_xform(&m[0].1.xform, [1.0, 0.0, 0.0]);
            assert!(tip[2] < -5.0, "star points down the bore");
            let (fb, fbl) = flak_burst([0.0, 800.0, 0.0], 127, seed);
            assert!(det(&fb.xform) > 0.0 && (2.0..=5.5).contains(&fbl));
            assert!(finite(&flame_tongue(nan, f32::NAN, f32::NAN, nan, seed).0));
            assert!(muzzle_layout(nan, nan, 0, nan, seed).iter().all(|e| finite(&e.1)));
            assert!(fireball_layout(nan, f32::NAN, nan, seed).iter().all(|e| finite(&e.1)));
        }
        for cal in [20u16, 127, 460, 0, u16::MAX] {
            let l = muzzle_light([0.0; 3], [1.0, 0.0, 0.0], cal);
            assert!(l.range_m <= MUZZLE_LIGHT_MAX_M && l.duration_s <= MUZZLE_LIGHT_MAX_S && l.range_m > 0.0);
        }
        // Decoys follow the sim's timings: chaff full at bloom, gone at its end; flare gone at its end.
        let size = |i: &VfxInstance| len([i.xform[0], i.xform[4], i.xform[8]]);
        assert!((size(&chaff_cloud([0.0; 3], 3.0, &tune, 1)) - CHAFF_DIAMETER_M).abs() < 1e-3);
        assert!(size(&chaff_cloud([0.0; 3], 0.0, &tune, 1)) < 0.2 * CHAFF_DIAMETER_M);
        assert_eq!(size(&chaff_cloud([0.0; 3], 60.0, &tune, 1)), 0.0);
        assert_eq!(chaff_cloud([0.0; 3], 1.0, &tune, 1).custom[1], 60.0);
        assert!((size(&flare_burn([0.0; 3], 1.0, &tune, 1)) - FLARE_DIAMETER_M).abs() < 1e-4);
        assert_eq!(size(&flare_burn([0.0; 3], 5.0, &tune, 1)), 0.0);
        assert!(det(&chaff_cloud([0.0; 3], 10.0, &tune, 1).xform) > 0.0 && det(&flare_burn([0.0; 3], 1.0, &tune, 1).xform) > 0.0);
        assert!(finite(&chaff_cloud(nan, f32::NAN, &tune, 1)) && finite(&flare_burn(nan, f32::NAN, &tune, 1)));
    }

    #[test]
    fn defence_smoke_fills_the_sim_cylinder() {
        let (r, h) = (60.0f32, 30.0f32);
        let base = [100.0, 0.0, -50.0];
        let mut covered = 0;
        for k in 0..DEFENCE_SMOKE_PUFFS {
            let p = defence_smoke_puff(base, r, h, 9, k);
            assert!(det(&p.xform) > 0.0);
            let o = origin(&p.xform);
            let d = len([p.xform[0], p.xform[4], p.xform[8]]);
            let reach = len(sub([o[0], 0.0, o[2]], [base[0], 0.0, base[2]])) + d / 2.0;
            assert!(reach <= 1.05 * r, "puff {k} reaches {reach}");
            let top = o[1] + len([p.xform[1], p.xform[5], p.xform[9]]) / 2.0;
            assert!(top <= 1.3 * h && top >= 0.5 * h, "puff {k} top {top}");
            covered += 1;
        }
        assert_eq!(covered, DEFENCE_SMOKE_PUFFS);
        // Points at mid height, 0.7 r out, on 16 bearings lie inside some puff: no sight gap through the wall.
        for b in 0..16 {
            let a = core::f32::consts::TAU * b as f32 / 16.0;
            let q = [base[0] + 0.7 * r * libm::cosf(a), 0.5 * h, base[2] + 0.7 * r * libm::sinf(a)];
            let hit = (0..DEFENCE_SMOKE_PUFFS).any(|k| {
                let p = defence_smoke_puff(base, r, h, 9, k);
                len(sub(q, origin(&p.xform))) <= len([p.xform[0], p.xform[4], p.xform[8]]) / 2.0
            });
            assert!(hit, "gap in the smoke wall at bearing {b}");
        }
        assert!(defence_smoke_puff([f32::NAN; 3], f32::NAN, f32::INFINITY, 1, 99).xform.iter().all(|v| v.is_finite()));
        assert_eq!(len([defence_smoke_puff(base, 0.0, 0.0, 1, 0).xform[0], 0.0, 0.0]), 0.0);
    }

    #[test]
    fn flooding_builders() {
        // Pump arc: the jet's end lands `height` below the outlet, thrown outward.
        let a = pump_arc([0.0, 4.0, 0.0], [0.0, 0.3, 1.0], 4.0, 1.0, 3);
        let end = apply_xform(&a.xform, [0.9, -0.85, 0.0]);
        assert!(end[1].abs() < 1e-4 && end[2] > 2.0 && end[0].abs() < 1e-4, "{end:?}");
        assert!(det(&a.xform) > 0.0 && a.custom[3] == 1.0);
        let b = boil_ring([0.0; 3], 1.0, 1.0, 3);
        let deep = boil_ring([0.0; 3], 1.0, 10.0, 3);
        assert!(det(&b.xform) > 0.0 && b.custom[3] > deep.custom[3] && b.xform[0].abs() + b.xform[8].abs() > 0.0);
        // Foam band: right-handed for either side, spans a..b, width grows with lost freeboard.
        for out in [[1.0, 0.0, 0.0], [-1.0, 0.0, 0.0]] {
            let (p, q) = ([0.0, 0.0, -20.0], [0.0, 0.0, 20.0]);
            let f = foam_band(p, q, out, 0.5);
            assert!(det(&f.xform) > 0.0);
            let (e0, e1) = (apply_xform(&f.xform, [0.0; 3]), apply_xform(&f.xform, [1.0, 0.0, 0.0]));
            assert!((close(e0, p, 1e-4) && close(e1, q, 1e-4)) || (close(e0, q, 1e-4) && close(e1, p, 1e-4)));
            let reach = apply_xform(&f.xform, [0.5, 0.0, 1.0]);
            assert!(reach[0] * out[0] > 2.7 && (reach[0].abs() - 2.8).abs() < 1e-4, "{reach:?}");
        }
        let bad = [f32::NAN; 3];
        for i in [pump_arc(bad, bad, f32::NAN, f32::NAN, 0), boil_ring(bad, f32::NAN, f32::NAN, 0), foam_band(bad, bad, bad, f32::NAN)] {
            assert!(i.xform.iter().chain(&i.custom).all(|v| v.is_finite()));
        }
    }
}
