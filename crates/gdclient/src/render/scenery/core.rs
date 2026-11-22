//! Part of unit `gd-render-scenery`. Everything of the scenery with no Godot in it: the environment state and
//! its blends, the synced wind drift, the cloud field, lightning, the colour-grading LUT blend, and the
//! island tiles with their meshing thread. `gpu.rs` uploads what this decides. Tests run headless.
//!
//! # Weather and blends (L1855-1875)
//! `WeatherChanged` is applied when playback reaches its tick. The first one of a sortie is taken as is; a
//! later one starts a blend from the state shown at that moment, lasting 20 s plus up to 40 s more the
//! further visibility moves (a change from clear to a fog bank takes the full 60 s). Blends run on
//! simulation time, so a paused game holds them. Fog density never goes below the simulation's own
//! `3.912 / V`: a thickening blend jumps to the new density at once and only clearing is gradual, so the
//! render never out-sees the sim (L1857). Aerial perspective blends by the table and is held to 0.3 or
//! less whenever density is 5e-4 or more (the CI lint).
//!
//! # Clouds (L1915, G5)
//! The layout is `cloud_layout(seed, cover, storm, R)` with the sortie's server seed and `R` inside the
//! visibility (85% of V, 2 km to 18 km). It drifts by the wind offset, which is a closed function of
//! simulation time and the replicated weather events (`base + wind x (t - t_change)`), so every client draws
//! the same sky. The six horizon masses (the G5 ring, `cloud_layout`'s first entries) stand round the
//! camera at their layout bearing and range, so no 90 degree sector of horizon is empty wherever the sortie
//! goes; at 85 to 100% of R their parallax and drift would not show anyway. The interior clusters drift
//! and repeat with period `2R`, each drawn at its copy nearest the camera, so a cluster that wraps does so
//! at the edge of visibility. A new layout (cover or storm changed) is built when its weather turn starts.
//! Cloud transforms step at most once a second.
//!
//! # Islands (L1917)
//! The map is cut into [`TILE_M`] tiles; every tile some island's land reaches gets a far mesh (32 m
//! triangles) and, within [`NEAR_TILE_M`] of the camera, a near mesh (8 m). Meshes are built on a thread
//! of the unit (`bp-scenery`) and uploaded a few per frame; until a near mesh lands the far one stays.
// Client rendering, not simulation (clippy.toml): std floats are allowed here. Nothing drawn from them feeds
// back into the sim, and a last-bit difference between platforms moves nothing on screen.
#![allow(clippy::disallowed_methods)]
use super::super::FrameContext;
use super::{EnvironmentState, Lightning};
use content::campaign::MapDef;
use content::enums::TimeOfDay;
use content::weather::{WeatherKind, WeatherState};
use hull_mesh::env::scenery::{CloudKind, IslandBands};
use sim_core::events::PhysEvent;
use sim_systems::protocol::SimEvent;
use std::collections::BTreeMap;
use std::sync::mpsc;

/// Koschmieder (L1857).
pub const KOSCHMIEDER: f64 = 3.912;
/// Blend length range, seconds (L1870).
pub const BLEND_S: (f64, f64) = (20.0, 60.0);
/// Visibility ratio that takes the longest blend: clear (30 km) to a fog bank (400 m).
const BLEND_FULL_RATIO: f64 = 75.0;
/// Aerial perspective ceiling at dense fog (the lint: above 0.3 at density 5e-4 or more fails).
pub const AERIAL_DENSE_CAP: f32 = 0.3;
pub const AERIAL_DENSE_FROM: f32 = 5.0e-4;
/// Cloud field radius as a share of visibility, and its bounds.
pub const CLOUD_RADIUS_OF_V: f64 = 0.85;
pub const CLOUD_RADIUS_M: (f64, f64) = (2_000.0, 18_000.0);
/// The sky shader's cirrus offset steps this often (sim seconds, L1915: at most every 2 s).
pub const CIRRUS_STEP_S: f64 = 2.0;
/// Cloud transforms step at most this often (sim seconds).
pub const CLOUD_STEP_S: f64 = 1.0;
/// Lightning (L1875): pulses fall in windows of this length, at most one per window and only in its first
/// half, so two pulses are always at least 2 s apart; this share of windows has one.
pub const LIGHTNING_WINDOW_S: f64 = 4.0;
pub const LIGHTNING_CHANCE: f32 = 0.55;
pub const LIGHTNING_S: (f32, f32) = (0.08, 0.15);
/// Island tiles.
pub const TILE_M: f64 = 512.0;
pub const FAR_EDGE_M: f64 = 32.0;
pub const NEAR_EDGE_M: f64 = 8.0;
/// A tile whose centre is within this of the camera draws its near mesh; it falls back past 15% more.
pub const NEAR_TILE_M: f64 = 1_200.0;
pub const NEAR_HYSTERESIS: f64 = 1.15;
/// Near meshes farther than this are freed.
pub const NEAR_FREE_M: f64 = 2.0 * NEAR_TILE_M;
/// Mesh uploads per frame (islands and clouds together).
pub const UPLOADS_PER_FRAME: usize = 3;
/// The land of an island reaches at most this share of its radius past it (shoreline harmonics within 1.16,
/// L-terrain docs), plus this margin.
const LAND_REACH: f64 = 1.16;
const LAND_MARGIN_M: f64 = 64.0;

/// Time of day from the sun's elevation (L1867): night below 3 degrees, dusk to under 25, day above.
pub fn time_of_day(sun_elevation_deg: f64) -> TimeOfDay {
    if sun_elevation_deg.is_nan() || sun_elevation_deg < 3.0 { TimeOfDay::Night } else if sun_elevation_deg < 25.0 { TimeOfDay::Dusk } else { TimeOfDay::Day }
}
/// Unit vector toward the sun: elevation above the horizon, azimuth compass degrees (0 = north = -Z, clockwise).
pub fn sun_dir(elevation_deg: f64, azimuth_deg: f64) -> [f32; 3] {
    let (e, a) = (elevation_deg.to_radians(), azimuth_deg.to_radians());
    if !(e.is_finite() && a.is_finite()) { return [0.0, 1.0, 0.0]; }
    [(a.sin() * e.cos()) as f32, e.sin() as f32, (-a.cos() * e.cos()) as f32]
}
/// Aerial perspective by weather (L1859-1866).
pub fn aerial_perspective(kind: WeatherKind) -> f32 {
    match kind { WeatherKind::Clear => 0.7, WeatherKind::Haze | WeatherKind::Overcast => 0.5, WeatherKind::Rain | WeatherKind::Storm => 0.2, WeatherKind::FogBank => 0.0 }
}
fn smoothstep(e0: f64, e1: f64, x: f64) -> f64 { let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0); t * t * (3.0 - 2.0 * t) }
/// `fog_sun_scatter` = 0.2 (1 - smoothstep(10, 25 degrees, elevation)) (L1872).
pub fn fog_sun_scatter(elevation_deg: f64) -> f32 { (0.2 * (1.0 - smoothstep(10.0, 25.0, elevation_deg))) as f32 }
fn fog_density(v_m: f64) -> f64 { KOSCHMIEDER / if v_m.is_finite() { v_m.clamp(50.0, 1.0e6) } else { 30_000.0 } }
/// Blend length for a change of visibility.
pub fn blend_len_s(v_from: f64, v_to: f64) -> f64 {
    let (a, b) = (v_from.clamp(50.0, 1.0e6), v_to.clamp(50.0, 1.0e6));
    if !(a.is_finite() && b.is_finite()) { return BLEND_S.0; }
    let f = ((a / b).ln().abs() / BLEND_FULL_RATIO.ln()).clamp(0.0, 1.0);
    BLEND_S.0 + (BLEND_S.1 - BLEND_S.0) * f
}

fn hash64(a: u64, b: u64) -> u64 {
    let mut h = a ^ b.wrapping_mul(0x9E37_79B9_7F4A_7C15);
    h ^= h >> 33;
    h = h.wrapping_mul(0xFF51_AFD7_ED55_8CCD);
    h ^= h >> 33;
    h = h.wrapping_mul(0xC4CE_B9FE_1A85_EC53);
    h ^ (h >> 33)
}
fn unit64(h: u64) -> f64 { (h >> 11) as f64 / (1u64 << 53) as f64 }

/// The lightning pulse that starts in `(t0, t1]`, if any. Deterministic in the sortie seed and the window.
pub fn lightning_between(seed: u64, t0: f64, t1: f64) -> Option<Lightning> {
    if !(t0.is_finite() && t1.is_finite()) || t1 <= t0 || t1 - t0 > 2.0 * LIGHTNING_WINDOW_S { return None; }
    let w0 = (t0 / LIGHTNING_WINDOW_S).floor() as i64;
    let w1 = (t1 / LIGHTNING_WINDOW_S).floor() as i64;
    for w in w0..=w1 {
        let h = hash64(seed ^ 0x11E7_4A1D, w as u64);
        if (unit64(h) as f32) >= LIGHTNING_CHANCE { continue; }
        let at = (w as f64 + 0.5 * unit64(hash64(h, 1))) * LIGHTNING_WINDOW_S;
        if at > t0 && at <= t1 {
            let duration_s = LIGHTNING_S.0 + (LIGHTNING_S.1 - LIGHTNING_S.0) * unit64(hash64(h, 2)) as f32;
            return Some(Lightning { intensity: 0.4 + 0.6 * unit64(hash64(h, 3)) as f32, duration_s, bearing_deg: 360.0 * unit64(hash64(h, 4)) as f32 });
        }
    }
    None
}

/// The G5 horizon ring: the first entries of `cloud_layout` (hull-mesh `env::scenery` docs: "a fixed horizon
/// ring of six masses").
pub const HORIZON_RING: usize = 6;

/// One cloud cluster of the layout.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Cloud { pub kind: CloudKind, pub seed: u64, pub size_m: f32, pub pos: [f64; 3], pub ring: bool }

/// The presets to draw: `(from, to, blend)` of (weather, time) pairs.
pub type PresetPair = ((WeatherKind, TimeOfDay), (WeatherKind, TimeOfDay), f32);

// ---------------------------------------------------------------------------------------------------------
// Island tiles and the meshing thread
// ---------------------------------------------------------------------------------------------------------

/// A tile by index (world corner = index x `TILE_M`).
pub type TileKey = (i32, i32);

pub enum Job { Island { generation: u32, key: TileKey, near: bool }, Cloud { generation: u32, index: usize, kind: CloudKind, seed: u64, size_m: f32 } }
pub enum Built { Island { generation: u32, key: TileKey, near: bool, mesh: crate::seams::MeshData }, Cloud { generation: u32, index: usize, mesh: crate::seams::MeshData } }

/// What the worker needs to mesh islands.
#[derive(Clone)]
pub struct IslandSource { pub terrain: std::sync::Arc<sim_core::terrain::Terrain>, pub bands: IslandBands }

/// Build one job (on the worker, or inline when there is none).
pub fn build(job: &Job, source: Option<&IslandSource>) -> Option<Built> {
    match job {
        Job::Island { generation, key, near } => {
            let s = source?;
            let origin = [key.0 as f64 * TILE_M, key.1 as f64 * TILE_M];
            let t = s.terrain.clone();
            let mesh = hull_mesh::env::scenery::island_mesh(&move |x, z| t.height_at(x, z), origin, [0.0, 0.0], [TILE_M, TILE_M], if *near { NEAR_EDGE_M } else { FAR_EDGE_M }, &s.bands);
            Some(Built::Island { generation: *generation, key: *key, near: *near, mesh })
        }
        Job::Cloud { generation, index, kind, seed, size_m } => Some(Built::Cloud { generation: *generation, index: *index, mesh: hull_mesh::env::scenery::cloud_cluster(*kind, *seed, *size_m) }),
    }
}

/// The unit's thread: islands and clouds are meshed off the main thread.
pub struct Worker { jobs: Option<mpsc::Sender<(Job, Option<IslandSource>)>>, done: Option<mpsc::Receiver<Built>>, thread: Option<std::thread::JoinHandle<()>>, backlog: std::collections::VecDeque<(Job, Option<IslandSource>)> }
impl Worker {
    pub fn spawn() -> Self {
        let (tx, rx) = mpsc::channel::<(Job, Option<IslandSource>)>();
        let (dtx, drx) = mpsc::channel::<Built>();
        let thread = std::thread::Builder::new().name("bp-scenery".into()).spawn(move || {
            while let Ok((job, source)) = rx.recv() {
                if let Some(b) = build(&job, source.as_ref()) { if dtx.send(b).is_err() { break; } }
            }
        }).ok();
        match thread {
            Some(t) => Self { jobs: Some(tx), done: Some(drx), thread: Some(t), backlog: Default::default() },
            None => Self::inline(),
        }
    }
    /// No thread: jobs are built on the caller, one per poll (tests, a thread that could not start).
    pub fn inline() -> Self { Self { jobs: None, done: None, thread: None, backlog: Default::default() } }
    pub fn send(&mut self, job: Job, source: Option<IslandSource>) {
        let unsent = match &self.jobs { Some(tx) => tx.send((job, source)).err().map(|e| e.0), None => Some((job, source)) };
        if let Some(j) = unsent { self.jobs = None; self.backlog.push_back(j); }
    }
    pub fn poll(&mut self, out: &mut Vec<Built>) {
        if let Some(rx) = &self.done { while let Ok(b) = rx.try_recv() { out.push(b); } }
        if let Some((job, source)) = self.backlog.pop_front() { out.extend(build(&job, source.as_ref())); }
    }
    pub fn stop(&mut self) {
        // Results first: the thread's next send fails and it stops without working through its queue.
        self.done = None;
        self.jobs = None;
        if let Some(t) = self.thread.take() { let _ = t.join(); }
        self.backlog.clear();
    }
}
impl Drop for Worker { fn drop(&mut self) { self.stop(); } }

/// One island tile's meshes as the main thread knows them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TileState { pub far_built: bool, pub near_wanted: bool, pub near_pending: bool, pub near_built: bool, pub show_near: bool }

/// The tiles some island's land reaches.
pub fn land_tiles(map: &MapDef) -> Vec<TileKey> {
    let mut out = std::collections::BTreeSet::new();
    let half = content::campaign::MAX_MAP_M / 2.0;
    for isl in &map.islands {
        let (c, r) = (isl.centre_m, isl.radius_m);
        if !(c[0].is_finite() && c[1].is_finite() && r.is_finite() && r > 0.0 && isl.height_m.is_finite() && isl.height_m > -1.0) { continue; }
        let reach = (r * LAND_REACH + LAND_MARGIN_M).min(half);
        let lo = |v: f64| ((v - reach).max(-half) / TILE_M).floor() as i32;
        let hi = |v: f64| ((v + reach).min(half) / TILE_M).floor() as i32;
        for tz in lo(c[1])..=hi(c[1]) {
            for tx in lo(c[0])..=hi(c[0]) {
                // Keep the tiles the land's circle touches.
                let (x0, z0) = (tx as f64 * TILE_M, tz as f64 * TILE_M);
                let nx = c[0].clamp(x0, x0 + TILE_M);
                let nz = c[1].clamp(z0, z0 + TILE_M);
                if (nx - c[0]).powi(2) + (nz - c[1]).powi(2) <= reach * reach { out.insert((tx, tz)); }
            }
        }
    }
    out.into_iter().collect()
}

// ---------------------------------------------------------------------------------------------------------
// The core
// ---------------------------------------------------------------------------------------------------------

/// What changed this frame, for the engine side.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Changes { pub env: bool, pub layout: bool, pub cloud_xforms: bool, pub lut: bool, pub islands_moved: bool, pub lightning: Option<Lightning> }

pub struct SceneryCore {
    pub env: EnvironmentState,
    pub rev: u64,
    /// The weather as the sim has it, and what the blend started from.
    target: Option<WeatherState>,
    from: WeatherState,
    from_density: f64,
    from_aerial: f32,
    blend_start_s: f64,
    blend_len_s: f64,
    wind_base: [f64; 2],
    wind_t0: f64,
    wind_vel: [f64; 2],
    queue: Vec<crate::seams::WireEvent>,
    last_s: Option<f64>,
    last_rev_s: i64,
    last_cloud_s: f64,
    /// Cloud field, its period radius and its generation.
    pub clouds: Vec<Cloud>,
    pub cloud_radius_m: f64,
    pub cloud_generation: u32,
    /// Drawn position of each cloud in world metres (x, base altitude, z), refreshed at `CLOUD_STEP_S`.
    pub cloud_world: Vec<[f64; 3]>,
    layout_key: Option<(u64, u64, bool, u64)>,
    /// Island tiles.
    pub tiles: BTreeMap<TileKey, TileState>,
    pub island_generation: u32,
    pub source: Option<IslandSource>,
    pub high_lut: bool,
    /// Lightning toggle (`graphics.lightning`).
    pub lightning_on: bool,
    /// Wall-clock seconds (`FrameContext::delta_s` summed): lightning is spaced in the time the viewer sees,
    /// so time compression cannot bring pulses closer than 2 s (L1875, L1717).
    pub real_s: f64,
    last_cirrus: i64,
}

impl Default for SceneryCore { fn default() -> Self { Self::new() } }

impl SceneryCore {
    pub fn new() -> Self {
        Self {
            env: EnvironmentState::default(), rev: 0, target: None, from: WeatherState::default(), from_density: 0.0, from_aerial: 0.0, blend_start_s: 0.0, blend_len_s: 0.0,
            wind_base: [0.0; 2], wind_t0: 0.0, wind_vel: [0.0; 2], queue: Vec::new(), last_s: None, last_rev_s: i64::MIN, last_cloud_s: f64::NEG_INFINITY,
            clouds: Vec::new(), cloud_radius_m: CLOUD_RADIUS_M.1, cloud_generation: 0, cloud_world: Vec::new(), layout_key: None, tiles: BTreeMap::new(), island_generation: 0, source: None,
            high_lut: false, lightning_on: true, real_s: 0.0, last_cirrus: i64::MIN,
        }
    }

    /// Install a sortie map: the tiles to mesh (far meshes are all queued at once).
    pub fn set_map(&mut self, map: &MapDef, bands: IslandBands, worker: &mut Worker) {
        self.island_generation = self.island_generation.wrapping_add(1);
        self.tiles.clear();
        let source = IslandSource { terrain: std::sync::Arc::new(sim_core::terrain::Terrain::new(map.clone())), bands };
        for key in land_tiles(map) {
            self.tiles.insert(key, TileState::default());
            worker.send(Job::Island { generation: self.island_generation, key, near: false }, Some(source.clone()));
        }
        self.source = Some(source);
    }

    pub fn ground_height(&self, x: f64, z: f64) -> f32 {
        match &self.source { Some(s) => s.terrain.height_at(x, z) as f32, None => -(sim_core::terrain::DEFAULT_DEPTH_M as f32) }
    }

    fn reset(&mut self, seed: u64) {
        self.target = None;
        self.queue.clear();
        self.env.cloud_seed = seed;
        self.wind_base = [0.0; 2];
        self.wind_t0 = 0.0;
        self.wind_vel = [0.0; 2];
        self.last_rev_s = i64::MIN;
        self.layout_key = None;
    }

    fn wind_offset(&self, t: f64) -> [f64; 2] {
        let dt = (t - self.wind_t0).max(0.0);
        [self.wind_base[0] + self.wind_vel[0] * dt, self.wind_base[1] + self.wind_vel[1] * dt]
    }

    /// Weather taken from an event at sim time `at_s`.
    fn take_weather(&mut self, w: WeatherState, at_s: f64) {
        let speed = if w.wind_speed_mps.is_finite() { w.wind_speed_mps.clamp(0.0, 80.0) } else { 0.0 };
        let dir = if w.wind_dir_rad.is_finite() { w.wind_dir_rad } else { 0.0 };
        self.wind_base = self.wind_offset(at_s);
        self.wind_t0 = at_s;
        self.wind_vel = [dir.sin() * speed, -dir.cos() * speed];
        match self.target {
            None => {
                self.from = w;
                self.from_density = fog_density(w.visibility_m);
                self.from_aerial = aerial_perspective(w.kind);
                self.blend_len_s = 0.0;
            }
            Some(_) => {
                // Blend from what is on screen now.
                self.from_density = self.env.fog_density as f64;
                self.from_aerial = self.env.aerial_perspective;
                self.from = self.env.weather;
                self.from.kind = self.env.blend_to;
                self.blend_len_s = blend_len_s(self.from.visibility_m, w.visibility_m);
            }
        }
        self.blend_start_s = at_s;
        self.target = Some(w);
    }

    /// One frame. Returns what changed.
    pub fn step(&mut self, ctx: &FrameContext) -> Changes {
        let mut ch = Changes::default();
        let view = ctx.view;
        let now = view.sim_time_s;
        let r0 = self.real_s;
        if ctx.delta_s.is_finite() { self.real_s += ctx.delta_s.clamp(0.0, 1.0); }
        if self.last_s.is_some_and(|l| now + 1e-6 < l) {
            // A rewound timeline (replay seek): pending turns belong to the old one.
            self.queue.clear();
        }
        self.last_s = Some(now);
        let high = ctx.quality.tier == crate::enums::QualityTier::High;
        if high != self.high_lut { self.high_lut = high; ch.lut = true; }
        if view.origin_shift.is_some() { ch.islands_moved = true; ch.cloud_xforms = true; }
        for ev in ctx.events {
            match &ev.event {
                SimEvent::SortieStarted { seed, .. } => self.reset(*seed),
                SimEvent::Phys(PhysEvent::WeatherChanged { .. }) => self.queue.push(ev.clone()),
                _ => {}
            }
        }
        // The first weather of a sortie applies at once; turns wait for playback.
        let tick = view.render_tick;
        let mut due = Vec::new();
        let first = self.target.is_none();
        self.queue.retain(|e| {
            let ahead = e.tick as f64 - tick;
            if first || ahead <= 0.0 || ahead > super::super::vfx::core::EVENT_HOLD_MAX_TICKS { due.push(e.clone()); false } else { true }
        });
        for ev in due {
            if let SimEvent::Phys(PhysEvent::WeatherChanged { weather, .. }) = ev.event {
                let at = if first { now } else { (ev.tick as f64 / sim_core::TICK_HZ as f64).min(now) };
                self.take_weather(weather, at);
                ch.env = true;
                ch.lut = true;
            }
        }
        let Some(target) = self.target else { return ch };

        // Blend state.
        let blend = if self.blend_len_s > 0.0 { ((now - self.blend_start_s) / self.blend_len_s).clamp(0.0, 1.0) } else { 1.0 };
        let sim_density = fog_density(target.visibility_m);
        let blended_density = (self.from_density.ln() + (sim_density.ln() - self.from_density.ln()) * blend).exp();
        let density = blended_density.max(sim_density);
        let mut aerial = self.from_aerial + (aerial_perspective(target.kind) - self.from_aerial) * blend as f32;
        if density as f32 >= AERIAL_DENSE_FROM { aerial = aerial.min(AERIAL_DENSE_CAP); }
        let offset = self.wind_offset(now);
        let env = EnvironmentState {
            weather: target,
            blend_from: if blend >= 1.0 { target.kind } else { self.from.kind },
            blend_to: target.kind,
            blend: blend as f32,
            sun_dir: sun_dir(target.sun_elevation_deg, target.sun_azimuth_deg),
            fog_density: density as f32,
            fog_sun_scatter: fog_sun_scatter(target.sun_elevation_deg),
            aerial_perspective: aerial.clamp(0.0, 1.0),
            wind_offset_m: [offset[0] as f32, offset[1] as f32],
            cloud_seed: self.env.cloud_seed,
        };
        // Re-apply on a turn, then once a second while it blends (L1846).
        let second = now.floor() as i64;
        let blending = blend < 1.0;
        // The sky's cirrus layer steps with the wind drift at most every 2 s (L1915): re-apply on each step.
        let cirrus = (now / CIRRUS_STEP_S).floor() as i64;
        let cirrus_step = cirrus != self.last_cirrus && self.wind_vel != [0.0, 0.0];
        self.last_cirrus = cirrus;
        if ch.env || cirrus_step || (blending && second != self.last_rev_s) || (!blending && self.env.blend < 1.0) {
            self.last_rev_s = second;
            ch.env = true;
            ch.lut |= blending || self.env.blend < 1.0;
            self.rev += 1;
        }
        self.env = env;

        // Clouds: the layout for this weather, transforms at 1 Hz.
        let radius = (CLOUD_RADIUS_OF_V * target.visibility_m.max(0.0)).clamp(CLOUD_RADIUS_M.0, CLOUD_RADIUS_M.1);
        let cover = if target.cloud_cover.is_finite() { target.cloud_cover.clamp(0.0, 1.0) } else { 0.0 };
        let key = (self.env.cloud_seed, (cover * 1000.0).round() as u64, target.kind == WeatherKind::Storm, radius.round() as u64);
        if self.layout_key != Some(key) {
            self.layout_key = Some(key);
            self.cloud_radius_m = radius;
            self.cloud_generation = self.cloud_generation.wrapping_add(1);
            self.clouds = hull_mesh::env::scenery::cloud_layout(self.env.cloud_seed, cover, target.kind == WeatherKind::Storm, radius).into_iter().enumerate().map(|(i, (kind, seed, size_m, pos))| Cloud { kind, seed, size_m, pos, ring: i < HORIZON_RING }).collect();
            ch.layout = true;
        }
        let camera = if ctx.camera.valid && ctx.camera.pos.iter().all(|v| v.is_finite()) { [ctx.camera.pos[0], ctx.camera.pos[2]] } else { [view.origin.x, view.origin.z] };
        if ch.layout || ch.cloud_xforms || now - self.last_cloud_s >= CLOUD_STEP_S || now < self.last_cloud_s {
            self.last_cloud_s = now;
            self.cloud_world = self.clouds.iter().map(|c| if c.ring { [camera[0] + c.pos[0], c.pos[1], camera[1] + c.pos[2]] } else { cloud_world(c.pos, offset, camera, self.cloud_radius_m) }).collect();
            ch.cloud_xforms = true;
        }

        // Lightning in storms (L1875): never with Reduce flashing or the setting off.
        let storm = target.kind == WeatherKind::Storm && blend > 0.5;
        if storm && !ctx.reduce_flashing && self.lightning_on { ch.lightning = lightning_between(self.env.cloud_seed, r0, self.real_s); }

        // Island tiles near the camera want their near mesh.
        for (key, t) in self.tiles.iter_mut() {
            let c = [(key.0 as f64 + 0.5) * TILE_M, (key.1 as f64 + 0.5) * TILE_M];
            let d = ((c[0] - camera[0]).powi(2) + (c[1] - camera[1]).powi(2)).sqrt();
            let limit = if t.near_wanted { NEAR_TILE_M * NEAR_HYSTERESIS } else { NEAR_TILE_M };
            t.near_wanted = d < limit;
            if d > NEAR_FREE_M { t.near_built = false; t.near_pending = false; }
            let show = t.near_wanted && t.near_built;
            if show != t.show_near { t.show_near = show; ch.islands_moved = true; }
        }
        ch
    }

    /// Near meshes to request now (the caller sends them and marks them pending).
    pub fn near_requests(&mut self) -> Vec<TileKey> {
        let mut out = Vec::new();
        for (k, t) in self.tiles.iter_mut() {
            if t.near_wanted && !t.near_built && !t.near_pending { t.near_pending = true; out.push(*k); }
        }
        out
    }

    /// The presets to grade with: the blend's two and its weight.
    pub fn presets(&self) -> Option<PresetPair> {
        let w = self.target?;
        let time = time_of_day(w.sun_elevation_deg);
        Some(((self.env.blend_from, time), (self.env.blend_to, time), self.env.blend))
    }
}

/// A cloud's drawn world position: its layout position plus the wind drift, at the copy of the `2R` field
/// nearest the camera.
pub fn cloud_world(pos: [f64; 3], offset: [f64; 2], camera: [f64; 2], radius: f64) -> [f64; 3] {
    let period = 2.0 * radius.max(1.0);
    let wrap = |p: f64, c: f64| { let d = p - c; c + d - period * (d / period).round() };
    let (x, z) = (pos[0] + offset[0], pos[2] + offset[1]);
    if !(x.is_finite() && z.is_finite()) { return [camera[0], pos[1], camera[1]]; }
    [wrap(x, camera[0]), pos[1], wrap(z, camera[1])]
}

/// Linear blend of two equally sized RGB8 LUTs, `t` 0..1 toward `b`.
pub fn blend_lut(a: &[u8], b: &[u8], t: f32) -> Vec<u8> {
    if a.len() != b.len() { return if t < 0.5 { a.to_vec() } else { b.to_vec() }; }
    let t = if t.is_finite() { t.clamp(0.0, 1.0) } else { 1.0 };
    a.iter().zip(b).map(|(&x, &y)| (x as f32 + (y as f32 - x as f32) * t).round().clamp(0.0, 255.0) as u8).collect()
}

/// The order in which LUT files are tried for a preset: itself, the same weather at dusk, clear at the same
/// time, then dusk clear (the first release ships 14 of the 18 presets).
pub fn lut_fallbacks(weather: WeatherKind, time: TimeOfDay) -> [(WeatherKind, TimeOfDay); 4] {
    [(weather, time), (weather, TimeOfDay::Dusk), (WeatherKind::Clear, time), (WeatherKind::Clear, TimeOfDay::Dusk)]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::interp::{CameraState, ViewFrame};
    use crate::seams::WireEvent;

    const HZ: f64 = sim_core::TICK_HZ as f64;
    fn weather(kind: WeatherKind) -> WeatherState { WeatherState { kind, visibility_m: kind.visibility_m(), cloud_cover: 0.5, ..Default::default() } }
    fn turn(tick: u64, kind: WeatherKind) -> WireEvent { WireEvent { tick, event: SimEvent::Phys(PhysEvent::WeatherChanged { weather: weather(kind), wave_seed: 1 }) } }
    fn step(c: &mut SceneryCore, t_s: f64, events: &[WireEvent], reduce_flashing: bool) -> Changes {
        let view = ViewFrame { render_tick: t_s * HZ, sim_time_s: t_s, ..Default::default() };
        let q = crate::quality::QualityProfile::default();
        let ctx = FrameContext { view: &view, events, camera: CameraState::default(), delta_s: 1.0 / 60.0, scenario: godot::prelude::Rid::Invalid, quality: &q, degrade_step: 0, reduce_flashing };
        c.step(&ctx)
    }

    #[test]
    fn spec_formulas() {
        assert!((fog_density(30_000.0) - 1.304e-4).abs() < 1e-6);
        assert!((fog_density(400.0) - 9.78e-3).abs() < 1e-5);
        assert_eq!(time_of_day(2.9), TimeOfDay::Night);
        assert_eq!(time_of_day(3.0), TimeOfDay::Dusk);
        assert_eq!(time_of_day(24.9), TimeOfDay::Dusk);
        assert_eq!(time_of_day(25.0), TimeOfDay::Day);
        assert_eq!(time_of_day(f64::NAN), TimeOfDay::Night);
        assert!((fog_sun_scatter(5.0) - 0.2).abs() < 1e-6);
        assert!(fog_sun_scatter(25.0).abs() < 1e-6);
        assert!((fog_sun_scatter(17.5) - 0.1).abs() < 1e-6);
        let d = sun_dir(0.0, 90.0);
        assert!((d[0] - 1.0).abs() < 1e-6 && d[1].abs() < 1e-6, "a sun due east on the horizon: {d:?}");
        assert!((sun_dir(90.0, 0.0)[1] - 1.0).abs() < 1e-6);
        assert_eq!(blend_len_s(8_000.0, 8_000.0), 20.0);
        assert!((blend_len_s(30_000.0, 400.0) - 60.0).abs() < 1e-9);
        assert_eq!(blend_lut(&[0, 100, 200], &[100, 200, 0], 0.5), vec![50, 150, 100]);
        assert_eq!(blend_lut(&[1, 2], &[3], 0.2), vec![1, 2]);
        assert_eq!(lut_fallbacks(WeatherKind::Rain, TimeOfDay::Night)[3], (WeatherKind::Clear, TimeOfDay::Dusk));
    }

    #[test]
    fn weather_turns_blend_and_never_out_see_the_sim() {
        let mut c = SceneryCore::new();
        assert_eq!(step(&mut c, 0.0, &[WireEvent { tick: 0, event: SimEvent::SortieStarted { seed: 9, tick_hz: 60 } }], false), Changes::default());
        let ch = step(&mut c, 0.0, &[turn(0, WeatherKind::Clear)], false);
        assert!(ch.env && ch.lut && ch.layout);
        assert_eq!(c.env.blend, 1.0);
        assert!((c.env.fog_density - 1.304e-4).abs() < 1e-6);
        assert!((c.env.aerial_perspective - 0.7).abs() < 1e-6);
        assert_eq!(c.env.cloud_seed, 9);
        // A fog bank rolls in at 10 s: density jumps to the sim's at once; aerial perspective is capped.
        step(&mut c, 10.0, &[turn(600, WeatherKind::FogBank)], false);
        assert!(c.env.fog_density >= (KOSCHMIEDER / 400.0) as f32 * 0.999);
        assert!(c.env.aerial_perspective <= AERIAL_DENSE_CAP);
        assert!(c.env.blend < 0.05 && c.env.blend_from == WeatherKind::Clear && c.env.blend_to == WeatherKind::FogBank);
        // It clears again: the blend is gradual and re-applies once a second.
        step(&mut c, 20.0, &[turn(1200, WeatherKind::Clear)], false);
        let rev = c.rev;
        let mut last = c.env.fog_density;
        let mut revs = 0;
        for k in 1..=70 {
            let before = c.rev;
            step(&mut c, 20.0 + k as f64, &[], false);
            assert!(c.env.fog_density <= last + 1e-9, "clearing never thickens");
            assert!(c.env.fog_density >= 1.304e-4 * 0.999);
            if c.env.fog_density >= AERIAL_DENSE_FROM { assert!(c.env.aerial_perspective <= AERIAL_DENSE_CAP); }
            last = c.env.fog_density;
            if c.rev != before { revs += 1; }
        }
        assert!(c.rev > rev && revs >= 55, "1 Hz during a 60 s blend: {revs}");
        assert_eq!(c.env.blend, 1.0);
        assert!((c.env.fog_density - 1.304e-4).abs() < 1e-6);
    }

    #[test]
    fn wind_drift_clouds_and_lightning_are_deterministic() {
        let mut c = SceneryCore::new();
        step(&mut c, 0.0, &[WireEvent { tick: 0, event: SimEvent::SortieStarted { seed: 4, tick_hz: 60 } }], false);
        let mut w = weather(WeatherKind::Storm);
        w.wind_dir_rad = std::f64::consts::FRAC_PI_2;
        w.wind_speed_mps = 10.0;
        step(&mut c, 0.0, &[WireEvent { tick: 0, event: SimEvent::Phys(PhysEvent::WeatherChanged { weather: w, wave_seed: 1 }) }], false);
        step(&mut c, 30.0, &[], false);
        assert!((c.env.wind_offset_m[0] - 300.0).abs() < 1e-3 && c.env.wind_offset_m[1].abs() < 1e-3, "{:?}", c.env.wind_offset_m);
        assert!(!c.clouds.is_empty());
        let r = c.cloud_radius_m;
        assert!(c.cloud_world.iter().all(|p| p[0].abs() <= r + 1e-6 && p[2].abs() <= r + 1e-6), "drawn round the camera");
        // A far camera still has the field round it.
        let p = cloud_world([100.0, 900.0, -50.0], [0.0, 0.0], [40_000.0, -30_000.0], 5_000.0);
        assert!((p[0] - 40_000.0).abs() <= 5_000.0 && (p[2] + 30_000.0).abs() <= 5_000.0 && p[1] == 900.0);
        // Lightning: pulses at least 2 s apart, 80 to 150 ms, the same on every run.
        let mut at = Vec::new();
        let mut t = 0.0;
        while t < 600.0 { if let Some(l) = lightning_between(4, t, t + 1.0 / 60.0) { assert!((0.08..=0.15).contains(&l.duration_s)); at.push(t); } t += 1.0 / 60.0; }
        assert!(at.len() > 40, "{}", at.len());
        assert!(at.windows(2).all(|w| w[1] - w[0] >= 2.0 - 1e-6));
        assert_eq!(lightning_between(4, 10.0, 9.0), None);
        // Reduce flashing removes it; so does the setting.
        let mut pulses = 0;
        for k in 1..=3600 { if step(&mut c, 30.0 + k as f64 / 60.0, &[], true).lightning.is_some() { pulses += 1; } }
        assert_eq!(pulses, 0);
        c.lightning_on = false;
        for k in 3601..=7200 { if step(&mut c, 30.0 + k as f64 / 60.0, &[], false).lightning.is_some() { pulses += 1; } }
        assert_eq!(pulses, 0);
    }

    #[test]
    fn island_tiles_cover_the_land() {
        let map = MapDef { size_m: [20_000.0, 20_000.0], depth_m: 200.0, islands: vec![content::campaign::Island { centre_m: [1_000.0, -500.0], radius_m: 600.0, height_m: 80.0, seed: 3 }], markers: Vec::new(), seed: 1 };
        let tiles = land_tiles(&map);
        assert!(tiles.contains(&(1, -1)) && tiles.contains(&(2, -1)));
        assert!(tiles.len() <= 16, "{}", tiles.len());
        let mut c = SceneryCore::new();
        let mut w = Worker::inline();
        c.set_map(&map, IslandBands { band_top_m: vec![3.0, 1000.0], colors: vec![[0.2, 0.2, 0.1], [0.1, 0.12, 0.05]] }, &mut w);
        assert_eq!(c.tiles.len(), tiles.len());
        assert!(c.ground_height(1_000.0, -500.0) > 40.0);
        assert!(c.ground_height(9_000.0, 9_000.0) < -100.0);
        let mut built = Vec::new();
        for _ in 0..tiles.len() { w.poll(&mut built); }
        assert_eq!(built.len(), tiles.len());
        let triangles: usize = built.iter().map(|b| match b { Built::Island { mesh, near, .. } => { assert!(!near); mesh.indices.len() / 3 } _ => 0 }).sum();
        assert!(triangles > 100, "{triangles}");
        // Bad maps build nothing and never panic.
        let bad = MapDef { islands: vec![content::campaign::Island { centre_m: [f64::NAN, 0.0], radius_m: 100.0, height_m: 10.0, seed: 0 }, content::campaign::Island { centre_m: [0.0, 0.0], radius_m: -5.0, height_m: 10.0, seed: 0 }], ..Default::default() };
        assert!(land_tiles(&bad).is_empty());
    }

    #[test]
    fn the_horizon_has_masses_all_round_wherever_the_camera_is() {
        // G5: no 90 degree sector of uniform horizon. Bearings of the drawn copies of every cluster, seen from
        // cameras across a 100 km map, at light and full cover.
        for (seed, cover) in [(3u64, 0.1), (8, 0.5), (21, 1.0)] {
            let r = 18_000.0;
            let layout = hull_mesh::env::scenery::cloud_layout(seed, cover, false, r);
            for cam in [[0.0, 0.0], [31_000.0, -7_000.0], [-45_000.0, 44_000.0], [12_345.0, 49_000.0]] {
                let mut bearings: Vec<f64> = layout.iter().enumerate().map(|(i, c)| {
                    let p = if i < HORIZON_RING { [cam[0] + c.3[0], c.3[1], cam[1] + c.3[2]] } else { cloud_world(c.3, [1_234.0, -567.0], cam, r) };
                    (p[0] - cam[0]).atan2(-(p[2] - cam[1])).to_degrees().rem_euclid(360.0)
                }).collect();
                bearings.sort_by(f64::total_cmp);
                let gap = bearings.windows(2).map(|w| w[1] - w[0]).fold(bearings[0] + 360.0 - bearings[bearings.len() - 1], f64::max);
                assert!(gap < 90.0, "seed {seed} cover {cover} camera {cam:?}: a {gap:.0} degree gap");
            }
        }
    }

}
