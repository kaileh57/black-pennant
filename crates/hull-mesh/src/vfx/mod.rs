//! Contract-owned. Layer A VFX: mesh kinds, caps, the 80-byte instance layout, colour ramps and the emission
//! ladder. Units: `hm-vfx-meshes` (meshes.rs), `hm-vfx-runtime` (runtime.rs).
//! DESIGN.md L1947-1973 (VFX), L2026-2033 (degrade order; never degraded: own fall-of-shot splashes, incoming
//! missile heads, live tracers, hit flashes, fire presence, flooding and list cues, defence smoke).
//!
//! Layer A runs on every renderer: one MultiMesh per [`VfxKind`], one `multimesh_set_buffer` per frame, 80 B per
//! instance. Rust writes age into `custom.x`, never derived from TIME. Opaque kinds use fixed ring slots; a reused
//! slot draws at scale 0 for a frame. Rust sorts blended kinds back to front. Layer B (GPUParticles3D spray,
//! sparks, embers, flak detail, mist) is the client's and has no meshes here.
pub mod meshes;
pub mod runtime;

use crate::types::QualityTier;

/// Bytes per Layer A instance (L1949).
pub const INSTANCE_BYTES: usize = 80;
/// Floats per instance: 12 transform + 4 colour + 4 custom.
pub const INSTANCE_FLOATS: usize = 20;

/// Emission ladder (L1955): values written into the instance colour's alpha-scaled emission by the shaders.
pub mod emission {
    /// Flash 12 to 16 for 2 to 3 frames.
    pub const FLASH: (f32, f32) = (12.0, 16.0);
    pub const FLASH_FRAMES: (u8, u8) = (2, 3);
    /// Tracers and sparks 4 to 8.
    pub const TRACER: (f32, f32) = (4.0, 8.0);
    /// Fire 2 to 4.
    pub const FIRE: (f32, f32) = (2.0, 4.0);
    /// Everything else 1 or less.
    pub const OTHER_MAX: f32 = 1.0;
    /// Reduce flashing caps emission at 4 and removes lightning (L1957).
    pub const REDUCE_FLASHING_CAP: f32 = 4.0;
    /// Accessibility floor 6: flashes blooming over this screen fraction count against the global budget.
    pub const FLASH_SCREEN_FRACTION: f32 = 0.025;
    /// At most this many counted flashes per second; extras in the same third of a second draw at 25% emission
    /// with no light (L1957).
    pub const FLASHES_PER_S: u32 = 3;
    pub const EXTRA_FLASH_EMISSION: f32 = 0.25;
}

/// One Layer A mesh type. Each is one MultiMesh on the client.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum VfxKind {
    /// Opaque emissive box from the projectile buffer, speed x 1/40 s long, at least 1.5 px wide (L1961).
    Tracer,
    /// White hex prism; 3 to 5 per splash (L1962).
    SplashColumn,
    /// Foam ring left by a splash for 6 s.
    SplashFoam,
    /// 2-frame hit flash (L1963).
    HitFlash,
    /// Icosphere ramping white to grey over 1.2 s; 3 to 7 per hit.
    HitPuff,
    /// 0.5 m cube flying from a hit; up to 64 per hit.
    Debris,
    Fireball,
    /// Emissive flame prism over a burning compartment (L1964).
    FlamePrism,
    /// Opaque faceted smoke puff (fires 2 to 4 per s, muzzle smoke).
    SmokePuff,
    /// Defence smoke screen puff: a sim object, the same on every tier (L1965); never culled.
    DefenceSmoke,
    FlakBurst,
    /// Sorted blended cross-quads every 0.1 s, 12 s long (6 s on Low); the 2 s head is never trimmed (L1966).
    MissileTrail,
    /// Wake ribbon segment 1.0 to 2.5 beams wide, 30 s (L1967).
    WakeSegment,
    /// Kelvin arm segment at 19.47 degrees, above 8 kn.
    KelvinArm,
    /// Bow wave sheet scaled by speed.
    BowWave,
    /// Emissive muzzle star, 2 to 3 frames (L1968).
    MuzzleStar,
    /// Blended muzzle haze (one of the three alpha-blended kinds, L1953).
    MuzzleHaze,
    /// Pump discharge arc (L1969).
    PumpArc,
    /// Boil ring at a hole below the waterline.
    BoilRing,
    /// Foam band along the hull, growing as freeboard drops.
    FoamBand,
    /// Chaff cloud facets and flare burn (decoys).
    ChaffCloud,
    FlareBurn,
}
impl VfxKind {
    pub const ALL: [VfxKind; 22] = [Self::Tracer, Self::SplashColumn, Self::SplashFoam, Self::HitFlash, Self::HitPuff, Self::Debris, Self::Fireball, Self::FlamePrism, Self::SmokePuff, Self::DefenceSmoke, Self::FlakBurst, Self::MissileTrail, Self::WakeSegment, Self::KelvinArm, Self::BowWave, Self::MuzzleStar, Self::MuzzleHaze, Self::PumpArc, Self::BoilRing, Self::FoamBand, Self::ChaffCloud, Self::FlareBurn];
    /// Alpha blending only for mist, missile trails and muzzle haze (L1953); these are sorted back to front.
    pub fn blended(self) -> bool { matches!(self, Self::MissileTrail | Self::MuzzleHaze) }
    pub fn cap_group(self) -> CapGroup {
        match self {
            Self::Tracer => CapGroup::Projectiles,
            Self::SplashColumn | Self::SplashFoam => CapGroup::Splashes,
            Self::Fireball | Self::HitFlash | Self::HitPuff => CapGroup::Fireballs,
            Self::FlamePrism => CapGroup::FlamePrisms,
            Self::SmokePuff => CapGroup::SmokePuffs,
            Self::DefenceSmoke => CapGroup::Protected,
            Self::Debris => CapGroup::Debris,
            Self::FlakBurst | Self::ChaffCloud | Self::FlareBurn => CapGroup::Flak,
            Self::WakeSegment | Self::KelvinArm | Self::BowWave | Self::FoamBand => CapGroup::Wake,
            Self::MissileTrail => CapGroup::Trails,
            Self::MuzzleStar | Self::MuzzleHaze | Self::PumpArc | Self::BoilRing => CapGroup::Minor,
        }
    }
    /// Colour ramp the kind's shader samples by normalised age.
    pub fn ramp(self) -> RampId {
        match self {
            Self::Tracer | Self::MuzzleStar | Self::HitFlash | Self::FlareBurn => RampId::Flash,
            Self::SplashColumn | Self::SplashFoam | Self::WakeSegment | Self::KelvinArm | Self::BowWave | Self::FoamBand | Self::BoilRing | Self::PumpArc => RampId::Foam,
            Self::Fireball | Self::FlamePrism => RampId::Fire,
            Self::HitPuff => RampId::HitPuff,
            Self::SmokePuff | Self::MuzzleHaze => RampId::Smoke,
            Self::DefenceSmoke => RampId::DefenceSmoke,
            Self::Debris => RampId::Debris,
            Self::FlakBurst | Self::ChaffCloud => RampId::Flak,
            Self::MissileTrail => RampId::Trail,
        }
    }
}

/// Live hard caps, Medium / Low (L1972); the oldest unprotected slot recycles. High uses Medium caps; Fallback
/// uses Low caps.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum CapGroup { Projectiles, Splashes, Fireballs, FlamePrisms, SmokePuffs, Debris, Flak, Wake, Trails, Protected, Minor }
impl CapGroup {
    /// Cap in live instances. `Splashes` counts splash columns plus foam rings (the spec's cap counts splashes;
    /// DECISION: 5 instances per splash, so 256 splashes = 1,280 instances). `Minor` (muzzle, pump and boil
    /// effects, not in the spec's table) is this contract's placeholder. `Protected` has no cap: defence smoke is
    /// bounded by the sim.
    pub fn cap(self, tier: QualityTier) -> usize {
        let low = matches!(tier, QualityTier::Low | QualityTier::Fallback);
        let (m, l) = match self {
            Self::Projectiles => (2_000, 2_000),
            Self::Splashes => (256 * 5, 128 * 5),
            Self::Fireballs => (64, 32),
            Self::FlamePrisms => (1_024, 384),
            Self::SmokePuffs => (4_096, 1_536),
            Self::Debris => (2_048, 512),
            Self::Flak => (512, 192),
            Self::Wake => (2_560, 1_280),
            Self::Trails => (2_048, 1_024),
            Self::Protected => (usize::MAX, usize::MAX),
            Self::Minor => (512, 256),
        };
        if low { l } else { m }
    }
}
/// Layer B particle caps, Medium / Low (L1972).
pub fn layer_b_particle_cap(tier: QualityTier) -> usize { match tier { QualityTier::Fallback => 0, QualityTier::Low => 8_192, _ => 32_768 } }
/// VFX light budget (L1971): 16 on Medium and High (8 for fires), 8 on Low, 0 on the fallback.
pub fn vfx_light_budget(tier: QualityTier) -> (usize, usize) { match tier { QualityTier::Fallback => (0, 0), QualityTier::Low => (8, 8), _ => (16, 8) } }
/// Own-side splash slots that are never culled (L1962).
pub const PROTECTED_OWN_SPLASHES: usize = 64;

/// A 256x1 RGBA8 colour ramp posterised to 4..=6 bands, generated from the world tokens by `cargo xtask gen`
/// (L1788, L1953); path `gen_files::ramp_path(id.name())`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum RampId { Flash, Foam, Fire, HitPuff, Smoke, DefenceSmoke, Debris, Flak, Trail }
impl RampId {
    pub const ALL: [RampId; 9] = [Self::Flash, Self::Foam, Self::Fire, Self::HitPuff, Self::Smoke, Self::DefenceSmoke, Self::Debris, Self::Flak, Self::Trail];
    pub fn name(self) -> &'static str {
        match self { Self::Flash => "flash", Self::Foam => "foam", Self::Fire => "fire", Self::HitPuff => "hit_puff", Self::Smoke => "smoke", Self::DefenceSmoke => "defence_smoke", Self::Debris => "debris", Self::Flak => "flak", Self::Trail => "trail" }
    }
}

/// One Layer A instance in Godot's MultiMesh 3D buffer order (`use_colors` and `use_custom_data` on):
/// the 3x4 transform row-major (basis.x.x, basis.y.x, basis.z.x, origin.x, basis.x.y, ... origin.z), then colour
/// RGBA, then custom. `custom.x` = age in seconds (written by Rust, never from TIME), `custom.y` = lifetime in
/// seconds, `custom.z` = per-instance seed 0..1, `custom.w` = kind-specific (emission scale after the flash
/// limiter, or calibre factor for splashes).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct VfxInstance { pub xform: [f32; 12], pub color: [f32; 4], pub custom: [f32; 4] }
impl VfxInstance {
    /// A zero-scale instance (a reused slot's first frame, or an empty slot).
    pub const HIDDEN: VfxInstance = VfxInstance { xform: [0.0; 12], color: [0.0; 4], custom: [0.0; 4] };
    pub fn write(&self, out: &mut [f32]) {
        out[..12].copy_from_slice(&self.xform); out[12..16].copy_from_slice(&self.color); out[16..20].copy_from_slice(&self.custom);
    }
}
