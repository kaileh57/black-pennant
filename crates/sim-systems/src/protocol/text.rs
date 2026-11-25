//! Contract-owned (no work unit). Messages as translation keys with typed arguments, and the typed reason
//! enums carried by tracks, mounts and officers. DESIGN.md L351-353 (every hold and every shot carries a
//! reason; officers report in text), L477 and L503 (track card reasons), L1695-1706 (copy rules).
//!
//! The wire carries the enums; `crate::text` (unit `sys-text`) turns each into a [`Msg`]; the Godot client
//! resolves `Msg::key` with `tr()` and formats the arguments (units, number style) itself.
use crate::ids::TrackId;
use content::enums::{Emcon, IdRung, Stance, TrackGrade};
use content::ids::{CompartmentId, PartSlot, TextKey};
use serde::{Deserialize, Serialize};

/// Unit of a numeric argument; the client formats it (L1668: value with units).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Unit { #[default] None, Metres, Kilometres, Knots, Degrees, Seconds, Minutes, Hours, Percent, Kilowatts, Litres, Tonnes, Millimetres, Rounds, Salvage, Intel }

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum MsgArg {
    Int(i64),
    Num { value: f64, unit: Unit },
    /// A nested translation key (part name, class name, officer billet).
    Key(TextKey),
    /// Player-entered or generated text that is never translated (ship name, "Gun 2", player name).
    Name(String),
    Track(TrackId),
    Part(PartSlot),
    Compartment(CompartmentId),
    Stance(Stance),
    Rung(IdRung),
    Grade(TrackGrade),
    Emcon(Emcon),
}

/// One player-facing sentence. English lives in `game/i18n/*.csv` under `key`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Msg { pub key: TextKey, pub args: Vec<MsgArg> }
impl Msg {
    pub fn new(key: &str) -> Self { Self { key: TextKey::new(key), args: Vec::new() } }
    pub fn arg(mut self, a: MsgArg) -> Self { self.args.push(a); self }
    pub fn num(self, value: f64, unit: Unit) -> Self { self.arg(MsgArg::Num { value, unit }) }
    pub fn is_empty(&self) -> bool { self.key.0.is_empty() }
}

/// Why a mount is not firing (L407 "Holding: reason", L353). One per mount per engagement tick.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub enum HoldReason {
    #[default] NoTarget,
    /// Doctrine needs a higher stance (never auto-engage Neutral or Friend, L487).
    Stance { have: Stance, need: Stance },
    Id { have: IdRung, need: IdRung },
    Grade { have: TrackGrade, need: TrackGrade },
    OutOfRange { range_m: f64, limit_m: f64 },
    /// Suspect track outside the ship's self-defence range (L484).
    SuspectOutsideSelfDefence { range_m: f64, limit_m: f64 },
    /// "Holding: Neutral inside seeker basket" (L483).
    NeutralInSeekerBasket { neutral: TrackId },
    /// "solution needs +74 degrees, mount maximum is +70" (L353).
    TrainLimit { need_deg: f64, limit_deg: f64 },
    ElevationLimit { need_deg: f64, limit_deg: f64 },
    /// Own structure blocks the line of fire.
    Masked,
    Slewing,
    Reloading { seconds: f64 },
    NoAmmunition,
    Unpowered,
    /// Shed by the Engineer's load shedder (L287).
    Shed,
    Damaged,
    /// Own-side unit in or near the line of fire.
    FriendInLine { friend: TrackId },
    /// Salvo already in flight against this track (SAM: second only after a miss, L485).
    SalvoInFlight,
    /// Fire permission withdrawn by the player, a rule, a graph node or a script.
    PermissionWithheld,
    /// The player holds this mount; the officer shows an advisory solution only (L953).
    PlayerControl,
    /// Emission control keeps the needed fire-control radar silent (L507).
    EmconSilent,
    NoSolution,
    /// An anti-ship missile offered a shore site: its seeker finds a ship against the sea, not a site against
    /// the land behind it (coordinator ruling, 22 November 2026; L483 and L523 do not mention sites). Appended
    /// last so the earlier variants keep their wire indices.
    ShoreSite,
}

/// Why a mount is firing (every shot carries a reason, L353).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum FireReason {
    /// The doctrine entry for (weapon, target category) is satisfied (L481-485).
    #[default] Doctrine,
    /// Inbound weapon, engaged regardless of launcher (L486).
    PointDefence,
    /// Suspect inside self-defence range (L484).
    SelfDefence,
    /// The player designated this track (L108).
    Designated,
    /// L2 rule at this index of the rules table (L336).
    Rule(u8),
    /// Player-held mount (L949-953).
    Manual,
    /// A pinned or replaced graph node, or a script, wrote the fire permission.
    Graph,
    /// Local control after losing the Data link: own sensor, 3x error (L394).
    LocalControl,
}

/// Engagement stage output per mount (L407).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub enum EngageStatus { Idle, Engaging { track: TrackId, reason: FireReason }, Holding { track: TrackId, reason: HoldReason } }
impl Default for EngageStatus { fn default() -> Self { EngageStatus::Idle } }

/// What moved a track up an identification rung (track card, L472-477).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum RungReason { #[default] None, AltitudeMeasured, DepthMeasured, EmitterMatch, AcousticMatch, RcsAndSpeed, EmitterUniqueToClass, VisualSighting, CloseVisualSighting, Photograph, Briefing, Datalink, PlayerOverride }

/// Why a track holds its current stance (L496-503). The track card always shows it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum StanceReason { #[default] Default, Datalink, FiredOnOwnSide, CombineEmitter, BriefingHostile, BriefingNeutral, LobbyOpposingSide, PlayerOverride, GraphOverride }

/// Who drives a role right now (L953, L1664-1665).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ControlHolder { #[default] Officer, Player }
