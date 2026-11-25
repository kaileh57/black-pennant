//! Unit `gd-audio-feed`. `BpAudioFeed` (autoload `AudioFeed`): Rust tells GDScript which generated sound
//! files to play and with what pitch and volume. DESIGN.md L1242-1247 (every sound is a fundsp patch
//! rendered offline to 48 kHz 16-bit WAV under `res://audio/gen/`; nothing is synthesized live; Godot plays
//! files; Rust sets pitch and volume from simulation state for engines, sea and weather; no music in
//! sorties), L1676-1687 (one sound pattern per alert priority; Silence all for 30 s; Warning chime repeats
//! at 120 s; Alarm repeats until acknowledged; Emergency is continuous), L1706 (radio effect for officer
//! and ace lines, text shown no later than the sound), L1659 (warning red always comes with sound).
//!
//! Two kinds of output, both pulled once per frame by `res://audio/sound.gd`:
//! - one-shots (`drain_events`): gunfire, impacts, splashes, alerts, radio cues, interface sounds;
//! - loops (`loops`): engines, sea, wind, rain, fire, flooding, alarm beds, with pitch and volume updated at
//!   30 Hz and a stable id so the player keeps one voice per source.
//!
//! Positions are in render space; distant one-shots carry a speed-of-sound delay.
//!
//! # Sound ids come from the `audio` crate
//! A sound is an `audio::SoundId` ([`crate::seams::sound`]); its `spec()` row gives the file stem, the bus,
//! whether it loops and whether it is positional. GDScript receives the stem as a `StringName` and plays
//! `res://audio/gen/<stem>.wav`. This unit keeps no list of ids or stems. Which file an event uses, and the
//! pitch and gain of each loop, are the pure functions of `audio::mix`:
//! | Source | Rule |
//! | `PhysEvent` gun fire | `mix::gun_report(calibre_mm, distance_m)`: near band file or the distant file, delayed by `mix::arrival_delay_s`, gain `mix::range_gain_db` plus `mix::gun_report_trim_db` |
//! | splashes, bursts | `mix::splash_sound(calibre_mm)`, `mix::explosion_sound(he_b, underwater)` |
//! | own engines | `mix::engine_loop(class)` with `mix::engine_params(class, power)`; `class` from `content::part::EngineKind` |
//! | hull wash, cavitation | `AmbienceSfx::{HullWashSlow, HullWashFast, Cavitation}` with `mix::wash_params(speed_kn, cavitation_kn)` |
//! | sea beds | `AmbienceSfx::{SeaCalm, SeaModerate, SeaRough, SeaStorm}` with `mix::sea_params(sea_state, wind_mps)` |
//! | wind, rain | `AmbienceSfx::{Wind, WindGale, Rain, RainHeavy}` with `mix::weather_params(wind_mps, rain)` |
//! | fire, flooding, pumps | `ImpactSfx::{FireLoop, FloodingLoop, PumpLoop}` per burning or flooding compartment and running pump |
//! | alerts | `mix::alert_sound(priority)`: Caution once, Warning again every `mix::WARNING_REPEAT_S`, Alarm repeated until acknowledged, Emergency as a loop; `Orders.silence_alerts` mutes for `mix::SILENCE_ALL_S` |
//! | officer and ace lines | `RadioSfx::SquelchOpen` (or `AceOpen`), `Static` as a loop while the line shows, `SquelchClose` |
//! | interface | `SoundId::Ui(UiSfx)` from `ui_sound(Bp.UI_SOUND_*)` |
//! | menu | `SoundId::MenuTheme`, a loop on the `Music` bus, never in a sortie |
//!
//! Buses are `audio::Bus`; `Bus::name()` (`Sfx`, `Ambience`, `Alert`, `Radio`, `Ui`, `Music`) is the bus name
//! in `res://audio/bus_layout.tres`. Loop files carry their loop points in the WAV `smpl` chunk, which
//! Godot's default WAV import ("detect from WAV") reads; no import override is needed.
//!
//! # Levels and positions
//! `volume_db` of a positional one-shot or loop already carries the distance law (`mix::range_gain_db`
//! from the listener: `mix::distance_gain_db`, air absorption included, faded to silence at the family's range) and the arrival delay is in `delay_s`. The player uses `pos`
//! for panning only (`AudioStreamPlayer3D.attenuation_model = ATTENUATION_DISABLED`); `max_distance_m` is
//! where the gain reaches `mix::SILENT_DB`, for culling. The listener is the active camera; without one, the
//! own hull.
//!
//! Decisions where the contract is silent:
//! - Gun reports come from `ProjectileSpawned` (it names calibre, origin and point defence); one report per
//!   mount at most every 0.12 s, so a merged burst or a CIWS stream does not stack copies. Point-defence guns
//!   of 48 mm or less use the CIWS file inside the near-report range.
//! - Launches come from `BodySpawned` by munition kind, decoys from `DecoyDeployed` (smoke uses its own file),
//!   mines from `MineLaid`. Shell hits: a ricochet file, the autocannon hit for 48 mm or less, else the
//!   penetrating hit; a bursting shell adds the explosion file (large from 100 mm). Body hits and bursts use
//!   the munition's HE B; a burst more than 2 m above the sea is an air burst.
//! - Damage to the own hull sounds at the compartment: fire ignition, a breach below the waterline (rush of
//!   water), a cook-off; a split or loss sounds the hull breaking; debris falls; collisions and groundings
//!   sound at their point. A hurt own hull (3% of its cells lost, or a compartment a quarter full) groans at
//!   most every 8 s while it heels further beyond 15 degrees (a steady list stays quiet; an undamaged hull
//!   rolling in a seaway is silent).
//! - One-shots from events more than 2 s older than the render tick are dropped (a replay seek or a rejoin
//!   would otherwise dump a burst). Alerts and radio cues sound only for events at or after the newest tick
//!   already walked (a kill-cam clip replays old events: its explosions play, its alerts do not).
//! - Alert sounds are the own hull's only, never for an alert born log-only or shelved: Caution one tick at
//!   its first annunciation, Warning a chime at first annunciation and again every `mix::WARNING_REPEAT_S`
//!   of sortie time while unacknowledged, Alarm the `alert_alarm` pattern listed as a loop while any
//!   unacknowledged Alarm is live, Emergency the continuous loop while any unacknowledged Emergency is live.
//!   Only the most urgent pattern sounds: an Emergency silences the Alarm pulses while it lasts.
//!   Silence (the server's `AlertsSilenced` deadline, in sortie time, so a pause holds it) mutes all of them,
//!   and repeats and beds stop on the after-action pause and with the hull lost, when nothing can be
//!   acknowledged any more.
//! - Radio: a squelch-open (ace-open for an ace) when an officer line of the own hull or an ace line arrives,
//!   static while it shows (1.2 s plus 50 ms per character, at most 5 s; a following line extends it), then a
//!   squelch-close. The HUD shows the text in the same frame, before this node runs.
//! - Loops: own engines (one per engine class with a working engine, pitch and gain from shaft power), hull
//!   wash and cavitation (cavitation speed from the slowest-cavitating propulsor, 20 kn without one), the
//!   sea, wind and rain beds from the newest `WeatherChanged`, a fire loop per burning own compartment, a
//!   flooding loop per own compartment taking water through a breach or open valve, a pump loop per running
//!   pump (at most 4 of each, the most intense first), motor loops for the 8 nearest missiles, drones,
//!   helicopters and RHIBs in view, and the turret-train loop for own mounts slewing faster than 3 degrees a
//!   second. A shell from another hull passing within 40 m of the listener plays the pass-by once; an
//!   aircraft within 400 m its pass once. The own active sonar pings every 6 s of sortie time while it
//!   radiates. The train loop holds 0.8 s past the last fast slew, so a tracking mount hums steadily.
//! - Pacing: pausing, resuming and a change of the effective time rate play their interface sounds; a faster
//!   rate the player asked for that the server refuses plays the refusal. While paused, alert patterns and
//!   repeats hold (they resume, still unacknowledged, with the sortie) and the world loops sit 12 dB down.
//! - When more than 24 one-shots wait, alert, radio and interface sounds go first, then by priority, then the
//!   louder. Thunder belongs to the lightning the world view draws: `world::thunder_for` queues it there.
use crate::seams::sound::{Bus, SoundId};
use godot::prelude::*;

/// One queued one-shot before conversion to `AudioEventDict`. `bus` and `positional` are not stored: they are
/// `sound.spec().bus` and `sound.spec().positional && pos.is_some()`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AudioEvent { pub sound: SoundId, pub volume_db: f32, pub pitch: f32, pub pos: Option<[f32; 3]>, pub max_distance_m: f32, pub delay_s: f32, pub priority: u8 }

/// One continuous source before conversion to `AudioLoopDict`. `sound.spec().playback` must be
/// `Playback::Loop`, except `AlertSound::Alarm`, a one-shot pattern the player restarts while it is listed.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AudioLoop { pub id: u64, pub sound: SoundId, pub volume_db: f32, pub pitch: f32, pub pos: Option<[f32; 3]>, pub max_distance_m: f32 }
use crate::enums::{GdEnum, SessionKind, SessionState};
use crate::interp::{CameraState, ViewFrame};
use crate::seams::sound::{mix, AlertSound, AmbienceSfx, ImpactSfx, Playback, RadioSfx, UiSfx, WeaponSfx};
use crate::seams::WireEvent;
use crate::services::services;
use content::catalogue::Catalogue;
use content::ids::{CompartmentId, ContentHash, MunitionIdx, PartSlot};
use content::part::weapons::MunitionKind;
use content::part::{DecoyKind, PartFunction};
use glam::{DVec3, Quat, Vec3};
use sim_core::events::PhysEvent;
use sim_core::projectiles::ProjectileId;
use sim_core::vessel::VesselStatus;
use sim_core::EntityId;
use sim_systems::ids::AlertId;
use sim_systems::protocol::event::SimEvent;
use sim_systems::protocol::{AlertPriority, AlertState, AlertView, Snapshot};
use sim_systems::ship::ShipDesign;
use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

/// Ticks per second of sortie time.
const TICK_HZ: f64 = sim_core::TICK_HZ as f64;
/// Event one-shots this far behind the render tick are dropped.
const STALE_S: f64 = 2.0;
/// Loops rebuild at this rate.
const LOOP_HZ: f64 = 30.0;
/// Shortest gap between two gun reports of one mount, wall seconds.
const GUN_GAP_S: f64 = 0.12;
/// Calibre at or below which a gun is a light (autocannon) gun.
const LIGHT_GUN_MM: u16 = 48;
/// Distances (metres) past which each family is not played; the gain law is at its floor long before.
const FAR_GUN_M: f32 = 40_000.0;
const FAR_BLAST_M: f32 = 25_000.0;
const FAR_SMALL_M: f32 = 6_000.0;
const FAR_LOOP_M: f32 = 4_000.0;
/// Most loops of one kind.
const MAX_DAMAGE_LOOPS: usize = 4;
const MAX_MOTOR_LOOPS: usize = 8;
/// Shell pass-by and aircraft pass distances.
const PASSBY_M: f32 = 40.0;
const AIRCRAFT_PASS_M: f32 = 400.0;
const AIRCRAFT_RESET_M: f32 = 2_000.0;
/// Turret slew that sounds the train loop, radians per second.
const TRAIN_MIN_RPS: f32 = 0.052;
/// The train loop keeps sounding this long after the slew rate last passed the threshold, so a mount that
/// tracks in small steps hums steadily instead of starting and stopping several times a second.
const TRAIN_HOLD_S: f64 = 0.8;
/// Hull groan: heel and repeat.
const GROAN_HEEL_DEG: f32 = 15.0;
const GROAN_GAP_S: f64 = 8.0;
/// The hull groans only when hurt: this fraction of its cells lost, or a compartment this full (of 255).
const GROAN_CELLS_LOST: f32 = 0.03;
const GROAN_FILL: u8 = 64;
/// Active sonar ping interval, sortie seconds.
const SONAR_GAP_S: f64 = 6.0;
/// Radio static per line: base, per character, cap.
const STATIC_BASE_S: f64 = 1.2;
const STATIC_PER_CHAR_S: f64 = 0.05;
const STATIC_MAX_S: f64 = 5.0;
/// World loops drop by this much while the sortie is paused.
const PAUSE_DUCK_DB: f32 = -12.0;
/// Bounded memories.
const MAX_REMEMBERED: usize = 4_096;
/// A burst counts as an air burst above this height, metres.
const AIR_BURST_M: f64 = 2.0;

/// Priorities (higher survives the per-frame cap).
mod prio {
    pub const DEBRIS: u8 = 1;
    pub const DISTANT: u8 = 2;
    pub const SPLASH: u8 = 3;
    pub const WEAPON: u8 = 4;
    pub const BLAST: u8 = 5;
    pub const OWN: u8 = 6;
    pub const ALERT: u8 = 9;
    pub const RADIO: u8 = 8;
    pub const UI: u8 = 7;
}

/// Engine class of a catalogue engine.
pub(crate) fn engine_class(kind: content::part::EngineKind) -> mix::EngineClass {
    match kind { content::part::EngineKind::Diesel => mix::EngineClass::Diesel, content::part::EngineKind::GasTurbine => mix::EngineClass::GasTurbine, content::part::EngineKind::ElectricMotor => mix::EngineClass::Electric }
}

/// Launch file of a munition kind (`None`: silent or not launched).
pub(crate) fn launch_sound(kind: MunitionKind) -> Option<SoundId> {
    Some(SoundId::Weapon(match kind {
        MunitionKind::AntiShipMissile => WeaponSfx::MissileLaunch,
        MunitionKind::Sam | MunitionKind::PdMissile => WeaponSfx::SamLaunch,
        MunitionKind::Torpedo => WeaponSfx::TorpedoLaunch,
        MunitionKind::DepthCharge => WeaponSfx::DepthChargeRoll,
        MunitionKind::Mine => WeaponSfx::MineDrop,
        MunitionKind::Drone => WeaponSfx::DroneLaunch,
        MunitionKind::Aircraft | MunitionKind::Helicopter | MunitionKind::Rhib => return None,
    }))
}

/// Motor loop of a body in flight.
pub(crate) fn motor_sound(kind: MunitionKind) -> Option<SoundId> {
    Some(SoundId::Weapon(match kind {
        MunitionKind::AntiShipMissile | MunitionKind::Sam | MunitionKind::PdMissile => WeaponSfx::MissileMotor,
        MunitionKind::Drone => WeaponSfx::DroneMotor,
        MunitionKind::Helicopter => WeaponSfx::HelicopterRotor,
        MunitionKind::Rhib => WeaponSfx::RhibOutboard,
        MunitionKind::Torpedo | MunitionKind::Mine | MunitionKind::DepthCharge | MunitionKind::Aircraft => return None,
    }))
}

/// Report file of a gun shot.
pub(crate) fn gun_sound(calibre_mm: u16, point_defence: bool, distance_m: f32) -> SoundId {
    if point_defence && calibre_mm <= LIGHT_GUN_MM && distance_m < mix::NEAR_REPORT_M { return SoundId::Weapon(WeaponSfx::Ciws); }
    mix::gun_report(calibre_mm, distance_m)
}

/// Files of a shell hit: the hit, and the burst when it burst.
pub(crate) fn shell_hit_sounds(calibre_mm: u16, outcome: sim_core::armour::HitOutcome, burst: bool) -> (SoundId, Option<SoundId>) {
    use sim_core::armour::HitOutcome as H;
    let hit = if calibre_mm <= LIGHT_GUN_MM && outcome != H::Ricochet { ImpactSfx::AutocannonHit } else if outcome == H::Ricochet { ImpactSfx::Ricochet } else { ImpactSfx::PenetrateHit };
    let blast = burst.then(|| mix::explosion_sound(if calibre_mm >= 100 { 6 } else { 4 }, false));
    (SoundId::Impact(hit), blast)
}

/// Seconds of radio static for a line of `chars` characters.
pub(crate) fn static_s(chars: usize) -> f64 { (STATIC_BASE_S + STATIC_PER_CHAR_S * chars as f64).min(STATIC_MAX_S) }

/// One-shot order under the cap: alert, radio, interface first; then priority; then the louder.
fn cap_rank(e: &AudioEvent) -> (u8, u8, i32) {
    let class = match e.sound.spec().bus { Bus::Alert => 3, Bus::Radio => 2, Bus::Ui => 1, _ => 0 };
    (class, e.priority, (e.volume_db.clamp(mix::SILENT_DB, 24.0) * 100.0) as i32)
}

/// The at most [`MAX_EVENTS_PER_FRAME`] one-shots to start, best first (stable for ties).
pub(crate) fn pick_events(mut events: Vec<AudioEvent>) -> Vec<AudioEvent> {
    events.retain(|e| e.volume_db.is_finite() && e.pitch.is_finite() && e.pitch > 0.0 && e.delay_s.is_finite());
    events.sort_by_key(|a| std::cmp::Reverse(cap_rank(a)));
    events.truncate(MAX_EVENTS_PER_FRAME);
    events
}

/// The alert sounds of a set of the own hull's alerts at sortie tick `tick`: (warnings due a chime, any
/// unacknowledged alarm, any unacknowledged emergency). `last_chime` holds the tick of each Warning's last
/// chime and is updated for the chimes returned.
pub(crate) fn alert_due(alerts: &BTreeMap<AlertId, AlertView>, last_chime: &mut BTreeMap<AlertId, u64>, tick: u64) -> (Vec<AlertId>, bool, bool) {
    let repeat = (mix::WARNING_REPEAT_S as f64 * TICK_HZ) as u64;
    let mut chimes = Vec::new();
    let (mut alarm, mut emergency) = (false, false);
    for a in alerts.values() {
        if a.state != AlertState::Unacknowledged { continue; }
        match a.priority {
            AlertPriority::Warning => {
                if let Some(t) = last_chime.get(&a.id) {
                    if tick >= t.saturating_add(repeat) { chimes.push(a.id); last_chime.insert(a.id, tick); }
                }
            }
            AlertPriority::Alarm => alarm = true,
            AlertPriority::Emergency => emergency = true,
            AlertPriority::Caution => {}
        }
    }
    (chimes, alarm, emergency)
}

/// What one frame of the feed reads.
pub(crate) struct FrameIn<'a> {
    pub now_s: f64,
    pub state: SessionState,
    pub kind: SessionKind,
    pub events: &'a [WireEvent],
    pub snapshot: Option<&'a Snapshot>,
    pub view: &'a ViewFrame,
    pub camera: CameraState,
    pub catalogue: &'a Catalogue,
    /// The built design of a hash, when this client has it.
    pub design: &'a dyn Fn(&ContentHash) -> Option<Arc<ShipDesign>>,
    /// Characters of a radio line as the player reads it (translated).
    pub line_chars: &'a dyn Fn(&sim_systems::protocol::Msg) -> usize,
}

/// Everything the feed keeps between frames. Plain data; `Default` is inert.
#[derive(Default)]
pub(crate) struct Feed {
    last_state: SessionState,
    watermark: Option<u64>,
    mute_below: Option<u64>,
    /// The own hull, its design hash and design.
    own: Option<EntityId>,
    blueprints: BTreeMap<EntityId, ContentHash>,
    design: Option<Arc<ShipDesign>>,
    /// The own hull's alerts as the events told them.
    alerts: BTreeMap<AlertId, AlertView>,
    last_chime: BTreeMap<AlertId, u64>,
    silenced_until_tick: u64,
    weather: Option<content::weather::WeatherState>,
    /// Live projectiles: calibre and shooter, for hit and pass-by sounds.
    projectiles: BTreeMap<ProjectileId, (u16, EntityId)>,
    bodies: BTreeMap<EntityId, MunitionIdx>,
    last_gun: BTreeMap<(EntityId, PartSlot), f64>,
    passed_shells: BTreeSet<ProjectileId>,
    passed_aircraft: BTreeSet<EntityId>,
    turret_prev: BTreeMap<PartSlot, f32>,
    /// Slewing mounts: until when (wall seconds) the train loop keeps sounding, and its pitch.
    turret_hold: BTreeMap<PartSlot, (f64, f32)>,
    /// The last pacing heard: paused, effective rate, the own requested rates.
    pacing: Option<(bool, u32, Vec<u32>)>,
    /// Tick of the newest `SortieStarted`: a pacing on that tick is a catch-up's starting state.
    started_tick: Option<u64>,
    static_until_s: f64,
    static_on: bool,
    groan_next_s: f64,
    /// Own heel magnitude at the last schedule check, degrees.
    last_heel: f32,
    sonar_next_tick: u64,
    loop_accum_s: f64,
    pub(crate) menu_theme: bool,
    pub(crate) loops: Vec<AudioLoop>,
    pub(crate) loops_rev: u64,
}

/// States whose events and view the feed sounds (loading included: the opening weather and reveals arrive then).
fn in_sortie(s: SessionState) -> bool { matches!(s, SessionState::Loading | SessionState::InSortie | SessionState::Ended | SessionState::Replay) }
fn remember<K: Ord + Copy>(set: &mut BTreeSet<K>, k: K) { if set.len() >= MAX_REMEMBERED { set.clear(); } set.insert(k); }
fn rq(r: [f32; 4]) -> Quat { let q = Quat::from_xyzw(r[0], r[1], r[2], r[3]); if q.is_finite() && q.length_squared() > 1e-6 { q.normalize() } else { Quat::IDENTITY } }

/// Where sounds are heard from.
#[derive(Clone, Copy)]
struct Ears { world: DVec3, render: Vec3 }

impl Feed {
    /// Forget the sortie (sounds and loops of the last one stop).
    fn reset(&mut self) {
        let (menu, rev, last) = (self.menu_theme, self.loops_rev, self.last_state);
        // A new revision of the (now empty) loop set, so the player stops what was sounding.
        *self = Feed { menu_theme: menu, loops_rev: rev + 1, last_state: last, loop_accum_s: 1.0, ..Feed::default() };
    }

    fn ears(&self, f: &FrameIn<'_>) -> Ears {
        let origin = f.view.origin;
        if f.camera.valid && f.camera.pos.iter().all(|v| v.is_finite()) {
            let w = DVec3::from_array(f.camera.pos);
            return Ears { world: w, render: Vec3::from_array(origin.to_render(w)) };
        }
        let own = f.snapshot.and_then(|s| self.own.and_then(|o| s.vessels.iter().find(|v| v.entity == o))).map(|v| v.kin.pose.pos).filter(|p| p.is_finite());
        let w = own.unwrap_or(DVec3::new(origin.x, 0.0, origin.z));
        Ears { world: w, render: Vec3::from_array(origin.to_render(w)) }
    }

    /// One frame: events to one-shots (appended to `out`), and the loop set when it is due.
    pub(crate) fn frame(&mut self, f: &FrameIn<'_>, delta_s: f64, out: &mut Vec<AudioEvent>) {
        let state = f.state;
        let was = self.last_state;
        self.last_state = state;
        let new_sortie = state == SessionState::Loading && was != SessionState::Loading;
        if new_sortie || (!in_sortie(state) && in_sortie(was)) { self.reset(); }
        if let Some(s) = f.snapshot.and_then(|s| s.ship.as_ref()) {
            if self.own != Some(s.vessel) { self.own = Some(s.vessel); self.alerts.clear(); self.last_chime.clear(); }
        }
        let ears = self.ears(f);
        if in_sortie(state) {
            self.walk(f, ears, out);
            self.resolve_design(f);
            self.periodic(f, ears, out);
        }
        self.loop_accum_s += if delta_s.is_finite() { delta_s.max(0.0) } else { 0.0 };
        if self.loop_accum_s >= 1.0 / LOOP_HZ || self.loops_rev == 0 {
            let dt = self.loop_accum_s.min(0.5);
            self.loop_accum_s = 0.0;
            let next = if in_sortie(state) { self.build_loops(f, ears, dt) } else { self.menu_loops() };
            if next != self.loops || self.loops_rev == 0 { self.loops = next; self.loops_rev += 1; }
        }
    }

    fn menu_loops(&self) -> Vec<AudioLoop> {
        if !self.menu_theme { return Vec::new(); }
        vec![AudioLoop { id: loop_id(13, 0), sound: SoundId::MenuTheme, volume_db: 0.0, pitch: 1.0, pos: None, max_distance_m: 0.0 }]
    }

    fn resolve_design(&mut self, f: &FrameIn<'_>) {
        let Some(own) = self.own else { self.design = None; return };
        let Some(hash) = self.blueprints.get(&own).copied() else { return };
        if self.design.as_ref().is_some_and(|d| d.hash == hash) { return; }
        self.design = (f.design)(&hash);
    }

    fn is_own(&self, v: EntityId) -> bool { self.own == Some(v) }

    /// A positional one-shot at a world point.
    #[allow(clippy::too_many_arguments)]
    fn at(&self, f: &FrameIn<'_>, ears: Ears, sound: SoundId, world: DVec3, gain_db: f32, far_m: f32, priority: u8) -> Option<AudioEvent> {
        if !world.is_finite() { return None; }
        let d = ears.world.distance(world) as f32;
        if d.is_nan() || d > far_m { return None; }
        let volume_db = (mix::range_gain_db(d, far_m) + gain_db).max(mix::SILENT_DB);
        if volume_db <= mix::SILENT_DB + 0.5 { return None; }
        Some(AudioEvent { sound, volume_db, pitch: vary_pitch(sound, world), pos: Some(f.view.origin.to_render(world)), max_distance_m: far_m, delay_s: mix::arrival_delay_s(d), priority })
    }

    fn own_pose(&self, f: &FrameIn<'_>) -> Option<sim_core::math::Pose> {
        let own = self.own?;
        f.snapshot?.vessels.iter().find(|v| v.entity == own).map(|v| v.kin.pose).filter(|p| p.pos.is_finite())
    }

    /// World point of an own compartment's centroid.
    fn compartment_world(&self, f: &FrameIn<'_>, c: CompartmentId) -> Option<DVec3> {
        let pose = self.own_pose(f)?;
        let d = self.design.as_ref()?;
        let comp = d.prefab.compartments.compartments.iter().find(|x| x.id == c)?;
        Some(pose.to_world(comp.centroid))
    }

    fn munition_kind(&self, f: &FrameIn<'_>, idx: MunitionIdx) -> Option<(MunitionKind, u8)> {
        f.catalogue.munitions.get_index(idx.index()).map(|(_, m)| (m.kind, m.he_b))
    }

    fn walk(&mut self, f: &FrameIn<'_>, ears: Ears, out: &mut Vec<AudioEvent>) {
        let render_tick = if f.view.render_tick.is_finite() { f.view.render_tick } else { 0.0 };
        for e in f.events {
            // Repeats: a re-opening behind the newest tick mutes announcements until the stream catches up;
            // a replay start, or a restart in a basin, starts again.
            if matches!(e.event, SimEvent::SortieStarted { .. }) {
                let behind = self.watermark.is_some_and(|w| e.tick < w);
                if f.state == SessionState::Replay || (behind && matches!(f.kind, SessionKind::Splash | SessionKind::Tutorial)) {
                    let keep_own = self.own;
                    self.reset();
                    self.own = keep_own;
                } else if behind {
                    self.mute_below = self.watermark;
                }
                self.started_tick = Some(e.tick);
            }
            let fresh = match self.mute_below { Some(w) if e.tick < w => false, Some(_) => { self.mute_below = None; true } None => true };
            if fresh { self.watermark = Some(self.watermark.map_or(e.tick, |w| w.max(e.tick))); }
            let stale = (e.tick as f64) + STALE_S * TICK_HZ < render_tick;
            self.event(f, ears, e.tick, &e.event, fresh, stale, out);
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn event(&mut self, f: &FrameIn<'_>, ears: Ears, tick: u64, event: &SimEvent, fresh: bool, stale: bool, out: &mut Vec<AudioEvent>) {
        let silenced = |s: &Self| tick < s.silenced_until_tick;
        match event {
            SimEvent::EntityRevealed(i) => {
                if self.blueprints.len() >= MAX_REMEMBERED && !self.blueprints.contains_key(&i.entity) { self.blueprints.clear(); }
                self.blueprints.insert(i.entity, i.blueprint);
            }
            SimEvent::Phys(p) => self.phys(f, ears, p, stale, out),
            SimEvent::AlertRaised(a) | SimEvent::AlertEscalated(a) => {
                if !self.own.is_some_and(|o| o == a.vessel) { return; }
                self.alerts.insert(a.id, *a);
                if a.priority == AlertPriority::Warning { self.last_chime.insert(a.id, tick); } else { self.last_chime.remove(&a.id); }
                let sounded = matches!(a.state, AlertState::Unacknowledged | AlertState::Acknowledged);
                if fresh && !stale && sounded && !silenced(self) {
                    // Alarms and Emergencies sound as listed loops; the lower two as one-shots.
                    let one = match a.priority { AlertPriority::Caution => Some(AlertSound::Caution), AlertPriority::Warning => Some(AlertSound::Warning), _ => None };
                    if let Some(s) = one { out.push(AudioEvent { sound: SoundId::Alert(s), volume_db: 0.0, pitch: 1.0, pos: None, max_distance_m: 0.0, delay_s: 0.0, priority: prio::ALERT }); }
                }
            }
            SimEvent::PacingChanged(p) => {
                let own: Vec<u32> = p.requested.iter().map(|(_, r)| r.factor()).collect();
                let now = (p.paused, p.effective.factor(), own);
                // The server says nothing on the first tick of a sortie at plain 1x, so with nothing heard yet
                // the sortie was running unpaused at 1x: the first pause of a sortie chimes. A catch-up (a join or
                // a replay seek) sends its pacing on the tick of its `SortieStarted`: that is the starting state.
                let catch_up = self.started_tick == Some(tick);
                let plain = (false, 1, own_plain(&now.2));
                let was = self.pacing.replace(now.clone()).or(if catch_up { None } else { Some(plain) });
                if let (Some(was), true, false) = (was, fresh, stale) {
                    let ui = if now.0 && !was.0 {
                        Some(UiSfx::Pause)
                    } else if !now.0 && was.0 {
                        Some(UiSfx::Resume)
                    } else if now.1 != was.1 {
                        Some(UiSfx::TimeRate)
                    } else if now.2 != was.2 && p.blocked.is_some() {
                        // A faster rate asked for and refused (a threat inside the envelope).
                        Some(UiSfx::Invalid)
                    } else {
                        None
                    };
                    if let Some(u) = ui { out.push(AudioEvent { sound: SoundId::Ui(u), volume_db: 0.0, pitch: 1.0, pos: None, max_distance_m: 0.0, delay_s: 0.0, priority: prio::UI }); }
                }
            }
            SimEvent::AlertChanged { id, state } => { if let Some(a) = self.alerts.get_mut(id) { a.state = *state; } }
            SimEvent::AlertsSilenced { vessel, until_tick } => { if self.own.is_none_or(|o| o == *vessel) { self.silenced_until_tick = *until_tick; } }
            SimEvent::OfficerMessage { vessel, text, .. } => { if fresh && !stale && self.is_own(*vessel) { self.radio(f, text, false, out); } }
            SimEvent::AceMessage { text, .. } => { if fresh && !stale { self.radio(f, text, true, out); } }
            SimEvent::ObjectiveChanged { state: sim_systems::protocol::ObjectiveState::Done, .. } if fresh && !stale => {
                out.push(AudioEvent { sound: SoundId::Ui(UiSfx::ObjectiveDone), volume_db: 0.0, pitch: 1.0, pos: None, max_distance_m: 0.0, delay_s: 0.0, priority: prio::UI });
            }
            _ => {}
        }
    }

    fn radio(&mut self, f: &FrameIn<'_>, text: &sim_systems::protocol::Msg, ace: bool, out: &mut Vec<AudioEvent>) {
        let open = if ace { RadioSfx::AceOpen } else { RadioSfx::SquelchOpen };
        out.push(AudioEvent { sound: SoundId::Radio(open), volume_db: 0.0, pitch: 1.0, pos: None, max_distance_m: 0.0, delay_s: 0.0, priority: prio::RADIO });
        let chars = (f.line_chars)(text);
        let start = f.now_s.max(self.static_until_s.min(f.now_s + STATIC_MAX_S));
        self.static_until_s = (start + static_s(chars)).max(self.static_until_s);
        self.static_on = true;
    }

    fn phys(&mut self, f: &FrameIn<'_>, ears: Ears, p: &PhysEvent, stale: bool, out: &mut Vec<AudioEvent>) {
        // Bookkeeping happens for stale events too; sounds do not.
        match p {
            PhysEvent::ProjectileSpawned(s) => {
                if self.projectiles.len() >= MAX_REMEMBERED { self.projectiles.clear(); }
                self.projectiles.insert(s.id, (s.calibre_mm, s.shooter));
                if stale { return; }
                let key = (s.shooter, s.mount);
                if self.last_gun.get(&key).is_some_and(|t| f.now_s - t < GUN_GAP_S && f.now_s >= *t) { return; }
                if self.last_gun.len() >= MAX_REMEMBERED { self.last_gun.clear(); }
                self.last_gun.insert(key, f.now_s);
                let d = ears.world.distance(s.origin) as f32;
                let own = self.is_own(s.shooter);
                // Point-defence guns use the CIWS file near (as loud as an autocannon) and the light distant file far.
                let trim = mix::gun_report_trim_db(if s.point_defence { s.calibre_mm.min(LIGHT_GUN_MM) } else { s.calibre_mm }, d);
                if let Some(e) = self.at(f, ears, gun_sound(s.calibre_mm, s.point_defence, d), s.origin, trim, FAR_GUN_M, if own { prio::OWN } else { prio::WEAPON }) { out.push(e); }
            }
            PhysEvent::BodySpawned { body, kind, launcher, pos, .. } => {
                if self.bodies.len() >= MAX_REMEMBERED { self.bodies.clear(); }
                self.bodies.insert(*body, *kind);
                if stale { return; }
                let Some((k, _)) = self.munition_kind(f, *kind) else { return };
                let Some(s) = launch_sound(k) else { return };
                let pr = if self.is_own(*launcher) { prio::OWN } else { prio::WEAPON };
                if let Some(e) = self.at(f, ears, s, *pos, 0.0, FAR_BLAST_M, pr) { out.push(e); }
            }
            PhysEvent::DecoyDeployed { kind, owner, pos, .. } if !stale => {
                let s = if *kind == DecoyKind::Smoke { WeaponSfx::SmokeLaunch } else { WeaponSfx::DecoyLaunch };
                let pr = if self.is_own(*owner) { prio::OWN } else { prio::WEAPON };
                if let Some(e) = self.at(f, ears, SoundId::Weapon(s), *pos, 0.0, FAR_SMALL_M, pr) { out.push(e); }
            }
            PhysEvent::MineLaid { layer, pos, .. } if !stale => {
                let pr = if self.is_own(*layer) { prio::OWN } else { prio::WEAPON };
                if let Some(e) = self.at(f, ears, SoundId::Weapon(WeaponSfx::MineDrop), *pos, 0.0, FAR_SMALL_M, pr) { out.push(e); }
            }
            PhysEvent::Splash { projectile, pos, calibre_mm, .. } => {
                self.projectiles.remove(projectile);
                if stale { return; }
                if let Some(e) = self.at(f, ears, mix::splash_sound(*calibre_mm), *pos, 0.0, FAR_SMALL_M * 2.0, prio::SPLASH) { out.push(e); }
            }
            PhysEvent::ShellImpact { projectile, vessel, outcome, world_point, burst, .. } => {
                let calibre = self.projectiles.remove(projectile).map_or(76, |(c, _)| c);
                if stale { return; }
                let pr = if self.is_own(*vessel) { prio::OWN } else { prio::BLAST };
                let (hit, blast) = shell_hit_sounds(calibre, *outcome, *burst);
                if let Some(e) = self.at(f, ears, hit, *world_point, 0.0, FAR_SMALL_M, pr) { out.push(e); }
                if let Some(b) = blast { if let Some(e) = self.at(f, ears, b, *world_point, 0.0, FAR_BLAST_M, pr) { out.push(e); } }
            }
            PhysEvent::BodyImpact { body, vessel, world_point, .. } => {
                let kind = self.bodies.remove(body);
                if stale { return; }
                let (k, he_b) = kind.and_then(|k| self.munition_kind(f, k)).unwrap_or((MunitionKind::AntiShipMissile, 6));
                let underwater = matches!(k, MunitionKind::Torpedo | MunitionKind::Mine | MunitionKind::DepthCharge);
                let pr = if self.is_own(*vessel) { prio::OWN } else { prio::BLAST };
                if let Some(e) = self.at(f, ears, mix::explosion_sound(he_b, underwater), *world_point, 0.0, FAR_BLAST_M, pr) { out.push(e); }
            }
            PhysEvent::BodyBurst { body, pos, underwater } => {
                let kind = self.bodies.remove(body);
                if stale { return; }
                let he_b = kind.and_then(|k| self.munition_kind(f, k)).map_or(4, |(_, b)| b);
                let s = if !*underwater && pos.y > AIR_BURST_M { SoundId::Impact(ImpactSfx::AirBurst) } else { mix::explosion_sound(he_b, *underwater) };
                if let Some(e) = self.at(f, ears, s, *pos, 0.0, FAR_BLAST_M, prio::BLAST) { out.push(e); }
            }
            PhysEvent::BodyDestroyed { body, pos, shot_down, .. } => {
                self.bodies.remove(body);
                if stale || !*shot_down { return; }
                if let Some(e) = self.at(f, ears, SoundId::Impact(ImpactSfx::AirBurst), *pos, 0.0, FAR_BLAST_M, prio::BLAST) { out.push(e); }
            }
            PhysEvent::BodyRecovered { body, .. } => { self.bodies.remove(body); }
            PhysEvent::CookOff { pos, vessel, .. } if !stale => {
                let pr = if self.is_own(*vessel) { prio::OWN } else { prio::BLAST };
                if let Some(e) = self.at(f, ears, SoundId::Impact(ImpactSfx::CookOff), *pos, 0.0, FAR_BLAST_M, pr) { out.push(e); }
            }
            PhysEvent::FireStarted { vessel, compartment, .. } if !stale && self.is_own(*vessel) => {
                if let Some(w) = self.compartment_world(f, *compartment) { if let Some(e) = self.at(f, ears, SoundId::Impact(ImpactSfx::FireIgnite), w, 0.0, FAR_SMALL_M, prio::OWN) { out.push(e); } }
            }
            PhysEvent::FireOut { vessel, compartment, flooded: true } if !stale && self.is_own(*vessel) => {
                if let Some(w) = self.compartment_world(f, *compartment) { if let Some(e) = self.at(f, ears, SoundId::Impact(ImpactSfx::SteamHiss), w, 0.0, FAR_SMALL_M, prio::OWN) { out.push(e); } }
            }
            PhysEvent::Breach { vessel, compartment, below_waterline: true } if !stale && self.is_own(*vessel) => {
                if let Some(w) = self.compartment_world(f, *compartment) { if let Some(e) = self.at(f, ears, SoundId::Impact(ImpactSfx::BreachRush), w, 0.0, FAR_SMALL_M, prio::OWN) { out.push(e); } }
            }
            PhysEvent::Split { vessel, .. } if !stale => {
                let pos = f.snapshot.and_then(|s| s.vessels.iter().find(|v| v.entity == *vessel)).map(|v| v.kin.pose.pos);
                let pr = if self.is_own(*vessel) { prio::OWN } else { prio::BLAST };
                if let Some(w) = pos { if let Some(e) = self.at(f, ears, SoundId::Impact(ImpactSfx::HullBreak), w, 0.0, FAR_BLAST_M, pr) { out.push(e); } }
            }
            PhysEvent::Debris { pos, .. } if !stale => {
                if let Some(e) = self.at(f, ears, SoundId::Impact(ImpactSfx::DebrisFall), *pos, 0.0, FAR_SMALL_M, prio::DEBRIS) { out.push(e); }
            }
            PhysEvent::Collision { a, b, point, impulse } if !stale => {
                let pr = if self.is_own(*a) || self.is_own(*b) { prio::OWN } else { prio::DISTANT };
                // Gentle bumps play quieter: about -12 dB at 100 kN s, full from 1 MN s.
                let gain = if impulse.is_finite() && *impulse > 0.0 { amp_db((impulse / 1.0e6).clamp(0.25, 1.0) as f32) } else { -12.0 };
                if let Some(e) = self.at(f, ears, SoundId::Impact(ImpactSfx::Collision), *point, gain, FAR_SMALL_M, pr) { out.push(e); }
            }
            PhysEvent::Grounded { vessel, point } if !stale => {
                let pr = if self.is_own(*vessel) { prio::OWN } else { prio::DISTANT };
                if let Some(e) = self.at(f, ears, SoundId::Impact(ImpactSfx::Grounding), *point, 0.0, FAR_SMALL_M, pr) { out.push(e); }
            }
            PhysEvent::VesselLost { vessel, .. } if !stale && self.is_own(*vessel) => {
                if let Some(pose) = self.own_pose(f) { if let Some(e) = self.at(f, ears, SoundId::Impact(ImpactSfx::HullBreak), pose.pos, 0.0, FAR_BLAST_M, prio::OWN) { out.push(e); } }
            }
            PhysEvent::WeatherChanged { weather, .. } => { self.weather = Some(*weather); }
            _ => {}
        }
    }

    /// Sounds on a schedule: alert repeats, radio close, hull groan, sonar pings, pass-bys.
    fn periodic(&mut self, f: &FrameIn<'_>, ears: Ears, out: &mut Vec<AudioEvent>) {
        let tick = f.snapshot.map_or(0, |s| s.tick);
        // Alerts the book never heard of (joined mid-sortie, or a kill cam hid them).
        if let (Some(s), Some(own)) = (f.snapshot, self.own) {
            for a in s.alerts.iter().filter(|a| a.vessel == own) {
                if let std::collections::btree_map::Entry::Vacant(slot) = self.alerts.entry(a.id) {
                    slot.insert(*a);
                    if a.priority == AlertPriority::Warning { self.last_chime.insert(a.id, a.raised_tick); }
                }
            }
        }
        let silenced = tick < self.silenced_until_tick;
        let (chimes, _, _) = alert_due(&self.alerts, &mut self.last_chime, tick);
        if !silenced && self.alerts_audible(f) {
            for _ in chimes.iter().take(1) { out.push(AudioEvent { sound: SoundId::Alert(AlertSound::Warning), volume_db: 0.0, pitch: 1.0, pos: None, max_distance_m: 0.0, delay_s: 0.0, priority: prio::ALERT }); }
        }
        if self.static_on && f.now_s >= self.static_until_s {
            self.static_on = false;
            out.push(AudioEvent { sound: SoundId::Radio(RadioSfx::SquelchClose), volume_db: 0.0, pitch: 1.0, pos: None, max_distance_m: 0.0, delay_s: 0.0, priority: prio::RADIO });
        }
        let Some(ship) = f.snapshot.and_then(|s| s.ship.as_ref()) else { return };
        let Some(pose) = self.own_pose(f) else { return };
        let afloat = f.snapshot.and_then(|s| s.vessels.iter().find(|v| Some(v.entity) == self.own)).is_some_and(|v| matches!(v.status, VesselStatus::Active | VesselStatus::Disabled));
        let heel = if ship.heel_deg.is_finite() { ship.heel_deg.abs() } else { 0.0 };
        let heeling = heel > self.last_heel + 0.05;
        self.last_heel = heel;
        // Only a hurt hull groans: rolling in a seaway is not structural stress (an undamaged 22 m hull rolls
        // past 15 degrees every few seconds in sea state 4).
        let stressed = ship.cells_lost >= GROAN_CELLS_LOST || ship.compartments.iter().any(|c| c.fill >= GROAN_FILL);
        if afloat && stressed && heeling && heel > GROAN_HEEL_DEG && f.now_s >= self.groan_next_s {
            self.groan_next_s = f.now_s + GROAN_GAP_S;
            let gain = -6.0 + 6.0 * ((ship.heel_deg.abs() - GROAN_HEEL_DEG) / 30.0).clamp(0.0, 1.0);
            if let Some(e) = self.at(f, ears, SoundId::Impact(ImpactSfx::HullGroan), pose.pos, gain, FAR_SMALL_M, prio::OWN) { out.push(e); }
        }
        if let Some(d) = self.design.clone() {
            let sonar = d.emitters.iter().find(|e| e.kind == sim_systems::signature::EmitterKind::ActiveSonar && ship.board.radiating.contains(&e.slot));
            if let Some(em) = sonar {
                if tick >= self.sonar_next_tick {
                    self.sonar_next_tick = tick + (SONAR_GAP_S * TICK_HZ) as u64;
                    let local = d.prefab.parts.get(em.slot.index()).map_or(DVec3::ZERO, |p| p.centre);
                    if let Some(e) = self.at(f, ears, SoundId::Ambience(AmbienceSfx::SonarPing), pose.to_world(local), 0.0, FAR_SMALL_M, prio::OWN) { out.push(e); }
                }
            }
        }
        // Shells from other hulls passing close by.
        for s in &f.view.shells {
            if self.passed_shells.contains(&s.id) { continue; }
            if self.projectiles.get(&s.id).is_some_and(|(_, shooter)| self.is_own(*shooter)) { continue; }
            let p = Vec3::from_array(s.pos);
            if p.is_finite() && p.distance(ears.render) < PASSBY_M {
                remember(&mut self.passed_shells, s.id);
                out.push(AudioEvent { sound: SoundId::Weapon(WeaponSfx::ShellPassby), volume_db: 0.0, pitch: 1.0, pos: Some(s.pos), max_distance_m: PASSBY_M * 4.0, delay_s: 0.0, priority: prio::BLAST });
            }
        }
        for b in &f.view.bodies {
            if !self.munition_kind(f, b.kind).is_some_and(|(k, _)| k == MunitionKind::Aircraft) { continue; }
            let d = Vec3::from_array(b.pos).distance(ears.render);
            if d > AIRCRAFT_RESET_M { self.passed_aircraft.remove(&b.entity); continue; }
            if d < AIRCRAFT_PASS_M && !self.passed_aircraft.contains(&b.entity) {
                remember(&mut self.passed_aircraft, b.entity);
                out.push(AudioEvent { sound: SoundId::Weapon(WeaponSfx::AircraftPass), volume_db: mix::range_gain_db(d, AIRCRAFT_RESET_M), pitch: 1.0, pos: Some(b.pos), max_distance_m: AIRCRAFT_RESET_M, delay_s: 0.0, priority: prio::WEAPON });
            }
        }
    }

    /// The loop set of a sortie frame.
    fn build_loops(&mut self, f: &FrameIn<'_>, ears: Ears, dt: f64) -> Vec<AudioLoop> {
        let mut v = Vec::new();
        let tick = f.snapshot.map_or(0, |s| s.tick);
        // Alarm beds, only while the player can still acknowledge.
        if tick >= self.silenced_until_tick && self.alerts_audible(f) {
            let (_, alarm, emergency) = alert_due(&self.alerts, &mut BTreeMap::new(), tick);
            // Only the most urgent pattern sounds: the Emergency warble replaces the Alarm pulses, which return
            // if the Emergency is acknowledged or rectified first.
            if emergency { v.push(AudioLoop { id: loop_id(12, 0), sound: SoundId::Alert(AlertSound::Emergency), volume_db: 0.0, pitch: 1.0, pos: None, max_distance_m: 0.0 }); }
            else if alarm { v.push(AudioLoop { id: loop_id(11, 0), sound: SoundId::Alert(AlertSound::Alarm), volume_db: 0.0, pitch: 1.0, pos: None, max_distance_m: 0.0 }); }
        }
        if self.static_on { v.push(AudioLoop { id: loop_id(10, 0), sound: SoundId::Radio(RadioSfx::Static), volume_db: 0.0, pitch: 1.0, pos: None, max_distance_m: 0.0 }); }
        // Sea and weather beds.
        if let Some(w) = &self.weather {
            let wind = w.wind_speed_mps as f32;
            let sea = mix::sea_params(w.sea_state.0, wind);
            for (i, (s, p)) in [AmbienceSfx::SeaCalm, AmbienceSfx::SeaModerate, AmbienceSfx::SeaRough, AmbienceSfx::SeaStorm].into_iter().zip(sea).enumerate() {
                push_bed(&mut v, loop_id(3, i as u64), s, p);
            }
            let wx = mix::weather_params(wind, w.rain as f32);
            for (i, (s, p)) in [AmbienceSfx::Wind, AmbienceSfx::WindGale, AmbienceSfx::Rain, AmbienceSfx::RainHeavy].into_iter().zip(wx).enumerate() {
                push_bed(&mut v, loop_id(4, i as u64), s, p);
            }
        }
        let own_pose = self.own_view_pose(f);
        let ship = f.snapshot.and_then(|s| s.ship.as_ref());
        if let (Some((pos, rot, power, afloat)), Some(ship), Some(d)) = (own_pose, ship, self.design.clone()) {
            let to_render = |local: DVec3| -> [f32; 3] { (pos + rot * local.as_vec3()).to_array() };
            let level = |s: PartSlot| ship.parts.iter().find(|p| p.slot == s).map(|p| p.level);
            if afloat {
                // Engines: one loop per class with a working engine, at the first such engine.
                let mut classes: BTreeMap<u8, (mix::EngineClass, DVec3)> = BTreeMap::new();
                let mut cav_kn = f64::INFINITY;
                for r in &d.prefab.parts {
                    let Some((_, def)) = f.catalogue.parts.get_index(r.def.index()) else { continue };
                    match &def.function {
                        PartFunction::Engine(e) if level(r.slot) != Some(content::enums::DamageLevel::Destroyed) => {
                            let c = engine_class(e.kind);
                            classes.entry(c as u8).or_insert((c, r.centre));
                        }
                        PartFunction::Propulsor(p) if p.cavitation_kn.is_finite() && p.cavitation_kn > 0.0 => cav_kn = cav_kn.min(p.cavitation_kn),
                        _ => {}
                    }
                }
                let power = power.abs().clamp(0.0, 1.0);
                for (k, (class, local)) in classes {
                    let lp = mix::engine_params(class, power);
                    let at = to_render(local);
                    let d_m = Vec3::from_array(at).distance(ears.render);
                    push_loop(&mut v, loop_id(1, k as u64), mix::engine_loop(class), lp.gain_db + mix::range_gain_db(d_m, FAR_LOOP_M), lp.pitch, Some(at), FAR_LOOP_M);
                }
                let wash = mix::wash_params(ship.speed_kn, if cav_kn.is_finite() { cav_kn as f32 } else { 20.0 });
                for (i, (s, p)) in [AmbienceSfx::HullWashSlow, AmbienceSfx::HullWashFast, AmbienceSfx::Cavitation].into_iter().zip(wash).enumerate() {
                    let at = to_render(DVec3::ZERO);
                    push_loop(&mut v, loop_id(2, i as u64), SoundId::Ambience(s), p.gain_db, p.pitch, Some(at), FAR_LOOP_M);
                }
            }
            // Fire, flooding and pumps of the own hull, the most intense first.
            let comp_local = |c: CompartmentId| d.prefab.compartments.compartments.iter().find(|x| x.id == c).map(|x| x.centroid);
            let mut fires: Vec<_> = ship.compartments.iter().filter(|c| c.fire > 0).collect();
            fires.sort_by_key(|c| (std::cmp::Reverse(c.fire), c.id));
            for c in fires.into_iter().take(MAX_DAMAGE_LOOPS) {
                let Some(local) = comp_local(c.id) else { continue };
                let at = to_render(local);
                let amp = 0.4 + 0.6 * (c.fire as f32 / 255.0);
                push_loop(&mut v, loop_id(5, c.id.0 as u64), SoundId::Impact(ImpactSfx::FireLoop), amp_db(amp) + mix::range_gain_db(Vec3::from_array(at).distance(ears.render), FAR_LOOP_M), 1.0, Some(at), FAR_LOOP_M);
            }
            let mut floods: Vec<_> = ship.compartments.iter().filter(|c| c.fill > 0 && c.fill < 250 && (c.breached || c.valve_open)).collect();
            floods.sort_by_key(|c| (std::cmp::Reverse(c.breached), c.fill, c.id));
            for c in floods.into_iter().take(MAX_DAMAGE_LOOPS) {
                let Some(local) = comp_local(c.id) else { continue };
                let at = to_render(local);
                let amp = 1.0 - 0.5 * (c.fill as f32 / 255.0);
                push_loop(&mut v, loop_id(6, c.id.0 as u64), SoundId::Impact(ImpactSfx::FloodingLoop), amp_db(amp) + mix::range_gain_db(Vec3::from_array(at).distance(ears.render), FAR_LOOP_M), 1.0, Some(at), FAR_LOOP_M);
            }
            let mut pumps: Vec<PartSlot> = d.pumps.iter().copied().filter(|s| ship.parts.iter().any(|p| p.slot == *s && matches!(p.readout, sim_systems::protocol::PartReadout::Running(true)))).collect();
            pumps.sort_unstable();
            for s in pumps.into_iter().take(MAX_DAMAGE_LOOPS) {
                let local = d.prefab.parts.get(s.index()).map_or(DVec3::ZERO, |p| p.centre);
                let at = to_render(local);
                push_loop(&mut v, loop_id(7, s.0 as u64), SoundId::Impact(ImpactSfx::PumpLoop), mix::range_gain_db(Vec3::from_array(at).distance(ears.render), FAR_LOOP_M), 1.0, Some(at), FAR_LOOP_M);
            }
            // Turrets slewing.
            let own_index = f.view.own.map(|i| i as usize);
            if let Some(vp) = own_index.and_then(|i| f.view.vessels.get(i)) {
                let range = vp.turrets.start as usize..(vp.turrets.end as usize).min(f.view.turrets.len());
                let mut seen = BTreeSet::new();
                for t in f.view.turrets.get(range).unwrap_or(&[]) {
                    seen.insert(t.slot);
                    let prev = self.turret_prev.insert(t.slot, t.train_rad);
                    if let (Some(prev), true) = (prev, dt > 1e-3) {
                        let mut delta = t.train_rad - prev;
                        while delta > std::f32::consts::PI { delta -= std::f32::consts::TAU; }
                        while delta < -std::f32::consts::PI { delta += std::f32::consts::TAU; }
                        let rate = (delta.abs() as f64 / dt) as f32;
                        if !rate.is_nan() && rate > TRAIN_MIN_RPS {
                            self.turret_hold.insert(t.slot, (f.now_s + TRAIN_HOLD_S, 0.85 + 0.3 * (rate / 0.6).min(1.0)));
                        }
                    }
                    let Some(&(until, pitch)) = self.turret_hold.get(&t.slot) else { continue };
                    if f.now_s >= until { self.turret_hold.remove(&t.slot); continue; }
                    let local = d.mounts.iter().find(|m| m.slot == t.slot).map_or(DVec3::ZERO, |m| m.pos_local);
                    let at = to_render(local);
                    push_loop(&mut v, loop_id(9, t.slot.0 as u64), SoundId::Weapon(WeaponSfx::TurretTrain), mix::range_gain_db(Vec3::from_array(at).distance(ears.render), FAR_LOOP_M), pitch, Some(at), FAR_LOOP_M);
                }
                self.turret_prev.retain(|s, _| seen.contains(s));
                self.turret_hold.retain(|s, _| seen.contains(s));
            }
        }
        // Motors of bodies in view, nearest first.
        let mut motors: Vec<(f32, u64, SoundId, [f32; 3])> = f.view.bodies.iter().filter_map(|b| {
            let (k, _) = self.munition_kind(f, b.kind)?;
            let s = motor_sound(k)?;
            let p = Vec3::from_array(b.pos);
            p.is_finite().then(|| (p.distance(ears.render), b.entity.to_u64(), s, b.pos))
        }).filter(|m| m.0 <= FAR_SMALL_M).collect();
        motors.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
        for (d_m, id, s, at) in motors.into_iter().take(MAX_MOTOR_LOOPS) {
            push_loop(&mut v, loop_id(8, id & 0xFFFF_FFFF_FFFF), s, mix::range_gain_db(d_m, FAR_SMALL_M), 1.0, Some(at), FAR_SMALL_M);
        }
        // Paused: the world holds still and sits back under the pause menu (the radio static of a line still
        // showing keeps its level).
        if self.paused() {
            for l in v.iter_mut().filter(|l| l.sound.spec().bus != Bus::Radio) { l.volume_db = (l.volume_db + PAUSE_DUCK_DB).max(mix::SILENT_DB); }
        }
        v
    }

    /// Alert sounds repeat only while the player can acknowledge: a running sortie or a replay, with the own
    /// hull afloat. The after-action pause and a lost hull fall silent.
    fn alerts_audible(&self, f: &FrameIn<'_>) -> bool {
        if !matches!(f.state, SessionState::InSortie | SessionState::Replay) || self.paused() { return false; }
        let Some(own) = self.own else { return false };
        f.snapshot.is_some_and(|s| s.ship.is_some() && s.vessels.iter().any(|v| v.entity == own && matches!(v.status, VesselStatus::Active | VesselStatus::Disabled)))
    }

    /// The sortie is paused (the newest pacing says so).
    fn paused(&self) -> bool { self.pacing.as_ref().is_some_and(|p| p.0) }

    /// The own hull in render space: position, rotation, shaft power, afloat.
    fn own_view_pose(&self, f: &FrameIn<'_>) -> Option<(Vec3, Quat, f32, bool)> {
        let vp = f.view.own.and_then(|i| f.view.vessels.get(i as usize))?;
        let pos = Vec3::from_array(vp.pos);
        if !pos.is_finite() { return None; }
        let power = if vp.power.is_finite() { vp.power } else { 0.0 };
        Some((pos, rq(vp.rot), power, matches!(vp.status, VesselStatus::Active | VesselStatus::Disabled)))
    }
}

/// Linear amplitude to dB, floored at the silent level. Presentation only (not the simulation), so std
/// floats are fine here.
#[allow(clippy::disallowed_methods)]
fn amp_db(a: f32) -> f32 { if a > 1e-4 && a.is_finite() { (20.0 * a.log10()).max(mix::SILENT_DB) } else { mix::SILENT_DB } }

/// Pitch of a positional one-shot: within 3% either side of 1, from a hash of the sound and the event's
/// position, so a burst of reports or splashes never repeats one sample exactly (the machine-gun effect),
/// while a given event always plays at the same pitch (replays sound alike).
fn vary_pitch(sound: SoundId, world: DVec3) -> f32 {
    let mut h = crate::seams::sound::stem(sound).bytes().fold(0xcbf2_9ce4_8422_2325u64, |h, b| (h ^ b as u64).wrapping_mul(0x0000_0100_0000_01b3));
    for v in [world.x, world.y, world.z] { h = (h ^ v.to_bits()).wrapping_mul(0x9E37_79B9_7F4A_7C15); h ^= h >> 29; }
    let u = (h >> 40) as f32 / (1u64 << 24) as f32;
    1.0 + PITCH_SPREAD * (2.0 * u - 1.0)
}
const PITCH_SPREAD: f32 = 0.03;

/// A stable loop id: family in the top 16 bits.
fn loop_id(family: u64, sub: u64) -> u64 { (family << 48) | (sub & 0xFFFF_FFFF_FFFF) }

fn push_loop(v: &mut Vec<AudioLoop>, id: u64, sound: SoundId, volume_db: f32, pitch: f32, pos: Option<[f32; 3]>, max_distance_m: f32) {
    if !volume_db.is_finite() || volume_db <= mix::SILENT_DB + 0.5 || !(pitch.is_finite() && pitch > 0.0) { return; }
    debug_assert!(sound.spec().playback == Playback::Loop || sound == SoundId::Alert(AlertSound::Alarm));
    v.push(AudioLoop { id, sound, volume_db: volume_db.min(6.0), pitch, pos, max_distance_m });
}
fn push_bed(v: &mut Vec<AudioLoop>, id: u64, s: AmbienceSfx, p: mix::LoopParams) { push_loop(v, id, SoundId::Ambience(s), p.gain_db, p.pitch, None, 0.0); }

fn v3(p: Option<[f32; 3]>) -> Vector3 { p.map_or(Vector3::ZERO, |p| Vector3::new(p[0], p[1], p[2])) }
fn stem_name(s: SoundId) -> StringName { StringName::from(crate::seams::sound::stem(s)) }

fn event_dict(e: &AudioEvent) -> VarDictionary {
    let spec = e.sound.spec();
    crate::convert::d! {
        "sound" => stem_name(e.sound),
        "bus" => StringName::from(spec.bus.name()),
        "volume_db" => e.volume_db as f64,
        "pitch" => e.pitch as f64,
        "positional" => spec.positional && e.pos.is_some(),
        "pos" => v3(e.pos),
        "max_distance_m" => e.max_distance_m as f64,
        "delay_s" => e.delay_s.max(0.0) as f64,
        "priority" => e.priority as i64,
    }
}

fn loop_dict(l: &AudioLoop) -> VarDictionary {
    let spec = l.sound.spec();
    crate::convert::d! {
        "id" => (l.id & (i64::MAX as u64)) as i64,
        "sound" => stem_name(l.sound),
        "bus" => StringName::from(spec.bus.name()),
        "volume_db" => l.volume_db as f64,
        "pitch" => l.pitch as f64,
        "positional" => spec.positional && l.pos.is_some(),
        "pos" => v3(l.pos),
        "max_distance_m" => l.max_distance_m as f64,
    }
}

/// Bus of a queued sound (real code: the spec table decides).
pub fn bus_of(sound: SoundId) -> Bus { sound.spec().bus }

/// Sounds queued by any unit this frame, and the loop set. Lives in `Services::audio`.
#[derive(Default)]
pub struct AudioQueue { pub events: Vec<AudioEvent>, pub loops: Vec<AudioLoop>, pub loops_rev: u64 }
impl AudioQueue {
    /// Queue a one-shot (real code: any unit may call this).
    pub fn play(&mut self, event: AudioEvent) { self.events.push(event); }
    /// Queue a non-positional interface sound at unit volume.
    pub fn ui(&mut self, sound: crate::seams::sound::UiSfx) { self.events.push(AudioEvent { sound: SoundId::Ui(sound), volume_db: 0.0, pitch: 1.0, pos: None, max_distance_m: 0.0, delay_s: 0.0, priority: 0 }); }
}

/// Most one-shots handed to GDScript per frame; lower priorities drop first (gunfire floods).
pub const MAX_EVENTS_PER_FRAME: usize = 24;

/// The audio event feed. Autoload `AudioFeed`.
#[derive(GodotClass)]
#[class(base = Node)]
pub struct BpAudioFeed {
    pub(crate) silenced_until_s: f64,
    /// Everything the feed keeps between frames.
    feed: Feed,
    /// `loops()` converted at the revision it was built for.
    loops_cache: RefCell<Option<(u64, Array<VarDictionary>)>>,
    base: Base<Node>,
}

#[godot_api]
impl INode for BpAudioFeed {
    fn init(base: Base<Node>) -> Self { Self { silenced_until_s: 0.0, feed: Feed::default(), loops_cache: RefCell::new(None), base } }
    fn ready(&mut self) { self.base_mut().set_process_priority(-70); }
    /// Unit `gd-audio-feed`: turn this frame's simulation events and view into queued one-shots and the loop
    /// set (engine pitch from shaft power, sea from speed and sea state, wind and rain from weather, fire and
    /// flooding from the own hull, alarm beds from the alert strip), applying silence and voice caps.
    fn process(&mut self, delta: f64) {
        let services = services();
        let catalogue = services.catalogue();
        let mut out = Vec::new();
        {
            let feed = services.feed.borrow();
            let view = services.view.borrow();
            let design = |hash: &ContentHash| -> Option<Arc<ShipDesign>> {
                if let Some(crate::worker::ReceivedDesign::Full { design, .. }) = services.designs.borrow().get(hash) { return Some(design.clone()); }
                services.design.borrow().as_ref().filter(|c| c.hash == *hash).and_then(|c| c.design.clone())
            };
            let input = FrameIn {
                now_s: feed.now_s,
                state: feed.state,
                kind: feed.kind,
                events: &feed.events,
                snapshot: feed.snapshot.as_deref(),
                view: &view,
                camera: services.camera.get(),
                catalogue: &catalogue,
                design: &design,
                line_chars: &|m| crate::text::msg_text(&crate::text::EngineLookup::new(), m).chars().count(),
            };
            self.feed.frame(&input, delta, &mut out);
        }
        self.silenced_until_s = self.feed.silenced_until_tick as f64 / TICK_HZ;
        let mut q = services.audio.borrow_mut();
        q.events.extend(out);
        // Interface and other units' sounds wait at most a frame; a backlog never builds past a few frames.
        if q.events.len() > MAX_EVENTS_PER_FRAME * 8 { let old = std::mem::take(&mut q.events); q.events = pick_events(old); }
        if q.loops_rev != self.feed.loops_rev { q.loops = self.feed.loops.clone(); q.loops_rev = self.feed.loops_rev; }
    }
}

#[godot_api]
impl BpAudioFeed {
    /// One-shots to start this frame: `Array[AudioEventDict]`, at most 24, highest priority first. Call
    /// once per frame; the queue is emptied.
    #[func]
    fn drain_events(&mut self) -> Array<VarDictionary> {
        let events = std::mem::take(&mut services().audio.borrow_mut().events);
        pick_events(events).iter().map(event_dict).collect()
    }
    /// Continuous sources that should be sounding now: `Array[AudioLoopDict]`. A loop missing from the list
    /// fades out over 150 ms. Rebuilt at 30 Hz; `loops_rev()` tells whether it changed.
    #[func]
    fn loops(&self) -> Array<VarDictionary> {
        let services = services();
        let q = services.audio.borrow();
        if let Some((rev, a)) = self.loops_cache.borrow().as_ref() { if *rev == q.loops_rev { return a.clone(); } }
        let a: Array<VarDictionary> = q.loops.iter().map(loop_dict).collect();
        *self.loops_cache.borrow_mut() = Some((q.loops_rev, a.clone()));
        a
    }
    #[func]
    fn loops_rev(&self) -> i64 { services().audio.borrow().loops_rev.min(i64::MAX as u64) as i64 }
    /// Listener pose in render space: `{ "pos": Vector3, "forward": Vector3, "up": Vector3 }` (the active
    /// camera).
    #[func]
    fn listener(&self) -> VarDictionary {
        let services = services();
        let cam = services.camera.get();
        let origin = services.view.borrow().origin;
        let unit = |v: [f32; 3], fallback: Vector3| { let w = Vector3::new(v[0], v[1], v[2]); if w.is_finite() && w.length_squared() > 1e-8 { w.normalized() } else { fallback } };
        let pos = if cam.valid && cam.pos.iter().all(|x| x.is_finite()) {
            let r = origin.to_render(DVec3::from_array(cam.pos));
            Vector3::new(r[0], r[1], r[2])
        } else {
            // The same fallback the gains use: the own hull.
            let view = services.view.borrow();
            view.own.and_then(|i| view.vessels.get(i as usize)).map(|v| Vector3::new(v.pos[0], v.pos[1], v.pos[2])).filter(|p| p.is_finite()).unwrap_or(Vector3::ZERO)
        };
        crate::convert::d! {
            "pos" => pos,
            "forward" => if cam.valid { unit(cam.forward, Vector3::new(0.0, 0.0, -1.0)) } else { Vector3::new(0.0, 0.0, -1.0) },
            "up" => if cam.valid { unit(cam.up, Vector3::UP) } else { Vector3::UP },
        }
    }
    /// Every file stem this build can emit (`seams::sound::all_stems()`, the `audio` crate's whole
    /// catalogue in `SoundId::all()` order), for preloading and for the test that checks each
    /// `res://audio/gen/<stem>.wav` exists.
    #[func]
    fn sound_ids(&self) -> PackedStringArray { crate::seams::sound::all_stems().into_iter().map(GString::from).collect() }
    /// How to play one stem: `{ "bus": StringName, "loop": bool, "positional": bool, "channels": int,
    /// "duration_s": float }` from `SoundId::spec()`, or an empty Dictionary for an unknown stem. The player
    /// reads it once per stem when it preloads.
    #[func]
    fn sound_info(&self, stem: StringName) -> VarDictionary {
        let Some(id) = crate::seams::sound::from_stem(&stem.to_string()) else { return VarDictionary::new() };
        let s = id.spec();
        crate::convert::d! {
            "bus" => StringName::from(s.bus.name()),
            "loop" => s.playback == Playback::Loop,
            "positional" => s.positional,
            "channels" => s.channels as i64,
            "duration_s" => s.duration_s as f64,
        }
    }
    /// Queue an interface sound from GDScript: `sound` is `Bp.UI_SOUND_*` (`audio::UiSfx`), so a wrong name
    /// is a parse error and interface and simulation sounds share one path and one volume policy. Any other
    /// integer is ignored.
    #[func]
    fn ui_sound(&mut self, sound: i64) {
        if let Some(s) = UiSfx::from_gd(sound) { services().audio.borrow_mut().ui(s); }
    }
    /// Start or stop the menu theme (the only music; never in a sortie).
    #[func]
    fn set_menu_theme(&mut self, playing: bool) {
        if self.feed.menu_theme == playing { return; }
        self.feed.menu_theme = playing;
        // Rebuild at the next frame.
        self.feed.loop_accum_s = 1.0;
    }
}

/// Every own requested rate at 1x, the requests a sortie starts with.
fn own_plain(requested: &[u32]) -> Vec<u32> { vec![1; requested.len()] }

#[cfg(test)]
mod tests {
    use super::*;
    use content::enums::OfficerRole;
    use sim_systems::protocol::{AlertKind, Msg, ShipStatus, Subsystem, VesselView};

    fn own() -> EntityId { EntityId::from_u64(77) }
    fn ev(e: SoundId, prio: u8, vol: f32) -> AudioEvent { AudioEvent { sound: e, volume_db: vol, pitch: 1.0, pos: None, max_distance_m: 0.0, delay_s: 0.0, priority: prio } }
    fn alert(id: u32, p: AlertPriority, state: AlertState) -> AlertView {
        AlertView { id: AlertId(id), vessel: own(), priority: p, subsystem: Subsystem::Fire, kind: AlertKind::Fire { compartment: CompartmentId(1) }, state, raised_tick: 0, officer: Some(OfficerRole::DamageControl), blink_fast: false }
    }

    #[test]
    fn cap_keeps_alerts_radio_ui_then_priority() {
        let mut v: Vec<AudioEvent> = (0..40).map(|i| ev(SoundId::Weapon(WeaponSfx::GunAuto), (i % 5) as u8, -(i as f32))).collect();
        v.push(ev(SoundId::Ui(UiSfx::Click), 0, -40.0));
        v.push(ev(SoundId::Alert(AlertSound::Caution), 0, -40.0));
        v.push(ev(SoundId::Radio(RadioSfx::SquelchOpen), 0, -40.0));
        v.push(AudioEvent { volume_db: f32::NAN, ..ev(SoundId::Weapon(WeaponSfx::GunAuto), 9, 0.0) });
        let picked = pick_events(v);
        assert_eq!(picked.len(), MAX_EVENTS_PER_FRAME);
        assert_eq!(picked[0].sound, SoundId::Alert(AlertSound::Caution));
        assert_eq!(picked[1].sound, SoundId::Radio(RadioSfx::SquelchOpen));
        assert_eq!(picked[2].sound, SoundId::Ui(UiSfx::Click));
        assert!(picked[3..].windows(2).all(|w| cap_rank(&w[0]) >= cap_rank(&w[1])));
        assert!(picked.iter().all(|e| e.volume_db.is_finite()));
    }

    #[test]
    fn warnings_repeat_after_120_s_and_beds_follow_state() {
        let mut alerts = BTreeMap::new();
        alerts.insert(AlertId(1), alert(1, AlertPriority::Warning, AlertState::Unacknowledged));
        alerts.insert(AlertId(2), alert(2, AlertPriority::Alarm, AlertState::Acknowledged));
        let mut chime = BTreeMap::new();
        chime.insert(AlertId(1), 0u64);
        let t120 = (120.0 * TICK_HZ) as u64;
        assert_eq!(alert_due(&alerts, &mut chime, t120 - 1), (vec![], false, false));
        assert_eq!(alert_due(&alerts, &mut chime, t120), (vec![AlertId(1)], false, false));
        assert_eq!(alert_due(&alerts, &mut chime, t120 + 1).0, vec![]);
        alerts.insert(AlertId(3), alert(3, AlertPriority::Alarm, AlertState::Unacknowledged));
        alerts.insert(AlertId(4), alert(4, AlertPriority::Emergency, AlertState::Cleared));
        assert_eq!(alert_due(&alerts, &mut chime, t120 + 2), (vec![], true, false));
    }

    #[test]
    fn file_choice() {
        assert_eq!(gun_sound(30, true, 500.0), SoundId::Weapon(WeaponSfx::Ciws));
        assert_eq!(gun_sound(30, true, 5_000.0), SoundId::Weapon(WeaponSfx::GunDistantLight));
        assert_eq!(gun_sound(76, false, 100.0), SoundId::Weapon(WeaponSfx::GunMedium));
        use sim_core::armour::HitOutcome as H;
        assert_eq!(shell_hit_sounds(30, H::Penetrated, false), (SoundId::Impact(ImpactSfx::AutocannonHit), None));
        assert_eq!(shell_hit_sounds(127, H::Ricochet, false).0, SoundId::Impact(ImpactSfx::Ricochet));
        assert_eq!(shell_hit_sounds(127, H::Penetrated, true).1, Some(SoundId::Impact(ImpactSfx::ExplosionLarge)));
        assert_eq!(launch_sound(MunitionKind::Sam), Some(SoundId::Weapon(WeaponSfx::SamLaunch)));
        assert_eq!(motor_sound(MunitionKind::Torpedo), None);
        assert!((static_s(10) - 1.7).abs() < 1e-9 && static_s(1_000) == STATIC_MAX_S);
        let g = SoundId::Weapon(WeaponSfx::GunMedium);
        let p: Vec<f32> = (0..50).map(|i| vary_pitch(g, DVec3::new(i as f64 * 3.1, 0.0, 7.0))).collect();
        assert!(p.iter().all(|x| (1.0 - PITCH_SPREAD..=1.0 + PITCH_SPREAD).contains(x)));
        assert!(p.windows(2).any(|w| (w[0] - w[1]).abs() > 0.005));
        assert_eq!(vary_pitch(g, DVec3::new(1.0, 2.0, 3.0)), vary_pitch(g, DVec3::new(1.0, 2.0, 3.0)));
        for k in [content::part::EngineKind::Diesel, content::part::EngineKind::GasTurbine, content::part::EngineKind::ElectricMotor] {
            assert_eq!(mix::engine_loop(engine_class(k)).spec().playback, Playback::Loop);
        }
    }

    fn snap(tick: u64) -> Snapshot {
        let v = VesselView { entity: own(), kin: Default::default(), status: VesselStatus::Active, flags: Default::default(), turrets: Vec::new(), rudder: 0.0, power: 0.5, track: Default::default(), relevance: Default::default() };
        Snapshot { tick, ship: Some(ShipStatus { vessel: own(), ..Default::default() }), vessels: vec![v], ..Default::default() }
    }

    fn run(feed: &mut Feed, state: SessionState, snapshot: &Snapshot, events: &[WireEvent], now_s: f64) -> Vec<AudioEvent> {
        let catalogue = Catalogue::default();
        let view = ViewFrame { render_tick: snapshot.tick as f64, ..Default::default() };
        let none = |_h: &ContentHash| None;
        let chars = |m: &Msg| m.key.as_str().len();
        let input = FrameIn { now_s, state, kind: SessionKind::Skirmish, events, snapshot: Some(snapshot), view: &view, camera: CameraState::default(), catalogue: &catalogue, design: &none, line_chars: &chars };
        let mut out = Vec::new();
        feed.frame(&input, 1.0 / 30.0, &mut out);
        out
    }

    #[test]
    fn alerts_sound_once_fresh_and_respect_silence() {
        let mut feed = Feed::default();
        let s = snap(600);
        let raise = |id, p| WireEvent { tick: 600, event: SimEvent::AlertRaised(alert(id, p, AlertState::Unacknowledged)) };
        let out = run(&mut feed, SessionState::InSortie, &s, &[raise(1, AlertPriority::Caution), raise(2, AlertPriority::Warning)], 1.0);
        let alerts: Vec<_> = out.iter().filter(|e| e.sound.spec().bus == Bus::Alert).map(|e| e.sound).collect();
        assert_eq!(alerts, vec![SoundId::Alert(AlertSound::Caution), SoundId::Alert(AlertSound::Warning)]);
        // An Alarm lists its pattern as a loop, not a one-shot.
        let out = run(&mut feed, SessionState::InSortie, &s, &[raise(3, AlertPriority::Alarm)], 1.1);
        assert!(out.iter().all(|e| e.sound.spec().bus != Bus::Alert));
        assert!(feed.loops.iter().any(|l| l.sound == SoundId::Alert(AlertSound::Alarm)));
        // An Emergency takes over from the Alarm pattern while it is live.
        let out = run(&mut feed, SessionState::InSortie, &s, &[raise(9, AlertPriority::Emergency)], 1.15);
        assert!(out.iter().all(|e| e.sound.spec().bus != Bus::Alert));
        let beds: Vec<SoundId> = feed.loops.iter().filter(|l| l.sound.spec().bus == Bus::Alert).map(|l| l.sound).collect();
        assert_eq!(beds, vec![SoundId::Alert(AlertSound::Emergency)]);
        feed.alerts.remove(&AlertId(9));
        // Silence drops the bed; a log-only birth never sounds.
        let silence = WireEvent { tick: 600, event: SimEvent::AlertsSilenced { vessel: own(), until_tick: 600 + 30 * 60 } };
        let born_quiet = WireEvent { tick: 601, event: SimEvent::AlertRaised(alert(4, AlertPriority::Warning, AlertState::LogOnly)) };
        let out = run(&mut feed, SessionState::InSortie, &snap(601), &[silence, born_quiet], 1.2);
        assert!(out.iter().all(|e| e.sound.spec().bus != Bus::Alert));
        assert!(!feed.loops.iter().any(|l| l.sound.spec().bus == Bus::Alert));
        // A kill-cam clip repeats an old raise: no sound.
        let reopen = WireEvent { tick: 300, event: SimEvent::SortieStarted { seed: 1, tick_hz: 60 } };
        let old = WireEvent { tick: 300, event: SimEvent::AlertRaised(alert(5, AlertPriority::Caution, AlertState::Unacknowledged)) };
        let out = run(&mut feed, SessionState::InSortie, &snap(2_000), &[reopen, old], 1.3);
        assert!(out.iter().all(|e| e.sound.spec().bus != Bus::Alert));
        // A new Alarm, then the sortie ends unacknowledged: the bed stops on the after-action pause and with
        // the hull lost.
        run(&mut feed, SessionState::InSortie, &snap(3_000), &[WireEvent { tick: 3_000, event: SimEvent::AlertRaised(alert(6, AlertPriority::Alarm, AlertState::Unacknowledged)) }], 1.4);
        assert!(feed.loops.iter().any(|l| l.sound == SoundId::Alert(AlertSound::Alarm)));
        run(&mut feed, SessionState::Ended, &snap(3_100), &[], 1.6);
        assert!(!feed.loops.iter().any(|l| l.sound.spec().bus == Bus::Alert));
        let mut sunk = snap(3_200);
        sunk.vessels[0].status = VesselStatus::Wreck;
        run(&mut feed, SessionState::InSortie, &sunk, &[], 1.8);
        assert!(!feed.loops.iter().any(|l| l.sound.spec().bus == Bus::Alert));
        // Leaving the sortie clears everything.
        run(&mut feed, SessionState::Idle, &snap(2_000), &[], 2.0);
        assert!(feed.alerts.is_empty() && feed.loops.is_empty());
    }

    #[test]
    fn radio_lines_open_static_and_close() {
        let mut feed = Feed::default();
        let s = snap(100);
        let line = WireEvent { tick: 100, event: SimEvent::OfficerMessage { vessel: own(), role: OfficerRole::Helm, text: Msg::new("officer.helm.turning") } };
        let out = run(&mut feed, SessionState::InSortie, &s, &[line], 10.0);
        assert!(out.iter().any(|e| e.sound == SoundId::Radio(RadioSfx::SquelchOpen)));
        assert!(feed.loops.iter().any(|l| l.sound == SoundId::Radio(RadioSfx::Static)));
        let out = run(&mut feed, SessionState::InSortie, &s, &[], 10.0 + STATIC_MAX_S + 0.1);
        assert!(out.iter().any(|e| e.sound == SoundId::Radio(RadioSfx::SquelchClose)));
        assert!(!feed.loops.iter().any(|l| l.sound == SoundId::Radio(RadioSfx::Static)));
    }

    #[test]
    fn stale_physical_events_are_dropped_and_weather_beds_play() {
        let mut feed = Feed::default();
        let s = snap(10_000);
        let splash = |tick| WireEvent { tick, event: SimEvent::Phys(PhysEvent::Splash { projectile: ProjectileId(1), pos: DVec3::new(10.0, 0.0, 0.0), calibre_mm: 76, shooter: EntityId::from_u64(3) }) };
        let weather = WireEvent { tick: 10_000, event: SimEvent::Phys(PhysEvent::WeatherChanged { weather: content::weather::WeatherState { sea_state: content::weather::SeaState(4), wind_speed_mps: 9.0, rain: 0.5, ..Default::default() }, wave_seed: 1 }) };
        let out = run(&mut feed, SessionState::InSortie, &s, &[splash(1_000), splash(10_000), weather], 1.0);
        assert_eq!(out.iter().filter(|e| e.sound == SoundId::Impact(ImpactSfx::SplashMedium)).count(), 1);
        assert!(out.iter().all(|e| e.volume_db <= 0.0 && e.delay_s >= 0.0));
        assert!(feed.loops.iter().any(|l| l.sound == SoundId::Ambience(AmbienceSfx::SeaModerate)));
        assert!(feed.loops.iter().any(|l| matches!(l.sound, SoundId::Ambience(AmbienceSfx::Rain | AmbienceSfx::RainHeavy))));
        assert!(feed.loops.iter().all(|l| l.volume_db.is_finite() && l.pitch > 0.0));
        let ids: BTreeSet<u64> = feed.loops.iter().map(|l| l.id).collect();
        assert_eq!(ids.len(), feed.loops.len());
    }

    #[test]
    fn pacing_changes_sound_and_a_rolling_hull_stays_quiet() {
        use sim_systems::protocol::{CompressionBlock, Pacing, TimeRate};
        let mut feed = Feed::default();
        let s = snap(100);
        let pacing = |paused: bool, rate: TimeRate, asked: TimeRate, blocked: Option<CompressionBlock>| WireEvent { tick: 100, event: SimEvent::PacingChanged(Pacing { paused, requested: vec![(content::ids::PlayerSlot(0), asked)], effective: rate, blocked, time_scale: 1.0, ..Default::default() }) };
        let ui = |out: &[AudioEvent]| out.iter().filter_map(|e| match e.sound { SoundId::Ui(u) => Some(u), _ => None }).collect::<Vec<_>>();
        // The server sends nothing for a sortie starting at plain 1x, so its first pacing may be a pause: it chimes.
        assert_eq!(ui(&run(&mut Feed::default(), SessionState::InSortie, &s, &[pacing(true, TimeRate::X1, TimeRate::X1, None)], 1.0)), vec![UiSfx::Pause]);
        // A catch-up's pacing, on the tick of its `SortieStarted`, is the starting state even when paused.
        let mut caught = Feed::default();
        let started = WireEvent { tick: 100, event: SimEvent::SortieStarted { seed: 1, tick_hz: 60 } };
        assert!(ui(&run(&mut caught, SessionState::InSortie, &s, &[started, pacing(true, TimeRate::X1, TimeRate::X1, None)], 1.0)).is_empty());
        // A first pacing at plain 1x is the starting state: silent.
        assert!(ui(&run(&mut feed, SessionState::InSortie, &s, &[pacing(false, TimeRate::X1, TimeRate::X1, None)], 1.0)).is_empty());
        assert_eq!(ui(&run(&mut feed, SessionState::InSortie, &s, &[pacing(false, TimeRate::X2, TimeRate::X2, None)], 1.1)), vec![UiSfx::TimeRate]);
        let raised = WireEvent { tick: 100, event: SimEvent::AlertRaised(alert(1, AlertPriority::Alarm, AlertState::Unacknowledged)) };
        let weather = WireEvent { tick: 100, event: SimEvent::Phys(PhysEvent::WeatherChanged { weather: content::weather::WeatherState { sea_state: content::weather::SeaState(4), wind_speed_mps: 9.0, ..Default::default() }, wave_seed: 1 }) };
        run(&mut feed, SessionState::InSortie, &s, &[raised, weather], 1.15);
        let sea_before = feed.loops.iter().find(|l| l.sound == SoundId::Ambience(AmbienceSfx::SeaModerate)).unwrap().volume_db;
        assert!(feed.loops.iter().any(|l| l.sound == SoundId::Alert(AlertSound::Alarm)));
        assert_eq!(ui(&run(&mut feed, SessionState::InSortie, &s, &[pacing(true, TimeRate::X2, TimeRate::X2, None)], 1.2)), vec![UiSfx::Pause]);
        // Paused: the Alarm pattern holds and the sea sits back.
        assert!(!feed.loops.iter().any(|l| l.sound.spec().bus == Bus::Alert));
        let sea_paused = feed.loops.iter().find(|l| l.sound == SoundId::Ambience(AmbienceSfx::SeaModerate)).unwrap().volume_db;
        assert!((sea_paused - (sea_before + PAUSE_DUCK_DB)).abs() < 0.01);
        assert_eq!(ui(&run(&mut feed, SessionState::InSortie, &s, &[pacing(false, TimeRate::X2, TimeRate::X2, None)], 1.3)), vec![UiSfx::Resume]);
        assert!(feed.loops.iter().any(|l| l.sound == SoundId::Alert(AlertSound::Alarm)));
        assert_eq!(ui(&run(&mut feed, SessionState::InSortie, &s, &[pacing(false, TimeRate::X2, TimeRate::X4, Some(CompressionBlock::ThreatInOwnEnvelope))], 1.4)), vec![UiSfx::Invalid]);
        // An undamaged hull rolling 25 degrees in a seaway does not groan; a holed one does.
        let mut feed = Feed::default();
        let mut rolling = snap(200);
        let mut out = Vec::new();
        for i in 0..40 {
            rolling.ship.as_mut().unwrap().heel_deg = 5.0 + i as f32;
            out.extend(run(&mut feed, SessionState::InSortie, &rolling, &[], 2.0 + i as f64 * 0.1));
        }
        assert!(out.iter().all(|e| e.sound != SoundId::Impact(ImpactSfx::HullGroan)));
        rolling.ship.as_mut().unwrap().cells_lost = 0.1;
        let mut out = Vec::new();
        for i in 0..40 {
            rolling.ship.as_mut().unwrap().heel_deg = 5.0 + i as f32;
            out.extend(run(&mut feed, SessionState::InSortie, &rolling, &[], 10.0 + i as f64 * 0.1));
        }
        assert_eq!(out.iter().filter(|e| e.sound == SoundId::Impact(ImpactSfx::HullGroan)).count(), 1);
    }

    #[test]
    fn enemy_fire_and_hits_sound_at_their_place_and_distance() {
        use content::part::weapons::CalibreBand;
        use sim_core::projectiles::ProjectileSpawn;
        let mut feed = Feed::default();
        let s = snap(1_000);
        let enemy = EntityId::from_u64(5);
        // The listener is the own hull at the origin (no camera in a unit test).
        let shot = |id: u32, at: DVec3| WireEvent { tick: 1_000, event: SimEvent::Phys(PhysEvent::ProjectileSpawned(ProjectileSpawn { id: ProjectileId(id), tick: 1_000, origin: at, velocity: DVec3::X, calibre_mm: 76, band: CalibreBand::Medium, shooter: enemy, mount: PartSlot(id as u16), rounds: 1, point_defence: false })) };
        let near = DVec3::new(800.0, 5.0, 0.0);
        let far = DVec3::new(5_000.0, 5.0, 0.0);
        let out = run(&mut feed, SessionState::InSortie, &s, &[shot(1, near), shot(2, far)], 1.0);
        let gun = |snd: WeaponSfx| out.iter().find(|e| e.sound == SoundId::Weapon(snd)).copied().unwrap();
        let (n, f) = (gun(WeaponSfx::GunMedium), gun(WeaponSfx::GunDistantLight));
        assert!(n.pos.is_some() && f.pos.is_some());
        assert!((n.delay_s - 800.0 / mix::SOUND_SPEED_MPS).abs() < 0.05 && (f.delay_s - 5_000.0 / mix::SOUND_SPEED_MPS).abs() < 0.05);
        assert!(n.volume_db > f.volume_db && f.volume_db > -30.0, "near {} far {}", n.volume_db, f.volume_db);
        assert!(n.pitch != 1.0 || f.pitch != 1.0);
        // The near shell hits the own hull and bursts: a penetrating hit and the medium-calibre blast, close and
        // loud; a body (missile, unknown munition) hitting plays the large blast.
        let hit = WireEvent { tick: 1_001, event: SimEvent::Phys(PhysEvent::ShellImpact { projectile: ProjectileId(1), vessel: own(), cell: content::ids::CellIndex(0), outcome: sim_core::armour::HitOutcome::Penetrated, world_point: DVec3::new(10.0, 2.0, 0.0), shooter: enemy, burst: true }) };
        let body = WireEvent { tick: 1_001, event: SimEvent::Phys(PhysEvent::BodyImpact { body: EntityId::from_u64(9), vessel: own(), cell: content::ids::CellIndex(0), world_point: DVec3::new(-20.0, 2.0, 0.0), cause: sim_core::damage::DamageCause::Missile }) };
        let out = run(&mut feed, SessionState::InSortie, &snap(1_001), &[hit, body], 1.1);
        let sounds: Vec<SoundId> = out.iter().map(|e| e.sound).collect();
        assert!(sounds.contains(&SoundId::Impact(ImpactSfx::PenetrateHit)) && sounds.contains(&SoundId::Impact(ImpactSfx::ExplosionSmall)) && sounds.contains(&SoundId::Impact(ImpactSfx::ExplosionLarge)), "{sounds:?}");
        assert!(out.iter().all(|e| e.priority == prio::OWN && e.volume_db > -6.0 && e.delay_s < 0.1));
        // Past the family's range nothing plays, and inside it the gain fades out smoothly.
        let out = run(&mut feed, SessionState::InSortie, &snap(1_002), &[shot(3, DVec3::new(30_000.0, 5.0, 0.0)), shot(4, DVec3::new(41_000.0, 5.0, 0.0))], 1.2);
        assert_eq!(out.len(), 1);
        assert!(out[0].volume_db < -35.0 && out[0].volume_db > -60.0, "{}", out[0].volume_db);
    }

    #[test]
    fn menu_theme_only_outside_sorties() {
        let mut feed = Feed { menu_theme: true, ..Feed::default() };
        run(&mut feed, SessionState::Idle, &snap(0), &[], 0.0);
        assert_eq!(feed.loops.iter().map(|l| l.sound).collect::<Vec<_>>(), vec![SoundId::MenuTheme]);
        run(&mut feed, SessionState::InSortie, &snap(10), &[], 0.1);
        assert!(feed.loops.iter().all(|l| l.sound != SoundId::MenuTheme));
    }
}
