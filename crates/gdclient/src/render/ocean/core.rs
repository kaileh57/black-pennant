//! Part of unit `gd-render-ocean`. Everything of the sea with no Godot in it: the wave set and when it
//! changes, the per-frame uniforms, the Nyquist-fade rule, the clipmap placement, the detail and cloud-shadow
//! inputs, and the client-side surface queries. `gpu.rs` applies what this decides. Tests run headless.
//!
//! # The sea is the simulation's
//! When the weather changes the simulation turns its wave set from the set in force to the new weather's
//! over `sim_core::world::weather::WEATHER_BLEND_S` (`World::set_weather`, `weather::blended_waves`), and
//! the renderer does the same: a `WeatherChanged` is applied when playback reaches its tick, with the
//! event's own seed and the catalogue's wave tuning, and the drawn set is the same blend at the rendered
//! time, which is the set `sim-core` floats ships on (to within the tick between the event and the blend's
//! first tick). What the simulation does not read (the shader's sea-state number for whitecaps, rain
//! roughness, cloud cover) eases over [`LOOK_BLEND_S`] of simulation time. Phases are packed by
//! `WaveSet::pack_uniforms` in f64 at the render origin and at `ViewFrame::sim_time_s`.
//!
//! # Nyquist fade (L1887)
//! The clipmap's far levels cannot carry short waves, and the shader fades a component out where its
//! wavelength is under four grid spacings. That makes the drawn surface differ from the simulated one, so
//! it is allowed only while the amplitude faded away under every drawn vessel stays within 0.001 of the
//! camera's distance to it. [`OceanCore::step`] finds the grid spacing under each vessel from the level
//! that draws it, sums the faded amplitude, and turns the fade off (every component drawn everywhere, with
//! whatever aliasing that brings far out) while any vessel breaks the limit.
// Client rendering, not simulation (clippy.toml): std floats are allowed here. The wave maths itself is
// sim-core's and stays on libm.
#![allow(clippy::disallowed_methods)]
use crate::interp::ViewFrame;
use crate::seams::WireEvent;
use content::tuning::WaveTuning;
use content::weather::WeatherState;
use glam::DVec3;
use hull_mesh::env::ocean::{snap_origins, ClipmapSpec};
use sim_core::events::PhysEvent;
use sim_core::waves::{WaveSet, WaveUniforms};
use sim_systems::protocol::SimEvent;

/// Whitecap level, rain roughness and cloud cover ease to a new weather over this long (simulation
/// seconds): the shortest blend the sky uses (L1870), so sea and sky turn together.
pub const LOOK_BLEND_S: f64 = 20.0;
/// Faded amplitude allowed under a vessel per metre of camera distance (L1887).
pub const NYQUIST_LIMIT: f64 = 0.001;
/// The fade comes back only once the worst vessel is this far inside the limit, so it does not flicker.
const NYQUIST_RETURN: f64 = 0.7;
/// Seconds the fade takes to switch (wall time): fast enough that a vessel never sits long on a wrong
/// surface, slow enough not to pop.
const NYQUIST_RAMP_S: f64 = 0.4;
/// Drawn vessels are treated as discs of this radius about their pose for the grid-spacing lookup: the
/// unit is given poses only, and no hull is longer than this.
const VESSEL_PAD_M: f64 = 120.0;
/// Edge of one cloud-shadow noise tile in metres (`cloud_shadow.gdshaderinc`).
pub const CLOUD_SHADOW_TILE_M: f64 = 6_000.0;
/// Baked-detail strength in a sortie and in the dry dock (`water.gdshader`).
pub const DETAIL_STRENGTH: f32 = 1.0;
pub const DOCK_DETAIL_STRENGTH: f32 = 0.25;
/// Degrade step at which the second detail tiling is dropped (L2031).
const ONE_TILING_FROM_STEP: u8 = 6;
/// Longest ray the surface query follows, metres.
pub const MAX_RAY_M: f64 = 60_000.0;
/// Samples along a ray's passage through the wave slab before the Newton steps.
const RAY_MARCH_STEPS: usize = 32;
/// A weather turn that is further ahead of playback than this is applied at once (a seek landed after it).
const TURN_HOLD_MAX_TICKS: f64 = 0.5 * sim_core::TICK_HZ as f64;

/// What the shader is told about the weather besides the waves.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct Look { pub sea_state: f32, pub rain: f32, pub cloud_cover: f32 }
impl Look {
    fn of(w: &WeatherState) -> Self {
        let unit = |v: f64| if v.is_finite() { v.clamp(0.0, 1.0) as f32 } else { 0.0 };
        Self { sea_state: w.sea_state.0.min(8) as f32, rain: unit(w.rain), cloud_cover: unit(w.cloud_cover) }
    }
    fn lerp(a: Look, b: Look, t: f32) -> Look {
        let l = |x: f32, y: f32| x + (y - x) * t;
        Look { sea_state: l(a.sea_state, b.sea_state), rain: l(a.rain, b.rain), cloud_cover: l(a.cloud_cover, b.cloud_cover) }
    }
}

/// One frame's inputs for the engine side.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct OceanFrame {
    /// Packed for the render origin and the rendered sim time. `t_ref` is replaced by the shader clock.
    pub uniforms: WaveUniforms,
    /// Render-space translation of each clipmap level; the skirt rides with the last.
    pub levels: Vec<[f32; 3]>,
    pub look: Look,
    pub nyquist_fade: f32,
    pub detail_phase: f32,
    pub detail_origin: [f32; 4],
    pub detail_tilings: i32,
    pub detail_strength: f32,
    /// `bp_cloud_shadow_xf`.
    pub cloud_xf: [f32; 4],
    /// How far a vertex can move from its rest position, for the instances' visibility margin.
    pub reach_m: f32,
    /// Camera position on the map, for the shore window.
    pub camera_xz: [f64; 2],
    pub origin: [f64; 2],
    /// Unit render-space xz the wind blows towards (the whitecaps' streaks run along it).
    pub wind_dir: [f32; 2],
    /// Whitecaps drawn as streaks along the wind; off on Low and Fallback, the first detail to go.
    pub foam_streaks: bool,
}

struct Turn { tick: u64, weather: WeatherState, seed: u64 }

pub(crate) struct OceanCore {
    pub spec: ClipmapSpec,
    pub waves: WaveSet,
    pub wave_seed: u64,
    /// A weather turn in progress: the set it turns from, the set it turns to and the sim time it began.
    wave_from: WaveSet,
    wave_to: WaveSet,
    wave_turn_s: Option<f64>,
    tuning: WaveTuning,
    weather: Option<WeatherState>,
    queue: Vec<Turn>,
    look: Look,
    look_from: Look,
    look_to: Look,
    look_start_s: f64,
    wind_base: [f64; 2],
    wind_t0: f64,
    wind_vel: [f64; 2],
    /// The dry dock's flat water level, when on.
    pub dock: Option<f32>,
    /// 1 while the Nyquist fade is allowed, easing to 0 when it is not.
    fade: f32,
    fade_allowed: bool,
    last_s: Option<f64>,
}

fn smoothstep(e0: f64, e1: f64, x: f64) -> f64 {
    if e1 <= e0 { return if x >= e1 { 1.0 } else { 0.0 }; }
    let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}
fn fract(v: f64) -> f32 { let f = v - v.floor(); if f.is_finite() { f as f32 } else { 0.0 } }

/// Amplitude the shader's fade removes on a grid of effective spacing `spacing_m`: the sum over components
/// of `A (1 - smoothstep(2 s, 4 s, lambda))`.
pub(crate) fn faded_amplitude(waves: &WaveSet, spacing_m: f64) -> f64 {
    let n = (waves.count as usize).min(sim_core::waves::MAX_WAVES);
    waves.waves[..n].iter().filter(|w| w.k > 0.0 && w.amplitude.is_finite()).map(|w| {
        let lambda = std::f64::consts::TAU / w.k;
        w.amplitude.abs() * (1.0 - smoothstep(2.0 * spacing_m, 4.0 * spacing_m, lambda))
    }).sum()
}

/// Effective grid spacing of the water drawn under a disc: the spacing of the finest level whose
/// unmorphed interior holds the whole disc, or twice the next one's where it reaches a morph band
/// (`s' = s (1 + w)`, L1895). `origins` are the levels' snapped centres.
pub(crate) fn spacing_under(spec: &ClipmapSpec, origins: &[[f64; 2]], at: [f64; 2], radius_m: f64) -> f64 {
    // hm-ocean's layout: q quads per side with q mod 4 == 2, grid indices -(q/2 - 1) ..= q/2 + 1, and a
    // morph band of n / 10 cells inside the edge.
    let q = (((spec.n as i64 - 1).max(14) - 2) / 4) * 4 + 2;
    let (lo, hi) = (-(q / 2 - 1) as f64, (q / 2 + 1) as f64);
    let band = (spec.n as f64 / 10.0).max(3.0);
    let s0 = if spec.s0_m.is_finite() && spec.s0_m > 0.0 { spec.s0_m } else { 1.0 };
    let mut s = s0;
    for c in origins {
        let (min, max) = ((lo + band) * s + radius_m, (hi - band) * s - radius_m);
        let (dx, dz) = (at[0] - c[0], at[1] - c[1]);
        if min <= max && dx >= min && dx <= max && dz >= min && dz <= max { return s; }
        s *= 2.0;
    }
    // Beyond the last level's interior: its morph band or the skirt, both at twice its spacing.
    s
}

impl OceanCore {
    pub fn new(spec: ClipmapSpec, tuning: WaveTuning) -> Self {
        Self {
            spec, waves: WaveSet::calm(), wave_seed: 0, wave_from: WaveSet::calm(), wave_to: WaveSet::calm(), wave_turn_s: None, tuning, weather: None, queue: Vec::new(),
            look: Look::default(), look_from: Look::default(), look_to: Look::default(), look_start_s: 0.0,
            wind_base: [0.0; 2], wind_t0: 0.0, wind_vel: [0.0; 2], dock: None, fade: 1.0, fade_allowed: true, last_s: None,
        }
    }

    fn reset(&mut self) {
        self.waves = WaveSet::calm();
        self.wave_seed = 0;
        self.wave_from = WaveSet::calm();
        self.wave_to = WaveSet::calm();
        self.wave_turn_s = None;
        self.weather = None;
        self.queue.clear();
        self.look = Look::default();
        self.look_from = Look::default();
        self.look_to = Look::default();
        self.wind_base = [0.0; 2];
        self.wind_t0 = 0.0;
        self.wind_vel = [0.0; 2];
        self.fade = 1.0;
        self.fade_allowed = true;
    }

    /// Cloud drift on the map at sim time `t`: the same closed form as the sky's (`base + wind (t - t0)`).
    fn wind_offset(&self, t: f64) -> [f64; 2] {
        let dt = (t - self.wind_t0).max(0.0);
        [self.wind_base[0] + self.wind_vel[0] * dt, self.wind_base[1] + self.wind_vel[1] * dt]
    }

    fn take(&mut self, turn: Turn, at_s: f64, first: bool) {
        let w = turn.weather;
        let to = WaveSet::generate(turn.seed, &w, &self.tuning);
        if first {
            self.waves = to;
            self.wave_turn_s = None;
        } else {
            // A turn during a turn starts from the blend in force, as `World::set_weather` does.
            self.wave_from = match self.wave_turn_s {
                Some(t0) => sim_core::world::weather::blended_waves(&self.wave_from, &self.wave_to, at_s - t0, &self.tuning),
                None => self.waves,
            };
            self.wave_to = to;
            self.wave_turn_s = Some(at_s);
        }
        self.wave_seed = turn.seed;
        let speed = if w.wind_speed_mps.is_finite() { w.wind_speed_mps.clamp(0.0, 80.0) } else { 0.0 };
        let dir = if w.wind_dir_rad.is_finite() { w.wind_dir_rad } else { 0.0 };
        self.wind_base = self.wind_offset(at_s);
        self.wind_t0 = at_s;
        self.wind_vel = [dir.sin() * speed, -dir.cos() * speed];
        self.look_to = Look::of(&w);
        self.look_from = if first { self.look_to } else { self.look };
        self.look_start_s = at_s;
        if first { self.look = self.look_to; }
        self.weather = Some(w);
    }

    /// The weather in force at the rendered time, once one has arrived.
    pub fn weather(&self) -> Option<&WeatherState> { self.weather.as_ref() }

    /// One frame. `camera` is the camera's world position when there is one.
    pub fn step(&mut self, view: &ViewFrame, events: &[WireEvent], camera: Option<[f64; 3]>, degrade_step: u8, delta_s: f64) -> OceanFrame {
        let now = if view.sim_time_s.is_finite() { view.sim_time_s } else { 0.0 };
        if self.last_s.is_some_and(|l| now + 1e-6 < l) {
            // A rewound timeline (replay seek): pending turns belong to the old one.
            self.queue.clear();
        }
        self.last_s = Some(now);
        for ev in events {
            match &ev.event {
                SimEvent::SortieStarted { .. } => self.reset(),
                SimEvent::Phys(PhysEvent::WeatherChanged { weather, wave_seed }) => self.queue.push(Turn { tick: ev.tick, weather: *weather, seed: *wave_seed }),
                _ => {}
            }
        }
        // The first weather of a sortie applies at once; turns wait for playback to reach their tick.
        let tick = view.render_tick;
        let mut i = 0;
        while i < self.queue.len() {
            let first = self.weather.is_none();
            let ahead = self.queue[i].tick as f64 - tick;
            if first || ahead <= 0.0 || ahead > TURN_HOLD_MAX_TICKS {
                let turn = self.queue.remove(i);
                let at = if first { now } else { (turn.tick as f64 / sim_core::TICK_HZ as f64).min(now) };
                self.take(turn, at, first);
            } else {
                i += 1;
            }
        }
        if let Some(t0) = self.wave_turn_s {
            let e = now - t0;
            self.waves = sim_core::world::weather::blended_waves(&self.wave_from, &self.wave_to, e, &self.tuning);
            if e >= sim_core::world::weather::WEATHER_BLEND_S { self.wave_turn_s = None; }
        }
        let blend = ((now - self.look_start_s) / LOOK_BLEND_S).clamp(0.0, 1.0);
        self.look = Look::lerp(self.look_from, self.look_to, smoothstep(0.0, 1.0, blend) as f32);

        let origin = [if view.origin.x.is_finite() { view.origin.x } else { 0.0 }, if view.origin.z.is_finite() { view.origin.z } else { 0.0 }];
        let camera = camera.filter(|c| c.iter().all(|v| v.is_finite())).unwrap_or([origin[0], 0.0, origin[1]]);
        let origins = snap_origins(&self.spec, camera[0], camera[2]);
        let level_y = self.dock.unwrap_or(0.0);
        let levels = origins.iter().map(|c| [(c[0] - origin[0]) as f32, level_y, (c[1] - origin[1]) as f32]).collect();

        let calm = WaveSet::calm();
        let set = if self.dock.is_some() { &calm } else { &self.waves };
        let uniforms = set.pack_uniforms(DVec3::new(origin[0], 0.0, origin[1]), now);
        let active = &set.waves[..(set.count as usize).min(sim_core::waves::MAX_WAVES)];
        let reach = active.iter().map(|w| w.amplitude.abs() * (1.0 + w.q.abs())).sum::<f64>();

        // The Nyquist rule, with hysteresis and a short ramp.
        let mut worst = 0.0f64;
        if self.dock.is_none() && !active.is_empty() {
            for v in &view.vessels {
                let at = [origin[0] + v.pos[0] as f64, origin[1] + v.pos[2] as f64];
                if !(at[0].is_finite() && at[1].is_finite()) { continue; }
                let d = DVec3::new(at[0] - camera[0], v.pos[1] as f64 - camera[1], at[1] - camera[2]).length();
                let distance = (d - VESSEL_PAD_M).max(1.0);
                let faded = faded_amplitude(set, spacing_under(&self.spec, &origins, at, VESSEL_PAD_M));
                worst = worst.max(faded / (NYQUIST_LIMIT * distance));
            }
        }
        self.fade_allowed = if self.fade_allowed { worst <= 1.0 } else { worst <= NYQUIST_RETURN };
        let step = if delta_s.is_finite() && delta_s > 0.0 { (delta_s / NYQUIST_RAMP_S) as f32 } else { 1.0 };
        let target = if self.fade_allowed { 1.0 } else { 0.0 };
        self.fade = if self.fade < target { (self.fade + step).min(target) } else { (self.fade - step).max(target) };

        let scales = crate::seams::gen::OCEAN_DETAIL_SCALES_M;
        let drift = self.wind_offset(now);
        let tile = 1.0 / CLOUD_SHADOW_TILE_M;
        OceanFrame {
            uniforms,
            levels,
            look: if self.dock.is_some() { Look::default() } else { self.look },
            nyquist_fade: self.fade,
            detail_phase: fract(now / crate::seams::gen::OCEAN_DETAIL_LOOP_S),
            detail_origin: [fract(origin[0] / scales[0]), fract(origin[1] / scales[0]), fract(origin[0] / scales[1]), fract(origin[1] / scales[1])],
            detail_tilings: if degrade_step >= ONE_TILING_FROM_STEP { 1 } else { 2 },
            detail_strength: if self.dock.is_some() { DOCK_DETAIL_STRENGTH } else { DETAIL_STRENGTH },
            // The pattern rides with the wind: a point's UV is (world - drift) / tile.
            cloud_xf: [fract((origin[0] - drift[0]) * tile), fract((origin[1] - drift[1]) * tile), tile as f32, if self.dock.is_some() { 0.0 } else { self.look.cloud_cover }],
            reach_m: reach as f32,
            camera_xz: [camera[0], camera[2]],
            origin,
            wind_dir: {
                let (x, z) = (self.wind_vel[0] as f32, self.wind_vel[1] as f32);
                let l = (x * x + z * z).sqrt();
                if l > 1.0e-3 && l.is_finite() { [x / l, z / l] } else { [0.0, 1.0] }
            },
            foam_streaks: true,
        }
    }

    /// Sea surface height at a map position: the simulation's `WaveSet::height_at`, or the dock's level.
    pub fn height_at(&self, x: f64, z: f64, t: f64) -> f64 {
        match self.dock { Some(level) => level as f64, None => self.waves.height_at(x, z, t) }
    }

    /// Ray against the displaced surface, world metres: the slab the waves live in is marched for the first
    /// crossing, which four Newton steps then refine.
    pub fn raycast(&self, origin: [f64; 3], dir: [f32; 3], t: f64) -> Option<[f64; 3]> {
        let o = DVec3::from(origin);
        let d = DVec3::new(dir[0] as f64, dir[1] as f64, dir[2] as f64);
        if !o.is_finite() || !d.is_finite() || d.length_squared() < 1e-18 || !t.is_finite() { return None; }
        let d = d.normalize();
        let level = self.dock.map_or(0.0, |l| l as f64);
        let active = &self.waves.waves[..(self.waves.count as usize).min(sim_core::waves::MAX_WAVES)];
        let amp: f64 = if self.dock.is_some() { 0.0 } else { active.iter().map(|w| w.amplitude.abs()).sum() };
        let on_surface = |s: f64| { let p = o + d * s; [p.x, self.height_at(p.x, p.z, t), p.z] };
        if amp <= 0.0 {
            if d.y >= -1e-9 || o.y < level { return if o.y < level { Some(on_surface(0.0)) } else { None }; }
            let s = (level - o.y) / d.y;
            return if s <= MAX_RAY_M { Some(on_surface(s)) } else { None };
        }
        let (top, bottom) = (level + amp, level - amp);
        let s_in = if o.y > top { if d.y >= -1e-9 { return None; } (top - o.y) / d.y } else { 0.0 };
        let s_out = if d.y < -1e-9 { (bottom - o.y) / d.y } else if d.y > 1e-9 { (top - o.y) / d.y } else { MAX_RAY_M };
        let s_out = s_out.min(MAX_RAY_M);
        if s_in.is_nan() || s_out.is_nan() || s_in > s_out { return None; }
        let f = |s: f64| { let p = o + d * s; p.y - self.height_at(p.x, p.z, t) };
        let mut a = s_in;
        let mut fa = f(a);
        // Already under the surface where the slab is entered (a camera in a trough wall): that is the hit.
        if fa <= 0.0 { return Some(on_surface(a)); }
        let step = (s_out - s_in) / RAY_MARCH_STEPS as f64;
        let mut bracket = None;
        for i in 1..=RAY_MARCH_STEPS {
            let b = if i == RAY_MARCH_STEPS { s_out } else { s_in + step * i as f64 };
            let fb = f(b);
            if fb <= 0.0 { bracket = Some((a, fa, b, fb)); break; }
            a = b;
            fa = fb;
        }
        let (a, fa, b, fb) = bracket?;
        // Secant start, then Newton with the surface slope; a step that leaves the bracket is bisected.
        let mut s = a + (b - a) * fa / (fa - fb).max(1e-12);
        let (mut lo, mut hi) = (a, b);
        for _ in 0..sim_core::waves::NEWTON_STEPS {
            let p = o + d * s;
            let sample = self.waves.sample(p.x, p.z, t);
            let fs = p.y - sample.height;
            if fs > 0.0 { lo = s; } else { hi = s; }
            let n = sample.normal;
            let (hx, hz) = if n.y > 1e-6 { (-n.x / n.y, -n.z / n.y) } else { (0.0, 0.0) };
            let slope = d.y - (hx * d.x + hz * d.z);
            let next = if slope.abs() > 1e-9 { s - fs / slope } else { f64::NAN };
            s = if next.is_finite() && next > lo && next < hi { next } else { 0.5 * (lo + hi) };
        }
        Some(on_surface(s))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::interp::{RenderOrigin, VesselPose};
    use content::weather::{SeaState, WeatherKind};
    use hull_mesh::QualityTier;
    use sim_core::waves::shader_port_displace;

    fn weather(sea: u8) -> WeatherState { WeatherState { kind: WeatherKind::Overcast, sea_state: SeaState(sea), wind_dir_rad: 0.7, wind_speed_mps: 10.0, cloud_cover: 0.6, rain: 0.2, ..Default::default() } }
    fn turn(tick: u64, sea: u8, seed: u64) -> WireEvent { WireEvent { tick, event: SimEvent::Phys(PhysEvent::WeatherChanged { weather: weather(sea), wave_seed: seed }) } }
    fn view(tick: f64, origin: [f64; 2]) -> ViewFrame { ViewFrame { render_tick: tick, sim_time_s: tick / sim_core::TICK_HZ as f64, origin: RenderOrigin { x: origin[0], z: origin[1] }, ..Default::default() } }
    fn core() -> OceanCore { OceanCore::new(ClipmapSpec::for_tier(QualityTier::Medium), WaveTuning::default()) }

    #[test]
    fn the_wave_set_is_the_simulations_and_changes_at_its_tick() {
        let mut c = core();
        let tuning = WaveTuning::default();
        // No weather yet: calm, and nothing panics.
        let f = c.step(&view(0.0, [0.0; 2]), &[], None, 0, 0.016);
        assert_eq!(f.uniforms.count, 0);
        assert_eq!(c.height_at(10.0, 20.0, 5.0), 0.0);
        // The first weather applies at once, whatever its tick.
        c.step(&view(60.0, [0.0; 2]), &[turn(0, 4, 77)], None, 0, 0.016);
        assert_eq!(c.waves, WaveSet::generate(77, &weather(4), &tuning));
        assert_eq!(c.wave_seed, 77);
        // A turn stamped ahead of playback waits for it.
        c.step(&view(100.0, [0.0; 2]), &[turn(110, 6, 77)], None, 0, 0.016);
        assert_eq!(c.waves, WaveSet::generate(77, &weather(4), &tuning));
        c.step(&view(109.9, [0.0; 2]), &[], None, 0, 0.016);
        assert_eq!(c.waves, WaveSet::generate(77, &weather(4), &tuning));
        // At the turn's tick the sea is still the old set, then turns over the simulation's blend time to the
        // new one through the same blend the simulation floats ships on.
        c.step(&view(110.0, [0.0; 2]), &[], None, 0, 0.016);
        let (from, to) = (WaveSet::generate(77, &weather(4), &tuning), WaveSet::generate(77, &weather(6), &tuning));
        assert_eq!(c.waves, from);
        let half = sim_core::world::weather::WEATHER_BLEND_S / 2.0;
        c.step(&view(110.0 + half * 60.0, [0.0; 2]), &[], None, 0, 0.016);
        assert_eq!(c.waves, sim_core::world::weather::blended_waves(&from, &to, half, &tuning));
        assert!(c.waves != from && c.waves != to, "half way through the turn");
        c.step(&view(110.0 + sim_core::world::weather::WEATHER_BLEND_S * 60.0, [0.0; 2]), &[], None, 0, 0.016);
        assert_eq!(c.waves, to);
        // The look eases.
        let f = c.step(&view(110.0 + 60.0 * 10.0, [0.0; 2]), &[], None, 0, 0.016);
        assert!(f.look.sea_state > 4.0 && f.look.sea_state < 6.0, "{}", f.look.sea_state);
        let f = c.step(&view(110.0 + 60.0 * 21.0, [0.0; 2]), &[], None, 0, 0.016);
        assert_eq!(f.look.sea_state, 6.0);
        // A new sortie starts calm.
        c.step(&view(0.0, [0.0; 2]), &[WireEvent { tick: 0, event: SimEvent::SortieStarted { seed: 1, tick_hz: 60 } }], None, 0, 0.016);
        assert_eq!(c.waves, WaveSet::calm());
    }

    #[test]
    fn packed_uniforms_draw_the_simulated_surface_far_from_the_origin_and_late() {
        // L1893: the f32 shader port within 2 mm of the f64 surface, 3 h in, 50 km out, 12 km from the origin.
        let mut c = core();
        let origin = [37_000.0, -49_000.0];
        let tick = 3.0 * 3600.0 * 60.0 + 17.3;
        let f = c.step(&view(tick, origin), &[turn(0, 6, 4242)], Some([origin[0] + 50.0, 30.0, origin[1] - 80.0]), 0, 0.016);
        assert_eq!(f.uniforms.count, 12);
        let t = tick / 60.0;
        for (i, p) in [[0.0f32, 0.0, 0.0], [130.5, 0.0, -77.25], [-4_000.0, 0.0, 3_900.0], [8_300.0, 0.0, -8_600.0]].iter().enumerate() {
            // The shader evaluates at TIME == t_ref, which is what the engine side arranges.
            let drawn = shader_port_displace(&f.uniforms, *p, f.uniforms.t_ref);
            let sim = c.waves.displace(origin[0] + p[0] as f64, origin[1] + p[2] as f64, t);
            let err = ((drawn[0] as f64 + origin[0] - sim.x).powi(2) + (drawn[1] as f64 - sim.y).powi(2) + (drawn[2] as f64 + origin[1] - sim.z).powi(2)).sqrt();
            assert!(err < 0.002, "point {i}: {err} m");
        }
    }

    #[test]
    fn height_and_raycast_agree_with_the_surface() {
        let mut c = core();
        c.step(&view(600.0, [0.0; 2]), &[turn(0, 5, 9)], None, 0, 0.016);
        let t = 10.0;
        // Newton height: the displaced image of a grid point lies on the surface the query reports (L1893).
        for (x0, z0) in [(0.0, 0.0), (13.0, -40.0), (250.0, 91.0), (-1_000.5, 777.0)] {
            let p = c.waves.displace(x0, z0, t);
            assert!((c.height_at(p.x, p.z, t) - p.y).abs() < 0.001);
        }
        // Rays from a mast-top camera at several depressions and bearings.
        let o = [5.0, 30.0, -8.0];
        for (i, dir) in [[0.0f32, -1.0, 0.0], [0.6, -0.8, 0.0], [0.9, -0.2, 0.39], [-0.7, -0.05, 0.71], [0.3, -0.012, -0.95]].iter().enumerate() {
            let hit = c.raycast(o, *dir, t).unwrap_or_else(|| panic!("ray {i} missed"));
            assert!((hit[1] - c.height_at(hit[0], hit[2], t)).abs() < 1e-9);
            // The hit lies on the ray.
            let d = DVec3::new(dir[0] as f64, dir[1] as f64, dir[2] as f64).normalize();
            let v = DVec3::from(hit) - DVec3::from(o);
            let off = (v - d * v.dot(d)).length();
            assert!(off < 0.02, "ray {i}: {off} m off the ray");
            // And it is the first crossing: the ray is above the water all the way there.
            let s_hit = v.dot(d);
            for k in 1..50 {
                let p = DVec3::from(o) + d * (s_hit * k as f64 / 50.0 * 0.999);
                assert!(p.y - c.height_at(p.x, p.z, t) > -0.25, "ray {i} passed under the surface before its hit");
            }
        }
        // Above the horizon, bad input, and the flat dock.
        assert!(c.raycast(o, [1.0, 0.2, 0.0], t).is_none());
        assert!(c.raycast(o, [0.0, 0.0, 0.0], t).is_none());
        assert!(c.raycast([f64::NAN, 1.0, 0.0], [0.0, -1.0, 0.0], t).is_none());
        c.dock = Some(-1.5);
        assert_eq!(c.height_at(3.0, 4.0, t), -1.5);
        let hit = c.raycast([0.0, 8.5, 0.0], [1.0, -1.0, 0.0], t).unwrap();
        assert!((hit[0] - 10.0).abs() < 1e-6 && hit[1] == -1.5);
        let f = c.step(&view(600.0, [0.0; 2]), &[], None, 0, 0.016);
        assert_eq!(f.uniforms.count, 0);
        assert_eq!(f.levels[0][1], -1.5);
        assert_eq!((f.detail_strength, f.cloud_xf[3], f.reach_m), (DOCK_DETAIL_STRENGTH, 0.0, 0.0));
    }

    #[test]
    fn the_clipmap_follows_the_camera_relative_to_the_origin() {
        let mut c = core();
        let origin = [12_000.0, -3_000.0];
        let cam = [12_345.6, 40.0, -2_987.3];
        let f = c.step(&view(0.0, origin), &[], Some(cam), 0, 0.016);
        assert_eq!(f.levels.len(), c.spec.levels as usize);
        let want = snap_origins(&c.spec, cam[0], cam[2]);
        for (l, p) in f.levels.iter().enumerate() {
            assert_eq!(*p, [(want[l][0] - origin[0]) as f32, 0.0, (want[l][1] - origin[1]) as f32]);
            // Each level's centre is within two of its cells of the camera.
            let s = c.spec.spacing(l as u8);
            assert!((want[l][0] - cam[0]).abs() <= 2.0 * s && (want[l][1] - cam[2]).abs() <= 2.0 * s);
        }
        // Detail and cloud UVs are continuous across a rebase: the same world point reads the same texel.
        let world = [12_500.0, -2_900.0];
        let uv = |f: &OceanFrame, o: [f64; 2]| {
            let a = (world[0] - o[0]) / 41.0 + f.detail_origin[0] as f64;
            let b = (world[0] - o[0]) / CLOUD_SHADOW_TILE_M + f.cloud_xf[0] as f64;
            (a - a.floor(), b - b.floor())
        };
        let other = [16_000.0, -7_000.0];
        let g = c.step(&view(0.0, other), &[], Some(cam), 0, 0.016);
        let (u0, u1) = (uv(&f, origin), uv(&g, other));
        let wrap = |d: f64| d.abs().min(1.0 - d.abs());
        assert!(wrap(u0.0 - u1.0) < 1e-4 && wrap(u0.1 - u1.1) < 1e-4, "{u0:?} {u1:?}");
        assert_eq!(c.step(&view(0.0, origin), &[], Some(cam), 6, 0.016).detail_tilings, 1);
    }

    #[test]
    fn the_nyquist_fade_follows_the_amplitude_under_ships() {
        let spec = ClipmapSpec::for_tier(QualityTier::Low);
        let origins = snap_origins(&spec, 0.0, 0.0);
        // Under the camera the finest level draws; far out, coarser ones.
        assert_eq!(spacing_under(&spec, &origins, [0.0, 0.0], 5.0), 1.0);
        assert!(spacing_under(&spec, &origins, [600.0, 0.0], 5.0) >= 16.0);
        assert!(spacing_under(&spec, &origins, [1.0e6, 0.0], 5.0) >= spec.spacing(spec.levels - 1));
        let waves = WaveSet::generate(5, &weather(5), &WaveTuning::default());
        // Nothing fades on a 1 m grid (the shortest wave is 8 m); everything fades on a 512 m grid.
        assert_eq!(faded_amplitude(&waves, 1.0), 0.0);
        let all: f64 = waves.waves[..waves.count as usize].iter().map(|w| w.amplitude).sum();
        assert!((faded_amplitude(&waves, 512.0) - all).abs() < 1e-9);

        let mut c = OceanCore::new(spec, WaveTuning::default());
        let ship = |x: f32| VesselPose { pos: [x, 0.0, 0.0], rot: [0.0, 0.0, 0.0, 1.0], ..Default::default() };
        let mut v = view(0.0, [0.0; 2]);
        // No vessel: the fade is allowed.
        let f = c.step(&v, &[turn(0, 5, 5)], Some([0.0, 20.0, 0.0]), 0, 0.016);
        assert_eq!(f.nyquist_fade, 1.0);
        // A ship 900 m off in a rough sea sits on a coarse level: more than 0.9 m would be faded there.
        v.vessels = vec![ship(900.0)];
        let need = faded_amplitude(&c.waves, spacing_under(&c.spec, &snap_origins(&c.spec, 0.0, 0.0), [900.0, 0.0], VESSEL_PAD_M));
        assert!(need > NYQUIST_LIMIT * 900.0, "{need}");
        let mut last = 1.0;
        for _ in 0..60 { let f = c.step(&v, &[], Some([0.0, 20.0, 0.0]), 0, 0.016); assert!(f.nyquist_fade <= last); last = f.nyquist_fade; }
        assert_eq!(last, 0.0);
        // The ship goes; the fade comes back.
        v.vessels.clear();
        for _ in 0..60 { last = c.step(&v, &[], Some([0.0, 20.0, 0.0]), 0, 0.016).nyquist_fade; }
        assert_eq!(last, 1.0);
        // A flat calm never needs the fade off.
        let mut calm = OceanCore::new(spec, WaveTuning::default());
        v.vessels = vec![ship(900.0)];
        assert_eq!(calm.step(&v, &[], Some([0.0, 20.0, 0.0]), 0, 0.016).nyquist_fade, 1.0);
    }

    #[test]
    fn cloud_shadows_drift_downwind_and_bad_input_is_harmless() {
        let mut c = core();
        let mut w = weather(3);
        w.wind_dir_rad = std::f64::consts::FRAC_PI_2; // blowing toward east (+X)
        w.wind_speed_mps = 12.0;
        let ev = [WireEvent { tick: 0, event: SimEvent::Phys(PhysEvent::WeatherChanged { weather: w, wave_seed: 3 }) }];
        let a = c.step(&view(0.0, [0.0; 2]), &ev, None, 0, 0.016);
        let b = c.step(&view(60.0 * 100.0, [0.0; 2]), &[], None, 0, 0.016);
        // 100 s at 12 m/s east: the pattern moved 1200 m east, so the origin's UV moved 0.2 tiles west.
        let du = (a.cloud_xf[0] - b.cloud_xf[0]).rem_euclid(1.0);
        assert!((du - 0.2).abs() < 1e-4, "{du}");
        assert!((a.cloud_xf[1] - b.cloud_xf[1]).abs() < 1e-5);
        assert_eq!(b.cloud_xf[3], 0.6);
        // Non-finite views and cameras do not poison the frame.
        let mut bad = view(f64::NAN, [f64::NAN, f64::INFINITY]);
        bad.sim_time_s = f64::NAN;
        bad.vessels = vec![VesselPose { pos: [f32::NAN; 3], ..Default::default() }];
        let f = c.step(&bad, &[], Some([f64::NAN; 3]), 0, f64::NAN);
        assert!(f.levels.iter().flatten().all(|v| v.is_finite()));
        assert!(f.uniforms.mats.iter().flatten().all(|v| v.is_finite()) && f.cloud_xf.iter().all(|v| v.is_finite()) && f.nyquist_fade.is_finite());
    }

    /// The unit's share of the frame is 0.1 ms with the LUT and the emits (L2009); the decisions here must
    /// leave nearly all of it to the engine calls.
    #[test]
    fn a_frame_with_thirty_ships_costs_microseconds() {
        let mut c = core();
        let mut v = view(0.0, [3_000.0, 1_000.0]);
        v.vessels = (0..30).map(|i| VesselPose { pos: [i as f32 * 211.0 - 3_000.0, 0.0, (i * i) as f32 * 9.0 - 2_000.0], rot: [0.0, 0.0, 0.0, 1.0], ..Default::default() }).collect();
        c.step(&v, &[turn(0, 6, 9)], Some([3_010.0, 30.0, 990.0]), 0, 0.016);
        // The best of several batches: one scheduler stall on a shared runner must not fail the build.
        let (batches, frames) = (8, 250);
        let mut per_frame_us = f64::MAX;
        for b in 0..batches {
            let start = std::time::Instant::now();
            for i in 0..frames {
                let tick = (b * frames + i) as f64;
                v.render_tick = tick;
                v.sim_time_s = tick / 60.0;
                std::hint::black_box(c.step(&v, &[], Some([3_010.0 + tick * 0.1, 30.0, 990.0]), 0, 0.016));
            }
            per_frame_us = per_frame_us.min(start.elapsed().as_secs_f64() * 1e6 / frames as f64);
        }
        assert!(per_frame_us < 50.0, "{per_frame_us} us per frame");
    }
}
