//! audio: every sound in the game, synthesized offline with fundsp 0.23.0. DESIGN.md "Art and audio"
//! (L1239-1247): every sound effect; the alert sounds, one distinct pattern per sounded alert priority (L1676-1685);
//! the radio effect (squelch, static and band-pass) that accompanies officer and ace text, never voiced (L1706);
//! one short menu theme. `cargo xtask gen` renders them offline to 48 kHz 16-bit WAV under `res://audio/gen/`.
//! Nothing is synthesized live. Godot plays the files; Rust sets pitch and volume from sim state for engines, sea
//! and weather ([`mix`]). There is no music in sorties. L59 and CLAUDE.md: no generative audio models and no
//! third-party samples; every sound is a patch in this crate.
//!
//! This crate depends on nothing in the workspace, so it defines its own small enums; gdclient maps sim events
//! (`PhysEvent`, alerts, officer messages) to [`SoundId`]s, helped by [`mix`].
//!
//! DECISION (the spec lists no individual effects): the catalogue below is derived from `sim_core::PhysEvent`
//! (fires, launches, splashes, impacts, bursts, fire, flooding, breaches, structure, collisions, grounding),
//! `content::part::EngineKind`, the WMO sea-state range and `content::weather::WeatherKind`, the four alert
//! priorities, the radio effect, editor and HUD interactions, and the menu theme.
//!
//! Module map: contract-owned: this file. Units: `audio-render` [`render`], `audio-weapons`
//! `patches/weapons.rs`, `audio-impacts` `patches/impacts.rs`, `audio-ambience` `patches/ambience.rs`,
//! `audio-signals` `patches/signals.rs` (alerts, radio, UI), `audio-theme` `patches/theme.rs`, `audio-mix` [`mix`].
//!
//! Determinism: every patch is seeded from [`SoundId::seed`] (fundsp `set_hash`), renders single-threaded, and
//! uses libm for any math outside fundsp, so two runs give byte-identical files (L1078).
#![allow(dead_code, unused_variables, unused_imports)]

pub mod analysis;
pub mod mix;
pub mod patches;
pub mod render;

/// Output sample rate (L1246).
pub const SAMPLE_RATE: u32 = 48_000;
/// Output bit depth (L1246).
pub const BITS_PER_SAMPLE: u16 = 16;
/// Godot directory of the rendered files, relative to the project root (`res://audio/gen/`).
pub const GEN_DIR: &str = "audio/gen";

/// Gun and launcher sounds. Calibre bands follow `content::part::weapons::CalibreBand` (L565-570).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum WeaponSfx {
    /// 20-40 mm autocannon report, near.
    GunAuto,
    /// 57-76 mm report, near.
    GunMedium,
    /// 100-127 mm report, near.
    GunLarge,
    /// 130-155 mm report, near (Act 3).
    GunHeavy,
    /// Distant report of auto and medium guns (low-passed, rolling).
    GunDistantLight,
    /// Distant report of large and heavy guns.
    GunDistantHeavy,
    /// Point-defence burst (rotary).
    Ciws,
    MissileLaunch,
    SamLaunch,
    /// Loop on a flying missile body.
    MissileMotor,
    TorpedoLaunch,
    /// Depth charge rolling off the rack (Act 3).
    DepthChargeRoll,
    MineDrop,
    /// Chaff and flare mortar.
    DecoyLaunch,
    SmokeLaunch,
    /// Shell passing overhead (supersonic crack and whistle).
    ShellPassby,
    /// Loop while a mount slews.
    TurretTrain,
    DroneLaunch,
    /// Loop on a drone body.
    DroneMotor,
    /// Strike aircraft pass (Act 2).
    AircraftPass,
    /// Loop on a helicopter (Act 2).
    HelicopterRotor,
    /// Loop on a RHIB.
    RhibOutboard,
}
impl WeaponSfx {
    pub const ALL: [WeaponSfx; 22] = [Self::GunAuto, Self::GunMedium, Self::GunLarge, Self::GunHeavy, Self::GunDistantLight, Self::GunDistantHeavy, Self::Ciws, Self::MissileLaunch, Self::SamLaunch, Self::MissileMotor, Self::TorpedoLaunch, Self::DepthChargeRoll, Self::MineDrop, Self::DecoyLaunch, Self::SmokeLaunch, Self::ShellPassby, Self::TurretTrain, Self::DroneLaunch, Self::DroneMotor, Self::AircraftPass, Self::HelicopterRotor, Self::RhibOutboard];
}

/// Hits, bursts, damage, fire and flooding.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ImpactSfx {
    /// Splash of an autocannon round.
    SplashSmall,
    /// Splash of a 57-76 mm shell.
    SplashMedium,
    /// Splash of a 100 mm and larger shell or a missile.
    SplashLarge,
    Ricochet,
    /// Shell penetrating plating.
    PenetrateHit,
    /// Autocannon rounds striking a hull.
    AutocannonHit,
    ExplosionSmall,
    ExplosionLarge,
    /// Magazine cook-off.
    CookOff,
    /// Torpedo, mine or depth charge burst under water.
    UnderwaterBurst,
    /// Proximity burst in the air (SAM, flak, missile intercept).
    AirBurst,
    Collision,
    Grounding,
    /// Structural stress creak (heavy damage, list).
    HullGroan,
    /// A piece splitting off.
    HullBreak,
    DebrisFall,
    /// Loop for a burning compartment.
    FireLoop,
    FireIgnite,
    /// Loop of water entering a breached compartment.
    FloodingLoop,
    /// The first rush of a new breach below the waterline.
    BreachRush,
    /// Loop of a running pump.
    PumpLoop,
    /// Fire put out by water or flooding.
    SteamHiss,
}
impl ImpactSfx {
    pub const ALL: [ImpactSfx; 22] = [Self::SplashSmall, Self::SplashMedium, Self::SplashLarge, Self::Ricochet, Self::PenetrateHit, Self::AutocannonHit, Self::ExplosionSmall, Self::ExplosionLarge, Self::CookOff, Self::UnderwaterBurst, Self::AirBurst, Self::Collision, Self::Grounding, Self::HullGroan, Self::HullBreak, Self::DebrisFall, Self::FireLoop, Self::FireIgnite, Self::FloodingLoop, Self::BreachRush, Self::PumpLoop, Self::SteamHiss];
}

/// Engines, sea, weather and sonar. Loops get pitch and volume from sim state through [`mix`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum AmbienceSfx {
    EngineDiesel,
    EngineGasTurbine,
    EngineElectric,
    /// Propeller cavitation above the cavitation speed (L453).
    Cavitation,
    HullWashSlow,
    HullWashFast,
    /// Sea loops by WMO sea state: calm 0-2, moderate 3-4, rough 5-6, storm 7-8.
    SeaCalm,
    SeaModerate,
    SeaRough,
    SeaStorm,
    Wind,
    WindGale,
    Rain,
    RainHeavy,
    ThunderNear,
    ThunderFar,
    /// Own active sonar transmission.
    SonarPing,
    /// Another unit's active sonar heard through the hull.
    SonarPingHeard,
}
impl AmbienceSfx {
    pub const ALL: [AmbienceSfx; 18] = [Self::EngineDiesel, Self::EngineGasTurbine, Self::EngineElectric, Self::Cavitation, Self::HullWashSlow, Self::HullWashFast, Self::SeaCalm, Self::SeaModerate, Self::SeaRough, Self::SeaStorm, Self::Wind, Self::WindGale, Self::Rain, Self::RainHeavy, Self::ThunderNear, Self::ThunderFar, Self::SonarPing, Self::SonarPingHeard];
}

/// One distinct pattern per sounded alert priority (L1680-1685).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum AlertSound {
    /// Seamless loop: plays continuously until acknowledged and rectified.
    Emergency,
    /// One cycle of a repeating pattern; the client repeats it until acknowledged or rectified.
    Alarm,
    /// A chime; the client plays it again at 120 s while unacknowledged.
    Warning,
    /// One tick.
    Caution,
}
impl AlertSound { pub const ALL: [AlertSound; 4] = [Self::Emergency, Self::Alarm, Self::Warning, Self::Caution]; }

/// The radio effect around officer and ace text (L1706): squelch, static and band-pass. Text appears no later
/// than the sound.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum RadioSfx {
    SquelchOpen,
    /// Band-passed static bed, looped for as long as the line shows.
    Static,
    SquelchClose,
    /// Ace transmissions: a heterodyne-tinged open on the enemy net.
    AceOpen,
}
impl RadioSfx { pub const ALL: [RadioSfx; 4] = [Self::SquelchOpen, Self::Static, Self::SquelchClose, Self::AceOpen]; }

/// Interface sounds (editor, HUD, menus). Short, quiet, unpitched where possible.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum UiSfx {
    Click,
    Select,
    /// Part or kit placed in the editor.
    Place,
    Remove,
    /// Placement refused or command rejected.
    Invalid,
    Confirm,
    /// Ghost rotated (R, Shift+R, T).
    Rotate,
    /// Kit socket snapped.
    Snap,
    /// Time compression stepped.
    TimeRate,
    Pause,
    Resume,
    /// Tutorial step completed.
    TutorialStep,
    ObjectiveDone,
    /// Design saved (one of the few allowed toasts).
    Saved,
}
impl UiSfx { pub const ALL: [UiSfx; 14] = [Self::Click, Self::Select, Self::Place, Self::Remove, Self::Invalid, Self::Confirm, Self::Rotate, Self::Snap, Self::TimeRate, Self::Pause, Self::Resume, Self::TutorialStep, Self::ObjectiveDone, Self::Saved]; }

/// Every sound file the game ships.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SoundId {
    Weapon(WeaponSfx),
    Impact(ImpactSfx),
    Ambience(AmbienceSfx),
    Alert(AlertSound),
    Radio(RadioSfx),
    Ui(UiSfx),
    /// One short menu theme (L1244). Never played in sorties.
    MenuTheme,
}

/// How the client plays a file.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Playback {
    OneShot,
    /// Seamless loop: the file's end joins its start with no click. Loop points are written into the WAV `smpl`
    /// chunk so Godot's importer (loop mode "Detect from WAV") picks them up.
    Loop,
}

/// Godot audio bus the client routes a sound to. Bus names in the Godot layout match [`Bus::name`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Bus { Sfx, Ambience, Alert, Radio, Ui, Music }
impl Bus {
    pub fn name(self) -> &'static str { match self { Bus::Sfx => "Sfx", Bus::Ambience => "Ambience", Bus::Alert => "Alert", Bus::Radio => "Radio", Bus::Ui => "Ui", Bus::Music => "Music" } }
}

/// Static facts about one sound, shared by the renderer, `cargo xtask gen` and gdclient.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SoundSpec {
    pub id: SoundId,
    /// File stem; the file is `audio/gen/<stem>.wav`.
    pub stem: &'static str,
    pub playback: Playback,
    /// 1 (positional 3D sounds, UI) or 2 (ambience beds, theme).
    pub channels: u16,
    /// Rendered length in seconds (loops: one loop period).
    pub duration_s: f32,
    pub bus: Bus,
    /// Positional: played from an `AudioStreamPlayer3D` at the event; else 2D.
    pub positional: bool,
    /// Sample-peak ceiling, dBFS (at most `render::CEILING_DBFS`). The level itself is a loudness target per
    /// sound (`analysis::target`), which `render::level` meets.
    pub peak_dbfs: f32,
}

const fn s(id: SoundId, stem: &'static str, playback: Playback, channels: u16, duration_s: f32, bus: Bus, positional: bool, peak_dbfs: f32) -> SoundSpec {
    SoundSpec { id, stem, playback, channels, duration_s, bus, positional, peak_dbfs }
}

impl SoundId {
    /// Every sound, in a fixed order (families in declaration order, variants in declaration order).
    pub fn all() -> Vec<SoundId> {
        let mut v = Vec::new();
        v.extend(WeaponSfx::ALL.iter().map(|&x| SoundId::Weapon(x)));
        v.extend(ImpactSfx::ALL.iter().map(|&x| SoundId::Impact(x)));
        v.extend(AmbienceSfx::ALL.iter().map(|&x| SoundId::Ambience(x)));
        v.extend(AlertSound::ALL.iter().map(|&x| SoundId::Alert(x)));
        v.extend(RadioSfx::ALL.iter().map(|&x| SoundId::Radio(x)));
        v.extend(UiSfx::ALL.iter().map(|&x| SoundId::Ui(x)));
        v.push(SoundId::MenuTheme);
        v
    }
    /// Deterministic seed for this sound's patch (FNV-1a of the stem).
    pub fn seed(self) -> u64 {
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        for b in self.spec().stem.bytes() { h ^= b as u64; h = h.wrapping_mul(0x0000_0100_0000_01b3); }
        h
    }
    /// The spec table. Patch owners may change their own rows' `duration_s` and `peak_dbfs` (never stems,
    /// playback or channels without the client owner). Levels are loudness targets in `analysis::target`.
    pub fn spec(self) -> SoundSpec {
        use Bus::*; use Playback::*;
        match self {
            SoundId::Weapon(w) => { let id = self; match w {
                WeaponSfx::GunAuto => s(id, "weapon_gun_auto", OneShot, 1, 0.6, Sfx, true, -1.0),
                WeaponSfx::GunMedium => s(id, "weapon_gun_medium", OneShot, 1, 1.6, Sfx, true, -1.0),
                WeaponSfx::GunLarge => s(id, "weapon_gun_large", OneShot, 1, 2.4, Sfx, true, -1.0),
                WeaponSfx::GunHeavy => s(id, "weapon_gun_heavy", OneShot, 1, 3.2, Sfx, true, -1.0),
                WeaponSfx::GunDistantLight => s(id, "weapon_gun_distant_light", OneShot, 1, 2.0, Sfx, true, -1.0),
                WeaponSfx::GunDistantHeavy => s(id, "weapon_gun_distant_heavy", OneShot, 1, 3.5, Sfx, true, -1.0),
                WeaponSfx::Ciws => s(id, "weapon_ciws", OneShot, 1, 1.5, Sfx, true, -1.0),
                WeaponSfx::MissileLaunch => s(id, "weapon_missile_launch", OneShot, 1, 3.0, Sfx, true, -1.0),
                WeaponSfx::SamLaunch => s(id, "weapon_sam_launch", OneShot, 1, 2.2, Sfx, true, -1.0),
                WeaponSfx::MissileMotor => s(id, "weapon_missile_motor", Loop, 1, 2.0, Sfx, true, -1.0),
                WeaponSfx::TorpedoLaunch => s(id, "weapon_torpedo_launch", OneShot, 1, 2.0, Sfx, true, -1.0),
                WeaponSfx::DepthChargeRoll => s(id, "weapon_depth_charge_roll", OneShot, 1, 2.2, Sfx, true, -1.0),
                WeaponSfx::MineDrop => s(id, "weapon_mine_drop", OneShot, 1, 1.5, Sfx, true, -1.0),
                WeaponSfx::DecoyLaunch => s(id, "weapon_decoy_launch", OneShot, 1, 1.2, Sfx, true, -1.0),
                WeaponSfx::SmokeLaunch => s(id, "weapon_smoke_launch", OneShot, 1, 1.5, Sfx, true, -1.0),
                WeaponSfx::ShellPassby => s(id, "weapon_shell_passby", OneShot, 1, 1.4, Sfx, true, -1.0),
                WeaponSfx::TurretTrain => s(id, "weapon_turret_train", Loop, 1, 1.0, Sfx, true, -1.0),
                WeaponSfx::DroneLaunch => s(id, "weapon_drone_launch", OneShot, 1, 1.5, Sfx, true, -1.0),
                WeaponSfx::DroneMotor => s(id, "weapon_drone_motor", Loop, 1, 2.0, Sfx, true, -1.0),
                WeaponSfx::AircraftPass => s(id, "weapon_aircraft_pass", OneShot, 1, 5.0, Sfx, true, -1.0),
                WeaponSfx::HelicopterRotor => s(id, "weapon_helicopter_rotor", Loop, 1, 2.0, Sfx, true, -1.0),
                WeaponSfx::RhibOutboard => s(id, "weapon_rhib_outboard", Loop, 1, 2.0, Sfx, true, -1.0),
            } }
            SoundId::Impact(i) => { let id = self; match i {
                ImpactSfx::SplashSmall => s(id, "impact_splash_small", OneShot, 1, 1.0, Sfx, true, -1.0),
                ImpactSfx::SplashMedium => s(id, "impact_splash_medium", OneShot, 1, 2.0, Sfx, true, -1.0),
                ImpactSfx::SplashLarge => s(id, "impact_splash_large", OneShot, 1, 3.5, Sfx, true, -1.0),
                ImpactSfx::Ricochet => s(id, "impact_ricochet", OneShot, 1, 1.1, Sfx, true, -1.0),
                ImpactSfx::PenetrateHit => s(id, "impact_penetrate", OneShot, 1, 1.2, Sfx, true, -1.0),
                ImpactSfx::AutocannonHit => s(id, "impact_autocannon_hit", OneShot, 1, 0.5, Sfx, true, -1.0),
                ImpactSfx::ExplosionSmall => s(id, "impact_explosion_small", OneShot, 1, 2.0, Sfx, true, -1.0),
                ImpactSfx::ExplosionLarge => s(id, "impact_explosion_large", OneShot, 1, 4.0, Sfx, true, -1.0),
                ImpactSfx::CookOff => s(id, "impact_cook_off", OneShot, 1, 6.0, Sfx, true, -1.0),
                ImpactSfx::UnderwaterBurst => s(id, "impact_underwater_burst", OneShot, 1, 3.0, Sfx, true, -1.0),
                ImpactSfx::AirBurst => s(id, "impact_air_burst", OneShot, 1, 1.5, Sfx, true, -1.0),
                ImpactSfx::Collision => s(id, "impact_collision", OneShot, 1, 2.0, Sfx, true, -1.0),
                ImpactSfx::Grounding => s(id, "impact_grounding", OneShot, 1, 3.0, Sfx, true, -1.0),
                ImpactSfx::HullGroan => s(id, "impact_hull_groan", OneShot, 1, 3.0, Sfx, true, -1.0),
                ImpactSfx::HullBreak => s(id, "impact_hull_break", OneShot, 1, 3.0, Sfx, true, -1.0),
                ImpactSfx::DebrisFall => s(id, "impact_debris_fall", OneShot, 1, 2.0, Sfx, true, -1.0),
                ImpactSfx::FireLoop => s(id, "impact_fire_loop", Loop, 1, 4.0, Sfx, true, -1.0),
                ImpactSfx::FireIgnite => s(id, "impact_fire_ignite", OneShot, 1, 1.2, Sfx, true, -1.0),
                ImpactSfx::FloodingLoop => s(id, "impact_flooding_loop", Loop, 1, 4.0, Sfx, true, -1.0),
                ImpactSfx::BreachRush => s(id, "impact_breach_rush", OneShot, 1, 2.0, Sfx, true, -1.0),
                ImpactSfx::PumpLoop => s(id, "impact_pump_loop", Loop, 1, 2.0, Sfx, true, -1.0),
                ImpactSfx::SteamHiss => s(id, "impact_steam_hiss", OneShot, 1, 2.0, Sfx, true, -1.0),
            } }
            SoundId::Ambience(a) => { let id = self; match a {
                AmbienceSfx::EngineDiesel => s(id, "ambience_engine_diesel", Loop, 1, 2.0, Ambience, true, -1.0),
                AmbienceSfx::EngineGasTurbine => s(id, "ambience_engine_gas_turbine", Loop, 1, 2.0, Ambience, true, -1.0),
                AmbienceSfx::EngineElectric => s(id, "ambience_engine_electric", Loop, 1, 2.0, Ambience, true, -1.0),
                AmbienceSfx::Cavitation => s(id, "ambience_cavitation", Loop, 1, 2.0, Ambience, true, -1.0),
                AmbienceSfx::HullWashSlow => s(id, "ambience_hull_wash_slow", Loop, 2, 8.0, Ambience, false, -1.0),
                AmbienceSfx::HullWashFast => s(id, "ambience_hull_wash_fast", Loop, 2, 8.0, Ambience, false, -1.0),
                AmbienceSfx::SeaCalm => s(id, "ambience_sea_calm", Loop, 2, 16.0, Ambience, false, -1.0),
                AmbienceSfx::SeaModerate => s(id, "ambience_sea_moderate", Loop, 2, 16.0, Ambience, false, -1.0),
                AmbienceSfx::SeaRough => s(id, "ambience_sea_rough", Loop, 2, 16.0, Ambience, false, -1.0),
                AmbienceSfx::SeaStorm => s(id, "ambience_sea_storm", Loop, 2, 16.0, Ambience, false, -1.0),
                AmbienceSfx::Wind => s(id, "ambience_wind", Loop, 2, 12.0, Ambience, false, -1.0),
                AmbienceSfx::WindGale => s(id, "ambience_wind_gale", Loop, 2, 12.0, Ambience, false, -1.0),
                AmbienceSfx::Rain => s(id, "ambience_rain", Loop, 2, 8.0, Ambience, false, -1.0),
                AmbienceSfx::RainHeavy => s(id, "ambience_rain_heavy", Loop, 2, 8.0, Ambience, false, -1.0),
                AmbienceSfx::ThunderNear => s(id, "ambience_thunder_near", OneShot, 2, 6.0, Ambience, false, -1.0),
                AmbienceSfx::ThunderFar => s(id, "ambience_thunder_far", OneShot, 2, 8.0, Ambience, false, -1.0),
                AmbienceSfx::SonarPing => s(id, "ambience_sonar_ping", OneShot, 1, 3.0, Ambience, false, -1.0),
                AmbienceSfx::SonarPingHeard => s(id, "ambience_sonar_ping_heard", OneShot, 1, 3.0, Ambience, false, -1.0),
            } }
            SoundId::Alert(a) => { let id = self; match a {
                AlertSound::Emergency => s(id, "alert_emergency", Loop, 1, 2.0, Alert, false, -1.0),
                AlertSound::Alarm => s(id, "alert_alarm", OneShot, 1, 1.5, Alert, false, -1.0),
                AlertSound::Warning => s(id, "alert_warning", OneShot, 1, 1.8, Alert, false, -1.0),
                AlertSound::Caution => s(id, "alert_caution", OneShot, 1, 0.15, Alert, false, -1.0),
            } }
            SoundId::Radio(r) => { let id = self; match r {
                RadioSfx::SquelchOpen => s(id, "radio_squelch_open", OneShot, 1, 0.25, Radio, false, -1.0),
                RadioSfx::Static => s(id, "radio_static", Loop, 1, 3.0, Radio, false, -1.0),
                RadioSfx::SquelchClose => s(id, "radio_squelch_close", OneShot, 1, 0.3, Radio, false, -1.0),
                RadioSfx::AceOpen => s(id, "radio_ace_open", OneShot, 1, 0.6, Radio, false, -1.0),
            } }
            SoundId::Ui(u) => { let id = self; match u {
                UiSfx::Click => s(id, "ui_click", OneShot, 1, 0.05, Ui, false, -1.0),
                UiSfx::Select => s(id, "ui_select", OneShot, 1, 0.08, Ui, false, -1.0),
                UiSfx::Place => s(id, "ui_place", OneShot, 1, 0.15, Ui, false, -1.0),
                UiSfx::Remove => s(id, "ui_remove", OneShot, 1, 0.15, Ui, false, -1.0),
                UiSfx::Invalid => s(id, "ui_invalid", OneShot, 1, 0.2, Ui, false, -1.0),
                UiSfx::Confirm => s(id, "ui_confirm", OneShot, 1, 0.2, Ui, false, -1.0),
                UiSfx::Rotate => s(id, "ui_rotate", OneShot, 1, 0.08, Ui, false, -1.0),
                UiSfx::Snap => s(id, "ui_snap", OneShot, 1, 0.1, Ui, false, -1.0),
                UiSfx::TimeRate => s(id, "ui_time_rate", OneShot, 1, 0.2, Ui, false, -1.0),
                UiSfx::Pause => s(id, "ui_pause", OneShot, 1, 0.25, Ui, false, -1.0),
                UiSfx::Resume => s(id, "ui_resume", OneShot, 1, 0.25, Ui, false, -1.0),
                UiSfx::TutorialStep => s(id, "ui_tutorial_step", OneShot, 1, 0.5, Ui, false, -1.0),
                UiSfx::ObjectiveDone => s(id, "ui_objective_done", OneShot, 1, 0.8, Ui, false, -1.0),
                UiSfx::Saved => s(id, "ui_saved", OneShot, 1, 0.3, Ui, false, -1.0),
            } }
            SoundId::MenuTheme => s(self, "menu_theme", Loop, 2, 64.0, Music, false, -1.0),
        }
    }
    /// File name inside [`GEN_DIR`], e.g. `weapon_gun_medium.wav`.
    pub fn file_name(self) -> String { format!("{}.wav", self.spec().stem) }
    /// Godot path, e.g. `res://audio/gen/weapon_gun_medium.wav`.
    pub fn res_path(self) -> String { format!("res://{}/{}", GEN_DIR, self.file_name()) }
}

/// Rendered 16-bit PCM, interleaved.
#[derive(Clone, Debug, PartialEq)]
pub struct Pcm16 {
    pub sample_rate: u32,
    pub channels: u16,
    pub samples: Vec<i16>,
    /// Loop start and end in frames for `Playback::Loop` sounds (end exclusive), written to the `smpl` chunk.
    pub loop_frames: Option<(u32, u32)>,
}
impl Pcm16 {
    pub fn frames(&self) -> usize { self.samples.len() / self.channels.max(1) as usize }
}

/// Render one sound: run its patch (dispatching to the family unit), then `render::finish` it (level it to its
/// loudness target under the `peak_dbfs` ceiling, seam loops, fade one-shots, quantise with seeded TPDF dither). Deterministic.
pub fn render(id: SoundId) -> Pcm16 {
    let spec = id.spec();
    let raw = match id {
        SoundId::Weapon(w) => patches::weapons::render(w, &spec, id.seed()),
        SoundId::Impact(i) => patches::impacts::render(i, &spec, id.seed()),
        SoundId::Ambience(a) => patches::ambience::render(a, &spec, id.seed()),
        SoundId::Alert(a) => patches::signals::render_alert(a, &spec, id.seed()),
        SoundId::Radio(r) => patches::signals::render_radio(r, &spec, id.seed()),
        SoundId::Ui(u) => patches::signals::render_ui(u, &spec, id.seed()),
        SoundId::MenuTheme => patches::theme::render(&spec, id.seed()),
    };
    render::finish(raw, &spec, id.seed())
}

/// Render one sound straight to WAV bytes (what `cargo xtask gen` writes).
pub fn render_wav(id: SoundId) -> Vec<u8> { render::encode_wav(&render(id)) }
