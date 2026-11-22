//! Unit `gd-quality`. `BpQuality` (autoload `Quality`): graphics tier auto-detect and the runtime visual
//! degradation of rung 1. DESIGN.md L1975-1998 (tiers and per-pass budgets), L2016-2020 (auto-detect at first
//! launch and on adapter or driver change: read adapter, driver, processor, model name and memory; map Macs
//! by chip, memory and the fanless list; Windows by adapter and VRAM; run a 10 s scripted flotilla and two
//! 2 s A/B tests; over budget at p95 drops one tier once; the player can override everything),
//! L2022-2024 (dynamic resolution steps 1.0, 0.87, 0.77, 0.67 down to the tier floor; down after 1 s over
//! 90% of budget, up after 5 s under 70%, steps at least 3 s apart; Mac thermal state polled at 1 Hz),
//! L2026-2033 (degrade order and what is never degraded), L1500-1502 (rung 1 runs on each client).
//! OS queries (Windows video memory, Mac thermal state) come from `crate::platform`.
//! Rust decides; `res://world/quality.gd` applies viewport and environment settings (scaling mode, MSAA,
//! shadow atlas, glow levels) from `profile()`, because those are engine settings, not gameplay.
//!
//! The tier type is `hull_mesh::QualityTier` (re-exported by `crate::enums`): the same value picks the hull
//! LOD threshold (`hull_mesh::lod::pick_lod`), the VFX caps (`hull_mesh::vfx::CapGroup::cap`,
//! `layer_b_particle_cap`, `vfx_light_budget`), the trail length and the ocean clipmap
//! (`hull_mesh::env::ocean::ClipmapSpec::for_tier`). `Fallback` is the Compatibility renderer.
//!
//! # Layout
//! The tier tables ([`classify`]), the profile per tier ([`build_profile`]) and the dynamic-resolution and
//! degrade controller ([`Governor`]) are plain Rust and unit-tested; `BpQuality` feeds them engine data and
//! persists the auto-detect result through `crate::settings::write` (keys `graphics.detected_*`).
//!
//! # Degrade step
//! `QualityProfile::degrade_step` is 0 with nothing degraded, 1 while the resolution is below the tier's base
//! (step 1 of L2026), and 2..=6 for steps 2..6 once resolution sits at the floor and the frame is still over
//! budget. This unit applies the profile-level parts itself: step 4 multiplies the shadow distance by 0.75,
//! step 5 sets glow to level 2 only, step 6 turns glow off (the clipmap and detail tiling of step 6 are the
//! ocean renderer's, read from `degrade_step`). `glow_levels == (0, 0)` means glow off.
use crate::enums::{GdEnum, QualityTier};
use crate::settings::{ClientSettings, SettingValue};
use godot::prelude::*;
use sim_systems::protocol::{Msg, MsgArg, Unit};

/// The effective graphics profile. Lives in `Services::quality`.
#[derive(Clone, Debug, PartialEq)]
pub struct QualityProfile {
    pub tier: QualityTier,
    /// 3D resolution scale before dynamic resolution, and the current dynamic step multiplier.
    pub base_scale: f32,
    pub dynamic_scale: f32,
    pub max_fps: u32,
    pub msaa: u8,
    pub smaa: bool,
    pub shadow_splits: u8,
    pub shadow_distance_m: f32,
    pub shadow_atlas: u32,
    pub ssao: bool,
    pub ssr: bool,
    pub volumetric_fog: bool,
    pub glow_levels: (u8, u8),
    /// 0..=6: how far down the degrade order rung 1 has gone (L2026-2031).
    pub degrade_step: u8,
    /// Hull LOD threshold in pixels per cell: 1.5, or 3.0 on Low (L1943). Informative (performance overlay);
    /// the pick itself is `hull_mesh::lod::pick_lod(.., tier, ..)`, which holds the same two numbers.
    pub lod_cell_px: f32,
    pub forward_plus: bool,
    /// Particle emission multiplier, 1.0 normally; 0.7 while a Mac reports a serious thermal state (L2024).
    /// VFX units multiply their particle caps by it.
    pub particle_scale: f32,
}
impl Default for QualityProfile {
    /// Medium as specified (L1835-1842, L1877-1881).
    fn default() -> Self { Self { tier: QualityTier::Medium, base_scale: 1.0, dynamic_scale: 1.0, max_fps: 60, msaa: 2, smaa: false, shadow_splits: 4, shadow_distance_m: 500.0, shadow_atlas: 4096, ssao: true, ssr: false, volumetric_fog: false, glow_levels: (2, 4), degrade_step: 0, lod_cell_px: 1.5, forward_plus: true, particle_scale: 1.0 } }
}

// ---------------------------------------------------------------------------------------------------------
// Auto-detect tables (pure)
// ---------------------------------------------------------------------------------------------------------

/// What the engine reports about this machine (`detect` argument).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Machine {
    pub adapter: String,
    pub vendor: String,
    pub driver_version: String,
    pub api_version: String,
    pub processor: String,
    pub model: String,
    pub memory_gb: f64,
    pub rendering_method: String,
    pub window_px: (i32, i32),
    /// Windows DXGI dedicated video memory.
    pub vram_bytes: Option<u64>,
}

/// Fanless Macs by model identifier (L2018), plus every `MacBookAir*` identifier.
pub const FANLESS_MODELS: &[&str] = &["MacBookAir10,1", "Mac14,2", "Mac14,15", "Mac15,12", "Mac15,13", "Mac16,12", "Mac16,13"];
const GIB: f64 = 1024.0 * 1024.0 * 1024.0;

/// The tier a machine maps to, with the reason as a message and whether it is below the minimum spec.
pub fn classify(m: &Machine) -> (QualityTier, Msg, bool) {
    let name = MsgArg::Name(clean_name(if m.adapter.trim().is_empty() { &m.processor } else { &m.adapter }));
    if m.rendering_method.trim() == "gl_compatibility" {
        return (QualityTier::Fallback, Msg::new("settings.detect.safe_mode"), true);
    }
    if is_apple(m) {
        let chip = apple_chip(m);
        let chip_arg = MsgArg::Name(chip.clone());
        let pro = ["Pro", "Max", "Ultra"].iter().any(|k| chip.split_whitespace().any(|w| w == *k));
        if pro { return (QualityTier::High, Msg::new("settings.detect.apple_pro").arg(chip_arg), false); }
        let fanless = m.model.starts_with("MacBookAir") || FANLESS_MODELS.contains(&m.model.trim());
        if fanless { return (QualityTier::Low, Msg::new("settings.detect.apple_fanless").arg(chip_arg), false); }
        if m.memory_gb > 0.0 && m.memory_gb < 12.0 {
            return (QualityTier::Low, Msg::new("settings.detect.apple_memory").arg(chip_arg).arg(MsgArg::Num { value: m.memory_gb.round(), unit: Unit::None }), false);
        }
        return (QualityTier::Medium, Msg::new("settings.detect.apple_base").arg(chip_arg), false);
    }
    let adapter = m.adapter.to_ascii_lowercase();
    if is_integrated(&adapter) { return (QualityTier::Low, Msg::new("settings.detect.integrated").arg(name), true); }
    if let Some(v) = m.vram_bytes.filter(|v| *v > 0) {
        if (v as f64) < 6.0 * GIB * 0.97 {
            // DXGI reports a few hundred MB under the marketing size; 3% slack keeps a 6 GB card at 6 GB.
            return (QualityTier::Low, Msg::new("settings.detect.low_vram").arg(name).arg(MsgArg::Num { value: (v as f64 / GIB * 10.0).round() / 10.0, unit: Unit::None }), true);
        }
    }
    if is_high_adapter(&adapter) { return (QualityTier::High, Msg::new("settings.detect.high_adapter").arg(name), false); }
    (QualityTier::Medium, Msg::new("settings.detect.medium_adapter").arg(name), false)
}

fn clean_name(s: &str) -> String { s.replace("(TM)", "").replace("(R)", "").replace("(tm)", "").replace("(r)", "").split_whitespace().collect::<Vec<_>>().join(" ").chars().take(64).collect() }

fn is_apple(m: &Machine) -> bool {
    m.vendor.contains("Apple") || m.adapter.starts_with("Apple") || m.processor.starts_with("Apple") || m.model.starts_with("Mac")
}
/// "Apple M3 Pro" from the adapter or processor name.
fn apple_chip(m: &Machine) -> String {
    for s in [&m.adapter, &m.processor] { if s.contains(" M") || s.starts_with('M') { return clean_name(s); } }
    clean_name(if m.adapter.is_empty() { &m.processor } else { &m.adapter })
}

/// Integrated graphics by name: Intel UHD, HD and Iris, Intel Arc graphics without a model number (Core
/// Ultra), AMD APU graphics ("Radeon Graphics", "Radeon 780M", Vega) and Microsoft's software adapter.
pub fn is_integrated(adapter_lower: &str) -> bool {
    let a = adapter_lower;
    if a.contains("microsoft basic render") || a.contains("llvmpipe") || a.contains("swiftshader") { return true; }
    if a.contains("intel") {
        // Discrete Arc cards carry a model: A380 ... A770, B570, B580.
        return !has_word_with(a, &["a3", "a5", "a7", "b5", "b7"]);
    }
    if a.contains("radeon") || a.contains("amd") {
        if a.contains("vega") && !a.contains("vega 56") && !a.contains("vega 64") { return true; }
        if a.contains(" rx ") || a.contains("rx ") || a.contains("pro w") { return false; }
        // "AMD Radeon(TM) Graphics", "Radeon 780M Graphics", "Radeon 890M".
        return a.contains("graphics") || words(a).any(|w| w.ends_with('m') && w[..w.len() - 1].chars().all(|c| c.is_ascii_digit()) && w.len() >= 4);
    }
    false
}
fn words(s: &str) -> impl Iterator<Item = &str> { s.split(|c: char| !c.is_ascii_alphanumeric()).filter(|w| !w.is_empty()) }
fn has_word_with(s: &str, prefixes: &[&str]) -> bool { words(s).any(|w| prefixes.iter().any(|p| w.starts_with(p) && w.len() == 4 && w[1..].chars().all(|c| c.is_ascii_digit()))) }

/// The High row's adapters (L1979): RTX 3060 Ti, 4060, RX 6700 and up.
pub fn is_high_adapter(adapter_lower: &str) -> bool {
    let a = adapter_lower;
    let ti = a.contains(" ti") || a.contains("ti ") || a.ends_with("ti");
    for w in words(a) {
        let Ok(n) = w.trim_end_matches("ti").parse::<u32>() else { continue };
        if a.contains("rtx") || a.contains("geforce") {
            let (series, model) = (n / 100, n % 100);
            match series {
                30 => return model >= 70 || (model == 60 && ti),
                40 | 50 => return model >= 60,
                _ => {}
            }
        }
        if a.contains("radeon") || a.contains(" rx") {
            // RX 6700 and up; RX 7700 and up (the 7600 sits with the 6600); every RX 9000 from the 9060.
            match n {
                6700..=6999 | 7700..=7999 => return true,
                9060..=9999 => return true,
                _ => {}
            }
        }
    }
    false
}

/// Ladder of dynamic resolution multipliers (L2022).
pub const DYNAMIC_STEPS: [f32; 4] = [1.0, 0.87, 0.77, 0.67];

/// The tier's base 3D scale for a window (L1977-1981, `sqrt(target MP / window MP)` where the target is in
/// megapixels), never above 1.
pub fn base_scale(tier: QualityTier, apple: bool, window_px: (i32, i32)) -> f32 {
    let mp = (window_px.0.max(1) as f64 * window_px.1.max(1) as f64) / 1.0e6;
    let fit = |target_mp: f64| ((target_mp / mp).sqrt()).min(1.0) as f32;
    match tier {
        QualityTier::Low => fit(1.0),
        QualityTier::Medium if apple => fit(1.4),
        QualityTier::Medium => 1.0,
        // 60 fps at 1080p, or 1440p at 0.77: above 1080p the base drops to 0.77 with the spatial upscaler.
        QualityTier::High => if mp > 1920.0 * 1080.0 / 1.0e6 * 1.05 { 0.77 } else { 1.0 },
        QualityTier::Fallback => 1.0,
    }
}

/// Lowest dynamic multiplier for a tier (L2022): Low keeps 0.7 MP on screen, Medium 0.77, High 0.67.
pub fn dynamic_floor(tier: QualityTier, base: f32, window_px: (i32, i32)) -> f32 {
    match tier {
        QualityTier::Low => {
            let mp = (window_px.0.max(1) as f64 * window_px.1.max(1) as f64) / 1.0e6;
            let floor_scale = (0.7 / mp).sqrt() as f32;
            (floor_scale / base.max(0.01)).clamp(DYNAMIC_STEPS[3], 1.0)
        }
        QualityTier::Medium => 0.77,
        QualityTier::High => 0.67,
        QualityTier::Fallback => 1.0,
    }
}

/// Frame budget in milliseconds: the tier's target rate.
pub fn budget_ms(max_fps: u32) -> f64 { 1000.0 / max_fps.max(1) as f64 }

/// Player overrides read from the settings (L2020).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Overrides { pub tier: Option<QualityTier>, pub msaa: Option<u8>, pub shadow_atlas: Option<u32>, pub ssao: Option<u8>, pub pcss: bool, pub dynamic_resolution: bool, pub smaa: bool, pub two_splits: bool }
impl Overrides {
    pub fn from_settings(s: &ClientSettings) -> Self {
        let tier = match s.int("graphics.tier") { t @ 0..=2 => QualityTier::from_gd(t), _ => None };
        let msaa = match s.int("graphics.msaa") { v @ (0 | 2 | 4) => Some(v as u8), _ => None };
        let shadow_atlas = match s.int("graphics.shadow_atlas") { v @ (2048 | 4096) => Some(v as u32), _ => None };
        let ssao = match s.int("graphics.ssao") { v @ 0..=2 => Some(v as u8), _ => None };
        Self { tier, msaa, shadow_atlas, ssao, pcss: s.bool("graphics.pcss"), dynamic_resolution: s.bool("graphics.dynamic_resolution"), smaa: s.bool("graphics.detected_smaa"), two_splits: s.bool("graphics.detected_two_splits") }
    }
}

/// Everything `profile()` reports beyond `QualityProfile`'s own fields.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ProfileExtras { pub ssao_level: u8, pub soft_high: bool, pub glow_bicubic: bool, pub lut_size: u32, pub apple: bool, pub floor: f32 }

/// The profile of a tier with the player's overrides (L1835-1842, L1877-1881).
pub fn build_profile(tier: QualityTier, apple: bool, window_px: (i32, i32), o: &Overrides) -> (QualityProfile, ProfileExtras) {
    let base = base_scale(tier, apple, window_px);
    let mut p = QualityProfile { tier, base_scale: base, dynamic_scale: 1.0, forward_plus: tier != QualityTier::Fallback, ..QualityProfile::default() };
    let mut x = ProfileExtras { ssao_level: 0, soft_high: false, glow_bicubic: true, lut_size: hull_mesh::gen_files::LUT_EDGE, apple, floor: dynamic_floor(tier, base, window_px) };
    match tier {
        QualityTier::Low => {
            p.max_fps = 30; p.msaa = 2; p.shadow_splits = 2; p.shadow_distance_m = 250.0; p.shadow_atlas = 2048;
            p.glow_levels = (2, 3); p.lod_cell_px = 3.0; x.glow_bicubic = false;
        }
        QualityTier::Medium => {
            p.max_fps = if apple { 30 } else { 60 };
            p.smaa = o.smaa; p.msaa = if o.smaa { 0 } else { 2 };
            p.shadow_splits = if o.two_splits { 2 } else { 4 };
            x.ssao_level = 1;
        }
        QualityTier::High => {
            p.msaa = 4; p.shadow_distance_m = 800.0; p.ssr = true; p.volumetric_fog = true;
            x.ssao_level = 2; x.soft_high = true; x.lut_size = hull_mesh::gen_files::LUT_EDGE_HIGH;
        }
        QualityTier::Fallback => {
            // Best effort: Layer A only, MSAA 2x (L1981); no SSAO, SSR or fog in Compatibility.
            p.msaa = 2; p.shadow_splits = 2; p.shadow_distance_m = 250.0; p.shadow_atlas = 2048; p.glow_levels = (2, 3); p.lod_cell_px = 3.0;
            x.glow_bicubic = false;
        }
    }
    if let Some(m) = o.msaa { p.msaa = m; if m > 0 { p.smaa = false; } }
    if let Some(a) = o.shadow_atlas { p.shadow_atlas = a; }
    if let Some(s) = o.ssao { x.ssao_level = if tier == QualityTier::Fallback { 0 } else { s }; }
    if o.pcss && tier != QualityTier::Fallback { x.soft_high = true; }
    p.ssao = x.ssao_level > 0;
    (p, x)
}

// ---------------------------------------------------------------------------------------------------------
// Dynamic resolution and the degrade ladder (pure)
// ---------------------------------------------------------------------------------------------------------

/// Seconds over 90% of budget before a step down, under 70% before a step up, and between steps (L2022).
pub const DOWN_AFTER_S: f64 = 1.0;
pub const UP_AFTER_S: f64 = 5.0;
pub const STEP_GAP_S: f64 = 3.0;
/// A step whose next frame takes over this many budgets hitched; later steps wait for a camera cut.
const HITCH_FACTOR: f64 = 2.0;
/// A queued step applies at the next camera cut, or after this long if no cut comes (a long engagement
/// with a fixed camera must still recover its frame rate).
const CUT_WAIT_MAX_S: f64 = 10.0;
/// Highest ladder step (L2026-2031).
pub const MAX_DEGRADE_STEP: u8 = 6;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Pending { Down, Up }

/// Dynamic resolution and degrade-ladder state.
#[derive(Clone, Debug, PartialEq)]
pub struct Governor {
    /// Index into the allowed resolution steps.
    pub res_index: usize,
    /// Ladder steps 2..=6 taken beyond resolution (0..=5).
    pub ladder: u8,
    pub over_s: f64,
    pub under_s: f64,
    pub since_step_s: f64,
    /// Steps hitch on this machine: resolution steps wait for a camera cut.
    pub hitchy: bool,
    pending: Option<(Pending, f64)>,
    /// Watching the frame after a resolution step for a hitch.
    watch_hitch: bool,
    /// Serious thermal state forces at least one resolution step; critical forces the floor (L2024).
    pub thermal: u8,
}
impl Default for Governor { fn default() -> Self { Self { res_index: 0, ladder: 0, over_s: 0.0, under_s: 0.0, since_step_s: STEP_GAP_S, hitchy: false, pending: None, watch_hitch: false, thermal: 0 } } }

impl Governor {
    /// Resolution steps allowed above the floor (always at least 1.0).
    pub fn steps(floor: f32) -> Vec<f32> { DYNAMIC_STEPS.iter().copied().filter(|s| *s >= floor - 1e-4).collect() }

    /// The multiplier now, honouring the thermal minimum.
    pub fn scale(&self, floor: f32) -> f32 {
        let steps = Self::steps(floor);
        let forced = match self.thermal { 3.. => steps.len() - 1, 2 => 1.min(steps.len() - 1), _ => 0 };
        steps[self.res_index.max(forced).min(steps.len() - 1)]
    }
    /// The degrade step of `QualityProfile` (see the module header).
    pub fn degrade_step(&self, floor: f32) -> u8 {
        if self.ladder > 0 { (self.ladder + 1).min(MAX_DEGRADE_STEP) } else if self.scale(floor) < 1.0 { 1 } else { 0 }
    }

    /// Feed one frame. `cost_ms` is GPU time where the driver measures it, else frame time; `dt_s` the
    /// frame's length. Returns true when the profile changed.
    pub fn sample(&mut self, cost_ms: f64, dt_s: f64, budget_ms: f64, floor: f32, dynamic_resolution: bool) -> bool {
        let dt = if dt_s.is_finite() { dt_s.clamp(0.0, 0.25) } else { 0.0 };
        if !cost_ms.is_finite() || cost_ms <= 0.0 || budget_ms <= 0.0 { return false; }
        if self.watch_hitch {
            self.watch_hitch = false;
            if cost_ms > HITCH_FACTOR * budget_ms { self.hitchy = true; }
        }
        self.since_step_s += dt;
        if cost_ms > 0.9 * budget_ms { self.over_s += dt; self.under_s = 0.0; } else if cost_ms < 0.7 * budget_ms { self.under_s += dt; self.over_s = 0.0; } else { self.over_s = 0.0; self.under_s = 0.0; }
        // A queued resolution step that waited too long for a cut goes ahead.
        if let Some((dir, waited)) = self.pending {
            let waited = waited + dt;
            if waited >= CUT_WAIT_MAX_S { self.pending = None; return self.apply_resolution(dir, floor); }
            self.pending = Some((dir, waited));
        }
        if self.since_step_s < STEP_GAP_S { return false; }
        let steps = Self::steps(floor).len();
        if self.over_s >= DOWN_AFTER_S {
            self.over_s = 0.0;
            if dynamic_resolution && self.res_index + 1 < steps { return self.request(Pending::Down, floor); }
            if self.ladder < MAX_DEGRADE_STEP - 1 { self.ladder += 1; self.since_step_s = 0.0; return true; }
        } else if self.under_s >= UP_AFTER_S {
            self.under_s = 0.0;
            // Undo in reverse order: the ladder first, resolution last.
            if self.ladder > 0 { self.ladder -= 1; self.since_step_s = 0.0; return true; }
            if self.res_index > 0 { return self.request(Pending::Up, floor); }
        }
        false
    }

    fn request(&mut self, dir: Pending, floor: f32) -> bool {
        if self.hitchy { if self.pending.is_none() { self.pending = Some((dir, 0.0)); } self.since_step_s = 0.0; return false; }
        self.apply_resolution(dir, floor)
    }
    fn apply_resolution(&mut self, dir: Pending, floor: f32) -> bool {
        let steps = Self::steps(floor).len();
        let before = self.res_index;
        self.res_index = match dir { Pending::Down => (self.res_index + 1).min(steps - 1), Pending::Up => self.res_index.saturating_sub(1) };
        self.since_step_s = 0.0;
        self.watch_hitch = self.res_index != before;
        self.res_index != before
    }

    /// A camera cut happened: a queued resolution step applies now. Returns true when the profile changed.
    pub fn camera_cut(&mut self, floor: f32) -> bool {
        match self.pending.take() { Some((dir, _)) => self.apply_resolution(dir, floor), None => false }
    }
    /// Forget the controller history (new tier, new window).
    pub fn reset(&mut self) { let (hitchy, thermal) = (self.hitchy, self.thermal); *self = Self { hitchy, thermal, ..Self::default() }; }
}

/// Apply the governor and thermal state to a tier profile.
pub fn effective_profile(mut p: QualityProfile, x: &ProfileExtras, g: &Governor, dynamic_resolution: bool) -> QualityProfile {
    p.dynamic_scale = if dynamic_resolution || g.thermal >= 2 { g.scale(x.floor) } else { 1.0 };
    p.degrade_step = if dynamic_resolution || g.thermal >= 2 { g.degrade_step(x.floor) } else if g.ladder > 0 { (g.ladder + 1).min(MAX_DEGRADE_STEP) } else { 0 };
    if p.degrade_step >= 4 { p.shadow_distance_m *= 0.75; }
    if p.degrade_step >= 5 { p.glow_levels = (2, 2); }
    if p.degrade_step >= 6 { p.glow_levels = (0, 0); }
    if g.thermal >= 2 { p.particle_scale = 0.7; }
    if g.thermal >= 3 { p.glow_levels = (0, 0); }
    p
}

/// Every `settings.detect.*` key `classify` and `detect_timings` emit.
pub const REASON_KEYS: &[&str] = &[
    "settings.detect.safe_mode", "settings.detect.apple_pro", "settings.detect.apple_fanless", "settings.detect.apple_memory",
    "settings.detect.apple_base", "settings.detect.integrated", "settings.detect.low_vram", "settings.detect.high_adapter",
    "settings.detect.medium_adapter", "settings.detect.dropped", "settings.detect.override",
];

// ---------------------------------------------------------------------------------------------------------
// The autoload
// ---------------------------------------------------------------------------------------------------------

/// What `detect` found.
#[derive(Clone, Debug, Default)]
pub(crate) struct Detection { pub tier: Option<QualityTier>, pub reason: Msg, pub below_minimum: bool, pub adapter: String, pub changed: bool, pub signature: String, pub dropped: bool }

/// Graphics quality. Autoload `Quality`.
#[derive(GodotClass)]
#[class(base = Node)]
pub struct BpQuality {
    pub(crate) over_budget_s: f32,
    pub(crate) under_budget_s: f32,
    pub(crate) since_step_s: f32,
    pub(crate) thermal_poll_s: f32,
    pub(crate) governor: Governor,
    pub(crate) detection: Detection,
    pub(crate) machine: Machine,
    /// Settings the profile was last built from, to notice a change.
    pub(crate) overrides: Option<Overrides>,
    pub(crate) extras: Option<ProfileExtras>,
    pub(crate) tier_profile: QualityProfile,
    base: Base<Node>,
}

#[godot_api]
impl INode for BpQuality {
    fn init(base: Base<Node>) -> Self {
        Self { over_budget_s: 0.0, under_budget_s: 0.0, since_step_s: 0.0, thermal_poll_s: 0.0, governor: Governor::default(), detection: Detection::default(), machine: Machine::default(), overrides: None, extras: None, tier_profile: QualityProfile::default(), base }
    }
    fn ready(&mut self) { self.base_mut().set_process_priority(-60); }
    /// Unit `gd-quality`: poll the Mac thermal state at 1 Hz and run the dynamic-resolution timers.
    fn process(&mut self, delta: f64) {
        self.thermal_poll_s -= delta.max(0.0) as f32;
        let mut changed = false;
        if self.thermal_poll_s <= 0.0 {
            self.thermal_poll_s = 1.0;
            let t = crate::platform::thermal_state();
            if t != self.governor.thermal { self.governor.thermal = t; changed = true; }
        }
        // Settings changed by the player (tier, MSAA, atlas, SSAO, PCSS, dynamic resolution) rebuild the profile.
        let o = Overrides::from_settings(&crate::services::services().settings.borrow());
        if self.overrides != Some(o) { changed |= self.rebuild(o); }
        if changed { self.publish(); }
    }
}

impl BpQuality {
    fn chosen_tier(&self, o: &Overrides) -> QualityTier {
        // The Compatibility renderer cannot run another tier; otherwise the player's choice wins.
        if self.machine.rendering_method == "gl_compatibility" { return QualityTier::Fallback; }
        o.tier.or(self.detection.tier).unwrap_or(QualityTier::Medium)
    }
    /// Rebuild the tier profile; returns true (always publishes).
    fn rebuild(&mut self, o: Overrides) -> bool {
        let tier = self.chosen_tier(&o);
        if self.tier_profile.tier != tier { self.governor.reset(); }
        let apple = is_apple(&self.machine);
        let window = if self.machine.window_px.0 > 0 { self.machine.window_px } else { (1920, 1080) };
        let (p, x) = build_profile(tier, apple, window, &o);
        self.tier_profile = p;
        self.extras = Some(x);
        self.overrides = Some(o);
        true
    }
    fn extras(&self) -> ProfileExtras { self.extras.unwrap_or(ProfileExtras { ssao_level: 1, soft_high: false, glow_bicubic: true, lut_size: hull_mesh::gen_files::LUT_EDGE, apple: false, floor: 0.77 }) }
    fn effective(&self) -> QualityProfile {
        let o = self.overrides.unwrap_or_default();
        effective_profile(self.tier_profile.clone(), &self.extras(), &self.governor, o.dynamic_resolution)
    }
    /// Store the effective profile for the render units and tell the scene scripts.
    fn publish(&mut self) {
        let p = self.effective();
        let changed = { let services = crate::services::services(); let mut q = services.quality.borrow_mut(); let c = *q != p; *q = p; c };
        if changed { self.base_mut().emit_signal("changed", &[]); }
    }
    /// The effective profile and extras, built from the settings when nothing has been built yet.
    fn current(&self) -> (QualityProfile, ProfileExtras) {
        if self.overrides.is_some() { return (self.effective(), self.extras()); }
        let o = Overrides::from_settings(&crate::services::services().settings.borrow());
        let window = if self.machine.window_px.0 > 0 { self.machine.window_px } else { (1920, 1080) };
        let (p, x) = build_profile(self.chosen_tier(&o), is_apple(&self.machine), window, &o);
        (effective_profile(p, &x, &self.governor, o.dynamic_resolution), x)
    }
    fn ensure_built(&mut self) {
        if self.overrides.is_none() { let o = Overrides::from_settings(&crate::services::services().settings.borrow()); self.rebuild(o); }
    }
}

#[godot_api]
impl BpQuality {
    /// `profile()` changed: re-apply viewport and environment settings and call `BpWorldView.apply_quality`.
    #[signal]
    fn changed();
    /// Auto-detect finished (`detect_result()`).
    #[signal]
    fn detected();

    /// The effective profile: `{ "tier": int (Bp.QUALITY_*), "scale_3d": float, "max_fps": int, "msaa": int
    /// (0, 2 or 4), "smaa": bool, "shadow_splits": int, "shadow_distance_m": float, "shadow_atlas": int,
    /// "shadow_filter": StringName (&"soft_low" or &"soft_high"), "ssao": StringName (&"off", &"low",
    /// &"medium"), "ssr": bool, "volumetric_fog": bool, "glow_min": int, "glow_max": int, "glow_bicubic":
    /// bool, "upscaler": StringName (&"none", &"fsr1", &"metalfx_spatial"), "lut_size": int, "degrade_step":
    /// int, "safe_mode": bool }`. `glow_max` 0 means glow off.
    #[func]
    fn profile(&self) -> VarDictionary {
        let (p, x) = self.current();
        let scale = (p.base_scale * p.dynamic_scale).clamp(0.25, 1.0);
        let upscaler = if scale >= 0.999 || p.tier == QualityTier::Fallback { "none" } else if cfg!(target_os = "macos") { "metalfx_spatial" } else { "fsr1" };
        crate::convert::d! {
            "tier" => p.tier.to_gd(),
            "scale_3d" => scale as f64,
            "max_fps" => p.max_fps as i64,
            "msaa" => p.msaa as i64,
            "smaa" => p.smaa,
            "shadow_splits" => p.shadow_splits as i64,
            "shadow_distance_m" => p.shadow_distance_m as f64,
            "shadow_atlas" => p.shadow_atlas as i64,
            "shadow_filter" => StringName::from(if x.soft_high { "soft_high" } else { "soft_low" }),
            "ssao" => StringName::from(match x.ssao_level { 0 => "off", 1 => "low", _ => "medium" }),
            "ssr" => p.ssr,
            "volumetric_fog" => p.volumetric_fog,
            "glow_min" => p.glow_levels.0 as i64,
            "glow_max" => p.glow_levels.1 as i64,
            "glow_bicubic" => x.glow_bicubic,
            "upscaler" => StringName::from(upscaler),
            "lut_size" => x.lut_size as i64,
            "degrade_step" => p.degrade_step as i64,
            "safe_mode" => p.tier == QualityTier::Fallback,
        }
    }
    /// Step 1 of auto-detect: map this machine to a tier from what the engine reports. `machine` keys:
    /// `adapter: String`, `vendor: String`, `driver_version: String`, `api_version: String`,
    /// `processor: String`, `model: String` (`OS.get_model_name()`), `memory_gb: float`,
    /// `rendering_method: String`, `window_px: Vector2i`. Windows VRAM is read in Rust. Returns the tier.
    ///
    /// When the adapter and driver match the last saved detection, that result (including any tier dropped
    /// by the timings) is reused and `changed_since_last` is false, so the script skips the flotilla.
    #[func]
    fn detect(&mut self, machine: VarDictionary) -> i64 {
        let text = |k: &str| crate::convert::get_text(&machine, k).unwrap_or_default();
        let window = machine.get("window_px").and_then(|v| v.try_to::<Vector2i>().ok()).map_or((1920, 1080), |v| (v.x.clamp(1, 16_384), v.y.clamp(1, 16_384)));
        self.machine = Machine {
            adapter: text("adapter"), vendor: text("vendor"), driver_version: text("driver_version"), api_version: text("api_version"),
            processor: text("processor"), model: text("model"), memory_gb: crate::convert::get_f64(&machine, "memory_gb").unwrap_or(0.0).max(0.0),
            rendering_method: text("rendering_method"), window_px: window, vram_bytes: crate::platform::dedicated_video_memory_bytes(),
        };
        let (tier, reason, below) = classify(&self.machine);
        let signature = format!("{}|{}|{}", self.machine.adapter, self.machine.driver_version, self.machine.rendering_method).chars().take(256).collect::<String>();
        let (stored_sig, stored_tier) = { let s = crate::services::services(); let s = s.settings.borrow(); (s.text("graphics.detected_machine"), s.int("graphics.detected_tier")) };
        let changed = stored_sig != signature || QualityTier::from_gd(stored_tier).is_none();
        let tier = if !changed && tier != QualityTier::Fallback { QualityTier::from_gd(stored_tier).unwrap_or(tier) } else { tier };
        self.detection = Detection { tier: Some(tier), reason, below_minimum: below, adapter: clean_name(&self.machine.adapter), changed, signature, dropped: false };
        if changed {
            crate::settings::write("graphics.detected_tier", SettingValue::Int(tier.to_gd()));
            crate::settings::write("graphics.detected_machine", SettingValue::Text(self.detection.signature.clone()));
            crate::settings::write("graphics.detected_smaa", SettingValue::Bool(false));
            crate::settings::write("graphics.detected_two_splits", SettingValue::Bool(false));
        }
        let o = Overrides::from_settings(&crate::services::services().settings.borrow());
        self.rebuild(o);
        self.publish();
        if !changed { self.base_mut().emit_signal("detected", &[]); }
        tier.to_gd()
    }
    /// Step 2: timings of the scripted flotilla and the A/B tests, measured by the scene script:
    /// `{ "p95_ms": float, "msaa_ms": float, "four_splits_ms": float }`. May drop one tier once and pick SMAA
    /// or 2 shadow splits (L2018). Emits `detected`. `msaa_ms` is the GPU (Mac: frame) time MSAA 2x costs over
    /// SMAA 1x, `four_splits_ms` the shadow pass with 4 splits; non-positive values mean "not measured".
    #[func]
    fn detect_timings(&mut self, timings: VarDictionary) {
        self.ensure_built();
        let p95 = crate::convert::get_f64(&timings, "p95_ms").unwrap_or(-1.0);
        let msaa = crate::convert::get_f64(&timings, "msaa_ms").unwrap_or(-1.0);
        let splits = crate::convert::get_f64(&timings, "four_splits_ms").unwrap_or(-1.0);
        let mut tier = self.detection.tier.unwrap_or(QualityTier::Medium);
        let budget = budget_ms(self.tier_profile.max_fps);
        if p95 > budget && !self.detection.dropped {
            let lower = match tier { QualityTier::High => Some(QualityTier::Medium), QualityTier::Medium => Some(QualityTier::Low), _ => None };
            if let Some(lower) = lower {
                self.detection.reason = Msg::new("settings.detect.dropped").arg(MsgArg::Num { value: p95, unit: Unit::None }).arg(MsgArg::Num { value: budget, unit: Unit::None });
                self.detection.dropped = true;
                tier = lower;
            }
        }
        self.detection.tier = Some(tier);
        crate::settings::write("graphics.detected_tier", SettingValue::Int(tier.to_gd()));
        crate::settings::write("graphics.detected_machine", SettingValue::Text(self.detection.signature.clone()));
        crate::settings::write("graphics.detected_smaa", SettingValue::Bool(msaa > 1.0));
        crate::settings::write("graphics.detected_two_splits", SettingValue::Bool(splits > 2.0));
        let o = Overrides::from_settings(&crate::services::services().settings.borrow());
        self.rebuild(o);
        self.publish();
        self.base_mut().emit_signal("detected", &[]);
    }
    /// What auto-detect found and why: `{ "tier": int, "reason": MsgDict, "below_minimum": bool,
    /// "adapter": String, "changed_since_last": bool }`.
    #[func]
    fn detect_result(&self) -> VarDictionary {
        let o = Overrides::from_settings(&crate::services::services().settings.borrow());
        let (tier, reason) = match o.tier {
            Some(t) if self.machine.rendering_method != "gl_compatibility" => (t, Msg::new("settings.detect.override")),
            _ => (self.detection.tier.unwrap_or(QualityTier::Medium), self.detection.reason.clone()),
        };
        crate::convert::d! {
            "tier" => tier.to_gd(),
            "reason" => crate::convert::msg_dict(&reason),
            "below_minimum" => self.detection.below_minimum,
            "adapter" => GString::from(self.detection.adapter.as_str()),
            "changed_since_last" => self.detection.changed,
        }
    }
    /// Feed one frame: CPU frame time and GPU time in ms (-1.0 where the driver cannot measure, L2022).
    #[func]
    fn frame_sample(&mut self, frame_ms: f64, gpu_ms: f64) {
        self.ensure_built();
        // D3D12 and Vulkan report GPU time; Metal's timer is unusable in 4.7, so Macs use frame time (L2022).
        let cost = if gpu_ms.is_finite() && gpu_ms > 0.0 { gpu_ms } else { frame_ms };
        let dt = if frame_ms.is_finite() { frame_ms / 1000.0 } else { 0.0 };
        let o = self.overrides.unwrap_or_default();
        let floor = self.extras().floor;
        if self.governor.sample(cost, dt, budget_ms(self.tier_profile.max_fps), floor, o.dynamic_resolution) { self.publish(); }
        self.over_budget_s = self.governor.over_s as f32;
        self.under_budget_s = self.governor.under_s as f32;
        self.since_step_s = self.governor.since_step_s as f32;
    }
    /// Tell the profile a camera cut happened: resolution steps that would hitch wait for one (L2024).
    #[func]
    fn camera_cut(&mut self) {
        let floor = self.extras().floor;
        if self.governor.camera_cut(floor) { self.publish(); }
    }
    /// Thermal state 0 nominal, 1 fair, 2 serious, 3 critical (macOS; 0 elsewhere).
    #[func]
    fn thermal_state(&self) -> i64 { self.governor.thermal as i64 }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mac(adapter: &str, model: &str, mem: f64) -> Machine { Machine { adapter: adapter.into(), vendor: "Apple".into(), processor: adapter.into(), model: model.into(), memory_gb: mem, rendering_method: "forward_plus".into(), window_px: (2560, 1600), ..Default::default() } }
    fn pc(adapter: &str, vram_gb: f64) -> Machine { Machine { adapter: adapter.into(), vendor: "NVIDIA".into(), memory_gb: 16.0, rendering_method: "forward_plus".into(), window_px: (1920, 1080), vram_bytes: Some((vram_gb * GIB) as u64), ..Default::default() } }

    #[test]
    fn macs_map_by_chip_memory_and_fans() {
        assert_eq!(classify(&mac("Apple M1", "MacBookAir10,1", 16.0)).0, QualityTier::Low);
        assert_eq!(classify(&mac("Apple M3", "Mac15,12", 24.0)).0, QualityTier::Low);
        assert_eq!(classify(&mac("Apple M2", "MacBookAir15,99", 24.0)).0, QualityTier::Low);
        assert_eq!(classify(&mac("Apple M2", "Mac14,7", 8.0)).0, QualityTier::Low);
        assert_eq!(classify(&mac("Apple M2", "Mac14,7", 16.0)).0, QualityTier::Medium);
        assert_eq!(classify(&mac("Apple M3 Pro", "Mac15,6", 18.0)).0, QualityTier::High);
        assert_eq!(classify(&mac("Apple M2 Ultra", "Mac14,14", 64.0)).0, QualityTier::High);
        for m in FANLESS_MODELS { assert_eq!(classify(&mac("Apple M4", m, 32.0)).0, QualityTier::Low, "{m}"); }
    }

    #[test]
    fn windows_maps_by_adapter_and_vram() {
        assert_eq!(classify(&pc("NVIDIA GeForce RTX 3050", 6.0)).0, QualityTier::Medium);
        assert_eq!(classify(&pc("NVIDIA GeForce RTX 3050", 4.0)), (QualityTier::Low, Msg::new("settings.detect.low_vram").arg(MsgArg::Name("NVIDIA GeForce RTX 3050".into())).arg(MsgArg::Num { value: 4.0, unit: Unit::None }), true));
        assert_eq!(classify(&pc("NVIDIA GeForce RTX 3060", 12.0)).0, QualityTier::Medium);
        assert_eq!(classify(&pc("NVIDIA GeForce RTX 3060 Ti", 8.0)).0, QualityTier::High);
        assert_eq!(classify(&pc("NVIDIA GeForce RTX 4060 Laptop GPU", 8.0)).0, QualityTier::High);
        assert_eq!(classify(&pc("NVIDIA GeForce RTX 4050 Laptop GPU", 6.0)).0, QualityTier::Medium);
        assert_eq!(classify(&pc("AMD Radeon RX 6700 XT", 12.0)).0, QualityTier::High);
        assert_eq!(classify(&pc("AMD Radeon RX 6600", 8.0)).0, QualityTier::Medium);
        assert_eq!(classify(&pc("AMD Radeon RX 7600", 8.0)).0, QualityTier::Medium);
        assert_eq!(classify(&pc("AMD Radeon RX 7800 XT", 16.0)).0, QualityTier::High);
        assert_eq!(classify(&pc("Intel(R) UHD Graphics 770", 0.1)).2, true);
        assert_eq!(classify(&pc("Intel(R) Arc(TM) Graphics", 0.1)).0, QualityTier::Low);
        assert_eq!(classify(&pc("Intel(R) Arc(TM) A770 Graphics", 16.0)).0, QualityTier::Medium);
        assert_eq!(classify(&pc("AMD Radeon(TM) Graphics", 0.5)).0, QualityTier::Low);
        assert_eq!(classify(&pc("AMD Radeon 780M Graphics", 2.0)).0, QualityTier::Low);
        let mut compat = pc("NVIDIA GeForce RTX 4090", 24.0);
        compat.rendering_method = "gl_compatibility".into();
        assert_eq!(classify(&compat).0, QualityTier::Fallback);
    }

    #[test]
    fn scales_follow_the_tier_table() {
        // 0.49 at 2560x1600 on Low (L1979).
        let s = base_scale(QualityTier::Low, true, (2560, 1600));
        assert!((s - 0.494).abs() < 0.002, "{s}");
        assert_eq!(base_scale(QualityTier::Low, false, (1280, 720)), 1.0);
        assert_eq!(base_scale(QualityTier::Medium, false, (2560, 1440)), 1.0);
        assert_eq!(base_scale(QualityTier::High, false, (2560, 1440)), 0.77);
        assert_eq!(base_scale(QualityTier::High, false, (1920, 1080)), 1.0);
        let apple_medium = base_scale(QualityTier::Medium, true, (3024, 1964));
        assert!(((3024.0 * 1964.0 * (apple_medium as f64).powi(2)) / 1.0e6 - 1.4).abs() < 0.01);
        // Low's floor keeps 0.7 MP on screen.
        let base = base_scale(QualityTier::Low, true, (2560, 1600));
        let floor = dynamic_floor(QualityTier::Low, base, (2560, 1600));
        let mp = 2560.0 * 1600.0 * ((base * floor) as f64).powi(2) / 1.0e6;
        assert!((mp - 0.7).abs() < 0.02 || floor == DYNAMIC_STEPS[3], "{mp}");
        assert_eq!(Governor::steps(0.77), vec![1.0, 0.87, 0.77]);
        assert_eq!(Governor::steps(0.67), DYNAMIC_STEPS.to_vec());
    }

    #[test]
    fn profiles_match_the_settings_tables() {
        let o = Overrides { dynamic_resolution: true, ..Default::default() };
        let (low, lx) = build_profile(QualityTier::Low, true, (1920, 1080), &o);
        assert_eq!((low.max_fps, low.msaa, low.shadow_splits, low.shadow_distance_m, low.shadow_atlas, low.ssao, low.glow_levels, low.lod_cell_px), (30, 2, 2, 250.0, 2048, false, (2, 3), 3.0));
        assert!(!lx.glow_bicubic);
        let (med, mx) = build_profile(QualityTier::Medium, false, (1920, 1080), &o);
        assert_eq!((med.max_fps, med.msaa, med.shadow_splits, med.shadow_distance_m, mx.ssao_level, med.ssr), (60, 2, 4, 500.0, 1, false));
        let (med_slow, _) = build_profile(QualityTier::Medium, false, (1920, 1080), &Overrides { smaa: true, two_splits: true, ..o });
        assert_eq!((med_slow.msaa, med_slow.smaa, med_slow.shadow_splits), (0, true, 2));
        let (high, hx) = build_profile(QualityTier::High, false, (1920, 1080), &o);
        assert_eq!((high.msaa, high.shadow_distance_m, high.ssr, high.volumetric_fog, hx.ssao_level, hx.soft_high, hx.lut_size), (4, 800.0, true, true, 2, true, 33));
        let (forced, _) = build_profile(QualityTier::High, false, (1920, 1080), &Overrides { msaa: Some(0), ssao: Some(0), shadow_atlas: Some(2048), ..o });
        assert_eq!((forced.msaa, forced.ssao, forced.shadow_atlas), (0, false, 2048));
        let (fb, _) = build_profile(QualityTier::Fallback, false, (1920, 1080), &o);
        assert!(!fb.forward_plus && fb.msaa == 2);
    }

    /// Run the governor for `secs` at a cost.
    fn run(g: &mut Governor, cost: f64, secs: f64, floor: f32) -> u32 {
        let mut changes = 0;
        for _ in 0..(secs * 60.0) as usize { if g.sample(cost, 1.0 / 60.0, 16.7, floor, true) { changes += 1; } }
        changes
    }

    #[test]
    fn resolution_steps_down_after_1s_up_after_5s_3s_apart() {
        let mut g = Governor::default();
        assert_eq!(run(&mut g, 16.0, 0.9, 0.67), 0);
        assert_eq!(run(&mut g, 16.0, 0.2, 0.67), 1);
        assert_eq!(g.scale(0.67), 0.87);
        // Still over budget: the next step waits for the 3 s gap.
        assert_eq!(run(&mut g, 16.0, 2.5, 0.67), 0);
        assert_eq!(run(&mut g, 16.0, 0.6, 0.67), 1);
        assert_eq!(g.scale(0.67), 0.77);
        // Comfortably under budget: up after 5 s.
        assert_eq!(run(&mut g, 8.0, 4.9, 0.67), 0);
        assert_eq!(run(&mut g, 8.0, 0.2, 0.67), 1);
        assert_eq!(g.scale(0.67), 0.87);
        // Between 70% and 90%: nothing moves.
        assert_eq!(run(&mut g, 13.0, 20.0, 0.67), 0);
    }

    #[test]
    fn ladder_continues_past_the_floor_and_unwinds_first() {
        let mut g = Governor::default();
        run(&mut g, 30.0, 60.0, 0.77);
        assert_eq!(g.scale(0.77), 0.77);
        assert_eq!(g.degrade_step(0.77), MAX_DEGRADE_STEP);
        run(&mut g, 5.0, 6.0, 0.77);
        assert_eq!(g.degrade_step(0.77), MAX_DEGRADE_STEP - 1);
        assert_eq!(g.scale(0.77), 0.77);
        run(&mut g, 5.0, 300.0, 0.77);
        assert_eq!((g.degrade_step(0.77), g.scale(0.77)), (0, 1.0));
    }

    #[test]
    fn hitching_steps_wait_for_a_camera_cut() {
        let mut g = Governor::default();
        while !g.sample(16.0, 1.0 / 60.0, 16.7, 0.67, true) {}
        assert_eq!(g.scale(0.67), 0.87);
        // The frame after the step took over two budgets: steps hitch here.
        g.sample(40.0, 1.0 / 60.0, 16.7, 0.67, true);
        assert!(g.hitchy);
        run(&mut g, 16.0, 4.0, 0.67);
        assert_eq!(g.scale(0.67), 0.87, "waits for a cut");
        assert!(g.camera_cut(0.67));
        assert_eq!(g.scale(0.67), 0.77);
        // Without a cut the queued step still goes after the wait limit.
        run(&mut g, 16.0, 3.5, 0.67);
        run(&mut g, 16.0, CUT_WAIT_MAX_S + 0.5, 0.67);
        assert_eq!(g.scale(0.67), 0.67);
    }

    #[test]
    fn thermal_states_force_steps_particles_and_glow() {
        let (p, x) = build_profile(QualityTier::Medium, true, (1920, 1080), &Overrides { dynamic_resolution: true, ..Default::default() });
        let mut g = Governor { thermal: 2, ..Default::default() };
        let e = effective_profile(p.clone(), &x, &g, true);
        assert_eq!((e.dynamic_scale, e.particle_scale, e.glow_levels), (0.87, 0.7, (2, 4)));
        g.thermal = 3;
        let e = effective_profile(p.clone(), &x, &g, true);
        assert_eq!((e.dynamic_scale, e.glow_levels), (x.floor, (0, 0)));
        g.thermal = 0;
        let e = effective_profile(p, &x, &g, true);
        assert_eq!((e.dynamic_scale, e.particle_scale, e.degrade_step), (1.0, 1.0, 0));
    }

    #[test]
    fn degrade_steps_apply_profile_parts() {
        let (p, x) = build_profile(QualityTier::Medium, false, (1920, 1080), &Overrides { dynamic_resolution: true, ..Default::default() });
        let g = Governor { res_index: 2, ladder: 3, ..Default::default() };
        let e = effective_profile(p.clone(), &x, &g, true);
        assert_eq!((e.degrade_step, e.shadow_distance_m, e.glow_levels), (4, 375.0, (2, 4)));
        let g = Governor { res_index: 2, ladder: 5, ..Default::default() };
        assert_eq!(effective_profile(p, &x, &g, true).glow_levels, (0, 0));
    }
}
